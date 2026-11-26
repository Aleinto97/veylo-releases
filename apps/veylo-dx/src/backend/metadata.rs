//! Artwork and metadata from outside the add-ons: posters, BetterPosters (charts, tagged posters,
//! fuller details), keyless ElfHosted metadata (cast portraits, logos, trailers, episode stills),
//! TVmaze, AniList/ani.zip and Wikidata/Wikipedia (open.rs). No API keys required.
use super::*;

// ---------------------------------------------------------------- posters

pub(super) static BETTER_POSTERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(super) static HDR_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Windows: send HDR to an HDR screen (the setting, kept in memory for the player).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn hdr_output() -> bool { HDR_OUTPUT.load(std::sync::atomic::Ordering::Relaxed) }

/// Poster images in order of preference: BetterPosters (by IMDb id, rating, genre and
/// 4K/1080p/Dolby quality badges), then the add-on's own poster (Cinemeta for most titles),
/// then the backdrop. A failing image moves on to the next one.
pub fn posters(m: &Media) -> Vec<String> {
    let imdb = Some(m.id.as_str()).filter(|id| id.starts_with("tt"))
        .or_else(|| m.raw["imdb_id"].as_str().filter(|id| id.starts_with("tt")));
    let mut out = Vec::new();
    if BETTER_POSTERS.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(id) = imdb { out.push(format!("https://btttr.cc/poster-q/imdb/poster-default/{id}.jpg")); }
    }
    out.push(m.poster.clone());
    // Cinemeta's image service has a poster for nearly every IMDb title; try high-res first.
    if let Some(id) = imdb {
        out.push(format!("https://images.metahub.space/poster/large/{id}/img"));
        out.push(format!("https://images.metahub.space/poster/medium/{id}/img"));
    }
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
pub(super) const BP_BASE: &str = "https://btttr.cc";
/// Home row keys that are BetterPosters charts.
pub(super) const BP_ROW: &str = "betterposters|";
/// The "today" charts (TMDB's most watched today) are Top 10 shelves.
pub(super) const BP_CHARTS: [(&str, &str, &str); 7] = [
    ("movie", "trakt-trending", "Trending movies"), ("series", "trakt-trending", "Trending series"),
    ("movie", "tmdb-today", "Top 10 movies today"), ("series", "tmdb-today-shows", "Top 10 series today"),
    ("movie", "trakt-popular", "Popular movies"), ("series", "trakt-popular", "Popular series"),
    ("movie", "tmdb-top", "Top rated movies"),
];
pub(super) fn bp_ranked(id: &str) -> bool { id.starts_with("tmdb-today") }

pub fn better_posters_on() -> bool { BETTER_POSTERS.load(std::sync::atomic::Ordering::Relaxed) }

/// Answers by path and when they came: kept 15 minutes, a failure 2 minutes, so a stalled
/// server isn't asked again on every screen.
pub(super) type Answers = HashMap<String, (i64, Option<Value>)>;
pub(super) static BP_CACHE: OnceLock<Mutex<Answers>> = OnceLock::new();
type InflightMap<T> = OnceLock<Mutex<HashMap<String, std::sync::Arc<tokio::sync::OnceCell<T>>>>>;
static BP_INFLIGHT: InflightMap<Option<Value>> = OnceLock::new();
static ELF_INFLIGHT: InflightMap<Option<Value>> = OnceLock::new();
static JW_INFLIGHT: InflightMap<Option<Vec<String>>> = OnceLock::new();

async fn dedup_inflight<T: Clone + Send + Sync + 'static>(
    map: &'static InflightMap<T>,
    key: &str,
    fetch: impl Future<Output = T>,
) -> T {
    let cell = {
        let mut g = map.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        g.entry(key.to_owned()).or_insert_with(|| std::sync::Arc::new(tokio::sync::OnceCell::new())).clone()
    };
    let val = cell.get_or_init(|| fetch).await.clone();
    if let Ok(mut g) = map.get_or_init(Default::default).lock() {
        if g.get(key).is_some_and(|c| std::sync::Arc::ptr_eq(c, &cell)) {
            g.remove(key);
        }
    }
    val
}

