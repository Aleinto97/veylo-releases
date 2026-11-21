//! The bridge between the Dioxus shell and `rustmedia-core`.
//!
//! Core requests run on one dedicated Tokio runtime; the UI awaits their join
//! handles, so it never needs to know which executor Dioxus itself uses.
//! Nothing here logs addon URLs, request bodies or credentials.

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

/// Prototype store, separate from any device library. Override with VEYLO_DX_DATA.
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let dir = data_dir();
    std::fs::create_dir_all(&dir).context("Couldn't create the data folder")?;
    // Some Android builds (Huawei) hide app logs: keep the last panic in the app's own folder.
    let panic_file = dir.join("last-panic.txt");
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::write(&panic_file, format!("{info}\n{}", std::backtrace::Backtrace::force_capture()));
        default_hook(info);
    }));
    let core = Core::open(&dir)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let proxy = { let _inside = runtime.enter(); rustmedia_media_proxy::MediaProxy::new() };
    // The settings the backend keeps in memory, read once.
    let saved: Value = std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    BETTER_POSTERS.store(saved["better_posters"].as_bool().unwrap_or(true), std::sync::atomic::Ordering::Relaxed);
    HDR_OUTPUT.store(saved["hdr_output"].as_bool().unwrap_or(true), std::sync::atomic::Ordering::Relaxed);
    set_stream_language(Some(audio_language(&saved)));
    set_tmdb_key(saved["tmdb_key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned));
    RUNTIME.set(runtime).map_err(|_| anyhow!("Backend already started"))?;
    BACKEND
        .set(Backend { core, proxy, data_dir: dir, http, accents: Mutex::new(HashMap::new()) })
        .map_err(|_| anyhow!("Backend already started"))?;
    Ok(())
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

/// Loads the bundled public catalogs so Picks has candidates to rank.
/// Failures are tolerated: whatever arrives is still usable.
pub async fn seed_catalogs() -> Vec<Media> {
    let requests = [(CINEMETA, "movie", "top"), (CINEMETA, "series", "top")];
    let mut out = Vec::new();
    if better_posters_on() {
        for (kind, id) in [("movie", "trakt-trending"), ("series", "trakt-trending")] {
            if let Some(v) = bp_get(format!("catalog/{kind}/{id}.json")).await { out.extend(bp_titles(&v, CINEMETA)); }
        }
    }
    for (addon, kind, id) in requests {
        let req = json!({"op":"catalog","addon_url":addon,"type":kind,"id":id});
        if let Ok(v) = exec(req).await {
            out.extend(metas(&v, addon));
        }
    }
    out
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
}

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
        } else if shared.contains(&r.title) && !r.addon_name.is_empty() { Some(r.addon_name.clone()) } else { None };
        CatalogRow { key: r.key, title, subtitle, addon_url: r.addon_url, kind: r.kind, id: r.id, extra: r.extra }
    }).collect())
}

pub async fn catalog(row: CatalogRow) -> Result<Vec<Media>> {
    // A BetterPosters chart; Cinemeta's popular list of the same type when it can't answer.
    if row.key.starts_with(BP_ROW) {
        let titles = bp_get(format!("catalog/{}/{}.json", row.kind, row.id)).await.map(|v| bp_titles(&v, &row.addon_url)).unwrap_or_default();
        if !titles.is_empty() { return Ok(titles); }
        return Box::pin(catalog(CatalogRow { key: String::new(), id: "top".into(), ..row })).await;
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
        .map(|v| format!("S{} E{}", v["season"].as_i64().unwrap_or(1), v["episode"].as_i64().unwrap_or(0)));
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
            match write { ProgressWrite::Update(req) => { let _ = backend().core.execute(req).await; }, ProgressWrite::Barrier(done) => { let _ = done.send(()); } }
        } });
        tx
    });
    let mut req = json!({"op":"progress","type":media.kind,"id":media.id,"addon_url":media.addon_url,
        "position_ms":position_ms.max(0),"duration_ms":duration_ms,"watched_add_ms":watched_ms.clamp(0, duration_ms),
        "meta": if with_meta { media.raw.clone() } else { json!({}) }});
    if let Some(v) = &video_id { req["video_id"] = json!(v); }
    let _ = queue.send(ProgressWrite::Update(req));
}

/// A film is watched when you marked it, or the core saw you play it through.
pub fn film_watched(m: &Media) -> bool { m.watched }

/// Mark episodes watched or not (without moving where you are in the series).
pub async fn set_episodes_watched(media: &Media, video_ids: Vec<String>, watched: bool) -> Result<()> {
    if video_ids.is_empty() { return Ok(()); }
    exec(json!({"op":"mark_videos","type":media.kind,"id":media.id,"addon_url":media.addon_url,"watched":watched,"video_ids":video_ids,"meta":media.raw})).await?;
    Ok(())
}

/// Stop tracking a title: its history, progress, marks and rating are deleted from the store.
pub async fn forget(media: &Media) -> Result<()> {
    exec(json!({"op":"forget","type":media.kind,"id":media.id,"addon_url":media.addon_url})).await?;
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
    let se = |s: &str, e: &str| match (s.parse::<i64>(), e.parse::<i64>()) { (Ok(s), Ok(e)) => Some(format!("S{s} E{e}")), _ => None };
    match parts.as_slice() {
        ["kitsu", _, e] => e.parse::<i64>().ok().map(|e| format!("E{e}")),
        [series, s, e] if series.starts_with("tt") => se(s, e),
        [_, _, s, e] => se(s, e),
        _ => None,
    }
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

pub async fn title_meta(media: &Media) -> Result<TitleMeta> {
    let request = exec(json!({"op":"meta","type":media.kind,"id":media.id,"addon_url":media.addon_url}));
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
    // TMDB's trailers follow the add-on's, as alternatives where one is blocked or missing.
    let art = artwork(media).await;
    let mut genres: Vec<String> = meta["genres"].as_array().into_iter().flatten().filter_map(Value::as_str).map(crate::picks_rules::canonical_genre).collect();
    for genre in art.as_ref().map(|a| a.genres.as_slice()).unwrap_or_default() { if !genres.contains(genre) { genres.push(genre.clone()); } }
    for key in art.map(|a| a.trailers).unwrap_or_default() {
        let t = Trailer { youtube: Some(key), url: None };
        if !trailers.contains(&t) { trailers.push(t); }
    }
    Ok(TitleMeta {
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
    })
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
    Ok(())
}

/// Mark a film watched or not: the core's own mutation.
pub async fn set_watched(media: &Media, watched: bool) -> Result<()> {
    exec(json!({"op":"mark_watched","type":media.kind,"id":media.id,"addon_url":media.addon_url,"watched":watched,"meta":media.raw})).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceKind { Direct, Torrent, External }

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub addon: String,
    pub name: String,
    pub description: String,
    pub url: String,
    pub filename: String,
    /// Declared size in bytes, or the add-on's own size text; None when absent (never estimated).
    pub size: Option<String>,
    pub size_bytes: Option<u64>,
    pub declared_size: bool,
    /// The size written in the add-on's label (💾), apart from the declared one.
    pub labelled_bytes: Option<u64>,
    /// The torrent is a season pack (its name is a season, not an episode), even when the
    /// file name is the one episode that plays.
    pub pack: bool,
    pub quality: Option<&'static str>,
    pub hdr: Option<&'static str>,
    pub kind: SourceKind,
    pub headers: Vec<(String, String)>,
    /// name, title, description and filename together: what the play order reads.
    pub label: String,
    /// No line of the release names the title being played (an add-on's loose match: "Call Me by
    /// Your Name" for "Your Name"): such sources go after every one that does.
    pub off_title: bool,
}

fn find_quality(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.contains("2160p") || t.contains("4k") || t.contains("uhd") { Some("4K") }
    else if t.contains("1440p") { Some("1440p") }
    else if t.contains("1080p") || t.contains("fhd") { Some("1080p") }
    else if t.contains("720p") { Some("720p") }
    else if t.contains("576p") || t.contains("480p") { Some("SD") }
    else { None }
}

fn find_hdr(text: &str) -> Option<&'static str> {
    match hdr_tier(text) { 3 => Some("HDR10+"), 2 => Some("HDR"), 1 => Some("DV"), _ => None }
}

/// "4.2 GiB": a size in bytes as the player shows it.
pub fn file_size_label(bytes: u64) -> String { binary_size(bytes as f64) }

fn binary_size(bytes: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes; let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 { v /= 1024.0; i += 1; }
    if i >= 3 { format!("{v:.1} {}", units[i]) } else { format!("{v:.0} {}", units[i]) }
}

fn re(pattern: &str) -> regex::Regex { regex::Regex::new(pattern).expect("static pattern") }
macro_rules! static_re { ($name:ident, $pat:expr) => {
    fn $name() -> &'static regex::Regex { static R: OnceLock<regex::Regex> = OnceLock::new(); R.get_or_init(|| re($pat)) }
}; }
static_re!(hdr10_plus, r"(?i)\bHDR10(?:\+|[ ._-]?PLUS\b|P\b)");
static_re!(hdr_any, r"(?i)\b(?:HDR10|HDR|HLG)\b");
static_re!(dolby_vision, r"(?i)\b(?:DV|DOVI|DOLBY[ ._-]?VISION)\b");
static_re!(remux, r"(?i)REMUX\b");
static_re!(bluray, r"(?i)\b(?:BLU[ ._-]?RAY|BDRIP|BRRIP|BDREMUX|BD(?:25|50|66|100))\b");
static_re!(web_dl, r"(?i)\bWEB(?:[ ._-]?DL)?\b");
static_re!(web_rip, r"(?i)\bWEB[ ._-]?RIP\b");
static_re!(low_source, r"(?i)\b(?:HDTV|PDTV|DVDRIP|DVDSCR|HDCAM|CAM|TS|TELESYNC|TC|SCREENER)\b");
static_re!(av1, r"(?i)\bAV1\b");
// Resolution or HDR made up after the fact: AI upscales and SDR regraded to HDR.
static_re!(upscaled, r"(?i)(?:\b|_)(?:AI[ ._-]?)?UPSCALE(?:D|S)?(?:\b|_)|\bUPSCL\b|\bAI[ ._-]?(?:ENHANCED|REMASTER(?:ED)?)\b|\bREGRADED?\b|\bSDR[ ._-]?TO[ ._-]?HDR\b|\bFAKE[ ._-]?HDR\b");
static_re!(hevc, r"(?i)\b(?:HEVC|[HX][ ._-]?265)\b");
static_re!(avc, r"(?i)\b(?:AVC|[HX][ ._-]?264)\b");
static_re!(episode_mark, r"(?i)\bS\d{1,2}[ ._-]?E\d{1,3}\b|\b\d{1,2}x\d{2,3}\b|\bEP?[ ._-]?\d{2,3}\b");
static_re!(season_pack, r"(?i)\bS\d{1,2}\b|\bSEASONS?[ ._-]?\d|\bCOMPLETE\b");
static_re!(html_tag, "<[^>]+>");
static_re!(multi_audio, r"(?i)\b(?:MULTI|DUAL)(?:[ ._-]?AUDIO)?\b|\bMULTi\b");
// Release tags used by Torrentio, AIOStreams and their upstream scrapers.
static_re!(stereo_video, r"(?i)\b(?:3[ .-]?D|(?:H(?:ALF)?|F(?:ULL)?)[ .-]?(?:SBS|OU|TAB)|SBS|TAB|SIDE[ .-]BY[ .-]SIDE|TOP[ .-](?:AND[ .-])?BOTTOM|OVER[ .-]UNDER|STEREOSCOPIC)\b");

fn is_3d(label: &str) -> bool { stereo_video().is_match(&label.replace('_', ".")) }

static_re!(size_label, r"(?i)(?:💾\s*)?(\d[\d.,]*)\s*([KMGT]i?B)\b");

/// HDR10+ (3), HDR10/HDR/HLG (2), Dolby Vision alone (1), SDR (0); "DV HDR10" counts as HDR10.
fn hdr_tier(text: &str) -> u8 {
    if hdr10_plus().is_match(text) { 3 } else if hdr_any().is_match(text) { 2 } else if dolby_vision().is_match(text) { 1 } else { 0 }
}
/// Remux (4), WEB-DL (3), WEBRip (2), unlabelled (1), HDTV/DVD/cam (0); a Blu-ray re-encode is None.
fn source_tier(text: &str) -> Option<u8> {
    if remux().is_match(text) { Some(4) } else if bluray().is_match(text) { None } else if web_rip().is_match(text) { Some(2) }
    else if web_dl().is_match(text) { Some(3) } else if low_source().is_match(text) { Some(0) } else { Some(1) }
}
fn codec_tier(text: &str) -> u8 { if av1().is_match(text) { 3 } else if hevc().is_match(text) { 2 } else if avc().is_match(text) { 1 } else { 0 } }

/// The size in the add-on's label ("💾 45.3 GB" preferred): text as written, and bytes.
fn labelled_size(text: &str) -> Option<(String, u64)> {
    let all: Vec<_> = size_label().captures_iter(text).filter(|c| c[1].chars().any(|d| ('1'..='9').contains(&d))).collect();
    let c = all.iter().find(|c| c[0].starts_with('💾')).or(all.first())?;
    let raw = &c[1];
    let number: f64 = (if raw.contains('.') { raw.replace(',', "") } else { raw.replace(',', ".") }).parse().ok()?;
    let unit = c[2].to_uppercase();
    let power = "BKMGTPE".find(unit.chars().next()?).unwrap_or(0) as i32;
    let base: f64 = if unit.contains('I') { 1024.0 } else { 1000.0 };
    Some((format!("{} {}", raw, &c[2]), (number * base.powi(power)) as u64))
}

fn source_of(addon: &str, v: &Value) -> Source {
    let hints = &v["behaviorHints"];
    let (name, description) = (s(v, "name"), [s(v, "description"), s(v, "title")].into_iter().find(|t| !t.is_empty()).unwrap_or_default());
    let filename = s(hints, "filename");
    let url = s(v, "url");
    let kind = if !s(v, "infoHash").is_empty() { SourceKind::Torrent }
        else if url.starts_with("https://") || url.starts_with("http://") { SourceKind::Direct } else { SourceKind::External };
    let label = format!("{name} {} {} {filename}", s(v, "title"), s(v, "description"));
    let headers = hints["proxyHeaders"]["request"].as_object()
        .map(|h| h.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))).collect()).unwrap_or_default();
    let declared = hints["videoSize"].as_u64().or_else(|| hints["videoSize"].as_str().and_then(|x| x.parse().ok())).filter(|b| *b > 0);
    let labelled = labelled_size(&format!("{}\n{}\n{}", s(v, "title"), s(v, "description"), s(v, "name")));
    // Add-ons write the torrent's name on its own line (the file name, if any, on another).
    let pack = format!("{name}\n{}\n{}", s(v, "title"), s(v, "description")).lines()
        .any(|line| season_pack().is_match(line) && !episode_mark().is_match(line))
        || (!episode_mark().is_match(&label) && season_pack().is_match(&label));
    Source {
        addon: addon.to_owned(), name, description, filename,
        size: declared.map(|b| binary_size(b as f64)).or_else(|| labelled.as_ref().map(|l| l.0.clone())),
        size_bytes: declared.or(labelled.as_ref().map(|l| l.1)),
        declared_size: declared.is_some(),
        labelled_bytes: labelled.as_ref().map(|l| l.1),
        pack,
        quality: find_quality(&label), hdr: find_hdr(&label), kind, headers,
        url: if kind == SourceKind::Direct { url } else { String::new() },
        label,
        off_title: false,
    }
}

