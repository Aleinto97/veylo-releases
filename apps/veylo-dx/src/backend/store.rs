//! What the app keeps in its own folder besides the library: settings.json, tonight's Picks duel
//! (picks-duel.json) and the search history (search-history.json).
use super::*;

// ---------------------------------------------------------------- settings

/// Viewer settings kept in the app's own folder (settings.json).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
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
    /// Subtitle font scale (0.6..=2.0, default 1.05).
    pub sub_scale: f64,
    /// Subtitle vertical position from top in percent (75..=100, default 100).
    pub sub_pos: i32,
    /// Draw a subtle dark pill behind subtitles for legibility over bright scenes.
    pub sub_bg: bool,
    /// Automatically count down to the next episode at credits or end of episode.
    pub auto_next: bool,
    /// Send redacted crashes, UI errors, playback failures and performance reports to Firebase.
    pub telemetry_enabled: bool,
}

static TELEMETRY_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn telemetry_enabled() -> bool {
    TELEMETRY_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub(super) fn set_telemetry_enabled(on: bool) {
    TELEMETRY_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The audio language when none was chosen, or the saved one is unknown.
pub const DEFAULT_LANGUAGE: &str = "english";

/// The saved audio language, English when there is none (older settings saved "Any language" as null).
pub(super) fn audio_language(saved: &Value) -> String {
    saved["stream_language"].as_str().filter(|l| crate::addons::language(l).is_some()).unwrap_or(DEFAULT_LANGUAGE).to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            better_posters: true, tmdb_key: None, picks_hide_watched: true, rd_key: None,
            stream_language: Some(DEFAULT_LANGUAGE.into()), rd_key_removed: false, hdr_output: true,
            sub_scale: 1.05, sub_pos: 100, sub_bg: false, auto_next: true, telemetry_enabled: true,
        }
    }
}

pub fn load_settings() -> Settings {
    let v: Value = std::fs::read_to_string(backend().data_dir.join("settings.json")).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    Settings {
        better_posters: v["better_posters"].as_bool().unwrap_or(true),
        tmdb_key: v["tmdb_key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned),
        picks_hide_watched: v["picks_hide_watched"].as_bool().unwrap_or(true),
        rd_key: v["rd_key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned),
        stream_language: Some(audio_language(&v)),
        rd_key_removed: v["rd_key_removed"].as_bool().unwrap_or(false),
        hdr_output: v["hdr_output"].as_bool().unwrap_or(true),
        sub_scale: v["sub_scale"].as_f64().unwrap_or(1.05).clamp(0.6, 2.0),
        sub_pos: v["sub_pos"].as_i64().unwrap_or(100).clamp(70, 100) as i32,
        sub_bg: v["sub_bg"].as_bool().unwrap_or(false),
        auto_next: v["auto_next"].as_bool().unwrap_or(true),
        telemetry_enabled: v["telemetry_enabled"].as_bool().unwrap_or(true),
    }
}

/// Write a file that may hold credentials or tokens, restricting it to owner-only (0600) on Unix.
pub(crate) fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        f.write_all(bytes)?;
        f.sync_all()
    }
    #[cfg(not(unix))]
    { std::fs::write(path, bytes) }
}

pub fn save_settings(settings: &Settings) {
    let data = json!({
        "better_posters": settings.better_posters,
        "tmdb_key": settings.tmdb_key,
        "picks_hide_watched": settings.picks_hide_watched,
        "rd_key": settings.rd_key,
        "stream_language": settings.stream_language,
        "rd_key_removed": settings.rd_key_removed,
        "hdr_output": settings.hdr_output,
        "sub_scale": settings.sub_scale.clamp(0.6, 2.0),
        "sub_pos": settings.sub_pos.clamp(70, 100),
        "sub_bg": settings.sub_bg,
        "auto_next": settings.auto_next,
        "telemetry_enabled": settings.telemetry_enabled,
    }).to_string();
    let _ = write_private_file(&backend().data_dir.join("settings.json"), data.as_bytes());
    BETTER_POSTERS.store(settings.better_posters, std::sync::atomic::Ordering::Relaxed);
    HDR_OUTPUT.store(settings.hdr_output, std::sync::atomic::Ordering::Relaxed);
    set_telemetry_enabled(settings.telemetry_enabled);
    set_stream_language(settings.stream_language.clone());
    set_tmdb_key(settings.tmdb_key.clone());
    // The key and language decide which add-ons answer and with which links.
    forget_sources();
    sync_soon();
}