pub(super) async fn bp_get(path: String) -> Option<Value> {
    let cache = BP_CACHE.get_or_init(Default::default);
    let now = now_secs();
    if let Some((at, hit)) = cache.lock().ok().and_then(|c| c.get(&path).cloned()) {
        if now - at < if hit.is_some() { 900 } else { 120 } { return hit; }
    }
    let key = path.clone();
    dedup_inflight(&BP_INFLIGHT, &key, async move {
        let now = now_secs();
        if let Some((at, hit)) = cache.lock().ok().and_then(|c| c.get(&path).cloned()) {
            if now - at < if hit.is_some() { 900 } else { 120 } { return hit; }
        }
        let url = format!("{BP_BASE}/{path}");
        let got = on_runtime(async move {
            let response = backend().http.get(url).timeout(Duration::from_secs(8)).send().await.ok()?.error_for_status().ok()?;
            response.json::<Value>().await.ok()
        }).await.ok().flatten();
        if let Ok(mut c) = cache.lock() {
            // Expired answers (whole charts among them) go before a new one is kept.
            c.retain(|_, (at, hit)| now - *at < if hit.is_some() { 900 } else { 120 });
            cache_insert(&mut c, 400, path, (now, got.clone()));
        }
        got
    }).await
}

/// A chart's titles as Cinemeta titles (`origin`): only IMDb ids, which Cinemeta also serves.
pub(super) fn bp_titles(v: &Value, origin: &str) -> Vec<Media> {
    v["metas"].as_array().into_iter().flatten()
        .filter(|m| s(m, "id").starts_with("tt"))
        .filter_map(|m| Media::from_json(m, origin))
        .scan(HashSet::new(), |seen, m| Some(seen.insert(m.key()).then_some(m))).flatten().collect()
}

/// BetterPosters' metadata for a film or series with an IMDb id.
pub(super) async fn bp_meta(m: &Media) -> Option<Value> {
    if !better_posters_on() || !matches!(m.kind.as_str(), "movie" | "series") { return None; }
    let id = imdb_of(m)?;
    bp_get(format!("meta/{}/{id}.json", m.kind)).await?.get("meta").filter(|v| v.is_object()).cloned()
}

pub(super) fn filled(v: &Value) -> bool {
    !(v.is_null() || v.as_str().is_some_and(|t| t.trim().is_empty()) || v.as_array().is_some_and(Vec::is_empty))
}

/// The add-on's metadata with BetterPosters' laid over it: BetterPosters wins where it has a
/// value, its trailers and credits come first, and the add-on's episode list stays (its ids
/// find sources and keep watched marks) with BetterPosters' titles, stills and synopses on it.
pub(super) fn merge_meta(mut meta: Value, bp: Option<&Value>) -> Value {
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

pub(super) static IDENTS: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
const IDENTS_CACHE_FILE: &str = "idents-v1.json";

fn idents_cache() -> &'static Mutex<HashMap<String, bool>> {
    IDENTS.get_or_init(|| {
        let map = BACKEND.get()
            .and_then(|b| std::fs::read_to_string(b.data_dir.join(IDENTS_CACHE_FILE)).ok())
            .and_then(|t| serde_json::from_str::<HashMap<String, bool>>(&t).ok())
            .unwrap_or_default();
        Mutex::new(map)
    })
}

/// The studio logos that read as white idents: transparent around the mark. A logo printed on its
/// own solid background would turn into a white box, so it is left out. Persisted across launches.
pub async fn idents(studios: Vec<Studio>) -> Vec<Studio> {
    let cache = idents_cache();
    let mut results: Vec<Option<Studio>> = vec![None; studios.len()];
    let mut pending = Vec::new();
    for (i, st) in studios.into_iter().enumerate() {
        let known = cache.lock().ok().and_then(|c| c.get(&st.logo).copied());
        match known {
            Some(true) => results[i] = Some(st),
            Some(false) => {}
            None => pending.push((i, st)),
        }
    }
    if !pending.is_empty() {
        let checked = on_runtime(async move {
            let mut jobs = tokio::task::JoinSet::new();
            for (i, st) in pending {
                jobs.spawn(async move {
                    let probe = st.logo
                        .replace("/w500/", "/w92/")
                        .replace("/w300/", "/w92/")
                        .replace("/w185/", "/w92/")
                        .replace("?width=360", "?width=92");
                    let ok = transparent_edges_inner(probe).await;
                    (i, st, ok)
                });
            }
            let mut done = Vec::new();
            while let Some(res) = jobs.join_next().await {
                if let Ok(item) = res { done.push(item); }
            }
            done
        }).await.unwrap_or_default();
        let mut snapshot: Option<String> = None;
        if let Ok(mut c) = cache.lock() {
            let mut changed = false;
            for (_, st, ok) in &checked {
                // Only cache definite decode verdicts; transient network errors retry next visit.
                if let Some(verdict) = *ok {
                    cache_insert(&mut c, 500, st.logo.clone(), verdict);
                    changed = true;
                }
            }
            if changed {
                snapshot = serde_json::to_string(&*c).ok();
            }
        }
        if let Some(text) = snapshot {
            if let Some(b) = BACKEND.get() {
                write_soon(b.data_dir.join(IDENTS_CACHE_FILE), text);
            }
        }
        for (i, st, ok) in checked {
            if ok == Some(true) { results[i] = Some(st); }
        }
    }
    results.into_iter().flatten().collect()
}

