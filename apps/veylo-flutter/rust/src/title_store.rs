//! Disk cache of everything a title page shows (stale-while-revalidate): re-opening a title —
//! even after a restart — paints its final layout from here at once, while the live ops refresh
//! it in the background. One small JSON file per title under <data>/title-cache/.

use crate::backend;
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_FILES: usize = 1500;

fn dir() -> PathBuf { backend::backend_data_dir().join("title-cache") }

fn file(key: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    dir().join(format!("{:016x}.json", h.finish()))
}

static WRITE: Mutex<()> = Mutex::new(());

/// Everything stored for `key` (parts: details, scores, more, studios, extras).
pub fn read(key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(file(key)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    (v["key"].as_str() == Some(key)).then_some(v)
}

/// Store one part of a title's page off the async runtime (atomic replace).
pub fn put(key: &str, part: &'static str, value: &Value) {
    let (key, value) = (key.to_owned(), value.clone());
    std::thread::spawn(move || {
        let _g = WRITE.lock();
        let d = dir();
        if std::fs::create_dir_all(&d).is_err() { return; }
        let mut doc = read(&key).unwrap_or_else(|| json!({ "key": key }));
        doc[part] = value;
        doc["at"] = json!(backend::now_secs());
        let path = file(&key);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, doc.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &path); }
        prune(&d);
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
