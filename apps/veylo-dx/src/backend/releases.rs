//! Home's "New digital releases" shelf and Home movie quality filter, powered by keyless
//! BetterPosters (`btttr.cc`) quality-badge availability (4K, 1080p, Dolby Vision, Atmos),
//! probed cleanly via zero-body `HEAD` requests and cached for 6 hours.
use super::*;

const FEED_TTL: Duration = Duration::from_secs(45 * 60);
const FEED_CACHE_FILE: &str = "digital-releases-v3.json";
const QUALITY_TTL_SECS: i64 = 6 * 3600;
const QUALITY_CACHE_FILE: &str = "home-quality-v1.json";

static FEED_MEM: OnceLock<Mutex<(std::time::Instant, Vec<Media>)>> = OnceLock::new();
/// Cached per-IMDb-id Home quality verdict: `(checked_at_secs, has_hd_or_4k_digital_release)`.
static QUALITY_MEM: OnceLock<Mutex<HashMap<String, (i64, bool)>>> = OnceLock::new();
type QualityCell = std::sync::Arc<tokio::sync::OnceCell<Option<bool>>>;
/// Global in-flight deduplication and concurrency cap across all concurrent Home shelves.
static QUALITY_INFLIGHT: OnceLock<Mutex<HashMap<String, QualityCell>>> = OnceLock::new();
static QUALITY_SEM: OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = OnceLock::new();

pub(super) fn clear_quality_cache() {
    if let Some(m) = QUALITY_MEM.get() { if let Ok(mut g) = m.lock() { g.clear(); } }
    if let Some(m) = FEED_MEM.get() { if let Ok(mut g) = m.lock() { g.1.clear(); } }
    if let Some(b) = BACKEND.get() {
        let _ = std::fs::remove_file(b.data_dir.join(QUALITY_CACHE_FILE));
        let _ = std::fs::remove_file(b.data_dir.join(FEED_CACHE_FILE));
    }
}

fn current_year() -> i32 {
    civil(now_secs()).0 as i32
}

/// Only recent movies (current or prior year, or unknown release year) need a digital/HD release
/// check. Older catalog films and TV series are always kept without network overhead.
pub(crate) fn needs_quality_check(m: &Media, year: i32) -> bool {
    if m.kind != "movie" { return false; }
    if imdb_of(m).is_none() { return false; }
    match m.first_year() {
        Some(y) => y >= year - 1,
        None => true,
    }
}

/// Given ETags from BetterPosters' `(poster, poster-q)` and `(poster-n, poster-nq)` `HEAD` probes:
/// - Returns `Some(false)` when at least one pair returned identical non-empty ETags (meaning
///   BetterPosters stamped no `4K / 1080p / Dolby` quality badge onto the poster).
/// - Returns `Some(true)` when both pairs returned distinct valid ETags (a quality badge was drawn).
/// - Returns `None` when network/CDN errors prevented a clear answer (fail-open).
pub(crate) fn bp_etag_has_quality(
    base: Option<&str>,
    base_q: Option<&str>,
    plain: Option<&str>,
    plain_q: Option<&str>,
) -> Option<bool> {
    let same_base = base.zip(base_q).map(|(a, b)| !a.is_empty() && a == b);
    let same_plain = plain.zip(plain_q).map(|(a, b)| !a.is_empty() && a == b);
    if same_base == Some(true) || same_plain == Some(true) {
        return Some(false);
    }
    if same_base == Some(false) && same_plain == Some(false) {
        return Some(true);
    }
    None
}

fn load_quality_disk() -> HashMap<String, (i64, bool)> {
    let path = backend().data_dir.join(QUALITY_CACHE_FILE);
    let Ok(text) = std::fs::read_to_string(path) else { return HashMap::new() };
    let Ok(map) = serde_json::from_str::<HashMap<String, (i64, bool)>>(&text) else { return HashMap::new() };
    let now = now_secs();
    map.into_iter().filter(|(_, (at, _))| now - *at < QUALITY_TTL_SECS).collect()
}

fn quality_cache() -> &'static Mutex<HashMap<String, (i64, bool)>> {
    QUALITY_MEM.get_or_init(|| Mutex::new(load_quality_disk()))
}

async fn head_etag(http: &reqwest::Client, url: String) -> Option<String> {
    let resp = http.head(url).timeout(Duration::from_secs(4)).send().await.ok()?;
    if !resp.status().is_success() { return None; }
    resp.headers().get(reqwest::header::ETAG)?.to_str().ok().map(str::to_owned)
}

