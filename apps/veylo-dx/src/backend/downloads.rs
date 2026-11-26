//! Offline downloads: stream a direct video source into the app's private `downloads/` folder
//! so movies and episodes play smoothly on flights or trains without network access.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, PartialEq)]
pub struct DownloadEntry {
    pub key: String,
    pub media: Media,
    pub video_id: String,
    pub title: String,
    pub subtitle: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub downloaded_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DownloadState {
    Running,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DownloadProgress {
    pub key: String,
    pub title: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

struct ActiveJob {
    progress: DownloadProgress,
    cancel_tx: tokio::sync::watch::Sender<bool>,
    spec: DownloadSpec,
}

static DOWNLOADS_LOCK: Mutex<()> = Mutex::new(());
static DOWNLOADS_CACHE: Mutex<Option<Vec<DownloadEntry>>> = Mutex::new(None);
static DOWNLOADS_REV: AtomicU64 = AtomicU64::new(0);
static ACTIVE_DOWNLOADS: Mutex<Option<HashMap<String, ActiveJob>>> = Mutex::new(None);
static DOWNLOAD_SLOTS: OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = OnceLock::new();

pub fn downloads_revision() -> u64 {
    DOWNLOADS_REV.load(Ordering::Relaxed)
}

fn download_slots() -> std::sync::Arc<tokio::sync::Semaphore> {
    DOWNLOAD_SLOTS.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2))).clone()
}

pub fn download_key(media_key: &str, video_id: &str) -> String {
    format!("{media_key}|{video_id}")
}

fn downloads_dir() -> PathBuf { backend().data_dir.join("downloads") }
fn downloads_index_path() -> PathBuf { backend().data_dir.join("downloads.json") }

fn key_hash(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(key.as_bytes());
    hash.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

fn safe_filename(key: &str, url: &str) -> String {
    let hex = key_hash(key);
    let path_part = url.split(['?', '#']).next().unwrap_or("");
    let ext = path_part.rsplit('.').next()
        .filter(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "mkv" | "webm" | "m4v" | "mov" | "avi" | "ts"))
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "mkv".into());
    format!("{hex}.{ext}")
}

/// Bind each `.part` file to both the title key AND source URL so resuming after a mirror/encode
/// change never appends bytes from a different file onto an incompatible partial download.
fn part_filename(key: &str, url: &str) -> String {
    use sha2::{Digest, Sha256};
    let khex = key_hash(key);
    let uhash = Sha256::digest(url.as_bytes());
    let uhex: String = uhash.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{khex}-{uhex}.part")
}

/// Parse an HTTP `Content-Range: bytes <start>-<end>/<total|*>` header into `(start, end, total)`.
fn parse_content_range(header: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = header.trim().strip_prefix("bytes ")?;
    let (range, total_s) = rest.split_once('/')?;
    let (start_s, end_s) = range.split_once('-')?;
    let start = start_s.trim().parse::<u64>().ok()?;
    let end = end_s.trim().parse::<u64>().ok()?;
    if end < start { return None; }
    let total = if total_s.trim() == "*" {
        None
    } else {
        let t = total_s.trim().parse::<u64>().ok()?;
        if t <= end { return None; }
        Some(t)
    };
    Some((start, end, total))
}

fn part_meta_path(part_path: &std::path::Path) -> PathBuf {
    let mut os = part_path.as_os_str().to_os_string();
    os.push(".meta");
    PathBuf::from(os)
}

