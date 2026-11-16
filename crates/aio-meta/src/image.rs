use axum::body::Bytes;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const IMAGE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const IMAGE_BYTES_CAP: usize = 128 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct ImageCache {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
    bytes: Arc<Mutex<usize>>,
}

struct Entry {
    body: Bytes,
    content_type: String,
    expires: Instant,
    stored: Instant,
}

impl ImageCache {
    pub fn get(&self, key: &str) -> Option<(Bytes, String)> {
        let map = self.inner.lock().ok()?;
        let entry = map.get(key)?;
        (entry.expires > Instant::now()).then(|| (entry.body.clone(), entry.content_type.clone()))
    }

    pub fn put(&self, key: String, body: Bytes, content_type: String) {
        let size = body.len();
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        let Ok(mut total) = self.bytes.lock() else {
            return;
        };
        if let Some(old) = map.remove(&key) {
            *total = total.saturating_sub(old.body.len());
        }
        map.insert(
            key,
            Entry {
                body,
                content_type,
                expires: Instant::now() + IMAGE_TTL,
                stored: Instant::now(),
            },
        );
        *total = total.saturating_add(size);
        while *total > IMAGE_BYTES_CAP && map.len() > 1 {
            let Some(oldest) = crate::util::oldest_key(&map, |entry: &Entry| entry.stored) else {
                break;
            };
            if let Some(old) = map.remove(&oldest) {
                *total = total.saturating_sub(old.body.len());
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|map| map.len()).unwrap_or(0)
    }
}

/// Replaces artwork URLs with the official AIOMetadata poster-cache URL shape:
/// /poster-cache/proxy/{class}/{type}/{id}?url=...&fallback=...
pub fn rewrite_images(value: &mut Value, base: &str) {
    rewrite_value(value, base, None);
}

fn rewrite_value(value: &mut Value, base: &str, context: Option<(&str, &str)>) {
    match value {
        Value::Object(map) => {
            let local = map
                .get("id")
                .and_then(Value::as_str)
                .zip(map.get("type").and_then(Value::as_str))
                .map(|(id, kind)| (kind.to_owned(), id.to_owned()));
            let context = local
                .as_ref()
                .map(|(kind, id)| (kind.as_str(), id.as_str()))
                .or(context);
            for (key, child) in map.iter_mut() {
                let class = match key.as_str() {
                    "poster" | "thumbnail" => Some("poster"),
                    "background" => Some("background"),
                    "logo" => Some("logo"),
                    _ => None,
                };
                if let (Some(class), Some(url), Some((kind, id))) = (class, child.as_str(), context)
                {
                    if let Some(proxied) = proxy_art_url(base, class, kind, id, url) {
                        *child = Value::String(proxied);
                        continue;
                    }
                }
                rewrite_value(child, base, context);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_value(item, base, context);
            }
        }
        _ => {}
    }
}

pub fn proxy_art_url(base: &str, class: &str, kind: &str, id: &str, url: &str) -> Option<String> {
    if !allowed_art_url(url) {
        return None;
    }
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("url", url).append_pair("fallback", url);
    Some(format!(
        "{base}/poster-cache/proxy/{class}/{kind}/{id}?{}",
        query.finish()
    ))
}

pub fn allowed_art_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    matches!(
        host,
        "image.tmdb.org" | "static.tvmaze.com" | "images.metahub.space"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rewrites_art_urls_with_meta_context() {
        let mut value = json!({
            "id": "tmdb:603",
            "type": "movie",
            "poster": "https://image.tmdb.org/t/p/w500/poster.jpg",
            "background": "https://image.tmdb.org/t/p/w1280/backdrop.jpg",
            "videos": [{"id": "tt0133093:1:1", "thumbnail": "https://static.tvmaze.com/uploads/images/medium/ep.jpg"}]
        });
        rewrite_images(&mut value, "http://127.0.0.1:7099");
        let poster = value["poster"].as_str().unwrap();
        assert!(
            poster.starts_with("http://127.0.0.1:7099/poster-cache/proxy/poster/movie/tmdb:603?")
        );
        assert!(poster.contains("url=https%3A%2F%2Fimage.tmdb.org%2Ft%2Fp%2Fw500%2Fposter.jpg"));
        assert!(poster.contains("fallback="));
        let thumb = value["videos"][0]["thumbnail"].as_str().unwrap();
        assert!(thumb.contains("/poster-cache/proxy/poster/movie/tmdb:603?"));
        assert!(allowed_art_url("https://image.tmdb.org/t/p/w500/x.jpg"));
        assert!(!allowed_art_url("https://evil.example/x.jpg"));
        assert!(!allowed_art_url("http://image.tmdb.org/t/p/w500/x.jpg"));
    }
}