// ---------------------------------------------------------------- per-title track preferences

/// Remembered audio and subtitle language for a series or film (`track-prefs.json`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrackPref {
    pub audio_lang: Option<String>,
    pub sub_lang: Option<String>,
    pub sub_off: bool,
}

fn track_prefs_path() -> PathBuf { backend().data_dir.join("track-prefs.json") }

static TRACK_PREFS: Mutex<Option<HashMap<String, TrackPref>>> = Mutex::new(None);

fn read_track_prefs_from(path: &std::path::Path) -> HashMap<String, TrackPref> {
    let v: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    v.as_object().into_iter().flatten().map(|(k, entry)| {
        let pref = TrackPref {
            audio_lang: entry["audio"].as_str().filter(|s| !s.is_empty()).map(str::to_owned),
            sub_lang: entry["sub"].as_str().filter(|s| !s.is_empty()).map(str::to_owned),
            sub_off: entry["sub_off"].as_bool().unwrap_or(false),
        };
        (k.clone(), pref)
    }).collect()
}

fn write_track_prefs_json(map: &HashMap<String, TrackPref>) -> String {
    let obj: serde_json::Map<String, Value> = map.iter().map(|(k, p)| {
        (k.clone(), json!({"audio": p.audio_lang, "sub": p.sub_lang, "sub_off": p.sub_off}))
    }).collect();
    Value::Object(obj).to_string()
}

pub fn load_track_pref(media_key: &str) -> Option<TrackPref> {
    let mut guard = TRACK_PREFS.lock().ok()?;
    let map = guard.get_or_insert_with(|| {
        flush_writes();
        read_track_prefs_from(&track_prefs_path())
    });
    map.get(media_key).cloned()
}

pub fn save_track_pref(media_key: &str, update: impl FnOnce(&mut TrackPref)) {
    let Ok(mut guard) = TRACK_PREFS.lock() else { return };
    let map = guard.get_or_insert_with(|| {
        flush_writes();
        read_track_prefs_from(&track_prefs_path())
    });
    let entry = map.entry(media_key.to_owned()).or_default();
    update(entry);
    // Keep the file bounded to 400 recent titles.
    if map.len() > 400 {
        let drop_key = map.keys().find(|k| k.as_str() != media_key).cloned();
        if let Some(k) = drop_key { map.remove(&k); }
    }
    write_soon(track_prefs_path(), write_track_prefs_json(map));
}

// ---------------------------------------------------------------- redacted diagnostics

static DIAG_RING: Mutex<std::collections::VecDeque<String>> = Mutex::new(std::collections::VecDeque::new());

/// Strip URLs, API keys and tokens from diagnostic lines so logs are always safe to share.
pub fn redact_diagnostic(line: &str) -> String {
    static_re!(url_re, r"https?://[^\s\x22'<>]+");
    static_re!(secret_re, r"(?i)(api[_-]?key|token|secret|password|authorization|rd_key|tmdb_key)\s*[:=]\s*[^\s&,\x22']+");
    let step = url_re().replace_all(line, "[URL]");
    secret_re().replace_all(&step, "$1=[REDACTED]").into_owned()
}

pub fn record_diag_line(message: &str) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let clean = redact_diagnostic(message);
    if let Ok(mut q) = DIAG_RING.lock() {
        if q.len() >= 200 { q.pop_front(); }
        q.push_back(format!("{ms} {clean}"));
    }
}

