//! Local title embeddings (bge-small-en-v1.5). Optional public metadata is fetched separately in
//! the background; inference never calls an AI service or exposes provider URLs or credentials.
//! One bounded CPU session is shared by background checks on every platform.
use crate::{backend::{self, Media}, picks_metadata};
use anyhow::{anyhow, ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ort::{session::Session, value::Tensor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Mutex, OnceLock, RwLock}, time::Duration};
use tokenizers::Tokenizer;

// Changing the model, input construction or normalization requires a new version.
pub const MODEL_VERSION: &str = "bge-small-en-v1.5-int8-ea104dac-v1";
/// Learned taste vectors live in this embedding space; a new space invalidates them.
pub const SPACE_VERSION: &str = "bge-small-en-v1.5-v1";
pub const DIM: usize = 384;
const MAX_TOKENS: usize = 256;
const HEAD_TOKENS: usize = 32;
const KEYWORD_TOKENS: usize = 64;
const MAX_ENTRIES: usize = 4096;
/// Typed queries kept beside the pinned mood anchors.
const MAX_QUERIES: usize = 256;
const SAVE_EVERY: Duration = Duration::from_secs(10);
const CACHE_FILE: &str = "picks-embeddings-v1.json";

/// What the embedding store may hold. Titles are the bulk and are evicted by recency of use;
/// queries (mood anchors and what was typed) live in a map of their own, so a long run of
/// titles can never push a mood anchor out.
struct Stored { vector: Vec<f32>, used: AtomicU64 }
/// Logical clock for recency: higher is more recently stored or looked up.
static CLOCK: AtomicU64 = AtomicU64::new(1);
fn tick() -> u64 { CLOCK.fetch_add(1, Ordering::Relaxed) }

