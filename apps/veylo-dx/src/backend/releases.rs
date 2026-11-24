//! Metadata-only release intelligence from free, keyless scene databases (srrDB & PreDB.net).
//! Zero torrent files, zero magnet links, zero API keys: strictly release names, IMDb IDs,
//! verified byte sizes, nuke statuses (corrupt/out-of-sync/fake), and release dates.
//! Used for two things:
//! 1. Home's "Just out in 4K & HD" shelf (verified 4K UHD, Blu-ray & WEB-DL releases).
//! 2. Autoplay & TitlePage enrichment (nuke filtering, free localized titles, verified release
//!    boost + fake-size guard, and "4K UHD / Blu-ray / No HD yet" availability chips).
use super::*;

const SRRDB_API: &str = "https://api.srrdb.com/v1/search";
const PREDB_API: &str = "https://api.predb.net/";

const INTEL_TTL: Duration = Duration::from_secs(6 * 3600);
const FEED_TTL: Duration = Duration::from_secs(45 * 60);
const FEED_CACHE_FILE: &str = "srrdb-hd-releases.json";

/// Verified release metadata for one movie (by IMDb ID), gathered strictly from free
/// metadata-only sources (`srrDB` + `PreDB.net`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReleaseIntel {
    /// Highest verified non-cam release badge ("4K UHD", "4K WEB", "Blu-ray", "1080p WEB").
    pub best_badge: Option<String>,
    /// True when at least one verified 2160p UHD Blu-ray or 4K WEB release exists.
    pub has_4k: bool,
    /// True when at least one verified non-cam 1080p/2160p WEB or Blu-ray release exists.
    pub has_hd: bool,
    /// Verified non-cam release stems (alphanumeric lowercase) with their recorded byte size.
    pub verified: HashMap<String, u64>,
    /// Nuked release stems (alphanumeric lowercase) -> nuke reason (e.g. "out.of.sync").
    pub nuked: HashMap<String, String>,
    /// Localized / alternate movie titles extracted for free from srrDB releases under the same IMDb ID.
    pub localized_titles: Vec<String>,
}

static INTEL_CACHE: OnceLock<Mutex<HashMap<String, (std::time::Instant, ReleaseIntel)>>> = OnceLock::new();
static FEED_MEM: OnceLock<Mutex<(std::time::Instant, Vec<Media>)>> = OnceLock::new();

static_re!(release_year_split, r"(?i)[._\s-]+((?:19|20)\d{2})(?:[._\s-]+|$)");
static_re!(series_release_tag, r"(?i)\bS\d{1,2}(?:E\d{1,3})?\b|\b\d{1,2}x\d{2,3}\b");
static_re!(cam_release_tag, r"(?i)\b(?:CAM|HDCAM|TS|HDTS|TELESYNC|TC|HDTC|TELECINE|SCREENER|DVDSCR|WORKPRINT|R5|DVDRIP|XVID)\b");

