//! Local GLiClass title traits. Optional public metadata is fetched separately in the background;
//! inference never calls an AI service or exposes provider URLs or credentials.
//! One bounded CPU session is shared by background checks on every platform.
use crate::{backend::{self, Media}, picks_metadata};
use anyhow::{anyhow, ensure, Context, Result};
use ort::{session::Session, value::Tensor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Mutex, OnceLock, RwLock}};
use tokenizers::Tokenizer;

pub const MODEL_VERSION: &str = "gliclass-21edefaf-int8-enriched-v2";
/// Input enrichment changes cache identity, but the label vocabulary and learned weights stay
/// compatible with the first local classifier release.
pub const TRAITS_VERSION: &str = "gliclass-21edefaf-int8-traits-v1";
// Changing labels, prompt construction or normalization requires a new cache version.
pub const LABELS: [&str; 18] = [
    "humor and comedy", "suspense and tension", "horror and fear", "romance and love",
    "family relationships", "friendship", "politics and power", "crime and investigation",
    "science fiction", "fantasy and magic", "action and adventure", "war and conflict",
    "hope and inspiration", "tragedy and grief", "psychological drama", "survival",
    "a lighthearted comforting story", "a dark disturbing story",
];
const MAX_ENTRIES: usize = 2048;
type Scores = Vec<f32>;
static CACHE: OnceLock<RwLock<BTreeMap<String, Scores>>> = OnceLock::new();
static CLASSIFIER: Mutex<Option<Result<Classifier, String>>> = Mutex::new(None);
static FAILURE: Mutex<Option<String>> = Mutex::new(None);

fn cache() -> &'static RwLock<BTreeMap<String, Scores>> { CACHE.get_or_init(Default::default) }