#[derive(Default)]
struct Cache { map: BTreeMap<String, Stored>, queries: BTreeMap<String, Stored> }
impl Cache {
    fn is_query(key: &str) -> bool { key.contains("-q-") }
    fn get(&self, key: &str) -> Option<&Vec<f32>> {
        let found = if Self::is_query(key) { self.queries.get(key) } else { self.map.get(key) };
        // A lookup counts as use: what Picks keeps asking about is what stays.
        found.map(|s| { s.used.store(tick(), Ordering::Relaxed); &s.vector })
    }
    fn contains_key(&self, key: &str) -> bool { if Self::is_query(key) { self.queries.contains_key(key) } else { self.map.contains_key(key) } }
    fn insert(&mut self, key: String, vector: Vec<f32>) {
        let stored = Stored { vector, used: AtomicU64::new(tick()) };
        if Self::is_query(&key) {
            self.queries.insert(key, stored);
            while self.queries.len() > MAX_QUERIES {
                // The mood words stay pinned; only other queries make way for new ones.
                let oldest = self.queries.iter().filter(|(k, _)| !pinned(k)).min_by_key(|(_, s)| s.used.load(Ordering::Relaxed)).map(|(k, _)| k.clone());
                let Some(oldest) = oldest else { break };
                self.queries.remove(&oldest);
            }
        } else {
            self.map.insert(key, stored);
            while self.map.len() > MAX_ENTRIES {
                let Some(oldest) = self.map.iter().min_by_key(|(_, s)| s.used.load(Ordering::Relaxed)).map(|(k, _)| k.clone()) else { break };
                self.map.remove(&oldest);
            }
        }
    }
    /// Keys of a store, least recently used first.
    fn order(map: &BTreeMap<String, Stored>) -> Vec<&String> {
        let mut keys: Vec<(&String, u64)> = map.iter().map(|(k, s)| (k, s.used.load(Ordering::Relaxed))).collect();
        keys.sort_by_key(|(_, used)| *used);
        keys.into_iter().map(|(k, _)| k).collect()
    }
    fn encode(&self) -> Value {
        let packed = |map: &BTreeMap<String, Stored>| -> BTreeMap<String, String> { map.iter().map(|(k, s)| (k.clone(), pack(&s.vector))).collect() };
        json!({"version": MODEL_VERSION, "entries": packed(&self.map), "order": Self::order(&self.map),
            "queries": packed(&self.queries), "query_order": Self::order(&self.queries)})
    }
}
/// The mood words are pinned: the brain's anchors must survive any amount of titles and typed queries.
fn pinned(key: &str) -> bool {
    static MOODS: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    MOODS.get_or_init(|| crate::brain::MOODS.iter().map(|(_, phrase)| query_key(phrase)).collect()).contains(key)
}
/// Unit vectors have components in [-1, 1]; 16 bits keep cosine error far below ranking noise.
pub(crate) fn pack(vector: &[f32]) -> String {
    let bytes: Vec<u8> = vector.iter().flat_map(|x| ((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()).collect();
    STANDARD.encode(bytes)
}
pub(crate) fn unpack(text: &str) -> Option<Vec<f32>> {
    let bytes = STANDARD.decode(text).ok()?;
    (bytes.len() == DIM * 2).then(|| bytes.chunks_exact(2).map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32767.0).collect())
}

static CACHE: OnceLock<RwLock<Cache>> = OnceLock::new();
static SAVE_DIRTY: AtomicBool = AtomicBool::new(false);
static SAVE_RUNNING: AtomicBool = AtomicBool::new(false);
static EMBEDDER: Mutex<Option<Result<Embedder, String>>> = Mutex::new(None);
static FAILURE: Mutex<Option<String>> = Mutex::new(None);

fn cache() -> &'static RwLock<Cache> { CACHE.get_or_init(Default::default) }

/// Number of embeddings currently loaded in the local cache.
pub fn embedding_count() -> usize { cache().read().map(|c| c.map.len()).unwrap_or(0) }

/// The saved embeddings, read once, before the model is first needed: Picks reads vectors (the
/// brain's projection, similarity) long before anything new is embedded.
fn ensure_cache() {
    static LOADED: OnceLock<()> = OnceLock::new();
    LOADED.get_or_init(|| {
        #[cfg(not(test))]
        {
            let loaded = read_cache(&backend::backend_data_dir().join(CACHE_FILE));
            // Oldest first, so the saved recency carries over.
            if let Ok(mut c) = cache().write() {
                for store in [&loaded.map, &loaded.queries] {
                    for key in Cache::order(store) { if let Some(s) = store.get(key) { if !c.contains_key(key) { c.insert(key.clone(), s.vector.clone()); } } }
                }
            }
        }
    });
}

/// bge's instruction for short queries matched against passages (the synopses).
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";
fn query_key(text: &str) -> String { format!("{MODEL_VERSION}-q-{:x}", Sha256::digest(text.trim().to_lowercase().as_bytes())) }

/// A short description (a mood) in the titles' space, if already embedded.
pub fn query_embedding(text: &str) -> Option<Vec<f32>> {
    ensure_cache();
    cache().read().ok()?.get(&query_key(text)).cloned()
}

/// Every title embedding kept, for fitting the brain's projection (queries left out).
pub fn all_embeddings() -> Vec<Vec<f32>> {
    ensure_cache();
    cache().read().map(|c| c.map.values().map(|s| s.vector.clone()).collect()).unwrap_or_default()
}

/// The text a title is embedded from. No genres: they are separate taste features, and leaving
/// them out lets synopsis and keywords decide the similarity on their own.
struct Parts { head: String, synopsis: String, keywords: String }
impl Parts {
    fn new(m: &Media, extra: &picks_metadata::Extra) -> Option<Self> {
        let synopsis = picks_metadata::clean(&m.description, 4000);
        if synopsis.chars().count() < 30 && extra.keywords.is_empty() { return None; }
        let name = picks_metadata::clean(&m.name, 256);
        let year = picks_metadata::clean(&m.year, 64);
        let head = if year.is_empty() { format!("{name}.") } else { format!("{name} ({year}).") };
        let keywords = if extra.keywords.is_empty() { String::new() } else { format!("Keywords: {}.", extra.keywords.join(", ")) };
        Some(Self { head, synopsis, keywords })
    }
    /// Hash only inference input; never put a provider URL, token or raw metadata in this cache.
    fn key(&self) -> String {
        let input = format!("{}\n{}\n{}", self.head, self.synopsis, self.keywords);
        format!("{MODEL_VERSION}-{:x}", Sha256::digest(input.as_bytes()))
    }
}
fn parts(m: &Media) -> Option<Parts> { Parts::new(m, &picks_metadata::cached(m)) }

/// The title's embedding, if computed. While refreshed enrichment is still being embedded, the
/// vector from the unenriched text keeps serving.
pub fn embedding(m: &Media) -> Option<Vec<f32>> {
    ensure_cache();
    let (full, base) = text_keys(m)?;
    let cache = cache().read().ok()?;
    cache.get(&full).cloned().or_else(|| base.and_then(|base| cache.get(&base).cloned()))
}

/// A title's cache keys (its text with and without enrichment), found by a fast hash of exactly
/// what goes into them: Picks looks titles up all the time, and cleaning and hashing a synopsis
/// with SHA-256 each time cost more than the lookup itself. None when there is too little text.
fn text_keys(m: &Media) -> Option<(String, Option<String>)> {
    use std::hash::{Hash, Hasher};
    type Keys = Option<(String, Option<String>)>;
    static KNOWN: OnceLock<Mutex<std::collections::HashMap<u64, Keys>>> = OnceLock::new();
    let extra = picks_metadata::cached(m);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&m.name, &m.year, &m.description, &extra.keywords).hash(&mut h);
    let print = h.finish();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(keys) = known.lock().ok().and_then(|k| k.get(&print).cloned()) { return keys; }
    let keys = Parts::new(m, &extra).map(|full| (full.key(), Parts::new(m, &picks_metadata::Extra::default()).map(|b| b.key())));
    if let Ok(mut k) = known.lock() { if k.len() > 20_000 { k.clear(); } k.insert(print, keys.clone()); }
    keys
}
/// Both vectors are unit length, so the dot product is the cosine.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum()
}