fn read_part_validator(part_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(part_meta_path(part_path)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["validator"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn write_part_validator(part_path: &std::path::Path, validator: Option<&str>) {
    let meta = part_meta_path(part_path);
    match validator {
        Some(val) if !val.trim().is_empty() => {
            let _ = write_private_file(&meta, json!({"validator": val.trim()}).to_string().as_bytes());
        }
        _ => { let _ = std::fs::remove_file(&meta); }
    }
}

fn extract_strong_validator(headers: &reqwest::header::HeaderMap) -> Option<String> {
    if let Some(etag) = headers.get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(str::trim) {
        if !etag.is_empty() && !etag.starts_with("W/") && !etag.starts_with("w/") {
            return Some(etag.to_owned());
        }
    }
    headers.get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Validate that a stored download path resolves to a regular (non-symlink) file directly inside `dir`.
fn safe_managed_path(dir: &std::path::Path, stored: &str) -> Option<PathBuf> {
    let raw = std::path::Path::new(stored);
    let candidate = if raw.components().count() == 1 {
        let name = raw.file_name()?;
        dir.join(name)
    } else {
        // Legacy entries store the full path; require exact parent containment with no traversal.
        if raw.components().any(|c| matches!(c, std::path::Component::ParentDir)) { return None; }
        let canon_dir = std::fs::canonicalize(dir).ok()?;
        let parent = raw.parent()?;
        let canon_parent = std::fs::canonicalize(parent).ok()?;
        if canon_parent != canon_dir { return None; }
        dir.join(raw.file_name()?)
    };
    let meta = std::fs::symlink_metadata(&candidate).ok()?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() { return None; }
    Some(candidate)
}

fn read_index(path: &std::path::Path, dir: &std::path::Path) -> Vec<DownloadEntry> {
    let v: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    v.as_array().into_iter().flatten().filter_map(|e| {
        let file = safe_managed_path(dir, &s(e, "path"))?;
        let media = Media::from_json(&e["media"], "")?;
        let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or_else(|_| e["bytes"].as_u64().unwrap_or(0));
        Some(DownloadEntry {
            key: s(e, "key"),
            media,
            video_id: s(e, "video_id"),
            title: s(e, "title"),
            subtitle: s(e, "subtitle"),
            path: file,
            bytes,
            downloaded_at: e["downloaded_at"].as_i64().unwrap_or(0),
        })
    }).collect()
}

fn write_index(path: &std::path::Path, entries: &[DownloadEntry]) -> std::io::Result<()> {
    let arr: Vec<Value> = entries.iter().map(|e| {
        let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| e.path.to_string_lossy().into_owned());
        json!({
            "key": e.key,
            "media": media_json(&e.media),
            "video_id": e.video_id,
            "title": e.title,
            "subtitle": e.subtitle,
            "path": name,
            "bytes": e.bytes,
            "downloaded_at": e.downloaded_at,
        })
    }).collect();
    write_private_file(path, Value::Array(arr).to_string().as_bytes())
}

pub fn list_downloads() -> Vec<DownloadEntry> {
    let _g = DOWNLOADS_LOCK.lock();
    let dir = downloads_dir();
    if let Ok(mut cache) = DOWNLOADS_CACHE.lock() {
        if let Some(cached) = cache.as_ref() {
            return cached.iter().filter(|e| e.path.is_file()).cloned().collect();
        }
        let mut list = read_index(&downloads_index_path(), &dir);
        list.sort_by_key(|e| std::cmp::Reverse(e.downloaded_at));
        *cache = Some(list.clone());
        return list;
    }
    let mut list = read_index(&downloads_index_path(), &dir);
    list.sort_by_key(|e| std::cmp::Reverse(e.downloaded_at));
    list
}

pub fn find_download(media_key: &str, video_id: &str) -> Option<DownloadEntry> {
    let want = download_key(media_key, video_id);
    list_downloads().into_iter().find(|e| e.key == want && e.path.is_file())
}

pub fn active_downloads() -> Vec<DownloadProgress> {
    ACTIVE_DOWNLOADS.lock().ok()
        .and_then(|g| g.as_ref().map(|m| m.values().map(|j| j.progress.clone()).collect()))
        .unwrap_or_default()
}

pub fn cancel_download(key: &str) {
    let mut part_to_clean = None;
    if let Ok(mut g) = ACTIVE_DOWNLOADS.lock() {
        if let Some(map) = g.as_mut() {
            if let Some(job) = map.get(key) {
                let _ = job.cancel_tx.send(true);
                if matches!(job.progress.state, DownloadState::Failed(_)) {
                    part_to_clean = Some(downloads_dir().join(part_filename(key, &job.spec.url)));
                }
            }
            if map.get(key).is_some_and(|j| matches!(j.progress.state, DownloadState::Failed(_))) {
                map.remove(key);
            }
        }
    }
    if let Some(p) = part_to_clean {
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(part_meta_path(&p));
    }
}

/// Retry a failed download, resuming cleanly from its existing `.part` file via HTTP Range.
pub fn retry_download(key: &str) -> Result<()> {
    let spec = {
        let g = ACTIVE_DOWNLOADS.lock().map_err(|_| anyhow!("download state busy"))?;
        g.as_ref().and_then(|m| m.get(key)).map(|j| j.spec.clone())
    }.context("Download is no longer queued")?;
    start_download(spec.media, spec.video_id, spec.title, spec.subtitle, spec.url, spec.headers, spec.filename)
}

pub fn remove_download(key: &str) -> Result<()> {
    let _g = DOWNLOADS_LOCK.lock();
    let dir = downloads_dir();
    let mut list = read_index(&downloads_index_path(), &dir);
    let target = list.iter().find(|e| e.key == key).map(|e| e.path.clone());
    if let Some(p) = target {
        if let Err(e) = std::fs::remove_file(&p) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e).context("Couldn't delete the downloaded file");
            }
        }
    }
    list.retain(|e| e.key != key);
    write_index(&downloads_index_path(), &list)?;
    list.sort_by_key(|e| std::cmp::Reverse(e.downloaded_at));
    if let Ok(mut cache) = DOWNLOADS_CACHE.lock() { *cache = Some(list); }
    DOWNLOADS_REV.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Start an offline download in the background on the backend Tokio runtime (max 2 concurrent,
/// resumable via HTTP Range + If-Range validator, with automatic preferred-language subtitle caching).
pub fn start_download(
    media: Media,
    video_id: String,
    title: String,
    subtitle: String,
    url: String,
    headers: Vec<(String, String)>,
    filename: Option<String>,
) -> Result<()> {
    let key = download_key(&media.key(), &video_id);
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let spec = DownloadSpec { key: key.clone(), media, video_id, title: title.clone(), subtitle, url, headers, filename };
    {
        let mut g = ACTIVE_DOWNLOADS.lock().map_err(|_| anyhow!("download state busy"))?;
        let map = g.get_or_insert_with(HashMap::new);
        if map.get(&key).is_some_and(|j| matches!(j.progress.state, DownloadState::Running)) {
            return Ok(());
        }
        map.insert(key.clone(), ActiveJob {
            progress: DownloadProgress {
                key: key.clone(),
                title,
                downloaded: 0,
                total: None,
                state: DownloadState::Running,
            },
            cancel_tx,
            spec: spec.clone(),
        });
    }
    let dir = downloads_dir();
    let idx = downloads_index_path();
    let runtime = RUNTIME.get().context("Backend not started")?;
    let slots = download_slots();
    runtime.spawn(async move {
        let permit = tokio::select! {
            biased;
            _ = cancel_rx.wait_for(|&c| c) => None,
            p = slots.acquire_owned() => p.ok(),
        };
        let res = if *cancel_rx.borrow() {
            Err(anyhow!("Cancelled"))
        } else {
            let sub_kind = spec.media.kind.clone();
            let sub_video = spec.video_id.clone();
            let sub_fname = spec.filename.clone();
            let res = run_download_to(&dir, &idx, spec, cancel_rx.clone()).await;
            // Release the download slot before best-effort subtitle fetching so queued videos start immediately.
            drop(permit);
            if res.is_ok() {
                if let Ok(subs) = online_subtitles(&sub_kind, &sub_video, sub_fname.as_deref(), None).await {
                    if let Some(best) = subs.first() {
                        let _ = fetch_subtitle(&best.url).await;
                    }
                }
            }
            res
        };
        if let Ok(mut g) = ACTIVE_DOWNLOADS.lock() {
            if let Some(map) = g.as_mut() {
                match res {
                    Ok(()) => { map.remove(&key); }
                    Err(e) => {
                        if let Some(job) = map.get_mut(&key) {
                            if *job.cancel_tx.borrow() { map.remove(&key); }
                            else { job.progress.state = DownloadState::Failed(format!("{e}")); }
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

#[derive(Clone)]
struct DownloadSpec {
    key: String,
    media: Media,
    video_id: String,
    title: String,
    subtitle: String,
    url: String,
    headers: Vec<(String, String)>,
    filename: Option<String>,
}

fn dl_same_origin(a: &url::Url, b: &url::Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

/// Follow up to 5 redirects while stripping request headers whenever a redirect leaves the origin.
async fn send_download_request(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    resume_from: u64,
    if_range: Option<&str>,
) -> Result<reqwest::Response> {
    let origin = url::Url::parse(url).context("Invalid download URL")?;
    let mut current = origin.clone();
    for _ in 0..=5 {
        let mut req = client.get(current.clone());
        if dl_same_origin(&origin, &current) {
            for (k, v) in headers { req = req.header(k, v); }
        }
        if resume_from > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
            if let Some(val) = if_range {
                req = req.header(reqwest::header::IF_RANGE, val);
            }
        }
        let resp = req.send().await.map_err(|_| anyhow!("Could not reach download source"))?;
        if resp.status().is_redirection() {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) {
                current = current.join(loc).context("Invalid redirect URL")?;
                continue;
            }
        }
        return Ok(resp);
    }
    anyhow::bail!("Too many redirects")
}

async fn run_download_to(
    dir: &std::path::Path,
    index_path: &std::path::Path,
    spec: DownloadSpec,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let DownloadSpec { key, media, video_id, title, subtitle, url, headers, .. } = spec;
    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(safe_filename(&key, &url));
    let part_path = dir.join(part_filename(&key, &url));
    let stored_validator = read_part_validator(&part_path);
    // Only attempt resume when we have BOTH an existing partial file AND a stored strong validator.
    let existing = if stored_validator.is_some() {
        std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut resp = tokio::select! {
        biased;
        _ = cancel_rx.wait_for(|&c| c) => anyhow::bail!("Cancelled"),
        r = send_download_request(&client, &url, &headers, existing, stored_validator.as_deref()) => r?,
    };
    // Verify Content-Range starts at `existing` and covers through EOF on 206 so a server returning
    // a different object or partial slice never corrupts `.part`.
    let content_range = resp.headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range);
    let (resumed, mut written, range_total) = if existing > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let valid_resume = match content_range {
            Some((start, end, Some(total))) => start == existing && end.checked_add(1) == Some(total),
            Some((start, _end, None)) => start == existing,
            None => false,
        };
        if valid_resume {
            let total = content_range.and_then(|(_, _, t)| t);
            (true, existing, total)
        } else {
            // Mismatched range or partial slice; restart cleanly from byte 0.
            resp = tokio::select! {
                biased;
                _ = cancel_rx.wait_for(|&c| c) => anyhow::bail!("Cancelled"),
                r = send_download_request(&client, &url, &headers, 0, None) => r?,
            };
            (false, 0u64, None)
        }
    } else if existing > 0 && resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        resp = tokio::select! {
            biased;
            _ = cancel_rx.wait_for(|&c| c) => anyhow::bail!("Cancelled"),
            r = send_download_request(&client, &url, &headers, 0, None) => r?,
        };
        (false, 0u64, None)
    } else {
        (false, 0u64, None)
    };
    let mut resp = resp.error_for_status().map_err(|e| anyhow!("Download failed (HTTP {})", e.status().map(|s| s.as_u16()).unwrap_or(0)))?;
    // Fresh requests must receive a full 200 OK body, never a partial 206 slice.
    if !resumed && resp.status() != reqwest::StatusCode::OK {
        anyhow::bail!("Download source returned partial or unexpected status (HTTP {})", resp.status().as_u16());
    }
    let validator = extract_strong_validator(resp.headers());
    if !resumed {
        write_part_validator(&part_path, validator.as_deref());
    }
    let total = range_total.or_else(|| resp.content_length().map(|len| if resumed { len.saturating_add(existing) } else { len }));
    if let Ok(mut g) = ACTIVE_DOWNLOADS.lock() {
        if let Some(job) = g.as_mut().and_then(|m| m.get_mut(&key)) {
            job.progress.downloaded = written;
            job.progress.total = total;
        }
    }
    let mut file = if resumed {
        tokio::fs::OpenOptions::new().write(true).append(true).open(&part_path).await?
    } else {
        tokio::fs::File::create(&part_path).await?
    };
    use tokio::io::AsyncWriteExt;
    let mut last_update = std::time::Instant::now();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel_rx.wait_for(|&c| c) => {
                drop(file);
                let _ = std::fs::remove_file(&part_path);
                let _ = std::fs::remove_file(part_meta_path(&part_path));
                anyhow::bail!("Cancelled");
            }
            c = resp.chunk() => c.map_err(|_| anyhow!("Connection interrupted"))?,
        };
        let Some(chunk) = next else { break };
        file.write_all(&chunk).await?;
        written = written.saturating_add(chunk.len() as u64);
        if last_update.elapsed() >= Duration::from_millis(250) {
            last_update = std::time::Instant::now();
            if let Ok(mut g) = ACTIVE_DOWNLOADS.lock() {
                if let Some(job) = g.as_mut().and_then(|m| m.get_mut(&key)) { job.progress.downloaded = written; }
            }
        }
    }
    file.flush().await?;
    drop(file);
    if *cancel_rx.borrow() {
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(part_meta_path(&part_path));
        anyhow::bail!("Cancelled");
    }
    anyhow::ensure!(written > 0, "Download returned no data");
    if let Some(expected) = total {
        anyhow::ensure!(written == expected, "Download size mismatch ({written}/{expected} bytes)");
    }
    let _g = DOWNLOADS_LOCK.lock();
    let mut list = read_index(index_path, dir);
    let old_path = list.iter().find(|e| e.key == key).map(|e| e.path.clone());
    std::fs::rename(&part_path, &final_path)?;
    list.retain(|e| e.key != key);
    list.push(DownloadEntry {
        key,
        media,
        video_id,
        title,
        subtitle,
        path: final_path.clone(),
        bytes: written,
        downloaded_at: now_secs(),
    });
    if let Err(e) = write_index(index_path, &list) {
        let _ = std::fs::rename(&final_path, &part_path);
        return Err(e).context("Couldn't save the downloads index");
    }
    let _ = std::fs::remove_file(part_meta_path(&part_path));
    if let Some(old) = old_path {
        if old != final_path {
            let _ = std::fs::remove_file(old);
        }
    }
    if BACKEND.get().is_some() && index_path == downloads_index_path() {
        list.sort_by_key(|e| std::cmp::Reverse(e.downloaded_at));
        if let Ok(mut cache) = DOWNLOADS_CACHE.lock() { *cache = Some(list); }
        DOWNLOADS_REV.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn offline_download_streams_to_disk_and_updates_index() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = vec![0x42u8; 32_768];
        let expected = payload.clone();
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let hdr = format!("HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
            let _ = sock.write_all(hdr.as_bytes());
            let _ = sock.write_all(&payload);
        });
        let dir = std::env::temp_dir().join(format!("veylo-dl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let files_dir = dir.join("files");
        let idx = dir.join("downloads.json");
        let media = Media::from_json(&json!({"id":"tt1375666","type":"movie","name":"Inception"}), CINEMETA).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (_tx, rx) = tokio::sync::watch::channel(false);
            run_download_to(
                &files_dir,
                &idx,
                DownloadSpec {
                    key: download_key(&media.key(), "tt1375666"),
                    media: media.clone(),
                    video_id: "tt1375666".into(),
                    title: "Inception".into(),
                    subtitle: "1080p".into(),
                    url: format!("http://{addr}/inception.mkv"),
                    headers: vec![],
                    filename: Some("Inception.2010.1080p.mkv".into()),
                },
                rx,
            ).await.unwrap();
        });
        let _ = srv.join();
        let list = read_index(&idx, &files_dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Inception");
        assert_eq!(list[0].bytes, 32_768);
        assert_eq!(std::fs::read(&list[0].path).unwrap(), expected);
        // Paths outside the managed directory must be rejected by read_index.
        let outside = dir.join("outside.mkv");
        std::fs::write(&outside, b"secret").unwrap();
        assert!(safe_managed_path(&files_dir, &outside.to_string_lossy()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_download_resumes_existing_part_via_http_range() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let payload: Vec<u8> = (0..16_384).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("range: bytes=4096-") || req.contains("Range: bytes=4096-"), "expected Range header: {req}");
            assert!(req.to_ascii_lowercase().contains("if-range: \"etag-v1\""), "expected If-Range header: {req}");
            let rest = &payload[4096..];
            let hdr = format!(
                "HTTP/1.1 206 Partial Content\r\nETag: \"etag-v1\"\r\nContent-Range: bytes 4096-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len() - 1, payload.len(), rest.len()
            );
            let _ = sock.write_all(hdr.as_bytes());
            let _ = sock.write_all(rest);
        });
        let dir = std::env::temp_dir().join(format!("veylo-dl-resume-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let files_dir = dir.join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        let idx = dir.join("downloads.json");
        let media = Media::from_json(&json!({"id":"tt1375666","type":"movie","name":"Inception"}), CINEMETA).unwrap();
        let key = download_key(&media.key(), "tt1375666");
        let url = format!("http://{addr}/inception.mkv");
        let part_path = files_dir.join(part_filename(&key, &url));
        std::fs::write(&part_path, &expected[..4096]).unwrap();
        write_part_validator(&part_path, Some("\"etag-v1\""));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (_tx, rx) = tokio::sync::watch::channel(false);
            run_download_to(
                &files_dir,
                &idx,
                DownloadSpec {
                    key,
                    media: media.clone(),
                    video_id: "tt1375666".into(),
                    title: "Inception".into(),
                    subtitle: "1080p".into(),
                    url,
                    headers: vec![],
                    filename: None,
                },
                rx,
            ).await.unwrap();
        });
        let _ = srv.join();
        let list = read_index(&idx, &files_dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].bytes, expected.len() as u64);
        assert_eq!(std::fs::read(&list[0].path).unwrap(), expected);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
