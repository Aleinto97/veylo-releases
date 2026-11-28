//! In-app YouTube trailer & clip stream resolver via YouTube's InnerTube player endpoint.
//! Runs inside the shared Rust core (reqwest + rustls), so every platform (Linux, macOS, Windows,
//! Android, Android TV) plays trailers directly in Veylo's own libmpv player with no external
//! browser launch and no external CLI tools.
//!
//! Primary: VISIONOS `hlsManifestUrl`, trimmed to the single best hardware-friendly H.264 (or VP9)
//! stream + its matching AAC audio group and served from a tiny loopback `.m3u8` endpoint so
//! libmpv/FFmpeg opens in ~1s instead of probing all 19 variant playlists sequentially.
//! Fallback: ANDROID muxed MP4 (`formats`, e.g. itag 18), fetched in parallel at zero extra latency.

use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PLAYER_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
const VISITOR_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false";

const VISIONOS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const VISIONOS_VER: &str = "1.02";
const ANDROID_UA: &str = "com.google.android.youtube/21.26.364 (Linux; U; Android 11) gzip";
const ANDROID_VER: &str = "21.26.364";

static VISITOR_CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
static HLS_STORE: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
static HLS_PORT: OnceLock<u16> = OnceLock::new();
static HLS_SEQ: AtomicU64 = AtomicU64::new(1);

fn hls_store() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    HLS_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn ensure_hls_server() -> Result<u16> {
    if let Some(&port) = HLS_PORT.get() {
        return Ok(port);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    if HLS_PORT.set(port).is_err() {
        return Ok(*HLS_PORT.get().unwrap());
    }
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { continue };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let Ok(n) = sock.read(&mut buf).await else { return };
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("");
                let token = path
                    .strip_prefix("/yt/")
                    .and_then(|s| s.strip_suffix(".m3u8"))
                    .unwrap_or("");
                let body = hls_store()
                    .lock()
                    .ok()
                    .and_then(|m| m.get(token).map(|(b, _)| b.clone()));
                let resp = match body {
                    Some(m3u8) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                        m3u8.len(),
                        m3u8
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    Ok(port)
}

async fn register_trimmed_hls(m3u8: String) -> Result<String> {
    let port = ensure_hls_server().await?;
    let token = format!("t{}", HLS_SEQ.fetch_add(1, Ordering::Relaxed));
    if let Ok(mut map) = hls_store().lock() {
        map.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(3600));
        map.insert(token.clone(), (m3u8, Instant::now()));
    }
    Ok(format!("http://127.0.0.1:{port}/yt/{token}.m3u8"))
}

/// Extract a strict 11-character YouTube video ID from a raw ID or a YouTube URL.
pub fn extract_id(input: &str) -> Option<String> {
    let s = input.trim();
    if is_valid_id(s) {
        return Some(s.to_owned());
    }
    let parsed = url::Url::parse(s).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host == "youtu.be" || host.ends_with(".youtu.be") {
        let seg = parsed.path_segments()?.next()?;
        return is_valid_id(seg).then(|| seg.to_owned());
    }
    if host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtube-nocookie.com"
        || host.ends_with(".youtube-nocookie.com")
    {
        if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "v") {
            if is_valid_id(&v) {
                return Some(v.into_owned());
            }
        }
        let segs: Vec<&str> = parsed.path_segments()?.collect();
        if segs.len() >= 2 && matches!(segs[0], "embed" | "shorts" | "live" | "v") && is_valid_id(segs[1]) {
            return Some(segs[1].to_owned());
        }
    }
    None
}