#[cfg(test)]
pub(crate) fn cache_test_embedding(media: &Media, vector: Vec<f32>) {
    cache().write().unwrap().insert(parts(media).unwrap().key(), vector);
}

pub fn failed() -> bool { FAILURE.lock().map(|f| f.is_some()).unwrap_or(true) }

/// ONNX Runtime's native library on this platform.
#[cfg_attr(all(target_os = "android", not(test)), allow(dead_code))]
const RUNTIME_LIBRARY: &str = if cfg!(windows) { "onnxruntime.dll" } else if cfg!(target_os = "macos") { "libonnxruntime.dylib" } else { "libonnxruntime.so" };

/// Desktop: `picks-ai/` beside the executable (Linux, Windows, a development build on macOS),
/// or in the bundle's Resources inside Veylo.app (tools/build-macos.sh).
#[cfg(not(target_os = "android"))]
#[cfg_attr(test, allow(dead_code))]
pub fn desktop_assets() -> Result<PathBuf> {
    let exe_dir = std::env::current_exe()?.parent().ok_or_else(|| anyhow!("Executable directory"))?.to_path_buf();
    let beside = exe_dir.join("picks-ai");
    if cfg!(target_os = "macos") && !beside.is_dir() {
        let bundled = exe_dir.join("../Resources/picks-ai");
        if bundled.is_dir() { return Ok(bundled); }
    }
    Ok(beside)
}

/// The model, tokenizer and runtime the app would load (self-test).
#[cfg(not(target_os = "android"))]
pub fn asset_files() -> Result<Vec<PathBuf>> {
    let (dir, runtime) = paths()?;
    Ok(vec![dir.join("model.onnx"), dir.join("tokenizer.json"), runtime])
}