async fn bp_quality_check(http: &reqwest::Client, imdb: &str) -> Option<bool> {
    // Short-circuit after the first pair (2 HEADs): if poster == poster-q ETag, no quality badge
    // exists so we skip the second pair completely (cuts HEAD traffic ~40% on in-cinema films).
    let (u1, u2) = (
        format!("{BP_BASE}/poster/imdb/poster-default/{imdb}.jpg"),
        format!("{BP_BASE}/poster-q/imdb/poster-default/{imdb}.jpg"),
    );
    let (e1, e2) = tokio::join!(head_etag(http, u1), head_etag(http, u2));
    if let Some(false) = bp_etag_has_quality(e1.as_deref(), e2.as_deref(), None, None) {
        return Some(false);
    }
    let (u3, u4) = (
        format!("{BP_BASE}/poster-n/imdb/poster-default/{imdb}.jpg"),
        format!("{BP_BASE}/poster-nq/imdb/poster-default/{imdb}.jpg"),
    );
    let (e3, e4) = tokio::join!(head_etag(http, u3), head_etag(http, u4));
    bp_etag_has_quality(e1.as_deref(), e2.as_deref(), e3.as_deref(), e4.as_deref())
}

async fn bp_quality_check_dedup(http: reqwest::Client, imdb: String) -> Option<bool> {
    let cell = {
        let mut g = QUALITY_INFLIGHT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        g.entry(imdb.clone()).or_insert_with(|| std::sync::Arc::new(tokio::sync::OnceCell::new())).clone()
    };
    let sem = QUALITY_SEM.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(6))).clone();
    let imdb_run = imdb.clone();
    let res = *cell.get_or_init(|| async move {
        let Ok(_permit) = sem.acquire_owned().await else { return None };
        bp_quality_check(&http, &imdb_run).await
    }).await;
    if let Ok(mut g) = QUALITY_INFLIGHT.get_or_init(Default::default).lock() {
        if g.get(&imdb).is_some_and(|c| std::sync::Arc::ptr_eq(c, &cell)) {
            g.remove(&imdb);
        }
    }
    res
}