fn is_valid_id(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn quality_label(height: u64) -> String {
    match height {
        2160.. => "4K".to_owned(),
        1440..=2159 => "1440p".to_owned(),
        1080..=1439 => "1080p".to_owned(),
        720..=1079 => "720p".to_owned(),
        480..=719 => "480p".to_owned(),
        360..=479 => "360p".to_owned(),
        h => format!("{h}p"),
    }
}

fn format_bytes(bytes: u64) -> Option<String> {
    if bytes == 0 {
        return None;
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        Some(format!("{:.2} GiB", mb / 1024.0))
    } else {
        Some(format!("{:.1} MiB", mb))
    }
}

fn codec_label(mime: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.contains("avc1") {
        "H.264"
    } else if m.contains("vp09") || m.contains("vp9") {
        "VP9"
    } else if m.contains("av01") {
        "AV1"
    } else {
        "MP4"
    }
}

struct TrimmedHls {
    m3u8: String,
    quality: Option<String>,
    hdr: Option<&'static str>,
}

/// Trim a multi-variant YouTube HLS master playlist down to the single best hardware-friendly
/// stream (preferring H.264 `avc1` at the highest height <= 2160p, then VP9) plus its referenced
/// AUDIO group. Cuts libmpv HLS open time from ~6.7s to ~1.2s and avoids VP9 Vulkan probe errors.
fn trim_hls_master(m3u8: &str) -> Option<TrimmedHls> {
    let lines: Vec<&str> = m3u8.lines().collect();
    let mut audio_lines: Vec<&str> = Vec::new();
    type HlsCandidate<'a> = ((u64, u8, u64), &'a str, &'a str, u64, Option<&'static str>);
    let mut best: Option<HlsCandidate<'_>> = None;

    for (i, &line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-MEDIA:") && line.contains("TYPE=AUDIO") {
            audio_lines.push(line);
        } else if line.starts_with("#EXT-X-STREAM-INF:") && i + 1 < lines.len() {
            let uri = lines[i + 1].trim();
            if uri.is_empty() || uri.starts_with('#') {
                continue;
            }
            let h = line
                .find("RESOLUTION=")
                .and_then(|pos| line[pos + 11..].split(',').next())
                .and_then(|tok| tok.split_once('x'))
                .and_then(|(_, hs)| hs.trim().parse::<u64>().ok())
                .unwrap_or(0);
            if h == 0 || h > 2160 {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            let codec_rank: u8 = if lower.contains("avc1") {
                3
            } else if lower.contains("vp09") || lower.contains("vp9") {
                2
            } else if lower.contains("av01") {
                1
            } else {
                0
            };
            let bw = line
                .find("BANDWIDTH=")
                .and_then(|pos| line[pos + 10..].split(',').next())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let hdr = if line.contains("VIDEO-RANGE=PQ") {
                Some("HDR10")
            } else if line.contains("VIDEO-RANGE=HLG") {
                Some("HLG")
            } else {
                None
            };
            let score = (h, codec_rank, bw);
            if best.is_none_or(|b| score > b.0) {
                best = Some((score, line, uri, h, hdr));
            }
        }
    }

    let (_, inf, uri, h, hdr) = best?;
    let audio_group = inf
        .find("AUDIO=\"")
        .and_then(|pos| inf[pos + 7..].split('"').next());
    let mut out = String::from("#EXTM3U\n#EXT-X-INDEPENDENT-SEGMENTS\n");
    for al in &audio_lines {
        let keep = match audio_group {
            Some(g) => al.contains(&format!("GROUP-ID=\"{g}\"")),
            None => true,
        };
        if keep {
            out.push_str(al);
            out.push('\n');
        }
    }
    out.push_str(inf);
    out.push('\n');
    out.push_str(uri);
    out.push('\n');

    Some(TrimmedHls {
        m3u8: out,
        quality: Some(quality_label(h)),
        hdr,
    })
}

fn extract_captions(body: &Value) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let tracks = body
        .pointer("/captions/playerCaptionsTracklistRenderer/captionTracks")
        .and_then(Value::as_array);
    for t in tracks.into_iter().flatten() {
        let Some(base) = t["baseUrl"].as_str().filter(|u| u.starts_with("https://")) else {
            continue;
        };
        let lang_code = t["languageCode"].as_str().unwrap_or("").to_owned();
        let label = t
            .pointer("/name/simpleText")
            .and_then(Value::as_str)
            .or_else(|| t.pointer("/name/runs/0/text").and_then(Value::as_str))
            .unwrap_or(&lang_code)
            .trim()
            .to_owned();
        if label.is_empty() || !seen.insert(format!("{lang_code}:{label}")) {
            continue;
        }
        let vtt_url = if base.contains("fmt=") {
            base.to_owned()
        } else {
            format!("{base}&fmt=vtt")
        };
        let is_asr = t["kind"].as_str() == Some("asr");
        out.push(json!({
            "language": label,
            "release": if is_asr { "YouTube (auto)" } else { "YouTube" },
            "url": vtt_url,
        }));
    }
    out
}