/// Hash only inference input; never put a provider URL, token or raw metadata in this cache.
fn sections(m: &Media, extra: &picks_metadata::Extra) -> Vec<String> {
    let description = picks_metadata::clean(&m.description,4000);
    if description.chars().count() < 30 && extra.keywords.is_empty() && extra.episodes.is_empty() { return Vec::new(); }
    let genres = m.genres.iter().take(8).map(|g| picks_metadata::clean(g,40)).collect::<Vec<_>>().join(", ");
    let mut parts = vec![format!("{} ({}). {}.",picks_metadata::clean(&m.name,256),picks_metadata::clean(&m.year,64),genres)];
    if !description.is_empty() { parts.push(format!("Synopsis: {description}")); }
    if !extra.keywords.is_empty() { parts.push(format!("Keywords: {}.",extra.keywords.join(", "))); }
    for episode in &extra.episodes { parts.push(format!("Episode summary: {episode}")); }
    parts
}
fn text(m: &Media) -> Option<String> {
    let parts = sections(m, &picks_metadata::cached(m));
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn key(input: &str) -> String {
    format!("{MODEL_VERSION}-{:x}", Sha256::digest(input.as_bytes()))
}
pub fn scores(m: &Media) -> Option<Scores> {
    let input = text(m)?;
    let cache = cache().read().ok()?;
    cache.get(&key(&input)).cloned().or_else(|| {
        // Continue using a base prediction while refreshed enrichment is being classified.
        let base = sections(m,&picks_metadata::Extra::default());
        cache.get(&key(&base.join("\n"))).cloned()
    })
}
/// How closely a challenger follows the title just kept. Genre/type are deliberately absent:
/// two animated series can have opposite moods despite sharing most catalogue metadata.
pub struct TraitMatch {
    pub similarity: f64,
    pub follows: bool,
}
pub fn match_traits(kept: &Media, candidate: &Media) -> Option<TraitMatch> {
    match_scores(&scores(kept)?, &scores(candidate)?)
}
fn match_scores(kept: &[f32], candidate: &[f32]) -> Option<TraitMatch> {
    if kept.len() != LABELS.len() || candidate.len() != LABELS.len() { return None; }
    // Mood takes precedence over topic: sci-fi and dark can be nearly tied, and selecting
    // the numerically highest of all labels would switch directions across CPU kernels.
    const MOODS: [usize; 5] = [0, 1, 2, 16, 17];
    let mood = MOODS.into_iter().max_by(|a, b| kept[*a].total_cmp(&kept[*b]))?;
    let mood_total: f32 = MOODS.iter().map(|i| kept[*i]).sum();
    let clear_mood = kept[mood] >= 0.07 && kept[mood] >= mood_total * 0.3;
    let dominant = if clear_mood { mood } else { (0..kept.len()).max_by(|a, b| kept[*a].total_cmp(&kept[*b]))? };
    let candidate_max = if clear_mood { MOODS.iter().map(|i| candidate[*i]).fold(0.0f32, f32::max) }
        else { candidate.iter().copied().fold(0.0f32, f32::max) };
    let dot: f64 = kept.iter().zip(candidate).map(|(a,b)| f64::from(*a) * f64::from(*b)).sum();
    let norm = (kept.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>()
        * candidate.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>()).sqrt();
    if norm <= 0.0 { return None; }
    // Only anchor to a distinctive trait, never an almost-uniform classifier output. Require
    // the same trait to be prominent in the challenger, relative to its own strongest trait.
    let follows = (clear_mood || kept[dominant] >= 2.0 / LABELS.len() as f32)
        && candidate[dominant] >= if clear_mood { 0.07 } else { 2.0 / LABELS.len() as f32 }
        && candidate[dominant] >= candidate_max * 0.65;
    Some(TraitMatch { similarity: (dot / norm).clamp(0.0, 1.0), follows })
}

#[cfg(test)]
pub(crate) fn cache_test_scores(media: &Media, values: Vec<f32>) {
    cache().write().unwrap().insert(key(&text(media).unwrap()), values);
}

pub fn failed() -> bool { FAILURE.lock().map(|f| f.is_some()).unwrap_or(true) }

fn paths() -> Result<(PathBuf, PathBuf)> {
    #[cfg(test)]
    {
        let dir = std::env::var_os("VEYLO_PICKS_AI_TEST_ASSETS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("picks-ai/model"));
        let target = if cfg!(target_os = "android") { "android-arm64" } else if cfg!(windows) { "windows-x64" } else if cfg!(target_arch = "aarch64") { "linux-aarch64" } else { "linux-x64" };
        let runtime = if std::env::var_os("VEYLO_PICKS_AI_TEST_ASSETS").is_some() { dir.join("libonnxruntime.so") }
            else { Path::new(env!("CARGO_MANIFEST_DIR")).join("picks-ai").join(target).join(if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" }) };
        return Ok((dir, runtime));
    }
    #[cfg(all(not(test), target_os = "android"))]
    { crate::android::picks_ai_paths() }
    #[cfg(all(not(test), not(target_os = "android")))]
    {
        let dir = std::env::current_exe()?.parent().context("Executable directory")?.join("picks-ai");
        let library = if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" };
        Ok((dir.clone(), dir.join("runtime").join(library)))
    }
}


struct Classifier { session: Session, tokenizer: Tokenizer }
impl Classifier {
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
        #[cfg(target_os = "android")]
        register_android_shutdown();
        let session = Session::builder()?.with_intra_threads(2)?.with_inter_threads(1)?
            .commit_from_file(dir.join("model.onnx"))?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("Tokenizer: {e}"))?;
        Ok(Self { session, tokenizer })
    }
    #[cfg(test)]
    fn classify(&mut self, input: &str) -> Result<Scores> {
        self.classify_sections(&[input.to_owned()])
    }
    fn input_ids(&self, parts: &[String]) -> Result<Vec<i64>> {
        // Reserve all label markers, then share the remaining budget between synopsis,
        // keywords and sampled episodes. A long synopsis cannot truncate the new evidence.
        let suffix = LABELS.iter().map(|label| format!("<<LABEL>>{label}")).collect::<String>() + "<<SEP>>";
        let suffix = self.tokenizer.encode(suffix,false).map_err(|e| anyhow!("Tokenization: {e}"))?;
        let budget = 384usize.saturating_sub(suffix.len()+2);
        let encoded: Vec<Vec<i64>> = parts.iter().map(|part| {
            let safe = part.replace("<<LABEL>>"," ").replace("<<SEP>>"," ");
            self.tokenizer.encode(safe,false).map(|e| e.get_ids().iter().map(|id| i64::from(*id)).collect())
                .map_err(|e| anyhow!("Tokenization: {e}"))
        }).collect::<Result<_>>()?;
        let mut lengths = vec![0;encoded.len()];
        let mut left = budget;
        while left > 0 {
            let before = left;
            for (part, len) in encoded.iter().zip(&mut lengths) {
                if left > 0 && *len < part.len() { *len += 1; left -= 1; }
            }
            if left == before { break; }
        }
        let mut ids = vec![1]; // DeBERTa [CLS]
        for (part,len) in encoded.iter().zip(lengths) { ids.extend(part.iter().take(len)); }
        ids.extend(suffix.get_ids().iter().map(|id| i64::from(*id)));
        ids.push(2); // DeBERTa [SEP]
        ensure!(ids.iter().filter(|id| **id == 128001).count() == LABELS.len(), "Invalid GLiClass label markers");
        Ok(ids)
    }
    fn classify_sections(&mut self, parts: &[String]) -> Result<Scores> {
        let ids = self.input_ids(parts)?;
        let len = ids.len();
        let outputs = self.session.run(ort::inputs![
            "input_ids" => Tensor::from_array(([1, len], ids))?,
            "attention_mask" => Tensor::from_array(([1, len], vec![1i64; len]))?,
        ])?;
        let (shape, logits) = outputs["logits"].try_extract_tensor::<f32>()?;
        ensure!(shape.as_ref() == [1, LABELS.len() as i64], "Unexpected GLiClass output shape");
        relative_scores(logits)
    }
}

/// ort keeps its Environment in a Rust static, which is not dropped at process exit. On
/// Android the native library's C++ static destructors can then destroy a mutex before the
/// environment releases it. Register after dlopen, so cleanup runs before those destructors.
#[cfg(target_os = "android")]
fn register_android_shutdown() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        extern "C" fn shutdown() {
            use ort::AsPointer;
            if let Ok(mut state) = CLASSIFIER.lock() {
                // Block queued workers from creating another session during process shutdown.
                let classifier = state.replace(Err("Local inference is shutting down".into()));
                drop(classifier);
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

/// Relative affinities, not calibrated independent probabilities. Softmax at temperature 2
/// retains distinctions that the shipped model's large logits lose under plain sigmoid.
fn relative_scores(logits: &[f32]) -> Result<Scores> {
    ensure!(logits.len() == LABELS.len() && logits.iter().all(|x| x.is_finite()), "Invalid GLiClass scores");
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut scores: Vec<f32> = logits.iter().map(|x| ((x - max) / 2.0).exp()).collect();
    let sum: f32 = scores.iter().sum();
    for score in &mut scores { *score /= sum; }
    Ok(scores)
}

fn read_cache(path: &Path) -> BTreeMap<String, Scores> {
    let value: Value = std::fs::read_to_string(path).ok().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    if value["version"] != MODEL_VERSION { return BTreeMap::new(); }
    value["entries"].as_object().into_iter().flatten().take(MAX_ENTRIES).filter_map(|(key, value)| {
        let scores: Vec<f32> = value.as_array()?.iter().map(|x| x.as_f64().map(|x| x as f32)).collect::<Option<_>>()?;
        let valid = scores.len() == LABELS.len() && scores.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x));
        valid.then(|| (key.clone(), scores))
    }).collect()
}

/// Called only from title verification. Serialize inference off the UI executor; cached titles
/// never rerun. A failed runtime is reported to the UI and retried on the next app launch.
pub async fn classify(m: &Media) -> Result<()> {
    picks_metadata::load().await;
    let parts = sections(m,&picks_metadata::cached(m));
    if parts.is_empty() { return Ok(()); }
    let input = parts.join("\n");
    if cache().read().ok().is_some_and(|c| c.contains_key(&key(&input))) { return Ok(()); }
    let result = backend::on_runtime(async move {
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut state = CLASSIFIER.lock().map_err(|_| anyhow!("Classifier lock"))?;
            let cache_path = backend::backend_data_dir().join("picks-traits-v1.json");
            if state.is_none() {
                *cache().write().map_err(|_| anyhow!("Trait cache lock"))? = read_cache(&cache_path);
                *state = Some(paths().and_then(|(dir, runtime)| Classifier::open(&dir, &runtime)).map_err(|e| e.to_string()));
            }
            let classifier = state.as_mut().unwrap().as_mut().map_err(|e| anyhow!(e.clone()))?;
            let id = key(&input);
            if cache().read().map_err(|_| anyhow!("Trait cache lock"))?.contains_key(&id) { return Ok(()); }
            let values = classifier.classify_sections(&parts)?;
            let serialized = {
                let mut cache = cache().write().map_err(|_| anyhow!("Trait cache lock"))?;
                if cache.len() >= MAX_ENTRIES { cache.pop_first(); }
                cache.insert(id, values);
                json!({"version": MODEL_VERSION, "entries": &*cache}).to_string()
            };
            std::fs::write(cache_path, serialized).context("Save title traits")?;
            Ok(())
        }).await.map_err(|_| anyhow!("Inference worker stopped"))?
    }).await?;
    if let Err(error) = &result { if let Ok(mut failure) = FAILURE.lock() { *failure = Some(error.to_string()); } }
    result
}