/// Normalize a release or file name to lowercase alphanumeric characters for robust matching
/// across dot/space/underscore/extension differences.
pub(crate) fn normalize_release_key(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stripped = super::sources::video_ext_suffix().replace_all(base, "");
    stripped.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Format a numeric srrDB `imdbId` ("21285562" or "987654") into a canonical IMDb ID ("tt21285562").
pub(crate) fn format_imdb_id(raw: &str) -> Option<String> {
    let digits: String = raw.trim().trim_start_matches("tt").chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 5 || digits.len() > 10 { return None; }
    if digits.len() < 7 {
        Some(format!("tt{:0>7}", digits))
    } else {
        Some(format!("tt{digits}"))
    }
}

/// Extract the title prefix before the 4-digit year from a scene release name
/// (`"Das.Ende.Der.Sterne.2026.German.DL.2160p..."` -> `Some("Das Ende Der Sterne")`).
pub(crate) fn extract_release_title(release: &str) -> Option<String> {
    let m = release_year_split().find(release)?;
    let prefix = &release[..m.start()];
    let cleaned = prefix.replace(['.', '_'], " ");
    let title = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    (title.len() >= 2 && title.len() <= 90).then_some(title)
}

/// Rank and label a clean HD/4K movie release name. Returns `None` for CAM/TS/SD/TV episodes.
pub(crate) fn release_quality_badge(release: &str) -> Option<(u8, &'static str)> {
    if series_release_tag().is_match(release) || cam_release_tag().is_match(release) {
        return None;
    }
    let u = release.to_ascii_uppercase();
    let is_4k = u.contains("2160P") || u.contains("UHD");
    let is_1080 = u.contains("1080P");
    let is_bluray = u.contains("BLURAY") || u.contains("BLU-RAY") || u.contains("REMUX") || u.contains("BDRIP");
    let is_web = u.contains("WEB");
    if is_4k && is_bluray { Some((4, "4K UHD")) }
    else if is_4k && is_web { Some((3, "4K WEB")) }
    else if is_4k { Some((3, "4K")) }
    else if is_1080 && is_bluray { Some((2, "Blu-ray")) }
    else if is_1080 && is_web { Some((1, "1080p WEB")) }
    else { None }
}

/// Parse a `ReleaseIntel` from raw `srrDB` and optional `PreDB` JSON answers.
pub(crate) fn parse_release_intel(srrdb: &Value, predb: Option<&Value>) -> ReleaseIntel {
    let mut intel = ReleaseIntel::default();
    let mut best_rank = 0u8;
    for row in srrdb["results"].as_array().into_iter().flatten() {
        let rel = s(row, "release");
        if rel.is_empty() { continue; }
        if let Some(title) = extract_release_title(&rel) {
            if !intel.localized_titles.iter().any(|t| t.eq_ignore_ascii_case(&title)) && intel.localized_titles.len() < 16 {
                intel.localized_titles.push(title);
            }
        }
        let Some((rank, badge)) = release_quality_badge(&rel) else { continue };
        intel.has_hd = true;
        if rank >= 3 { intel.has_4k = true; }
        if rank > best_rank {
            best_rank = rank;
            intel.best_badge = Some(badge.to_owned());
        }
        let key = normalize_release_key(&rel);
        let size = row["size"].as_u64().unwrap_or(0);
        if !key.is_empty() {
            intel.verified.entry(key).and_modify(|s| *s = (*s).max(size)).or_insert(size);
        }
    }
    if let Some(pre) = predb {
        for row in pre["data"].as_array().into_iter().flatten() {
            let status = row["status"].as_i64().unwrap_or(0);
            if status == 0 { continue; }
            let rel = s(row, "release");
            let key = normalize_release_key(&rel);
            if !key.is_empty() {
                let reason = s(row, "reason");
                intel.nuked.insert(key, if reason.is_empty() { "nuked".into() } else { reason });
            }
        }
    }
    intel
}

/// Check whether a candidate source matches a nuked PreDB release.
pub(crate) fn is_nuked_source(intel: &ReleaseIntel, src: &Source) -> bool {
    if intel.nuked.is_empty() { return false; }
    let file_key = if !src.filename.is_empty() { normalize_release_key(&src.filename) } else { String::new() };
    let desc_key = normalize_release_key(src.description.lines().next().unwrap_or(""));
    (!file_key.is_empty() && intel.nuked.contains_key(&file_key))
        || (!desc_key.is_empty() && intel.nuked.contains_key(&desc_key))
}

/// Authenticity check against verified srrDB releases:
/// - `Some(true)`  = verified scene release with plausible byte size (boost).
/// - `Some(false)` = claims a verified scene release name, but size is suspiciously tiny (<40% of real size).
/// - `None`        = not in srrDB (e.g. P2P/remux release; treated neutrally).
pub(crate) fn verified_scene_check(intel: &ReleaseIntel, src: &Source) -> Option<bool> {
    if intel.verified.is_empty() { return None; }
    let candidates = [
        (!src.filename.is_empty()).then(|| normalize_release_key(&src.filename)),
        Some(normalize_release_key(src.description.lines().next().unwrap_or(""))),
    ];
    for key in candidates.into_iter().flatten().filter(|k| !k.is_empty()) {
        if let Some(&expected_size) = intel.verified.get(&key) {
            if expected_size > 0 {
                if let Some(actual) = src.size_bytes.filter(|b| *b > 0) {
                    if (actual as f64) < (expected_size as f64 * 0.40) {
                        return Some(false);
                    }
                }
            }
            return Some(true);
        }
    }
    None
}

/// Synchronously read cached `ReleaseIntel` for an IMDb ID (0 ms latency).
pub fn cached_release_intel(imdb_id: &str) -> Option<ReleaseIntel> {
    let canonical = format_imdb_id(imdb_id)?;
    let map = INTEL_CACHE.get()?.lock().ok()?;
    let (at, intel) = map.get(&canonical)?;
    (at.elapsed() < INTEL_TTL).then(|| intel.clone())
}

/// Query free, keyless `srrDB` (+ optional fast `PreDB.net` nuke check) for one movie's IMDb ID.
/// Cached in memory for 6 hours. Never blocks on failure.
pub async fn release_intel(imdb_id: &str, title_hint: Option<&str>) -> Option<ReleaseIntel> {
    let canonical = format_imdb_id(imdb_id)?;
    if let Some(hit) = cached_release_intel(&canonical) { return Some(hit); }
    let numeric = canonical.trim_start_matches("tt").trim_start_matches('0').to_owned();
    if numeric.is_empty() { return None; }
    let predb_query = title_hint
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join("."))
        .filter(|q| q.len() >= 3);

    let fetched = on_runtime(async move {
        let http = backend().http.clone();
        let srr_url = format!("{SRRDB_API}/imdb:{numeric}");
        let srr_fut = async {
            http.get(&srr_url)
                .timeout(Duration::from_millis(3500))
                .send().await.ok()?
                .error_for_status().ok()?
                .json::<Value>().await.ok()
        };
        let pre_fut = async {
            let q = predb_query?;
            http.get(PREDB_API)
                .query(&[("type", "search"), ("q", q.as_str())])
                .timeout(Duration::from_millis(1200))
                .send().await.ok()?
                .error_for_status().ok()?
                .json::<Value>().await.ok()
        };
        let (srr, pre) = tokio::join!(srr_fut, pre_fut);
        srr.map(|s| parse_release_intel(&s, pre.as_ref()))
    }).await.ok().flatten()?;

    if let Ok(mut map) = INTEL_CACHE.get_or_init(Default::default).lock() {
        cache_insert(&mut map, 300, canonical, (std::time::Instant::now(), fetched.clone()));
    }
    Some(fetched)
}

