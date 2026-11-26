//! The bridge between the Dioxus shell and `rustmedia-core`.
//!
//! Core requests run on one dedicated Tokio runtime; the UI awaits their join
//! handles, so it never needs to know which executor Dioxus itself uses.
//! Nothing here logs addon URLs, request bodies or credentials.
//!
//! This file holds core access, catalogues, the title page, Picks, Search and viewing history;
//! `sources` (Play's choice), `subtitles` (online subtitles, skip times), `metadata` (posters,
//! BetterPosters, TMDB, TVmaze) and `store` (settings, tonight's duel, search history) hold the
//! rest, all re-exported here as `crate::backend::*`.

use crate::picks_rules::Style;
use anyhow::{anyhow, Context, Result};
use rustmedia_core::Core;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::runtime::Runtime;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

fn re(pattern: &str) -> regex::Regex { regex::Regex::new(pattern).expect("static pattern") }
macro_rules! static_re { ($name:ident, $pat:expr) => {
    pub(super) fn $name() -> &'static regex::Regex { static R: OnceLock<regex::Regex> = OnceLock::new(); R.get_or_init(|| re($pat)) }
}; }

mod account;
mod downloads;
mod metadata;
mod open;
mod releases;
mod sources;
mod store;
mod subtitles;
mod telemetry;
pub use account::*;
pub use downloads::*;
pub use metadata::*;
pub use open::*;
pub use releases::*;
pub use sources::*;
pub use store::*;
pub use subtitles::*;
pub use telemetry::*;

const CINEMETA: &str = "https://v3-cinemeta.strem.io/manifest.json";
/// Kitsu stays installed only so anime already in your Library keeps opening (its titles have
/// Kitsu ids); it is no longer shown or browsed: Home, Search, Picks and Settings skip it.
const KITSU_HOST: &str = "anime-kitsu.strem.fun";
pub fn retired_addon(manifest_url: &str) -> bool { manifest_url.contains(KITSU_HOST) }
const HISTORY_LIMIT: usize = 12;

struct Backend {
    core: Core,
    proxy: rustmedia_media_proxy::MediaProxy,
    data_dir: PathBuf,
    http: reqwest::Client,
    accents: Mutex<HashMap<String, Option<String>>>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static BACKEND: OnceLock<Backend> = OnceLock::new();

/// The app's own store (library, settings, caches). Override with VEYLO_DX_DATA.
#[cfg(target_os = "android")]
fn data_dir() -> PathBuf {
    crate::android::files_dir().unwrap_or_else(|_| PathBuf::from("/data/local/tmp/veylo-dx"))
}

#[cfg(not(target_os = "android"))]
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("VEYLO_DX_DATA") {
        return PathBuf::from(dir);
    }
    // Windows: %LOCALAPPDATA%\veylo-dx (the store and caches stay on this machine).
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    // macOS: ~/Library/Application Support/veylo-dx.
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("veylo-dx")
}

pub fn init() -> Result<()> {
    // Android can launch the UI again in the same process (activity recreated): keep the backend.
    if BACKEND.get().is_some() { return Ok(()); }
    let workers = std::thread::available_parallelism().map(|n| n.get().clamp(2, 4)).unwrap_or(2);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    let dir = data_dir();
    std::fs::create_dir_all(&dir).context("Couldn't create the data folder")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    // Some Android builds (Huawei) hide app logs: keep the last panic in the app's own folder.
    let panic_file = dir.join("last-panic.txt");
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let raw = format!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        let clean = redact_diagnostic(&raw);
        let _ = write_private_file(&panic_file, clean.as_bytes());
        default_hook(info);
    }));
    let core = Core::open(&dir)?;
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("Veylo/", env!("CARGO_PKG_VERSION"), " (+https://github.com/Aleinto97/veylo-releases)"))
        .build()?;
    let proxy = { let _inside = runtime.enter(); rustmedia_media_proxy::MediaProxy::new() };
    // The settings the backend keeps in memory, read once.
    let saved: Value = std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    BETTER_POSTERS.store(saved["better_posters"].as_bool().unwrap_or(true), std::sync::atomic::Ordering::Relaxed);
    HDR_OUTPUT.store(saved["hdr_output"].as_bool().unwrap_or(true), std::sync::atomic::Ordering::Relaxed);
    set_telemetry_enabled(saved["telemetry_enabled"].as_bool().unwrap_or(false));
    set_prefetch_sources_enabled(saved["prefetch_sources"].as_bool().unwrap_or(false));
    set_max_bitrate_mbps(parse_max_bitrate(&saved));
    set_stream_language(Some(audio_language(&saved)));
    RUNTIME.set(runtime).map_err(|_| anyhow!("Backend already started"))?;
    BACKEND
        .set(Backend { core, proxy, data_dir: dir, http, accents: Mutex::new(HashMap::new()) })
        .map_err(|_| anyhow!("Backend already started"))?;
    // Signed in: your devices' changes come in, and this one's go out, from launch on.
    start_sync();
    upload_pending_panic();
    Ok(())
}

/// Insert into an in-memory cache of answers that can be fetched again, keeping it bounded: past
/// `cap` entries it evicts a quarter of entries rather than wiping 100% of the cache at once.
pub(super) fn cache_insert<K: Eq + std::hash::Hash + Clone, V>(map: &mut HashMap<K, V>, cap: usize, key: K, value: V) {
    if map.len() >= cap && !map.contains_key(&key) {
        let drop_n = (cap / 4).max(1);
        let victims: Vec<K> = map.keys().take(drop_n).cloned().collect();
        for k in victims { map.remove(&k); }
    }
    map.insert(key, value);
}

pub(crate) fn backend_data_dir() -> PathBuf { backend().data_dir.clone() }
pub(crate) fn backend_http() -> reqwest::Client { backend().http.clone() }

fn backend() -> &'static Backend {
    BACKEND.get().expect("backend::init runs before launch")
}

/// Run a future on the core runtime and await it from any executor.
pub(crate) async fn on_runtime<T: Send + 'static>(f: impl Future<Output = T> + Send + 'static) -> Result<T> {
    let runtime = RUNTIME.get().context("Backend not started")?;
    runtime.spawn(f).await.map_err(|e| anyhow!("Background task failed: {e}"))
}

pub(crate) async fn exec(request: Value) -> Result<Value> {
    on_runtime(async move { backend().core.execute(request).await }).await?
}

pub async fn sleep(ms: u64) {
    let _ = on_runtime(tokio::time::sleep(Duration::from_millis(ms))).await;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub addon_url: String,
    pub poster: String,
    pub background: String,
    pub description: String,
    pub year: String,
    pub genres: Vec<String>,
    pub reason: String,
    pub liked: i64,
    pub watched: bool,
    /// Saved to the Library by hand.
    pub saved: bool,
    pub video_watched: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub last_video_id: Option<String>,
    pub raw: Value,
}

pub(crate) fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_owned()
}

impl Media {
    pub fn from_json(v: &Value, origin: &str) -> Option<Self> {
        let id = Some(s(v, "id")).filter(|x| !x.is_empty()).or_else(|| Some(s(v, "_id")).filter(|x| !x.is_empty()))?;
        let addon_url = [s(v, "addon_url"), s(v, "_addon_url"), origin.to_owned()]
            .into_iter()
            .find(|x| !x.is_empty())?;
        Some(Media {
            id,
            kind: Some(s(v, "type")).filter(|x| !x.is_empty()).unwrap_or_else(|| "movie".into()),
            name: Some(s(v, "name")).filter(|x| !x.is_empty()).unwrap_or_else(|| "Untitled".into()),
            addon_url,
            poster: s(v, "poster"),
            background: s(v, "background"),
            description: s(v, "description"),
            year: s(v, "releaseInfo"),
            genres: v
                .get("genres")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            reason: s(v, "reason"),
            liked: v.get("liked").and_then(Value::as_i64).unwrap_or(0),
            watched: v.get("watched").and_then(Value::as_bool).unwrap_or(false),
            saved: ["saved", "library"].iter().any(|k| v.get(*k).and_then(Value::as_bool).unwrap_or(false)),
            video_watched: v.get("video_watched").and_then(Value::as_bool).unwrap_or(false),
            position_ms: v.get("position_ms").and_then(Value::as_i64).unwrap_or(0),
            duration_ms: v.get("duration_ms").and_then(Value::as_i64).unwrap_or(0),
            last_video_id: Some(s(v, "video_id")).filter(|x| !x.is_empty()),
            raw: v.clone(),
        })
    }
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.kind, self.id, self.addon_url)
    }
    /// Wide artwork when the add-on supplies it, else the poster.
    pub fn backdrop(&self) -> &str {
        if self.background.is_empty() { &self.poster } else { &self.background }
    }
    /// First four-digit year in the release text; ranges like "2010–2017" give 2010.
    pub fn first_year(&self) -> Option<i32> {
        let bytes = self.year.as_bytes();
        (0..bytes.len().saturating_sub(3)).find_map(|i| {
            let slice = &self.year.get(i..i + 4)?;
            let boundary_before = i == 0 || !bytes[i - 1].is_ascii_digit();
            let boundary_after = bytes.get(i + 4).is_none_or(|b| !b.is_ascii_digit());
            (boundary_before && boundary_after && slice.bytes().all(|b| b.is_ascii_digit()))
                .then(|| slice.parse().ok())
                .flatten()
                .filter(|y| (1800..2200).contains(y))
        })
    }
    /// "Movie, 2024" — commas, matching the Android app's meta lines.
    pub fn meta_line(&self, with_type: bool) -> String {
        let mut parts = Vec::new();
        if with_type { parts.push(type_label(&self.kind)); }
        if !self.year.is_empty() { parts.push(self.year.clone()); }
        parts.join(", ")
    }
}

