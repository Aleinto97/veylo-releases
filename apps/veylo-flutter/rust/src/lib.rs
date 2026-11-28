#![allow(dead_code, unused_imports)]

#[path = "../../../veylo-dx/src/addons.rs"]
pub mod addons;
#[path = "../../../veylo-dx/src/backend/mod.rs"]
pub mod backend;
#[path = "../../../veylo-dx/src/brain.rs"]
pub mod brain;
#[path = "../../../veylo-dx/src/display.rs"]
pub mod display;
#[path = "../../../veylo-dx/src/picks_ai.rs"]
pub mod picks_ai;
#[path = "../../../veylo-dx/src/picks_metadata.rs"]
pub mod picks_metadata;
#[path = "../../../veylo-dx/src/picks_rules.rs"]
pub mod picks_rules;
#[path = "../../../veylo-dx/src/tracks.rs"]
pub mod tracks;
mod crash;
mod studios;
mod title_store;
mod youtube;

pub fn is_tv() -> bool { false }

pub fn open_external(url: &str) {
    if !url.starts_with("https://") { return; }
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg(url).spawn(); }
    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn(); }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    if std::process::Command::new("xdg-open").arg(url).spawn().is_err() {
        let _ = std::process::Command::new("gio").args(["open", url]).spawn();
    }
}

pub mod engine {
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum SegmentKind { Intro, Recap, Credits }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Segment { pub kind: SegmentKind, pub start: f64, pub end: f64 }

    #[derive(Clone, Copy, Debug, PartialEq, Default)]
    pub struct Screen { pub hdr10: bool, pub hlg: bool, pub hdr10_plus: bool, pub dolby_vision: bool, pub peak_nits: u32 }

    pub fn language_name(code: &str) -> String {
        let c = code.to_lowercase();
        match c.as_str() {
            "en" | "eng" => "English", "it" | "ita" => "Italian", "es" | "spa" => "Spanish", "fr" | "fre" | "fra" => "French",
            "de" | "ger" | "deu" => "German", "pt" | "por" => "Portuguese", "ja" | "jpn" => "Japanese", "ko" | "kor" => "Korean",
            "zh" | "chi" | "zho" => "Chinese", "ru" | "rus" => "Russian", "nl" | "dut" | "nld" => "Dutch", "pl" | "pol" => "Polish",
            "sv" | "swe" => "Swedish", "da" | "dan" => "Danish", "no" | "nor" | "nob" | "nb" => "Norwegian", "fi" | "fin" => "Finnish",
            "tr" | "tur" => "Turkish", "ar" | "ara" => "Arabic", "hi" | "hin" => "Hindi", "el" | "gre" | "ell" => "Greek",
            "he" | "heb" => "Hebrew", "cs" | "cze" | "ces" => "Czech", "hu" | "hun" => "Hungarian", "ro" | "rum" | "ron" => "Romanian",
            "pb" | "pob" | "pt-br" => "Portuguese (Brazil)", "uk" | "ukr" => "Ukrainian", "bg" | "bul" => "Bulgarian", "hr" | "hrv" => "Croatian",
            "sr" | "srp" | "scc" => "Serbian", "sl" | "slv" => "Slovenian", "sk" | "slo" | "slk" => "Slovak", "et" | "est" => "Estonian",
            "lv" | "lav" => "Latvian", "lt" | "lit" => "Lithuanian", "fa" | "per" | "fas" => "Persian", "id" | "ind" => "Indonesian",
            "ms" | "may" | "msa" => "Malay", "th" | "tha" => "Thai", "vi" | "vie" => "Vietnamese", "ca" | "cat" => "Catalan",
            "eu" | "baq" | "eus" => "Basque", "gl" | "glg" => "Galician", "is" | "ice" | "isl" => "Icelandic", "mk" | "mac" | "mkd" => "Macedonian",
            "sq" | "alb" | "sqi" => "Albanian", "bs" | "bos" => "Bosnian", "bn" | "ben" => "Bengali", "ta" | "tam" => "Tamil",
            "te" | "tel" => "Telugu", "ml" | "mal" => "Malayalam", "ur" | "urd" => "Urdu", "tl" | "tgl" | "fil" => "Filipino",
            "zt" | "zht" => "Chinese (traditional)", "ze" => "Chinese (bilingual)", "ea" | "spl" | "spn" => "Spanish (Latin America)",
            "ka" | "geo" | "kat" => "Georgian", "hy" | "arm" | "hye" => "Armenian", "km" | "khm" => "Khmer", "my" | "bur" | "mya" => "Burmese",
            "af" | "afr" => "Afrikaans", "sw" | "swa" => "Swahili", "kk" | "kaz" => "Kazakh", "mn" | "mon" => "Mongolian", "ne" | "nep" => "Nepali",
            "si" | "sin" => "Sinhala", "eo" | "epo" => "Esperanto", "ga" | "gle" => "Irish", "cy" | "wel" | "cym" => "Welsh", "la" | "lat" => "Latin",
            "und" | "" => "", _ => return code.to_uppercase(),
        }.into()
    }
}

pub mod player {
    pub fn resolution_class(width: i64, height: i64) -> &'static str {
        match () {
            _ if height >= 2000 || width >= 3600 => "4K",
            _ if height >= 1400 || width >= 2400 => "1440p",
            _ if height >= 1000 || width >= 1800 => "1080p",
            _ if height >= 700 || width >= 1200 => "720p",
            _ if height > 0 => "SD",
            _ => "",
        }
    }
}

pub mod updater {
    fn parse(v: &str) -> Vec<u32> {
        v.trim().trim_start_matches(['v', 'V'])
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    }
    pub fn newer(latest: &str, current: &str) -> bool {
        let (a, b) = (parse(latest), parse(current));
        !a.is_empty() && a > b
    }
}

/// The Flutter shell has no native window hooks into the shared backend: no HWND for DXGI's HDR
/// query (Windows) and no EDR headroom (macOS), so the display reads as "unknown"/SDR there.
pub mod video {
    #[cfg(target_os = "windows")]
    pub fn hwnd() -> Option<isize> { None }
    #[cfg(target_os = "macos")]
    pub fn potential_headroom() -> f64 { 0.0 }
}

/// Android glue the shared backend expects. Flutter owns the activity, so the data folder comes
/// from Dart (`set_data_dir`, before `init`) and the Dioxus-only JNI features answer "unavailable".
#[cfg(target_os = "android")]
pub mod android {
    use anyhow::{anyhow, Result};
    use std::path::PathBuf;
    use std::sync::OnceLock;

    pub static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

    pub fn files_dir() -> Result<PathBuf> {
        DATA_DIR.get().cloned().ok_or_else(|| anyhow!("data folder not set yet"))
    }

    #[link(name = "log")]
    extern "C" { fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32; }

    pub fn log(message: &str) {
        if let Ok(text) = std::ffi::CString::new(message) {
            unsafe { __android_log_write(4, c"veylo".as_ptr(), text.as_ptr()); }
        }
    }

    pub fn screen() -> Result<crate::engine::Screen> { Err(anyhow!("screen capabilities unavailable in the Flutter shell")) }
    /// (model dir, libonnxruntime.so) unpacked/located by MainActivity (Kotlin), set from Dart.
    pub static PICKS_AI: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    pub fn picks_ai_paths() -> Result<(PathBuf, PathBuf)> {
        PICKS_AI.get().cloned().ok_or_else(|| anyhow!("Picks model not unpacked yet"))
    }
    pub fn google_id_token(_web_client: &str) -> Result<()> { Err(anyhow!("Google sign-in isn't available in this build")) }
    pub fn scan_code() -> Result<()> { Err(anyhow!("Code scanning isn't available in this build")) }
    pub fn account_result() -> Option<String> { None }
}

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::{Mutex, OnceLock};

type AsyncCallback = extern "C" fn(u64, *mut c_char);

fn media_to_json(m: &backend::Media) -> Value {
    let ep_label = m.last_video_id.as_deref().and_then(backend::episode_label);
    let rating = ["imdbRating", "tmdbRating", "tvmazeRating"]
        .iter()
        .map(|k| m.raw.get(*k).and_then(Value::as_str).unwrap_or_default().trim().to_owned())
        .find(|r| !r.is_empty())
        .unwrap_or_default();
    let runtime = m.raw.get("runtime").and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    let logo = m.raw.get("logo").and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    json!({
        "key": m.key(),
        "id": m.id,
        "type": m.kind,
        "name": m.name,
        "addon_url": m.addon_url,
        "poster": m.poster,
        "posters": backend::posters(m),
        "background": m.background,
        "backdrop": m.backdrop(),
        "description": m.description,
        "year": m.year,
        "genres": m.genres,
        "reason": m.reason,
        "liked": m.liked,
        "watched": m.watched,
        "saved": m.saved,
        "video_watched": m.video_watched,
        "position_ms": m.position_ms,
        "duration_ms": m.duration_ms,
        "last_video_id": m.last_video_id,
        "in_progress": backend::in_progress(m),
        "progress_fraction": backend::progress_fraction(m),
        "resume_context": backend::resume_context(m),
        "episode_label": ep_label,
        "meta_line": m.meta_line(false),
        "meta_line_typed": m.meta_line(true),
        "rating": rating,
        "runtime": runtime,
        "logo": logo,
        "raw": m.raw,
    })
}

fn media_from_arg(v: &Value) -> Result<backend::Media> {
    if let Some(raw) = v.get("raw").filter(|r| r.is_object()) {
        let origin = v.get("addon_url").and_then(Value::as_str).unwrap_or("");
        if let Some(mut m) = backend::Media::from_json(raw, origin) {
            if let Some(s) = v.get("saved").and_then(Value::as_bool) { m.saved = s; }
            if let Some(w) = v.get("watched").and_then(Value::as_bool) { m.watched = w; }
            if let Some(l) = v.get("liked").and_then(Value::as_i64) { m.liked = l; }
            if let Some(p) = v.get("position_ms").and_then(Value::as_i64) { m.position_ms = p; }
            if let Some(d) = v.get("duration_ms").and_then(Value::as_i64) { m.duration_ms = d; }
            if let Some(vw) = v.get("video_watched").and_then(Value::as_bool) { m.video_watched = vw; }
            if let Some(lv) = v.get("last_video_id").and_then(Value::as_str) { m.last_video_id = Some(lv.to_owned()); }
            return Ok(m);
        }
    }
    let origin = v.get("addon_url").and_then(Value::as_str).unwrap_or("");
    backend::Media::from_json(v, origin).ok_or_else(|| anyhow!("invalid media payload"))
}

