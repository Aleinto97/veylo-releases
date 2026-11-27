//! Disk cache of everything a title page shows (stale-while-revalidate): re-opening a title —
//! even after a restart — paints its final layout from here at once, while the live ops refresh
//! it in the background. One small JSON file per title under <data>/title-cache/.

use crate::backend;
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const MAX_FILES: usize = 1500;

fn dir() -> PathBuf { backend::backend_data_dir().join("title-cache") }

fn hash(key: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn file(key: &str) -> PathBuf { dir().join(format!("{}.json", hash(key))) }

/// One writer at a time; writes of the same part keep their call order (a sequence number per
/// title+part drops a write that lost the race to a newer one).
static WRITE: std::sync::LazyLock<Mutex<HashMap<String, u64>>> = std::sync::LazyLock::new(Default::default);
static SEQ: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU64 = AtomicU64::new(0);

/// Add-on payloads can carry their whole response (`raw`); the page never needs it on disk.
fn strip(v: &mut Value) {
    match v {
        Value::Object(o) => { o.remove("raw"); o.values_mut().for_each(strip); }
        Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

/// Everything stored for `key` (parts: details, scores, more, studios, extras).
pub fn read(key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(file(key)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    // Only a hash of the key is kept on disk (the key embeds the add-on's URL).
    (v["key"].as_str() == Some(hash(key).as_str())).then_some(v)
}

/// Store one part of a title's page off the async runtime (atomic replace).
pub fn put(key: &str, part: &'static str, value: &Value) {
    let (key, mut value) = (key.to_owned(), value.clone());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        let Ok(mut latest) = WRITE.lock() else { return };
        let slot = format!("{}/{part}", hash(&key));
        if latest.get(&slot).is_some_and(|&s| s > seq) { return; } // a newer write already landed
        latest.insert(slot, seq);
        if latest.len() > 4096 { latest.clear(); }
        let d = dir();
        if std::fs::create_dir_all(&d).is_err() { return; }
        strip(&mut value);
        let mut doc = read(&key).unwrap_or_else(|| json!({ "key": hash(&key) }));
        doc[part] = value;
        doc["at"] = json!(backend::now_secs());
        let path = file(&key);
        let tmp = path.with_extension(format!("{}.{seq}.tmp", std::process::id()));
        if std::fs::write(&tmp, doc.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &path); } else { let _ = std::fs::remove_file(&tmp); }
        if WRITES.fetch_add(1, Ordering::Relaxed) % 64 == 0 { prune(&d); }
    });
}

fn prune(d: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd.flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= MAX_FILES { return; }
    files.sort();
    for (_, p) in files.iter().take(files.len() - MAX_FILES + 100) { let _ = std::fs::remove_file(p); }
}