pub fn type_label(kind: &str) -> String {
    match kind {
        "movie" => "Movie".into(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

pub fn group_label(kind: &str) -> String {
    match kind {
        "movie" => "Movies".into(),
        "series" => "Series".into(),
        "anime" => "Anime".into(),
        other => type_label(other),
    }
}

// ---------------------------------------------------------------- catalogs

/// Loads the bundled public catalogs concurrently so Picks has candidates to rank quickly.
/// Failures are tolerated: whatever arrives is still usable.
pub async fn seed_catalogs() -> Vec<Media> {
    let bp_on = better_posters_on();
    on_runtime(async move {
        let mut jobs = tokio::task::JoinSet::new();
        let mut idx = 0usize;
        if bp_on {
            for (kind, id) in [("movie", "trakt-trending"), ("series", "trakt-trending")] {
                let order = idx; idx += 1;
                jobs.spawn(async move {
                    let items = bp_get(format!("catalog/{kind}/{id}.json")).await.map(|v| bp_titles(&v, CINEMETA)).unwrap_or_default();
                    (order, items)
                });
            }
        }
        for (addon, kind, id) in [(CINEMETA, "movie", "top"), (CINEMETA, "series", "top")] {
            let order = idx; idx += 1;
            jobs.spawn(async move {
                let req = json!({"op":"catalog","addon_url":addon,"type":kind,"id":id});
                let items = backend().core.execute(req).await.ok().map(|v| metas(&v, addon)).unwrap_or_default();
                (order, items)
            });
        }
        let mut results: Vec<(usize, Vec<Media>)> = Vec::new();
        while let Some(done) = jobs.join_next().await {
            if let Ok(pair) = done { results.push(pair); }
        }
        results.sort_by_key(|(o, _)| *o);
        results.into_iter().flat_map(|(_, items)| items).collect()
    }).await.unwrap_or_default()
}

fn metas(v: &Value, origin: &str) -> Vec<Media> {
    v.get("metas")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| Media::from_json(m, origin)).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- home

/// One Home shelf: an add-on catalog, optionally narrowed by one extra (genre).
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRow {
    pub key: String,
    pub title: String,
    /// Shown under the title only where it tells two shelves apart.
    pub subtitle: Option<String>,
    pub addon_url: String,
    pub kind: String,
    pub id: String,
    pub extra: Option<(String, String)>,
    /// A Top 10 shelf: ten titles (BetterPosters' posters carry each one's place).
    pub ranked: bool,
}

/// Home's "New episodes" shelf (TVmaze's schedules), not an add-on catalog.
const NEW_EPISODES_ROW: &str = "tvmaze|new-episodes";
/// Home's "New digital releases" shelf (official TMDB Digital/Physical releases or BetterPosters/Cinemeta).
const NEW_DIGITAL_RELEASES_ROW: &str = "releases|new-digital";

struct RawRow { key: String, title: String, addon_id: String, addon_name: String, addon_url: String, kind: String, id: String, extra: Option<(String, String)> }

/// A catalog that can't be listed without an extra (search-only and the like): the current
/// `extra[].isRequired`, or the legacy `extraRequired` list.
fn catalog_needs_extra(c: &Value) -> bool {
    c["extra"].as_array().into_iter().flatten().any(|e| e["isRequired"] == true)
        || c["extraRequired"].as_array().is_some_and(|r| !r.is_empty())
}

fn options_contain(extra: &Value, name: &str, option: &str) -> bool {
    extra["name"] == name && extra["options"].as_array().is_some_and(|o| o.iter().any(|x| x == option))
}

/// Same rules as homeCatalogRows + homeShelves in the Android app: three catalogs per
/// add-on without required extras, Cinemeta's animation shelf, and titles that
/// disambiguate repeated catalog names.
pub async fn home_rows() -> Result<Vec<CatalogRow>> {
    // Manifests are refreshed once per launch (addons::startup), not on every visit to Home.
    let v = exec(json!({"op":"addons"})).await?;
    let mut raw = Vec::new();
    for addon in v["addons"].as_array().into_iter().flatten() {
        let url = s(addon, "manifest_url");
        if retired_addon(&url) { continue; }
        let (aid, aname) = (s(addon, "id"), s(addon, "name"));
        let catalogs: Vec<&Value> = addon["catalogs"].as_array().into_iter().flatten().collect();
        let free = catalogs.iter().filter(|c| !catalog_needs_extra(c));
        for c in free.take(3) {
            let (kind, id) = (s(c, "type"), s(c, "id"));
            let name = Some(s(c, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| kind.clone());
            raw.push(RawRow { key: format!("{url}|{kind}|{id}"), title: name, addon_id: aid.clone(), addon_name: aname.clone(),
                addon_url: url.clone(), kind, id, extra: None });
        }
        let animation = catalogs.iter().find(|c| aid == "com.linvo.cinemeta" && c["type"] == "series" && c["id"] == "top"
            && c["extra"].as_array().into_iter().flatten().any(|e| options_contain(e, "genre", "Animation")));
        if let Some(c) = animation {
            raw.push(RawRow { key: format!("{url}|series|top|genre=Animation"), title: "Anime & animation".into(), addon_id: aid.clone(),
                addon_name: aname.clone(), addon_url: url.clone(), kind: s(c, "type"), id: s(c, "id"), extra: Some(("genre".into(), "Animation".into())) });
        }
    }
    // BetterPosters on: its charts lead Home in place of Cinemeta's own catalogs (Cinemeta's
    // animation shelf stays). Its titles keep Cinemeta as their add-on (see bp_titles).
    if better_posters_on() {
        if let Some(cinemeta) = raw.iter().find(|r| r.addon_id == "com.linvo.cinemeta").map(|r| r.addon_url.clone()) {
            raw.retain(|r| r.addon_id != "com.linvo.cinemeta" || r.extra.is_some());
            let charts = BP_CHARTS.iter().map(|(kind, id, title)| RawRow { key: format!("{BP_ROW}{kind}|{id}"), title: (*title).into(),
                addon_id: "community.betterposters".into(), addon_name: "BetterPosters".into(), addon_url: cinemeta.clone(),
                kind: (*kind).into(), id: (*id).into(), extra: None });
            raw.splice(0..0, charts);
        }
    }
    // Recent digital & physical movie releases (BetterPosters) and new TV episodes (TVmaze).
    let at = raw.iter().position(|r| r.key.starts_with(BP_ROW) && r.id == "trakt-popular").unwrap_or(raw.len().min(2));
    raw.insert(at, RawRow { key: NEW_DIGITAL_RELEASES_ROW.into(), title: "New digital releases".into(), addon_id: "releases".into(), addon_name: "BetterPosters".into(),
        addon_url: CINEMETA.into(), kind: "movie".into(), id: "new-digital".into(), extra: None });
    raw.insert(at + 1, RawRow { key: NEW_EPISODES_ROW.into(), title: "New episodes".into(), addon_id: "tvmaze".into(), addon_name: "TVmaze".into(),
        addon_url: CINEMETA.into(), kind: "series".into(), id: "new-episodes".into(), extra: None });
    let shared: Vec<String> = raw.iter().map(|r| r.title.clone()).filter(|t| {
        let mut names: Vec<&str> = raw.iter().filter(|r| &r.title == t).map(|r| r.addon_name.as_str()).collect();
        names.sort(); names.dedup(); names.len() > 1
    }).collect();
    let mut seen = std::collections::HashSet::new();
    Ok(raw.into_iter().map(|r| {
        let title = if seen.insert(format!("{}|{}", r.title, r.addon_name)) { r.title.clone() }
            else { format!("{} {}", r.title, group_label(&r.kind).to_lowercase()) };
        let subtitle = if r.addon_id == "com.linvo.cinemeta" && r.extra.as_ref().is_some_and(|(_, v)| v == "Animation") {
            Some("Cinemeta · Animated series, including anime".to_owned())
        } else if r.key == NEW_DIGITAL_RELEASES_ROW {
            Some("Recent releases · BetterPosters / Cinemeta".to_owned())
        } else if r.key == NEW_EPISODES_ROW {
            Some("Out in the last three days · TVmaze".to_owned())
        } else if shared.contains(&r.title) && !r.addon_name.is_empty() { Some(r.addon_name.clone()) } else { None };
        let ranked = r.key.starts_with(BP_ROW) && bp_ranked(&r.id);
        CatalogRow { key: r.key, title, subtitle, addon_url: r.addon_url, kind: r.kind, id: r.id, extra: r.extra, ranked }
    }).collect())
}

pub async fn catalog(row: CatalogRow) -> Result<Vec<Media>> {
    // A BetterPosters chart; Cinemeta's popular list of the same type when it can't answer.
    if row.key.starts_with(BP_ROW) {
        let mut titles = bp_get(format!("catalog/{}/{}.json", row.kind, row.id)).await.map(|v| bp_titles(&v, &row.addon_url)).unwrap_or_default();
        if !titles.is_empty() {
            if row.ranked { titles.truncate(18); }
            if row.kind == "movie" {
                titles = filter_home_quality(titles).await;
            }
            if row.ranked { titles.truncate(10); }
            if !titles.is_empty() { return Ok(titles); }
        }
        return Box::pin(catalog(CatalogRow { key: String::new(), id: "top".into(), ranked: false, ..row })).await;
    }
    if row.key == NEW_DIGITAL_RELEASES_ROW {
        let titles = new_digital_releases().await;
        anyhow::ensure!(!titles.is_empty(), "Release feed can't be reached right now");
        return Ok(titles);
    }
    if row.key == NEW_EPISODES_ROW {
        let titles = new_episodes().await;
        anyhow::ensure!(!titles.is_empty(), "TVmaze can't be reached right now");
        return Ok(titles);
    }
    let mut req = json!({"op":"catalog","addon_url":row.addon_url,"type":row.kind,"id":row.id});
    if let Some((k, v)) = &row.extra { req["extra"] = json!({ k.as_str(): v }); }
    let v = exec(req).await?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        if v["metas"].as_array().is_none_or(|m| m.is_empty()) { anyhow::bail!("{e}"); }
    }
    let mut out = metas(&v, &row.addon_url);
    let mut keys = std::collections::HashSet::new();
    out.retain(|m| keys.insert(m.key()));
    if row.kind == "movie" {
        out = filter_home_quality(out).await;
    }
    Ok(out)
}

/// The library op answers `{"items": [...]}`, not catalog-style `metas`.
pub async fn library() -> Result<Vec<Media>> {
    let v = exec(json!({"op":"library"})).await?;
    Ok(v["items"].as_array().into_iter().flatten().filter_map(|m| Media::from_json(m, "")).collect())
}

/// Started, not finished: the Continue watching shelf and the marquee's first choice.
pub fn in_progress(m: &Media) -> bool {
    // A series may have been marked complete before another season arrived. Its current
    // episode's completion state controls resume, rather than that older title-level flag.
    m.position_ms > 0 && !m.video_watched && (m.kind != "movie" || !m.watched)
}

pub fn progress_fraction(m: &Media) -> f64 {
    if m.duration_ms > 0 { (m.position_ms as f64 / m.duration_ms as f64).clamp(0.0, 1.0) } else { 0.0 }
}

fn remaining_label(m: &Media) -> Option<String> {
    let left = m.duration_ms - m.position_ms;
    if m.duration_ms <= 0 || left <= 0 { return None; }
    let minutes = (left + 30_000) / 60_000;
    Some(if minutes >= 60 { format!("{} h {} min left", minutes / 60, minutes % 60) } else { format!("{minutes} min left") })
}

/// "S2 E5, 23 min left", else "40% complete", else "In progress" (resumeContext in Kotlin).
pub fn resume_context(m: &Media) -> String {
    let episode = m.last_video_id.as_ref().and_then(|id| m.raw["videos"].as_array()?.iter().find(|v| v["id"] == id.as_str()).cloned())
        .map(|v| episode_tag(v["season"].as_i64().unwrap_or(1), v["episode"].as_i64().unwrap_or(0)));
    let parts: Vec<String> = [episode, remaining_label(m)].into_iter().flatten().collect();
    if !parts.is_empty() { return parts.join(", "); }
    if m.duration_ms > 0 {
        let f = progress_fraction(m);
        let pct = (f * 100.0).round() as i64;
        format!("{}% complete", if f > 0.0 { pct.max(1) } else { pct })
    } else { "In progress".into() }
}

enum ProgressWrite { Update(Value), Barrier(tokio::sync::oneshot::Sender<()>) }
static PROGRESS: OnceLock<tokio::sync::mpsc::UnboundedSender<ProgressWrite>> = OnceLock::new();

/// Playback completion refreshes Picks only after all preceding progress writes are stored.
pub(crate) async fn flush_progress() {
    if let Some(queue) = PROGRESS.get() {
        let (tx,rx) = tokio::sync::oneshot::channel();
        if queue.send(ProgressWrite::Barrier(tx)).is_ok() { let _ = rx.await; }
    }
}

/// Save where playback is and how much was really played since the last report (`watched_ms`):
/// the core marks a film or episode watched from that (80% played and 90% reached), never from
/// a seek. Reports are written one after another, in order, on the core runtime. `with_meta`
/// sends the title's metadata (the player's first and last report); the others only merge.
pub fn record_progress(media: Media, video_id: Option<String>, position_ms: i64, duration_ms: i64, watched_ms: i64, with_meta: bool) {
    let Some(runtime) = RUNTIME.get() else { return };
    if duration_ms <= 0 { return; }
    let queue = PROGRESS.get_or_init(|| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProgressWrite>();
        runtime.spawn(async move { while let Some(write) = rx.recv().await {
            match write {
                ProgressWrite::Update(req) => {
                    if backend().core.execute(req).await.is_ok() { sync_soon(); }
                }
                ProgressWrite::Barrier(done) => { let _ = done.send(()); }
            }
        } });
        tx
    });
    let mut req = json!({"op":"progress","type":media.kind,"id":media.id,"addon_url":media.addon_url,
        "position_ms":position_ms.max(0),"duration_ms":duration_ms,"watched_add_ms":watched_ms.clamp(0, duration_ms),
        "meta": if with_meta { media.raw.clone() } else { json!({}) }});
    if let Some(v) = &video_id { req["video_id"] = json!(v); }
    let _ = queue.send(ProgressWrite::Update(req));
}


/// Mark episodes watched or not (without moving where you are in the series).
pub async fn set_episodes_watched(media: &Media, video_ids: Vec<String>, watched: bool) -> Result<()> {
    if video_ids.is_empty() { return Ok(()); }
    exec(json!({"op":"mark_videos","type":media.kind,"id":media.id,"addon_url":media.addon_url,"watched":watched,"video_ids":video_ids,"meta":media.raw})).await?;
    sync_soon();
    Ok(())
}

/// Stop tracking a title: its history, progress, marks and rating are deleted from the store.
pub async fn forget(media: &Media) -> Result<()> {
    // Wait for any in-flight playback progress write so it cannot recreate the row right after DELETE.
    flush_progress().await;
    exec(json!({"op":"forget","type":media.kind,"id":media.id,"addon_url":media.addon_url})).await?;
    sync_forgotten(media);
    Ok(())
}

/// Episodes of a series you watched: marked, or played through.
pub async fn watched_episodes(media: &Media) -> HashSet<String> {
    exec(json!({"op":"watched_videos","type":media.kind,"id":media.id})).await.ok()
        .and_then(|v| v["video_ids"].as_array().map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect()))
        .unwrap_or_default()
}

/// "S2 E5" for "tt123:2:5" and "tmdb:123:2:5" (scheme, id, season, episode), "E5" for Kitsu's
/// "kitsu:123:5"; None for anything else, such as "mal:5114:3" (its label comes from the
/// episode list).
pub fn episode_label(video_id: &str) -> Option<String> {
    let parts: Vec<&str> = video_id.split(':').collect();
    let se = |s: &str, e: &str| match (s.parse::<i64>(), e.parse::<i64>()) { (Ok(s), Ok(e)) => Some(episode_tag(s, e)), _ => None };
    match parts.as_slice() {
        ["kitsu", _, e] => e.parse::<i64>().ok().map(|e| format!("E{e}")),
        [series, s, e] if series.starts_with("tt") => se(s, e),
        [_, _, s, e] => se(s, e),
        _ => None,
    }
}

/// "S2 E5": how an episode is named everywhere in the app.
pub fn episode_tag(season: i64, episode: i64) -> String { format!("S{season} E{episode}") }

impl Episode {
    /// "S2 E5".
    pub fn label(&self) -> String { episode_tag(self.season, self.number) }
}

/// The packaged legal player test (same title and stream as demoMedia() in Models.kt).
pub const DEMO_STREAM: &str = "https://media.w3.org/2010/05/sintel/trailer.mp4";

// ---------------------------------------------------------------- title page

#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    pub id: String, pub season: i64, pub number: i64, pub title: String,
    pub thumbnail: String, pub overview: String, pub released: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Trailer { pub youtube: Option<String>, pub url: Option<String> }

/// Everything the title's add-on says about it; absent fields stay empty, never invented.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TitleMeta {
    pub genres: Vec<String>,
    pub logo: String,
    pub runtime: String,
    pub rating: String,
    pub released: String,
    pub country: String,
    pub language: String,
    pub awards: String,
    pub website: String,
    pub directors: Vec<String>,
    pub writers: Vec<String>,
    pub cast: Vec<String>,
    pub trailers: Vec<Trailer>,
    pub episodes: Vec<Episode>,
    /// The synopsis, for titles whose catalog entry had none.
    pub description: String,
    /// Original and alternate titles when provided by the add-on or BetterPosters.
    pub alt_titles: Vec<String>,
}

