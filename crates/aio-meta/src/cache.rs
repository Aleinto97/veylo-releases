use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Serve stale content for at most this long while upstream is unavailable.
pub const MAX_STALE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Default)]
pub struct Cache {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

struct Entry {
    body: Value,
    expires: Instant,
    stored: Instant,
}

impl Cache {
    pub fn get_fresh(&self, key: &str) -> Option<Value> {
        let map = self.inner.lock().ok()?;
        let entry = map.get(key)?;
        (entry.expires > Instant::now()).then(|| entry.body.clone())
    }

    /// Stale data is only useful for a bounded outage window.
    pub fn get_stale(&self, key: &str, max_age: Duration) -> Option<Value> {
        let map = self.inner.lock().ok()?;
        let entry = map.get(key)?;
        (entry.stored.elapsed() <= max_age).then(|| entry.body.clone())
    }

    pub fn put(&self, key: String, body: Value, ttl: Duration) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        map.insert(
            key,
            Entry {
                body,
                expires: Instant::now() + ttl,
                stored: Instant::now(),
            },
        );
        if map.len() > 512 {
            let cutoff = Instant::now() - Duration::from_secs(3600);
            map.retain(|_, entry| entry.expires > Instant::now() || entry.stored > cutoff);
            while map.len() > 512 {
                let Some(oldest) = crate::util::oldest_key(&map, |entry: &Entry| entry.stored)
                else {
                    break;
                };
                map.remove(&oldest);
            }
        }
    }

    pub fn clear(&self) {
        if let Ok(mut map) = self.inner.lock() {
            map.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|map| map.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stale_entries_expire_after_max_age() {
        let cache = Cache::default();
        cache.put("k".to_owned(), json!({"ok": true}), Duration::from_secs(1));
        assert!(cache.get_stale("k", Duration::from_secs(3600)).is_some());
        // Only this module can age an entry by hand.
        let mut map = cache.inner.lock().expect("cache");
        map.get_mut("k").expect("entry").stored = Instant::now() - Duration::from_secs(7200);
        drop(map);
        assert!(cache.get_stale("k", Duration::from_secs(3600)).is_none());
    }
}
