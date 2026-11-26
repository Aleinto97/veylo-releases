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
        if !entry.needs_keywords(now) || backed_off(&id, now) { return; }
        let Ok(_permit) = GATE.acquire().await else { return };
        entry.update(backend::picks_keywords(&media).await, now);
        // Only the entry is changed under the lock (Picks reads the store all the time); the file
        // follows a few seconds later, one write for every title enriched meanwhile.
        if let Ok(mut store) = store.write() {
            let due = entry.keywords_due;
            if !insert_bounded(&mut store, id.clone(), entry) {
                // Not worth a place in a full store, but not worth asking again straight away either.
                back_off(&id, due);
                return;
            }
        }
        save_soon();
    }).await;
}

/// Titles whose lookup found nothing and found no room in a full store: when to ask again.
static BACKOFF: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
fn backed_off(id: &str, now: i64) -> bool {
    BACKOFF.get().and_then(|b| b.lock().ok()).is_some_and(|b| b.get(id).is_some_and(|due| *due > now))
}
fn back_off(id: &str, due: i64) {
    if let Ok(mut b) = BACKOFF.get_or_init(Default::default).lock() {
        if b.len() >= 512 { b.clear(); }
        b.insert(id.to_owned(), due);
    }
}

/// Store an entry within the size limit. A title with nothing known (a lookup that failed or came
/// back empty) never pushes out one that has keywords: it takes the place of the oldest entry
/// that has none, and is not stored at all when every entry has some. Returns whether it was stored.
fn insert_bounded(store: &mut Store, id: String, entry: Entry) -> bool {
    if !store.contains_key(&id) && store.len() >= LIMIT {
        let has_nothing = |e: &Entry| e.extra.keywords.is_empty();
        let oldest = |only_empty: bool| store.iter().filter(|(_, e)| !only_empty || has_nothing(e)).min_by_key(|(_, e)| e.touched).map(|(k, _)| k.clone());
        let Some(victim) = oldest(true).or_else(|| if has_nothing(&entry) { None } else { oldest(false) }) else { return false };
        store.remove(&victim);
    }
    store.insert(id, entry);
    true
}

/// Write the store once a burst of enrichment is over, encoding it under a read lock on a single
/// background thread and writing atomically.
fn save_soon() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DIRTY: AtomicBool = AtomicBool::new(false);
    static RUNNING: AtomicBool = AtomicBool::new(false);
    DIRTY.store(true, Ordering::Release);
    if RUNNING.swap(true, Ordering::AcqRel) { return; }
    std::thread::spawn(|| {
        let path = backend::backend_data_dir().join("picks-metadata-v1.json");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            DIRTY.store(false, Ordering::Release);
            if let Some(text) = STORE.get().and_then(|s| s.read().ok().map(|s| encode_store(&s).to_string())) {
                let _ = backend::write_private_file(&path, text.as_bytes());
            }
            if !DIRTY.load(Ordering::Acquire) {
                RUNNING.store(false, Ordering::Release);
                if !DIRTY.load(Ordering::Acquire) || RUNNING.swap(true, Ordering::AcqRel) { break; }
            }
        }
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
/// Entities are decoded in a single pass, so "&amp;#39;" stays the text "&#39;" and is never
/// decoded twice; a reference to NUL or another control character is dropped.
pub fn clean(input: &str, max_chars: usize) -> String {
    let chars: Vec<char> = input.chars().take(16000).collect();
    let mut plain = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Only "<" followed by a tag-like character opens markup ("I <3 you" stays text), and only
        // when the tag is closed by ">" before another "<": an unclosed "<b" never swallows the rest.
        let opens = c == '<' && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!'));
        if opens {
            if let Some(end) = chars[i + 1..].iter().position(|x| matches!(x, '<' | '>')).filter(|p| chars[i + 1 + p] == '>') {
                plain.push(' ');
                i += end + 2;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    static ENTITY: OnceLock<regex::Regex> = OnceLock::new();
    let entity = ENTITY.get_or_init(|| regex::Regex::new(r"&(?:#([xX][0-9a-fA-F]{1,8}|[0-9]{1,10})|(nbsp|quot|apos|lt|gt|amp));").expect("entity regex"));
    let decoded = entity.replace_all(&plain, |caps: &regex::Captures| -> String {
        if let Some(digits) = caps.get(1).map(|d| d.as_str()) {
            let number = match digits.strip_prefix(['x', 'X']) { Some(hex) => u32::from_str_radix(hex, 16).ok(), None => digits.parse::<u32>().ok() };
            return number.and_then(char::from_u32).filter(|c| !c.is_control() || c.is_whitespace()).map(String::from).unwrap_or_default();
        }
        match &caps[2] { "nbsp" => " ", "quot" => "\"", "apos" => "'", "lt" => "<", "gt" => ">", _ => "&" }.to_owned()
    });
    decoded.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect::<String>()
        .split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max_chars).collect()
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
    #[test]
    fn entities_decode_once_and_unclosed_tags_keep_the_text() {
        assert_eq!(clean("&amp;#39; &amp;lt;b&amp;gt; &amp;amp;", 100), "&#39; &lt;b&gt; &amp;");
        assert_eq!(clean("A&#0;B &#x0; &#7; C", 100), "AB C");
        assert_eq!(clean("&#xD800; &#1114112; ok", 100), "ok");
        assert_eq!(clean("Great show <b and more text", 100), "Great show <b and more text");
        assert_eq!(clean("a <b and c <i>text</i> end", 100), "a <b and c text end");
        assert_eq!(clean("<p>one</p><p>two", 100), "one two");
        assert_eq!(clean("Fish &AMP; Chips &lt;3", 100), "Fish &AMP; Chips <3");
    }
    #[test]
    fn failed_lookups_never_evict_titles_that_have_keywords() {
        let mut store = Store::new();
        let known = |touched| { let mut e = Entry::default(); e.update(Some(vec!["dark".into()]), touched); e };
        for i in 0..LIMIT { store.insert(format!("{i:064}"), known(1000 + i as i64)); }
        let failed = { let mut e = Entry::default(); e.update(None, 5000); e };
        // Every entry has keywords: the failed lookup is not stored and nothing is lost.
        assert!(!insert_bounded(&mut store, "f".repeat(64), failed.clone()));
        assert_eq!(store.len(), LIMIT);
        assert!(store.contains_key(&format!("{:064}", 0)));
        // A real answer still replaces the oldest one.
        assert!(insert_bounded(&mut store, "a".repeat(64), known(6000)));
        assert!(!store.contains_key(&format!("{:064}", 0)) && store.contains_key(&"a".repeat(64)) && store.len() == LIMIT);
        // With a keyword-less entry present, a failed lookup replaces only that one.
        let key = format!("{:064}", 1);
        store.insert(key.clone(), failed.clone());
        assert!(insert_bounded(&mut store, "b".repeat(64), failed));
        assert!(!store.contains_key(&key) && store.contains_key(&"b".repeat(64)) && store.len() == LIMIT);
        assert!(store.contains_key(&format!("{:064}", 2)));
    }
}