fn names(meta: &Value, field: &str, link_category: &str) -> Vec<String> {
    let mut out: Vec<String> = meta[field].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    for l in meta["links"].as_array().into_iter().flatten() {
        if l["category"].as_str().is_some_and(|c| c.eq_ignore_ascii_case(link_category)) {
            if let Some(n) = l["name"].as_str() { out.push(n.to_owned()); }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| !n.trim().is_empty() && seen.insert(n.to_lowercase()));
    out
}

/// "2014-11-05T00:00:00.000Z" -> "5 Nov 2014"; anything else is shown as the add-on wrote it.
pub fn date_label(raw: &str) -> String {
    let d = raw.get(..10).unwrap_or(raw);
    let parts: Vec<&str> = d.split('-').collect();
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match parts.as_slice() {
        [y, m, day] if y.len() == 4 => match (m.parse::<usize>(), day.parse::<u32>()) {
            (Ok(m @ 1..=12), Ok(day)) => format!("{day} {} {y}", MONTHS[m - 1]),
            _ => raw.to_owned(),
        },
        _ => raw.to_owned(),
    }
}

fn episodes_from_meta(meta: &Value) -> Vec<Episode> {
    let mut episodes: Vec<Episode> = meta["videos"].as_array().into_iter().flatten().filter_map(|e| {
        let id = s(e, "id");
        (!id.is_empty()).then(|| Episode {
            id, season: e["season"].as_i64().unwrap_or(1), number: e["episode"].as_i64().unwrap_or(0),
            title: Some(s(e, "title")).filter(|t| !t.is_empty()).unwrap_or_else(|| s(e, "name")),
            thumbnail: s(e, "thumbnail"),
            overview: Some(s(e, "overview")).filter(|t| !t.is_empty()).unwrap_or_else(|| s(e, "description")),
            released: s(e, "released"),
        })
    }).collect();
    episodes.sort_by_key(|e| (if e.season == 0 { i64::MAX } else { e.season }, e.number));
    episodes.dedup_by(|a, b| a.id == b.id);
    episodes
}

static TITLE_META_CACHE: OnceLock<Mutex<HashMap<String, TitleMeta>>> = OnceLock::new();
static TITLE_PREFETCHING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Start warming `title_artwork_fast` and `title_meta` the instant a finger touches a card
/// (`onpointerdown`), gaining a 100-160ms head start before `onclick` even mounts `TitlePage`.
pub fn prefetch_title(media: &Media) {
    if cached_title_meta(media).is_some() && cached_title_artwork(media).is_some() {
        return;
    }
    let key = media.key();
    {
        let Ok(mut set) = TITLE_PREFETCHING.get_or_init(Default::default).lock() else { return };
        if set.len() > 64 { set.clear(); }
        if !set.insert(key.clone()) { return; }
    }
    let m = media.clone();
    let Some(rt) = RUNTIME.get() else { return };
    rt.spawn(async move {
        let _ = tokio::join!(title_artwork_fast(&m), title_meta(&m));
        if let Ok(mut set) = TITLE_PREFETCHING.get_or_init(Default::default).lock() {
            set.remove(&key);
        }
    });
}

/// Synchronously read cached `TitleMeta` from this session so re-opening a title renders
/// immediately on Frame 0 with zero loading flash.
pub fn cached_title_meta(media: &Media) -> Option<TitleMeta> {
    TITLE_META_CACHE.get()?.lock().ok()?.get(&media.key()).cloned()
}

pub async fn title_meta(media: &Media) -> Result<TitleMeta> {
    if let Some(hit) = cached_title_meta(media) {
        return Ok(hit);
    }
    let request = exec(json!({"op":"meta","type":media.kind,"id":media.id,"addon_url":media.addon_url}));
    // Fetch add-on meta and BetterPosters overlay concurrently — do not block title details on
    // slower artwork/TVmaze/TMDB calls (which `use_title_artwork` loads independently).
    let (addon, bp) = tokio::join!(request, bp_meta(media));
    let meta = match (addon, bp) {
        (Ok(v), bp) => merge_meta(v.get("meta").cloned().unwrap_or(v), bp.as_ref()),
        // A title Cinemeta doesn't know yet: BetterPosters alone.
        (Err(_), Some(bp)) => bp,
        (Err(e), None) => return Err(e),
    };
    let meta = &meta;
    let episodes = episodes_from_meta(meta);
    let mut trailers: Vec<Trailer> = Vec::new();
    for t in meta["trailerStreams"].as_array().into_iter().flatten() {
        trailers.push(Trailer { youtube: Some(s(t, "ytId")).filter(|x| !x.is_empty()), url: Some(s(t, "url")).filter(|x| x.starts_with("https://")) });
    }
    for t in meta["trailers"].as_array().into_iter().flatten() {
        if t["type"].as_str().is_none_or(|k| k == "Trailer") {
            trailers.push(Trailer { youtube: Some(s(t, "source")).filter(|x| !x.is_empty()), url: None });
        }
    }
    trailers.retain(|t| t.youtube.is_some() || t.url.is_some());
    let mut seen_trailers = HashSet::new();
    trailers.retain(|t| seen_trailers.insert(t.clone()));
    let genres: Vec<String> = meta["genres"].as_array().into_iter().flatten().filter_map(Value::as_str).map(crate::picks_rules::canonical_genre).collect();
    let mut alt_titles: Vec<String> = Vec::new();
    for field in ["original_title", "original_name"] {
        let t = s(meta, field);
        if !t.is_empty() && !alt_titles.iter().any(|x| x.eq_ignore_ascii_case(&t)) { alt_titles.push(t); }
    }
    for field in ["aliases", "alternate_titles", "alt_titles"] {
        for v in meta[field].as_array().into_iter().flatten() {
            let t = v.as_str().unwrap_or_default().trim();
            if !t.is_empty() && !alt_titles.iter().any(|x| x.eq_ignore_ascii_case(t)) { alt_titles.push(t.to_owned()); }
        }
    }
    let out = TitleMeta {
        genres,
        logo: s(meta, "logo"),
        runtime: s(meta, "runtime"),
        rating: s(meta, "imdbRating"),
        released: Some(s(meta, "released")).filter(|r| !r.is_empty()).map(|r| date_label(&r)).unwrap_or_default(),
        country: s(meta, "country"),
        language: s(meta, "language"),
        awards: s(meta, "awards"),
        website: Some(s(meta, "website")).filter(|w| w.starts_with("https://")).unwrap_or_default(),
        directors: names(meta, "director", "Directors"),
        writers: names(meta, "writer", "Writers"),
        cast: names(meta, "cast", "Cast"),
        trailers,
        episodes,
        description: s(meta, "description"),
        alt_titles,
    };
    if let Ok(mut c) = TITLE_META_CACHE.get_or_init(Default::default).lock() {
        cache_insert(&mut c, 200, media.key(), out.clone());
    }
    Ok(out)
}

/// What the store knows about a title opened from anywhere (a catalog shelf, Search, Picks):
/// saved, watched, liked and progress come from there, the rest from the add-on.
pub async fn stored(media: &Media) -> Option<Media> {
    let v = exec(json!({"op":"item","type":media.kind,"id":media.id,"addon_url":media.addon_url})).await.ok()?;
    Media::from_json(&v["item"], &media.addon_url)
}

/// Save a title to the Library, or take it out (its history stays).
pub async fn set_saved(media: &Media, saved: bool) -> Result<()> {
    exec(json!({"op":"set_library","type":media.kind,"id":media.id,"addon_url":media.addon_url,"saved":saved,"meta":media.raw})).await?;
    sync_soon();
    Ok(())
}

/// Clear the on-disk subtitle cache.
pub async fn clear_subtitle_cache() -> Result<()> {
    exec(json!({"op":"clear_subtitle_cache"})).await?;
    Ok(())
}

/// Per-episode playback map for one series: `video_id -> (position_ms, duration_ms, watched)`.
pub async fn episode_playback_map(media: &Media) -> HashMap<String, (i64, i64, bool)> {
    let Ok(v) = exec(json!({"op":"playback_all","type":media.kind,"id":media.id})).await else { return HashMap::new() };
    v["entries"].as_array().into_iter().flatten().filter_map(|e| {
        let vid = e["video_id"].as_str()?.to_owned();
        let pos = e["position_ms"].as_i64().unwrap_or(0);
        let dur = e["duration_ms"].as_i64().unwrap_or(0);
        let watched = e["watched"].as_bool().unwrap_or(false);
        Some((vid, (pos, dur, watched)))
    }).collect()
}

/// Live SQLite resume state for a single video/episode: `(position_ms, duration_ms, watched)`.
pub async fn single_episode_playback(media: &Media, video_id: &str) -> (i64, i64, bool) {
    let Ok(v) = exec(json!({
        "op":"playback","type":media.kind,"id":media.id,"addon_url":media.addon_url,"video_id":video_id
    })).await else { return (0, 0, false) };
    (
        v["position_ms"].as_i64().unwrap_or(0),
        v["duration_ms"].as_i64().unwrap_or(0),
        v["watched"].as_bool().unwrap_or(false),
    )
}

/// A movie franchise / collection shelf shown on a TitlePage.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CollectionShelf {
    pub name: String,
    pub parts: Vec<Media>,
}

/// All released installments in a movie's collection (chronological), via keyless Wikidata `P179`
/// franchise links, falling back to `crates/core`'s curated franchise collections.
pub async fn collection_parts(m: &Media) -> Option<CollectionShelf> {
    if m.kind != "movie" { return None; }
    if let Some(shelf) = wikidata_collection_parts(m).await {
        return Some(shelf);
    }
    // Fallback: check core's local franchise collections.
    let v = exec(json!({"op":"collections"})).await.ok()?;
    for col in v["collections"].as_array()? {
        if col["kind"].as_str() != Some("addon_curated_franchise") { continue; }
        let items = metas(col, "");
        if items.len() >= 2 && items.iter().any(|x| x.key() == m.key() || x.id == m.id) {
            return Some(CollectionShelf { name: s(col, "name"), parts: items });
        }
    }
    None
}

/// Titles similar to `m`: merges `crates/core`'s local `similar` + TMDB/Cinemeta `related()`,
/// and re-ranks by on-device `bge-small-en-v1.5` synopsis cosine similarity + learned taste.
pub async fn similar_titles(m: &Media) -> Vec<Media> {
    let core_fut = exec(json!({"op":"similar","type":m.kind,"id":m.id,"addon_url":m.addon_url,"limit":16}));
    let (core_res, rel) = tokio::join!(core_fut, related(m));
    let mut combined = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(m.key());
    let mut push = |item: Media| {
        if item.liked < 0 || item.id == m.id { return; }
        if seen.insert(item.key()) { combined.push(item); }
    };
    for item in rel { push(item); }
    if let Ok(v) = core_res {
        for item in metas(&v, "") { push(item); }
    }
    if combined.len() <= 1 { return combined; }
    // Re-rank with on-device synopsis embeddings + lasting taste when available.
    let target = m.clone();
    if !crate::picks_ai::failed() && crate::picks_ai::embedding(&target).is_none() {
        let _ = crate::picks_ai::embed(&target).await;
    }
    let target_emb = crate::picks_ai::embedding(&target);
    let list = combined.clone();
    on_runtime(async move {
        tokio::task::spawn_blocking(move || {
            let taste = crate::brain::load(0);
            let mut scored: Vec<(f64, Media)> = list.into_iter().enumerate().map(|(i, item)| {
                let sim = match (&target_emb, crate::picks_ai::embedding(&item)) {
                    (Some(a), Some(b)) => crate::picks_ai::cosine(a, &b) * 2.0,
                    _ => 0.0,
                };
                let score = sim + taste.lasting(&item) * 0.15 - (i as f64) * 0.015;
                (score, item)
            }).collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            scored.into_iter().map(|(_, item)| item).take(16).collect()
        }).await.unwrap_or(combined)
    }).await.unwrap_or_default()
}

/// On-device semantic / "Vibe" search using `bge-small-en-v1.5` query embeddings against the
/// local catalog, library, and Picks pools (zero cloud calls).
pub async fn semantic_search(query: String) -> Result<Vec<Media>> {
    if crate::picks_ai::failed() {
        anyhow::bail!("Local AI model is unavailable on this device");
    }
    let q_emb = crate::picks_ai::embed_query(&query).await?;
    let (lib_res, picks_res, seeds) = tokio::join!(library(), picks(), seed_catalogs());
    let mut pool = Vec::new();
    let mut seen = HashSet::new();
    for m in lib_res.unwrap_or_default().into_iter()
        .chain(picks_res.map(|p| p.items).unwrap_or_default())
        .chain(seeds)
    {
        if m.liked < 0 { continue; }
        if seen.insert(m.key()) { pool.push(m); }
    }
    // Embed up to 48 unembedded titles from the pool quickly so cold caches still return great matches.
    let missing: Vec<Media> = pool.iter().filter(|m| crate::picks_ai::embedding(m).is_none() && !m.description.is_empty()).take(48).cloned().collect();
    for m in missing {
        let _ = crate::picks_ai::embed(&m).await;
    }
    let ranked = on_runtime(async move {
        tokio::task::spawn_blocking(move || {
            let taste = crate::brain::load(0);
            let mut scored: Vec<(f64, Media)> = pool.into_iter().filter_map(|m| {
                let emb = crate::picks_ai::embedding(&m)?;
                let sim = crate::picks_ai::cosine(&q_emb, &emb);
                if sim < 0.30 { return None; }
                let score = sim + taste.lasting(&m) * 0.04;
                Some((score, m))
            }).collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            scored.into_iter().map(|(_, m)| m).take(24).collect::<Vec<_>>()
        }).await.unwrap_or_default()
    }).await?;
    Ok(ranked)
}

/// Mark a film watched or not: the core's own mutation.
pub async fn set_watched(media: &Media, watched: bool) -> Result<()> {
    exec(json!({"op":"mark_watched","type":media.kind,"id":media.id,"addon_url":media.addon_url,"watched":watched,"meta":media.raw})).await?;
    sync_soon();
    Ok(())
}

// ---------------------------------------------------------------- picks

pub struct Picks {
    pub items: Vec<Media>,
}

pub async fn picks() -> Result<Picks> {
    let v = exec(json!({"op":"recommendations","limit":100,"exclude_liked":true})).await?;
    Ok(Picks {
        items: metas(&v, "").into_iter().filter(|m| m.liked == 0).collect(),
    })
}

/// -1 not for me, 0 clear, 1 like. The title's metadata travels with it so the
/// core can rank against it later.
pub async fn feedback(media: &Media, value: i64) -> Result<()> {
    exec(json!({"op":"feedback","type":media.kind,"id":media.id,"addon_url":media.addon_url,
        "value":value,"meta":media.raw})).await?;
    sync_soon();
    Ok(())
}

/// For a later film in a collection: the first released part you have not watched, when it
/// comes before this one (a Cinemeta title, ready for Picks). None when this film is where to
/// start, or for anything that is not part of a collection. Powered keylessly via Wikidata `P179`.
pub async fn earlier_part(m: &Media, watched: &HashSet<String>) -> Option<Media> {
    if m.kind != "movie" { return None; }
    let own_imdb = imdb_of(m)?;
    let shelf = wikidata_collection_parts(m).await?;
    let at = shelf.parts.iter().position(|p| p.id == own_imdb)?;
    for part in shelf.parts.into_iter().take(at) {
        let key = crate::picks_rules::title_key(&part);
        if watched.contains(&part.id) || watched.contains(&key) { continue; }
        return Some(part);
    }
    None
}

/// The trailers to play for a Picks card: curated artwork trailers first when available, the
/// add-on's YouTube trailers after them as alternatives; the add-on's own list only when empty.
pub fn official_first(tmdb: &[String], addon: &[Trailer]) -> Vec<Trailer> {
    if tmdb.is_empty() { return addon.to_vec(); }
    let mut out: Vec<Trailer> = tmdb.iter().map(|k| Trailer { youtube: Some(k.clone()), url: None }).collect();
    for t in addon {
        if t.youtube.is_some() && !out.iter().any(|o| o.youtube == t.youtube) { out.push(Trailer { youtube: t.youtube.clone(), url: None }); }
    }
    out
}