fn quality_rank(q: Option<&str>) -> u8 { match q { Some("4K") => 0, Some("1440p") => 1, Some("1080p") => 2, Some("720p") => 3, Some("SD") => 4, _ => 5 } }

/// Every add-on's sources for one video, best declared quality first; add-on order kept within a quality.
/// With a Real-Debrid key the free web add-ons are not asked: only debrid sources are listed.
pub async fn sources(kind: &str, video_id: &str) -> Result<(Vec<Source>, Vec<String>)> {
    sources_within(kind, video_id, None, None).await
}

/// How long Play waits for the slower add-ons once one has answered with sources it can play.
const ADDON_GRACE: Duration = Duration::from_secs(6);

/// `sources`, but with `grace` the answer comes that long after the first add-on with a playable
/// source, without waiting for an add-on that is slow or down (the core waits up to 30 s each).
/// `early` gets each add-on's sources as they come, while others are still answering.
async fn sources_within(kind: &str, video_id: &str, grace: Option<Duration>, early: Option<tokio::sync::mpsc::UnboundedSender<Vec<Source>>>) -> Result<(Vec<Source>, Vec<String>)> {
    let targets = exec(json!({"op":"streams_targets","type":kind,"id":video_id})).await?;
    let mut targets: Vec<Value> = targets["targets"].as_array().cloned().unwrap_or_default();
    let free = |t: &Value| crate::addons::is_free_addon(&s(t, "addon_name"));
    if load_settings().rd_key.is_some() && targets.iter().any(|t| !free(t)) {
        targets.retain(|t| !free(t));
    }
    let (kind, id) = (kind.to_owned(), video_id.to_owned());
    let groups = on_runtime(async move {
        let mut jobs = tokio::task::JoinSet::new();
        for (i, t) in targets.into_iter().enumerate() {
            let request = json!({"op":"streams","type":kind,"id":id,"addon_url":t["addon_url"]});
            jobs.spawn(async move { (i, s(&t, "addon_name"), backend().core.execute(request).await) });
        }
        let mut groups: Vec<(usize, Value)> = Vec::new();
        let mut deadline: Option<tokio::time::Instant> = None;
        loop {
            let next = match deadline {
                Some(at) => match tokio::time::timeout_at(at, jobs.join_next()).await { Ok(n) => n, Err(_) => break },
                None => jobs.join_next().await,
            };
            let Some(done) = next else { break };
            let Ok((i, name, answer)) = done else { continue };
            match answer {
                Ok(v) => for g in v["groups"].as_array().into_iter().flatten() {
                    let playable = g["streams"].as_array().into_iter().flatten().any(|st| s(st, "url").starts_with("http"));
                    if let (Some(grace), None, true) = (grace, deadline, playable) { deadline = Some(tokio::time::Instant::now() + grace); }
                    if let Some(tx) = early.as_ref().filter(|_| playable && !jobs.is_empty()) {
                        let addon = s(g, "addon_name");
                        let _ = tx.send(g["streams"].as_array().into_iter().flatten().map(|st| source_of(&addon, st)).filter(|s| !is_3d(&s.label)).collect());
                    }
                    groups.push((i, g.clone()));
                },
                Err(_) => groups.push((i, json!({"addon_name": name, "streams": [], "error": "unavailable"}))),
            }
        }
        // Add-ons still answering are left out (dropping the set cancels them).
        groups.sort_by_key(|(i, _)| *i);
        groups.into_iter().map(|(_, g)| g).collect::<Vec<Value>>()
    }).await?;
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for g in &groups {
        let addon = s(g, "addon_name");
        if g.get("error").is_some() { failed.push(addon.clone()); }
        out.extend(g["streams"].as_array().into_iter().flatten().map(|st| source_of(&addon, st)).filter(|s| !is_3d(&s.label)));
    }
    out.sort_by_key(|x| (x.kind != SourceKind::Direct, quality_rank(x.quality)));
    Ok((out, failed))
}

// ---------------------------------------------------------------- subtitles

/// OpenSubtitles' public add-on, asked when no installed add-on has subtitles for a video.
const OPENSUBTITLES: &str = "https://opensubtitles-v3.strem.io";
const INTRODB: &str = "https://api.introdb.app/segments";
const ANISKIP: &str = "https://api.aniskip.com/v2/skip-times";
const KITSU_API: &str = "https://kitsu.app/api/edge/anime";

/// Where an episode's intro times can be looked up, from its video id.
#[derive(Debug, PartialEq)]
pub enum SkipLookup { IntroDb { imdb: String, season: u32, episode: u32 }, Kitsu { id: u64, episode: u32 }, Mal { id: u64, episode: u32 } }

/// "tt0903747:1:2" (IntroDB), "kitsu:1376:5" and "mal:5114:3" (AniSkip); None for films and the rest.
pub fn skip_lookup(video_id: &str) -> Option<SkipLookup> {
    let parts: Vec<&str> = video_id.split(':').collect();
    match parts.as_slice() {
        [imdb, s, e] if imdb.starts_with("tt") && imdb[2..].chars().all(|c| c.is_ascii_digit()) && imdb.len() > 2 =>
            Some(SkipLookup::IntroDb { imdb: (*imdb).to_owned(), season: s.parse().ok()?, episode: e.parse().ok()? }),
        ["kitsu", id, e] => Some(SkipLookup::Kitsu { id: id.parse().ok()?, episode: e.parse().ok()? }),
        ["mal", id, e] => Some(SkipLookup::Mal { id: id.parse().ok()?, episode: e.parse().ok()? }),
        _ => None,
    }
}

/// The intro, recap and credits the online databases know for an episode: IntroDB for IMDb
/// episodes, AniSkip for anime (a Kitsu id is turned into its MyAnimeList id first). Only public
/// ids are sent. Nothing found, or a service down, is an empty list: the player just offers no skip.
pub async fn online_segments(video_id: &str, duration: f64) -> Vec<crate::engine::Segment> {
    let Some(lookup) = skip_lookup(video_id) else { return Vec::new() };
    let found = on_runtime(async move {
        let http = backend_http();
        let get = |url: url::Url| { let http = http.clone(); async move {
            let r = http.get(url).send().await?;
            // AniSkip answers 404 when it has nothing; that is an answer, not a failure.
            if r.status() == reqwest::StatusCode::NOT_FOUND { return Ok::<Value, anyhow::Error>(Value::Null); }
            Ok(r.error_for_status()?.json::<Value>().await?)
        } };
        let aniskip = |mal: u64, episode: u32| async move {
            let mut url = url::Url::parse(ANISKIP)?;
            url.path_segments_mut().map_err(|_| anyhow!("bad url"))?.extend([mal.to_string(), episode.to_string()]);
            {
                let mut q = url.query_pairs_mut();
                for t in ["op", "ed", "mixed-op", "mixed-ed", "recap"] { q.append_pair("types", t); }
                q.append_pair("episodeLength", &format!("{:.3}", duration.max(0.0)));
            }
            Ok::<_, anyhow::Error>(aniskip_segments(&get(url).await?, duration))
        };
        match lookup {
            SkipLookup::IntroDb { imdb, season, episode } => {
                let mut url = url::Url::parse(INTRODB)?;
                url.query_pairs_mut().append_pair("imdb_id", &imdb).append_pair("season", &season.to_string()).append_pair("episode", &episode.to_string());
                Ok(introdb_segments(&get(url).await?, duration))
            }
            SkipLookup::Mal { id, episode } => aniskip(id, episode).await,
            SkipLookup::Kitsu { id, episode } => {
                let mut url = url::Url::parse(KITSU_API)?;
                url.path_segments_mut().map_err(|_| anyhow!("bad url"))?.extend([id.to_string(), "mappings".into()]);
                url.query_pairs_mut().append_pair("filter[externalSite]", "myanimelist/anime");
                match mal_of(&get(url).await?) { Some(mal) => aniskip(mal, episode).await, None => Ok(Vec::new()) }
            }
        }
    }).await;
    match found { Ok(Ok(list)) => list, _ => Vec::new() }
}

/// The MyAnimeList id in a Kitsu mappings answer.
pub fn mal_of(v: &Value) -> Option<u64> {
    v["data"].as_array()?.iter().find(|m| m["attributes"]["externalSite"] == "myanimelist/anime")
        .and_then(|m| m["attributes"]["externalId"].as_str()?.parse().ok())
}

/// A segment from a database, kept only when it fits inside this video (an entry for another cut
/// of the episode can run past its end).
fn online_segment(kind: crate::engine::SegmentKind, start: f64, end: f64, duration: f64) -> Option<crate::engine::Segment> {
    let end = if duration > 0.0 && end > duration { if end - duration <= 5.0 { duration } else { return None } } else { end };
    (start >= 0.0 && end - start >= 3.0).then_some(crate::engine::Segment { kind, start, end })
}

pub fn introdb_segments(v: &Value, duration: f64) -> Vec<crate::engine::Segment> {
    use crate::engine::SegmentKind::*;
    [("recap", Recap), ("intro", Intro), ("outro", Credits)].into_iter().filter_map(|(key, kind)| {
        let g = &v[key];
        let start = g["start_ms"].as_f64().map(|ms| ms / 1000.0).or_else(|| g["start_sec"].as_f64())?;
        // Credits to the end can be stored without an end.
        let end = g["end_ms"].as_f64().map(|ms| ms / 1000.0).or_else(|| g["end_sec"].as_f64())
            .or((kind == Credits && duration > 0.0).then_some(duration))?;
        online_segment(kind, start, end, duration)
    }).collect()
}

pub fn aniskip_segments(v: &Value, duration: f64) -> Vec<crate::engine::Segment> {
    use crate::engine::SegmentKind::*;
    let mut out: Vec<crate::engine::Segment> = v["results"].as_array().into_iter().flatten().filter_map(|r| {
        let kind = match r["skipType"].as_str()? { "op" | "mixed-op" => Intro, "ed" | "mixed-ed" => Credits, "recap" => Recap, _ => return None };
        online_segment(kind, r["interval"]["startTime"].as_f64()?, r["interval"]["endTime"].as_f64()?, duration)
    }).collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// A subtitle file an add-on offers for the playing video.
#[derive(Clone, Debug, PartialEq)]
pub struct OnlineSubtitle {
    pub id: String,
    pub url: String,
    /// The add-on's language code, verbatim ("ita", "pob").
    pub code: String,
    /// The language in English ("Italian"), or the code when it is not known.
    pub language: String,
    /// The file or release it was made for, as the add-on names it (helps pick one in sync).
    pub release: Option<String>,
}

/// Every subtitle the add-ons offer for one video: the preferred language first, then English,
/// then the rest by name; the add-on's own order is kept within a language.
pub async fn online_subtitles(kind: &str, video_id: &str) -> Result<Vec<OnlineSubtitle>> {
    let v = exec(json!({"op":"subtitles","type":kind,"id":video_id})).await?;
    let mut list = v["subtitles"].as_array().cloned().unwrap_or_default();
    if list.is_empty() && video_id.starts_with("tt") {
        list = opensubtitles(kind, video_id).await?;
    }
    if list.is_empty() && v["errors"].as_array().is_some_and(|e| !e.is_empty()) {
        anyhow::bail!("Subtitle providers could not be reached");
    }
    let preferred = stream_language().map(|k| subtitle_language_of(&k));
    Ok(rank_subtitles(list.iter().filter_map(subtitle_of).collect(), preferred.as_deref()))
}

async fn opensubtitles(kind: &str, video_id: &str) -> Result<Vec<Value>> {
    let mut url = url::Url::parse(OPENSUBTITLES)?;
    url.path_segments_mut().map_err(|_| anyhow!("Invalid subtitle add-on"))?.extend(["subtitles", kind, &format!("{video_id}.json")]);
    let body: Value = on_runtime(async move {
        backend_http().get(url).send().await?.error_for_status()?.json::<Value>().await
    }).await?.context("Subtitle add-on unavailable")?;
    Ok(body["subtitles"].as_array().cloned().unwrap_or_default())
}

/// The subtitle-list name of an audio-language setting (`crate::addons::LANGUAGES` key): the two
/// lists name a few languages differently.
pub fn subtitle_language_of(key: &str) -> String {
    match key {
        "latino" => "Spanish (Latin America)".into(),
        other => crate::addons::language(other).map(|l| l.label.to_owned()).unwrap_or_default(),
    }
}

fn subtitle_of(v: &Value) -> Option<OnlineSubtitle> {
    let url = v["url"].as_str()?.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) { return None; }
    let code = v["lang"].as_str().unwrap_or("").trim().to_owned();
    let language = Some(crate::engine::language_name(&code)).filter(|l| !l.is_empty()).unwrap_or_else(|| "Unknown".into());
    let id = v["id"].as_str().map(str::to_owned).or_else(|| v["id"].as_i64().map(|n| n.to_string())).unwrap_or_else(|| url.to_owned());
    let release = ["subtitleFileName", "movieReleaseName"].iter().find_map(|k| v[*k].as_str().map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned));
    Some(OnlineSubtitle { id, url: url.to_owned(), code, language, release })
}

pub fn rank_subtitles(mut list: Vec<OnlineSubtitle>, preferred: Option<&str>) -> Vec<OnlineSubtitle> {
    let mut seen = HashSet::new();
    list.retain(|s| seen.insert(s.url.clone()));
    // A stable sort keeps the add-on's order within one language.
    list.sort_by_key(|s| (Some(s.language.as_str()) != preferred, s.language != "English", s.language == "Unknown", s.language.clone()));
    list
}

/// Subtitles whose language name or code starts a word with the query ("ita", "port", "pob").
pub fn subtitle_matches(s: &OnlineSubtitle, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() { return true; }
    s.code.to_lowercase() == q || s.language.to_lowercase().split([' ', '(', ')', ',']).any(|w| w.starts_with(&q)) || s.language.to_lowercase().starts_with(&q)
}

/// Download one subtitle into the cache and return the local file for the player; a file the
/// server says not to keep (or one Veylo can't read, like ASS) is handed to the player as a link.
pub async fn fetch_subtitle(url: &str) -> String {
    // Plain http needs consent the core doesn't have for subtitle links: such a link isn't
    // cached, and goes to the player as offered.
    match exec(json!({"op":"subtitle_file","url":url})).await {
        Ok(v) => v["cache_path"].as_str().map(str::to_owned).unwrap_or_else(|| url.to_owned()),
        Err(_) => url.to_owned(),
    }
}

/// Size of the one file that plays: a season pack's size is split per episode. Some add-ons
/// (AIOStreams) declare the whole torrent's size as the file's; a declared size that matches
/// the labelled pack size (or has no label to compare with) is split too, a different one is
/// the file's own.
fn episode_size(src: &Source, pack_episodes: Option<usize>) -> Option<u64> {
    let size = src.size_bytes?;
    let Some(n) = pack_episodes.filter(|n| src.pack && *n > 1) else { return Some(size) };
    if src.declared_size {
        if let Some(label) = src.labelled_bytes.filter(|l| *l > 0) {
            if (size as f64 - label as f64).abs() / label as f64 > 0.1 { return Some(size); }
        }
    }
    Some(size / n as u64)
}