/// Candidate discovered from the live srrDB feed before metadata hydration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FeedEntry {
    pub imdb_id: String,
    pub best_rank: u8,
    pub badge: &'static str,
    pub date: String,
    pub non_foreign: bool,
}

/// Merge raw srrDB search responses into a deduplicated, newest-first list of verified 4K/HD movies.
pub(crate) fn merge_srrdb_feed(responses: &[Value], limit: usize) -> Vec<FeedEntry> {
    let mut by_id: HashMap<String, FeedEntry> = HashMap::new();
    for v in responses {
        for row in v["results"].as_array().into_iter().flatten() {
            let rel = s(row, "release");
            let Some(imdb_raw) = row["imdbId"].as_str() else { continue };
            let Some(imdb_id) = format_imdb_id(imdb_raw) else { continue };
            let Some((rank, badge)) = release_quality_badge(&rel) else { continue };
            let date = s(row, "date");
            let non_foreign = row["isForeign"].as_str() != Some("yes");
            by_id.entry(imdb_id.clone())
                .and_modify(|e| {
                    if rank > e.best_rank { e.best_rank = rank; e.badge = badge; }
                    if date > e.date { e.date = date.clone(); }
                    e.non_foreign |= non_foreign;
                })
                .or_insert(FeedEntry { imdb_id, best_rank: rank, badge, date, non_foreign });
        }
    }
    let mut entries: Vec<FeedEntry> = by_id.into_values().collect();
    // Prioritize non-foreign (English/international) verified HD/4K releases, newest first, then best format.
    entries.sort_by(|a, b| {
        b.non_foreign.cmp(&a.non_foreign)
            .then_with(|| b.date.cmp(&a.date))
            .then_with(|| b.best_rank.cmp(&a.best_rank))
    });
    entries.truncate(limit);
    entries
}

