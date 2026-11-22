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
}

/// The audio language when none was chosen, or the saved one is unknown.
pub const DEFAULT_LANGUAGE: &str = "english";

/// The saved audio language, English when there is none (older settings saved "Any language" as null).
pub(super) fn audio_language(saved: &Value) -> String {
    saved["stream_language"].as_str().filter(|l| crate::addons::language(l).is_some()).unwrap_or(DEFAULT_LANGUAGE).to_owned()
}

impl Default for Settings { fn default() -> Self { Settings { better_posters: true, tmdb_key: None, picks_hide_watched: true, rd_key: None, stream_language: Some(DEFAULT_LANGUAGE.into()), rd_key_removed: false, hdr_output: true } } }

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
    }
}

pub fn save_settings(settings: &Settings) {
    let _ = std::fs::write(backend().data_dir.join("settings.json"), json!({"better_posters": settings.better_posters, "tmdb_key": settings.tmdb_key, "picks_hide_watched": settings.picks_hide_watched, "rd_key": settings.rd_key, "stream_language": settings.stream_language, "rd_key_removed": settings.rd_key_removed, "hdr_output": settings.hdr_output}).to_string());
    BETTER_POSTERS.store(settings.better_posters, std::sync::atomic::Ordering::Relaxed);
    HDR_OUTPUT.store(settings.hdr_output, std::sync::atomic::Ordering::Relaxed);
    set_stream_language(settings.stream_language.clone());
    set_tmdb_key(settings.tmdb_key.clone());
    // The key and language decide which add-ons answer and with which links.
    forget_sources();
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
    let mut out = Vec::new();
    for req in requests.into_iter().take(6) {
        let origin = s(&req, "addon_url");
        if let Ok(v) = exec(req).await { out.extend(metas(&v, &origin)); }
    }
    out
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

/// Write what `write_soon` still holds, now: before a file is read back.
fn flush_writes() {
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