fn artwork_to_json(a: &backend::Artwork) -> Value {
    let season_posters: serde_json::Map<String, Value> = a.season_posters.iter()
        .map(|(k, v)| (k.to_string(), json!(v))).collect();
    json!({
        "genres": a.genres,
        "backdrop": a.backdrop,
        "backdrop_clean": a.backdrop_clean,
        "logo": a.logo,
        "poster": a.poster,
        "trailers": a.trailers,
        "stills": a.stills,
        "tagline": a.tagline,
        "certificate": a.certificate,
        "language": a.language,
        "season_posters": season_posters,
        "cast": a.cast.iter().map(|p| json!({
            "name": p.name, "role": p.role, "photo": p.photo,
            "tvmaze_id": p.tvmaze_id, "wikidata_id": p.wikidata_id,
        })).collect::<Vec<_>>(),
        "crew": a.crew.iter().map(|(job, list)| json!({
            "job": job,
            "people": list.iter().map(|p| json!({
                "name": p.name, "role": p.role, "photo": p.photo,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "studios": a.studios.iter().map(|s| json!({ "name": s.name, "logo": s.logo })).collect::<Vec<_>>(),
        "videos": a.videos.iter().map(|v| json!({
            "youtube": v.youtube, "name": v.name, "kind": v.kind, "official": v.official,
        })).collect::<Vec<_>>(),
    })
}

/// DX's free-first merge (`components::use_title_artwork`): TVmaze/AniList season posters & cast
/// win over TMDB, the verified textless hero backdrop & logo always win, Wikidata fills gaps.
fn merge_title_art(
    hero: Option<backend::Artwork>,
    series: Option<backend::Artwork>,
    wiki: Option<backend::Artwork>,
) -> Option<backend::Artwork> {
    let combined = match (series, hero) {
        (Some(s), Some(h)) => {
            let mut ff = backend::with_extras(s, h.clone());
            if h.backdrop.is_some() { ff.backdrop = h.backdrop; ff.backdrop_clean = h.backdrop_clean; }
            if h.logo.is_some() { ff.logo = h.logo; }
            Some(ff)
        }
        (s, h) => s.or(h),
    };
    match (combined, wiki) {
        (Some(c), Some(w)) => Some(backend::with_extras(c, w)),
        (c, w) => c.or(w),
    }
}

fn title_meta_json(tm: &backend::TitleMeta) -> Value {
    json!({
        "genres": tm.genres,
        "logo": tm.logo,
        "runtime": tm.runtime,
        "rating": tm.rating,
        "released": tm.released,
        "country": tm.country,
        "language": tm.language,
        "awards": tm.awards,
        "directors": tm.directors,
        "writers": tm.writers,
        "cast": tm.cast,
        "description": tm.description,
        "trailers": tm.trailers.iter().map(|t| json!({"youtube": t.youtube, "url": t.url})).collect::<Vec<_>>(),
        "episodes": tm.episodes.iter().map(|e| json!({
            "id": e.id, "season": e.season, "number": e.number, "title": e.title,
            "thumbnail": e.thumbnail, "overview": e.overview, "released": e.released,
            "date_label": backend::date_label(&e.released),
            "label": e.label(),
        })).collect::<Vec<_>>(),
    })
}

fn source_to_json(s: &backend::Source) -> Value {
    let kind = match s.kind {
        backend::SourceKind::Direct => "direct",
        backend::SourceKind::Torrent => "torrent",
        backend::SourceKind::External => "external",
    };
    json!({
        "addon": s.addon,
        "name": s.name,
        "description": s.description,
        "url": s.url,
        "filename": s.filename,
        "size": s.size,
        "size_bytes": s.size_bytes,
        "quality": s.quality,
        "hdr": s.hdr,
        "kind": kind,
        "headers": s.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "off_title": s.off_title,
        "video_codec": s.video_codec,
    })
}

fn settings_to_json(s: &backend::Settings) -> Value {
    json!({
        "better_posters": s.better_posters,
        "picks_hide_watched": s.picks_hide_watched,
        "stream_language": s.stream_language,
        "hdr_output": s.hdr_output,
        "sub_scale": s.sub_scale,
        "sub_pos": s.sub_pos,
        "sub_bg": s.sub_bg,
        "sub_color": s.sub_color,
        "audio_night_mode": s.audio_night_mode,
        "audio_passthrough": s.audio_passthrough,
        "hwdec_mode": s.hwdec_mode,
        "auto_next": s.auto_next,
        "max_bitrate_mbps": s.max_bitrate_mbps,
        "telemetry_enabled": s.telemetry_enabled,
        "prefetch_sources": s.prefetch_sources,
        "connection_mbps": backend::connection_mbps(),
    })
}

struct PicksSession {
    night: u64,
    brain: brain::Brain,
    pool: Vec<backend::Media>,
    pair: (Option<backend::Media>, Option<backend::Media>),
    shown: Vec<String>,
    kept: Vec<backend::Media>,
    starred: Vec<backend::Media>,
    round: u32,
    streak: u32,
    kind: Option<String>,
    style: picks_rules::Style,
    genres: Vec<String>,
    from: String,
    to: String,
    saved_only: bool,
    watched: HashSet<String>,
    disliked: HashSet<String>,
    liked: HashSet<String>,
    saved: HashSet<String>,
}

static PICKS_SESSION: OnceLock<Mutex<Option<PicksSession>>> = OnceLock::new();
static LINK_OFFER: OnceLock<Mutex<Option<(u64, backend::LinkOffer)>>> = OnceLock::new();
static LINK_OFFER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn account_state_json() -> Value {
    let acc = backend::account();
    let key_needed = backend::sync_key_needed();
    let recovery_code: Option<String> = if acc.is_some() && !key_needed {
        tokio::task::spawn_blocking(backend::sync_recovery_code).await.ok().flatten()
    } else {
        None
    };
    json!({
        "accounts_available": backend::accounts_available(),
        "google_available": backend::google_available(),
        "account": acc.map(|a| json!({
            "uid": a.uid,
            "email": a.email,
            "name": a.name,
            "provider": a.provider,
        })),
        "last_synced": backend::last_synced(),
        "now_secs": backend::now_secs(),
        "sync_key_needed": key_needed,
        "recovery_code": recovery_code,
    })
}

impl PicksSession {
    fn filtered(&self) -> Vec<backend::Media> {
        let hide_watched = backend::load_settings().picks_hide_watched;
        let from = self.from.parse::<i32>().ok().filter(|_| self.from.len() == 4);
        let to = self.to.parse::<i32>().ok().filter(|_| self.to.len() == 4);
        let (from, to) = if from.zip(to).is_some_and(|(a, b)| a > b) { (None, None) } else { (from, to) };
        let this_year = backend::civil(backend::now_secs()).0 as i32;
        let base = if self.saved_only {
            self.pool.iter().filter(|m| self.saved.contains(&brain::id(m)) || m.saved).cloned().collect()
        } else {
            picks_rules::filter_picks(&self.pool, self.kind.as_deref(), self.style, &self.genres, from, to, this_year)
        };
        base.into_iter().filter(|m| {
            let id = brain::id(m);
            if self.disliked.contains(&id) { return false; }
            if hide_watched && (self.watched.contains(&m.id) || self.watched.contains(&picks_rules::title_key(m))) { return false; }
            true
        }).collect()
    }

    fn avoid(&self) -> Vec<String> {
        let mut ex = self.shown.clone();
        ex.extend(self.kept.iter().chain(self.starred.iter()).flat_map(brain::identities));
        ex
    }

    fn mark_shown(&mut self, m: &Option<backend::Media>) {
        if let Some(m) = m {
            for name in brain::identities(m) {
                if !self.shown.contains(&name) { self.shown.push(name); }
            }
        }
    }

    fn ensure_pair(&mut self) {
        if self.pair.0.is_some() && self.pair.1.is_some() { return; }
        let list = self.filtered();
        let ex = self.avoid();
        let (l, r) = self.brain.opening_pair(&list, &ex);
        self.mark_shown(&l);
        self.mark_shown(&r);
        self.pair = (l, r);
    }

    fn to_json(&self) -> Value {
        let list = self.filtered();
        let reference = self.brain.reference(&list);
        let card_info = |m: &Option<backend::Media>, is_left: bool| -> Value {
            let Some(m) = m else { return Value::Null };
            let matching = if self.brain.votes_tonight() > 0 || !self.brain.moods().is_empty() {
                Some((self.brain.match_of(m, &reference) * 100.0).round().clamp(1.0, 99.0) as u8)
            } else { None };
            let wins = if self.kept.first().is_some_and(|k| k.key() == m.key()) { self.streak } else { 0 };
            let id = brain::id(m);
            let _ = is_left;
            json!({
                "media": media_to_json(m),
                "matching": matching,
                "why": self.brain.why(m, &self.kept).map(|w| w.text()),
                "feels": self.brain.feels(m),
                "wins": wins,
                "liked": self.liked.contains(&id),
                "saved": self.saved.contains(&id),
            })
        };
        let mut all_short: Vec<backend::Media> = self.starred.clone();
        for m in &self.kept {
            if !all_short.iter().any(|x| x.key() == m.key()) { all_short.push(m.clone()); }
        }
        let shortlist: Vec<Value> = self.brain.ranked(&all_short).into_iter().take(8).map(|m| {
            let pct = (self.brain.match_of(&m, &reference) * 100.0).round().clamp(1.0, 99.0) as u8;
            let star = self.starred.iter().any(|x| x.key() == m.key());
            json!({ "media": media_to_json(&m), "star": star, "pct": pct })
        }).collect();
        let leaning: Vec<String> = self.brain.leaning().into_iter().map(|(l, _)| l).collect();
        let mut gcounts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for m in &self.pool {
            for g in picks_rules::genre_choices(m) { *gcounts.entry(g).or_default() += 1; }
        }
        let mut gvec: Vec<(String, usize)> = gcounts.into_iter().collect();
        gvec.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let genres: Vec<String> = gvec.into_iter().take(24).map(|(g, _)| g).collect();
        let available_moods: Vec<Value> = brain::MOODS.iter().map(|(l, p)| json!({"label": l, "phrase": p})).collect();
        let learning = self.brain.learning(&list, &reference);
        let lamps = ((learning * 5.0).ceil() as usize).min(5);
        json!({
            "left": card_info(&self.pair.0, true),
            "right": card_info(&self.pair.1, false),
            "votes": self.brain.votes_tonight(),
            "lamps": lamps,
            "leaning": leaning,
            "moods": self.brain.moods(),
            "available_moods": available_moods,
            "shortlist": shortlist,
            "kind": self.kind,
            "style": self.style.as_str(),
            "genres": self.genres,
            "available_genres": genres,
            "from": self.from,
            "to": self.to,
            "saved_only": self.saved_only,
            "hide_watched": backend::load_settings().picks_hide_watched,
            "ai_failed": picks_ai::failed(),
            "pool_count": list.len(),
        })
    }
}

async fn handle_request(req: Value) -> Result<Value> {
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    match op {
        "set_picks_ai_paths" => {
            #[cfg(target_os = "android")]
            if let (Some(model), Some(runtime)) = (req["model"].as_str(), req["runtime"].as_str()) {
                let _ = android::PICKS_AI.set((model.into(), runtime.into()));
            }
            Ok(json!({}))
        }
        "set_data_dir" => {
            #[cfg(target_os = "android")]
            if let Some(dir) = req["path"].as_str() { let _ = android::DATA_DIR.set(dir.into()); }
            Ok(json!({}))
        }
        "init" => {
            backend::init()?;
            crash::init();
            backend::start_sync();
            // Warm the on-device Picks AI in the background (ONNX session, tokenizer and the mood
            // anchors, all cached), so the first Vibe search or Picks load doesn't pay for it.
            static WARMED: std::sync::Once = std::sync::Once::new();
            WARMED.call_once(|| {
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    if !picks_ai::failed() { let _ = brain::anchors().await; }
                });
            });
            Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "demo_stream": backend::DEMO_STREAM,
                "ai_failed": picks_ai::failed(),
                "accounts_available": backend::accounts_available(),
                "google_available": backend::google_available(),
                "sync_key_needed": backend::sync_key_needed(),
            }))
        }
        "picks_ai_check" => {
            let (space, galaxy, baking) = tokio::try_join!(
                picks_ai::embed_query("space adventure"),
                picks_ai::embed_query("astronauts explore a distant galaxy"),
                picks_ai::embed_query("a cooking show about baking bread"),
            )?;
            let near = picks_ai::cosine(&space, &galaxy);
            let far = picks_ai::cosine(&space, &baking);
            if space.len() != picks_ai::DIM || near <= far {
                return Err(anyhow!("unexpected Picks AI embedding (dim={}, near={near:.3}, far={far:.3})", space.len()));
            }
            Ok(json!({
                "ok": true,
                "dim": space.len(),
                "near": near,
                "far": far,
                "ai_failed": picks_ai::failed(),
            }))
        }
        "home_rows" => {
            let rows = backend::home_rows().await?;
            Ok(json!({
                "rows": rows.into_iter().map(|r| json!({
                    "key": r.key,
                    "title": r.title,
                    "subtitle": r.subtitle,
                    "addon_url": r.addon_url,
                    "kind": r.kind,
                    "id": r.id,
                    "extra": r.extra.map(|(a, b)| json!([a, b])),
                    "ranked": r.ranked,
                })).collect::<Vec<_>>()
            }))
        }
        "catalog" => {
            let r = &req["row"];
            let extra = r.get("extra").and_then(Value::as_array).and_then(|a| {
                Some((a.first()?.as_str()?.to_owned(), a.get(1)?.as_str()?.to_owned()))
            });
            let row = backend::CatalogRow {
                key: r["key"].as_str().unwrap_or("").to_owned(),
                title: r["title"].as_str().unwrap_or("").to_owned(),
                subtitle: r["subtitle"].as_str().map(str::to_owned),
                addon_url: r["addon_url"].as_str().unwrap_or("").to_owned(),
                kind: r["kind"].as_str().unwrap_or("").to_owned(),
                id: r["id"].as_str().unwrap_or("").to_owned(),
                extra,
                ranked: r["ranked"].as_bool().unwrap_or(false),
            };
            let items = backend::catalog(row).await?;
            Ok(json!({ "items": items.iter().map(media_to_json).collect::<Vec<_>>() }))
        }
        "library" => {
            let items = backend::library().await?;
            Ok(json!({ "items": items.iter().map(media_to_json).collect::<Vec<_>>() }))
        }
        "tonight_picks" => {
            let p = backend::picks().await?;
            let fresh: Vec<backend::Media> = p.items.into_iter()
                .filter(|m| !m.watched && !backend::in_progress(m)).take(24).collect();
            let fresh = backend::filter_home_quality(fresh).await;
            let known = backend::library().await.unwrap_or_default();
            let ranked = tokio::task::spawn_blocking(move || {
                let mut taste = brain::load(0);
                taste.set_facts(brain::facts(&known));
                let mut scored: Vec<(f64, backend::Media)> = fresh.into_iter().enumerate().map(|(i, mut m)| {
                    if let Some(r) = taste.home_reason(&m) { m.reason = r; }
                    (taste.lasting(&m) - i as f64 * 0.02, m)
                }).collect();
                scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                scored.into_iter().map(|(_, m)| m).take(6).collect::<Vec<_>>()
            }).await.unwrap_or_default();
            Ok(json!({ "items": ranked.iter().map(media_to_json).collect::<Vec<_>>() }))
        }
        "artwork" => {
            let m = media_from_arg(&req["media"])?;
            let art = backend::artwork(&m).await;
            Ok(json!({ "artwork": art.as_ref().map(artwork_to_json) }))
        }
        "accent" => {
            let url = req["url"].as_str().unwrap_or("").to_owned();
            let hex = backend::accent(url).await;
            Ok(json!({ "accent": hex }))
        }
        "ambient_url" => {
            let url = req["url"].as_str().unwrap_or("");
            Ok(json!({ "url": backend::ambient_url(url) }))
        }
        "recent_viewing" => {
            let limit = req["limit"].as_u64().unwrap_or(4) as usize;
            let list = backend::recent_viewing(limit).await?;
            let now = backend::now_secs();
            Ok(json!({
                "items": list.iter().map(|s| json!({
                    "media": media_to_json(&s.media),
                    "at": s.at,
                    "episode": s.episode,
                    "when_label": backend::when_label(s.at, now),
                })).collect::<Vec<_>>()
            }))
        }
        "viewing_history" => {
            let list = backend::viewing_history().await?;
            let now = backend::now_secs();
            let this_year = backend::civil(now).0;
            Ok(json!({
                "now": now,
                "this_year": this_year,
                "items": list.iter().map(|s| {
                    let (y, mo, _, _) = backend::civil(s.at);
                    json!({
                        "media": media_to_json(&s.media),
                        "at": s.at,
                        "episode": s.episode,
                        "when_label": backend::when_label(s.at, now),
                        "is_this_week": now - s.at < 7 * 86_400,
                        "year": y,
                        "month": mo,
                        "month_label": if y == this_year { backend::month_name(mo).to_owned() } else { format!("{} {y}", backend::month_name(mo)) },
                    })
                }).collect::<Vec<_>>()
            }))
        }
        "title_cached" => {
            // Synchronous session caches only: lets a re-opened title paint its final layout on frame 0.
            let m = media_from_arg(&req["media"])?;
            let hero = backend::merge_title_stills(
                backend::cached_title_artwork(&m),
                &backend::cached_title_stills(&m).unwrap_or_default(),
            );
            let art = merge_title_art(hero, backend::cached_series_free(&m), backend::cached_wikidata(&m));
            Ok(json!({
                "meta": backend::cached_title_meta(&m).as_ref().map(title_meta_json),
                "artwork": art.as_ref().map(artwork_to_json),
                "stored": title_store::read(&m.key()),
            }))
        }
        "title_details" => {
            // Only what the page's first paint needs (~0.1–0.7 s cold): addon meta, hero art,
            // stills, free series art and local progress. Scores, collection/similar and
            // Wikidata each come from their own op, so the slowest (a saga lookup can take
            // 10+ s) never holds the page back.
            let m = media_from_arg(&req["media"])?;
            // Stills (JustWatch, up to 4 s cold) and free series art come from `title_art`.
            let (meta_res, hero_art, stored, watched_eps, ep_prog) = tokio::join!(
                backend::title_meta(&m),
                backend::title_artwork_fast(&m),
                backend::stored(&m),
                backend::watched_episodes(&m),
                backend::episode_playback_map(&m),
            );
            let hero_merged = backend::merge_title_stills(hero_art, &backend::cached_title_stills(&m).unwrap_or_default());
            let full_art = merge_title_art(hero_merged, backend::cached_series_free(&m), None);
            let meta_json = match meta_res {
                Ok(tm) => title_meta_json(&tm),
                Err(e) => json!({ "error": format!("{e:#}") }),
            };
            let ep_prog_map: serde_json::Map<String, Value> = ep_prog.into_iter()
                .map(|(k, (pos, dur, w))| (k, json!({"position_ms": pos, "duration_ms": dur, "watched": w}))).collect();
            Ok(json!({
                "meta": meta_json,
                "artwork": full_art.as_ref().map(artwork_to_json),
                "stored": stored.as_ref().map(media_to_json),
                "watched_episodes": watched_eps.into_iter().collect::<Vec<_>>(),
                "episode_progress": ep_prog_map,
            }))
        }
        "title_scores" => {
            let m = media_from_arg(&req["media"])?;
            let (scores, status, rank) = tokio::join!(
                backend::review_scores(&m),
                async { if m.kind != "movie" { backend::series_status(&m).await } else { None } },
                backend::today_rank(&m),
            );
            Ok(json!({
                "scores": { "tomatometer": scores.tomatometer, "metascore": scores.metascore },
                "status": status,
                "rank": rank,
            }))
        }
        "title_art" => {
            // Hero art + stills + free series art (posters per season, billing-order cast).
            let m = media_from_arg(&req["media"])?;
            let (hero_art, stills, series_art) = tokio::join!(
                backend::title_artwork_fast(&m),
                backend::title_stills(&m),
                async { if m.kind != "movie" { backend::open_series_free(&m).await } else { None } },
            );
            let art = merge_title_art(backend::merge_title_stills(hero_art, &stills), series_art, None);
            Ok(json!({ "artwork": art.as_ref().map(artwork_to_json) }))
        }
        "title_similar" => {
            let m = media_from_arg(&req["media"])?;
            let sim = backend::similar_titles(&m).await;
            Ok(json!({ "similar": sim.iter().map(media_to_json).collect::<Vec<_>>() }))
        }
        "title_collection" => {
            // The saga lookup can take 10+ s cold: its own op so nothing else waits for it.
            let m = media_from_arg(&req["media"])?;
            let col = backend::collection_parts(&m).await;
            Ok(json!({
                "collection": col.map(|c| json!({ "name": c.name, "parts": c.parts.iter().map(media_to_json).collect::<Vec<_>>() })),
            }))
        }
        "title_studios" => {
            // Studio idents on their own fast path (keyless Wikidata API + disk cache), so the
            // logos don't wait for the big Wikidata query.
            let m = media_from_arg(&req["media"])?;
            // A failed lookup is an error (the slow path stays eligible), not "no studios".
            let list = studios::studios(&m.id).await.ok_or_else(|| anyhow!("studio lookup unavailable"))?;
            let idents = backend::idents(list).await;
            Ok(json!({ "idents": idents.iter().map(|s| json!({"name": s.name, "logo": s.logo})).collect::<Vec<_>>() }))
        }
        "title_prefetch_art" => {
            // The hero backdrop + logo a title page will paint first, so the UI can download and
            // decode them while the pointer rests on the card.
            let m = media_from_arg(&req["media"])?;
            let hero = backend::merge_title_stills(
                backend::title_artwork_fast(&m).await,
                &backend::cached_title_stills(&m).unwrap_or_default(),
            );
            let art = merge_title_art(hero, backend::cached_series_free(&m), None);
            Ok(json!({
                "backdrop": art.as_ref().and_then(|a| a.backdrop.clone()),
                "logo": art.as_ref().and_then(|a| a.logo.clone()),
            }))
        }
        "title_prefetch" => {
            // Hovering/focusing a card warms the caches the title page reads on open, so opening
            // it paints its final layout from the session cache at once. Fire-and-forget.
            let m = media_from_arg(&req["media"])?;
            tokio::spawn(async move {
                let _ = tokio::join!(
                    backend::title_meta(&m),
                    backend::title_artwork_fast(&m),
                    backend::title_stills(&m),
                    async { if m.kind != "movie" { backend::open_series_free(&m).await } else { None } },
                    // Studios only come from Wikidata (0.4–3 s cold) and each logo is then checked:
                    // warming both here is what makes the idents row appear with the page.
                    async {
                        if let Some(list) = studios::studios(&m.id).await {
                            let _ = backend::idents(list).await;
                        }
                    },
                    backend::open_wikidata(&m),
                );
            });
            Ok(json!({}))
        }
        "title_extras" => {
            // Slow layer: Wikidata credits/certificate/studios, then studio idents. Fast pieces are
            // already cached by `title_details`, so re-merging them here is free.
            let m = media_from_arg(&req["media"])?;
            let (hero_art, stills, series_art, wiki_art) = tokio::join!(
                backend::title_artwork_fast(&m),
                backend::title_stills(&m),
                async { if m.kind != "movie" { backend::open_series_free(&m).await } else { None } },
                backend::open_wikidata(&m),
            );
            let full_art = merge_title_art(backend::merge_title_stills(hero_art, &stills), series_art, wiki_art);
            let idents = match full_art.as_ref() {
                Some(a) => backend::idents(a.studios.clone()).await,
                None => Vec::new(),
            };
            Ok(json!({
                "artwork": full_art.as_ref().map(artwork_to_json),
                "idents": idents.iter().map(|s| json!({"name": s.name, "logo": s.logo})).collect::<Vec<_>>(),
            }))
        }
        "search" => {
            let query = req["query"].as_str().unwrap_or("").to_owned();
            let vibe = req["vibe"].as_bool().unwrap_or(false);
            if vibe {
                let list = backend::semantic_search(query).await?;
                Ok(json!({ "vibe": true, "items": list.iter().map(media_to_json).collect::<Vec<_>>() }))
            } else {
                let out = backend::search(query.clone()).await?;
                let top_rank = out.results.first().map(|(_, m)| backend::search_rank(&query, &m.name)).unwrap_or(99);
                Ok(json!({
                    "vibe": false,
                    "top_rank": top_rank,
                    "catalogs": out.catalogs,
                    "failed": out.failed,
                    "results": out.results.iter().map(|(addon, m)| json!({
                        "addon": addon,
                        "media": media_to_json(m),
                    })).collect::<Vec<_>>(),
                }))
            }
        }
        "search_history" => {
            let h = backend::load_history();
            Ok(json!({
                "queries": h.queries,
                "items": h.items.iter().map(media_to_json).collect::<Vec<_>>(),
            }))
        }
        "remember_query" => {
            let mut h = backend::load_history();
            if let Some(q) = req["query"].as_str() { backend::remember_query(&mut h, q); }
            Ok(json!({"ok": true}))
        }
        "remember_item" => {
            let mut h = backend::load_history();
            let m = media_from_arg(&req["media"])?;
            backend::remember_item(&mut h, &m);
            Ok(json!({"ok": true}))
        }
        "clear_history" => {
            let mut h = backend::load_history();
            backend::clear_history(&mut h);
            Ok(json!({"ok": true}))
        }
        "sources" => {
            let m = media_from_arg(&req["media"])?;
            let video = req["video_id"].as_str().unwrap_or(&m.id).to_owned();
            let pack = req["pack"].as_u64().map(|n| n as usize);
            let (list, failed) = backend::sources(&m.kind, &video).await?;
            let ordered = backend::play_order(&list, pack);
            Ok(json!({
                "sources": ordered.iter().map(source_to_json).collect::<Vec<_>>(),
                "failed": failed,
            }))
        }
        "play_target" => {
            let m = media_from_arg(&req["media"])?;
            if m.kind == "movie" {
                let stored = backend::stored(&m).await.unwrap_or_else(|| m.clone());
                let start_ms = if backend::in_progress(&stored) { stored.position_ms } else { 0 };
                return Ok(json!({
                    "video_id": m.id,
                    "episode_label": Value::Null,
                    "start_ms": start_ms,
                    "pack": Value::Null,
                    "season_episodes": [],
                    "next_episode": Value::Null,
                }));
            }
            let (stored_opt, meta_res, seen) = tokio::join!(
                backend::stored(&m),
                backend::title_meta(&m),
                backend::watched_episodes(&m),
            );
            let stored = stored_opt.unwrap_or_else(|| m.clone());
            let episodes = meta_res.map(|tm| tm.episodes).unwrap_or_default();
            let now = backend::now_secs();
            let picked = picks_rules::card_playback(&stored, &episodes, &seen, now);
            let video_id = picked.as_ref().map(|p| p.video.clone())
                .or_else(|| stored.last_video_id.clone().filter(|v| backend::in_progress(&stored) && !seen.contains(v)))
                .unwrap_or_else(|| format!("{}:1:1", m.id));
            let matched_ep = episodes.iter().find(|e| e.id == video_id);
            let ep_label = matched_ep.map(|e| e.label())
                .or_else(|| backend::episode_label(&video_id));
            let season_num = matched_ep.map(|e| e.season).unwrap_or(1);
            let mut season_eps: Vec<&backend::Episode> = episodes.iter()
                .filter(|e| e.season == season_num && e.number > 0)
                .collect();
            season_eps.sort_by_key(|e| e.number);
            let ep_json = |e: &backend::Episode| json!({
                "id": e.id,
                "season": e.season,
                "number": e.number,
                "title": e.title,
                "thumbnail": e.thumbnail,
                "overview": e.overview,
                "released": e.released,
                "date_label": e.released,
                "label": e.label(),
            });
            let next_ep = season_eps.iter().position(|e| e.id == video_id)
                .and_then(|i| season_eps.get(i + 1).copied())
                .map(ep_json);
            let start_ms = if backend::in_progress(&stored) && stored.last_video_id.as_deref() == Some(video_id.as_str()) {
                stored.position_ms
            } else {
                0
            };
            Ok(json!({
                "video_id": video_id,
                "episode_label": ep_label,
                "start_ms": start_ms,
                "pack": if season_eps.is_empty() { Value::Null } else { json!(season_eps.len()) },
                "season_episodes": season_eps.iter().map(|e| ep_json(e)).collect::<Vec<_>>(),
                "next_episode": next_ep,
            }))
        }
        "autoplay" => {
            let m = media_from_arg(&req["media"])?;
            let video = req["video_id"].as_str().unwrap_or(&m.id).to_owned();
            let pack = req["pack"].as_u64().map(|n| n as usize);
            if let Some(dl) = backend::find_download(&m.key(), &video) {
                return Ok(json!({
                    "offline": {
                        "path": dl.path.to_string_lossy(),
                        "title": dl.title,
                        "subtitle": dl.subtitle,
                        "bytes": dl.bytes,
                    }
                }));
            }
            let mut ctx = backend::autoplay_context_for_media(&m, &video, pack);
            // If metadata wasn't in memory yet (e.g. direct Play from Home/Picks/Search), briefly
            // fetch title_meta so `original_language` (e.g. Korean for Parasite) & runtime are known
            // both for MKV audio-track ranking and for libmpv's `alang` fallback.
            if ctx.original_language.is_none() || ctx.expected_seconds.is_none() {
                if let Ok(Ok(tm)) = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    backend::title_meta(&m),
                ).await {
                    if ctx.original_language.is_none() {
                        ctx.original_language = backend::language_key_of(&tm.language).map(str::to_owned);
                    }
                    if ctx.expected_seconds.is_none() {
                        ctx.expected_seconds = tm.runtime.split_whitespace().next()
                            .and_then(|n| n.parse::<f64>().ok())
                            .filter(|&mins| (1.0..=600.0).contains(&mins))
                            .map(|mins| mins * 60.0);
                    }
                }
            }
            let original_language = ctx.original_language.clone();
            let pick = backend::autoplay_for(&m.kind, &video, ctx).await;
            Ok(json!({
                "best": pick.best.as_ref().map(source_to_json),
                "rest": pick.rest.iter().take(15).map(source_to_json).collect::<Vec<_>>(),
                "debrid": pick.debrid,
                "original_language": original_language,
                "message": if pick.best.is_none() { Some(backend::no_source_message(pick.debrid.as_deref())) } else { None },
            }))
        }
        "feedback" => {
            let m = media_from_arg(&req["media"])?;
            let value = req["value"].as_i64().unwrap_or(0);
            backend::feedback(&m, value).await?;
            Ok(json!({"ok": true}))
        }
        "set_saved" => {
            let m = media_from_arg(&req["media"])?;
            let saved = req["saved"].as_bool().unwrap_or(false);
            backend::set_saved(&m, saved).await?;
            Ok(json!({"ok": true}))
        }
        "set_watched" => {
            let m = media_from_arg(&req["media"])?;
            let watched = req["watched"].as_bool().unwrap_or(false);
            backend::set_watched(&m, watched).await?;
            Ok(json!({"ok": true}))
        }
        "set_episodes_watched" => {
            let m = media_from_arg(&req["media"])?;
            let ids: Vec<String> = req["video_ids"].as_array().into_iter().flatten()
                .filter_map(|v| v.as_str().map(str::to_owned)).collect();
            let watched = req["watched"].as_bool().unwrap_or(false);
            backend::set_episodes_watched(&m, ids, watched).await?;
            Ok(json!({"ok": true}))
        }
        "forget" => {
            let m = media_from_arg(&req["media"])?;
            backend::forget(&m).await?;
            Ok(json!({"ok": true}))
        }
        "record_progress" => {
            let m = media_from_arg(&req["media"])?;
            let video_id = req["video_id"].as_str().map(str::to_owned);
            let pos = req["position_ms"].as_i64().unwrap_or(0);
            let dur = req["duration_ms"].as_i64().unwrap_or(0);
            let watched_ms = req["watched_ms"].as_i64().unwrap_or(0);
            backend::record_progress(m, video_id, pos, dur, watched_ms, true);
            Ok(json!({"ok": true}))
        }
        "flush_progress" => {
            backend::flush_progress().await;
            Ok(json!({"ok": true}))
        }
        "online_segments" => {
            let video = req["video_id"].as_str().unwrap_or("").to_owned();
            let dur = req["duration"].as_f64().unwrap_or(0.0);
            let segs = backend::online_segments(&video, dur).await.unwrap_or_default();
            Ok(json!({
                "segments": segs.iter().map(|s| json!({
                    "kind": match s.kind {
                        engine::SegmentKind::Intro => "intro",
                        engine::SegmentKind::Recap => "recap",
                        engine::SegmentKind::Credits => "credits",
                    },
                    "start": s.start,
                    "end": s.end,
                })).collect::<Vec<_>>()
            }))
        }
        "online_subtitles" => {
            let kind = req["kind"].as_str().unwrap_or("movie").to_owned();
            let video = req["video_id"].as_str().unwrap_or("").to_owned();
            let fname = req["filename"].as_str().map(str::to_owned);
            let fsize = req["file_size"].as_u64();
            let subs = backend::online_subtitles(&kind, &video, fname.as_deref(), fsize).await?;
            Ok(json!({
                "subtitles": subs.iter().map(|s| json!({
                    "id": s.id,
                    "url": s.url,
                    "code": s.code,
                    "language": s.language,
                    "release": s.release,
                })).collect::<Vec<_>>()
            }))
        }
        "downloads_list" => {
            let done = backend::list_downloads();
            let active = backend::active_downloads();
            Ok(json!({
                "downloads": done.iter().map(|d| json!({
                    "key": d.key, "media": media_to_json(&d.media), "video_id": d.video_id,
                    "title": d.title, "subtitle": d.subtitle, "path": d.path.to_string_lossy(),
                    "bytes": d.bytes, "size_label": backend::file_size_label(d.bytes),
                })).collect::<Vec<_>>(),
                "active": active.iter().map(|a| json!({
                    "key": a.key, "title": a.title, "downloaded": a.downloaded, "total": a.total,
                    "downloaded_label": backend::file_size_label(a.downloaded),
                    "total_label": a.total.map(backend::file_size_label),
                    "state": match &a.state { backend::DownloadState::Running => "running", backend::DownloadState::Failed(_) => "failed" },
                    "error": match &a.state { backend::DownloadState::Failed(e) => Some(e.clone()), _ => None },
                })).collect::<Vec<_>>(),
            }))
        }
        "start_download" => {
            let m = media_from_arg(&req["media"])?;
            let video = req["video_id"].as_str().unwrap_or(&m.id).to_owned();
            let title = req["title"].as_str().unwrap_or(&m.name).to_owned();
            let subtitle = req["subtitle"].as_str().unwrap_or("").to_owned();
            let url = req["url"].as_str().unwrap_or("").to_owned();
            let fname = req["filename"].as_str().map(str::to_owned);
            let headers: Vec<(String, String)> = req["headers"].as_array().into_iter().flatten().filter_map(|p| {
                let a = p.as_array()?;
                Some((a.first()?.as_str()?.to_owned(), a.get(1)?.as_str()?.to_owned()))
            }).collect();
            backend::start_download(m, video, title, subtitle, url, headers, fname)?;
            Ok(json!({"ok": true}))
        }
        "cancel_download" => {
            if let Some(k) = req["key"].as_str() { backend::cancel_download(k); }
            Ok(json!({"ok": true}))
        }
        "remove_download" => {
            if let Some(k) = req["key"].as_str() { backend::remove_download(k)?; }
            Ok(json!({"ok": true}))
        }
        "settings_get" => {
            let s = backend::load_settings();
            Ok(json!({
                "settings": settings_to_json(&s),
                "languages": addons::LANGUAGES.iter().map(|l| json!({"key": l.key, "label": l.label})).collect::<Vec<_>>(),
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }
        "settings_update" => {
            let patch = req["patch"].clone();
            let s = backend::update_settings(|s| {
                if let Some(v) = patch.get("better_posters").and_then(Value::as_bool) { s.better_posters = v; }
                if let Some(v) = patch.get("picks_hide_watched").and_then(Value::as_bool) { s.picks_hide_watched = v; }
                if let Some(v) = patch.get("stream_language").and_then(Value::as_str) { s.stream_language = Some(v.to_owned()); }
                if let Some(v) = patch.get("hdr_output").and_then(Value::as_bool) { s.hdr_output = v; }
                if let Some(v) = patch.get("auto_next").and_then(Value::as_bool) { s.auto_next = v; }
                if let Some(v) = patch.get("audio_passthrough").and_then(Value::as_bool) { s.audio_passthrough = v; }
                if let Some(v) = patch.get("audio_night_mode").and_then(Value::as_bool) { s.audio_night_mode = v; }
                if let Some(v) = patch.get("prefetch_sources").and_then(Value::as_bool) { s.prefetch_sources = v; }
                if let Some(v) = patch.get("telemetry_enabled").and_then(Value::as_bool) { s.telemetry_enabled = v; }
                if let Some(v) = patch.get("hwdec_mode").and_then(Value::as_str) { s.hwdec_mode = v.to_owned(); }
                if let Some(v) = patch.get("sub_scale").and_then(Value::as_f64) { s.sub_scale = v.clamp(0.6, 2.0); }
                if let Some(v) = patch.get("sub_pos").and_then(Value::as_i64) { s.sub_pos = v.clamp(70, 100) as i32; }
                if let Some(v) = patch.get("sub_bg").and_then(Value::as_bool) { s.sub_bg = v; }
                if let Some(v) = patch.get("sub_color").and_then(Value::as_str) { s.sub_color = v.to_owned(); }
                if patch.get("max_bitrate_mbps").is_some() {
                    s.max_bitrate_mbps = patch["max_bitrate_mbps"].as_u64().map(|n| n as u32);
                }
            });
            Ok(json!({ "settings": settings_to_json(&s) }))
        }
        "addons_overview" => {
            let ov = addons::overview().await;
            Ok(json!({
                "updates": ov.updates,
                "addons": ov.addons.iter().enumerate().map(|(idx, a)| json!({
                    "idx": idx,
                    "name": a.name,
                    "version": a.version,
                    "offers": a.offers,
                    "update": a.update,
                    "configurable": a.configurable,
                    "removable": a.removable(),
                })).collect::<Vec<_>>(),
            }))
        }
        "addons_check" => {
            let (n, note) = addons::check().await;
            Ok(json!({ "updates": n, "note": note }))
        }
        "addon_install" => {
            let url = req["url"].as_str().unwrap_or("").to_owned();
            let name = addons::install_custom(url).await?;
            Ok(json!({ "name": name }))
        }
        "addon_remove" => {
            let idx = req["idx"].as_u64().ok_or_else(|| anyhow!("missing idx"))? as usize;
            let ov = addons::overview().await;
            let row = ov.addons.get(idx).ok_or_else(|| anyhow!("add-on not found"))?;
            addons::remove(row).await?;
            Ok(json!({"ok": true}))
        }
        "addon_update" => {
            let idx = req["idx"].as_u64().ok_or_else(|| anyhow!("missing idx"))? as usize;
            let ov = addons::overview().await;
            let row = ov.addons.get(idx).ok_or_else(|| anyhow!("add-on not found"))?;
            addons::update(row).await?;
            Ok(json!({"ok": true}))
        }
        "addon_configure" => {
            let idx = req["idx"].as_u64().ok_or_else(|| anyhow!("missing idx"))? as usize;
            let ov = addons::overview().await;
            if let Some(row) = ov.addons.get(idx) {
                row.open_configure();
            }
            Ok(json!({"ok": true}))
        }
        "addon_import_stremio" => {
            let email = req["email"].as_str().unwrap_or("").to_owned();
            let password = req["password"].as_str().unwrap_or("").to_owned();
            let (ok, failed) = addons::import_from_stremio(email, password).await?;
            Ok(json!({ "ok": ok, "failed": failed }))
        }
        "addon_export_bundle" => {
            let (count, bundle) = addons::export_bundle().await?;
            Ok(json!({ "count": count, "bundle": bundle }))
        }
        "addon_import_bundle" => {
            let raw = req["raw"].as_str().unwrap_or("");
            let (ok, failed) = addons::import_bundle(raw).await?;
            Ok(json!({ "ok": ok, "failed": failed }))
        }
        "addon_fetch_directory" => {
            let url = req["url"].as_str().unwrap_or("").to_owned();
            let list = addons::fetch_directory(url).await?;
            Ok(json!({
                "entries": list.into_iter().map(|e| json!({
                    "name": e.name,
                    "version": e.version,
                    "description": e.description,
                    "offers": e.offers,
                    "manifest_url": e.manifest_url,
                    "installed": e.installed,
                })).collect::<Vec<_>>()
            }))
        }
        "history_export" => {
            let path = backend::export_watch_history().await?;
            Ok(json!({ "path": path }))
        }
        "history_import" => {
            let raw = req["raw"].as_str().unwrap_or("");
            let imported = backend::import_watch_history(raw).await?;
            Ok(json!({ "imported": imported }))
        }
        "report_error" => {
            crash::dart_error(
                req["source"].as_str().unwrap_or("dart"),
                req["error"].as_str().unwrap_or(""),
                req["stack"].as_str().unwrap_or(""),
                req["fatal"].as_bool().unwrap_or(false),
            );
            Ok(json!({}))
        }
        "crash_reporting" => Ok(json!({
            "available": crash::active(),
            "enabled": crash::active() && backend::telemetry_enabled(),
        })),
        "force_reports" => {
            crash::force_reports();
            if let Some(t) = req["target"].as_str() {
                let t = t.to_owned();
                sentry::configure_scope(|scope| scope.set_tag("test_target", t));
            }
            Ok(json!({ "available": crash::active() }))
        }
        "flush_reports" => {
            tokio::task::spawn_blocking(crash::flush).await.ok();
            Ok(json!({}))
        }
        "diagnostic_report" => {
            let report = backend::diagnostic_report();
            Ok(json!({ "report": report }))
        }
        "account_state" => Ok(account_state_json().await),
        "account_sign_in_google" => {
            backend::sign_in_google().await?;
            Ok(account_state_json().await)
        }
        "account_sign_in" => {
            let email = req["email"].as_str().unwrap_or("").to_owned();
            let password = req["password"].as_str().unwrap_or("").to_owned();
            backend::sign_in(email, password).await?;
            Ok(account_state_json().await)
        }
        "account_sign_up" => {
            let email = req["email"].as_str().unwrap_or("").to_owned();
            let password = req["password"].as_str().unwrap_or("").to_owned();
            backend::sign_up(email, password).await?;
            Ok(account_state_json().await)
        }
        "account_reset_password" => {
            let email = req["email"].as_str().unwrap_or("").to_owned();
            backend::reset_password(email).await?;
            Ok(json!({"ok": true}))
        }
        "account_sync_now" => {
            let (took, sent) = backend::sync_now().await?;
            let mut state = account_state_json().await;
            state["took"] = json!(took);
            state["sent"] = json!(sent);
            Ok(state)
        }
        "account_set_recovery_code" => {
            let code = req["code"].as_str().unwrap_or("").to_owned();
            let (took, sent) = backend::set_sync_recovery_code(&code).await?;
            let mut state = account_state_json().await;
            state["took"] = json!(took);
            state["sent"] = json!(sent);
            Ok(state)
        }
        "account_sign_out" => {
            backend::sign_out();
            Ok(account_state_json().await)
        }
        "account_delete" => {
            backend::delete_account().await?;
            Ok(account_state_json().await)
        }
        "account_offer_link" => {
            let seq = LINK_OFFER_SEQ.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
            if let Ok(mut g) = LINK_OFFER.get_or_init(|| Mutex::new(None)).lock() {
                if let Some((_, old)) = g.take() {
                    backend::cancel_link_soon(old);
                }
            }
            let offer = backend::offer_link().await?;
            // If another offer or cancel ran while offer_link() was in flight, cancel immediately.
            if LINK_OFFER_SEQ.load(std::sync::atomic::Ordering::Acquire) != seq {
                backend::cancel_link_soon(offer);
                return Ok(json!({ "cancelled": true }));
            }
            let check_code = offer.check_code();
            let svg = qrcode::QrCode::new(offer.code.as_bytes()).ok().map(|code| {
                code.render::<qrcode::render::svg::Color>()
                    .min_dimensions(240, 240)
                    .quiet_zone(true)
                    .dark_color(qrcode::render::svg::Color("#000000"))
                    .light_color(qrcode::render::svg::Color("#ffffff"))
                    .build()
            });
            let superseded = if let Ok(mut g) = LINK_OFFER.get_or_init(|| Mutex::new(None)).lock() {
                if LINK_OFFER_SEQ.load(std::sync::atomic::Ordering::Acquire) != seq {
                    true
                } else {
                    *g = Some((seq, offer.clone()));
                    false
                }
            } else {
                true
            };
            if superseded {
                backend::cancel_link_soon(offer);
                return Ok(json!({ "cancelled": true }));
            }
            Ok(json!({ "offer_id": seq, "check_code": check_code, "svg": svg }))
        }
        "account_take_link" => {
            let wanted = req["offer_id"].as_u64();
            let pair = LINK_OFFER.get_or_init(|| Mutex::new(None)).lock()
                .ok()
                .and_then(|g| g.clone())
                .filter(|(id, _)| wanted.is_none_or(|w| w == *id));
            let Some((id, offer)) = pair else {
                return Ok(json!({ "signed_in": false, "transient": false, "expired": true }));
            };
            match backend::take_link(&offer).await {
                Ok(Some(_)) => {
                    if let Ok(mut g) = LINK_OFFER.get_or_init(|| Mutex::new(None)).lock() {
                        if g.as_ref().is_some_and(|(cur, _)| *cur == id) {
                            *g = None;
                        }
                    }
                    Ok(json!({ "signed_in": true, "transient": false, "state": account_state_json().await }))
                }
                Ok(None) => Ok(json!({ "signed_in": false, "transient": false })),
                Err(e) => Ok(json!({
                    "signed_in": false,
                    "transient": backend::is_transient(&e),
                    "error": format!("{e:#}"),
                })),
            }
        }
        "account_cancel_link" => {
            let wanted = req["offer_id"].as_u64();
            LINK_OFFER_SEQ.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if let Ok(mut g) = LINK_OFFER.get_or_init(|| Mutex::new(None)).lock() {
                let matches = wanted.is_none() || g.as_ref().is_some_and(|(id, _)| Some(*id) == wanted);
                if matches {
                    if let Some((_, old)) = g.take() {
                        backend::cancel_link_soon(old);
                    }
                }
            }
            Ok(json!({"ok": true}))
        }
        "account_take_keys_arrived" => {
            Ok(json!({ "from": backend::take_keys_arrived() }))
        }
        "picks_load" => {
            let saved_only = req["saved_only"].as_bool().unwrap_or(false);
            let force = req["force"].as_bool().unwrap_or(false);
            let already = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock().ok().and_then(|mut g| {
                if force { *g = None; }
                g.as_mut().map(|s| { s.saved_only = saved_only; s.ensure_pair(); s.to_json() })
            });
            if let Some(j) = already { return Ok(j); }
            let (picks_res, history_res, lib_res, watched) = tokio::join!(
                backend::picks(),
                backend::viewing_history(),
                backend::library(),
                backend::watched_ids(),
            );
            let mut items = picks_res.map(|p| p.items).unwrap_or_default();
            if items.len() < 12 { items.extend(backend::seed_catalogs().await); }
            let lib = lib_res.unwrap_or_default();
            items.extend(history_res.unwrap_or_default().into_iter().map(|s| s.media));
            items.extend(lib.iter().filter(|m| m.saved).cloned());
            let mut pool = Vec::new();
            picks_rules::merge(&mut pool, items);
            let disliked: HashSet<String> = lib.iter().filter(|m| m.liked < 0).map(brain::id).collect();
            let liked: HashSet<String> = lib.iter().filter(|m| m.liked > 0).map(brain::id).collect();
            let saved: HashSet<String> = lib.iter().filter(|m| m.saved).map(brain::id).collect();
            let night = backend::rand_u64() | 1;
            let mut b = tokio::task::spawn_blocking(move || {
                let mut b = brain::load(night);
                b.set_session(night);
                b.set_facts(brain::facts(&lib));
                b
            }).await.unwrap_or_default();
            if !picks_ai::failed() {
                let anchors = brain::anchors().await;
                if !anchors.is_empty() { b.set_anchors(anchors); }
                // Warm local synopsis embeddings in the background (same bounded strategy as DX Picks):
                // taste history first (liked/watched), then unembedded pool candidates.
                let mut to_embed: Vec<backend::Media> = pool.iter()
                    .filter(|m| (m.liked != 0 || m.watched) && picks_ai::embedding(m).is_none())
                    .take(24)
                    .cloned()
                    .collect();
                let seen_keys: HashSet<String> = to_embed.iter().map(backend::Media::key).collect();
                to_embed.extend(
                    pool.iter()
                        .filter(|m| !seen_keys.contains(&m.key()) && picks_ai::embedding(m).is_none())
                        .take(24)
                        .cloned(),
                );
                if !to_embed.is_empty() {
                    tokio::spawn(async move {
                        for m in to_embed {
                            if picks_ai::failed() { break; }
                            let _ = picks_ai::enrich_and_embed(&m).await;
                        }
                    });
                }
            }
            let mut sess = PicksSession {
                night, brain: b, pool, pair: (None, None), shown: Vec::new(), kept: Vec::new(), starred: Vec::new(),
                round: 0, streak: 0, kind: None, style: picks_rules::Style::Both, genres: Vec::new(),
                from: String::new(), to: String::new(), saved_only, watched, disliked, liked, saved,
            };
            sess.ensure_pair();
            let out = sess.to_json();
            if let Ok(mut g) = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock() {
                *g = Some(sess);
            }
            Ok(out)
        }
        "picks_choose" => {
            let keep_left = req["left"].as_bool().unwrap_or(true);
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    let (winner, loser) = if keep_left {
                        (s.pair.0.clone(), s.pair.1.clone())
                    } else {
                        (s.pair.1.clone(), s.pair.0.clone())
                    };
                    if let (Some(w), Some(l)) = (winner, loser) {
                        let ev = s.brain.keep(&w, &l);
                        brain::persist(&mut s.brain, Some(&ev));
                        s.round += 1;
                        let same_champ = s.kept.first().is_some_and(|k| k.key() == w.key());
                        s.streak = if same_champ { s.streak + 1 } else { 1 };
                        s.kept.retain(|k| k.key() != w.key());
                        s.kept.insert(0, w.clone());
                        if s.streak >= 4 {
                            if !s.starred.iter().any(|x| x.key() == w.key()) { s.starred.insert(0, w.clone()); }
                            s.streak = 0;
                            let list = s.filtered();
                            let ex = s.avoid();
                            let (nl, nr) = s.brain.opening_pair(&list, &ex);
                            s.mark_shown(&nl);
                            s.mark_shown(&nr);
                            s.pair = (nl, nr);
                        } else {
                            let list = s.filtered();
                            let ex = s.avoid();
                            let ch = s.brain.challenger(&list, &ex, &w);
                            s.mark_shown(&ch);
                            if keep_left { s.pair = (Some(w), ch); } else { s.pair = (ch, Some(w)); }
                        }
                    }
                    s.to_json()
                })?;
            Ok(out)
        }
        "picks_skip" => {
            let neither = req["neither"].as_bool().unwrap_or(false);
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    if neither {
                        if let (Some(l), Some(r)) = (s.pair.0.clone(), s.pair.1.clone()) {
                            let ev = s.brain.neither(&l, &r);
                            brain::persist(&mut s.brain, Some(&ev));
                        }
                    }
                    s.streak = 0;
                    let list = s.filtered();
                    let ex = s.avoid();
                    let (nl, nr) = s.brain.opening_pair(&list, &ex);
                    s.mark_shown(&nl);
                    s.mark_shown(&nr);
                    s.pair = (nl, nr);
                    s.to_json()
                })?;
            Ok(out)
        }
        "picks_filters" => {
            let kind = req.get("kind").and_then(Value::as_str).map(str::to_owned);
            let style = req.get("style").and_then(Value::as_str).map(picks_rules::Style::parse).unwrap_or(picks_rules::Style::Both);
            let genres: Vec<String> = req.get("genres").and_then(Value::as_array).into_iter().flatten()
                .filter_map(|v| v.as_str().map(str::to_owned)).collect();
            let from = req.get("from").and_then(Value::as_str).unwrap_or("").to_owned();
            let to = req.get("to").and_then(Value::as_str).unwrap_or("").to_owned();
            let saved_only = req.get("saved_only").and_then(Value::as_bool).unwrap_or(false);
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    s.kind = kind;
                    s.style = style;
                    s.genres = genres;
                    s.from = from;
                    s.to = to;
                    s.saved_only = saved_only;
                    s.streak = 0;
                    let list = s.filtered();
                    let ex = s.avoid();
                    let (nl, nr) = s.brain.opening_pair(&list, &ex);
                    s.mark_shown(&nl);
                    s.mark_shown(&nr);
                    s.pair = (nl, nr);
                    s.to_json()
                })?;
            Ok(out)
        }
        "picks_mood" => {
            let label = req["label"].as_str().unwrap_or("").to_owned();
            let phrase = req["phrase"].as_str().map(str::to_owned);
            let clear = req["clear"].as_bool().unwrap_or(false);
            let already_has = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .ok().and_then(|g| g.as_ref().map(|s| s.brain.moods().iter().any(|m| m == &label))).unwrap_or(false);
            let vec = if !clear && !already_has && !label.is_empty() {
                let text = phrase.unwrap_or_else(|| label.clone());
                picks_ai::embed_query(&text).await.ok()
            } else { None };
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    if clear {
                        for ev in s.brain.clear_moods() { brain::persist(&mut s.brain, Some(&ev)); }
                    } else if already_has {
                        let ev = s.brain.set_mood(&label, None);
                        brain::persist(&mut s.brain, Some(&ev));
                    } else if let Some(v) = vec {
                        let ev = s.brain.set_mood(&label, Some(v));
                        brain::persist(&mut s.brain, Some(&ev));
                    }
                    s.to_json()
                })?;
            Ok(out)
        }
        "picks_new_night" => {
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    s.night = backend::rand_u64() | 1;
                    s.brain.set_session(s.night);
                    brain::persist(&mut s.brain, None);
                    s.shown.clear();
                    s.kept.clear();
                    s.starred.clear();
                    s.round = 0;
                    s.streak = 0;
                    let list = s.filtered();
                    let (nl, nr) = s.brain.opening_pair(&list, &[]);
                    s.mark_shown(&nl);
                    s.mark_shown(&nr);
                    s.pair = (nl, nr);
                    s.to_json()
                })?;
            Ok(out)
        }
        "picks_reset_taste" => {
            brain::reset();
            if let Ok(mut g) = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock() {
                *g = None;
            }
            Ok(json!({"ok": true}))
        }
        "picks_unstar" => {
            let key = req["key"].as_str().unwrap_or("").to_owned();
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    s.kept.retain(|m| m.key() != key);
                    s.starred.retain(|m| m.key() != key);
                    s.to_json()
                })?;
            Ok(out)
        }
        "picks_card_extras" => {
            let m = media_from_arg(&req["media"])?;
            let (meta_res, scores) = tokio::join!(
                backend::title_meta(&m),
                backend::review_scores(&m),
            );
            let meta = meta_res.ok();
            let director = meta.as_ref().and_then(|tm| tm.directors.first().cloned());
            let trailers: Vec<Value> = meta.as_ref().map(|tm| {
                tm.trailers.iter().map(|t| json!({"youtube": t.youtube, "url": t.url})).collect()
            }).unwrap_or_default();
            Ok(json!({
                "logo": meta.as_ref().map(|tm| tm.logo.as_str()).unwrap_or(""),
                "rating": meta.as_ref().map(|tm| tm.rating.as_str()).unwrap_or(""),
                "runtime": meta.as_ref().map(|tm| tm.runtime.as_str()).unwrap_or(""),
                "director": director,
                "trailers": trailers,
                "scores": { "tomatometer": scores.tomatometer, "metascore": scores.metascore },
            }))
        }
        "picks_played" => {
            let m = media_from_arg(&req["media"])?;
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .map(|s| {
                    let ev = s.brain.play(&m);
                    brain::persist(&mut s.brain, Some(&ev));
                    s.to_json()
                });
            Ok(out.unwrap_or_else(|| json!({"ok": true})))
        }
        "picks_rate" => {
            let m = media_from_arg(&req["media"])?;
            let value = req["value"].as_i64().unwrap_or(0);
            backend::feedback(&m, value).await?;
            let id = brain::id(&m);
            let key = m.key();
            let out = PICKS_SESSION.get_or_init(|| Mutex::new(None)).lock()
                .map_err(|_| anyhow!("lock"))?
                .as_mut()
                .ok_or_else(|| anyhow!("no picks session"))
                .map(|s| {
                    if value > 0 {
                        s.liked.insert(id.clone());
                        s.disliked.remove(&id);
                        if !s.starred.iter().any(|x| x.key() == key) {
                            s.starred.insert(0, m.clone());
                        }
                    } else if value == 0 {
                        s.liked.remove(&id);
                        s.starred.retain(|x| x.key() != key);
                    } else {
                        s.liked.remove(&id);
                        s.disliked.insert(id.clone());
                        s.kept.retain(|x| x.key() != key);
                        s.starred.retain(|x| x.key() != key);
                        let is_left = s.pair.0.as_ref().is_some_and(|x| x.key() == key);
                        let is_right = s.pair.1.as_ref().is_some_and(|x| x.key() == key);
                        if is_left || is_right {
                            s.streak = 0;
                            let list = s.filtered();
                            let ex = s.avoid();
                            let other = if is_left { s.pair.1.clone() } else { s.pair.0.clone() };
                            let next = match other.as_ref() {
                                Some(o) => s.brain.challenger(&list, &ex, o),
                                None => s.brain.opening_pair(&list, &ex).0,
                            };
                            s.mark_shown(&next);
                            if is_left { s.pair.0 = next; } else { s.pair.1 = next; }
                        }
                    }
                    s.to_json()
                })?;
            Ok(out)
        }
        "open_external" => {
            if let Some(url) = req["url"].as_str() { open_external(url); }
            Ok(json!({"ok": true}))
        }
        "youtube_stream" => {
            let id = req["id"].as_str().unwrap_or("");
            youtube::resolve(id).await
        }
        _ => Err(anyhow!("unknown op: {op}")),
    }
}