/// Play order: 2D direct sources of 1080p or more only (unlabelled ones after every labelled one);
/// HDR before SDR, then biggest file (per episode for season packs), then 4K > 1440p > 1080p,
/// remux > WEB-DL > WEBRip > other, HDR10+ > HDR > SDR, then AV1 > H.265 > H.264.
/// Dolby-Vision-only files (usually profile 5, wrong colours without a DV decoder) come last;
/// AI upscales and SDR regraded to HDR after every native file.
/// Torrents, external links and anything under 1080p never.
pub fn play_order(sources: &[Source], pack_episodes: Option<usize>) -> Vec<Source> {
    let language = stream_language();
    play_order_for(sources, pack_episodes, language.as_deref())
}

static STREAM_LANGUAGE: Mutex<Option<String>> = Mutex::new(None);
fn set_stream_language(language: Option<String>) {
    if let Ok(mut l) = STREAM_LANGUAGE.lock() { *l = language; }
}

/// The preferred audio language (a `crate::addons::LANGUAGES` key), None for any.
pub fn stream_language() -> Option<String> { STREAM_LANGUAGE.lock().ok().and_then(|l| l.clone()) }

/// The language codes a file's tracks use for an audio-language setting, in mpv's `alang`
/// order (ISO 639-1, then -2/B and -2/T).
pub fn track_codes(key: &str) -> &'static [&'static str] {
    match key {
        "english" => &["en", "eng"], "italian" => &["it", "ita"], "spanish" => &["es", "spa"],
        "latino" => &["es-419", "es-mx", "es", "spa"], "french" => &["fr", "fre", "fra"], "german" => &["de", "ger", "deu"],
        "portuguese" => &["pt", "por", "pt-br", "pob"], "dutch" => &["nl", "dut", "nld"], "polish" => &["pl", "pol"],
        "russian" => &["ru", "rus"], "ukrainian" => &["uk", "ukr"], "turkish" => &["tr", "tur"], "greek" => &["el", "gre", "ell"],
        "swedish" => &["sv", "swe"], "danish" => &["da", "dan"], "norwegian" => &["no", "nor", "nob", "nb"],
        "finnish" => &["fi", "fin"], "czech" => &["cs", "cze", "ces"], "hungarian" => &["hu", "hun"],
        "romanian" => &["ro", "rum", "ron"], "arabic" => &["ar", "ara"], "hebrew" => &["he", "heb"], "hindi" => &["hi", "hin"],
        "japanese" => &["ja", "jpn"], "korean" => &["ko", "kor"], "chinese" => &["zh", "chi", "zho"],
        _ => &[],
    }
}

/// Whether a track's language tag is the setting's language ("ita", "it", "it-IT" for Italian).
/// Also for subtitle add-on codes: "pob" is Portuguese, "spl" Latin-American Spanish.
pub fn track_in_language(code: &str, key: &str) -> bool {
    let code = code.trim().to_lowercase();
    if code.is_empty() { return false; }
    let base = code.split(['-', '_']).next().unwrap_or("");
    let named = crate::engine::language_name(&code);
    track_codes(key).iter().any(|c| *c == code || (!c.contains('-') && *c == base && key != "latino") || (key == "latino" && code.starts_with("es-") && code != "es-es"))
        || named == subtitle_language_of(key)
        || (key == "portuguese" && named == "Portuguese (Brazil)")
}

/// `play_order` with a preferred audio language, in three tiers, each in the usual order:
/// sources tagged with it (flag, name or release tag, multi-audio included); then untagged or
/// multi-language ones; then those tagged only with other languages (an Italian-only release
/// when you chose English). English is mostly untagged, so for English the first two are one.
/// Within a tier: Real-Debrid sources first, then the free web add-ons' (NoTorrent, Absolute
/// Cinema); a free web link marked as a trial goes last, as it may stop partway.
pub fn play_order_for(sources: &[Source], pack_episodes: Option<usize>, language: Option<&str>) -> Vec<Source> {
    let lang = language.and_then(crate::addons::language);
    let mut out: Vec<Source> = sources.iter().filter(|s| playable(s)).cloned().collect();
    out.sort_by_cached_key(|s| order_key(s, episode_size(s, pack_episodes).unwrap_or(0), lang));
    out
}

/// What Play may choose: a direct 2D link, not labelled under 1080p (nor an unlabelled DVD rip, TV capture or cam).
fn playable(s: &Source) -> bool {
    let low = match s.quality { Some(q) => quality_rank(Some(q)) > 2, None => low_source().is_match(&s.label) };
    s.kind == SourceKind::Direct && !is_3d(&s.label) && !low
}

type OrderKey = (bool, u8, u8, bool, bool, bool, std::cmp::Reverse<(bool, u64, u8, i16, u8, u8)>);

/// The Play-order sort key of one source whose file is `size` bytes: smaller plays first.
fn order_key(s: &Source, size: u64, lang: Option<&crate::addons::Language>) -> OrderKey {
    let tier = match lang {
        None => 0,
        Some(l) if l.matches(&s.label) => 0,
        Some(l) if other_language_only(&s.label, l.key) => 2,
        Some(l) => u8::from(l.key != "english"),
    };
    let provider = if !crate::addons::is_free_addon(&s.addon) { 0 } else if s.label.to_uppercase().contains("FREE TRIAL") { 2 } else { 1 };
    let hdr = hdr_tier(&s.label);
    // Dolby Vision alone ranks below SDR: without a DV decoder its colours are wrong.
    let hdr_rank = match hdr { 1 => 0, 0 => 1, t => t };
    // An upscale or regrade goes after every native file: its 4K and HDR are made up, its size padding.
    (s.off_title, tier, provider, hdr == 1, upscaled().is_match(&s.label), s.quality.is_none(), std::cmp::Reverse((hdr >= 2, size,
        5 - quality_rank(s.quality), source_tier(&s.label).map_or(-1, i16::from), hdr_rank, codec_tier(&s.label))))
}

/// The source's label names the Audio-language setting's language (flag, name or release tag).
pub fn claims_language(s: &Source) -> bool {
    stream_language().and_then(|l| crate::addons::language(&l)).is_some_and(|l| l.matches(&s.label))
}

/// A label tagged with some language other than `key`, and not as a multi-language release.
fn other_language_only(label: &str, key: &str) -> bool {
    !multi_audio().is_match(label) && crate::addons::LANGUAGES.iter().any(|l| l.key != key && l.matches(label))
}

/// The automatic choice: the source that plays, the others to try next ("Another source"),
/// and why nothing played when nothing did.
/// `audio_known`: the chosen file's own tracks said whether it has your language (the player
/// need not check again).
#[derive(Clone, Debug, Default)]
pub struct Pick { pub best: Option<Source>, pub rest: Vec<Source>, pub debrid: Option<String>, pub audio_known: bool }

/// Probe this many different files at once (the first round), then smaller rounds while an
/// unprobed one could still beat the best that works: at most `PROBE_MAX` files and about
/// `PROBE_BUDGET` in all.
const PROBE_FIRST: usize = 12;
const PROBE_NEXT: usize = 6;
const PROBE_MAX: usize = 20;
const PROBE_BUDGET: Duration = Duration::from_millis(2500);
/// One probe's limit, and how long the others in a round get once one source is known to work.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const PROBE_GRACE: Duration = Duration::from_millis(1200);

/// Play's automatic choice for one video: every add-on's sources (not waiting for one that is
/// slow or down), the Play order, then the probe. `title` (and a film's `year`) puts releases
/// of other titles last.
pub async fn autoplay(kind: &str, video_id: &str, pack_episodes: Option<usize>, title: &str, year: Option<i32>) -> Pick {
    let started = std::time::Instant::now();
    dev_log("autoplay: start");
    // While slower add-ons answer, the first ones' best files are checked already (the checks are
    // kept by link, so the final round reuses or joins them).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<Source>>();
    let (early_title, early_kind) = (title.to_owned(), kind.to_owned());
    let early = async move {
        while let Some(mut batch) = rx.recv().await {
            for s in &mut batch { s.off_title = !names_title(s, &early_title, year); }
            let order = play_order(&batch, pack_episodes);
            dev_log(&format!("autoplay: early round on {} {} sources", order.len(), early_kind));
            if !order.is_empty() { let _ = pick_source(order).await; }
        }
    };
    let fetch = play_sources(kind, video_id, Some(tx));
    tokio::pin!(fetch, early);
    let mut list = tokio::select! { biased; list = &mut fetch => list, _ = &mut early => fetch.await };
    for s in &mut list { s.off_title = !names_title(s, title, year); }
    let order = play_order(&list, pack_episodes);
    let listed = started.elapsed().as_millis();
    let pick = pick_source(order.clone()).await;
    dev_log(&format!("autoplay: {} sources, {} playable in {listed} ms; chose {} after {} ms",
        list.len(), order.len(), pick.best.as_ref().map(source_summary).unwrap_or_else(|| "nothing".into()), started.elapsed().as_millis()));
    pick
}

/// Play's source lists, fetched ahead (`prefetch_sources`) or by Play itself, kept for a while:
/// Play joins a fetch already under way instead of asking the add-ons again.
type Ahead = std::sync::Arc<tokio::sync::OnceCell<Vec<Source>>>;
static AHEAD: OnceLock<Mutex<HashMap<String, (std::time::Instant, Ahead)>>> = OnceLock::new();
const AHEAD_TTL: Duration = Duration::from_secs(10 * 60);

async fn play_sources(kind: &str, video_id: &str, early: Option<tokio::sync::mpsc::UnboundedSender<Vec<Source>>>) -> Vec<Source> {
    let cell = {
        let Ok(mut map) = AHEAD.get_or_init(Default::default).lock() else { return Vec::new() };
        // Expired lists, and fetches that found nothing (an add-on may have been down), are asked again.
        map.retain(|_, (at, cell)| at.elapsed() < AHEAD_TTL && cell.get().is_none_or(|v| !v.is_empty()));
        map.entry(format!("{kind}/{video_id}")).or_insert_with(|| (std::time::Instant::now(), Ahead::default())).1.clone()
    };
    let (kind, id) = (kind.to_owned(), video_id.to_owned());
    cell.get_or_init(|| async move { sources_within(&kind, &id, Some(ADDON_GRACE), early).await.map(|r| r.0).unwrap_or_default() }).await.clone()
}

/// Ask the add-ons for a video's sources before Play is pressed (a title page or Tonight's pick is
/// open), so Play only has to check them. Nothing is resolved on the debrid service yet.
pub fn prefetch_sources(kind: &str, video_id: &str) {
    let (kind, id) = (kind.to_owned(), video_id.to_owned());
    if let Some(runtime) = RUNTIME.get() { runtime.spawn(async move { play_sources(&kind, &id, None).await; }); }
}

/// Developer option: Play's automatic choice for each "kind/id/title/year" line, one after the
/// other, nothing played: cold (sources asked for then), then again with the sources fetched
/// ahead as a title page does. Times and picks go to the developer log.
pub async fn dev_autoplay(list: String) {
    for pass in ["cold", "ahead"] {
        for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let parts: Vec<&str> = line.splitn(4, '/').collect();
            let [kind, id, ..] = parts[..] else { continue };
            let (name, year) = (parts.get(2).copied().unwrap_or(""), parts.get(3).and_then(|y| y.parse().ok()));
            dev_log(&format!("bench {pass} {name}"));
            let t = std::time::Instant::now();
            let pick = autoplay(kind, id, (kind == "series").then_some(10), name, year).await;
            dev_log(&format!("bench {pass} {name}: {} ms -> {}", t.elapsed().as_millis(), pick.best.as_ref().map(source_summary).unwrap_or_else(|| "nothing".into())));
        }
    }
}

/// "4K HDR 58.2 GiB REMUX (Torrentio)": what the log says about a source; never its link.
pub(crate) fn source_summary(s: &Source) -> String {
    format!("{} {} {} ({})", s.quality.unwrap_or("?"), s.hdr.unwrap_or("SDR"), s.size.as_deref().unwrap_or("size?"), s.addon)
}

/// One line in the developer log (logcat "veylo" on Android, stderr elsewhere). Never URLs or keys.
pub(crate) fn dev_log(message: &str) {
    #[cfg(target_os = "android")]
    crate::android::log(message);
    #[cfg(not(target_os = "android"))]
    eprintln!("{message}");
}

/// The best candidates are probed together (the relay's one-byte check), one link per file, and
/// the ones that answer are ranked again by the size their server reports: add-ons often declare
/// a whole pack's or collection's size as the file's. A round stops waiting once no pending
/// candidate could outrank a working one, or `PROBE_GRACE` after the first works; another round
/// runs while the best unprobed candidate (by its declared size) could still win. Debrid status
/// clips report why nothing worked.
pub async fn pick_source(candidates: Vec<Source>) -> Pick {
    let lang = stream_language().and_then(|l| crate::addons::language(&l));
    let started = std::time::Instant::now();
    // The same file from several scrapers: probe one of its links.
    let file = |s: &Source| if s.filename.is_empty() { s.url.clone() } else { format!("{}|{:?}", s.filename.to_lowercase(), s.size_bytes) };
    let mut probed_files: HashSet<String> = HashSet::new();
    let mut probed_urls: HashSet<String> = HashSet::new();
    let mut working: Vec<Source> = Vec::new();
    let mut verdicts: Vec<AudioVerdict> = Vec::new();
    let mut debrid = None;
    let mut cursor = 0;
    loop {
        let take = if probed_files.is_empty() { PROBE_FIRST } else { PROBE_NEXT }.min(PROBE_MAX - probed_files.len());
        let mut batch = Vec::new();
        while cursor < candidates.len() && batch.len() < take {
            let c = &candidates[cursor];
            cursor += 1;
            if probed_files.insert(file(c)) { probed_urls.insert(c.url.clone()); batch.push(c.clone()); }
        }
        if batch.is_empty() { break; }
        // Later rounds only run while something works: each gets `PROBE_GRACE`, within the budget.
        let cap = (!working.is_empty()).then(|| tokio::time::Instant::from_std((started + PROBE_BUDGET).min(std::time::Instant::now() + PROBE_GRACE)));
        let (found, status) = probe_batch(batch, lang, cap).await;
        if status.is_some() { debrid = status; }
        for (src, verdict) in found { working.push(src); verdicts.push(verdict); }
        let Some(best) = working.iter().map(|s| order_key(s, s.size_bytes.unwrap_or(0), lang)).min() else { continue };
        // Candidates are in Play order by their declared sizes, which are at least the real ones;
        // a round is worth it only for a file at least a tenth bigger, or of a better kind.
        let next = candidates[cursor..].iter().find(|c| !probed_files.contains(&file(c)));
        let could_win = next.is_some_and(|c| order_key(c, c.size_bytes.unwrap_or(0) / 10 * 9, lang) < best)
            || (started.elapsed() < PROBE_GRACE && rivals_wanted(&working, lang).is_some_and(|class| candidates[cursor..].iter().take(PROBE_NEXT * 2).any(|c| remux_class(c) == Some(class))));
        if !could_win || probed_files.len() >= PROBE_MAX || started.elapsed() >= PROBE_BUDGET { break; }
    }
    if working.is_empty() { return Pick { best: None, rest: Vec::new(), debrid, audio_known: false }; }
    let mut paired: Vec<(Source, AudioVerdict)> = working.into_iter().zip(verdicts).collect();
    paired.sort_by_cached_key(|(s, _)| order_key(s, s.size_bytes.unwrap_or(0), lang));
    dev_log(&format!("checked {} working files in {} ms, audio: {}", paired.len(), started.elapsed().as_millis(),
        paired.iter().map(|(_, v)| match v { Some((true, true)) => "main", Some((true, false)) => "yes", Some((false, _)) => "no", None => "?" }).collect::<Vec<_>>().join(" ")));
    // The files' own audio tracks: one in your language first, then those that can't tell, then
    // the rest, each in the Play order.
    let (working, verdicts): (Vec<Source>, Vec<AudioVerdict>) = paired.into_iter().unzip();
    let ranked = rank_by_audio(working, verdicts);
    let audio_known = ranked[0].0.is_some();
    let mut working: Vec<Source> = ranked.into_iter().map(|(_, s)| s).collect();
    let mut rest = working.split_off(1);
    rest.extend(candidates.into_iter().filter(|c| !probed_urls.contains(&c.url)));
    Pick { best: working.pop(), rest, debrid: None, audio_known }
}