fn current_year_month() -> (i32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_767_225_600);
    // Civil date from Unix days (Howard Hinnant's algorithm, exact for all Gregorian years).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + i64::from(m <= 2)) as i32;
    (year, m as u32)
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
        let _ = std::fs::write(backend().data_dir.join(FEED_CACHE_FILE), text);
    }
}

/// Home's "Just out in 4K & HD" shelf: queries free, keyless `srrDB` for the latest verified
/// 2160p UHD / Blu-ray / WEB-DL movie releases and hydrates them via free metadata
/// (BetterPosters first, Cinemeta fallback). Cached in memory & on disk for 45 minutes.
pub async fn new_hd_releases() -> Vec<Media> {
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

    let (year, month) = current_year_month();
    let mut urls = vec![
        format!("{SRRDB_API}/{year}/2160p/order:date-desc"),
        format!("{SRRDB_API}/{year}/bluray/order:date-desc"),
        format!("{SRRDB_API}/{year}/web/order:date-desc"),
    ];
    if month <= 4 {
        urls.push(format!("{SRRDB_API}/{}/2160p/order:date-desc", year - 1));
    }

    let responses: Vec<Value> = on_runtime(async move {
        let http = backend().http.clone();
        let mut set = tokio::task::JoinSet::new();
        for url in urls {
            let http = http.clone();
            set.spawn(async move {
                http.get(&url)
                    .timeout(Duration::from_secs(5))
                    .send().await.ok()?
                    .error_for_status().ok()?
                    .json::<Value>().await.ok()
            });
        }
        let mut out = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some(v)) = res { out.push(v); }
        }
        out
    }).await.unwrap_or_default();

    let entries = merge_srrdb_feed(&responses, 22);
    if entries.is_empty() { return Vec::new(); }

    let bp_on = better_posters_on();
    // Hydrate the discovered IMDb IDs concurrently via free metadata (BetterPosters -> Cinemeta).
    let hydrated: Vec<(usize, Media)> = on_runtime(async move {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut set = tokio::task::JoinSet::new();
        for (idx, entry) in entries.into_iter().enumerate() {
            let sem = sem.clone();
            set.spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                let meta_val = if bp_on {
                    bp_get(format!("meta/movie/{}.json", entry.imdb_id)).await
                        .and_then(|v| v.get("meta").cloned())
                        .filter(|v| v.is_object())
                } else {
                    None
                };
                let meta_val = match meta_val {
                    Some(v) => Some(v),
                    None => backend().core
                        .execute(json!({"op":"meta","type":"movie","id":entry.imdb_id,"addon_url":CINEMETA}))
                        .await.ok()
                        .and_then(|v| v.get("meta").cloned().or(Some(v)))
                        .filter(|v| v.is_object()),
                }?;
                let mut raw = meta_val;
                if let Some(obj) = raw.as_object_mut() {
                    obj.entry("id").or_insert_with(|| json!(entry.imdb_id));
                    obj.entry("type").or_insert_with(|| json!("movie"));
                    obj.insert("reason".into(), json!(entry.badge));
                    if obj.get("poster").and_then(Value::as_str).is_none_or(str::is_empty) {
                        obj.insert("poster".into(), json!(format!("https://images.metahub.space/poster/medium/{}/img", entry.imdb_id)));
                    }
                }
                let media = Media::from_json(&raw, CINEMETA)?;
                Some((idx, media))
            });
        }
        let mut out = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some(item)) = res { out.push(item); }
        }
        out
    }).await.unwrap_or_default();

    let mut ordered = hydrated;
    ordered.sort_by_key(|(idx, _)| *idx);
    let items: Vec<Media> = ordered.into_iter().map(|(_, m)| m).collect();
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
    fn formats_imdb_ids_and_extracts_localized_titles() {
        assert_eq!(format_imdb_id("21285562").as_deref(), Some("tt21285562"));
        assert_eq!(format_imdb_id("987654").as_deref(), Some("tt0987654"));
        assert_eq!(format_imdb_id("tt39316936").as_deref(), Some("tt39316936"));
        assert_eq!(format_imdb_id("123"), None);

        assert_eq!(
            extract_release_title("Das.Ende.Der.Sterne.2026.German.DL.2160p.UHD.BluRay.x265-WRECKED").as_deref(),
            Some("Das Ende Der Sterne")
        );
        assert_eq!(
            extract_release_title("Clashing.Through.the.Snow.2026.2160p.WEB.h265-ETHEL").as_deref(),
            Some("Clashing Through the Snow")
        );
    }

    #[test]
    fn parses_srrdb_and_predb_nukes_and_rejects_cams_and_fake_sizes() {
        let srr = json!({"results": [
            {"release": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE", "date": "2026-11-24 16:16:30", "isForeign": "no", "imdbId": "21285562", "size": 29927807079u64},
            {"release": "Das.Ende.Der.Sterne.2026.German.DL.2160p.UHD.BluRay.x265-WRECKED", "date": "2026-11-24 18:53:54", "isForeign": "yes", "imdbId": "21285562", "size": 28631597302u64},
            {"release": "The.Dog.Stars.2026.1080p.HDCAM-BAD", "date": "2026-11-10 10:00:00", "isForeign": "no", "imdbId": "21285562", "size": 3000000000u64}
        ]});
        let pre = json!({"data": [
            {"release": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-BADGRP", "status": 1, "reason": "out.of.sync"},
            {"release": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE", "status": 0, "reason": ""}
        ]});
        let intel = parse_release_intel(&srr, Some(&pre));
        assert!(intel.has_4k);
        assert!(intel.has_hd);
        assert_eq!(intel.best_badge.as_deref(), Some("4K UHD"));
        assert!(intel.localized_titles.iter().any(|t| t == "Das Ende Der Sterne"));

        let good = source_of("A", &json!({
            "title": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE.mkv",
            "url": "https://x.invalid/good",
            "behaviorHints": {"filename": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE.mkv", "videoSize": 29900000000u64}
        }));
        let fake_tiny = source_of("A", &json!({
            "title": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE.mkv",
            "url": "https://x.invalid/tiny",
            "behaviorHints": {"filename": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE.mkv", "videoSize": 2000000000u64}
        }));
        let nuked = source_of("A", &json!({
            "title": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-BADGRP.mkv",
            "url": "https://x.invalid/nuked",
            "behaviorHints": {"filename": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-BADGRP.mkv", "videoSize": 29000000000u64}
        }));
        assert_eq!(verified_scene_check(&intel, &good), Some(true));
        assert_eq!(verified_scene_check(&intel, &fake_tiny), Some(false));
        assert!(!is_nuked_source(&intel, &good));
        assert!(is_nuked_source(&intel, &nuked));
    }

    #[test]
    fn merges_srrdb_feed_deduping_by_imdb_and_excluding_cams_and_tv() {
        let r1 = json!({"results": [
            {"release": "The.Dog.Stars.2026.2160p.UHD.BluRay.x265-SURCODE", "date": "2026-11-24 16:16:30", "isForeign": "no", "imdbId": "21285562"},
            {"release": "Das.Ende.Der.Sterne.2026.German.DL.2160p.UHD.BluRay.x265-WRECKED", "date": "2026-11-24 18:53:54", "isForeign": "yes", "imdbId": "21285562"},
            {"release": "Some.Show.S01E01.2160p.WEB.h265-GRP", "date": "2026-11-24 19:00:00", "isForeign": "no", "imdbId": "11111111"},
            {"release": "Cam.Movie.2026.1080p.HDCAM-GRP", "date": "2026-11-24 19:00:00", "isForeign": "no", "imdbId": "22222222"}
        ]});
        let r2 = json!({"results": [
            {"release": "Clashing.Through.the.Snow.2026.2160p.WEB.h265-ETHEL", "date": "2026-11-23 09:02:09", "isForeign": "no", "imdbId": "39316936"}
        ]});
        let feed = merge_srrdb_feed(&[r1, r2], 10);
        assert_eq!(feed.len(), 2);
        assert_eq!(feed[0].imdb_id, "tt21285562");
        assert_eq!(feed[0].badge, "4K UHD");
        assert_eq!(feed[1].imdb_id, "tt39316936");
        assert_eq!(feed[1].badge, "4K WEB");
    }
}