/// Whether the logo is a mark on see-through ground: a transparent corner, and ink over less than
/// about half of it (a solid plate with rounded corners is not an ident).
async fn transparent_edges_inner(url: String) -> Option<bool> {
    const MAX_LOGO_BYTES: usize = 2 * 1024 * 1024;
    let mut resp = backend().http.get(url).timeout(Duration::from_secs(6)).send().await.ok()?.error_for_status().ok()?;
    if resp.content_length().is_some_and(|n| n > MAX_LOGO_BYTES as u64) { return Some(false); }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if bytes.len() + chunk.len() > MAX_LOGO_BYTES { return Some(false); }
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 { return None; }
        let corner = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)].iter().any(|&(x, y)| img.get_pixel(x, y)[3] < 128);
        let ink = img.pixels().filter(|p| p[3] >= 128).count() as f64 / (w as f64 * h as f64);
        Some(corner && ink < 0.55)
    }).await.ok().flatten()
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
pub(super) async fn bp_artwork(m: &Media) -> Option<Artwork> {
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

// ---------------------------------------------------------------- keyless ElfHosted metadata

/// Wide artwork and production details merged across BetterPosters, ElfHosted, TVmaze, AniList
/// and Wikidata. Empty fields fall back to the add-on's (Metahub) images.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Artwork {
    pub genres: Vec<String>,
    pub backdrop: Option<String>,
    /// True only when `backdrop` comes from a verified clean / ad-free source.
    pub backdrop_clean: bool,
    pub logo: Option<String>, pub poster: Option<String>,
    /// YouTube trailers and teasers (video keys), best first.
    pub trailers: Vec<String>,
    /// Additional official frames, tagline, cast with photos and parts, key crew, studio and
    /// network logos, certificate, original language, season posters and videos.
    pub stills: Vec<String>,
    pub tagline: Option<String>,
    pub cast: Vec<Person>,
    /// Key crew by job ("Directed by", "Created by", "Written by", "Music by", "Cinematography by"),
    /// at most three each, with their photos where known.
    pub crew: Vec<(String, Vec<Person>)>,
    pub studios: Vec<Studio>,
    pub certificate: Option<String>,
    pub language: Option<String>,
    /// ISO 639-1 original language code (e.g. "en", "ko", "ja", "it") for autoplay fallback.
    pub original_language: Option<String>,
    /// Original and localized alternate titles (e.g. "Scontrarsi a Natale") for autoplay matching.
    pub alt_titles: Vec<String>,
    /// Expected movie/episode duration in seconds, for pre-probe bitrate and trailer rejection.
    pub runtime_secs: Option<f64>,
    pub season_posters: HashMap<i64, String>,
    pub videos: Vec<Video>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Person {
    pub name: String,
    pub role: String,
    pub photo: Option<String>,
    pub tvmaze_id: Option<i64>,
    pub wikidata_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Studio { pub name: String, pub logo: String }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Video { pub youtube: String, pub name: String, pub kind: String, pub official: bool }

const ELFHOSTED_BASE: &str = "https://tmdb.elfhosted.com";
const JUSTWATCH_GRAPHQL: &str = "https://apis.justwatch.com/graphql";
const JUSTWATCH_IMAGES: &str = "https://images.justwatch.com";
pub(super) static ELF_CACHE: OnceLock<Mutex<Answers>> = OnceLock::new();
pub(super) static JW_STILLS_CACHE: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
pub(super) static ARTWORK: OnceLock<Mutex<HashMap<String, Artwork>>> = OnceLock::new();

pub(super) fn imdb_of(m: &Media) -> Option<String> {
    Some(m.id.as_str()).filter(|id| id.starts_with("tt"))
        .or_else(|| m.raw["imdb_id"].as_str().filter(|id| id.starts_with("tt")))
        .map(|id| id.split(':').next().unwrap_or(id).to_owned())
}

/// Downscale oversized `/original/` TMDB image URLs to a crisp WebView-friendly size.
fn sized_tmdb_img(url: &str, size: &str) -> String {
    if url.contains("image.tmdb.org/t/p/original/") {
        url.replacen("/original/", &format!("/{size}/"), 1)
    } else {
        url.to_owned()
    }
}

/// Parse runtime strings like `"2h47min"`, `"166 min"`, `"45min"`, or `"1h 32m"` into seconds.
pub(super) fn parse_runtime_secs(raw: &str) -> Option<f64> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() { return None; }
    let mut mins: u64 = 0;
    let mut cur: u64 = 0;
    let mut saw_digit = false;
    let mut saw_unit = false;
    for ch in lower.chars() {
        if let Some(d) = ch.to_digit(10) {
            cur = cur.saturating_mul(10).saturating_add(u64::from(d));
            saw_digit = true;
        } else if ch == 'h' && saw_digit {
            mins = mins.saturating_add(cur.saturating_mul(60));
            cur = 0;
            saw_digit = false;
            saw_unit = true;
        } else if ch == 'm' && saw_digit {
            mins = mins.saturating_add(cur);
            cur = 0;
            saw_digit = false;
            saw_unit = true;
        }
    }
    if saw_digit && !saw_unit { mins = mins.saturating_add(cur); }
    (mins > 0 && mins <= 600).then(|| (mins * 60) as f64)
}

/// Fetch keyless Stremio-protocol metadata from ElfHosted (`tmdb.elfhosted.com`), cached 15 min.
pub(super) async fn elfhosted_meta(kind: &str, imdb: &str) -> Option<Value> {
    if !matches!(kind, "movie" | "series") || !imdb.starts_with("tt") { return None; }
    let path = format!("meta/{kind}/{imdb}.json");
    let cache = ELF_CACHE.get_or_init(Default::default);
    let now = now_secs();
    if let Some((at, hit)) = cache.lock().ok().and_then(|c| c.get(&path).cloned()) {
        if now - at < if hit.is_some() { 900 } else { 120 } { return hit; }
    }
    let key = path.clone();
    dedup_inflight(&ELF_INFLIGHT, &key, async move {
        let now = now_secs();
        if let Some((at, hit)) = cache.lock().ok().and_then(|c| c.get(&path).cloned()) {
            if now - at < if hit.is_some() { 900 } else { 120 } { return hit; }
        }
        let url = format!("{ELFHOSTED_BASE}/{path}");
        let got = on_runtime(async move {
            let response = backend().http.get(url).timeout(Duration::from_secs(6)).send().await.ok()?.error_for_status().ok()?;
            let v = response.json::<Value>().await.ok()?;
            v.get("meta").filter(|m| m.is_object()).cloned()
        }).await.ok().flatten();
        if let Ok(mut c) = cache.lock() {
            c.retain(|_, (at, hit)| now - *at < if hit.is_some() { 900 } else { 120 });
            cache_insert(&mut c, 400, path, (now, got.clone()));
        }
        got
    }).await
}

/// Parse an ElfHosted `/meta/{kind}/{imdb}.json` object into Veylo's `Artwork` model.
pub(super) fn parse_elfhosted_artwork(meta: &Value) -> Artwork {
    let url_of = |key: &str, size: &str| {
        Some(s(meta, key))
            .filter(|u| u.starts_with("https://"))
            .map(|u| sized_tmdb_img(&u, size))
    };
    let genres: Vec<String> = meta["genres"].as_array().or_else(|| meta["genre"].as_array())
        .into_iter().flatten().filter_map(Value::as_str).map(crate::picks_rules::canonical_genre).collect();
    let trailers: Vec<String> = meta["trailers"].as_array().into_iter().flatten()
        .map(|t| s(t, "source")).filter(|k| !k.is_empty()).collect();
    let videos: Vec<Video> = meta["trailers"].as_array().into_iter().flatten()
        .filter_map(|t| {
            let youtube = s(t, "source");
            if youtube.is_empty() { return None; }
            let kind = Some(s(t, "type")).filter(|k| !k.is_empty()).unwrap_or_else(|| "Trailer".into());
            let name = Some(s(t, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| kind.clone());
            Some(Video { youtube, name, kind, official: true })
        }).take(12).collect();
    let cast: Vec<Person> = meta["app_extras"]["cast"].as_array().into_iter().flatten().take(15).filter_map(|c| {
        let name = s(c, "name");
        if name.is_empty() { return None; }
        let role = s(c, "character");
        let photo = Some(s(c, "photo")).filter(|u| u.starts_with("https://"));
        Some(Person { name, role, photo, ..Default::default() })
    }).collect();
    let mut crew: Vec<(String, Vec<Person>)> = Vec::new();
    for (field_name, label) in [("director", "Directed by"), ("writer", "Written by")] {
        let people: Vec<Person> = meta[field_name].as_array().into_iter().flatten()
            .filter_map(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).take(3)
            .map(|name| Person { name: name.to_owned(), ..Default::default() }).collect();
        if !people.is_empty() { crew.push((label.to_owned(), people)); }
    }
    let certificate = Some(s(meta, "ageRating")).filter(|c| !c.is_empty() && c.len() <= 10);
    let runtime_secs = parse_runtime_secs(&s(meta, "runtime"));
    Artwork {
        genres,
        backdrop: url_of("background", "w1280"),
        logo: url_of("logo", "w500"),
        poster: url_of("poster", "w780"),
        trailers,
        cast,
        crew,
        certificate,
        runtime_secs,
        videos,
        ..Artwork::default()
    }
}

/// Keyless ElfHosted artwork & production metadata (backdrop, logo, cast portraits, trailers,
/// age rating, runtime); cached for the session.
pub async fn elfhosted_artwork(m: &Media) -> Option<Artwork> {
    let imdb = imdb_of(m)?;
    let kind = if m.kind == "movie" { "movie" } else if m.kind == "series" { "series" } else { return None };
    let cache = ARTWORK.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok()?.get(&imdb).cloned() { return Some(hit); }
    let meta = elfhosted_meta(kind, &imdb).await?;
    let art = parse_elfhosted_artwork(&meta);
    if let Ok(mut c) = cache.lock() { cache_insert(&mut c, 800, imdb, art.clone()); }
    Some(art)
}

/// Free keyless artwork for a title across the whole app (Home, Library, Picks, Related):
/// BetterPosters (and TVmaze untouched originals for series) come first; keyless ElfHosted runs
/// in parallel and fills any gaps (cast portraits, logos, backdrops, trailers, runtime).
pub async fn artwork(m: &Media) -> Option<Artwork> {
    let (bp, elf) = tokio::join!(bp_artwork(m), elfhosted_artwork(m));
    let free = match bp {
        Some(art) if art.backdrop.is_some() => Some(art),
        other => match tvmaze_backdrop_artwork(m).await {
            Some(tv) => Some(with_extras(other.unwrap_or_default(), tv)),
            None => other,
        },
    };
    match (free, elf) {
        (Some(f), Some(e)) => Some(with_extras(f, e)),
        (Some(f), None) => Some(f),
        (None, Some(e)) => Some(e),
        (None, None) => None,
    }
}

/// Shrink a TMDB backdrop URL to `w300` for the blurred 72px `.ambient` glow: visually identical
/// after blur, 16x smaller GPU texture, and decodes in ~10ms instead of stalling WebView frames.
pub fn ambient_url(url: &str) -> String {
    if url.contains("image.tmdb.org/t/p/") {
        url.replacen("/original/", "/w300/", 1)
            .replacen("/w1280/", "/w300/", 1)
            .replacen("/w780/", "/w300/", 1)
    } else {
        url.to_owned()
    }
}

static TITLE_ART_CACHE: OnceLock<Mutex<HashMap<String, Artwork>>> = OnceLock::new();

/// Parse a JustWatch GraphQL response into crisp, textless 1440px widescreen backdrops for the
/// exact matching IMDb ID (never matching on title alone).
pub(super) fn parse_justwatch_stills(v: &Value, imdb: &str) -> Vec<String> {
    let edges = v["data"]["popularTitles"]["edges"].as_array();
    let Some(content) = edges.into_iter().flatten().find_map(|e| {
        let c = &e["node"]["content"];
        (c["externalIds"]["imdbId"].as_str() == Some(imdb)).then_some(c)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for b in content["backdrops"].as_array().into_iter().flatten() {
        let Some(path) = b["backdropUrl"].as_str() else { continue };
        if !path.starts_with("/backdrop/") { continue; }
        let url = format!("{JUSTWATCH_IMAGES}{path}");
        if !out.contains(&url) { out.push(url); }
        if out.len() >= 5 { break; }
    }
    out
}

/// Keyless multi-frame widescreen backdrops (~60-90ms) from JustWatch's public GraphQL endpoint,
/// matched strictly by IMDb ID and cached for the session.
pub(super) async fn justwatch_stills(m: &Media) -> Vec<String> {
    let Some(imdb) = imdb_of(m) else { return Vec::new() };
    let obj_type = if m.kind == "movie" { "MOVIE" } else if m.kind == "series" { "SHOW" } else { return Vec::new() };
    if m.name.trim().is_empty() { return Vec::new(); }
    let cache = JW_STILLS_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&imdb).cloned()) {
        return hit;
    }
    let name = m.name.clone();
    let key = imdb.clone();
    let got = dedup_inflight(&JW_INFLIGHT, &key, async move {
        if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&imdb).cloned()) {
            return Some(hit);
        }
        let query = r#"query GetBackdrops($country: Country!, $language: Language!, $first: Int!, $filter: TitleFilter) {
  popularTitles(country: $country, first: $first, filter: $filter) {
    edges {
      node {
        content(country: $country, language: $language) {
          externalIds { imdbId }
          backdrops(profile: S1440, format: JPG) { backdropUrl }
        }
      }
    }
  }
}"#;
        let body = json!({
            "query": query,
            "variables": {
                "country": "US",
                "language": "en",
                "first": 8,
                "filter": { "searchQuery": name, "objectTypes": [obj_type] }
            }
        });
        let imdb_match = imdb.clone();
        let res = on_runtime(async move {
            let resp = backend().http
                .post(JUSTWATCH_GRAPHQL)
                .json(&body)
                .timeout(Duration::from_secs(4))
                .send().await.ok()?.error_for_status().ok()?;
            let v = resp.json::<Value>().await.ok()?;
            Some(parse_justwatch_stills(&v, &imdb_match))
        }).await.ok().flatten();
        if let Some(ref stills) = res {
            if let Ok(mut c) = cache.lock() { cache_insert(&mut c, 600, imdb, stills.clone()); }
        }
        res
    }).await;
    got.unwrap_or_default()
}

