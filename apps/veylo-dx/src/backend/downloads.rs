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
    /// Unique per started job: a cancelled task that is still winding down never touches the
    /// entry (or the `.part` file) of a newer job for the same title.
    generation: u64,
}

static DOWNLOADS_LOCK: Mutex<()> = Mutex::new(());
static DOWNLOADS_CACHE: Mutex<Option<Vec<DownloadEntry>>> = Mutex::new(None);
static DOWNLOADS_REV: AtomicU64 = AtomicU64::new(0);
static DOWNLOAD_NONCE: AtomicU64 = AtomicU64::new(1);
static DOWNLOAD_GENERATION: AtomicU64 = AtomicU64::new(1);
/// One lock per download key: a job waits here until the previous job for the same title has
/// fully stopped, so two tasks never write the same `.part` file.
static KEY_LOCKS: Mutex<Option<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> = Mutex::new(None);

fn key_lock(key: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let Ok(mut g) = KEY_LOCKS.lock() else { return Default::default() };
    g.get_or_insert_with(HashMap::new).entry(key.to_owned()).or_default().clone()
}

/// A newer job for `key` has replaced the one numbered `generation` (or it was cancelled and a new one started).
fn superseded(key: &str, generation: u64) -> bool {
    ACTIVE_DOWNLOADS.lock().ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(key)).map(|j| j.generation != generation))
        .unwrap_or(false)
}
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
    let nonce = DOWNLOAD_NONCE.fetch_add(1, Ordering::Relaxed);
    let ts = now_secs().max(0) as u64;
    format!("{hex}-{ts:x}-{nonce:x}.{ext}")
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

/// The downloads index could not be read as JSON (and no usable backup exists): never replaced
/// by an empty list, which would orphan every downloaded file.
#[derive(Debug)]
struct IndexUnreadable;

impl std::fmt::Display for IndexUnreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The downloads list couldn't be read, so it was left unchanged")
    }
}
impl std::error::Error for IndexUnreadable {}

fn index_sidecar(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn parse_index_array(text: &str) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(text).ok()? { Value::Array(a) => Some(a), _ => None }
}

fn entries_from(arr: &[Value], dir: &std::path::Path) -> Vec<DownloadEntry> {
    arr.iter().filter_map(|e| {
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

/// The index: a missing file is an empty list; a file that doesn't parse falls back to the last
/// good copy (`downloads.json.bak`); with no usable copy it is an error, never "no downloads".
fn read_index_checked(path: &std::path::Path, dir: &std::path::Path) -> std::result::Result<Vec<DownloadEntry>, IndexUnreadable> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let arr = parse_index_array(&text)
                .or_else(|| std::fs::read_to_string(index_sidecar(path, ".bak")).ok().and_then(|t| parse_index_array(&t)));
            arr.map(|a| entries_from(&a, dir)).ok_or(IndexUnreadable)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(IndexUnreadable),
    }
}

fn read_index(path: &std::path::Path, dir: &std::path::Path) -> Vec<DownloadEntry> {
    read_index_checked(path, dir).unwrap_or_default()
}

/// Write the index, keeping the previous good one as `downloads.json.bak`. An unparsable index
/// is set aside as `downloads.json.corrupt` (once) and is only replaced when `entries` came from
/// somewhere good (the backup); an empty list never replaces a file that could not be read.
fn write_index(path: &std::path::Path, entries: &[DownloadEntry]) -> std::io::Result<()> {
    if let Ok(raw) = std::fs::read(path) {
        let parses = std::str::from_utf8(&raw).ok().and_then(parse_index_array).is_some();
        if parses {
            let _ = write_private_file(&index_sidecar(path, ".bak"), &raw);
        } else {
            let backup_ok = std::fs::read_to_string(index_sidecar(path, ".bak")).ok().and_then(|t| parse_index_array(&t)).is_some();
            if entries.is_empty() && !backup_ok {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, IndexUnreadable));
            }
            let corrupt = index_sidecar(path, ".corrupt");
            if !corrupt.exists() { let _ = write_private_file(&corrupt, &raw); }
        }
    }
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

/// Leftovers of interrupted downloads: a `.part` (and its `.part.meta`) not touched for a week
/// will never be resumed, and a `.part.meta` without its `.part` is useless. Run once at launch.
pub(super) fn sweep_stale_parts() { sweep_stale_in(&downloads_dir(), std::time::Duration::from_secs(7 * 24 * 3600)); }

fn sweep_stale_in(dir: &std::path::Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let is_part = name.ends_with(".part");
        let is_meta = name.ends_with(".part.meta");
        if !is_part && !is_meta { continue; }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() { continue; }
        let stale = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age > max_age);
        let orphan_meta = is_meta && !path.with_file_name(name.trim_end_matches(".meta")).exists();
        if stale || orphan_meta { let _ = std::fs::remove_file(&path); }
    }
}