/// What a file's tracks say about the Audio-language setting: (it has that language, its main
/// track is in it); None when unread or unclear.
type AudioVerdict = Option<(bool, bool)>;

/// Working sources (in the Play order) ranked again by their own audio: files with your language,
/// then those that can't tell, then the rest. Among remuxes of the same resolution and HDR (the
/// same picture: their sizes differ by the dubs they carry) the one whose main track is in your
/// language, the original release, goes first.
fn rank_by_audio(working: Vec<Source>, verdicts: Vec<AudioVerdict>) -> Vec<(AudioVerdict, Source)> {
    let mut groups: Vec<usize> = Vec::new();
    for (i, s) in working.iter().enumerate() {
        let same = remux_class(s).and_then(|c| working[..i].iter().position(|o| remux_class(o) == Some(c)));
        groups.push(same.unwrap_or(i));
    }
    let mut ranked: Vec<(usize, AudioVerdict, Source)> = groups.into_iter().zip(verdicts).zip(working).map(|((g, v), s)| (g, v, s)).collect();
    ranked.sort_by_key(|(group, v, _)| {
        let tier = match v { Some((true, _)) => 0, None => 1, Some((false, _)) => 2 };
        (tier, *group, !matches!(v, Some((_, true))))
    });
    ranked.into_iter().map(|(_, v, s)| (v, s)).collect()
}

/// A remux's picture class (resolution, HDR, Dolby Vision only, upscaled): remuxes of one class
/// carry the same picture. None for encodes, whose size is their bitrate.
type RemuxClass = (Option<&'static str>, bool, bool, bool);
fn remux_class(s: &Source) -> Option<RemuxClass> {
    (source_tier(&s.label) == Some(4)).then(|| (s.quality, hdr_tier(&s.label) >= 2, hdr_tier(&s.label) == 1, upscaled().is_match(&s.label)))
}

/// Working remuxes of the best one's class wanted before probing stops, so their audio can decide.
const REMUX_RIVALS: usize = 3;

/// The best working source's remux class while fewer than `REMUX_RIVALS` of it work.
fn rivals_wanted(working: &[Source], lang: Option<&crate::addons::Language>) -> Option<RemuxClass> {
    let best = working.iter().min_by_key(|s| order_key(s, s.size_bytes.unwrap_or(0), lang))?;
    let class = remux_class(best)?;
    (working.iter().filter(|s| remux_class(s) == Some(class)).count() < REMUX_RIVALS).then_some(class)
}

/// How long a working file's header may take to read.
const AUDIO_TIMEOUT: Duration = Duration::from_millis(1500);

/// What checking one link found: it works (and its real size, debrid status clip, audio tracks
/// and the link its redirects end at).
#[derive(Clone, Debug)]
struct ProbeOutcome { ok: bool, status: u64, bytes: Option<u64>, debrid: Option<String>, tracks: Option<Vec<crate::tracks::AudioTrack>>, resolved: Option<String> }

/// Checks by link, for a while: a check under way is joined, not repeated (Play's early round
/// on the first add-on's sources, the final one, "Another source"). Only links that work are
/// kept: a status clip (often a passing refusal: a burst of requests, the debrid service busy),
/// a timeout or a network error is tried again next time.
type Probed = std::sync::Arc<tokio::sync::OnceCell<ProbeOutcome>>;
static PROBED: OnceLock<Mutex<HashMap<String, (std::time::Instant, Probed)>>> = OnceLock::new();
const PROBED_TTL: Duration = Duration::from_secs(5 * 60);
/// Checks under way at once for one add-on's links.
const PROBES_PER_ADDON: usize = 12;
/// The pause before checking a link again that answered "too many requests".
const RETRY_AFTER_429: Duration = Duration::from_millis(1200);
static PROBE_GATES: OnceLock<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Semaphore>>>> = OnceLock::new();

/// The relay's one-byte check of a link, then (when it works) its header for the audio tracks.
async fn probe_one(src: Source) -> Option<ProbeOutcome> {
    let cell = {
        let mut map = PROBED.get_or_init(Default::default).lock().ok()?;
        map.retain(|_, (at, _)| at.elapsed() < PROBED_TTL);
        map.entry(src.url.clone()).or_insert_with(|| (std::time::Instant::now(), Probed::default())).1.clone()
    };
    match cell.get_or_try_init(|| async move {
        // A few at a time per add-on: a burst of resolves is what debrid services refuse.
        let gate = {
            let mut gates = PROBE_GATES.get_or_init(Default::default).lock().map_err(|_| None)?;
            gates.entry(src.addon.clone()).or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(PROBES_PER_ADDON))).clone()
        };
        let _turn = gate.acquire_owned().await.map_err(|_| None)?;
        let headers = Value::Object(src.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
        let check = || tokio::time::timeout(PROBE_TIMEOUT, backend().proxy.probe(&src.url, &headers, false));
        let mut probe = check().await.map_err(|_| None)?.map_err(|_| None)?;
        // "Too many requests": once more after a pause, as such a refusal passes.
        if probe["status"] == 429 {
            tokio::time::sleep(RETRY_AFTER_429).await;
            probe = check().await.map_err(|_| None)?.map_err(|_| None)?;
        }
        let ok = probe["ok"] == true;
        let (tracks, resolved) = if ok { tokio::time::timeout(AUDIO_TIMEOUT, file_audio(src)).await.unwrap_or_default() } else { (None, None) };
        let outcome = ProbeOutcome { ok, status: probe["status"].as_u64().unwrap_or(0), bytes: probe["bytes"].as_u64(), debrid: probe["debrid_status"].as_str().map(str::to_owned), tracks, resolved };
        if ok { Ok(outcome) } else { Err(Some(outcome)) }
    }).await {
        Ok(found) => Some(found.clone()),
        Err(answer) => answer,
    }
}

/// The audio tracks a file's header declares, read from its first bytes (stopping as soon as
/// they are known), and the https link the source's redirects end at.
async fn file_audio(src: Source) -> (Option<Vec<crate::tracks::AudioTrack>>, Option<String>) {
    let mut req = backend_http().get(&src.url).header("range", format!("bytes=0-{}", crate::tracks::HEAD_BYTES - 1));
    for (k, v) in &src.headers { req = req.header(k.as_str(), v.as_str()); }
    let Ok(mut response) = req.send().await else { return (None, None) };
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT { return (None, None); }
    let resolved = Some(response.url().to_string()).filter(|u| u.starts_with("https://") && *u != src.url);
    let mut head = Vec::new();
    while head.len() < crate::tracks::HEAD_BYTES {
        let Ok(Some(chunk)) = response.chunk().await else { break };
        head.extend_from_slice(&chunk);
        if let Some(tracks) = crate::tracks::audio_tracks(&head) { return (Some(tracks), resolved); }
    }
    (crate::tracks::audio_tracks(&head), resolved)
}

/// The batch's working sources, each with the size its server reports, and a debrid status seen.
async fn probe_batch(batch: Vec<Source>, lang: Option<&'static crate::addons::Language>, cap: Option<tokio::time::Instant>) -> (Vec<(Source, AudioVerdict)>, Option<String>) {
    on_runtime(async move {
        let mut jobs = tokio::task::JoinSet::new();
        for (i, src) in batch.iter().enumerate() {
            let src = src.clone();
            jobs.spawn(async move {
                let outcome = probe_one(src).await;
                let audio = lang.zip(outcome.as_ref().and_then(|o| o.tracks.clone()))
                    .and_then(|(l, t)| crate::tracks::in_language(&t, l.key).map(|has| (has, crate::tracks::primary_in_language(&t, l.key))));
                (i, outcome, audio)
            });
        }
        let estimated: Vec<OrderKey> = batch.iter().map(|s| order_key(s, s.size_bytes.unwrap_or(0), lang)).collect();
        let mut pending: HashSet<usize> = (0..batch.len()).collect();
        let mut working: Vec<Source> = Vec::new();
        let mut verdicts: Vec<AudioVerdict> = Vec::new();
        let mut debrid = None;
        let mut deadline: Option<tokio::time::Instant> = cap;
        let started = std::time::Instant::now();
        while !pending.is_empty() {
            let next = match deadline {
                Some(at) => match tokio::time::timeout_at(at, jobs.join_next()).await { Ok(n) => n, Err(_) => break },
                None => jobs.join_next().await,
            };
            let Some(Ok((i, outcome, audio))) = next else { if next.is_none() { break } else { continue } };
            pending.remove(&i);
            if std::env::var_os("VEYLO_PROBE_TRACE").is_some() {
                let what = outcome.as_ref().map(|o| format!("ok={} status={} bytes={:?}", o.ok, o.status, o.bytes)).unwrap_or_else(|| "error or timeout".into());
                dev_log(&format!("  probe #{i} {} {:.60} est {} -> {what} at {} ms", batch[i].hdr.unwrap_or("SDR"), batch[i].filename, batch[i].size.as_deref().unwrap_or("?"), started.elapsed().as_millis()));
            }
            let Some(result) = outcome else { continue };
            if !result.ok {
                if let Some(status) = result.debrid { debrid = Some(status); }
                else if result.status == 429 && debrid.is_none() { debrid = Some("busy".into()); }
                continue;
            }
            let mut src = batch[i].clone();
            // The player opens the link the add-on's resolve led to: no second resolve on Play.
            if let Some(url) = result.resolved { src.url = url; }
            if let Some(bytes) = result.bytes.filter(|b| *b > 0) {
                // The server's size is the file's own: no pack share, no label.
                src.size_bytes = Some(bytes);
                src.size = Some(binary_size(bytes as f64));
                src.declared_size = true;
                src.labelled_bytes = None;
                src.pack = false;
            }
            working.push(src);
            verdicts.push(audio);
            let grace = tokio::time::Instant::now() + PROBE_GRACE;
            if working.len() == 1 { deadline = Some(deadline.map_or(grace, |d| d.min(grace))); }
            // A pending source's real size is at most its declared one: if none could beat the
            // best working source even at that size, the answer is known.
            let best = working.iter().map(|s| order_key(s, s.size_bytes.unwrap_or(0), lang)).min().expect("one works");
            let wanted = rivals_wanted(&working, lang);
            if pending.iter().all(|p| estimated[*p] >= best && (wanted.is_none() || remux_class(&batch[*p]) != wanted)) { break; }
        }
        (working.into_iter().zip(verdicts).collect(), debrid)
    }).await.unwrap_or_default()
}

/// What to say when no checked source worked (the Android app's wording).
pub fn no_source_message(debrid: Option<&str>) -> &'static str {
    match debrid {
        Some("downloading") => "Your debrid service is still downloading this file. Try again later or choose another source.",
        Some("failed") => "Your debrid service could not provide this file. Choose another source.",
        Some("busy") => "Your sources are refusing requests for a moment (too many at once). Try again in a minute.",
        _ => "No source responded. Try again or choose another source.",
    }
}

// ---------------------------------------------------------------- picks

pub struct Picks {
    pub items: Vec<Media>,
}

pub async fn picks() -> Result<Picks> {
    let v = exec(json!({"op":"recommendations","limit":100})).await?;
    Ok(Picks {
        items: metas(&v, "").into_iter().filter(|m| m.liked == 0).collect(),
    })
}

/// -1 not for me, 0 clear, 1 like. The title's metadata travels with it so the
/// core can rank against it later.
pub async fn feedback(media: &Media, value: i64) -> Result<()> {
    exec(json!({"op":"feedback","type":media.kind,"id":media.id,"addon_url":media.addon_url,
        "value":value,"meta":media.raw})).await?;
    Ok(())
}

/// For a later film in a collection: the first released part you have not watched, when it
/// comes before this one (a Cinemeta title, ready for Picks). None when this film is where to
/// start, or for anything that is not part of a collection.
pub async fn earlier_part(m: &Media, watched: &HashSet<String>) -> Option<Media> {
    if m.kind != "movie" { return None; }
    let Artwork { collection: Some(cid), tmdb: Some((_, own)), .. } = artwork(m).await? else { return None };
    let c = tmdb(format!("collection/{cid}"), vec![]).await?;
    let today = { let (y, mo, d, _) = civil(now_secs()); format!("{y:04}-{mo:02}-{d:02}") };
    let mut parts: Vec<&Value> = c["parts"].as_array()?.iter()
        .filter(|p| p["release_date"].as_str().is_some_and(|d| !d.is_empty() && d <= today.as_str())).collect();
    parts.sort_by_key(|p| p["release_date"].as_str().unwrap_or_default().to_owned());
    let at = parts.iter().position(|p| p["id"].as_i64() == Some(own))?;
    for p in parts.iter().take(at) {
        // A part TMDB can't place (no id, no IMDb id) is skipped, not the end of the search.
        let Some(id) = p["id"].as_i64() else { continue };
        let Some(imdb) = tmdb(format!("movie/{id}/external_ids"), vec![]).await.and_then(|e| e["imdb_id"].as_str().map(str::to_owned)).filter(|i| i.starts_with("tt")) else { continue };
        let name = p["title"].as_str().unwrap_or("Untitled");
        let year = p["release_date"].as_str().and_then(|d| d.get(..4)).unwrap_or_default();
        let Some(probe) = Media::from_json(&json!({"id": imdb, "name": name, "releaseInfo": year}), CINEMETA) else { continue };
        let key = crate::picks_rules::title_key(&probe);
        if watched.contains(&imdb) || watched.contains(&key) { continue; }
        let genres = tmdb_genre_list("movie").await;
        let genre_names: Vec<String> = p["genre_ids"].as_array().into_iter().flatten().filter_map(Value::as_i64)
            .filter_map(|g| genres.iter().find(|(x, _)| *x == g).map(|(_, n)| n.clone())).collect();
        let meta = json!({
            "id": imdb, "type": "movie", "name": name, "description": p["overview"].as_str().unwrap_or_default(), "releaseInfo": year,
            "poster": p["poster_path"].as_str().map(|x| format!("https://image.tmdb.org/t/p/w500{x}")).unwrap_or_default(),
            "background": p["backdrop_path"].as_str().map(|x| format!("https://image.tmdb.org/t/p/w1280{x}")).unwrap_or_default(),
            "genres": genre_names, "imdbRating": p["vote_average"].as_f64().map(|v| format!("{v:.1}")).unwrap_or_default(),
        });
        return Media::from_json(&meta, CINEMETA).map(crate::picks_rules::tidy);
    }
    None
}

/// The trailers to play for a Picks card: TMDB's videos (official trailers first) when TMDB has
/// any, the add-on's YouTube trailers after them as alternatives; the add-on's own list (direct
/// links included) only when TMDB has none.
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
    // A catalogue's "Animation" can be wrong; TMDB's genres decide when TMDB knows the title.
    if let Some(art) = artwork(&ready).await.filter(|a| a.tmdb.is_some() && !a.genres.is_empty()) {
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
    for group in v.get("groups").and_then(Value::as_array).into_iter().flatten() {
        if retired_addon(&s(group, "addon_url")) { continue; }
        out.catalogs += 1;
        let addon = s(group, "addon_name");
        if group.get("error").is_some() && !out.failed.contains(&addon) { out.failed.push(addon.clone()); }
        for m in metas(group, &s(group, "addon_url")) {
            if !out.results.iter().any(|(_, r)| r.key() == m.key()) {
                out.results.push((addon.clone(), m));
            }
        }
    }
    out.results = rank_search_results(&query, out.results);
    Ok(out)
}