/// Clear session artwork caches (called when the user toggles BetterPosters).
pub fn clear_artwork_caches() {
    if let Some(c) = TITLE_ART_CACHE.get() { if let Ok(mut m) = c.lock() { m.clear(); } }
    if let Some(c) = ARTWORK.get() { if let Ok(mut m) = c.lock() { m.clear(); } }
    if let Some(c) = BP_CACHE.get() { if let Ok(mut m) = c.lock() { m.clear(); } }
    if let Some(c) = ELF_CACHE.get() { if let Ok(mut m) = c.lock() { m.clear(); } }
    if let Some(c) = JW_STILLS_CACHE.get() { if let Ok(mut m) = c.lock() { m.clear(); } }
    clear_quality_cache();
}

/// Synchronously read cached title hero artwork from this session so re-opening a title paints
/// its clean backdrop, logo, tagline, and videos on Frame 0 with zero loading state.
pub fn cached_title_artwork(m: &Media) -> Option<Artwork> {
    TITLE_ART_CACHE.get()?.lock().ok()?.get(&m.key()).cloned()
}

/// Synchronously read cached card artwork (Home Tonight's pick, Library week cards, Picks duel)
/// from session caches so returning to a screen paints artwork on Frame 0.
pub fn cached_artwork(m: &Media) -> Option<Artwork> {
    if let Some(hit) = cached_title_artwork(m) { return Some(hit); }
    let imdb = imdb_of(m)?;
    ARTWORK.get()?.lock().ok()?.get(&imdb).cloned()
}