static BRIDGE_RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn rt() -> &'static tokio::runtime::Runtime {
    BRIDGE_RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("bridge runtime")
    })
}

fn to_cstring_ptr(v: Value) -> *mut c_char {
    let s = v.to_string().replace('\0', "");
    CString::new(s).unwrap_or_else(|_| CString::new("{}").unwrap()).into_raw()
}

/// Run one op in its own task so a panic can never leave the Dart future waiting forever: it
/// becomes an ordinary error reply, and failures are logged (redacted) and reported to Sentry.
async fn run_op(v: Value) -> Value {
    let op = v["op"].as_str().unwrap_or("").to_owned();
    // Title page parts are kept on disk (stale-while-revalidate, see title_store).
    let store_part: Option<(&'static str, String)> = match op.as_str() {
        "title_details" => Some("details"),
        "title_scores" => Some("scores"),
        "title_art" => Some("art"),
        "title_similar" => Some("similar"),
        "title_collection" => Some("collection"),
        "title_studios" => Some("studios"),
        "title_extras" => Some("extras"),
        _ => None,
    }.and_then(|part| Some((part, media_from_arg(&v["media"]).ok()?.key())));
    let started = std::time::Instant::now();
    let tx = crash::op_transaction(&op);
    let joined = tokio::spawn(handle_request(v)).await;
    let elapsed = started.elapsed();
    use sentry::protocol::SpanStatus;
    match joined {
        Ok(Ok(data)) => {
            if let Some((part, key)) = &store_part {
                // A failed meta fetch isn't worth keeping over a good stored copy.
                if !(*part == "details" && data["meta"]["error"].is_string()) { title_store::put(key, part, &data); }
            }
            crash::finish_transaction(tx, SpanStatus::Ok);
            crash::op_slow(&op, elapsed);
            json!({ "ok": true, "data": data })
        }
        Ok(Err(e)) => {
            crash::finish_transaction(tx, SpanStatus::InternalError);
            crash::op_failed(&op, elapsed, &e);
            json!({ "ok": false, "error": format!("{e:#}") })
        }
        Err(join) => {
            crash::finish_transaction(tx, SpanStatus::Aborted);
            crash::op_panicked(&op, elapsed);
            let why = if join.is_panic() { "hit an internal error" } else { "was cancelled" };
            json!({ "ok": false, "error": format!("Veylo {why} ({op}). It has been logged.") })
        }
    }
}