/// Whether a title can stand in a duel: a poster to show and a trailer to watch.
pub async fn duel_ready(m: &Media) -> Option<Media> {
    if posters(m).is_empty() { return None; }
    let meta = title_meta(m).await.ok()?;
    if meta.trailers.is_empty() { return None; }
    let mut ready = m.clone();
    for genre in meta.genres { if !ready.genres.contains(&genre) { ready.genres.push(genre); } }
    ready = crate::picks_rules::tidy(ready);
    ready.raw["genres"] = json!(ready.genres);
    if let Some(art) = artwork(&ready).await.filter(|a| !a.genres.is_empty()) {
        crate::picks_rules::settle_style(&mut ready, art.genres.iter().any(|g| g.eq_ignore_ascii_case("Animation")));
    }
    Some(ready)
}


// ---------------------------------------------------------------- search

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SearchOutcome {
    pub results: Vec<(String, Media)>,
    pub failed: Vec<String>,
    pub catalogs: usize,
}

pub async fn search(query: String) -> Result<SearchOutcome> {
    let v = exec(json!({"op":"search","query":query})).await?;
    let mut out = SearchOutcome::default();
    let mut seen = HashSet::new();
    for group in v.get("groups").and_then(Value::as_array).into_iter().flatten() {
        if retired_addon(&s(group, "addon_url")) { continue; }
        out.catalogs += 1;
        let addon = s(group, "addon_name");
        if group.get("error").is_some() && !out.failed.contains(&addon) { out.failed.push(addon.clone()); }
        // The first add-on to list a title keeps it.
        for m in metas(group, &s(group, "addon_url")) {
            if seen.insert(m.key()) { out.results.push((addon.clone(), m)); }
        }
    }
    out.results = rank_search_results(&query, out.results);
    Ok(out)
}

/// Whether a release names this title: its file name, or a line of its torrent name, starts with
/// Parse runtime text ("112 min", "1h 52m", "173") into seconds.
pub fn parse_runtime_seconds(raw: &str) -> Option<f64> {
    let t = raw.trim().to_ascii_lowercase();
    if t.is_empty() { return None; }
    if let Some((h_part, rest)) = t.split_once('h') {
        let hours: f64 = h_part.trim().parse().ok()?;
        let mins: f64 = rest.chars().take_while(|c| c.is_ascii_digit() || c.is_whitespace()).collect::<String>().trim().parse().unwrap_or(0.0);
        let secs = hours * 3600.0 + mins * 60.0;
        return (60.0..=12.0 * 3600.0).contains(&secs).then_some(secs);
    }
    let digits: String = t.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    let mins: f64 = digits.parse().ok()?;
    let secs = mins * 60.0;
    (60.0..=12.0 * 3600.0).contains(&secs).then_some(secs)
}

/// Map an ISO 639-1/2 code or English language name ("ja", "jpn", "Japanese", "Italian") to a
/// `crate::addons::LANGUAGES` key when one matches.
pub fn language_key_of(raw: &str) -> Option<&'static str> {
    let first = raw.split([',', '/', ';']).next()?.trim();
    if first.is_empty() { return None; }
    let lower = first.to_ascii_lowercase();
    crate::addons::LANGUAGES.iter()
        .find(|l| l.key == lower || l.label.eq_ignore_ascii_case(first) || track_in_language(&lower, l.key))
        .map(|l| l.key)
}

/// True when `s` names ANY of `titles` (display title, original title, or localized alias).
/// Secondary/alternate titles (`titles[1..]`) on movies with a known `year` require an explicit
/// matching year (±1) right after the title so a working title or short alias (e.g. "Judy" for
/// Digger 2026) can never match a year-less release of an older film (`Judy.BDRemux.1080p.pk.mkv`).
pub fn names_title_any(s: &Source, titles: &[String], year: Option<i32>) -> bool {
    if titles.is_empty() { return true; }
    titles.iter().enumerate()
        .filter(|(_, t)| !t.trim().is_empty())
        .any(|(idx, t)| names_title_inner(s, t, year, idx > 0 && year.is_some()))
}

/// Build an `AutoplayContext` synchronously from everything already cached in memory for `media`
/// (display title + original/localized titles, expected runtime, original language, per-title
/// remembered audio preference, and requested episode number) with zero extra network latency.
pub fn autoplay_context_for_media(media: &Media, video_id: &str, pack_episodes: Option<usize>) -> AutoplayContext {
    let meta = cached_title_meta(media);
    let art = cached_title_artwork(media).or_else(|| cached_artwork(media));
    let mut titles = vec![media.name.clone()];
    let mut push_title = |t: &str| {
        let t = t.trim();
        if !t.is_empty() && !titles.iter().any(|x| x.eq_ignore_ascii_case(t)) { titles.push(t.to_owned()); }
    };
    for field in ["original_title", "original_name"] { push_title(&s(&media.raw, field)); }
    if let Some(m) = &meta { for t in &m.alt_titles { push_title(t); } }
    if let Some(a) = &art { for t in &a.alt_titles { push_title(t); } }

    // Always prioritize free metadata (TitleMeta from BetterPosters/Cinemeta, Media.raw, TVmaze/Wikidata)
    // before falling back to optional TMDB fields.
    let expected_seconds = meta.as_ref().and_then(|m| parse_runtime_seconds(&m.runtime))
        .or_else(|| parse_runtime_seconds(&s(&media.raw, "runtime")))
        .or_else(|| (media.duration_ms >= 60_000).then(|| media.duration_ms as f64 / 1000.0))
        .or_else(|| art.as_ref().and_then(|a| a.runtime_secs));

    let original_language = meta.as_ref().and_then(|m| language_key_of(&m.language))
        .or_else(|| language_key_of(&s(&media.raw, "original_language")))
        .or_else(|| language_key_of(&s(&media.raw, "language")))
        .or_else(|| art.as_ref().and_then(|a| a.language.as_deref()).and_then(language_key_of))
        .or_else(|| art.as_ref().and_then(|a| a.original_language.as_deref()).and_then(language_key_of))
        .map(str::to_owned);

    let preferred_language = load_track_pref(&media.key())
        .and_then(|p| p.audio_lang)
        .and_then(|code| language_key_of(&code).map(str::to_owned))
        .or_else(stream_language);

    let episode_code = parse_video_episode(video_id);

    AutoplayContext {
        titles,
        year: (media.kind == "movie").then(|| media.first_year()).flatten(),
        pack_episodes,
        expected_seconds,
        preferred_language,
        original_language,
        episode_code,
        strict_title: true,
    }
}

/// Extract `(season, episode)` from a Stremio series video id such as `"tt0903747:1:2"`.
pub fn parse_video_episode(video_id: &str) -> Option<(i64, i64)> {
    let parts: Vec<&str> = video_id.split(':').collect();
    if parts.len() >= 3 {
        let s: i64 = parts[parts.len() - 2].parse().ok()?;
        let e: i64 = parts[parts.len() - 1].parse().ok()?;
        if s >= 0 && e > 0 { return Some((s, e)); }
    }
    None
}

/// Whether a release names this title: its file name, or a line of its torrent name, starts with
/// the title's words (leading "[site]" and "www.site - " tags and a first "The" aside) and has
/// more after them; for a film, a year right after the title is within one of `year`. A line
/// that is only the title — or only `Title (Year)` with no release tags — says nothing (add-ons
/// often print the requested title and year as a header line). A translated title may come first:
/// "Kód 3 - Code 3 (2024)", "Код 3 / Code 3 (2025)".
#[cfg(test)]
pub fn names_title(s: &Source, title: &str, year: Option<i32>) -> bool {
    names_title_inner(s, title, year, false)
}

fn names_title_inner(s: &Source, title: &str, year: Option<i32>, require_year: bool) -> bool {
    fn words(text: &str) -> Vec<String> {
        let mut t = text.trim();
        loop {
            if let Some(rest) = t.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(_, r)| r.trim_start()) { t = rest; continue; }
            if t.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www.")) { if let Some((_, rest)) = t.split_once(" - ") { t = rest.trim_start(); continue; } }
            break;
        }
        // "Schindler's" and "Schindlers", "Fast & Furious" and "Fast and Furious" are the same words.
        let t = t.replace(['\'', '\u{2019}', '`'], "").replace('&', " and ");
        let mut w = search_words(&t);
        if w.first().is_some_and(|f| f == "the") { w.remove(0); }
        w
    }
    fn line_matches(line: &str, wanted: &[String], year: Option<i32>, require_year: bool) -> bool {
        let w = words(line);
        if w.len() <= wanted.len() || w[..wanted.len()] != wanted[..] { return false; }
        // What follows the title must not make it another one ("The Dark Knight Rises"): a year
        // (the film's, give or take one, with release info after it so a bare "Title (2026)"
        // add-on header line is rejected), a season or episode, or a release tag.
        let next = w[wanted.len()].as_str();
        match next.parse::<i32>().ok().filter(|y| (1900..2100).contains(y)) {
            Some(got) => w.len() > wanted.len() + 1 && year.is_none_or(|want| (want - got).abs() <= 1),
            None => !require_year && release_word().is_match(next),
        }
    }
    fn segment_matches(line: &str, wanted: &[String], year: Option<i32>, require_year: bool) -> bool {
        std::iter::once(line)
            .chain([" - ", " / "].into_iter().filter_map(|sep| line.split_once(sep).map(|(_, rest)| rest)))
            .any(|seg| line_matches(seg, wanted, year, require_year))
    }
    let wanted = words(title);
    if wanted.is_empty() { return true; }
    let file = s.filename.rsplit(['/', '\\']).next().unwrap_or("");
    if !file.is_empty() && segment_matches(file, &wanted, year, require_year) {
        return true;
    }
    // For a movie with an explicit filename, never let a description header rescue a file that
    // plainly names a different year or a completely different movie (e.g. `Judy.BDRemux.1080p.pk.mkv`
    // under a `Digger (2026)` add-on header). Bilingual releases whose description line names both
    // the file's leading title word and the wanted title (e.g. "Kód 3 - Code 3 (2024)") still pass.
    let mut file_title_lead: Option<String> = None;
    if !file.is_empty() {
        let fw = words(file);
        if let Some(want) = year {
            let file_years: Vec<i32> = fw.iter().skip(1)
                .filter_map(|t| t.parse::<i32>().ok().filter(|y| (1900..2100).contains(y)))
                .collect();
            if !file_years.is_empty() && !file_years.iter().any(|&y| (want - y).abs() <= 1) {
                return false;
            }
            // Detect a named movie file (non-generic leading word followed by a year or release tag).
            let first = fw.first().map(String::as_str).unwrap_or("");
            let generic = first.is_empty()
                || first.chars().all(|c| c.is_ascii_digit())
                || matches!(first, "video" | "movie" | "film" | "stream" | "index" | "master" | "playlist" | "play" | "media")
                || release_word().is_match(first);
            let looks_like_release = !generic && fw.iter().skip(1).any(|t| {
                t.parse::<i32>().ok().is_some_and(|y| (1900..2100).contains(&y)) || release_word().is_match(t)
            });
            if looks_like_release {
                file_title_lead = Some(first.to_owned());
            }
        }
    }
    s.description.lines().any(|line| {
        if !segment_matches(line, &wanted, year, require_year) { return false; }
        match &file_title_lead {
            Some(lead) => words(line).iter().any(|w| w == lead),
            None => true,
        }
    })
}
static_re!(release_word, r"^(?:s\d{1,2}(?:e\d{1,3})?|e\d{1,3}|\d{1,2}x\d{2,3}|season|series|complete|\d{3,4}p|4k|8k|uhd|fhd|hd|sdr|hdr\d*|hdr10plus|dv|dovi|bluray|blu|bdrip|brrip|bdremux|remux|web|webdl|webrip|dl|hdtv|imax|extended|unrated|uncut|remastered|theatrical|directors|criterion|proper|repack|hybrid|multi|dual|x264|x265|h264|h265|hevc|avc|av1|10bit|ita|eng|fre|french|ger|german|spa|spanish|latino|rus|jpn|kor|us|uk)$");

fn search_words(text: &str) -> Vec<String> {
    let folded: String = text.to_lowercase().nfd().filter(|c| !is_combining_mark(*c)).collect();
    folded.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// 0 exact, 1 starts with it, 2 every word starts a title word, 3 contains it,
/// 4 contains every word, 5 matched only by add-on metadata (same as SearchPanel.kt).
pub fn search_rank(query: &str, name: &str) -> u8 {
    let q = search_words(query);
    let n = search_words(name);
    if q.is_empty() { return 5; }
    let qt = q.join(" ");
    let nt = n.join(" ");
    if nt == qt { 0 }
    else if nt.starts_with(&qt) { 1 }
    else if q.iter().all(|w| n.iter().any(|x| x.starts_with(w.as_str()))) { 2 }
    else if nt.contains(&qt) { 3 }
    else if q.iter().all(|w| nt.contains(w.as_str())) { 4 }
    else { 5 }
}

/// Best title match first; add-on order is kept within a rank (stable sort).
pub fn rank_search_results(query: &str, mut results: Vec<(String, Media)>) -> Vec<(String, Media)> {
    results.sort_by_key(|(_, m)| search_rank(query, &m.name));
    results
}

// ---------------------------------------------------------------- history of viewing

/// One title you watched or started, and when you last did.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen { pub media: Media, pub at: i64, pub episode: Option<String> }

/// Everything watched or started, latest first (the core's history, with when you last watched).
pub async fn viewing_history() -> Result<Vec<Seen>> { recent_viewing(600).await }

/// The latest `limit` entries of `viewing_history` (fewer when some are add-on bookkeeping rows).
pub async fn recent_viewing(limit: usize) -> Result<Vec<Seen>> {
    let v = exec(json!({"op":"history","limit":limit})).await?;
    let mut out = Vec::new();
    for item in v["items"].as_array().into_iter().flatten() {
        let Some(media) = Media::from_json(item, "") else { continue };
        if media.kind == "other" { continue; } // add-on bookkeeping rows (e.g. a debrid "Downloads" folder)
        let episode = media.last_video_id.as_deref().and_then(episode_label).filter(|_| media.kind != "movie");
        out.push(Seen { at: item["seen"].as_i64().unwrap_or(0), media, episode });
    }
    Ok(out)
}

/// A small random number (xorshift seeded from the clock): for shuffling picks, not security.
pub fn rand_u64() -> u64 {
    static STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut x = STATE.load(std::sync::atomic::Ordering::Relaxed);
    if x == 0 { x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E37_79B9_7F4A_7C15) | 1; }
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    STATE.store(x, std::sync::atomic::Ordering::Relaxed);
    x
}

/// Unix seconds now.
pub fn now_secs() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }

/// Calendar date (UTC) from unix seconds: (year, month 1-12, day, weekday 0 = Monday).
pub fn civil(secs: i64) -> (i64, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let weekday = ((days + 3).rem_euclid(7)) as u32; // 1970-01-01 was a Thursday
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d, weekday)
}