fn paths() -> Result<(PathBuf, PathBuf)> {
    #[cfg(test)]
    {
        let dir = std::env::var_os("VEYLO_PICKS_AI_TEST_ASSETS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("picks-ai/model"));
        let target = if cfg!(all(target_os = "android", target_arch = "arm")) { "android-armv7" } else if cfg!(target_os = "android") { "android-arm64" } else if cfg!(windows) { "windows-x64" } else if cfg!(target_os = "macos") { "macos-arm64" }
            else if cfg!(target_arch = "aarch64") { "linux-aarch64" } else { "linux-x64" };
        let runtime = if std::env::var_os("VEYLO_PICKS_AI_TEST_ASSETS").is_some() { dir.join(RUNTIME_LIBRARY) }
            else { Path::new(env!("CARGO_MANIFEST_DIR")).join("picks-ai").join(target).join(RUNTIME_LIBRARY) };
        Ok((dir, runtime))
    }
    #[cfg(all(not(test), target_os = "android"))]
    { crate::android::picks_ai_paths() }
    #[cfg(all(not(test), not(target_os = "android")))]
    {
        let dir = desktop_assets()?;
        Ok((dir.clone(), dir.join("runtime").join(RUNTIME_LIBRARY)))
    }
}

struct Embedder { session: Session, tokenizer: Tokenizer, cls: i64, sep: i64 }
impl Embedder {
    fn open(dir: &Path, runtime: &Path) -> Result<Self> {
        ensure!(runtime.is_file(), "Bundled ONNX Runtime is missing");
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows::{core::PCWSTR, Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS}};
            // Resolve app-local VC runtime dependencies beside this DLL. Keep the module loaded
            // for the process lifetime, just like ORT's own global runtime handle.
            let wide: Vec<u16> = runtime.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) }?;
        }
        ort::init_from(runtime.to_string_lossy()).with_name("Veylo Picks").commit()?;
        #[cfg(any(target_os = "android", target_os = "macos"))]
        register_shutdown();
        let session = Session::builder()?.with_intra_threads(2)?.with_inter_threads(1)?
            .commit_from_file(dir.join("model.onnx"))?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("Tokenizer: {e}"))?;
        let special = |token: &str| tokenizer.token_to_id(token).map(i64::from).ok_or_else(|| anyhow!("Tokenizer lacks {token}"));
        let (cls, sep) = (special("[CLS]")?, special("[SEP]")?);
        Ok(Self { session, tokenizer, cls, sep })
    }
    fn encode(&self, text: &str) -> Result<Vec<i64>> {
        if text.is_empty() { return Ok(Vec::new()); }
        self.tokenizer.encode(text, false).map(|e| e.get_ids().iter().map(|id| i64::from(*id)).collect())
            .map_err(|e| anyhow!("Tokenization: {e}"))
    }
    /// The name and keywords keep a reserved share, so a long synopsis never crowds them out.
    fn input_ids(&self, parts: &Parts) -> Result<Vec<i64>> {
        let head = self.encode(&parts.head)?;
        let keywords = self.encode(&parts.keywords)?;
        let synopsis = self.encode(&parts.synopsis)?;
        let head = &head[..head.len().min(HEAD_TOKENS)];
        let keywords = &keywords[..keywords.len().min(KEYWORD_TOKENS)];
        let room = (MAX_TOKENS - 2).saturating_sub(head.len() + keywords.len());
        let mut ids = vec![self.cls];
        ids.extend(head);
        ids.extend(&synopsis[..synopsis.len().min(room)]);
        ids.extend(keywords);
        ids.push(self.sep);
        Ok(ids)
    }
    fn embed(&mut self, parts: &Parts) -> Result<Vec<f32>> {
        let ids = self.input_ids(parts)?;
        self.run(ids)
    }
    /// A short text on its own (a mood), within the token budget.
    fn embed_text(&mut self, text: &str) -> Result<Vec<f32>> {
        let body = self.encode(text)?;
        let mut ids = vec![self.cls];
        ids.extend(&body[..body.len().min(MAX_TOKENS - 2)]);
        ids.push(self.sep);
        self.run(ids)
    }
    fn run(&mut self, ids: Vec<i64>) -> Result<Vec<f32>> {
        let len = ids.len();
        let outputs = self.session.run(ort::inputs![
            "input_ids" => Tensor::from_array(([1, len], ids))?,
            "attention_mask" => Tensor::from_array(([1, len], vec![1i64; len]))?,
            "token_type_ids" => Tensor::from_array(([1, len], vec![0i64; len]))?,
        ])?;
        let (shape, hidden) = outputs["last_hidden_state"].try_extract_tensor::<f32>()?;
        ensure!(shape.as_ref() == [1, len as i64, DIM as i64], "Unexpected embedding output shape");
        // The first ([CLS]) token is the sentence embedding for bge models.
        unit(&hidden[..DIM])
    }
}

fn unit(vector: &[f32]) -> Result<Vec<f32>> {
    ensure!(vector.len() == DIM && vector.iter().all(|x| x.is_finite()), "Invalid embedding");
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    ensure!(norm > 0.0, "Empty embedding");
    Ok(vector.iter().map(|x| x / norm).collect())
}

/// ort keeps its Environment in a Rust static, which is not dropped at process exit. On
/// Android and macOS the native library's C++ static destructors can then destroy a mutex before
/// the environment releases it ("mutex lock failed", an abort at every exit). Register after dlopen,
/// so cleanup runs before those destructors.
#[cfg(any(target_os = "android", target_os = "macos"))]
fn register_shutdown() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        extern "C" fn shutdown() {
            use ort::AsPointer;
            if let Ok(mut state) = EMBEDDER.lock() {
                // Block queued workers from creating another session during process shutdown.
                let embedder = state.replace(Err("Local inference is shutting down".into()));
                drop(embedder);
                if let Ok(environment) = ort::environment::get_environment() {
                    // No session survives this point. G_ENV itself never drops, so this is
                    // the only ReleaseEnv; the process exits immediately after atexit hooks.
                    unsafe { (ort::api().ReleaseEnv)(environment.ptr().cast_mut()) };
                }
            }
        }
        unsafe { libc::atexit(shutdown) };
    });
}