fn build_hls_source(
    hls_url: &str,
    quality: Option<String>,
    hdr: Option<&'static str>,
    author: &str,
    ua: &str,
) -> Value {
    let q_label = quality.clone().unwrap_or_else(|| "Auto".to_owned());
    json!({
        "addon": "YouTube",
        "name": format!("{q_label} · HLS"),
        "description": author,
        "url": hls_url,
        "audio_url": Value::Null,
        "filename": "",
        "size": Value::Null,
        "size_bytes": Value::Null,
        "quality": quality.unwrap_or_else(|| "Auto".to_owned()),
        "hdr": hdr,
        "kind": "direct",
        "headers": [["User-Agent", ua]],
        "off_title": false,
    })
}

fn build_muxed_sources(body: &Value, author: &str, ua: &str) -> Vec<Value> {
    let muxed = body
        .pointer("/streamingData/formats")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for f in muxed {
        let Some(url) = f["url"].as_str().filter(|u| !u.is_empty()) else {
            continue;
        };
        let h = f["height"].as_u64().unwrap_or(360);
        let q = quality_label(h);
        let mime = f["mimeType"].as_str().unwrap_or("");
        let bytes = f["contentLength"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        rows.push((
            h,
            json!({
                "addon": "YouTube",
                "name": format!("{q} · {}", codec_label(mime)),
                "description": author,
                "url": url,
                "audio_url": Value::Null,
                "filename": "",
                "size": format_bytes(bytes),
                "size_bytes": (bytes > 0).then_some(bytes),
                "quality": q,
                "hdr": Value::Null,
                "kind": "direct",
                "headers": [["User-Agent", ua]],
                "off_title": false,
            }),
        ));
    }
    rows.sort_by_key(|a| std::cmp::Reverse(a.0));
    rows.into_iter().map(|(_, v)| v).collect()
}

async fn visitor_data(client: &reqwest::Client) -> Option<String> {
    let cache = VISITOR_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((ref vd, at)) = *g {
            if at.elapsed() < Duration::from_secs(1800) {
                return Some(vd.clone());
            }
        }
    }
    let payload = json!({
        "context": {
            "client": {
                "clientName": "WEB",
                "clientVersion": "2.20260708.00.00"
            }
        }
    });
    let resp = client
        .post(VISITOR_ENDPOINT)
        .header("User-Agent", VISIONOS_UA)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await
        .ok()?;
    let body: Value = resp.json().await.ok()?;
    let vd = body
        .pointer("/responseContext/visitorData")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    if let Ok(mut g) = cache.lock() {
        *g = Some((vd.clone(), Instant::now()));
    }
    Some(vd)
}