pub fn month_name(m: u32) -> &'static str {
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][(m.clamp(1, 12) - 1) as usize]
}

/// "Today", "Yesterday", a weekday within the week, else "12 Oct".
pub fn when_label(at: i64, now: i64) -> String {
    let (day_at, day_now) = (at.div_euclid(86_400), now.div_euclid(86_400));
    match day_now - day_at {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][civil(at).3 as usize].into(),
        _ => { let (_, m, d, _) = civil(at); format!("{d} {}", &month_name(m)[..3]) }
    }
}

/// IMDb (or add-on) ids of titles you finished, whichever add-on they were watched from.
/// Films count once finished; a series counts only when every released regular episode is
/// watched. Unfinished series stay available with Resume. Also match by title/year.
pub async fn watched_ids() -> HashSet<String> {
    let history = viewing_history().await.unwrap_or_default();
    // Bound concurrency so large watch histories don't stampede SQLite and metadata add-ons.
    let finished = on_runtime(async move {
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut checks = tokio::task::JoinSet::new();
        for item in history {
            let gate = gate.clone();
            checks.spawn(async move {
                let _permit = gate.acquire_owned().await;
                let m = item.media;
                finished_title(&m).await.then_some(m)
            });
        }
        let mut finished = Vec::new();
        while let Some(done) = checks.join_next().await { if let Ok(Some(m)) = done { finished.push(m); } }
        finished
    }).await.unwrap_or_default();
    finished.into_iter().flat_map(|m| [Some(m.id.clone()), imdb_of(&m), Some(crate::picks_rules::title_key(&m))]).flatten().collect()
}

/// A film you watched, or a series with every released regular episode watched.
async fn finished_title(m: &Media) -> bool {
    if m.kind == "movie" { return m.watched; }
    let seen = watched_episodes(m).await;
    if seen.is_empty() { return m.watched; }
    // Metadata is normally cached by the prior playback/title page. If unavailable, keep
    // the series visible rather than inferring that one finished episode completes it.
    let mut episodes = episodes_from_meta(&m.raw);
    if episodes.is_empty() { episodes = title_meta(m).await.map(|m| m.episodes).unwrap_or_default(); }
    crate::picks_rules::series_complete(&episodes, &seen, now_secs()) || (episodes.is_empty() && m.watched)
}

// ---------------------------------------------------------------- accent

/// The accent a title lends the interface, as CSS hex; None keeps Bone.
pub async fn accent(url: String) -> Option<String> {
    if url.is_empty() { return None; }
    if let Some(hit) = backend().accents.lock().ok()?.get(&url).cloned() { return hit; }
    let key = url.clone();
    // 48×48 pixels are all that is read: TMDB's small size is a tenth of the download.
    let url = small_tmdb_image(&url).unwrap_or(url);
    // Distinguish a decoded greyscale image (Ok(None) -> cacheable) from a transient network error (Err -> retry later).
    let computed: std::result::Result<Option<String>, ()> = on_runtime(async move {
        const LIMIT: usize = 12_000_000;
        let mut response = backend().http.get(&url).send().await.map_err(|_| ())?.error_for_status().map_err(|_| ())?;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) { return Ok(None); }
        // Read at most LIMIT bytes whatever the server says, and decode within a memory budget.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes.len() + chunk.len() > LIMIT { return Ok(None); }
            bytes.extend_from_slice(&chunk);
        }
        // Offload CPU-intensive image decoding & thumbnailing off the Tokio async I/O workers.
        tokio::task::spawn_blocking(move || {
            let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(256 * 1024 * 1024);
            limits.max_image_width = Some(12_000);
            limits.max_image_height = Some(12_000);
            reader.limits(limits);
            let img = reader.decode().ok()?.thumbnail(48, 48).to_rgba8();
            let pixels: Vec<[u8; 4]> = img.pixels().map(|p| p.0).collect();
            Some(artwork_accent(&pixels).map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}")))
        }).await.ok().flatten().ok_or(())
    }).await.ok().unwrap_or(Err(()));
    if let Ok(val) = computed {
        if let Ok(mut cache) = backend().accents.lock() { cache_insert(&mut cache, 2000, key, val.clone()); }
        val
    } else {
        None
    }
}

/// The same TMDB image at 300 px wide (TMDB serves every image at that size), for reading colours.
fn small_tmdb_image(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://image.tmdb.org/t/p/")?;
    let (size, path) = rest.split_once('/')?;
    (size != "w300").then(|| format!("https://image.tmdb.org/t/p/w300/{path}"))
}

/// Port of ArtworkAccent.kt: the most vivid well-lit colour, lifted until black
/// text on it (and it on black) reaches 4.5:1. None for greyscale artwork.
pub fn artwork_accent(pixels: &[[u8; 4]]) -> Option<(u8, u8, u8)> {
    let mut best = None;
    let mut best_score = 0.0;
    for p in pixels {
        if p[3] < 128 { continue; }
        let (r, g, b) = (p[0] as f64 / 255.0, p[1] as f64 / 255.0, p[2] as f64 / 255.0);
        let high = r.max(g).max(b);
        let low = r.min(g).min(b);
        if high < 0.25 { continue; }
        let saturation = (high - low) / high;
        let score = saturation * saturation * high;
        if score > best_score { best_score = score; best = Some((r, g, b)); }
    }
    let (mut r, mut g, mut b) = best.filter(|_| best_score >= 0.08)?;
    for _ in 0..20 {
        if luminance(r, g, b) >= 0.175 { break; }
        r += (1.0 - r) * 0.12;
        g += (1.0 - g) * 0.12;
        b += (1.0 - b) * 0.12;
    }
    Some(((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8))
}

fn luminance(r: f64, g: f64, b: f64) -> f64 {
    let c = |x: f64| if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b)
}

#[cfg(test)]
mod review_tests {
    #[test]
    fn accents_read_tmdb_images_small() {
        assert_eq!(super::small_tmdb_image("https://image.tmdb.org/t/p/w1280/abc.jpg").as_deref(), Some("https://image.tmdb.org/t/p/w300/abc.jpg"));
        assert_eq!(super::small_tmdb_image("https://image.tmdb.org/t/p/original/abc.jpg").as_deref(), Some("https://image.tmdb.org/t/p/w300/abc.jpg"));
        assert_eq!(super::small_tmdb_image("https://image.tmdb.org/t/p/w300/abc.jpg"), None);
        assert_eq!(super::small_tmdb_image("https://images.metahub.space/background/medium/tt1/img"), None);
    }

    use super::*;

    #[test]
    fn track_languages_match_the_setting() {
        assert!(track_in_language("ita", "italian"));
        assert!(track_in_language("it-IT", "italian"));
        assert!(!track_in_language("eng", "italian"));
        assert!(!track_in_language("", "italian"));
        assert!(track_in_language("es-419", "latino"));
        assert!(!track_in_language("es-ES", "latino"));
        assert!(track_in_language("pt-BR", "portuguese"));
        assert_eq!(track_codes("italian").join(","), "it,ita");
        // Subtitle add-on codes.
        assert!(track_in_language("pob", "portuguese"));
        assert!(track_in_language("spl", "latino"));
        assert!(track_in_language("nb", "norwegian"));
    }

    #[test]
    fn skip_times_come_from_introdb_and_aniskip() {
        use crate::engine::{Segment, SegmentKind::*};
        assert_eq!(skip_lookup("tt0903747:1:2"), Some(SkipLookup::IntroDb { imdb: "tt0903747".into(), season: 1, episode: 2 }));
        assert_eq!(skip_lookup("kitsu:1376:5"), Some(SkipLookup::Kitsu { id: 1376, episode: 5 }));
        assert_eq!(skip_lookup("mal:5114:3"), Some(SkipLookup::Mal { id: 5114, episode: 3 }));
        assert_eq!(skip_lookup("tt0903747"), None);
        assert_eq!(skip_lookup("kitsu:1376"), None);
        assert_eq!(skip_lookup("tt:1:2"), None);
        // IntroDB's answer, as the service sends it.
        let intro = json!({"imdb_id":"tt0903747","intro":{"start_sec":60,"end_sec":95,"start_ms":60000,"end_ms":95500},"recap":null,
            "outro":{"start_sec":3431,"end_sec":3500,"start_ms":3431000,"end_ms":3500000},"no_intro":false});
        assert_eq!(introdb_segments(&intro, 3480.0), vec![Segment { kind: Intro, start: 60.0, end: 95.5 }]);
        assert_eq!(introdb_segments(&intro, 3502.0).len(), 2);
        assert_eq!(introdb_segments(&json!({"outro":{"start_ms":3000000,"end_ms":null}}), 3100.0), vec![Segment { kind: Credits, start: 3000.0, end: 3100.0 }]);
        assert!(introdb_segments(&Value::Null, 100.0).is_empty());
        // AniSkip's, and a Kitsu mapping.
        let ani = json!({"found":true,"results":[{"interval":{"startTime":1349.903,"endTime":1474},"skipType":"ed"},
            {"interval":{"startTime":58.642,"endTime":148.642},"skipType":"op"},{"interval":{"startTime":1,"endTime":2},"skipType":"recap"}]});
        assert_eq!(aniskip_segments(&ani, 1474.6), vec![Segment { kind: Intro, start: 58.642, end: 148.642 }, Segment { kind: Credits, start: 1349.903, end: 1474.0 }]);
        assert_eq!(mal_of(&json!({"data":[{"attributes":{"externalSite":"myanimelist/anime","externalId":"1535"}}]})), Some(1535));
        assert_eq!(mal_of(&json!({"data":[]})), None);
    }
    #[test]
    fn episode_labels_for_series_and_kitsu() {
        assert_eq!(episode_label("tt123:2:5").as_deref(), Some("S2 E5"));
        assert_eq!(episode_label("kitsu:12345:5").as_deref(), Some("E5"));
        assert_eq!(episode_label("tt123"), None);
        assert_eq!(episode_label("custom:abc:x"), None);
        assert_eq!(episode_label("mal:5114:3"), None);
        assert_eq!(episode_label("tmdb:123:1:2").as_deref(), Some("S1 E2"));
    }

    #[test]
    fn catalogs_needing_an_extra_are_not_shelves() {
        assert!(catalog_needs_extra(&json!({"extra":[{"name":"genre","isRequired":true}]})));
        assert!(catalog_needs_extra(&json!({"extraRequired":["search"]})));
        assert!(!catalog_needs_extra(&json!({"extra":[{"name":"skip"}],"extraRequired":[]})));
    }

    #[test]
    fn audio_language_names_its_subtitles() {
        assert_eq!(subtitle_language_of("latino"), "Spanish (Latin America)");
        assert_eq!(subtitle_language_of("italian"), "Italian");
        assert_eq!(subtitle_language_of("nope"), "");
    }
}

#[cfg(test)]
mod subtitle_tests {
    use super::*;
    fn sub(id: &str, code: &str) -> OnlineSubtitle {
        subtitle_of(&json!({"id": id, "url": format!("https://subs.example/{id}"), "lang": code})).unwrap()
    }