pub fn list_downloads() -> Vec<DownloadEntry> {
    let _g = DOWNLOADS_LOCK.lock();
    let dir = downloads_dir();
    if let Ok(mut cache) = DOWNLOADS_CACHE.lock() {
        if let Some(cached) = cache.as_ref() {
            return cached.iter().filter(|e| e.path.is_file()).cloned().collect();
        }
        // An unreadable index is shown as empty but never cached: once it is fixed it comes back.
        let Ok(mut list) = read_index_checked(&downloads_index_path(), &dir) else {
            dev_log("downloads: the index couldn't be read; left untouched");
            return Vec::new();
        };
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
            // Cancelled or dismissed, the job leaves the list now; its task winds down on its own
            // and (by generation) can no longer touch a job started later for the same title.
            if let Some(job) = map.remove(key) {
                let _ = job.cancel_tx.send(true);
                if matches!(job.progress.state, DownloadState::Failed(_)) {
                    part_to_clean = Some(downloads_dir().join(part_filename(key, &job.spec.url)));
                }
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
    let mut list = read_index_checked(&downloads_index_path(), &dir)?;
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
    let generation = DOWNLOAD_GENERATION.fetch_add(1, Ordering::Relaxed);
    let spec = DownloadSpec { key: key.clone(), media, video_id, title: title.clone(), subtitle, url, headers, filename, generation };
    {
        let mut g = ACTIVE_DOWNLOADS.lock().map_err(|_| anyhow!("download state busy"))?;
        let map = g.get_or_insert_with(HashMap::new);
        if map.get(&key).is_some_and(|j| matches!(j.progress.state, DownloadState::Running) && !*j.cancel_tx.borrow()) {
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
            generation,
        });
    }
    let dir = downloads_dir();
    let idx = downloads_index_path();
    let runtime = RUNTIME.get().context("Backend not started")?;
    let slots = download_slots();
    runtime.spawn(async move {
        let cleanup_part_path = dir.join(part_filename(&spec.key, &spec.url));
        // Wait for an earlier (cancelled) job of this title to stop writing its `.part` first.
        let key_turn = tokio::select! {
            biased;
            _ = cancel_rx.wait_for(|&c| c) => None,
            g = key_lock(&spec.key).lock_owned() => Some(g),
        };
        let permit = if key_turn.is_none() { None } else {
            tokio::select! {
                biased;
                _ = cancel_rx.wait_for(|&c| c) => None,
                p = slots.acquire_owned() => p.ok(),
            }
        };
        let res = if *cancel_rx.borrow() {
            if !superseded(&key, generation) {
                let _ = std::fs::remove_file(&cleanup_part_path);
                let _ = std::fs::remove_file(part_meta_path(&cleanup_part_path));
            }
            Err(anyhow!("Cancelled"))
        } else {
            let sub_kind = spec.media.kind.clone();
            let sub_video = spec.video_id.clone();
            let sub_fname = spec.filename.clone();
            let res = run_download_to(&dir, &idx, spec, cancel_rx.clone()).await;
            // Release the download slot before best-effort subtitle fetching so queued videos start immediately.
            drop(permit);
            if *cancel_rx.borrow() && !superseded(&key, generation) {
                let _ = std::fs::remove_file(&cleanup_part_path);
                let _ = std::fs::remove_file(part_meta_path(&cleanup_part_path));
            }
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
                // Only this job's own entry: a cancelled job's task must never remove (or fail) a
                // newer one started for the same title.
                let mine = map.get(&key).is_some_and(|j| j.generation == generation);
                if mine {
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
    /// The generation of the job this spec runs for (0 outside a queued job).
    generation: u64,
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
    let DownloadSpec { key, media, video_id, title, subtitle, url, headers, generation, .. } = spec;
    let cleanup_part = |part: &std::path::Path| {
        if !superseded(&key, generation) {
            let _ = std::fs::remove_file(part);
            let _ = std::fs::remove_file(part_meta_path(part));
        }
    };
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
        // Require a known total and an end offset reaching EOF (`end + 1 == total`) so a bounded
        // `206` slice with an unknown total (`/*`) can never be published as a complete file.
        // The object must be the one the partial file came from: its validator (ETag, else
        // Last-Modified) has to equal the stored one, even though If-Range asked for that.
        let same_object = extract_strong_validator(resp.headers()).as_deref() == stored_validator.as_deref();
        let valid_resume = same_object && match content_range {
            Some((start, end, Some(total))) => start == existing && end.checked_add(1) == Some(total),
            _ => false,
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
    let total = range_total.or_else(|| resp.content_length().map(|len| if resumed { len.saturating_add(existing) } else { len }));
    if let Ok(mut g) = ACTIVE_DOWNLOADS.lock() {
        if let Some(job) = g.as_mut().and_then(|m| m.get_mut(&key)).filter(|j| j.generation == generation) {
            job.progress.downloaded = written;
            job.progress.total = total;
        }
    }
    let mut file = if resumed {
        tokio::fs::OpenOptions::new().write(true).append(true).open(&part_path).await?
    } else {
        // Invalidate the old sidecar, truncate `.part`, then record the new validator so a crash
        // between steps can never pair old bytes with the new object's validator.
        write_part_validator(&part_path, None);
        let f = tokio::fs::File::create(&part_path).await?;
        write_part_validator(&part_path, validator.as_deref());
        f
    };
    use tokio::io::AsyncWriteExt;
    let mut last_update = std::time::Instant::now();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel_rx.wait_for(|&c| c) => {
                drop(file);
                cleanup_part(&part_path);
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
                if let Some(job) = g.as_mut().and_then(|m| m.get_mut(&key)).filter(|j| j.generation == generation) { job.progress.downloaded = written; }
            }
        }
    }
    file.flush().await?;
    // The bytes are on disk before the file takes its final name and the index points at it.
    file.sync_all().await?;
    drop(file);
    if *cancel_rx.borrow() {
        cleanup_part(&part_path);
        anyhow::bail!("Cancelled");
    }
    anyhow::ensure!(written > 0, "Download returned no data");
    if let Some(expected) = total {
        anyhow::ensure!(written == expected, "Download size mismatch ({written}/{expected} bytes)");
    }
    let _g = DOWNLOADS_LOCK.lock();
    // An unreadable index is never replaced by a list holding only this download: the finished
    // `.part` stays where it is until the index can be read again.
    let mut list = read_index_checked(index_path, dir)?;
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
                    generation: 0,
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

    fn sample_entry(dir: &std::path::Path) -> DownloadEntry {
        let media = Media::from_json(&json!({"id":"tt1375666","type":"movie","name":"Inception"}), CINEMETA).unwrap();
        let path = dir.join("film.mkv");
        std::fs::write(&path, b"video").unwrap();
        DownloadEntry { key: download_key(&media.key(), "tt1375666"), media, video_id: "tt1375666".into(), title: "Inception".into(),
            subtitle: "1080p".into(), path, bytes: 5, downloaded_at: 1 }
    }

    #[test]
    fn unparsable_index_is_never_replaced_by_an_empty_list() {
        let dir = std::env::temp_dir().join(format!("veylo-dl-index-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let files = dir.join("files");
        std::fs::create_dir_all(&files).unwrap();
        let idx = dir.join("downloads.json");
        let entry = sample_entry(&files);
        // A good index, then a second write keeps the first as the backup.
        write_index(&idx, std::slice::from_ref(&entry)).unwrap();
        write_index(&idx, std::slice::from_ref(&entry)).unwrap();
        assert!(index_sidecar(&idx, ".bak").exists());
        // Corrupt the index: reading falls back to the backup instead of reporting no downloads.
        std::fs::write(&idx, b"{ not json").unwrap();
        assert_eq!(read_index_checked(&idx, &files).unwrap().len(), 1);
        // No backup and an unparsable index: an error, and an empty write is refused untouched.
        std::fs::remove_file(index_sidecar(&idx, ".bak")).unwrap();
        assert!(read_index_checked(&idx, &files).is_err());
        assert!(write_index(&idx, &[]).is_err());
        assert_eq!(std::fs::read(&idx).unwrap(), b"{ not json");
        // A missing index is simply empty.
        std::fs::remove_file(&idx).unwrap();
        assert!(read_index_checked(&idx, &files).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_removes_only_stale_parts_and_orphan_meta() {
        let dir = std::env::temp_dir().join(format!("veylo-dl-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.part"), b"x").unwrap();
        std::fs::write(dir.join("a.part.meta"), b"{}").unwrap();
        std::fs::write(dir.join("b.part.meta"), b"{}").unwrap();
        std::fs::write(dir.join("film.mkv"), b"video").unwrap();
        // Nothing is a week old: only the meta without its part goes.
        sweep_stale_in(&dir, std::time::Duration::from_secs(7 * 24 * 3600));
        assert!(dir.join("a.part").exists() && dir.join("a.part.meta").exists());
        assert!(!dir.join("b.part.meta").exists());
        // With a zero max age every part is stale; finished files are never touched.
        std::thread::sleep(std::time::Duration::from_millis(20));
        sweep_stale_in(&dir, std::time::Duration::from_millis(1));
        assert!(!dir.join("a.part").exists() && !dir.join("a.part.meta").exists());
        assert!(dir.join("film.mkv").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stopping_job_never_touches_a_newer_one() {
        let key = "series|tt1|gen-test".to_owned();
        let (tx, _rx) = tokio::sync::watch::channel(false);
        let media = Media::from_json(&json!({"id":"tt1","type":"movie","name":"A"}), CINEMETA).unwrap();
        let spec = DownloadSpec { key: key.clone(), media, video_id: "v".into(), title: "A".into(), subtitle: String::new(),
            url: "https://x.invalid/a.mkv".into(), headers: vec![], filename: None, generation: 7 };
        ACTIVE_DOWNLOADS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key.clone(), ActiveJob {
            progress: DownloadProgress { key: key.clone(), title: "A".into(), downloaded: 0, total: None, state: DownloadState::Running },
            cancel_tx: tx, spec, generation: 7,
        });
        assert!(superseded(&key, 6), "an older generation is superseded by the one in the list");
        assert!(!superseded(&key, 7));
        assert!(!superseded("nobody", 1));
        cancel_download(&key);
        assert!(!active_downloads().iter().any(|d| d.key == key), "a cancelled job leaves the list at once");
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
                    generation: 0,
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
