pub fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = decode_entities(&out);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';').filter(|end| *end <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn truncate(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in input.chars().take(max_chars) {
        out.push(ch);
    }
    if input.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

pub fn year(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let bytes = value.as_bytes();
    if bytes.len() >= 4 && bytes[..4].iter().all(u8::is_ascii_digit) {
        Some(value[..4].to_owned())
    } else {
        None
    }
}

pub fn iso_date(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let bytes = value.as_bytes();
    let is_date = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && [0usize, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|index| bytes[*index].is_ascii_digit());
    is_date.then(|| format!("{value}T00:00:00.000Z"))
}

/// TVMaze airstamps carry a numeric offset; Stremio clients and Cinemeta use UTC Z.
pub fn iso_utc(value: &str) -> String {
    value
        .strip_suffix("+00:00")
        .map(|value| format!("{value}Z"))
        .unwrap_or_else(|| value.to_owned())
}

pub fn rating(value: Option<f64>) -> Option<String> {
    value
        .filter(|value| *value > 0.0)
        .map(|value| format!("{value:.1}"))
}

pub fn tmdb_image(path: Option<&str>, size: &str) -> Option<String> {
    path.filter(|path| path.starts_with('/'))
        .map(|path| format!("https://image.tmdb.org/t/p/{size}{path}"))
}

pub fn dedupe_videos(videos: &mut Vec<serde_json::Value>) {
    let mut seen = std::collections::HashSet::new();
    videos.retain(|video| {
        let season = video
            .get("season")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let episode = video
            .get("episode")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        seen.insert((season, episode))
    });
    videos.sort_by_key(|video| {
        (
            video
                .get("season")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            video
                .get("episode")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )
    });
    videos.truncate(3000);
}

/// Sends a GET built by `build`, retrying once after a timeout or connect
/// failure (cold DNS/connect blips); GETs are idempotent. HTTP status
/// handling is left to the caller.
pub async fn send_with_retry(
    service: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> anyhow::Result<reqwest::Response> {
    let mut last_error = None;
    for attempt in 0..2 {
        match build().send().await {
            Ok(response) => return Ok(response),
            Err(error) => {
                let transient = error.is_timeout() || error.is_connect();
                last_error = Some(error);
                if attempt == 0 && transient {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                break;
            }
        }
    }
    let error = last_error
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("{service} request failed"));
    Err(error.context(format!("{service} request failed")))
}

/// Key of the least recently stored entry, for size-bounded caches.
pub fn oldest_key<K: Clone, V>(
    map: &std::collections::HashMap<K, V>,
    stored: impl Fn(&V) -> std::time::Instant,
) -> Option<K> {
    map.iter()
        .min_by_key(|(_, entry)| stored(entry))
        .map(|(key, _)| key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_helpers_reject_garbage_without_panicking() {
        assert_eq!(year(Some("２０２０-01-01")), None);
        assert_eq!(year(Some("2019-05-06")), Some("2019".to_owned()));
        assert_eq!(iso_date(Some("2019-aa-bb")), None);
        assert_eq!(
            iso_date(Some("2019-05-06")),
            Some("2019-05-06T00:00:00.000Z".to_owned())
        );
        assert_eq!(iso_utc("2019-05-07T01:00:00+00:00"), "2019-05-07T01:00:00Z");
        assert_eq!(
            iso_utc("2019-05-07T01:00:00+02:00"),
            "2019-05-07T01:00:00+02:00"
        );
    }
}
