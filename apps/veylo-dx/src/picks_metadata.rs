//! Public title metadata for local classification (keyless Wikidata subjects and genres).
//! Fixed provider endpoints only; cache contains
//! hashed identities and cleaned keywords/summaries, never provider URLs or credentials.
use crate::backend::{self, Media};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashMap}, path::Path, sync::{Arc, Mutex, OnceLock, RwLock, Weak}};

const VERSION: u32 = 1;
const LIMIT: usize = 1024;
const WEEK: i64 = 7 * 86400;
const RETRY: i64 = 300;
#[derive(Clone, Default)]
pub struct Extra {
    pub keywords: Vec<String>,
}
#[derive(Clone, Default)]
struct Entry { extra: Extra, keywords_due: i64, touched: i64 }
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
    /// TMDB answers with a key, Wikidata without one, so there is always a source to ask.
    fn needs_keywords(&self, now: i64) -> bool { now >= self.keywords_due }
    fn update(&mut self, keywords: Option<Vec<String>>, now: i64) {
        match keywords {
            Some(fetched) => { self.keywords_due = now + if fetched.is_empty() { 86400 } else { WEEK }; self.extra.keywords = fetched; }
            None => { self.keywords_due = now + RETRY; } // preserve a stale successful answer while offline
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
        if !entry.needs_keywords(now) { return; }
        let Ok(_permit) = GATE.acquire().await else { return };
        entry.update(backend::picks_keywords(&media).await, now);
        // Only the entry is changed under the lock (Picks reads the store all the time); the file
        // follows a few seconds later, one write for every title enriched meanwhile.
        if let Ok(mut store) = store.write() {
            if !store.contains_key(&id) && store.len() >= LIMIT {
                if let Some(oldest) = store.iter().min_by_key(|(_, e)| e.touched).map(|(k, _)| k.clone()) { store.remove(&oldest); }
            }
            store.insert(id, entry);
        }
        save_soon();
    }).await;
}

/// Write the store once a burst of enrichment is over, encoding it under a read lock on a thread
/// of its own (one save at a time: the next waits for the next burst).
fn save_soon() {
    static PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if PENDING.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        PENDING.store(false, std::sync::atomic::Ordering::Release);
        let Some(text) = STORE.get().and_then(|s| s.read().ok().map(|s| encode_store(&s).to_string())) else { return };
        let _ = std::fs::write(backend::backend_data_dir().join("picks-metadata-v1.json"), text);
    });
}

fn encode_store(store: &Store) -> Value {
    let entries: BTreeMap<_, _> = store.iter().map(|(k,e)| (k.clone(), json!({"keywords":e.extra.keywords,
        "keywords_due":e.keywords_due,"touched":e.touched}))).collect();
    json!({"version":VERSION,"entries":entries})
}
fn read_store(path: &Path) -> Store {
    let v: Value = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if v["version"] != VERSION { return Store::new(); }
    v["entries"].as_object().into_iter().flatten().filter(|(k,_)| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit())).take(LIMIT).map(|(k,e)| {
        let strings = |field: &str, limit, chars| e[field].as_array().into_iter().flatten().filter_map(Value::as_str).map(|s| clean(s,chars)).filter(|s| !s.is_empty()).take(limit).collect();
        (k.clone(), Entry { extra: Extra { keywords: strings("keywords",24,80) },
            keywords_due: e["keywords_due"].as_i64().unwrap_or(0), touched: e["touched"].as_i64().unwrap_or(0) })
    }).collect()
}

/// Remove markup and decode common named/numeric entities before classification and caching.
pub fn clean(input: &str, max_chars: usize) -> String {
    let mut plain = String::new();
    let mut tag = false;
    let mut chars = input.chars().take(16000).peekable();
    while let Some(c) = chars.next() {
        // Only "<" followed by a tag-like character opens markup; "I <3 you" stays text.
        match c {
            '<' if !tag && chars.peek().is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!')) => { tag = true; plain.push(' '); }
            '>' if tag => tag = false,
            _ if !tag => plain.push(c),
            _ => {}
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleans_html_tags_and_entities() {
        assert_eq!(clean("I <3 you, a < b > c",100),"I <3 you, a < b > c");
        assert_eq!(clean("<p>Bob&#39;s <b>family</b>&nbsp;&amp; &#x66;riends.</p>",100),"Bob's family & friends.");
    }
    #[test]
    fn expiry_and_offline_refresh_preserve_data() {
        let mut e = Entry::default();
        assert!(e.needs_keywords(100));
        e.update(Some(vec!["dark".into()]),100);
        assert!(!e.needs_keywords(101));
        assert!(e.needs_keywords(100+WEEK));
        e.update(None,100+WEEK);
        assert_eq!(e.extra.keywords,["dark"]);
        assert!(!e.needs_keywords(101+WEEK));
        assert!(e.needs_keywords(100+WEEK+RETRY));
    }
    #[test]
    fn cache_roundtrip_uses_an_isolated_file_and_rejects_other_versions() {
        let path = std::env::temp_dir().join(format!("veylo-metadata-{}-{}.json",std::process::id(),backend::now_secs()));
        let mut store = Store::new();
        let mut e = Entry::default();e.update(Some(vec!["dystopia".into()]),123);
        store.insert("a".repeat(64),e);
        let mut value = encode_store(&store);
        std::fs::write(&path,value.to_string()).unwrap();
        assert_eq!(read_store(&path)[&"a".repeat(64)].extra.keywords,["dystopia"]);
        value["version"]=json!(99);std::fs::write(&path,value.to_string()).unwrap();
        assert!(read_store(&path).is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