/// Synchronously read cached JustWatch stills for a title (if already fetched in this session).
pub fn cached_title_stills(m: &Media) -> Option<Vec<String>> {
    let imdb = imdb_of(m)?;
    JW_STILLS_CACHE.get()?.lock().ok()?.get(&imdb).cloned()
}

/// Optional keyless multi-frame backdrops from JustWatch, fetched independently so slow
/// responses never delay the primary hero backdrop.
pub async fn title_stills(m: &Media) -> Vec<String> {
    justwatch_stills(m).await
}

/// Merge JustWatch stills into hero artwork without displacing an existing primary backdrop.
pub fn merge_title_stills(art: Option<Artwork>, stills: &[String]) -> Option<Artwork> {
    if stills.is_empty() {
        return art;
    }
    let mut out = art.unwrap_or_default();
    let mut iter = stills.iter().cloned();
    if out.backdrop.is_none() {
        out.backdrop = iter.next();
    }
    for s in iter {
        if out.stills.len() >= 4 { break; }
        if Some(s.as_str()) != out.backdrop.as_deref() && !out.stills.contains(&s) {
            out.stills.push(s);
        }
    }
    Some(out)
}

/// Fast title hero artwork (~90-200ms): runs BetterPosters + keyless ElfHosted in parallel
/// without waiting on optional JustWatch stills or slower TVmaze/AniList/Wikidata calls.
pub async fn title_artwork_fast(m: &Media) -> Option<Artwork> {
    if let Some(hit) = cached_title_artwork(m) {
        return Some(hit);
    }
    let (bp, elf) = tokio::join!(bp_artwork(m), elfhosted_artwork(m));
    let mut out = match (bp, elf) {
        (Some(f), Some(e)) => Some(with_extras(f, e)),
        (Some(f), None) => Some(f),
        (None, Some(e)) => Some(e),
        (None, None) => None,
    };
    if let Some(cached_stills) = cached_title_stills(m) {
        out = merge_title_stills(out, &cached_stills);
    }
    // Fallback second frame via Metahub (usually a distinct textless backdrop from BetterPosters)
    // when no stills are cached yet for this IMDb title.
    if let Some(ref mut art) = out {
        if art.stills.is_empty() && art.backdrop.is_some() {
            if let Some(imdb) = imdb_of(m) {
                let metahub = format!("https://images.metahub.space/background/medium/{imdb}/img");
                if art.backdrop.as_deref() != Some(metahub.as_str()) {
                    art.stills.push(metahub);
                }
            }
        }
    }
    if let Some(ref art) = out {
        if let Ok(mut c) = TITLE_ART_CACHE.get_or_init(Default::default).lock() {
            cache_insert(&mut c, 400, m.key(), art.clone());
        }
    }
    out
}