/// A shareable, redacted diagnostic snapshot (version, OS/arch, HDR screen, recent events, last panic).
pub fn diagnostic_report() -> String {
    let mut out = String::new();
    let settings = load_settings();
    let (screen_title, _) = crate::display::summary(crate::display::screen(), settings.hdr_output);
    out.push_str(&format!("Veylo {} ({}/{})\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH));
    out.push_str(&format!("Screen: {screen_title}\n"));
    out.push_str(&format!("Language: {} | RD key: {} | TMDB key: {} | Account: {}\n",
        settings.stream_language.as_deref().unwrap_or(DEFAULT_LANGUAGE),
        if settings.rd_key.is_some() { "set" } else { "none" },
        if settings.tmdb_key.is_some() { "set" } else { "none" },
        if account().is_some() { "signed in" } else { "signed out" },
    ));
    out.push_str("--- Recent events ---\n");
    let mut lines: Vec<String> = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(backend().data_dir.join("dev-log.txt")) {
        for l in raw.lines().rev().take(120).collect::<Vec<_>>().into_iter().rev() {
            lines.push(redact_diagnostic(l));
        }
    }
    if lines.is_empty() {
        if let Ok(q) = DIAG_RING.lock() { lines.extend(q.iter().cloned()); }
    }
    if lines.is_empty() { out.push_str("(no recent events)\n"); }
    else { for l in lines { out.push_str(&l); out.push('\n'); } }
    if let Ok(panic_text) = std::fs::read_to_string(backend().data_dir.join("last-panic.txt")) {
        let snippet: String = panic_text.lines().take(25).map(redact_diagnostic).collect::<Vec<_>>().join("\n");
        if !snippet.trim().is_empty() {
            out.push_str("--- Last panic ---\n");
            out.push_str(&snippet);
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------- watch-history import & export (Trakt / IMDb / Veylo)

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedTitle {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub year: String,
    pub watched: bool,
    pub saved: bool,
    pub liked: i64,
}

/// Parse a Trakt watched JSON export (`[{"movie":{...}}]` / `[{"show":{...}}]`), a Veylo JSON
/// export (`{"veylo_export":1,"items":[...]}`), or an IMDb CSV / plain list of `tt...` IDs.
pub fn parse_watch_history_import(raw: &str) -> Vec<ImportedTitle> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    fn push_item(out: &mut Vec<ImportedTitle>, seen: &mut HashSet<String>, item: ImportedTitle) {
        if item.id.is_empty() { return; }
        let k = format!("{}|{}", item.kind, item.id);
        if seen.insert(k) { out.push(item); }
    }

    if let Ok(v) = serde_json::from_str::<Value>(raw.trim()) {
        // Veylo JSON export
        if let Some(items) = v.get("items").and_then(Value::as_array) {
            for e in items {
                let id = s(e, "id");
                let kind = Some(s(e, "type")).filter(|k| !k.is_empty()).unwrap_or_else(|| "movie".into());
                let name = Some(s(e, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
                let year = s(e, "year");
                let watched = e["watched"].as_bool().unwrap_or(true);
                let saved = e["saved"].as_bool().unwrap_or(false);
                let liked = e["liked"].as_i64().unwrap_or(0).clamp(-1, 1);
                push_item(&mut out, &mut seen, ImportedTitle { id, kind, name, year, watched, saved, liked });
            }
            return out;
        }
        // Trakt watched / history / watchlist JSON array
        if let Some(arr) = v.as_array() {
            for e in arr {
                if let Some(m) = e.get("movie") {
                    let id = s(&m["ids"], "imdb");
                    let name = Some(s(m, "title")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
                    let year = m["year"].as_i64().map(|y| y.to_string()).unwrap_or_default();
                    let saved = e.get("listed_at").is_some();
                    push_item(&mut out, &mut seen, ImportedTitle { id, kind: "movie".into(), name, year, watched: !saved, saved, liked: 0 });
                } else if let Some(sh) = e.get("show") {
                    let id = s(&sh["ids"], "imdb");
                    let name = Some(s(sh, "title")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
                    let year = sh["year"].as_i64().map(|y| y.to_string()).unwrap_or_default();
                    let saved = e.get("listed_at").is_some();
                    push_item(&mut out, &mut seen, ImportedTitle { id, kind: "series".into(), name, year, watched: !saved, saved, liked: 0 });
                }
            }
            if !out.is_empty() { return out; }
        }
    }

    // Fallback: IMDb CSV export or plain text containing `tt1234567` IDs.
    static_re!(imdb_re, r"\b(tt\d{7,9})\b");
    for line in raw.lines() {
        let Some(caps) = imdb_re().captures(line) else { continue };
        let id = caps[1].to_owned();
        let lower = line.to_lowercase();
        let kind = if lower.contains("tv series") || lower.contains("tv mini") || lower.contains("series") { "series" } else { "movie" }.to_owned();
        // Best-effort title from CSV if present (column after id or quoted title).
        let name = line.split(',').nth(5)
            .map(|c| c.trim().trim_matches('"').to_owned())
            .filter(|n| !n.is_empty() && !n.starts_with("http"))
            .unwrap_or_else(|| id.clone());
        push_item(&mut out, &mut seen, ImportedTitle { id, kind, name, year: String::new(), watched: true, saved: false, liked: 0 });
    }
    out
}

/// Export watched, saved, and liked titles as a portable JSON backup (zero keys or addon secrets).
pub async fn export_watch_history() -> Result<String> {
    let items = library().await?;
    let rows: Vec<Value> = items.into_iter().filter(|m| m.watched || m.saved || m.liked != 0 || m.position_ms > 0).map(|m| json!({
        "id": m.id,
        "type": m.kind,
        "name": m.name,
        "year": m.year,
        "watched": m.watched,
        "saved": m.saved,
        "liked": m.liked,
    })).collect();
    let payload = json!({"veylo_export": 1, "exported_at": now_secs(), "items": rows});
    let text = serde_json::to_string_pretty(&payload)?;
    let _ = write_private_file(&backend().data_dir.join("veylo-history-export.json"), text.as_bytes());
    Ok(text)
}

/// Import titles from Trakt JSON, Veylo JSON, or IMDb CSV into the local library & Picks taste.
pub async fn import_watch_history(raw: &str) -> Result<usize> {
    let parsed = parse_watch_history_import(raw);
    anyhow::ensure!(!parsed.is_empty(), "No titles found. Paste a Trakt watched JSON, Veylo export JSON, or IMDb CSV.");
    let mut count = 0usize;
    for item in parsed {
        let meta = json!({
            "id": item.id,
            "type": item.kind,
            "name": item.name,
            "releaseInfo": item.year,
        });
        let Some(media) = Media::from_json(&meta, CINEMETA) else { continue };
        if item.watched { let _ = set_watched(&media, true).await; }
        if item.saved { let _ = set_saved(&media, true).await; }
        if item.liked != 0 { let _ = feedback(&media, item.liked).await; }
        count += 1;
    }
    sync_soon();
    Ok(count)
}

/// Change one or more settings: read, changed, saved and applied; returns what was saved.
pub fn update_settings(change: impl FnOnce(&mut Settings)) -> Settings {
    let mut settings = load_settings();
    change(&mut settings);
    save_settings(&settings);
    settings
}

/// Whether TMDB accepts a v3 API key (a cheap request that needs one).
pub async fn check_tmdb_key(key: String) -> Result<()> {
    on_runtime(async move {
        let response = backend().http.get("https://api.themoviedb.org/3/configuration").query(&[("api_key", key)]).send().await
            .map_err(|_| anyhow!("TMDB didn't answer. Check your connection."))?;
        anyhow::ensure!(response.status().is_success(), "TMDB didn't accept this key. Use the API Key, not the Read Access Token.");
        Ok(())
    }).await?
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
    let requests: Vec<Value> = requests.into_iter().take(6).collect();
    on_runtime(async move {
        let mut jobs = tokio::task::JoinSet::new();
        for (i, req) in requests.into_iter().enumerate() {
            jobs.spawn(async move {
                let origin = s(&req, "addon_url");
                let items = backend().core.execute(req).await.ok().map(|v| metas(&v, &origin)).unwrap_or_default();
                (i, items)
            });
        }
        let mut results: Vec<(usize, Vec<Media>)> = Vec::new();
        while let Some(done) = jobs.join_next().await {
            if let Ok(pair) = done { results.push(pair); }
        }
        results.sort_by_key(|(i, _)| *i);
        results.into_iter().flat_map(|(_, items)| items).collect()
    }).await.unwrap_or_default()
}

pub(super) fn duel_state_path() -> PathBuf { backend().data_dir.join("picks-duel.json") }

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

/// A title as JSON that `Media::from_json` reads back unchanged, without a series' episode list
/// (hundreds of entries the duel never reads back: its cards load the title's metadata).
pub(super) fn media_json(m: &Media) -> Value {
    let mut o: serde_json::Map<String, Value> = m.raw.as_object().into_iter().flatten()
        .filter(|(k, _)| k.as_str() != "videos").map(|(k, v)| (k.clone(), v.clone())).collect();
    for (k, x) in [("id", &m.id), ("type", &m.kind), ("name", &m.name), ("addon_url", &m.addon_url), ("poster", &m.poster),
        ("background", &m.background), ("description", &m.description), ("releaseInfo", &m.year)] { o.insert(k.into(), json!(x)); }
    o.insert("genres".into(), json!(m.genres));
    Value::Object(o)
}

/// Files written behind the UI: each keeps only its latest content, and a later write always
/// lands after an earlier one. For state saved often that nothing reads back at once.
fn write_soon(path: std::path::PathBuf, text: String) {
    let first = PENDING_WRITES.lock().map(|mut p| { let map = p.get_or_insert_with(HashMap::new); let first = map.is_empty(); map.insert(path, text); first }).unwrap_or(false);
    if first { std::thread::spawn(|| { std::thread::sleep(Duration::from_millis(200)); flush_writes(); }); }
}
static PENDING_WRITES: Mutex<Option<HashMap<std::path::PathBuf, String>>> = Mutex::new(None);
static WRITING: Mutex<()> = Mutex::new(());

/// Write what `write_soon` still holds, now: before a file is read back or the app exits.
pub fn flush_writes() {
    let _turn = WRITING.lock();
    let batch = PENDING_WRITES.lock().ok().and_then(|mut p| p.take()).unwrap_or_default();
    for (path, text) in batch { let _ = std::fs::write(path, text); }
}

pub fn save_duel(d: &DuelState) {
    let m = |x: &Option<Media>| x.as_ref().map(media_json).unwrap_or(Value::Null);
    let list = |l: &[Media]| l.iter().map(media_json).collect::<Vec<_>>();
    write_soon(duel_state_path(), json!({"version": 3, "kind": d.kind, "style": d.style, "genre": d.genre, "from": d.from, "to": d.to,
        "night": d.night.to_string(), "last": d.last, "round": d.round, "streak": d.streak, "shown": d.shown,
        "left": m(&d.left), "right": m(&d.right), "kept": list(&d.kept), "starred": list(&d.starred)}).to_string());
}

pub fn load_duel() -> Option<DuelState> {
    flush_writes();
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

// ---------------------------------------------------------------- search history

/// App-private search history: recorded on submit or opening a result, never per keystroke.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct History {
    pub queries: Vec<String>,
    pub items: Vec<Media>,
}

pub(super) fn history_path() -> PathBuf {
    backend().data_dir.join("search-history.json")
}

pub fn load_history() -> History {
    flush_writes();
    let Ok(text) = std::fs::read_to_string(history_path()) else { return History::default() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return History::default() };
    History {
        queries: v["queries"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
        items: v["items"].as_array().into_iter().flatten().filter_map(|m| Media::from_json(m, "")).collect(),
    }
}

pub(super) fn save_history(h: &History) {
    let items: Vec<Value> = h.items.iter().map(|m| {
        let mut raw: serde_json::Map<String, Value> = m.raw.as_object().into_iter().flatten()
            .filter(|(k, _)| k.as_str() != "videos").map(|(k, v)| (k.clone(), v.clone())).collect();
        raw.insert("addon_url".into(), json!(m.addon_url));
        if let Some(Value::String(d)) = raw.get_mut("description") { d.truncate(d.char_indices().nth(1000).map_or(d.len(), |(i, _)| i)); }
        Value::Object(raw)
    }).collect();
    write_soon(history_path(), json!({"queries":h.queries,"items":items}).to_string());
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
    // Through the same writer, so a save still pending can't bring the old history back.
    save_history(h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_private_file_enforces_owner_only_permissions_on_unix() {
        let dir = std::env::temp_dir().join(format!("veylo-store-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secret.json");
        std::fs::create_dir_all(&dir).unwrap();
        // Start with world-readable 0644 and verify write_private_file tightens it to 0600.
        std::fs::write(&path, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private_file(&path, b"{\"key\":\"top-secret\"}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"key\":\"top-secret\"}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "secret file mode was {mode:o}, expected 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_soon_coalesces_and_flush_writes_persists_immediately() {
        let dir = std::env::temp_dir().join(format!("veylo-store-flush-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        for i in 1..=20 {
            write_soon(path.clone(), format!("{{\"rev\":{i}}}"));
        }
        flush_writes();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"rev\":20}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn track_prefs_roundtrip_and_redaction() {
        let dir = std::env::temp_dir().join(format!("veylo-track-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("track-prefs.json");
        let mut map = HashMap::new();
        map.insert("series|tt1|a".into(), TrackPref { audio_lang: Some("jpn".into()), sub_lang: Some("eng".into()), sub_off: false });
        std::fs::write(&path, write_track_prefs_json(&map)).unwrap();
        assert_eq!(read_track_prefs_from(&path), map);
        let _ = std::fs::remove_dir_all(&dir);

        let clean = redact_diagnostic("fetch https://torrentio.strem.fun/rd=SECRET/manifest.json api_key=12345 token=abc");
        assert!(!clean.contains("SECRET") && !clean.contains("12345") && !clean.contains("abc"), "{clean}");
        assert!(clean.contains("[URL]") && clean.contains("[REDACTED]"), "{clean}");
    }

    #[test]
    fn parses_trakt_veylo_and_imdb_history_imports() {
        let trakt = r#"[
            {"plays":1,"movie":{"title":"Dune","year":2021,"ids":{"imdb":"tt1160419"}}},
            {"plays":2,"show":{"title":"Severance","year":2022,"ids":{"imdb":"tt11280740"}}}
        ]"#;
        let t = parse_watch_history_import(trakt);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].id.as_str(), t[0].kind.as_str(), t[0].name.as_str(), t[0].watched), ("tt1160419", "movie", "Dune", true));
        assert_eq!((t[1].id.as_str(), t[1].kind.as_str(), t[1].name.as_str()), ("tt11280740", "series", "Severance"));

        let veylo = r#"{"veylo_export":1,"items":[{"id":"tt0133093","type":"movie","name":"The Matrix","year":"1999","watched":true,"saved":true,"liked":1}]}"#;
        let v = parse_watch_history_import(veylo);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].id.as_str(), v[0].saved, v[0].liked), ("tt0133093", true, 1));

        let csv = "Const,Your Rating,Date Rated,Title Type,URL,Title\ntt0468569,10,2024-01-01,Movie,https://imdb.com,The Dark Knight\ntt0903747,10,2024-01-02,TV Series,https://imdb.com,Breaking Bad\n";
        let c = parse_watch_history_import(csv);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].id.as_str(), c[0].kind.as_str(), c[0].name.as_str()), ("tt0468569", "movie", "The Dark Knight"));
        assert_eq!((c[1].id.as_str(), c[1].kind.as_str(), c[1].name.as_str()), ("tt0903747", "series", "Breaking Bad"));
    }
}