fn read_cache(path: &Path) -> Cache {
    let value: Value = std::fs::read_to_string(path).ok().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    if value["version"] != MODEL_VERSION { return Cache::default(); }
    let vectors = |field: &str| -> BTreeMap<String, Vec<f32>> { value[field].as_object().into_iter().flatten()
        .filter_map(|(key, value)| Some((key.clone(), unpack(value.as_str()?)?))).collect() };
    let mut entries = vectors("entries");
    // Earlier files kept queries among the titles: they move to their own map.
    entries.extend(vectors("queries"));
    let mut cache = Cache::default();
    // Saved recency first (least recent first); entries without it follow in key order.
    for field in ["order", "query_order"] {
        for key in value[field].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if let Some(vector) = entries.remove(key) { cache.insert(key.to_owned(), vector); }
        }
    }
    for (key, vector) in entries { cache.insert(key, vector); }
    cache
}

/// Rewriting the whole file per title is costly on flash, so one delayed atomic write covers
/// every title embedded meanwhile. A failed save must not disable inference.
fn save_soon(path: PathBuf) {
    SAVE_DIRTY.store(true, Ordering::Release);
    if SAVE_RUNNING.swap(true, Ordering::AcqRel) { return; }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(SAVE_EVERY);
            SAVE_DIRTY.store(false, Ordering::Release);
            let serialized = cache().read().map(|c| c.encode().to_string());
            if let Ok(serialized) = serialized {
                let _ = backend::write_private_file(&path, serialized.as_bytes());
            }
            if !SAVE_DIRTY.load(Ordering::Acquire) {
                SAVE_RUNNING.store(false, Ordering::Release);
                if !SAVE_DIRTY.load(Ordering::Acquire) || SAVE_RUNNING.swap(true, Ordering::AcqRel) { break; }
            }
        }
    });
}

/// Serialize inference off the UI executor, on the one shared session. A failed runtime is
/// reported to the UI and retried on the next app launch.
async fn with_embedder<T: Send + 'static>(work: impl FnOnce(&mut Embedder, PathBuf) -> Result<T> + Send + 'static) -> Result<T> {
    backend::on_runtime(async move {
        tokio::task::spawn_blocking(move || -> Result<T> {
            let mut state = EMBEDDER.lock().map_err(|_| anyhow!("Embedder lock"))?;
            let dir = backend::backend_data_dir();
            if state.is_none() {
                // The earlier classifier's score cache is of no further use.
                let _ = std::fs::remove_file(dir.join("picks-traits-v1.json"));
                ensure_cache();
                *state = Some(paths().and_then(|(dir, runtime)| Embedder::open(&dir, &runtime)).map_err(|e| e.to_string()));
            }
            let embedder = match state.as_mut().unwrap() {
                Ok(embedder) => embedder,
                // Only an unusable runtime is reported to the UI; one bad title is skipped.
                Err(error) => {
                    if let Ok(mut failure) = FAILURE.lock() { *failure = Some(error.clone()); }
                    return Err(anyhow!(error.clone()));
                }
            };
            work(embedder, dir.join(CACHE_FILE))
        }).await.map_err(|_| anyhow!("Inference worker stopped"))?
    }).await?
}

/// Embed a title (from verification and prefetch); cached titles never rerun.
pub async fn embed(m: &Media) -> Result<()> {
    picks_metadata::load().await;
    let Some(parts) = parts(m) else { return Ok(()) };
    let id = parts.key();
    ensure_cache();
    if cache().read().ok().is_some_and(|c| c.contains_key(&id)) { return Ok(()); }
    with_embedder(move |embedder, cache_path| {
        if cache().read().map_err(|_| anyhow!("Embedding cache lock"))?.contains_key(&id) { return Ok(()); }
        let vector = embedder.embed(&parts)?;
        cache().write().map_err(|_| anyhow!("Embedding cache lock"))?.insert(id, vector);
        save_soon(cache_path);
        Ok(())
    }).await
}