/// Metadata requests stay outside verification/playback; inference itself remains local.
pub async fn enrich_and_classify(m: &Media) -> Result<()> {
    picks_metadata::enrich(m).await;
    classify(m).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn affinities_remain_finite_and_reject_bad_output() {
        let mut logits = vec![1000.0; LABELS.len()];
        logits[0] = 1004.0;
        let scores = relative_scores(&logits).unwrap();
        assert!(scores[0] > scores[1]);
        assert!((scores.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(relative_scores(&[f32::NAN; 18]).is_err());
    }
    #[test]
    fn bundled_model_classifies_and_preserves_labels_for_long_text() {
        let (dir, runtime) = paths().unwrap();
        let mut classifier = Classifier::open(&dir, &runtime).unwrap();
        let politics = classifier.classify("The senate votes in an election to replace the prime minister amid a political scandal.").unwrap();
        let comedy = classifier.classify("A group of friends get drunk at a bachelor party, wake up with no memory, and retrace their steps through hilarious disasters.").unwrap();
        assert!(politics[6] > comedy[6], "Politics should reflect the synopsis");
        assert!(comedy[0] > politics[0], "Comedy should reflect the synopsis");
        let dark = classifier.classify("Love, Death & Robots (2019–2025). Animation, Sci-Fi & Fantasy. Terrifying creatures, wicked surprises and dark comedy converge in this NSFW anthology of animated stories presented by Tim Miller and David Fincher.").unwrap();
        let similar = classifier.classify("Castlevania (2017–2021). Animation, Action & Adventure, Sci-Fi & Fantasy. A vampire hunter fights to save a besieged city from an army of otherworldly creatures controlled by Dracula.").unwrap();
        let light = classifier.classify("Bob's Burgers (2011–). Animation, Comedy. Bob's Burgers follows a third-generation restaurateur, Bob, as he runs Bob's Burgers with the help of his wife and their three kids. Bob and his quirky family have big ideas about burgers, but fall short on service and sophistication.").unwrap();
        let matched = match_scores(&dark, &similar).unwrap();
        let unrelated = match_scores(&dark, &light).unwrap();
        for (title, scores) in [("Love, Death & Robots", &dark), ("Castlevania", &similar), ("Bob's Burgers", &light)] {
            let mut ranked: Vec<_> = LABELS.iter().zip(scores).collect();
            ranked.sort_by(|a,b| b.1.total_cmp(a.1));
            println!("{title}: {:?}", &ranked[..4]);
        }
        assert!(matched.follows && !unrelated.follows, "Dark match: {}, comedy match: {}", matched.follows, unrelated.follows);
        assert!(matched.similarity > unrelated.similarity);
        let extra = picks_metadata::Extra { keywords: vec!["psychological horror".into(),"dystopia".into()], episodes: vec!["A vampire threatens a city in a dark and disturbing nightmare.".into();4] };
        let mut m = Media::from_json(&json!({"id":"test:input-budget","type":"series","name":"Input budget fixture","description":"Long synopsis. ".repeat(2000)}),"fixture").unwrap();
        let parts = sections(&m,&extra);
        let ids = classifier.input_ids(&parts).unwrap();
        assert!(ids.len() <= 384);
        let decoded = classifier.tokenizer.decode(&ids.iter().map(|i| *i as u32).collect::<Vec<_>>(),false).unwrap();
        assert!(decoded.contains("psychological horror") && decoded.contains("vampire"),"All evidence needs a token budget");
        assert_eq!(classifier.classify_sections(&parts).unwrap().len(),LABELS.len());
        let base = key(&sections(&m,&picks_metadata::Extra::default()).join("\n"));
        assert_ne!(base,key(&parts.join("\n")));
        m.description.clear();
        assert!(!sections(&m,&extra).is_empty(),"Enrichment can classify a missing synopsis");
        assert_eq!(classifier.classify(&"An adventure. ".repeat(1000)).unwrap().len(), LABELS.len());
    }
    #[test]
    fn cache_version_and_input_both_matter() {
        assert_ne!(key("one synopsis"), key("another synopsis"));
        assert!(key("synopsis").starts_with(MODEL_VERSION));
    }
}
