//! Public title metadata for local classification. Fixed provider endpoints only; cache contains
//! hashed identities and cleaned keywords/summaries, never provider URLs or credentials.
use crate::backend::{self, Media};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashMap, HashSet}, path::Path, sync::{Arc, Mutex, OnceLock, RwLock, Weak}};

const VERSION: u32 = 1;
const LIMIT: usize = 1024;
const WEEK: i64 = 7 * 86400;
const RETRY: i64 = 300;
#[derive(Clone, Default)]
pub struct Extra {
    pub keywords: Vec<String>,
    pub episodes: Vec<String>,
}
#[derive(Clone, Default)]
struct Entry { extra: Extra, keywords_due: i64, episodes_due: i64, touched: i64 }
type Store = BTreeMap<String, Entry>;
static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
type FetchLocks = HashMap<String, Weak<tokio::sync::Mutex<()>>>;
static LOCKS: OnceLock<Mutex<FetchLocks>> = OnceLock::new();
static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

fn identity(m: &Media) -> String {
    format!("{:x}", Sha256::digest(format!("{}|{}|{}|{}|{}", m.kind, m.id, m.name, m.year, m.addon_url).as_bytes()))
}
pub fn cached(m: &Media) -> Extra {
    STORE.get().and_then(|s| s.read().ok()?.get(&identity(m)).map(|e| e.extra.clone())).unwrap_or_default()
}
pub async fn load() {
    if STORE.get().is_some() { return; }
    let _ = backend::on_runtime(async {
        tokio::task::spawn_blocking(|| {
            STORE.get_or_init(|| RwLock::new(read_store(&backend::backend_data_dir().join("picks-metadata-v1.json"))));
        }).await
    }).await;
}
fn title_lock(key: &str) -> Option<Arc<tokio::sync::Mutex<()>>> {
    let mut locks = LOCKS.get_or_init(Default::default).lock().ok()?;
    locks.retain(|_, v| v.strong_count() > 0);
    if let Some(lock) = locks.get(key).and_then(Weak::upgrade) { return Some(lock); }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key.into(), Arc::downgrade(&lock));
    Some(lock)
}
impl Entry {
    fn needs_keywords(&self, now: i64, key_present: bool) -> bool { key_present && now >= self.keywords_due }
    fn needs_episodes(&self, now: i64, series: bool) -> bool { series && now >= self.episodes_due }
    fn update(&mut self, keywords: Option<Option<Vec<String>>>, episodes: Option<Option<Vec<String>>>, now: i64) {
        for (result, values, due) in [(keywords, &mut self.extra.keywords, &mut self.keywords_due), (episodes, &mut self.extra.episodes, &mut self.episodes_due)] {
            if let Some(result) = result {
                match result {
                    Some(fetched) => { *due = now + if fetched.is_empty() { 86400 } else { WEEK }; *values = fetched; }
                    None => { *due = now + RETRY; } // preserve a stale successful answer while offline
                }
            }
        }
        self.touched = now;
    }
}

/// Refresh asynchronously from Picks prefetch, never on the path that makes a card playable.
/// Per-title deduplication and two concurrent titles keep lookups bounded across card checks.
pub async fn enrich(media: &Media) {
    load().await;
    let media = media.clone();
    let _ = backend::on_runtime(async move {
        let id = identity(&media);
        let Some(lock) = title_lock(&id) else { return };
        let _title = lock.lock().await;
        let Some(store) = STORE.get() else { return };
        let mut entry = store.read().ok().and_then(|s| s.get(&id).cloned()).unwrap_or_default();
        let now = backend::now_secs();
        let want_keywords = entry.needs_keywords(now, backend::tmdb_ready());
        let want_episodes = entry.needs_episodes(now, media.kind != "movie");
        if !want_keywords && !want_episodes { return; }
        let Ok(_permit) = GATE.acquire().await else { return };
        let (keywords, episodes) = tokio::join!(
            async { if want_keywords { Some(backend::picks_keywords(&media).await) } else { None } },
            async { if want_episodes { Some(backend::picks_episode_summaries(&media).await) } else { None } },
        );
        entry.update(keywords, episodes, now);
        // Serialize writes on the blocking pool, so concurrent refreshes cannot overwrite a
        // newer snapshot or leave a partially interleaved JSON document.
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(mut store) = store.write() {
                if !store.contains_key(&id) && store.len() >= LIMIT {
                    if let Some(oldest) = store.iter().min_by_key(|(_, e)| e.touched).map(|(k, _)| k.clone()) { store.remove(&oldest); }
                }
                store.insert(id, entry);
                let _ = std::fs::write(backend::backend_data_dir().join("picks-metadata-v1.json"), encode_store(&store).to_string());
            }
        }).await;
    }).await;
}

fn encode_store(store: &Store) -> Value {
    let entries: BTreeMap<_, _> = store.iter().map(|(k,e)| (k.clone(), json!({"keywords":e.extra.keywords,"episodes":e.extra.episodes,
        "keywords_due":e.keywords_due,"episodes_due":e.episodes_due,"touched":e.touched}))).collect();
    json!({"version":VERSION,"entries":entries})
}
fn read_store(path: &Path) -> Store {
    let v: Value = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if v["version"] != VERSION { return Store::new(); }
    v["entries"].as_object().into_iter().flatten().filter(|(k,_)| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit())).take(LIMIT).map(|(k,e)| {
        let strings = |field: &str, limit, chars| e[field].as_array().into_iter().flatten().filter_map(Value::as_str).map(|s| clean(s,chars)).filter(|s| !s.is_empty()).take(limit).collect();
        (k.clone(), Entry { extra: Extra { keywords: strings("keywords",24,80), episodes: strings("episodes",4,600) },
            keywords_due: e["keywords_due"].as_i64().unwrap_or(0), episodes_due: e["episodes_due"].as_i64().unwrap_or(0), touched: e["touched"].as_i64().unwrap_or(0) })
    }).collect()
}