/// Dispatch an asynchronous JSON bridge request from Dart.
///
/// # Safety
/// `req_json` must be either null or a valid, NUL-terminated C string readable for the duration of
/// this call. `cb` must be a valid function pointer safe to invoke from a Tokio worker thread.
#[no_mangle]
pub unsafe extern "C" fn veylo_call_async(req_id: u64, req_json: *const c_char, cb: AsyncCallback) {
    let req_str = if req_json.is_null() {
        "{}".to_owned()
    } else {
        unsafe { CStr::from_ptr(req_json) }.to_string_lossy().into_owned()
    };
    rt().spawn(async move {
        let resp = match serde_json::from_str::<Value>(&req_str) {
            Ok(v) => run_op(v).await,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        };
        cb(req_id, to_cstring_ptr(resp));
    });
}

/// Free a C string previously returned by `veylo_call_async`.
///
/// # Safety
/// `ptr` must be either null or a pointer returned by `to_cstring_ptr` that has not already been
/// freed.
#[no_mangle]
pub unsafe extern "C" fn veylo_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe { drop(CString::from_raw(ptr)); }
    }
}

/// Headless backend smoke test: drives real bridge ops (network) against a throwaway data folder,
/// so CI catches backend runtime errors and panics on every OS without launching the UI.
/// `cargo test --release -- --ignored smoke` (never touches the owner's store).
#[cfg(test)]
mod smoke {
    use super::*;