/// Whether a release names this title: its file name, or a line of its torrent name, starts with
/// the title's words (leading "[site]" and "www.site - " tags and a first "The" aside) and has
/// more after them; for a film, a year right after the title is within one of `year`. A line
/// that is only the title says nothing (some add-ons print the requested title on its own line).
pub fn names_title(s: &Source, title: &str, year: Option<i32>) -> bool {
    fn words(text: &str) -> Vec<String> {
        let mut t = text.trim();
        loop {
            if let Some(rest) = t.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(_, r)| r.trim_start()) { t = rest; continue; }
            if t.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www.")) { if let Some((_, rest)) = t.split_once(" - ") { t = rest.trim_start(); continue; } }
            break;
        }
        let mut w = search_words(t);
        if w.first().is_some_and(|f| f == "the") { w.remove(0); }
        w
    }
    let wanted = words(title);
    if wanted.is_empty() { return true; }
    let file = s.filename.rsplit(['/', '\\']).next().unwrap_or("");
    std::iter::once(file).chain(s.description.lines()).any(|line| {
        let w = words(line);
        if w.len() <= wanted.len() || w[..wanted.len()] != wanted[..] { return false; }
        // What follows the title must not make it another one ("The Dark Knight Rises"): a year
        // (the film's, give or take one), a season or episode, or a release tag.
        let next = w[wanted.len()].as_str();
        match next.parse::<i32>().ok().filter(|y| (1900..2100).contains(y)) {
            Some(got) => year.is_none_or(|want| (want - got).abs() <= 1),
            None => release_word().is_match(next),
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
pub async fn viewing_history() -> Result<Vec<Seen>> {
    let v = exec(json!({"op":"history","limit":600})).await?;
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

// ---------------------------------------------------------------- settings

/// Viewer settings kept in the app's own folder (settings.json).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Rounds of a Picks duel before the final pick.
    pub picks_rounds: u32,
    /// BetterPosters (btttr.cc) first, the add-on's poster as fallback.
    pub better_posters: bool,
    /// TMDB v3 key for artwork; never logged or shown.
    pub tmdb_key: Option<String>,
    /// Picks leaves out titles you finished watching.
    pub picks_hide_watched: bool,
    /// Real-Debrid API token shared by every add-on Veylo configures; never logged or shown.
    pub rd_key: Option<String>,
    /// Preferred audio language for streams (a key of `crate::addons::LANGUAGES`): English unless
    /// chosen (there is no "any language"; settings saved without one read as English).
    pub stream_language: Option<String>,
    /// You removed the Real-Debrid key: it is not adopted again from an add-on's link.
    pub rd_key_removed: bool,
    /// Windows: send HDR to the screen when it is in HDR (off: always tone-map to SDR).
    pub hdr_output: bool,
}

/// The audio language when none was chosen, or the saved one is unknown.
pub const DEFAULT_LANGUAGE: &str = "english";

/// The saved audio language, English when there is none (older settings saved "Any language" as null).
fn audio_language(saved: &Value) -> String {
    saved["stream_language"].as_str().filter(|l| crate::addons::language(l).is_some()).unwrap_or(DEFAULT_LANGUAGE).to_owned()
}

impl Default for Settings { fn default() -> Self { Settings { picks_rounds: 5, better_posters: true, tmdb_key: None, picks_hide_watched: true, rd_key: None, stream_language: Some(DEFAULT_LANGUAGE.into()), rd_key_removed: false, hdr_output: true } } }

pub fn load_settings() -> Settings {
    let v: Value = std::fs::read_to_string(backend().data_dir.join("settings.json")).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    Settings {
        picks_rounds: v["picks_rounds"].as_u64().map(|n| n.clamp(3, 15) as u32).unwrap_or(5),
        better_posters: v["better_posters"].as_bool().unwrap_or(true),
        tmdb_key: v["tmdb_key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned),
        picks_hide_watched: v["picks_hide_watched"].as_bool().unwrap_or(true),
        rd_key: v["rd_key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned),
        stream_language: Some(audio_language(&v)),
        rd_key_removed: v["rd_key_removed"].as_bool().unwrap_or(false),
        hdr_output: v["hdr_output"].as_bool().unwrap_or(true),
    }
}

pub fn save_settings(settings: &Settings) {
    let _ = std::fs::write(backend().data_dir.join("settings.json"), json!({"picks_rounds": settings.picks_rounds, "better_posters": settings.better_posters, "tmdb_key": settings.tmdb_key, "picks_hide_watched": settings.picks_hide_watched, "rd_key": settings.rd_key, "stream_language": settings.stream_language, "rd_key_removed": settings.rd_key_removed, "hdr_output": settings.hdr_output}).to_string());
    BETTER_POSTERS.store(settings.better_posters, std::sync::atomic::Ordering::Relaxed);
    HDR_OUTPUT.store(settings.hdr_output, std::sync::atomic::Ordering::Relaxed);
    set_stream_language(settings.stream_language.clone());
    set_tmdb_key(settings.tmdb_key.clone());
}

// ---------------------------------------------------------------- posters

static BETTER_POSTERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static HDR_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Windows: send HDR to an HDR screen (the setting, kept in memory for the player).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn hdr_output() -> bool { HDR_OUTPUT.load(std::sync::atomic::Ordering::Relaxed) }

/// Poster images in order of preference: BetterPosters (by IMDb id, rating and genre tags),
/// then the add-on's own poster (Cinemeta for most titles), then the backdrop. A failing
/// image moves on to the next one.
pub fn posters(m: &Media) -> Vec<String> {
    let imdb = Some(m.id.as_str()).filter(|id| id.starts_with("tt"))
        .or_else(|| m.raw["imdb_id"].as_str().filter(|id| id.starts_with("tt")));
    let mut out = Vec::new();
    if BETTER_POSTERS.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(id) = imdb { out.push(format!("https://btttr.cc/poster/imdb/poster-default/{id}.jpg")); }
    }
    out.push(m.poster.clone());
    // Cinemeta's image service has a poster for nearly every IMDb title.
    if let Some(id) = imdb { out.push(format!("https://images.metahub.space/poster/medium/{id}/img")); }
    out.push(m.background.clone());
    out.retain(|u| !u.is_empty());
    let mut seen = HashSet::new();
    out.retain(|u| seen.insert(u.clone()));
    out
}

// ---------------------------------------------------------------- BetterPosters

/// BetterPosters (btttr.cc) is a public Stremio add-on: Trakt and TMDB charts, metadata and the
/// tagged posters. Veylo reads it directly, with a short timeout, and never installs it: its
/// titles keep Cinemeta as their add-on, so the Library, history and sources stay keyed as
/// before, and Cinemeta answers whenever BetterPosters is slow or down. "Add-on" in Settings
/// turns all of it off.
const BP_BASE: &str = "https://btttr.cc";
/// Home row keys that are BetterPosters charts.
const BP_ROW: &str = "betterposters|";
const BP_CHARTS: [(&str, &str, &str); 5] = [
    ("movie", "trakt-trending", "Trending movies"), ("series", "trakt-trending", "Trending series"),
    ("movie", "trakt-popular", "Popular movies"), ("series", "trakt-popular", "Popular series"),
    ("movie", "tmdb-top", "Top rated movies"),
];

pub fn better_posters_on() -> bool { BETTER_POSTERS.load(std::sync::atomic::Ordering::Relaxed) }

/// Answers by path and when they came: kept 15 minutes, a failure 2 minutes, so a stalled
/// server isn't asked again on every screen.
type Answers = HashMap<String, (i64, Option<Value>)>;
static BP_CACHE: OnceLock<Mutex<Answers>> = OnceLock::new();

async fn bp_get(path: String) -> Option<Value> {
    let cache = BP_CACHE.get_or_init(Default::default);
    let now = now_secs();
    if let Some((at, hit)) = cache.lock().ok().and_then(|c| c.get(&path).cloned()) {
        if now - at < if hit.is_some() { 900 } else { 120 } { return hit; }
    }
    let url = format!("{BP_BASE}/{path}");
    let got = on_runtime(async move {
        let response = backend().http.get(url).timeout(Duration::from_secs(8)).send().await.ok()?.error_for_status().ok()?;
        response.json::<Value>().await.ok()
    }).await.ok().flatten();
    if let Ok(mut c) = cache.lock() { c.insert(path, (now, got.clone())); }
    got
}

/// A chart's titles as Cinemeta titles (`origin`): only IMDb ids, which Cinemeta also serves.
fn bp_titles(v: &Value, origin: &str) -> Vec<Media> {
    v["metas"].as_array().into_iter().flatten()
        .filter(|m| s(m, "id").starts_with("tt"))
        .filter_map(|m| Media::from_json(m, origin))
        .scan(HashSet::new(), |seen, m| Some(seen.insert(m.key()).then_some(m))).flatten().collect()
}

/// BetterPosters' metadata for a film or series with an IMDb id.
async fn bp_meta(m: &Media) -> Option<Value> {
    if !better_posters_on() || !matches!(m.kind.as_str(), "movie" | "series") { return None; }
    let id = imdb_of(m)?;
    bp_get(format!("meta/{}/{id}.json", m.kind)).await?.get("meta").filter(|v| v.is_object()).cloned()
}

fn filled(v: &Value) -> bool {
    !(v.is_null() || v.as_str().is_some_and(|t| t.trim().is_empty()) || v.as_array().is_some_and(Vec::is_empty))
}

/// The add-on's metadata with BetterPosters' laid over it: BetterPosters wins where it has a
/// value, its trailers and credits come first, and the add-on's episode list stays (its ids
/// find sources and keep watched marks) with BetterPosters' titles, stills and synopses on it.
fn merge_meta(mut meta: Value, bp: Option<&Value>) -> Value {
    let Some(bp) = bp.filter(|b| b.is_object()) else { return meta };
    if !meta.is_object() { return bp.clone(); }
    for key in ["logo", "runtime", "imdbRating", "released", "country", "description", "genres", "director", "cast", "writer"] {
        if let Some(v) = bp.get(key).filter(|v| filled(v)) { meta[key] = v.clone(); }
    }
    for key in ["trailers", "trailerStreams", "links"] {
        let mut list: Vec<Value> = bp[key].as_array().cloned().unwrap_or_default();
        list.extend(meta[key].as_array().cloned().unwrap_or_default());
        if !list.is_empty() { meta[key] = Value::Array(list); }
    }
    let episodes: HashMap<String, &Value> = bp["videos"].as_array().into_iter().flatten().map(|e| (s(e, "id"), e)).collect();
    match meta["videos"].as_array_mut().filter(|v| !v.is_empty()) {
        Some(videos) => for e in videos {
            if let Some(b) = episodes.get(&s(e, "id")) {
                for key in ["title", "thumbnail", "overview"] {
                    if let Some(v) = b.get(key).filter(|v| filled(v)) { e[key] = v.clone(); }
                }
            }
        },
        None => if !episodes.is_empty() { meta["videos"] = bp["videos"].clone(); },
    }
    meta
}

static IDENTS: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

/// The studio logos that read as white idents: transparent around the mark. A logo printed on its
/// own solid background would turn into a white box, so it is left out. Kept for the session.
pub async fn idents(studios: Vec<Studio>) -> Vec<Studio> {
    let cache = IDENTS.get_or_init(Default::default);
    let mut out = Vec::new();
    for st in studios {
        let known = cache.lock().ok().and_then(|c| c.get(&st.logo).copied());
        let ok = match known {
            Some(ok) => ok,
            None => {
                let ok = transparent_edges(st.logo.replace("/w185/", "/w92/")).await.unwrap_or(false);
                if let Ok(mut c) = cache.lock() { c.insert(st.logo.clone(), ok); }
                ok
            }
        };
        if ok { out.push(st); }
    }
    out
}

/// Whether the logo is a mark on see-through ground: a transparent corner, and ink over less than
/// about half of it (a solid plate with rounded corners is not an ident).
async fn transparent_edges(url: String) -> Option<bool> {
    let bytes = on_runtime(async move {
        backend().http.get(url).timeout(Duration::from_secs(6)).send().await.ok()?.error_for_status().ok()?.bytes().await.ok()
    }).await.ok().flatten()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 { return None; }
    let corner = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)].iter().any(|&(x, y)| img.get_pixel(x, y)[3] < 128);
    let ink = img.pixels().filter(|p| p[3] >= 128).count() as f64 / (w as f64 * h as f64);
    Some(corner && ink < 0.55)
}

/// Its place in BetterPosters' "today" chart (TMDB's most watched today; 1 is the top), if it is in it.
pub async fn today_rank(m: &Media) -> Option<u64> {
    if !better_posters_on() { return None; }
    let id = imdb_of(m)?;
    let chart = if m.kind == "series" { "catalog/series/tmdb-today-shows.json" } else { "catalog/movie/tmdb-today.json" };
    let v = bp_get(chart.into()).await?;
    v["metas"].as_array()?.iter().find(|x| s(x, "id") == id).and_then(|x| x["_rank"].as_u64())
}

/// Without a TMDB key, BetterPosters lends the same TMDB backdrop, logo and trailers.
async fn bp_artwork(m: &Media) -> Option<Artwork> {
    let meta = bp_meta(m).await?;
    let art = Artwork {
        genres: meta["genres"].as_array().into_iter().flatten().filter_map(Value::as_str).map(crate::picks_rules::canonical_genre).collect(),
        backdrop: Some(s(&meta, "background")).filter(|u| u.starts_with("https://")),
        logo: Some(s(&meta, "logo")).filter(|u| u.starts_with("https://")),
        trailers: meta["trailers"].as_array().into_iter().flatten().map(|t| s(t, "source")).filter(|k| !k.is_empty()).collect(),
        ..Artwork::default()
    };
    (art.backdrop.is_some() || art.logo.is_some()).then_some(art)
}

// ---------------------------------------------------------------- TMDB artwork

/// Wide artwork from TMDB: a text-free backdrop and an English logo. Empty fields fall back
/// to the add-on's (Metahub) images.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Artwork {
    pub genres: Vec<String>,
    pub backdrop: Option<String>, pub logo: Option<String>, pub poster: Option<String>,
    /// TMDB's YouTube trailers and teasers (video keys), best first: alternatives for regions
    /// where one is blocked.
    pub trailers: Vec<String>,
    pub tmdb: Option<(String, i64)>,
    /// The TMDB collection a film belongs to (a series of films), for "watch part 1 first".
    pub collection: Option<i64>,
    /// The title page's own material from the real production, all from the same TMDB answer:
    /// more official frames (textless), its tagline, cast with photos and parts, key crew,
    /// studio and network logos, the certificate, the original language, each season's
    /// poster and the videos (trailers, featurettes, behind the scenes).
    pub stills: Vec<String>,
    pub tagline: Option<String>,
    pub cast: Vec<Person>,
    pub crew: Vec<(String, Vec<String>)>,
    pub studios: Vec<Studio>,
    pub certificate: Option<String>,
    pub language: Option<String>,
    pub season_posters: HashMap<i64, String>,
    pub videos: Vec<Video>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Person { pub name: String, pub role: String, pub photo: Option<String> }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Studio { pub name: String, pub logo: String }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Video { pub youtube: String, pub name: String, pub kind: String, pub official: bool }

const TMDB_IMG: &str = "https://image.tmdb.org/t/p";

/// The production's own material from one TMDB details answer (see Artwork).
fn production(details: &Value, kind: &str, backdrop: Option<&str>) -> Artwork {
    let img = |size: &str, v: &Value| v.as_str().filter(|p| !p.is_empty()).map(|p| format!("{TMDB_IMG}/{size}{p}"));
    let mut frames: Vec<&Value> = details["images"]["backdrops"].as_array().into_iter().flatten().filter(|i| i["iso_639_1"].is_null()).collect();
    frames.sort_by(|a, b| b["vote_average"].as_f64().unwrap_or(0.0).partial_cmp(&a["vote_average"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
    let stills: Vec<String> = frames.iter().filter_map(|i| img("w1280", &i["file_path"])).filter(|u| Some(u.as_str()) != backdrop).take(4).collect();
    let tv = kind == "tv";
    let credits = if tv { &details["aggregate_credits"] } else { &details["credits"] };
    let cast = credits["cast"].as_array().into_iter().flatten().take(15).map(|c| Person {
        name: s(c, "name"),
        role: if tv { c["roles"][0]["character"].as_str().unwrap_or_default().to_owned() } else { s(c, "character") },
        photo: img("w185", &c["profile_path"]),
    }).filter(|p| !p.name.is_empty()).collect();
    let mut crew: Vec<(String, Vec<String>)> = Vec::new();
    let mut credit = |label: &str, name: String| {
        if name.is_empty() { return; }
        match crew.iter_mut().find(|(l, _)| l == label) {
            Some((_, names)) => if !names.contains(&name) && names.len() < 3 { names.push(name) },
            None => crew.push((label.to_owned(), vec![name])),
        }
    };
    for c in details["created_by"].as_array().into_iter().flatten() { credit("Created by", s(c, "name")); }
    for c in credits["crew"].as_array().into_iter().flatten() {
        let jobs: Vec<String> = if tv { c["jobs"].as_array().into_iter().flatten().map(|j| s(j, "job")).collect() } else { vec![s(c, "job")] };
        for job in jobs {
            let label = match job.as_str() { "Director" if !tv => "Directed by", "Screenplay" | "Writer" if !tv => "Written by",
                "Original Music Composer" => "Music by", "Director of Photography" if !tv => "Cinematography by", _ => continue };
            credit(label, s(c, "name"));
        }
    }
    let order = ["Created by", "Directed by", "Written by", "Music by", "Cinematography by"];
    crew.sort_by_key(|(l, _)| order.iter().position(|o| o == l));
    let mut studios: Vec<Studio> = Vec::new();
    for c in details["networks"].as_array().into_iter().flatten().chain(details["production_companies"].as_array().into_iter().flatten()) {
        if let Some(logo) = img("w185", &c["logo_path"]) {
            if studios.len() < 4 && !studios.iter().any(|x| x.name == s(c, "name")) { studios.push(Studio { name: s(c, "name"), logo }); }
        }
    }
    let rated = |list: &Value, field: &dyn Fn(&Value) -> Option<String>| {
        let all: Vec<&Value> = list["results"].as_array().into_iter().flatten().collect();
        all.iter().find(|r| r["iso_3166_1"] == "US").and_then(|r| field(r)).or_else(|| all.iter().find_map(|r| field(r)))
    };
    let certificate = if tv { rated(&details["content_ratings"], &|r| Some(s(r, "rating")).filter(|x| !x.is_empty())) }
        else { rated(&details["release_dates"], &|r| r["release_dates"].as_array().into_iter().flatten().map(|d| s(d, "certification")).find(|x| !x.is_empty())) };
    let original = s(details, "original_language");
    let language = details["spoken_languages"].as_array().into_iter().flatten().find(|l| l["iso_639_1"] == original.as_str())
        .map(|l| s(l, "english_name")).filter(|x| !x.is_empty());
    let season_posters = details["seasons"].as_array().into_iter().flatten()
        .filter_map(|x| Some((x["season_number"].as_i64()?, img("w185", &x["poster_path"])?))).collect();
    // The production's own videos first: official trailers, teasers, clips, then the rest.
    let rank = |kind: &str| match kind { "Trailer" => 4, "Teaser" => 3, "Clip" => 2, "Featurette" => 1, _ => 0 };
    let mut videos: Vec<Video> = details["videos"]["results"].as_array().into_iter().flatten()
        .filter(|v| v["site"] == "YouTube" && matches!(v["type"].as_str(), Some("Trailer" | "Teaser" | "Featurette" | "Behind the Scenes" | "Clip")))
        .map(|v| Video { youtube: s(v, "key"), name: s(v, "name"), kind: s(v, "type"), official: v["official"].as_bool().unwrap_or(false) })
        .filter(|v| !v.youtube.is_empty()).collect();
    videos.sort_by_key(|v| std::cmp::Reverse((v.official, rank(&v.kind))));
    videos.truncate(12);
    Artwork { stills, tagline: Some(s(details, "tagline")).filter(|t| !t.is_empty()), cast, crew, studios, certificate, language, season_posters, videos, ..Artwork::default() }
}

static ARTWORK: OnceLock<Mutex<HashMap<String, Artwork>>> = OnceLock::new();

fn imdb_of(m: &Media) -> Option<String> {
    Some(m.id.as_str()).filter(|id| id.starts_with("tt"))
        .or_else(|| m.raw["imdb_id"].as_str().filter(|id| id.starts_with("tt")))
        .map(|id| id.split(':').next().unwrap_or(id).to_owned())
}

static TMDB_KEY: Mutex<Option<String>> = Mutex::new(None);
fn set_tmdb_key(key: Option<String>) { if let Ok(mut k) = TMDB_KEY.lock() { *k = key; } }
fn tmdb_key() -> Option<String> { TMDB_KEY.lock().ok().and_then(|k| k.clone()) }

/// GET a TMDB v3 endpoint. The key travels as a query parameter and is never logged.
async fn tmdb(path: String, query: Vec<(&'static str, String)>) -> Option<Value> {
    let key = tmdb_key()?;
    on_runtime(async move {
        let response = backend().http.get(format!("https://api.themoviedb.org/3/{path}"))
            .query(&[("api_key", key)]).query(&query).send().await.ok()?.error_for_status().ok()?;
        response.json::<Value>().await.ok()
    }).await.ok().flatten()
}

fn best_image(list: &Value, languages: &[Option<&str>]) -> Option<String> {
    let images: Vec<&Value> = list.as_array().into_iter().flatten().collect();
    languages.iter().find_map(|lang| {
        images.iter().filter(|i| i["iso_639_1"].as_str() == *lang)
            .max_by(|a, b| a["vote_average"].as_f64().unwrap_or(0.0).partial_cmp(&b["vote_average"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal))
            .and_then(|i| i["file_path"].as_str().map(str::to_owned))
    })
}

/// TMDB artwork for a title with an IMDb id; cached for the session. None without a key.
pub async fn artwork(m: &Media) -> Option<Artwork> {
    if !tmdb_ready() { return bp_artwork(m).await; }
    let imdb = imdb_of(m);
    let cache_key = imdb.clone().unwrap_or_else(|| format!("{}|{}", m.name.to_lowercase(), m.first_year().unwrap_or(0)));
    let cache = ARTWORK.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok()?.get(&cache_key).cloned() { return Some(hit); }
    let by_imdb = match &imdb {
        Some(id) => tmdb(format!("find/{id}"), vec![("external_source", "imdb_id".into())]).await.and_then(|f| {
            f["movie_results"][0]["id"].as_i64().map(|i| ("movie", i)).or_else(|| f["tv_results"][0]["id"].as_i64().map(|i| ("tv", i)))
        }),
        None => None,
    };
    // No IMDb id (Kitsu anime, for one) or unknown to TMDB: search by name, accepting only a
    // result whose year agrees within one year, so another title's art is never borrowed.
    let found = match by_imdb { Some(x) => Some(x), None => tmdb_by_name(m).await };
    let (kind, id) = found?;
    // Images and videos in one request.
    let extra = if kind == "tv" { "images,videos,aggregate_credits,content_ratings" } else { "images,videos,credits,release_dates" };
    let details = tmdb(format!("{kind}/{id}"), vec![("append_to_response", extra.into()), ("include_image_language", "en,null".into())]).await.unwrap_or_default();
    let images = &details["images"];
    let mut videos: Vec<&Value> = details["videos"]["results"].as_array().into_iter().flatten()
        .filter(|v| v["site"] == "YouTube" && (v["type"] == "Trailer" || v["type"] == "Teaser")).collect();
    // The production's own videos first: official trailers, then official teasers, then the rest.
    videos.sort_by_key(|v| std::cmp::Reverse((v["official"].as_bool().unwrap_or(false), v["type"] == "Trailer", v["iso_639_1"] == "en", v["size"].as_i64().unwrap_or(0))));
    let trailers: Vec<String> = videos.iter().filter_map(|v| v["key"].as_str().map(str::to_owned)).collect();
    let backdrop = best_image(&images["backdrops"], &[None, Some("en")]).map(|p| format!("https://image.tmdb.org/t/p/w1280{p}"));
    let art = Artwork {
        genres: details["genres"].as_array().into_iter().flatten().filter_map(|g| g["name"].as_str()).map(crate::picks_rules::canonical_genre).collect(),
        // Text-free backdrops first (no language), then English ones.
        backdrop: backdrop.clone(),
        logo: best_image(&images["logos"], &[Some("en"), None]).map(|p| format!("https://image.tmdb.org/t/p/w500{p}")),
        poster: best_image(&images["posters"], &[Some("en"), None]).or_else(|| details["poster_path"].as_str().map(str::to_owned))
            .map(|p| format!("https://image.tmdb.org/t/p/w500{p}")),
        trailers,
        tmdb: Some((kind.to_owned(), id)),
        collection: details["belongs_to_collection"]["id"].as_i64(),
        ..production(&details, kind, backdrop.as_deref())
    };
    if let Ok(mut c) = cache.lock() { c.insert(cache_key, art.clone()); }
    Some(art)
}

/// Whether TMDB can be asked (a key is set).
pub fn tmdb_ready() -> bool { tmdb_key().is_some() }

/// TMDB genre ids and names, per "movie" or "tv".
type GenreLists = HashMap<String, Vec<(i64, String)>>;
static TMDB_GENRES: OnceLock<Mutex<GenreLists>> = OnceLock::new();

/// TMDB's genre list for "movie" or "tv", cached.
async fn tmdb_genre_list(kind: &str) -> Vec<(i64, String)> {
    let cache = TMDB_GENRES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(kind).cloned()) { return hit; }
    let list: Vec<(i64, String)> = tmdb(format!("genre/{kind}/list"), vec![]).await.map(|v| v["genres"].as_array().into_iter().flatten()
        .filter_map(|g| Some((g["id"].as_i64()?, g["name"].as_str()?.to_owned()))).collect()).unwrap_or_default();
    if let Ok(mut c) = cache.lock() { c.insert(kind.to_owned(), list.clone()); }
    list
}

/// Popular, well-rated titles from TMDB Discover for a genre (TMDB genre, else a keyword such as
/// "martial arts"), type, style and years, as Cinemeta titles (IMDb ids) so they open and play
/// like any other. Titles without an IMDb id are skipped. Formats (reality, talk, news, soap)
/// never come back, kids' series only when asked for, and a series carries the years it aired.
pub async fn tmdb_candidates(genre: Option<String>, kind: Option<String>, style: Style, from: Option<i32>, to: Option<i32>, pages: Vec<u32>) -> Vec<Media> {
    if !tmdb_ready() { return Vec::new(); }
    let kinds: Vec<&str> = match kind.as_deref() { Some("movie") => vec!["movie"], Some("series") => vec!["tv"], _ => vec!["movie", "tv"] };
    let wants_kids = genre.as_deref().is_some_and(|g| crate::picks_rules::canonical_genre(g) == "Kids");
    let mut out = Vec::new();
    for tk in kinds {
        let genres = tmdb_genre_list(tk).await;
        let mut query: Vec<(&'static str, String)> = vec![("sort_by", "popularity.desc".into()), ("vote_count.gte", "80".into()), ("include_adult", "false".into())];
        let mut with_genres: Vec<i64> = Vec::new();
        let mut with_keywords: Vec<i64> = Vec::new();
        let mut without: Vec<i64> = Vec::new();
        match style { Style::Animation => with_genres.push(16), Style::Live => without.push(16), Style::Both => {} }
        // Reality, News, Soap, Talk; Kids unless asked for.
        if tk == "tv" { without.extend([10764, 10763, 10766, 10767]); if !wants_kids { without.push(10762); } }
        if let Some(g) = &genre {
            let wanted = match crate::picks_rules::canonical_genre(g).to_lowercase().as_str() {
                "sci-fi" => if tk == "tv" { "sci-fi & fantasy".into() } else { "science fiction".into() },
                "kids" => if tk == "tv" { "kids".into() } else { "family".into() },
                other => other.to_owned(),
            };
            match genres.iter().find(|(_, n)| n.to_lowercase() == wanted || n.to_lowercase().split(" & ").any(|p| p == wanted)) {
                Some((id, _)) => { with_genres.push(*id); without.retain(|w| w != id); }
                None => {
                    let found = tmdb("search/keyword".into(), vec![("query", g.clone())]).await;
                    match found.and_then(|v| v["results"][0]["id"].as_i64()) { Some(id) => with_keywords.push(id), None => continue }
                }
            }
        }
        let join = |v: &[i64]| v.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        if !with_genres.is_empty() { query.push(("with_genres", join(&with_genres))); }
        if !with_keywords.is_empty() { query.push(("with_keywords", join(&with_keywords))); }
        if !without.is_empty() { query.push(("without_genres", join(&without))); }
        // A film by its release; a series by any episode aired in the range.
        if tk == "movie" {
            if let Some(f) = from { query.push(("primary_release_date.gte", format!("{f}-01-01"))); }
            if let Some(t) = to { query.push(("primary_release_date.lte", format!("{t}-12-31"))); }
        } else {
            if let Some(f) = from { query.push(("air_date.gte", format!("{f}-01-01"))); }
            if let Some(t) = to { query.push(("first_air_date.lte", format!("{t}-12-31"))); }
        }
        let mut results: Vec<Value> = Vec::new();
        for page in &pages {
            let mut q = query.clone(); q.push(("page", page.to_string()));
            let Some(v) = tmdb(format!("discover/{tk}"), q).await else { break };
            results.extend(v["results"].as_array().cloned().unwrap_or_default());
        }
        out.extend(tmdb_titles(tk, results, genre.as_deref()).await);
    }
    out
}

/// TMDB results (Discover, recommendations) as Cinemeta titles, so they open and play like any
/// other: IMDb ids looked up together, a series with the years it aired. `genre` is added to a
/// title that lacks it (a keyword match). Titles without an IMDb id are skipped.
async fn tmdb_titles(tk: &str, results: Vec<Value>, genre: Option<&str>) -> Vec<Media> {
    let genres = tmdb_genre_list(tk).await;
    let names = |ids: &[i64]| ids.iter().filter_map(|id| genres.iter().find(|(g, _)| g == id).map(|(_, n)| n.clone())).collect::<Vec<_>>();
    let mut out = Vec::new();
    // IMDb ids (and, for a series, the years it aired) for every result at once.
    let ids: Vec<i64> = results.iter().filter_map(|r| r["id"].as_i64()).collect();
    let kind_path = tk.to_owned();
    let found: HashMap<i64, (String, String)> = on_runtime(async move {
        let mut set = tokio::task::JoinSet::new();
        // At most eight TMDB requests at a time (it rate-limits bursts).
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        for id in ids {
            let kp = kind_path.clone();
            let gate = gate.clone();
            set.spawn(async move {
                let _permit = gate.acquire_owned().await;
                if kp == "tv" {
                    let d = tmdb(format!("tv/{id}"), vec![("append_to_response", "external_ids".into())]).await;
                    (id, d.and_then(|d| {
                        let imdb = d["external_ids"]["imdb_id"].as_str()?.to_owned();
                        let first = d["first_air_date"].as_str().and_then(|x| x.get(..4)).unwrap_or_default().to_owned();
                        let last = d["last_air_date"].as_str().and_then(|x| x.get(..4)).unwrap_or_default();
                        let running = d["in_production"].as_bool().unwrap_or(false);
                        let years = if first.is_empty() { String::new() } else if running { format!("{first}–") } else if last.is_empty() || last == first { first } else { format!("{first}–{last}") };
                        Some((imdb, years))
                    }))
                } else {
                    (id, tmdb(format!("movie/{id}/external_ids"), vec![]).await.and_then(|e| e["imdb_id"].as_str().map(|i| (i.to_owned(), String::new()))))
                }
            });
        }
        let mut found = HashMap::new();
        while let Some(done) = set.join_next().await {
            if let Ok((id, Some((imdb, years)))) = done { if imdb.starts_with("tt") { found.insert(id, (imdb, years)); } }
        }
        found
    }).await.unwrap_or_default();
    for r in &results {
        let Some((imdb, years)) = r["id"].as_i64().and_then(|id| found.get(&id).cloned()) else { continue };
        let ids: Vec<i64> = r["genre_ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
        let mut genre_names: Vec<String> = names(&ids);
        if let Some(g) = genre { if !genre_names.iter().any(|n| crate::picks_rules::has_genre_name(n, g)) { genre_names.push(g.to_owned()); } }
        // Japanese animation is anime.
        if ids.contains(&16) && r["original_language"] == "ja" { genre_names.push("Anime".into()); }
        let date = r[if tk == "movie" { "release_date" } else { "first_air_date" }].as_str().unwrap_or_default();
        let release = if years.is_empty() { date.get(..4).unwrap_or_default().to_owned() } else { years };
        let meta = json!({
            "id": imdb, "type": if tk == "movie" { "movie" } else { "series" },
            "name": r[if tk == "movie" { "title" } else { "name" }].as_str().unwrap_or("Untitled"),
            "description": r["overview"].as_str().unwrap_or_default(),
            "releaseInfo": release,
            "poster": r["poster_path"].as_str().map(|p| format!("https://image.tmdb.org/t/p/w500{p}")).unwrap_or_default(),
            "background": r["backdrop_path"].as_str().map(|p| format!("https://image.tmdb.org/t/p/w1280{p}")).unwrap_or_default(),
            "genres": genre_names, "imdbRating": r["vote_average"].as_f64().map(|v| format!("{v:.1}")).unwrap_or_default(),
            "tmdb_id": r["id"],
        });
        if let Some(m) = Media::from_json(&meta, CINEMETA) { out.push(crate::picks_rules::tidy(m)); }
    }
    out
}

/// Titles TMDB recommends for one you kept or liked: the pool grows toward your taste.
pub async fn tmdb_related(m: &Media) -> Vec<Media> {
    if !tmdb_ready() { return Vec::new(); }
    let Some((kind, id)) = artwork(m).await.and_then(|a| a.tmdb) else { return Vec::new() };
    let Some(v) = tmdb(format!("{kind}/{id}/recommendations"), vec![("page", "1".into())]).await else { return Vec::new() };
    // Formats (reality, news, soap, talk) and kids' series are never offered; skip their lookups.
    let results: Vec<Value> = v["results"].as_array().into_iter().flatten()
        .filter(|r| !r["genre_ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).any(|g| [10764, 10763, 10766, 10767, 10762].contains(&g)))
        .filter(|r| r["vote_count"].as_i64().unwrap_or(0) >= 50)
        .take(12).cloned().collect();
    tmdb_titles(&kind, results, None).await
}

fn tvmaze_genres(s: &Value) -> Vec<String> {
    let mut genres: Vec<String> = s["genres"].as_array().into_iter().flatten().filter_map(Value::as_str)
        .map(|g| if g == "Science-Fiction" { "Sci-Fi".to_owned() } else { g.to_owned() }).collect();
    // TVmaze classifies animation in the show's TYPE, often not its genres (e.g. Simpsons).
    if s["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("animation") || t.eq_ignore_ascii_case("anime"))
        && !genres.iter().any(|g| g.eq_ignore_ascii_case("animation")) { genres.push("Animation".into()); }
    genres
}

static TVMAZE: OnceLock<tokio::sync::OnceCell<Vec<Value>>> = OnceLock::new();

/// Series from TVmaze (free, no key): popular, well-rated shows with IMDb ids, sampled across
/// its index once per session, as Cinemeta titles so they open and play like any other.
pub async fn tvmaze_candidates(genre: Option<String>, style: Style, from: Option<i32>, to: Option<i32>) -> Vec<Media> {
    let cell = TVMAZE.get_or_init(tokio::sync::OnceCell::new);
    // An empty answer (offline, TVmaze down) is not kept: the next visit asks again.
    let shows = cell.get_or_try_init(|| async {
        let mut all = Vec::new();
        for page in [0u32, 4, 8, 12, 16, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70] {
            let fetched = on_runtime(async move {
                let r = backend().http.get(format!("https://api.tvmaze.com/shows?page={page}")).send().await.ok()?.error_for_status().ok()?;
                r.json::<Vec<Value>>().await.ok()
            }).await.ok().flatten();
            if let Some(list) = fetched {
                all.extend(list.into_iter().filter(|s| s["externals"]["imdb"].is_string() && s["weight"].as_i64().unwrap_or(0) >= 70
                    && s["rating"]["average"].as_f64().unwrap_or(0.0) >= 6.8));
            }
        }
        if all.is_empty() { Err(()) } else { Ok(all) }
    }).await;
    let Ok(shows) = shows else { return Vec::new() };
    let wanted = genre.as_deref().map(|g| match g.to_lowercase().as_str() { "sci-fi" | "science fiction" => "science-fiction".to_owned(), x => x.to_owned() });
    let mut out: Vec<(f64, Media)> = shows.iter().filter_map(|s| {
        let genres = tvmaze_genres(s);
        let lower: Vec<String> = genres.iter().map(|g| g.to_lowercase().replace("sci-fi", "science-fiction")).collect();
        let animated = lower.iter().any(|g| g == "anime" || g == "animation");
        match style { Style::Animation if !animated => return None, Style::Live if animated => return None, _ => {} }
        if let Some(w) = &wanted { if !lower.iter().any(|g| g == w) { return None; } }
        // The years it aired: from its premiere to its end (or still running).
        let year: i32 = s["premiered"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0);
        let ended: Option<i32> = s["ended"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok());
        let last = ended.unwrap_or(9999);
        if from.is_some_and(|f| year == 0 || last < f) || to.is_some_and(|t| year == 0 || year > t) { return None; }
        let summary = html_tag().replace_all(s["summary"].as_str().unwrap_or_default(), "").to_string();
        let meta = json!({
            "id": s["externals"]["imdb"], "type": "series", "name": s["name"], "description": summary,
            "releaseInfo": match (year, ended) { (0, _) => String::new(), (y, Some(e)) if e != y => format!("{y}–{e}"), (y, Some(_)) => y.to_string(), (y, None) => format!("{y}–") },
            "poster": s["image"]["original"].as_str().or(s["image"]["medium"].as_str()).unwrap_or_default(),
            "genres": genres, "imdbRating": s["rating"]["average"].as_f64().map(|v| format!("{v:.1}")).unwrap_or_default(),
        });
        let score = s["weight"].as_f64().unwrap_or(0.0) + s["rating"]["average"].as_f64().unwrap_or(0.0) * 4.0;
        Media::from_json(&meta, CINEMETA).map(|m| (score, crate::picks_rules::tidy(m)))
    }).collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().take(40).map(|(_, m)| m).collect()
}

async fn tmdb_by_name(m: &Media) -> Option<(&'static str, i64)> {
    let v = tmdb("search/multi".into(), vec![("query", m.name.clone()), ("include_adult", "false".into())]).await?;
    let year = m.first_year();
    let want_tv = m.kind != "movie";
    let mut fits: Vec<(&'static str, i64, bool)> = v["results"].as_array().into_iter().flatten().filter_map(|r| {
        let kind = match r["media_type"].as_str()? { "movie" => "movie", "tv" => "tv", _ => return None };
        let date = r["release_date"].as_str().or(r["first_air_date"].as_str()).unwrap_or_default();
        let ry: Option<i32> = date.get(..4).and_then(|y| y.parse().ok());
        let year_ok = match (year, ry) { (Some(a), Some(b)) => (a - b).abs() <= 1, (None, _) => true, (Some(_), None) => false };
        year_ok.then_some((kind, r["id"].as_i64()?, (kind == "tv") == want_tv))
    }).collect();
    fits.sort_by_key(|f| !f.2); // the right kind first
    fits.first().map(|f| (f.0, f.1))
}

/// TMDB episode stills for one season: episode number -> image URL.
pub async fn season_stills(m: &Media, season: i64) -> HashMap<i64, String> {
    let Some(Artwork { tmdb: Some((kind, id)), .. }) = artwork(m).await else { return HashMap::new() };
    if kind != "tv" { return HashMap::new(); }
    let Some(v) = tmdb(format!("tv/{id}/season/{season}"), vec![]).await else { return HashMap::new() };
    v["episodes"].as_array().into_iter().flatten().filter_map(|e| {
        Some((e["episode_number"].as_i64()?, format!("https://image.tmdb.org/t/p/w400{}", e["still_path"].as_str()?)))
    }).collect()
}

/// Keywords use the existing in-memory Settings key and fixed TMDB endpoints. A successful
/// empty list is distinct from a network/auth failure, which enrichment retries later.
pub(crate) async fn picks_keywords(m: &Media) -> Option<Vec<String>> {
    let kind = if m.kind == "movie" { "movie" } else { "tv" };
    let id = if let Some(id) = m.raw["tmdb_id"].as_i64().filter(|i| *i > 0) { Some(id) }
        else if let Some(imdb) = imdb_of(m) {
            let found = tmdb(format!("find/{imdb}"), vec![("external_source","imdb_id".into())]).await?;
            found[if kind == "movie" { "movie_results" } else { "tv_results" }][0]["id"].as_i64()
        } else {
            let found = tmdb(format!("search/{kind}"), vec![("query",m.name.clone()),("language","en-US".into())]).await?;
            let mut matches = found["results"].as_array().into_iter().flatten().filter(|r| {
                let title = r[if kind == "movie" { "title" } else { "name" }].as_str().unwrap_or_default();
                let year = r[if kind == "movie" { "release_date" } else { "first_air_date" }].as_str().and_then(|s| s.get(..4)).and_then(|s| s.parse::<i32>().ok());
                search_words(title) == search_words(&m.name) && match (m.first_year(),year) { (Some(a),Some(b)) => (a-b).abs() <= 1, (None,_) => true, _ => false }
            });
            let first = matches.next().and_then(|r| r["id"].as_i64());
            if matches.next().is_some() { None } else { first }
        };
    let Some(id) = id.filter(|id| *id > 0) else { return Some(Vec::new()) };
    let response = tmdb(format!("{kind}/{id}/keywords"),vec![]).await?;
    Some(crate::picks_metadata::keywords(&response))
}

// ---------------------------------------------------------------- picks duel

/// Genres some installed catalog can be asked for (lower-case), e.g. Cinemeta's genre list.
pub async fn catalog_genres() -> HashSet<String> {
    let Ok(v) = exec(json!({"op":"addons"})).await else { return HashSet::new() };
    v["addons"].as_array().into_iter().flatten()
        .filter(|a| !retired_addon(&s(a, "manifest_url")))
        .flat_map(|a| a["catalogs"].as_array().into_iter().flatten())
        .flat_map(|c| c["extra"].as_array().into_iter().flatten())
        .filter(|e| e["name"] == "genre")
        .flat_map(|e| e["options"].as_array().into_iter().flatten())
        .filter_map(Value::as_str).map(str::to_lowercase).collect()
}

/// More candidates for a genre: every installed catalog that declares this genre as an option
/// (Cinemeta for its own, an installed anime add-on for genres such as Martial Arts), first page of each.
pub async fn genre_candidates(genre: &str) -> Vec<Media> {
    let Ok(v) = exec(json!({"op":"addons"})).await else { return Vec::new() };
    let mut requests = Vec::new();
    for addon in v["addons"].as_array().into_iter().flatten() {
        let url = s(addon, "manifest_url");
        if retired_addon(&url) { continue; }
        for c in addon["catalogs"].as_array().into_iter().flatten() {
            let extras: Vec<&Value> = c["extra"].as_array().into_iter().flatten().collect();
            // Only catalogs whose other extras are optional, and that list this genre.
            if extras.iter().any(|e| e["isRequired"] == true && e["name"] != "genre") { continue; }
            let option = extras.iter().filter(|e| e["name"] == "genre").flat_map(|e| e["options"].as_array().into_iter().flatten())
                .filter_map(Value::as_str).find(|o| o.eq_ignore_ascii_case(genre));
            if let Some(option) = option {
                requests.push(json!({"op":"catalog","addon_url":url,"type":s(c, "type"),"id":s(c, "id"),"extra":{"genre":option}}));
            }
        }
    }
    let mut out = Vec::new();
    for req in requests.into_iter().take(6) {
        let origin = s(&req, "addon_url");
        if let Ok(v) = exec(req).await { out.extend(metas(&v, &origin)); }
    }
    out
}

fn duel_state_path() -> PathBuf { backend().data_dir.join("picks-duel.json") }

/// Tonight in Picks, kept across tabs and restarts: the filters, the night it belongs to, the
/// cards on screen and what you kept and starred.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DuelState {
    pub kind: Option<String>, pub style: String, pub genre: Option<String>, pub from: String, pub to: String,
    /// The night (the brain's session) and when it was last played.
    pub night: u64, pub last: i64,
    pub round: u32, pub streak: u32, pub shown: Vec<String>,
    pub left: Option<Media>, pub right: Option<Media>,
    /// Titles kept at least once tonight, and the ones that won their way onto the shortlist.
    pub kept: Vec<Media>, pub starred: Vec<Media>,
}

/// A title as JSON that `Media::from_json` reads back unchanged.
fn media_json(m: &Media) -> Value {
    let mut v = if m.raw.is_object() { m.raw.clone() } else { json!({}) };
    let o = v.as_object_mut().expect("object");
    for (k, x) in [("id", &m.id), ("type", &m.kind), ("name", &m.name), ("addon_url", &m.addon_url), ("poster", &m.poster),
        ("background", &m.background), ("description", &m.description), ("releaseInfo", &m.year)] { o.insert(k.into(), json!(x)); }
    o.insert("genres".into(), json!(m.genres));
    v
}

pub fn save_duel(d: &DuelState) {
    let m = |x: &Option<Media>| x.as_ref().map(media_json).unwrap_or(Value::Null);
    let list = |l: &[Media]| l.iter().map(media_json).collect::<Vec<_>>();
    let _ = std::fs::write(duel_state_path(), json!({"version": 3, "kind": d.kind, "style": d.style, "genre": d.genre, "from": d.from, "to": d.to,
        "night": d.night.to_string(), "last": d.last, "round": d.round, "streak": d.streak, "shown": d.shown,
        "left": m(&d.left), "right": m(&d.right), "kept": list(&d.kept), "starred": list(&d.starred)}).to_string());
}

pub fn load_duel() -> Option<DuelState> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(duel_state_path()).ok()?).ok()?;
    let m = |k: &str| (!v[k].is_null()).then(|| Media::from_json(&v[k], "")).flatten();
    let list = |k: &str| v[k].as_array().into_iter().flatten().filter_map(|x| Media::from_json(x, "")).collect::<Vec<_>>();
    // An earlier duel's suggested pick carries on as the card you kept.
    let winner = m("winner");
    Some(DuelState {
        // "anime" was a type before Style existed: it is the Animation style now.
        kind: v["kind"].as_str().filter(|k| *k != "anime").map(str::to_owned),
        style: if v["kind"] == "anime" { "animation".into() } else { v["style"].as_str().unwrap_or("live").to_owned() },
        genre: v["genre"].as_str().map(str::to_owned),
        from: s(&v, "from"), to: s(&v, "to"),
        night: v["night"].as_str().and_then(|n| n.parse().ok()).unwrap_or(0), last: v["last"].as_i64().unwrap_or(0),
        round: v["round"].as_u64().unwrap_or(0) as u32, streak: v["streak"].as_u64().unwrap_or(0) as u32,
        shown: v["shown"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
        left: m("left").or_else(|| winner.clone()), right: m("right"),
        kept: { let mut k = list("kept"); if let Some(w) = winner { if !k.iter().any(|x| x.key() == w.key()) { k.push(w); } } k },
        starred: list("starred"),
    })
}

/// IMDb (or add-on) ids of titles you finished, whichever add-on they were watched from.
/// Films count once finished; a series counts only when every released regular episode is
/// watched. Unfinished series stay available with Resume. Also match by title/year.
pub async fn watched_ids() -> HashSet<String> {
    let history = viewing_history().await.unwrap_or_default();
    let mut finished = Vec::new();
    for item in history {
        let m = item.media;
        if m.kind == "movie" { if m.watched { finished.push(m); } continue; }
        let seen = watched_episodes(&m).await;
        if seen.is_empty() { if m.watched { finished.push(m); } continue; }
        // Metadata is normally cached by the prior playback/title page. If unavailable, keep
        // the series visible rather than inferring that one finished episode completes it.
        let mut episodes = episodes_from_meta(&m.raw);
        if episodes.is_empty() { episodes = title_meta(&m).await.map(|m| m.episodes).unwrap_or_default(); }
        if crate::picks_rules::series_complete(&episodes,&seen,now_secs()) || (episodes.is_empty() && m.watched) { finished.push(m); }
    }
    finished.into_iter().flat_map(|m| [Some(m.id.clone()),imdb_of(&m),Some(crate::picks_rules::title_key(&m))]).flatten().collect()
}

// ---------------------------------------------------------------- history

/// App-private search history: recorded on submit or opening a result, never per keystroke.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct History {
    pub queries: Vec<String>,
    pub items: Vec<Media>,
}

fn history_path() -> PathBuf {
    backend().data_dir.join("search-history.json")
}

pub fn load_history() -> History {
    let Ok(text) = std::fs::read_to_string(history_path()) else { return History::default() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return History::default() };
    History {
        queries: v["queries"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
        items: v["items"].as_array().into_iter().flatten().filter_map(|m| Media::from_json(m, "")).collect(),
    }
}

fn save_history(h: &History) {
    let items: Vec<Value> = h.items.iter().map(|m| {
        let mut raw = m.raw.clone();
        if let Some(o) = raw.as_object_mut() {
            o.insert("addon_url".into(), json!(m.addon_url));
            if let Some(Value::String(d)) = o.get_mut("description") { d.truncate(d.char_indices().nth(1000).map_or(d.len(), |(i, _)| i)); }
            o.remove("videos");
        }
        raw
    }).collect();
    let _ = std::fs::write(history_path(), json!({"queries":h.queries,"items":items}).to_string());
}

pub fn recent_queries(query: &str, previous: &[String]) -> Vec<String> {
    let trimmed: String = query.trim().chars().take(200).collect();
    if trimmed.chars().count() < 2 { return previous.to_vec(); }
    let mut out = vec![trimmed];
    for p in previous {
        if !out.iter().any(|x| x.to_lowercase() == p.to_lowercase()) { out.push(p.clone()); }
    }
    out.truncate(HISTORY_LIMIT);
    out
}

pub fn remember_query(h: &mut History, query: &str) {
    h.queries = recent_queries(query, &h.queries);
    save_history(h);
}

pub fn remember_item(h: &mut History, media: &Media) {
    let mut items = vec![media.clone()];
    items.extend(h.items.iter().filter(|m| m.key() != media.key()).cloned());
    items.truncate(HISTORY_LIMIT);
    h.items = items;
    save_history(h);
}

pub fn clear_history(h: &mut History) {
    *h = History::default();
    let _ = std::fs::remove_file(history_path());
}

// ---------------------------------------------------------------- accent

/// The accent a title lends the interface, as CSS hex; None keeps Bone.
pub async fn accent(url: String) -> Option<String> {
    if url.is_empty() { return None; }
    if let Some(hit) = backend().accents.lock().ok()?.get(&url).cloned() { return hit; }
    let key = url.clone();
    let computed = on_runtime(async move {
        const LIMIT: usize = 12_000_000;
        let mut response = backend().http.get(&url).send().await.ok()?.error_for_status().ok()?;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) { return None; }
        // Read at most LIMIT bytes whatever the server says, and decode within a memory budget.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > LIMIT { return None; }
            bytes.extend_from_slice(&chunk);
        }
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(256 * 1024 * 1024);
        limits.max_image_width = Some(12_000);
        limits.max_image_height = Some(12_000);
        reader.limits(limits);
        let img = reader.decode().ok()?.thumbnail(48, 48).to_rgba8();
        let pixels: Vec<[u8; 4]> = img.pixels().map(|p| p.0).collect();
        artwork_accent(&pixels).map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
    }).await.ok().flatten();
    if let Ok(mut cache) = backend().accents.lock() { cache.insert(key, computed.clone()); }
    computed
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
    fn the_production_from_one_tmdb_answer() {
        let movie = json!({"tagline": "A tagline", "original_language": "en", "spoken_languages": [{"iso_639_1": "en", "english_name": "English"}],
            "images": {"backdrops": [{"iso_639_1": null, "file_path": "/a.jpg", "vote_average": 5.0}, {"iso_639_1": null, "file_path": "/b.jpg", "vote_average": 7.0}, {"iso_639_1": "en", "file_path": "/t.jpg", "vote_average": 9.0}]},
            "credits": {"cast": [{"name": "Matt Damon", "character": "Odysseus", "profile_path": "/m.jpg"}],
                "crew": [{"name": "Ludwig Göransson", "job": "Original Music Composer"}, {"name": "Christopher Nolan", "job": "Director"}, {"name": "Christopher Nolan", "job": "Screenplay"}]},
            "production_companies": [{"name": "Universal", "logo_path": "/u.png"}, {"name": "No logo", "logo_path": null}],
            "release_dates": {"results": [{"iso_3166_1": "GB", "release_dates": [{"certification": "12A"}]}, {"iso_3166_1": "US", "release_dates": [{"certification": ""}, {"certification": "PG-13"}]}]},
            "videos": {"results": [{"site": "YouTube", "type": "Featurette", "key": "k1", "name": "Making of"}, {"site": "Vimeo", "type": "Trailer", "key": "v"}]}});
        let a = production(&movie, "movie", Some("https://image.tmdb.org/t/p/w1280/b.jpg"));
        assert_eq!(a.stills, ["https://image.tmdb.org/t/p/w1280/a.jpg"], "textless only, the backdrop not twice");
        assert_eq!(a.tagline.as_deref(), Some("A tagline"));
        assert_eq!(a.cast[0].role, "Odysseus");
        assert_eq!(a.crew, [("Directed by".to_owned(), vec!["Christopher Nolan".to_owned()]), ("Written by".to_owned(), vec!["Christopher Nolan".to_owned()]),
            ("Music by".to_owned(), vec!["Ludwig Göransson".to_owned()])]);
        assert_eq!(a.studios.len(), 1);
        assert_eq!(a.certificate.as_deref(), Some("PG-13"), "the US certificate first");
        assert_eq!(a.language.as_deref(), Some("English"));
        assert_eq!(a.videos.len(), 1);
        let show = json!({"created_by": [{"name": "Graham Wagner"}], "networks": [{"name": "Prime Video", "logo_path": "/p.png"}],
            "aggregate_credits": {"cast": [{"name": "Ella Purnell", "roles": [{"character": "Lucy MacLean"}], "profile_path": null}]},
            "content_ratings": {"results": [{"iso_3166_1": "DE", "rating": "16"}]}, "seasons": [{"season_number": 1, "poster_path": "/s1.jpg"}, {"season_number": 2, "poster_path": null}]});
        let b = production(&show, "tv", None);
        assert_eq!(b.crew[0].0, "Created by");
        assert_eq!(b.cast[0].role, "Lucy MacLean");
        assert_eq!(b.cast[0].photo, None);
        assert_eq!(b.certificate.as_deref(), Some("16"), "any country when the US has none");
        assert_eq!(b.season_posters.len(), 1);
        assert_eq!(b.studios[0].name, "Prime Video");
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
    fn real_debrid_plays_before_the_free_web_addons() {
        let rd = source_of("Torrentio", &json!({"name":"[RD+] Torrentio 1080p","title":"Film.1080p.WEBRip.mkv","url":"https://x.invalid/rd"}));
        let web = source_of("Absolute Cinema", &json!({"name":"Absolute Cinema","title":"[4KHDHub] Film 2160p REMUX\n💾 60 GB","url":"https://x.invalid/w"}));
        let trial = source_of("NoTorrent", &json!({"name":"🌐 1080p - MPV Player","title":"Original Audio [FREE TRIAL]","url":"https://x.invalid/t"}));
        let free = source_of("NoTorrent", &json!({"name":"🌐 1080p - VLC Player","title":"Original Audio [NEW FREE]","url":"https://x.invalid/f"}));
        let urls = |v: Vec<Source>| v.into_iter().map(|s| s.url).collect::<Vec<_>>();
        assert_eq!(urls(play_order_for(&[trial.clone(), free.clone(), web.clone(), rd.clone()], None, None)),
            ["https://x.invalid/rd", "https://x.invalid/w", "https://x.invalid/f", "https://x.invalid/t"]);
        // Without a key there are only web sources: they play.
        assert_eq!(urls(play_order_for(&[free, web], None, None)), ["https://x.invalid/w", "https://x.invalid/f"]);
        // The chosen language still comes first: Italian web audio before English Real-Debrid.
        let ita = source_of("Absolute Cinema", &json!({"name":"Absolute Cinema","title":"[VixSrc] Film 1080P · 🇬🇧🇮🇹","url":"https://x.invalid/i"}));
        assert_eq!(urls(play_order_for(&[rd, ita], None, Some("italian")))[0], "https://x.invalid/i");
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
        assert_eq!(posters(&m), ["https://btttr.cc/poster/imdb/poster-default/tt6262770.jpg", "https://images.example/p.jpg",
            "https://images.metahub.space/poster/medium/tt6262770/img"]);
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

    /// A/B of AIOStreams scrapers: the usual three against them plus eight more, on the same titles,
    /// through the whole pick (probe, real sizes, audio). Both configurations are deleted after.
    #[test]
    #[ignore]
    fn addon_experiment() {
        start();
        set_stream_language(Some("english".into()));
        let extra = [("knaben", "Knaben"), ("torrent-galaxy", "TorrentGalaxy"), ("the-pirate-bay", "The Pirate Bay"), ("therarbg", "TheRARBG"),
            ("eztv", "EZTV"), ("torrents-db", "TorrentsDB"), ("peerflix", "Peerflix"), ("meteor", "Meteor")];
        // BENCH_TIMEOUT=3000: compare the usual scrapers with a shorter per-scraper timeout instead.
        let short: Option<u64> = std::env::var("BENCH_TIMEOUT").ok().and_then(|t| t.parse().ok());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let base = crate::addons::trial_config(&[], None).await;
            let more = match short { Some(ms) => crate::addons::trial_config(&[], Some(ms)).await, None => crate::addons::trial_config(&extra, None).await };
            println!("configs: usual {}, more {}", base.as_ref().map(|_| "made").unwrap_or_else(|e| { println!("  {e:#}"); "failed" }),
                more.as_ref().map(|_| "made").unwrap_or_else(|e| { println!("  {e:#}"); "failed" }));
            if let (Ok((base_url, _)), Ok((more_url, _))) = (&base, &more) {
                let titles: Vec<String> = std::env::var("BENCH_TITLES").map(|t| t.split(',').map(str::to_owned).collect())
                    .unwrap_or_else(|_| TITLES.iter().map(|t| t.to_string()).collect());
                for (n, entry) in titles.iter().enumerate() {
                    let parts: Vec<&str> = entry.splitn(4, '/').collect();
                    let (kind, id, name, year) = (parts[0], parts[1], parts.get(2).copied().unwrap_or(""), parts.get(3).and_then(|y| y.parse().ok()));
                    println!("\n=== {name}");
                    // AIOStreams limits how fast one client may ask.
                    if n > 0 { tokio::time::sleep(Duration::from_secs(std::env::var("BENCH_PAUSE").ok().and_then(|p| p.parse().ok()).unwrap_or(0))).await; }
                    // Alternate which goes first, so neither always meets warm upstream caches.
                    let order = if n % 2 == 0 { [("usual", base_url), ("more", more_url)] } else { [("more", more_url), ("usual", base_url)] };
                    for (label, base) in order {
                        let url = format!("{base}/stream/{kind}/{id}.json");
                        let t = Instant::now();
                        let body = on_runtime(async move {
                            let r = backend_http().get(url).timeout(Duration::from_secs(40)).send().await.ok()?;
                            r.json::<Value>().await.ok()
                        }).await.ok().flatten().unwrap_or_default();
                        let fetch = t.elapsed().as_millis();
                        let mut list: Vec<Source> = body["streams"].as_array().into_iter().flatten().map(|st| source_of("AIOStreams", st)).filter(|s| !is_3d(&s.label)).collect();
                        for s in &mut list { s.off_title = !names_title(s, name, year); }
                        let order = play_order(&list, (kind == "series").then_some(10));
                        let t = Instant::now();
                        let pick = pick_source(order.clone()).await;
                        println!("  {label:<5} {:>3} streams in {fetch:>5} ms, {:>3} playable; pick in {:>4} ms: {}", list.len(), order.len(), t.elapsed().as_millis(),
                            pick.best.as_ref().map(|b| format!("{} {:.60}", source_summary(b), b.filename)).unwrap_or_else(|| "nothing".into()));
                    }
                }
            }
            for login in [base, more].into_iter().flatten().map(|(_, l)| l) {
                println!("delete: {}", crate::addons::trial_delete(login).await.map(|_| "ok".to_string()).unwrap_or_else(|e| format!("{e:#}")));
            }
        });
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