/// Embed a short description (a mood word or what you typed) as a search query.
pub async fn embed_query(text: &str) -> Result<Vec<f32>> {
    if let Some(v) = query_embedding(text) { return Ok(v); }
    let clean = picks_metadata::clean(text, 300);
    ensure!(!clean.is_empty(), "Nothing to embed");
    let id = query_key(text);
    with_embedder(move |embedder, cache_path| {
        if let Some(v) = cache().read().map_err(|_| anyhow!("Embedding cache lock"))?.get(&id) { return Ok(v.clone()); }
        let vector = embedder.embed_text(&format!("{QUERY_PREFIX}{clean}"))?;
        cache().write().map_err(|_| anyhow!("Embedding cache lock"))?.insert(id, vector.clone());
        save_soon(cache_path);
        Ok(vector)
    }).await
}

/// Metadata requests stay outside verification/playback; inference itself remains local.
pub async fn enrich_and_embed(m: &Media) -> Result<()> {
    picks_metadata::enrich(m).await;
    embed(m).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str, year: &str, synopsis: &str) -> Media {
        Media::from_json(&json!({"id": format!("test:{name}"), "type": "series", "name": name, "releaseInfo": year, "description": synopsis}), "fixture").unwrap()
    }
    #[test]
    fn bundled_model_places_similar_titles_closer() {
        let (dir, runtime) = paths().unwrap();
        let mut embedder = Embedder::open(&dir, &runtime).unwrap();
        let mut embed = |m: &Media| embedder.embed(&Parts::new(m, &picks_metadata::Extra::default()).unwrap()).unwrap();
        let dark = embed(&fixture("Love, Death & Robots", "2019–2025", "Terrifying creatures, wicked surprises and dark comedy converge in this NSFW anthology of animated stories presented by Tim Miller and David Fincher."));
        let similar = embed(&fixture("Castlevania", "2017–2021", "A vampire hunter fights to save a besieged city from an army of otherworldly creatures controlled by Dracula."));
        let light = embed(&fixture("Bob's Burgers", "2011–", "Bob follows a third-generation restaurateur as he runs Bob's Burgers with the help of his wife and their three kids. Bob and his quirky family have big ideas about burgers, but fall short on service and sophistication."));
        assert!((cosine(&dark, &dark) - 1.0).abs() < 1e-4);
        let (near, far) = (cosine(&dark, &similar), cosine(&dark, &light));
        println!("dark~similar {near:.3}, dark~light {far:.3}");
        assert!(near > far, "Dark animation {near} should sit closer than a family sitcom {far}");
        let politics = embed(&fixture("Senate", "2020", "The senate votes in an election to replace the prime minister amid a political scandal."));
        let party = embed(&fixture("Bachelor", "2009", "A group of friends get drunk at a bachelor party, wake up with no memory, and retrace their steps through hilarious disasters."));
        let comedy = embed(&fixture("Reunion", "2012", "Old friends hold a chaotic wedding weekend where every plan falls apart in hilarious ways."));
        assert!(cosine(&party, &comedy) > cosine(&party, &politics));
    }
    #[test]
    fn long_synopses_keep_name_and_keywords_within_the_token_budget() {
        let (dir, runtime) = paths().unwrap();
        let mut embedder = Embedder::open(&dir, &runtime).unwrap();
        let m = fixture("Input budget fixture", "2020", &"Long synopsis. ".repeat(2000));
        let extra = picks_metadata::Extra { keywords: vec!["psychological horror".into(), "dystopia".into()] };
        let parts = Parts::new(&m, &extra).unwrap();
        let ids = embedder.input_ids(&parts).unwrap();
        assert!(ids.len() <= MAX_TOKENS);
        let decoded = embedder.tokenizer.decode(&ids.iter().map(|i| *i as u32).collect::<Vec<_>>(), false).unwrap();
        assert!(decoded.contains("psychological horror") && decoded.contains("input budget fixture"));
        assert_eq!(embedder.embed(&parts).unwrap().len(), DIM);
        let mut empty = m.clone();
        empty.description.clear();
        assert!(Parts::new(&empty, &extra).is_some(), "Keywords can stand in for a missing synopsis");
        assert!(Parts::new(&empty, &picks_metadata::Extra::default()).is_none());
    }
    #[test]
    fn packed_vectors_round_trip_and_bad_ones_are_rejected() {
        let vector = unit(&(0..DIM).map(|i| (i as f32 * 0.37).sin()).collect::<Vec<_>>()).unwrap();
        let restored = unpack(&pack(&vector)).unwrap();
        assert!(cosine(&vector, &restored) > 0.99999);
        assert!(unpack("AAAA").is_none() && unpack("not base64!").is_none());
        assert!(unit(&vec![0.0; DIM]).is_err() && unit(&vec![f32::NAN; DIM]).is_err() && unit(&[1.0]).is_err());
    }
    #[test]
    fn cache_evicts_the_oldest_entry_and_keeps_order_across_save() {
        let mut cache = Cache::default();
        let vector = unit(&vec![1.0; DIM]).unwrap();
        for i in 0..=MAX_ENTRIES { cache.insert(format!("k{i:05}"), vector.clone()); }
        assert!(!cache.contains_key("k00000") && cache.contains_key(&format!("k{MAX_ENTRIES:05}")));
        let path = std::env::temp_dir().join(format!("veylo-embeddings-{}.json", std::process::id()));
        std::fs::write(&path, cache.encode().to_string()).unwrap();
        let mut loaded = read_cache(&path);
        std::fs::remove_file(path).unwrap();
        assert_eq!(loaded.map.len(), MAX_ENTRIES);
        loaded.insert("new".into(), vector);
        assert!(!loaded.contains_key("k00001") && loaded.contains_key("k00002"));
    }
    #[test]
    fn mood_anchors_stay_pinned_while_titles_and_typed_queries_come_and_go() {
        let vector = unit(&vec![1.0; DIM]).unwrap();
        let mood = query_key(crate::brain::MOODS[0].1);
        let typed = |i: usize| format!("{MODEL_VERSION}-q-typed{i:05}");
        let mut cache = Cache::default();
        cache.insert(mood.clone(), vector.clone());
        for i in 0..MAX_ENTRIES + 50 { cache.insert(format!("k{i:05}"), vector.clone()); }
        assert!(cache.contains_key(&mood), "a flood of titles must not evict a mood anchor");
        assert_eq!(cache.map.len(), MAX_ENTRIES);
        // Typed queries have their own bound, and never push the mood out either.
        for i in 0..MAX_QUERIES + 20 { cache.insert(typed(i), vector.clone()); }
        assert!(cache.contains_key(&mood) && cache.queries.len() == MAX_QUERIES);
        assert!(!cache.contains_key(&typed(0)) && cache.contains_key(&typed(MAX_QUERIES + 19)));
        // The anchors come back from the file with the titles.
        let path = std::env::temp_dir().join(format!("veylo-embeddings-pinned-{}.json", std::process::id()));
        std::fs::write(&path, cache.encode().to_string()).unwrap();
        let loaded = read_cache(&path);
        std::fs::remove_file(path).unwrap();
        assert!(loaded.contains_key(&mood) && loaded.map.len() == MAX_ENTRIES && loaded.queries.len() == MAX_QUERIES);
    }
    #[test]
    fn titles_looked_up_recently_outlive_older_ones() {
        let mut cache = Cache::default();
        let vector = unit(&vec![1.0; DIM]).unwrap();
        for i in 0..MAX_ENTRIES { cache.insert(format!("k{i:05}"), vector.clone()); }
        assert!(cache.get("k00000").is_some(), "a lookup counts as use");
        cache.insert("fresh".into(), vector.clone());
        assert!(cache.contains_key("k00000") && !cache.contains_key("k00001") && cache.contains_key("fresh"));
        // The recency survives a save and load.
        let path = std::env::temp_dir().join(format!("veylo-embeddings-lru-{}.json", std::process::id()));
        std::fs::write(&path, cache.encode().to_string()).unwrap();
        let mut loaded = read_cache(&path);
        std::fs::remove_file(path).unwrap();
        loaded.insert("another".into(), vector);
        assert!(loaded.contains_key("k00000") && !loaded.contains_key("k00002"));
    }
    #[test]
    fn cache_key_depends_on_model_and_input() {
        let extra = picks_metadata::Extra::default();
        let key = |synopsis: &str| Parts::new(&fixture("Same", "2020", synopsis), &extra).unwrap().key();
        assert_ne!(key("one synopsis that is long enough"), key("another synopsis that is long enough"));
        assert!(key("one synopsis that is long enough").starts_with(MODEL_VERSION));
    }
}