/// Filter a Home list of media to hide recent movies that do NOT yet have verified digital HD/4K
/// quality (e.g. theatrical-only / In Cinema titles with no BetterPosters quality badge).
/// Series, older movies, and network failures pass safely.
pub async fn filter_home_quality(items: Vec<Media>) -> Vec<Media> {
    if !better_posters_on() || items.is_empty() {
        return items;
    }
    let year = current_year();
    let now = now_secs();
    let mut to_check: Vec<String> = Vec::new();
    let mut known: HashMap<String, bool> = HashMap::new();
    {
        let cache = quality_cache();
        if let Ok(guard) = cache.lock() {
            for m in &items {
                if !needs_quality_check(m, year) { continue; }
                let Some(imdb) = imdb_of(m) else { continue };
                if let Some(&(at, ok)) = guard.get(&imdb) {
                    if now - at < QUALITY_TTL_SECS {
                        known.insert(imdb, ok);
                        continue;
                    }
                }
                if !to_check.contains(&imdb) {
                    to_check.push(imdb);
                }
            }
        }
    }
    // Cap per-shelf probes and total shelf wait time so degraded connections never stall Home.
    to_check.truncate(40);
    if !to_check.is_empty() {
        let fetched: Vec<(String, bool)> = on_runtime(async move {
            let http = backend().http.clone();
            let mut set = tokio::task::JoinSet::new();
            for imdb in to_check {
                let http = http.clone();
                set.spawn(async move {
                    bp_quality_check_dedup(http, imdb.clone()).await.map(|ok| (imdb, ok))
                });
            }
            let deadline = tokio::time::Instant::now() + Duration::from_millis(3500);
            let mut out = Vec::new();
            loop {
                match tokio::time::timeout_at(deadline, set.join_next()).await {
                    Ok(Some(Ok(Some(pair)))) => out.push(pair),
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
            out
        }).await.unwrap_or_default();

        if !fetched.is_empty() {
            let cache = quality_cache();
            let snapshot = if let Ok(mut guard) = cache.lock() {
                for (imdb, ok) in &fetched {
                    cache_insert(&mut guard, 600, imdb.clone(), (now, *ok));
                    known.insert(imdb.clone(), *ok);
                }
                serde_json::to_string(&*guard).ok()
            } else { None };
            // Persist off-lock via debounced write_soon so concurrent Home shelves never block on fsync/icacls.
            if let Some(text) = snapshot {
                if let Some(b) = BACKEND.get() {
                    write_soon(b.data_dir.join(QUALITY_CACHE_FILE), text);
                }
            }
        }
    }

    items.into_iter().filter(|m| {
        if !needs_quality_check(m, year) { return true; }
        let Some(imdb) = imdb_of(m) else { return true };
        // Keep unless positively confirmed to lack digital HD/4K quality.
        known.get(&imdb).copied().unwrap_or(true)
    }).collect()
}

fn read_disk_feed() -> Option<Vec<Media>> {
    let path = backend().data_dir.join(FEED_CACHE_FILE);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.modified().ok()?.elapsed().ok()? >= FEED_TTL { return None; }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let list: Vec<Media> = v.as_array()?
        .iter()
        .filter_map(|item| Media::from_json(item, CINEMETA))
        .collect();
    (!list.is_empty()).then_some(list)
}

fn write_disk_feed(items: &[Media]) {
    let payload: Vec<&Value> = items.iter().map(|m| &m.raw).collect();
    if let Ok(text) = serde_json::to_string(&payload) {
        let _ = write_private_file(&backend().data_dir.join(FEED_CACHE_FILE), text.as_bytes());
    }
}

/// Filter a list of catalog movies to recent releases (current or prior year), deduplicated.
pub(crate) fn filter_recent_movies(items: Vec<Media>, year: i32, limit: usize) -> Vec<Media> {
    let mut seen = HashSet::new();
    items.into_iter()
        .filter(|m| m.kind == "movie" && valid_imdb(&m.id))
        .filter(|m| m.first_year().is_some_and(|y| y >= year - 1 && y <= year + 1))
        .filter(|m| seen.insert(m.id.clone()))
        .take(limit)
        .collect()
}

/// Home's "New digital releases" shelf: recent HD/4K movies from BetterPosters / Cinemeta,
/// verified against digital quality badges. Cached for 45 minutes.
pub async fn new_digital_releases() -> Vec<Media> {
    if let Some(lock) = FEED_MEM.get() {
        if let Ok(guard) = lock.lock() {
            if guard.0.elapsed() < FEED_TTL && !guard.1.is_empty() {
                return guard.1.clone();
            }
        }
    }
    if let Some(disk) = read_disk_feed() {
        if let Ok(mut guard) = FEED_MEM.get_or_init(|| Mutex::new((std::time::Instant::now(), Vec::new()))).lock() {
            *guard = (std::time::Instant::now(), disk.clone());
        }
        return disk;
    }

    let year = current_year();
    let mut items = Vec::new();
    if better_posters_on() {
        if let Some(v) = bp_get("catalog/movie/trakt-trending.json".into()).await {
            items = filter_recent_movies(bp_titles(&v, CINEMETA), year, 30);
        }
    }
    if items.is_empty() {
        if let Ok(v) = exec(json!({"op":"catalog","addon_url":CINEMETA,"type":"movie","id":"top"})).await {
            items = filter_recent_movies(metas(&v, CINEMETA), year, 30);
        }
    }
    items = filter_home_quality(items).await;
    items.truncate(20);

    if !items.is_empty() {
        write_disk_feed(&items);
        if let Ok(mut guard) = FEED_MEM.get_or_init(|| Mutex::new((std::time::Instant::now(), Vec::new()))).lock() {
            *guard = (std::time::Instant::now(), items.clone());
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_recent_movies_and_deduplicates() {
        let mk = |id: &str, year: &str| Media::from_json(&json!({"id": id, "type": "movie", "name": id, "releaseInfo": year}), CINEMETA).unwrap();
        let list = vec![
            mk("tt1000001", "2026"),
            mk("tt1000001", "2026"),
            mk("tt1000002", "2025"),
            mk("tt1000003", "1999"),
        ];
        let out = filter_recent_movies(list, 2026, 10);
        assert_eq!(out.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["tt1000001", "tt1000002"]);
    }

    #[test]
    fn better_posters_etag_detects_missing_quality_badges() {
        assert_eq!(bp_etag_has_quality(Some("a"), Some("b"), Some("c"), Some("d")), Some(true));
        assert_eq!(bp_etag_has_quality(Some("a"), Some("a"), Some("c"), Some("d")), Some(false));
        assert_eq!(bp_etag_has_quality(Some("a"), Some("b"), Some("c"), Some("c")), Some(false));
        assert_eq!(bp_etag_has_quality(Some("a"), Some("a"), Some("c"), Some("c")), Some(false));
        assert_eq!(bp_etag_has_quality(None, None, Some("c"), Some("d")), None);
    }

    #[test]
    fn quality_check_targets_only_recent_imdb_movies() {
        let recent = Media::from_json(&json!({"id":"tt1","type":"movie","name":"New","releaseInfo":"2026"}), CINEMETA).unwrap();
        let classic = Media::from_json(&json!({"id":"tt2","type":"movie","name":"Old","releaseInfo":"2010"}), CINEMETA).unwrap();
        let series = Media::from_json(&json!({"id":"tt3","type":"series","name":"Show","releaseInfo":"2026"}), CINEMETA).unwrap();
        assert!(needs_quality_check(&recent, 2026));
        assert!(!needs_quality_check(&classic, 2026));
        assert!(!needs_quality_check(&series, 2026));
    }
}
