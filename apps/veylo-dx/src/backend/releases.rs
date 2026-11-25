//! Home's "New digital releases" shelf powered strictly by official, neutral metadata:
//! 1. When a TMDB v3 API key is configured, queries TMDB Discover for recent Digital & Physical
//!    movie releases (`with_release_type=4|5`) and resolves IMDb IDs cleanly.
//! 2. Without a TMDB key (or as fallback), uses BetterPosters' TMDB/Trakt charts or Cinemeta's
//!    popular catalog filtered to recent years.
use super::*;

const FEED_TTL: Duration = Duration::from_secs(45 * 60);
const FEED_CACHE_FILE: &str = "digital-releases-v2.json";

static FEED_MEM: OnceLock<Mutex<(std::time::Instant, Vec<Media>)>> = OnceLock::new();

fn current_year() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_767_225_600);
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2)) as i32
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
        .filter(|m| m.kind == "movie" && m.id.starts_with("tt"))
        .filter(|m| m.first_year().is_some_and(|y| y >= year - 1 && y <= year + 1))
        .filter(|m| seen.insert(m.id.clone()))
        .take(limit)
        .collect()
}

/// Query TMDB Discover for official Digital & Physical movie releases (`with_release_type=4|5`).
async fn tmdb_digital_releases(api_key: String, year: i32) -> Vec<Media> {
    on_runtime(async move {
        let http = backend().http.clone();
        let min_date = format!("{}-01-01", year - 1);
        let resp = http.get("https://api.themoviedb.org/3/discover/movie")
            .query(&[
                ("api_key", api_key.as_str()),
                ("sort_by", "popularity.desc"),
                ("with_release_type", "4|5"),
                ("release_date.gte", min_date.as_str()),
                ("vote_count.gte", "25"),
                ("page", "1"),
            ])
            .timeout(Duration::from_secs(6))
            .send().await.ok()?
            .error_for_status().ok()?
            .json::<Value>().await.ok()?;
        let results = resp["results"].as_array()?.iter().take(18).cloned().collect::<Vec<_>>();
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(6));
        let mut set = tokio::task::JoinSet::new();
        for (idx, row) in results.into_iter().enumerate() {
            let Some(tmdb_id) = row["id"].as_i64() else { continue };
            let http = http.clone();
            let api_key = api_key.clone();
            let sem = sem.clone();
            set.spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                let ext = http.get(format!("https://api.themoviedb.org/3/movie/{tmdb_id}/external_ids"))
                    .query(&[("api_key", api_key.as_str())])
                    .timeout(Duration::from_secs(5))
                    .send().await.ok()?
                    .error_for_status().ok()?
                    .json::<Value>().await.ok()?;
                let imdb_id = ext["imdb_id"].as_str().filter(|id| id.starts_with("tt"))?.to_owned();
                let title = s(&row, "title");
                let rel_year = s(&row, "release_date").chars().take(4).collect::<String>();
                let poster = row["poster_path"].as_str()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"))
                    .unwrap_or_else(|| format!("https://images.metahub.space/poster/medium/{imdb_id}/img"));
                let backdrop = row["backdrop_path"].as_str()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!("https://image.tmdb.org/t/p/w1280{p}"))
                    .unwrap_or_default();
                let raw = json!({
                    "id": imdb_id,
                    "type": "movie",
                    "name": title,
                    "releaseInfo": rel_year,
                    "description": s(&row, "overview"),
                    "poster": poster,
                    "background": backdrop,
                    "reason": "Digital & Physical",
                });
                let media = Media::from_json(&raw, CINEMETA)?;
                Some((idx, media))
            });
        }
        let mut hydrated = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some(pair)) = res { hydrated.push(pair); }
        }
        hydrated.sort_by_key(|(idx, _)| *idx);
        Some(hydrated.into_iter().map(|(_, m)| m).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
}

/// Home's "New digital releases" shelf: official TMDB Digital/Physical releases when a TMDB key is
/// configured, otherwise recent movies from BetterPosters / Cinemeta. Cached for 45 minutes.
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
    let mut items = if let Some(key) = load_settings().tmdb_key {
        tmdb_digital_releases(key, year).await
    } else {
        Vec::new()
    };

    if items.is_empty() && better_posters_on() {
        if let Some(v) = bp_get("catalog/movie/trakt-trending.json".into()).await {
            items = filter_recent_movies(bp_titles(&v, CINEMETA), year, 20);
        }
    }
    if items.is_empty() {
        if let Ok(v) = exec(json!({"op":"catalog","addon_url":CINEMETA,"type":"movie","id":"top"})).await {
            items = filter_recent_movies(metas(&v, CINEMETA), year, 20);
        }
    }

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
}