    #[test]
    fn preferred_language_then_english_then_by_name() {
        let list = vec![sub("1", "spa"), sub("2", "eng"), sub("3", "ita"), sub("4", "xx"), sub("5", "ita"), sub("6", "fre"), sub("2", "eng")];
        let ranked = rank_subtitles(list, Some("Italian"));
        let ids: Vec<&str> = ranked.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["3", "5", "2", "6", "1", "4"]);
        assert_eq!(ranked[5].language, "XX");
        assert_eq!(rank_subtitles(vec![sub("1", "spa"), sub("2", "eng")], None)[0].language, "English");
    }

    #[test]
    fn search_by_language_name_or_code() {
        let br = sub("1", "pob");
        assert_eq!(br.language, "Portuguese (Brazil)");
        for q in ["", "port", "Brazil", "pob", "  PORTUGUESE "] { assert!(subtitle_matches(&br, q), "{q}"); }
        for q in ["ita", "razil", "po b"] { assert!(!subtitle_matches(&br, q), "{q}"); }
        assert!(subtitle_matches(&sub("2", "ita"), "it"));
    }

    #[test]
    fn keeps_the_release_name_and_skips_files_without_a_link() {
        let s = subtitle_of(&json!({"id": "9", "url": "https://subs.example/9", "lang": "eng", "subtitleFileName": "Film.2019.1080p.srt"})).unwrap();
        assert_eq!(s.release.as_deref(), Some("Film.2019.1080p.srt"));
        assert!(subtitle_of(&json!({"id": "9", "url": "file:///etc/passwd", "lang": "eng"})).is_none());
        assert!(subtitle_of(&json!({"id": "9", "lang": "eng"})).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyless_elfhosted_artwork_and_wikidata_collections_parse() {
        let meta = json!({
            "background": "https://image.tmdb.org/t/p/original/bg.jpg",
            "logo": "https://image.tmdb.org/t/p/original/logo.png",
            "poster": "https://image.tmdb.org/t/p/w500/p.jpg",
            "runtime": "2h47min",
            "ageRating": "PG-13",
            "genres": ["Science Fiction", "Adventure"],
            "director": ["Denis Villeneuve"],
            "writer": ["Jon Spaihts"],
            "trailers": [{"source": "U2Qp5pL3ovA", "type": "Trailer"}],
            "app_extras": {
                "cast": [
                    {"name": "Timothée Chalamet", "character": "Paul Atreides", "photo": "https://image.tmdb.org/t/p/w276_and_h350_face/tc.jpg"},
                    {"name": "Zendaya", "character": "Chani", "photo": ""}
                ]
            }
        });
        let a = parse_elfhosted_artwork(&meta);
        assert_eq!(a.backdrop.as_deref(), Some("https://image.tmdb.org/t/p/w1280/bg.jpg"));
        assert_eq!(a.logo.as_deref(), Some("https://image.tmdb.org/t/p/w500/logo.png"));
        assert_eq!(a.certificate.as_deref(), Some("PG-13"));
        assert_eq!(a.runtime_secs, Some(167.0 * 60.0));
        assert_eq!(a.cast.len(), 2);
        assert_eq!(a.cast[0].role, "Paul Atreides");
        assert_eq!(a.cast[0].photo.as_deref(), Some("https://image.tmdb.org/t/p/w276_and_h350_face/tc.jpg"));
        assert_eq!(a.cast[1].photo, None);
        assert_eq!(a.crew[0].0, "Directed by");
        assert_eq!(a.crew[0].1[0].name, "Denis Villeneuve");
        assert_eq!(a.trailers, ["U2Qp5pL3ovA"]);

        let rows = json!([
            {"colLabel": {"value": "Dune"}, "imdb": {"value": "tt15239678"}, "partLabel": {"value": "Dune: Part Two"}, "date": {"value": "2024-03-01T00:00:00Z"}},
            {"colLabel": {"value": "Dune"}, "imdb": {"value": "tt1160419"}, "partLabel": {"value": "Dune"}, "date": {"value": "2021-10-22T00:00:00Z"}},
            {"colLabel": {"value": "Dune"}, "imdb": {"value": "tt9999999"}, "partLabel": {"value": "Dune: Messiah"}, "date": {"value": "2027-12-18T00:00:00Z"}}
        ]);
        let shelf = parse_wikidata_collection(&rows, "2026-11-25").unwrap();
        assert_eq!(shelf.name, "Dune");
        assert_eq!(shelf.parts.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["tt1160419", "tt15239678"]);

        let (bio, photo, qid) = parse_wiki_summary(&json!({
            "type": "standard",
            "extract": "Denis Villeneuve is a Canadian filmmaker.",
            "thumbnail": {"source": "https://upload.wikimedia.org/dv.jpg"},
            "wikibase_item": "Q3022982"
        }));
        assert_eq!(bio.as_deref(), Some("Denis Villeneuve is a Canadian filmmaker."));
        assert_eq!(photo.as_deref(), Some("https://upload.wikimedia.org/dv.jpg"));
        assert_eq!(qid.as_deref(), Some("Q3022982"));
    }

    #[test]
    fn better_posters_charts_stay_cinemeta_titles() {
        let v = json!({"metas": [
            {"id": "tt33764258", "type": "movie", "name": "The Odyssey", "poster": "https://btttr.cc/poster/movie/tt33764258/auto~gr.png", "runtime": "173 min"},
            {"id": "tmdb:12", "type": "movie", "name": "TMDB only"},
            {"id": "tt33764258", "type": "movie", "name": "The Odyssey"},
        ]});
        let titles = bp_titles(&v, CINEMETA);
        assert_eq!(titles.len(), 1, "IMDb ids only, once each");
        assert_eq!(titles[0].addon_url, CINEMETA, "keyed like the same title from Cinemeta");
        assert_eq!(titles[0].raw["runtime"], "173 min");
    }

    #[test]
    fn better_posters_metadata_over_the_addons() {
        let addon = json!({"runtime": "1 min", "description": "Cinemeta's", "cast": ["A"], "trailers": [{"source": "x", "type": "Trailer"}],
            "videos": [{"id": "tt1:1:1", "season": 1, "episode": 1, "name": "Pilot", "thumbnail": ""}, {"id": "tt1:1:2", "season": 1, "episode": 2, "name": "Two"}]});
        let bp = json!({"runtime": "44 min", "description": "", "imdbRating": "8.2", "trailers": [{"source": "y", "type": "Trailer"}],
            "videos": [{"id": "tt1:1:1", "title": "Pilot (BP)", "thumbnail": "https://image.tmdb.org/s.jpg"}, {"id": "tt1:9:9", "title": "Not in the add-on"}]});
        let m = merge_meta(addon.clone(), Some(&bp));
        assert_eq!(m["runtime"], "44 min");
        assert_eq!(m["imdbRating"], "8.2");
        assert_eq!(m["description"], "Cinemeta's", "empty BetterPosters fields never erase the add-on's");
        assert_eq!(m["cast"], json!(["A"]));
        assert_eq!(m["trailers"].as_array().map(Vec::len), Some(2));
        assert_eq!(m["trailers"][0]["source"], "y");
        let videos = m["videos"].as_array().unwrap();
        assert_eq!(videos.len(), 2, "the add-on's episode list stays");
        assert_eq!(videos[0]["title"], "Pilot (BP)");
        assert_eq!(videos[0]["thumbnail"], "https://image.tmdb.org/s.jpg");
        assert_eq!(videos[1]["name"], "Two");
        // Nothing from BetterPosters: the add-on's, unchanged. An add-on without episodes takes BetterPosters'.
        assert_eq!(merge_meta(addon.clone(), None), addon);
        assert_eq!(merge_meta(json!({"name": "X"}), Some(&bp))["videos"].as_array().map(Vec::len), Some(2));
    }

    fn media(name: &str, kind: &str, year: &str, genre: &str) -> Media {
        Media::from_json(&json!({"id":name,"name":name,"type":kind,"releaseInfo":year,"genres":[genre]}), "a").unwrap()
    }

    #[test]
    fn tvmaze_show_type_keeps_animation_out_of_live_action() {
        let source=json!({"type":"Animation","genres":["Comedy","Family"]});
        let genres=tvmaze_genres(&source);
        let m=Media::from_json(&json!({"id":"fixture-simpsons","name":"The Simpsons","type":"series","genres":genres}),"fixture").unwrap();
        assert!(!crate::picks_rules::style_matches(&m,Style::Live));
        assert!(crate::picks_rules::style_matches(&m,Style::Animation));
        assert!(!tvmaze_genres(&json!({"type":"Scripted","genres":["Drama"]})).contains(&"Animation".into()));
    }
    #[test]
    fn search_ranking_matches_the_android_app() {
        let names = ["Dune", "Godzilla Minus One", "One Piece Film: Red", "One Piece", "The Big Red One"];
        let results = names.iter().map(|n| ("Cinemeta".to_owned(), media(n, "movie", "2020", "Drama"))).collect();
        let ranked: Vec<_> = rank_search_results("one piece", results).into_iter().map(|(_, m)| m.name).collect();
        assert_eq!(ranked, ["One Piece", "One Piece Film: Red", "Dune", "Godzilla Minus One", "The Big Red One"]);
        assert_eq!(search_rank("  ONE piece ", "One Piece"), 0);
        assert_eq!(search_rank("piece one", "One Piece"), 2);
        assert_eq!(search_rank("amelie", "Amélie"), 0);
        assert_eq!(search_rank("one", "Dune"), 5);
    }

    #[test]
    fn recent_queries_are_trimmed_deduplicated_and_bounded() {
        assert_eq!(recent_queries(" Dune ", &["Alien".into(), "dune".into()]), ["Dune", "Alien"]);
        assert_eq!(recent_queries("a", &["Alien".into()]), ["Alien"]);
        assert_eq!(recent_queries("x1", &(0..20).map(|i| format!("q{i}")).collect::<Vec<_>>()).len(), 12);
    }

    #[test]
    fn sources_read_quality_size_and_kind_without_estimating() {
        let st = source_of("A", &json!({"name":"RD 4K","title":"Film.2160p.HDR10+.mkv\n💾 21.4 GB","url":"https://x.invalid/f"}));
        assert_eq!((st.quality, st.hdr, st.kind, st.size.as_deref()), (Some("4K"), Some("HDR10+"), SourceKind::Direct, Some("21.4 GB")));
        let t = source_of("B", &json!({"name":"P2P","title":"Film 1080p","infoHash":"abc"}));
        assert_eq!((t.kind, t.size, t.url.as_str()), (SourceKind::Torrent, None, ""));
        let b = source_of("C", &json!({"url":"https://x.invalid/g","behaviorHints":{"videoSize":1610612736u64,"filename":"g.720p.mp4"}}));
        assert_eq!((b.size.as_deref(), b.quality), (Some("1.5 GiB"), Some("720p")));
    }

    #[test]
    fn season_packs_are_split_even_when_the_file_is_named_for_the_episode() {
        // AIOStreams: the torrent name on the first line, the episode's file below, the whole
        // season's size declared as the file's.
        let pack = source_of("A", &json!({"url":"https://x.invalid/p","behaviorHints":{"filename":"Show.S01E01.1080p.BluRay.REMUX.AVC-GRP.mkv","videoSize":119243487977u64},
            "description":"Show.S01.1080p.BluRay.REMUX.AVC-GRP\nShow.S01E01.1080p.BluRay.REMUX.AVC-GRP.mkv\n💾111.05 GiB"}));
        assert!(pack.pack);
        assert_eq!(episode_size(&pack, Some(10)), Some(11_924_348_797));
        // A declared size unlike the pack label is the episode file's own.
        let own = source_of("A", &json!({"url":"https://x.invalid/q","behaviorHints":{"videoSize":9_000_000_000u64},
            "title":"Show.S01.2160p.WEB-DL\nShow.S01E01.mkv\n💾 90 GB"}));
        assert_eq!(episode_size(&own, Some(10)), Some(9_000_000_000));
        let single = source_of("A", &json!({"url":"https://x.invalid/e","title":"Show.S01E01.2160p.WEB-DL\n💾 9 GB"}));
        assert!(!single.pack);
        // HDR beats a larger SDR pack, comparing pack sizes per episode within each HDR tier.
        let hdr = source_of("A", &json!({"url":"https://x.invalid/h","behaviorHints":{"filename":"Show.S01E01.2160p.WEB.DL.DV.HDR10P.H.265-GRP.mkv","videoSize":100813874330u64},
            "description":"Show.S01.2160p.WEB.DL.DV.HDR10P.H.265-GRP\nShow.S01E01.2160p.WEB.DL.DV.HDR10P.H.265-GRP.mkv\n💾93.89 GiB","name":"[RD+] MediaFusion 2160p\nHDR10+ | DV"}));
        assert_eq!(play_order_for(&[pack.clone(), hdr.clone()], Some(10), None)[0].url, hdr.url);
    }

    #[test]
    fn preferred_language_plays_first() {
        let big = source_of("A", &json!({"url":"https://x.invalid/a","title":"Film 2160p REMUX\n💾 60 GB\n🇬🇧"}));
        let ita = source_of("A", &json!({"url":"https://x.invalid/b","title":"Film 1080p WEB-DL ITA ENG\n💾 4 GB\n🇬🇧 / 🇮🇹"}));
        let list = vec![big.clone(), ita.clone()];
        assert_eq!(play_order_for(&list, None, None)[0].url, big.url);
        assert_eq!(play_order_for(&list, None, Some("italian"))[0].url, ita.url);
        // English: untagged sources count as English; the ITA ENG release has English too.
        assert_eq!(play_order_for(&list, None, Some("english"))[0].url, big.url);
        // An Italian-only 4K HDR10+ release goes after a smaller English one when you chose English.
        let ita_only = source_of("A", &json!({"url":"https://x.invalid/c","title":"Film 2160p WEB-DL HDR10+ ITA\n💾 30 GB\n🇮🇹"}));
        let eng = source_of("A", &json!({"url":"https://x.invalid/d","title":"Film 1080p WEB-DL\n💾 8 GB"}));
        let multi = source_of("A", &json!({"url":"https://x.invalid/e","title":"Film 2160p WEB-DL HDR10+ MULTi\n💾 32 GB\n🇮🇹 / 🇫🇷"}));
        let order: Vec<String> = play_order_for(&[ita_only.clone(), eng.clone()], None, Some("english")).into_iter().map(|s| s.url).collect();
        assert_eq!(order, [eng.url.clone(), ita_only.url.clone()]);
        assert_eq!(play_order_for(&[ita_only.clone(), eng.clone()], None, None)[0].url, ita_only.url);
        // A multi-audio release isn't "Italian only": it keeps its place for English.
        assert_eq!(play_order_for(&[eng.clone(), multi.clone()], None, Some("english"))[0].url, multi.url);
        // For Italian: Italian first, then untagged, then other languages only.
        let fr = source_of("A", &json!({"url":"https://x.invalid/f","title":"Film 2160p REMUX FRENCH\n💾 70 GB\n🇫🇷"}));
        let order: Vec<String> = play_order_for(&[fr.clone(), eng.clone(), ita_only.clone()], None, Some("italian")).into_iter().map(|s| s.url).collect();
        assert_eq!(order, [ita_only.url.clone(), eng.url.clone(), fr.url.clone()]);
    }

    #[test]
    fn non_trial_and_preferred_language_streams_rank_first() {
        let hd = source_of("Addon A", &json!({"name":"1080p","title":"Film.1080p.WEBRip.mkv","url":"https://x.invalid/hd"}));
        let uhd = source_of("Addon B", &json!({"name":"4K","title":"Film 2160p REMUX\n💾 60 GB","url":"https://x.invalid/uhd"}));
        let trial = source_of("Addon B", &json!({"name":"1080p","title":"Original Audio [FREE TRIAL]","url":"https://x.invalid/t"}));
        let urls = |v: Vec<Source>| v.into_iter().map(|s| s.url).collect::<Vec<_>>();
        assert_eq!(urls(play_order_for(&[trial.clone(), hd.clone(), uhd.clone()], None, None)),
            ["https://x.invalid/uhd", "https://x.invalid/hd", "https://x.invalid/t"]);
        // The chosen language still comes first: Italian audio before untagged English.
        let ita = source_of("Addon B", &json!({"name":"1080p","title":"Film 1080P · 🇬🇧🇮🇹","url":"https://x.invalid/i"}));
        assert_eq!(urls(play_order_for(&[uhd, ita], None, Some("italian")))[0], "https://x.invalid/i");
    }
    #[test]
    fn a_refusal_for_too_many_requests_says_so() {
        assert!(no_source_message(Some("busy")).contains("Try again in a minute"));
        assert!(no_source_message(None).starts_with("No source responded"));
    }

    #[test]
    fn audio_language_is_english_unless_chosen() {
        assert_eq!(audio_language(&json!({})), "english");
        assert_eq!(audio_language(&json!({"stream_language": null})), "english");
        assert_eq!(audio_language(&json!({"stream_language": "klingon"})), "english");
        assert_eq!(audio_language(&json!({"stream_language": "italian"})), "italian");
        assert_eq!(Settings::default().stream_language.as_deref(), Some("english"));
    }

    /// Code 3 (2025) as Real-Debrid had it cached on 2026-11-22: Play chose the 1080p telesync, a
    /// copy filmed in a cinema, and passed over the 4K WEB-DL named with its Czech title first.
    #[test]
    fn cinema_copies_never_play_and_translated_titles_count() {
        let rows = [("[RD+] Torrentio\n4k", "Code 3 2025 2160p WEBRip\n👤 32 💾 4.45 GB ⚙️ ThePirateBay"),
            ("[RD+] Torrentio\n4k", "Kód 3 - Code 3 (2024)(CZ.EN)[2160p][WebDL] CZ-AAC2.0 EN-DDP5.1 SDR x265\n👤 20 💾 11.2 GB ⚙️ SkTorrent 🇬🇧 / 🇨🇿"),
            ("[RD+] Torrentio\n1080p", "Codigo 3 (Code 3) (2024) [Bluray 1080p][Esp]\n👤 2 💾 6.28 GB ⚙️ MejorTorrent 🇪🇸"),
            ("[RD+] Torrentio\nTeleSync", "Code 3 2025 1080p HDTS x264-RGB\n👤 9 💾 6.17 GB ⚙️ 1337x")];
        let mut list: Vec<Source> = rows.iter().enumerate()
            .map(|(i, (n, t))| source_of("Torrentio", &json!({"name": n, "title": t, "url": format!("https://x.invalid/{i}")}))).collect();
        for s in &mut list { s.off_title = !names_title(s, "Code 3", Some(2025)); }
        let order = play_order_for(&list, None, Some("english"));
        assert_eq!(order.iter().map(|s| s.size.as_deref().unwrap_or("")).collect::<Vec<_>>(), ["11.2 GB", "4.45 GB", "6.28 GB"],
            "the English 4K first; the telesync never");
        for camera in ["Film 2025 1080p HDTS x264", "Film.2025.2160p.HDCAM.x265", "Film 2025 1080p TELESYNC", "Film.2025.1080p.HD-TC.x264",
            "Film 2025 1080p CAMRip", "Film.2025.1080p.DVDSCR.x264", "Film 2025 2160p SCREENER"] {
            assert!(!playable(&source_of("A", &json!({"url":"https://x.invalid/c","title": camera}))), "{camera}");
        }
        // A bare TS is also the MPEG-TS container: a labelled release in one still plays.
        assert!(playable(&source_of("A", &json!({"url":"https://x.invalid/t","title":"Film.2025.2160p.UHD.BluRay.REMUX.HEVC.ts"}))));
    }

    #[test]
    fn releases_of_other_titles_go_last() {
        let src = |file: &str, description: &str| source_of("A", &json!({"url":"https://x.invalid/a","behaviorHints":{"filename":file},"title":description}));
        let other = src("Call.Me.by.Your.Name.2017.2160p.BluRay.REMUX.mkv", "Call.Me.by.Your.Name.2017.2160p.BluRay.REMUX.mkv\n💾 72 GB");
        assert!(!names_title(&other, "Your Name", Some(2016)));
        assert!(names_title(&src("Your Name 2016 2160p_DCPRip.mkv", ""), "Your Name", Some(2016)));
        assert!(names_title(&src("Your.Name.(2016).1080p.mkv", ""), "Your Name", Some(2016)));
        // The other film of that name, years apart.
        assert!(!names_title(&src("Dune.1984.2160p.REMUX.mkv", ""), "Dune", Some(2021)));
        assert!(names_title(&src("Dune.Part.Two.2024.2160p.mkv", ""), "Dune: Part Two", Some(2024)));
        // Series: the file names the show; the requested title alone on a line says nothing.
        assert!(names_title(&src("S02/The.Bear.S02E03.2160p.WEB.mkv", "The Bear"), "The Bear", None));
        assert!(!names_title(&src("Medvěd_S02E03_Pohár.mkv", "The Bear"), "The Bear", None));
        assert!(names_title(&src("", "www.1TamilMV.mx - The Bear (2023) S02 2160p\nEp 3"), "The Bear", None));
        assert!(names_title(&src("[Group] Amélie.2001.1080p.mkv", ""), "Amélie", Some(2001)));
        assert!(names_title(&src("Spider-Man.Across.the.Spider-Verse.2023.2160p.mkv", ""), "Spider-Man: Across the Spider-Verse", Some(2023)));
        // A longer title that starts with this one is another film or show.
        assert!(!names_title(&src("The.Dark.Knight.Rises.2012.IMAX.2160p.REMUX.mkv", ""), "The Dark Knight", Some(2008)));
        assert!(names_title(&src("The.Dark.Knight.2008.2160p.REMUX.mkv", ""), "The Dark Knight", Some(2008)));
        assert!(names_title(&src("The Dark Knight 2160p UHD BluRay.mkv", ""), "The Dark Knight", Some(2008)));
        assert!(!names_title(&src("Doctor.Who.Confidential.S01E01.mkv", ""), "Doctor Who", None));
        assert!(names_title(&src("Doctor.Who.2005.S01E01.1080p.mkv", ""), "Doctor Who", None));
        assert!(names_title(&src("Breaking Bad (2008) S01E01 (2160p).mkv", ""), "Breaking Bad", None));
        assert!(names_title(&src("Game.of.Thrones.S08E03.2160p.mkv", ""), "Game of Thrones", None));
        // Apostrophes and "&" are written either way in release names.
        assert!(names_title(&src("Schindlers.List.1993.2160p.UHD.BluRay.REMUX.mkv", ""), "Schindler's List", Some(1993)));
        assert!(names_title(&src("Schindler's List (1993) [1080p].mp4", ""), "Schindler\u{2019}s List", Some(1993)));
        assert!(names_title(&src("Fast.and.Furious.2009.1080p.mkv", ""), "Fast & Furious", Some(2009)));
        assert!(names_title(&src("Fast & Furious 2009 2160p.mkv", ""), "Fast & Furious", Some(2009)));
        // A translated title first, then the film's own.
        assert!(names_title(&src("", "Kód 3 - Code 3 (2024)(CZ.EN)[2160p][WebDL] CZ-AAC2.0 EN-DDP5.1 SDR x265"), "Code 3", Some(2025)));
        assert!(names_title(&src("", "Код 3 / Code 3 (2025) WEB-DL-HEVC 2160p"), "Code 3", Some(2025)));
        assert!(!names_title(&src("", "Code 37 - Season 1 (2011)"), "Code 3", Some(2025)));
        let mut native = src("Your.Name.2016.1080p.BluRay.mkv", "💾 8 GB");
        let mut off = other.clone();
        native.off_title = !names_title(&native, "Your Name", Some(2016));
        off.off_title = !names_title(&off, "Your Name", Some(2016));
        assert_eq!(play_order_for(&[off, native.clone()], None, None)[0].url, native.url);
    }

    #[test]
    fn the_original_remux_beats_a_bigger_dub_collection() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": format!("https://x.invalid/{}", title.len())}));
        let dubs = src("Film.1985.2160p.UHD.BluRay.REMUX.HDR.HEVC.22xRUS.UKR.ENG 💾 124 GB");
        let original = src("Film.1985.2160p.UHD.BluRay.REMUX.HDR.HEVC.TrueHD.7.1 💾 60 GB");
        let web = src("Film.1985.2160p.WEB-DL.HDR.HEVC 💾 20 GB");
        let order = play_order_for(&[web.clone(), original.clone(), dubs.clone()], None, None);
        assert_eq!(order[0].url, dubs.url);
        let urls = |r: Vec<(AudioVerdict, Source)>| r.into_iter().map(|(_, s)| s.url).collect::<Vec<_>>();
        // Both remuxes have English; the one with English as its main track goes first. Encodes keep their order.
        assert_eq!(urls(rank_by_audio(order.clone(), vec![Some((true, false)), Some((true, true)), Some((true, true))])), [original.url.clone(), dubs.url.clone(), web.url.clone()]);
        // A remux without English still goes after a WEB-DL with it.
        assert_eq!(urls(rank_by_audio(order.clone(), vec![Some((false, false)), Some((false, false)), Some((true, true))])), [web.url.clone(), dubs.url.clone(), original.url.clone()]);
        // Unread files keep the Play order.
        assert_eq!(urls(rank_by_audio(order, vec![None, None, None])), [dubs.url, original.url, web.url]);
    }

    #[test]
    fn upscales_go_after_native_files() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": format!("https://x.invalid/{}", title.len())}));
        let list = [src("Film.2001.BD.AI_UPSCALE_3840x2160 HDR 💾 57 GB"), src("Show.S01E01.2160p.Upscale.BluRay.DV.HDR 💾 13 GB"),
            src("Film.2016.2160p_DCPRip_4k_Upscale_DV_HDR10Plus 💾 83 GB"), src("Film.2001.1080p.BluRay.REMUX 💾 30 GB"),
            src("Film.2160p.SDR.to.HDR.WEB 💾 20 GB"), src("Film.2001.1080p.WEB-DL 💾 6 GB")];
        let order: Vec<String> = play_order_for(&list, None, None).into_iter().map(|s| s.label.trim().to_owned()).collect();
        assert_eq!(&order[..2], ["Film.2001.1080p.BluRay.REMUX 💾 30 GB", "Film.2001.1080p.WEB-DL 💾 6 GB"]);
        assert_eq!(order.len(), 6);
        for title in ["Upscaler.Documentary.2020.1080p", "Film.2160p.HDR.REMUX", "Film AI 2001 1080p"] { assert!(!upscaled().is_match(title), "{title}"); }
    }

    #[test]
    fn play_never_chooses_under_1080p() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": format!("https://x.invalid/{}", title.len())}));
        let list = [src("Film.720p.HDR.REMUX 💾 90 GB"), src("Film.480p.WEB 💾 2 GB"), src("Film.DVDRip 💾 9 GB"), src("Film.HDCAM 💾 3 GB"),
            src("Film HDR 💾 80 GB"), src("Film.1080p.WEB-DL 💾 4 GB"), src("Film.2160p.WEB-DL 💾 12 GB")];
        let order: Vec<String> = play_order_for(&list, None, None).into_iter().map(|s| s.label.trim().to_owned()).collect();
        // 720p, SD, DVD rips and cams never; a file without a resolution only after every labelled
        // one, even when it is bigger and HDR.
        assert_eq!(order, ["Film.2160p.WEB-DL 💾 12 GB", "Film.1080p.WEB-DL 💾 4 GB", "Film HDR 💾 80 GB"]);
    }

    #[test]
    fn play_order_matches_the_android_app() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": "https://x.invalid/a"}));
        let list = [src("Film.2160p.DV.WEB-DL 💾 30 GB"), src("Film.2160p.HDR10+.WEB-DL.HEVC 💾 20 GB"),
            src("Film.1080p.BluRay.x264 💾 40 GB"), src("Film.2160p.HDR.REMUX 💾 20.5 GB"),
            source_of("B", &json!({"title": "Film 4K 💾 90 GB", "infoHash": "abc"}))];
        let order: Vec<String> = play_order(&list, None).into_iter().map(|s| s.label.trim().to_owned()).collect();
        // HDR first, size before HDR10+; DV-only stays last; torrents never.
        assert_eq!(order, ["Film.2160p.HDR.REMUX 💾 20.5 GB", "Film.2160p.HDR10+.WEB-DL.HEVC 💾 20 GB",
            "Film.1080p.BluRay.x264 💾 40 GB", "Film.2160p.DV.WEB-DL 💾 30 GB"]);
        let pack = source_of("C", &json!({"title": "Show S01 COMPLETE 1080p 💾 40 GB", "url": "https://x.invalid/p"}));
        assert_eq!(episode_size(&pack, Some(10)), Some(4_000_000_000));
        assert_eq!(date_label("2014-11-05T00:00:00.000Z"), "5 Nov 2014");
    }

    /// What a file's header says outranks its label: a "4K HDR" that is a 1080p in SDR, a "HDR"
    /// that is Dolby Vision alone (profile 5), and a 4K whose real bitrate is a re-encode's.
    #[test]
    fn a_file_s_own_picture_outranks_its_label() {
        use crate::tracks::{Picture, Transfer};
        let src = |title: &str| source_of("A", &json!({"title": title, "url": format!("https://x.invalid/{}", title.len())}));
        let pic = |w, h, transfer, dv, seconds| Picture { width: w, height: h, transfer, dolby_vision: dv, seconds };
        let fake = with_picture(src("Film 2025 2160p HDR WEB-DL 30 GB"), pic(1920, 800, Transfer::Sdr, None, Some(7200.0)));
        assert_eq!((fake.quality, fake.hdr), (Some("1080p"), None));
        let real = with_picture(src("Film 2025 2160p WEB-DL 20 GB"), pic(3840, 1600, Transfer::Pq, None, Some(7200.0)));
        assert_eq!((real.quality, real.hdr), (Some("4K"), Some("HDR")), "HDR the label didn't say");
        let dv5 = with_picture(src("Film 2025 2160p HDR WEB-DL 25 GB"), pic(3840, 2160, Transfer::Pq, Some(5), Some(7200.0)));
        assert_eq!(dv5.hdr, Some("DV"));
        let plus = with_picture(src("Film 2025 2160p HDR10+ WEB-DL 22 GB"), pic(3840, 2160, Transfer::Pq, None, Some(7200.0)));
        assert_eq!(plus.hdr, Some("HDR10+"), "HDR10+ is in the frames: the label's stands over the header's HDR");
        let order: Vec<_> = play_order_for(&[fake.clone(), dv5.clone(), real.clone(), plus.clone()], None, None).into_iter().map(|s| s.url).collect();
        assert_eq!(order, [plus.url, real.url, fake.url, dv5.url]);
        // 2.5 GB over two hours is under 3 Mb/s: a 4K that competes as a 1080p, against a bigger 1080p.
        let thin = with_picture(src("Film 2025 2160p WEB-DL 2.5 GB"), pic(3840, 2160, Transfer::Sdr, None, Some(7200.0)));
        let full = src("Film 2025 1080p BluRay REMUX 30 GB");
        assert_eq!(play_order_for(&[thin.clone(), full.clone()], None, None)[0].url, full.url);
        assert_eq!(picture_summary(&thin), "3840x2160 SDR 3 Mb/s");
    }

    /// Resolution before size, but a file too small for its resolution competes as a 1080p.
    #[test]
    fn resolution_then_size_unless_the_file_is_too_small_for_it() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": format!("https://x.invalid/{}", title.len())}));
        let order = |list: &[Source]| play_order_for(list, None, None).into_iter().map(|s| s.label.trim().to_owned()).collect::<Vec<_>>();
        // A film: a 4.45 GB 4K WEBRip over a bigger 1080p; a 2 GB "4K" is ranked among the 1080p files by size.
        assert_eq!(order(&[src("Film 2025 1080p WEB-DL 6.28 GB"), src("Film 2025 2160p WEBRip 4.45 GB")]),
            ["Film 2025 2160p WEBRip 4.45 GB", "Film 2025 1080p WEB-DL 6.28 GB"]);
        assert_eq!(order(&[src("Film 2025 2160p WEBRip 2 GB"), src("Film 2025 1080p WEB-DL 6 GB"), src("Film 2025 1080p WEBRip 1.5 GB")]),
            ["Film 2025 1080p WEB-DL 6 GB", "Film 2025 2160p WEBRip 2 GB", "Film 2025 1080p WEBRip 1.5 GB"]);
        // An episode needs less: 1.5 GB is a real 4K episode, 0.6 GB isn't.
        assert_eq!(order(&[src("Show S01E01 1080p WEB-DL 3 GB"), src("Show S01E01 2160p WEB-DL 1.5 GB")]),
            ["Show S01E01 2160p WEB-DL 1.5 GB", "Show S01E01 1080p WEB-DL 3 GB"]);
        assert_eq!(order(&[src("Show S01E01 2160p WEBRip 0.6 GB"), src("Show S01E01 1080p WEB-DL 1 GB")]),
            ["Show S01E01 1080p WEB-DL 1 GB", "Show S01E01 2160p WEBRip 0.6 GB"]);
    }

    #[test]
    fn hdr_then_size_then_hdr_format() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": "https://x.invalid/a"}));
        let list = [src("Film 2160p HDR10+ WEB-DL 20 GB"), src("Film 2160p REMUX 60 GB"),
            src("Film 2160p HDR10+ WEB-DL"), src("Film 2160p HDR10+ REMUX 60 GB")];
        let order: Vec<_> = play_order_for(&list, None, None).into_iter().map(|s| s.label.trim().to_owned()).collect();
        assert_eq!(order, ["Film 2160p HDR10+ REMUX 60 GB", "Film 2160p HDR10+ WEB-DL 20 GB",
            "Film 2160p HDR10+ WEB-DL", "Film 2160p REMUX 60 GB"]);
    }

    #[test]
    fn exclude_3d_from_every_provider_and_metadata_field() {
        for tag in ["3D", "3-D", "SBS", "HSBS", "H-SBS", "Half.SBS", "Full_SBS", "HOU", "H-TAB", "TAB", "Side.by.Side", "Top.And.Bottom", "Over-Under", "Stereoscopic"] {
            assert!(is_3d(&format!("Show.S04E07.{tag}.2160p.mkv")), "{tag}");
        }
        for title in ["Three Days 2160p", "3 Days to Kill", "S03E03 1080p", "Film AC3 DDP5.1 HDR", "Film Stereotype"] {
            assert!(!is_3d(title), "{title}");
        }
        for addon in ["Torrentio RD", "AIOStreams", "Other"] {
            for field in ["name", "title", "description", "filename"] {
                let mut v = json!({"url":"https://x.invalid/3d", "title":"Show S04E07 2160p HDR 90 GB"});
                if field == "filename" { v["behaviorHints"] = json!({"filename":"Show.S04E07.3D.HSBS.mkv"}); }
                else { v[field] = json!("Show S04E07 3D HDR 90 GB"); }
                assert!(play_order_for(&[source_of(addon, &v)], None, None).is_empty(), "{addon}/{field}");
            }
        }
        let src = |title: &str| source_of("AIOStreams", &json!({"title":title,"url":"https://x.invalid/2d"}));
        let small = src("Film 2160p HDR10+ WEB-DL 5 GB");
        let big = src("Film 2160p HDR REMUX 50 GB");
        let sdr = src("Film 2160p SDR REMUX 90 GB");
        let order: Vec<_> = play_order_for(&[sdr.clone(), small, big], None, None).into_iter().map(|s| s.size_bytes).collect();
        assert_eq!(order, [Some(50_000_000_000), Some(5_000_000_000), Some(90_000_000_000)]);
        assert_eq!(play_order_for(std::slice::from_ref(&sdr), None, None), [sdr]);
    }

    #[test]
    fn picks_trailers_are_tmdb_s_official_ones_first() {
        let addon = vec![Trailer { youtube: None, url: Some("https://x.invalid/trailer.mp4".into()) }, Trailer { youtube: Some("fan".into()), url: None }];
        let tmdb = vec!["official".to_owned(), "teaser".to_owned()];
        let list = official_first(&tmdb, &addon);
        assert_eq!(list.iter().map(|t| t.youtube.clone().unwrap()).collect::<Vec<_>>(), ["official", "teaser", "fan"]);
        assert!(list.iter().all(|t| t.url.is_none()), "an add-on link must not jump ahead of TMDB's trailers");
        assert_eq!(official_first(&[], &addon), addon);
    }

    #[test]
    fn better_posters_first_then_the_addon_poster() {
        let m = Media::from_json(&json!({"id":"tt6262770","name":"Tenzing","type":"movie","poster":"https://images.example/p.jpg"}), "a").unwrap();
        assert_eq!(posters(&m), [
            "https://btttr.cc/poster-q/imdb/poster-default/tt6262770.jpg",
            "https://images.example/p.jpg",
            "https://images.metahub.space/poster/large/tt6262770/img",
            "https://images.metahub.space/poster/medium/tt6262770/img",
        ]);
        let k = Media::from_json(&json!({"id":"kitsu:1","name":"Anime","type":"anime","poster":"https://images.example/k.jpg"}), "a").unwrap();
        assert_eq!(posters(&k), ["https://images.example/k.jpg"]);
    }

    #[test]
    fn calendar_dates_and_relative_days() {
        assert_eq!(civil(0), (1970, 1, 1, 3));
        assert_eq!(civil(1_763_164_800), (2025, 11, 15, 5)); // a Saturday
        let now = 1_763_164_800 + 3600;
        assert_eq!(when_label(now - 60, now), "Today");
        assert_eq!(when_label(now - 86_400, now), "Yesterday");
        assert_eq!(when_label(now - 30 * 86_400, now), "16 Oct");
    }

    #[test]
    fn accent_is_readable_under_black_text_and_skips_greyscale() {
        let (r, g, b) = artwork_accent(&[[120, 20, 20, 255], [40, 40, 40, 255]]).unwrap();
        assert!(luminance(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0) >= 0.17);
        assert!(artwork_accent(&[[128, 128, 128, 255]]).is_none());
    }

    #[test]
    fn justwatch_stills_require_exact_imdb_match() {
        let payload = json!({
            "data": {
                "popularTitles": {
                    "edges": [
                        { "node": { "content": {
                            "externalIds": { "imdbId": "tt7549996" },
                            "backdrops": [{ "backdropUrl": "/backdrop/999/s1440/judy.jpg" }]
                        }}},
                        { "node": { "content": {
                            "externalIds": { "imdbId": "tt31450459" },
                            "backdrops": [
                                { "backdropUrl": "/backdrop/1/s1440/digger.jpg" },
                                { "backdropUrl": "/backdrop/2/s1440/digger.jpg" },
                                { "backdropUrl": "/backdrop/2/s1440/digger.jpg" },
                                { "backdropUrl": "/poster/3/s718/ignored.jpg" }
                            ]
                        }}}
                    ]
                }
            }
        });
        assert_eq!(parse_justwatch_stills(&payload, "tt31450459"), [
            "https://images.justwatch.com/backdrop/1/s1440/digger.jpg",
            "https://images.justwatch.com/backdrop/2/s1440/digger.jpg",
        ]);
        assert!(parse_justwatch_stills(&payload, "tt0000000").is_empty());
    }

    #[test]
    fn digger_header_and_working_title_alias_never_match_judy() {
        let src = |file: &str, description: &str| source_of("A", &json!({
            "url": "https://x.invalid/a",
            "behaviorHints": { "filename": file },
            "title": description
        }));
        // Bare "Digger (2026)" add-on header line must never rescue a Judy filename.
        let bare_header = src("Judy.BDRemux.1080p.pk.mkv", "Digger (2026)\n💾 18.4 GB");
        assert!(!names_title(&bare_header, "Digger", Some(2026)));
        // Even a tagged description line for Digger must not override a clearly different movie filename.
        let tagged_header = src("Judy.BDRemux.1080p.pk.mkv", "Digger (2026) 1080p BDRemux\n💾 18.4 GB");
        assert!(!names_title(&tagged_header, "Digger", Some(2026)));
        // A filename with a different movie year (2019 vs 2026) is always rejected.
        let wrong_year_file = src("Judy.2019.1080p.BluRay.mkv", "Digger 2026 1080p WEB-DL");
        assert!(!names_title(&wrong_year_file, "Digger", Some(2026)));
        // A secondary alias / working title ("Judy") requires a matching release year (±1) on movies.
        let judy_no_year = src("Judy.BDRemux.1080p.pk.mkv", "Judy.BDRemux.1080p.pk.mkv\n💾 18.4 GB");
        assert!(!names_title_any(&judy_no_year, &["Digger".into(), "Judy".into()], Some(2026)));
        // Legitimate bilingual filename + description still matches.
        let bilingual = src("Kod.3.2024.2160p.mkv", "Kód 3 - Code 3 (2024)(CZ.EN)[2160p][WebDL]");
        assert!(names_title(&bilingual, "Code 3", Some(2025)));
        // Legitimate localized alias with matching release year still matches.
        let localized = src("Scontrarsi.a.Natale.2026.1080p.WEB-DL.ITA.mkv", "Scontrarsi.a.Natale.2026.1080p.WEB-DL.ITA.mkv");
        assert!(names_title_any(&localized, &["Clashing Through the Snow".into(), "Scontrarsi a Natale".into()], Some(2026)));
    }
}