async fn fetch_visionos(client: &reqwest::Client, id: &str, vd: &str) -> Result<Value> {
    let payload = json!({
        "context": {
            "client": {
                "clientName": "VISIONOS",
                "clientVersion": VISIONOS_VER,
                "deviceMake": "Apple",
                "deviceModel": "RealityDevice17,1",
                "userAgent": VISIONOS_UA,
                "osName": "visionOS",
                "osVersion": "26.5.23O471",
                "visitorData": vd,
                "hl": "en",
                "gl": "US"
            }
        },
        "videoId": id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let resp = client
        .post(PLAYER_ENDPOINT)
        .header("User-Agent", VISIONOS_UA)
        .header("Content-Type", "application/json")
        .header("X-YouTube-Client-Name", "101")
        .header("X-YouTube-Client-Version", VISIONOS_VER)
        .header("X-Goog-Visitor-Id", vd)
        .header("Origin", "https://www.youtube.com")
        .body(payload.to_string())
        .send()
        .await?;
    ensure!(resp.status().is_success(), "YouTube HTTP {}", resp.status());
    Ok(resp.json().await?)
}

async fn fetch_android(client: &reqwest::Client, id: &str) -> Result<Value> {
    let payload = json!({
        "context": {
            "client": {
                "clientName": "ANDROID",
                "clientVersion": ANDROID_VER,
                "androidSdkVersion": 30,
                "userAgent": ANDROID_UA,
                "osName": "Android",
                "osVersion": "11",
                "hl": "en",
                "gl": "US"
            }
        },
        "videoId": id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let resp = client
        .post(PLAYER_ENDPOINT)
        .header("User-Agent", ANDROID_UA)
        .header("Content-Type", "application/json")
        .header("X-YouTube-Client-Name", "3")
        .header("X-YouTube-Client-Version", ANDROID_VER)
        .body(payload.to_string())
        .send()
        .await?;
    ensure!(resp.status().is_success(), "YouTube HTTP {}", resp.status());
    Ok(resp.json().await?)
}

pub async fn resolve(id_or_url: &str) -> Result<Value> {
    let id = extract_id(id_or_url).ok_or_else(|| anyhow!("Invalid YouTube video id"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()?;

    let vd = visitor_data(&client).await;
    let (vision_res, android_res) = tokio::join!(
        async {
            match vd.as_deref() {
                Some(v) => fetch_visionos(&client, &id, v).await,
                None => Err(anyhow!("No YouTube visitorData")),
            }
        },
        fetch_android(&client, &id),
    );

    let mut sources: Vec<Value> = Vec::new();
    let mut captions: Vec<Value> = Vec::new();
    let mut title = "Trailer".to_owned();
    let mut author = String::new();
    let mut duration_secs: u64 = 0;
    let mut last_err = anyhow!("No playable YouTube stream found");

    if let Ok(body) = vision_res {
        let status = body.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("");
        if status == "OK" {
            if let Some(t) = body.pointer("/videoDetails/title").and_then(Value::as_str) {
                title = t.to_owned();
            }
            if let Some(a) = body.pointer("/videoDetails/author").and_then(Value::as_str) {
                author = a.to_owned();
            }
            duration_secs = body
                .pointer("/videoDetails/lengthSeconds")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            captions = extract_captions(&body);
            if let Some(hls) = body
                .pointer("/streamingData/hlsManifestUrl")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
            {
                let trimmed = match client.get(hls).header("User-Agent", VISIONOS_UA).send().await {
                    Ok(r) if r.status().is_success() => {
                        let text = r.text().await.unwrap_or_default();
                        trim_hls_master(&text)
                    }
                    _ => None,
                };
                if let Some(t) = trimmed {
                    if let Ok(local_url) = register_trimmed_hls(t.m3u8).await {
                        sources.push(build_hls_source(&local_url, t.quality, t.hdr, &author, VISIONOS_UA));
                    } else {
                        sources.push(build_hls_source(hls, t.quality, t.hdr, &author, VISIONOS_UA));
                    }
                } else {
                    sources.push(build_hls_source(hls, None, None, &author, VISIONOS_UA));
                }
            }
        } else {
            let reason = body
                .pointer("/playabilityStatus/reason")
                .and_then(Value::as_str)
                .unwrap_or("Unavailable");
            last_err = anyhow!("YouTube returned {status}: {reason}");
        }
    } else if let Err(e) = vision_res {
        last_err = e;
    }

    if let Ok(body) = android_res {
        let status = body.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("");
        if status == "OK" {
            if title == "Trailer" {
                if let Some(t) = body.pointer("/videoDetails/title").and_then(Value::as_str) {
                    title = t.to_owned();
                }
            }
            if author.is_empty() {
                if let Some(a) = body.pointer("/videoDetails/author").and_then(Value::as_str) {
                    author = a.to_owned();
                }
            }
            if duration_secs == 0 {
                duration_secs = body
                    .pointer("/videoDetails/lengthSeconds")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
            }
            if captions.is_empty() {
                captions = extract_captions(&body);
            }
            sources.extend(build_muxed_sources(&body, &author, ANDROID_UA));
        } else if sources.is_empty() {
            let reason = body
                .pointer("/playabilityStatus/reason")
                .and_then(Value::as_str)
                .unwrap_or("Unavailable");
            last_err = anyhow!("YouTube returned {status}: {reason}");
        }
    }

    if sources.is_empty() {
        return Err(last_err);
    }

    let best = sources[0].clone();
    Ok(json!({
        "id": id,
        "title": title,
        "author": author,
        "duration_secs": duration_secs,
        "headers": best["headers"],
        "best": best,
        "sources": sources,
        "captions": captions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_id_handles_ids_and_urls() {
        assert_eq!(extract_id("vKQi3bBA1y8").as_deref(), Some("vKQi3bBA1y8"));
        assert_eq!(extract_id("https://www.youtube.com/watch?v=vKQi3bBA1y8&t=12s").as_deref(), Some("vKQi3bBA1y8"));
        assert_eq!(extract_id("https://youtu.be/Way9Dexny3w").as_deref(), Some("Way9Dexny3w"));
        assert_eq!(extract_id("https://www.youtube-nocookie.com/embed/Way9Dexny3w?autoplay=1").as_deref(), Some("Way9Dexny3w"));
        assert_eq!(extract_id("https://example.com/watch?v=vKQi3bBA1y8"), None);
        assert_eq!(extract_id("not-valid"), None);
    }

    #[test]
    fn trims_hls_master_to_single_h264_variant_and_matching_audio() {
        let m3u8 = "\
#EXTM3U
#EXT-X-INDEPENDENT-SEGMENTS
#EXT-X-MEDIA:URI=\"https://cdn.invalid/a1.m3u8\",TYPE=AUDIO,GROUP-ID=\"a1\",NAME=\"Default\",DEFAULT=YES
#EXT-X-MEDIA:URI=\"https://cdn.invalid/a2.m3u8\",TYPE=AUDIO,GROUP-ID=\"a2\",NAME=\"Default\",DEFAULT=YES
#EXT-X-STREAM-INF:BANDWIDTH=1200000,CODECS=\"avc1.4D401F,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a1\"
https://cdn.invalid/720.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3200000,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1920x1080,AUDIO=\"a2\"
https://cdn.invalid/1080-h264.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3800000,CODECS=\"vp09.00.40.08,mp4a.40.2\",RESOLUTION=1920x1080,VIDEO-RANGE=PQ,AUDIO=\"a2\"
https://cdn.invalid/1080-vp9.m3u8
";
        let trimmed = trim_hls_master(m3u8).unwrap();
        assert_eq!(trimmed.quality.as_deref(), Some("1080p"));
        assert!(trimmed.m3u8.contains("1080-h264.m3u8"));
        assert!(!trimmed.m3u8.contains("1080-vp9.m3u8"));
        assert!(trimmed.m3u8.contains("GROUP-ID=\"a2\""));
        assert!(!trimmed.m3u8.contains("GROUP-ID=\"a1\""));
    }

    #[test]
    fn builds_muxed_sources_and_vtt_captions() {
        let body = json!({
            "streamingData": {
                "formats": [
                    { "url": "https://cdn.invalid/360.mp4", "height": 360, "mimeType": "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"", "contentLength": "9000000" }
                ]
            },
            "captions": {
                "playerCaptionsTracklistRenderer": {
                    "captionTracks": [
                        { "baseUrl": "https://www.youtube.com/api/timedtext?v=test&lang=en", "languageCode": "en", "name": { "simpleText": "English" } }
                    ]
                }
            }
        });
        let srcs = build_muxed_sources(&body, "Studio", "test-ua");
        assert_eq!(srcs.len(), 1);
        assert_eq!(srcs[0]["quality"], "360p");
        let caps = extract_captions(&body);
        assert_eq!(caps[0]["url"], "https://www.youtube.com/api/timedtext?v=test&lang=en&fmt=vtt");
    }
}