    fn call(op: &str, mut args: Value) -> Value {
        args["op"] = json!(op);
        let resp = rt().block_on(run_op(args));
        let ok = resp["ok"].as_bool().unwrap_or(false);
        let err = resp["error"].as_str().unwrap_or("");
        assert!(!err.contains("internal error"), "{op} panicked: {err}");
        eprintln!("smoke {op}: {}", if ok { "ok".to_owned() } else { backend::redact_diagnostic(err) });
        resp
    }

    fn must(op: &str, args: Value) -> Value {
        let resp = call(op, args);
        assert!(resp["ok"].as_bool().unwrap_or(false), "{op} failed: {}", resp["error"]);
        resp["data"].clone()
    }

    #[test]
    #[ignore = "network; run with --ignored"]
    fn smoke() {
        let dir = std::env::temp_dir().join(format!("veylo-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("VEYLO_DX_DATA", &dir);

        must("init", json!({}));
        must("settings_get", json!({}));
        call("home_rows", json!({}));
        let found = must("search", json!({ "query": "The Matrix" }));
        let media = found["results"].as_array()
            .and_then(|r| r.iter().find(|x| x["media"]["type"] == "movie"))
            .map(|x| x["media"].clone())
            .expect("search returned no film");
        must("title_cached", json!({ "media": media }));
        let details = must("title_details", json!({ "media": media }));
        assert!(details["meta"]["error"].is_null(), "title meta failed: {}", details["meta"]["error"]);
        must("title_extras", json!({ "media": media }));
        call("sources", json!({ "media": media, "video_id": media["id"] }));
        let series = must("search", json!({ "query": "Breaking Bad" }));
        if let Some(show) = series["results"].as_array()
            .and_then(|r| r.iter().find(|x| x["media"]["type"] == "series"))
            .map(|x| x["media"].clone())
        {
            let d = must("title_details", json!({ "media": show }));
            assert!(d["meta"]["episodes"].as_array().is_some_and(|e| !e.is_empty()), "series has no episodes");
        }
        must("report_error", json!({ "source": "smoke", "error": "test error https://example.com/?token=abc", "stack": "" }));
        let report = must("diagnostic_report", json!({}));
        let text = report["report"].as_str().unwrap_or("");
        assert!(!text.contains("token=abc") && !text.contains("example.com"), "diagnostics leaked a URL");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Per-call timing of what a title page waits for (cold, then warm), to find what makes opening
/// a film slow. `cargo test --release -- --ignored title_timings --nocapture`
#[cfg(test)]
mod timings {
    use super::*;

    #[test]
    #[ignore = "network; run with --ignored"]
    fn title_timings() {
        let dir = std::env::temp_dir().join(format!("veylo-timings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("VEYLO_DX_DATA", &dir);
        rt().block_on(async {
            handle_request(json!({"op": "init"})).await.unwrap();
            for q in ["Dune Part Two", "Oppenheimer", "Severance"] {
                let found = handle_request(json!({"op": "search", "query": q})).await.unwrap();
                let Some(v) = found["results"].as_array().and_then(|r| r.first()).map(|x| x["media"].clone()) else { continue };
                let m = media_from_arg(&v).unwrap();
                let s0 = std::time::Instant::now();
                let _ = handle_request(json!({"op": "title_details", "media": v.clone()})).await;
                eprintln!("{q} first-paint title_details (cold) = {}ms", s0.elapsed().as_millis());
                let ss = std::time::Instant::now();
                let st = studios::studios(&m.id).await.unwrap_or_default();
                let t_st = ss.elapsed().as_millis();
                let ids = backend::idents(st.clone()).await;
                let ss2 = std::time::Instant::now();
                let _ = studios::studios(&m.id).await;
                eprintln!("{q} NEW studios: {} in {t_st}ms (+idents {}ms total, {} logos) | reopen {}us",
                    st.len(), ss.elapsed().as_millis(), ids.len(), ss2.elapsed().as_micros());
                let s1 = std::time::Instant::now();
                let fast = backend::title_artwork_fast(&m).await;
                let fast_studios = fast.as_ref().map(|a| a.studios.clone()).unwrap_or_default();
                let n_fast = fast_studios.len();
                let si = std::time::Instant::now();
                let ids_fast = backend::idents(fast_studios).await;
                let t_ids_fast = si.elapsed().as_millis();
                let sw = std::time::Instant::now();
                let wiki = backend::open_wikidata(&m).await;
                let t_wiki = sw.elapsed().as_millis();
                let n_wiki = wiki.as_ref().map(|a| a.studios.len()).unwrap_or(0);
                let sx = std::time::Instant::now();
                let x = handle_request(json!({"op": "title_extras", "media": v.clone()})).await.unwrap();
                eprintln!("{q} studios: fast={n_fast} idents(fast)={t_ids_fast}ms -> {} | wiki={n_wiki} wikidata={t_wiki}ms | title_extras(after)={}ms idents={} | total {}ms",
                    ids_fast.len(), sx.elapsed().as_millis(), x["idents"].as_array().map(|a| a.len()).unwrap_or(0), s1.elapsed().as_millis());
                for pass in ["cold", "warm"] {
                    macro_rules! t { ($name:expr, $f:expr) => { async { let s = std::time::Instant::now(); let _ = $f.await; ($name, s.elapsed().as_millis()) } } }
                    let rows = tokio::join!(
                        t!("title_meta", backend::title_meta(&m)),
                        t!("artwork_fast", backend::title_artwork_fast(&m)),
                        t!("stills", backend::title_stills(&m)),
                        t!("series_free", async { if m.kind != "movie" { backend::open_series_free(&m).await } else { None } }),
                        t!("wikidata", backend::open_wikidata(&m)),
                        t!("scores", backend::review_scores(&m)),
                        t!("status", async { if m.kind != "movie" { backend::series_status(&m).await } else { None } }),
                        t!("rank", backend::today_rank(&m)),
                        t!("collection", backend::collection_parts(&m)),
                        t!("similar", backend::similar_titles(&m)),
                    );
                    let s = std::time::Instant::now();
                    let _ = handle_request(json!({"op": "title_details", "media": v.clone()})).await;
                    let total = s.elapsed().as_millis();
                    let r = [rows.0, rows.1, rows.2, rows.3, rows.4, rows.5, rows.6, rows.7, rows.8, rows.9];
                    eprintln!("{q} [{pass}] title_details={total}ms :: {}", r.iter().map(|(n, ms)| format!("{n}={ms}")).collect::<Vec<_>>().join(" "));
                }
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