/// Real-network timing of the autoplay pipeline (add-ons → order → probe → first bytes).
/// Run against a copy of a store: `VEYLO_DX_DATA=/tmp/copy cargo test autoplay_bench -- --ignored --nocapture`.
/// Prints add-on names, labels' quality tags and timings only: never a URL.
#[cfg(test)]
mod autoplay_bench {
    use super::*;
    use std::time::Instant;

    /// kind/id/title/year (year for films).
    const TITLES: &[&str] = &["movie/tt15239678/Dune: Part Two/2024", "movie/tt15398776/Oppenheimer/2023", "movie/tt0133093/The Matrix/1999",
        "movie/tt0118799/Life Is Beautiful/1997", "series/tt0903747:1:1/Breaking Bad", "series/tt14452776:2:3/The Bear",
        "series/tt11280740:1:1/Severance", "movie/tt5311514/Your Name./2016", "movie/tt0211915/Amélie/2001",
        "series/tt0306414:1:1/The Wire", "movie/tt0034583/Casablanca/1942", "movie/tt6751668/Parasite/2019"];

    /// The backend on a copy of a store, once for every test here.
    fn start() {
        assert!(std::env::var_os("VEYLO_DX_DATA").is_some(), "point VEYLO_DX_DATA at a copy of a store");
        static STARTED: std::sync::Once = std::sync::Once::new();
        STARTED.call_once(|| init().unwrap());
    }