/// Remove markup and decode common named/numeric entities before classification and caching.
pub fn clean(input: &str, max_chars: usize) -> String {
    let mut plain = String::new();
    let mut tag = false;
    for c in input.chars().take(16000) {
        match c { '<' => { tag = true; plain.push(' '); }, '>' => tag = false, _ if !tag => plain.push(c), _ => {} }
    }
    for (a,b) in [("&nbsp;"," "),("&quot;","\""),("&apos;","'"),("&lt;","<"),("&gt;",">"),("&amp;","&")] { plain = plain.replace(a,b); }
    static ENTITY: OnceLock<regex::Regex> = OnceLock::new();
    let numeric = ENTITY.get_or_init(|| regex::Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").expect("entity regex"));
    let decoded = numeric.replace_all(&plain, |caps: &regex::Captures| {
        let digits = &caps[1];
        let number = if let Some(hex) = digits.strip_prefix('x') { u32::from_str_radix(hex,16).ok() } else { digits.parse::<u32>().ok() };
        number.and_then(char::from_u32).map(|c| c.to_string()).unwrap_or_default()
    });
    decoded.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max_chars).collect()
}

pub fn keywords(payload: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    payload["keywords"].as_array().or_else(|| payload["results"].as_array()).into_iter().flatten()
        .filter_map(|k| k["name"].as_str()).map(|s| clean(s,80)).filter(|s| !s.is_empty() && seen.insert(s.to_lowercase())).take(24).collect()
}
pub fn episode_sample(payload: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut episodes: Vec<_> = payload.as_array().into_iter().flatten().filter_map(|e| {
        let season = e["season"].as_i64()?;
        let number = e["number"].as_i64()?;
        if season <= 0 || number <= 0 { return None; }
        let summary = clean(e["summary"].as_str()?,600);
        (summary.chars().count() >= 30).then_some((season,number,summary))
    }).collect();
    episodes.sort_by_key(|(s,n,_)| (*s,*n));
    episodes.retain(|(_,_,s)| seen.insert(s.to_lowercase()));
    let count = episodes.len().min(4);
    (0..count).map(|i| episodes[if count == 1 { 0 } else { i * (episodes.len()-1) / (count-1) }].2.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handles_both_keyword_shapes_and_html_entities() {
        let rows = json!([{"name":"psychological horror"},{"name":"Psychological Horror"},{"name":"<b>dark</b> &amp; disturbing"}]);
        assert_eq!(keywords(&json!({"keywords": rows})), keywords(&json!({"results":rows})));
        assert_eq!(keywords(&json!({"results":rows})).len(),2);
        assert_eq!(clean("<p>Bob&#39;s <b>family</b>&nbsp;&amp; &#x66;riends.</p>",100),"Bob's family & friends.");
    }
    #[test]
    fn episodes_span_the_series_skip_specials_and_deduplicate() {
        let rows: Vec<_> = (0..12).rev().map(|i| json!({"season":i/4+1,"number":i%4+1,"summary":format!("<p>Distinct episode {i}: a sufficiently detailed synopsis with &amp; entities.</p>")})).chain([
            json!({"season":0,"number":1,"summary":"A special that must not become the first regular episode summary."}),
            json!({"season":4,"number":1,"summary":null}),
        ]).collect();
        let sample = episode_sample(&json!(rows));
        assert_eq!(sample.len(),4);
        assert!(sample[0].contains("episode 0:"));
        assert!(sample[3].contains("episode 11:"));
        assert!(sample.iter().all(|s| !s.contains('<') && s.contains('&')));
        assert!(episode_sample(&json!([])).is_empty());
    }
    #[test]
    fn expiry_key_enabling_and_offline_refresh_preserve_data() {
        let mut e = Entry::default();
        assert!(!e.needs_keywords(100,false));
        assert!(e.needs_keywords(100,true));
        assert!(!e.needs_episodes(100,false));
        e.update(Some(Some(vec!["dark".into()])),Some(Some(vec!["Episode summary".into()])),100);
        assert!(!e.needs_keywords(101,true));
        assert!(e.needs_keywords(100+WEEK,true));
        e.update(Some(None),Some(None),100+WEEK);
        assert_eq!(e.extra.keywords,["dark"]);
        assert_eq!(e.extra.episodes,["Episode summary"]);
        assert!(!e.needs_keywords(101+WEEK,true));
        assert!(e.needs_keywords(100+WEEK+RETRY,true));
    }
    #[test]
    fn cache_roundtrip_uses_an_isolated_file_and_rejects_other_versions() {
        let path = std::env::temp_dir().join(format!("veylo-metadata-{}-{}.json",std::process::id(),backend::now_secs()));
        let mut store = Store::new();
        let mut e = Entry::default();e.update(Some(Some(vec!["dystopia".into()])),None,123);
        store.insert("a".repeat(64),e);
        let mut value = encode_store(&store);
        std::fs::write(&path,value.to_string()).unwrap();
        assert_eq!(read_store(&path)[&"a".repeat(64)].extra.keywords,["dystopia"]);
        value["version"]=json!(99);std::fs::write(&path,value.to_string()).unwrap();
        assert!(read_store(&path).is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