/// Popular, well-rated titles for a genre, type, style and years, as Cinemeta titles (IMDb ids)
/// so they open and play like any other. Fully keyless via Cinemeta's ranked catalogs.
pub async fn discover_candidates(selected_genres: Vec<String>, kind: Option<String>, style: Style, from: Option<i32>, to: Option<i32>, pages: Vec<u32>) -> Vec<Media> {
    cinemeta_candidates(selected_genres, kind, style, from, to, pages).await
}

/// Titles related to one you kept or liked: Cinemeta's lists for its genres, then re-ranked by
/// Picks' on-device story embedding model.
pub async fn related(m: &Media) -> Vec<Media> {
    cinemeta_related(m).await
}

pub(super) fn tvmaze_genres(s: &Value) -> Vec<String> {
    let mut genres: Vec<String> = s["genres"].as_array().into_iter().flatten().filter_map(Value::as_str)
        .map(|g| if g == "Science-Fiction" { "Sci-Fi".to_owned() } else { g.to_owned() }).collect();
    // TVmaze classifies animation in the show's TYPE, often not its genres (e.g. Simpsons).
    if s["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("animation") || t.eq_ignore_ascii_case("anime"))
        && !genres.iter().any(|g| g.eq_ignore_ascii_case("animation")) { genres.push("Animation".into()); }
    genres
}

pub(super) static TVMAZE: OnceLock<tokio::sync::OnceCell<Vec<Value>>> = OnceLock::new();

/// Series from TVmaze (free, no key): popular, well-rated shows with IMDb ids, sampled across
/// its index once per session, as Cinemeta titles so they open and play like any other.
pub async fn tvmaze_candidates(selected_genres: Vec<String>, style: Style, from: Option<i32>, to: Option<i32>) -> Vec<Media> {
    let cell = TVMAZE.get_or_init(tokio::sync::OnceCell::new);
    // An empty answer (offline, TVmaze down) is not kept: the next visit asks again.
    let shows = cell.get_or_try_init(|| async {
        // Fetch sampled pages with bounded concurrency (4 at a time) instead of 16 sequential requests.
        let all = on_runtime(async move {
            let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
            let mut jobs = tokio::task::JoinSet::new();
            for page in [0u32, 4, 8, 12, 16, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70] {
                let gate = gate.clone();
                jobs.spawn(async move {
                    let _permit = gate.acquire_owned().await;
                    let r = backend().http.get(format!("https://api.tvmaze.com/shows?page={page}")).send().await.ok()?.error_for_status().ok()?;
                    r.json::<Vec<Value>>().await.ok()
                });
            }
            let mut all = Vec::new();
            while let Some(done) = jobs.join_next().await {
                if let Ok(Some(list)) = done {
                    all.extend(list.into_iter().filter(|s| s["externals"]["imdb"].is_string() && s["weight"].as_i64().unwrap_or(0) >= 70
                        && s["rating"]["average"].as_f64().unwrap_or(0.0) >= 6.8));
                }
            }
            all
        }).await.unwrap_or_default();
        if all.is_empty() { Err(()) } else { Ok(all) }
    }).await;
    let Ok(shows) = shows else { return Vec::new() };
    let wanted: Vec<String> = selected_genres.iter().map(|g| match g.to_lowercase().as_str() { "sci-fi" | "science fiction" => "science-fiction".to_owned(), x => x.to_owned() }).collect();
    let mut out: Vec<(f64, Media)> = shows.iter().filter_map(|s| {
        let genres = tvmaze_genres(s);
        let lower: Vec<String> = genres.iter().map(|g| g.to_lowercase().replace("sci-fi", "science-fiction")).collect();
        let animated = lower.iter().any(|g| g == "anime" || g == "animation");
        match style { Style::Animation if !animated => return None, Style::Live if animated => return None, _ => {} }
        if !wanted.iter().all(|w| lower.iter().any(|g| g == w)) { return None; }
        // The years it aired: from its premiere to its end (or still running).
        let year: i32 = s["premiered"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0);
        let ended: Option<i32> = s["ended"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok());
        let last = ended.unwrap_or(9999);
        if from.is_some_and(|f| year == 0 || last < f) || to.is_some_and(|t| year == 0 || year > t) { return None; }
        let score = s["weight"].as_f64().unwrap_or(0.0) + s["rating"]["average"].as_f64().unwrap_or(0.0) * 4.0;
        tvmaze_media(s).map(|m| (score, m))
    }).collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().take(40).map(|(_, m)| m).collect()
}

/// Episode stills for one season: episode number -> high-resolution image URL.
/// Free keyless sources ALWAYS come first: TVmaze untouched originals (all series), then
/// ARM + ani.zip TVDB screencaps (for animated series), then keyless ElfHosted stills for any
/// remaining missing episodes.
pub async fn season_stills(m: &Media, season: i64) -> HashMap<i64, String> {
    let (mut out, anime) = tokio::join!(tvmaze_stills(m, season), anime_stills(m, season));
    for (ep, url) in anime {
        out.entry(ep).or_insert(url);
    }
    if m.kind != "series" { return out; }
    let Some(imdb) = imdb_of(m) else { return out };
    let Some(meta) = elfhosted_meta("series", &imdb).await else { return out };
    for e in meta["videos"].as_array().into_iter().flatten() {
        if e["season"].as_i64() != Some(season) { continue; }
        let num = e["episode"].as_i64().or_else(|| e["number"].as_i64());
        let thumb = e["thumbnail"].as_str().filter(|u| u.starts_with("https://"));
        if let (Some(num), Some(thumb)) = (num, thumb) {
            out.entry(num).or_insert_with(|| sized_tmdb_img(thumb, "w780"));
        }
    }
    out
}

/// Keyless Picks keywords from Wikidata's subjects, finer genres and settings.
/// A successful empty list is distinct from a network failure, which enrichment retries later.
pub(crate) async fn picks_keywords(m: &Media) -> Option<Vec<String>> {
    wikidata_keywords(m).await
}