    async fn first_bytes(src: &Source, want: u64) -> Option<(u128, f64)> {
        let src = src.clone();
        on_runtime(async move {
            let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().ok()?;
            let mut req = client.get(&src.url).header("range", format!("bytes=0-{}", want - 1));
            for (k, v) in &src.headers { req = req.header(k.as_str(), v.as_str()); }
            let t = Instant::now();
            let mut r = req.send().await.ok()?;
            let ttfb = t.elapsed().as_millis();
            let t2 = Instant::now();
            let mut got = 0u64;
            while let Ok(Some(chunk)) = r.chunk().await { got += chunk.len() as u64; if got >= want { break; } }
            Some((ttfb, got as f64 * 8.0 / 1e6 / t2.elapsed().as_secs_f64().max(0.001)))
        }).await.ok().flatten()
    }

    /// The app's developer benchmark (`dev_autoplay`) on BENCH_LIST: every title cold, then fetched ahead.
    #[test]
    #[ignore]
    fn dev_autoplay_list() {
        start();
        let list = std::fs::read_to_string(std::env::var("BENCH_LIST").expect("BENCH_LIST: a file of kind/id/title/year lines")).unwrap();
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(dev_autoplay(list));
    }

    #[test]
    #[ignore]
    fn autoplay_bench() {
        start();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let picked: Vec<String> = std::env::var("BENCH_TITLES").map(|t| t.split(',').map(str::to_owned).collect())
                .unwrap_or_else(|_| TITLES.iter().map(|t| t.to_string()).collect());
            for entry in &picked {
                let parts: Vec<&str> = entry.splitn(4, '/').collect();
                let (kind, id, name, year) = (parts[0], parts[1], parts.get(2).copied().unwrap_or(""), parts.get(3).and_then(|y| y.parse().ok()));
                println!("\n=== {kind} {id} {name}");
                let targets = exec(json!({"op":"streams_targets","type":kind,"id":id})).await.unwrap();
                for t in targets["targets"].as_array().unwrap().iter().filter(|_| std::env::var_os("BENCH_QUICK").is_none()) {
                    let t0 = Instant::now();
                    let r = exec(json!({"op":"streams","type":kind,"id":id,"addon_url":t["addon_url"],"force":true})).await;
                    let n = r.as_ref().ok().and_then(|v| v["groups"][0]["streams"].as_array().map(|a| a.len())).unwrap_or(0);
                    let err = r.as_ref().ok().and_then(|v| v["groups"][0]["error"].as_str().map(str::to_owned)).unwrap_or_default();
                    println!("  addon {:<28} {:>6} ms  {n:>3} streams {err}", t["addon_name"].as_str().unwrap_or(""), t0.elapsed().as_millis());
                }
                let pack = (kind == "series").then_some(10);
                if let Ok(lang) = std::env::var("BENCH_LANG") {
                    let (list, _) = sources(kind, id).await.unwrap();
                    for s in play_order_for(&list, pack, Some(&lang)).iter().take(14) {
                        let l = crate::addons::language(&lang).unwrap();
                        let tier = if l.matches(&s.label) { 0 } else if other_language_only(&s.label, l.key) { 2 } else { 1 };
                        println!("  [tier {tier}] {} {} {} | {}", s.quality.unwrap_or("?"), s.hdr.unwrap_or("SDR"), s.size.as_deref().unwrap_or("?"),
                            format!("{:?} // {:?}", s.name, s.description).chars().take(260).collect::<String>());
                    }
                    continue;
                }
                let t0 = Instant::now();
                let pick = autoplay(kind, id, pack, name, year).await;
                let took = t0.elapsed().as_millis();
                let rate = match &pick.best { Some(b) => first_bytes(b, 16 << 20).await.map(|(ttfb, mbps)| format!("ttfb {ttfb} ms, {mbps:.0} Mbit/s")).unwrap_or_else(|| "no data".into()), None => String::new() };
                println!("  AUTOPLAY {took} ms -> {} | {rate}", pick.best.as_ref().map(|b| format!("{} [lang {}] {} {:?}", source_summary(b), if claims_language(b) { "claimed" } else if other_language_only(&b.label, "english") { "OTHER" } else { "untagged" }, b.filename.chars().take(70).collect::<String>(), b.description.lines().last().unwrap_or(""))).unwrap_or_else(|| format!("nothing {:?}", pick.debrid)));
                for r in pick.rest.iter().take(if std::env::var_os("BENCH_NAMES").is_some() { 4 } else { 0 }) {
                    println!("     next: {} {}", source_summary(r), r.filename.chars().take(70).collect::<String>());
                }
            }
        });
    }
}
