//! Artwork and metadata from outside the add-ons: posters, BetterPosters (charts, tagged posters,
//! fuller details), TMDB (artwork, cast, videos, Picks candidates) and TVmaze. Without a TMDB key
//! the keyless sources in open.rs stand in.
use super::*;

// ---------------------------------------------------------------- posters

pub(super) static BETTER_POSTERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(super) static HDR_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

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

pub(super) async fn bp_get(path: String) -> Option<Value> {
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
    if let Ok(mut c) = cache.lock() {
        // Expired answers (whole charts among them) go before a new one is kept.
        c.retain(|_, (at, hit)| now - *at < if hit.is_some() { 900 } else { 120 });
        cache_insert(&mut c, 400, path, (now, got.clone()));
    }
    got
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
pub(super) async fn transparent_edges(url: String) -> Option<bool> {
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
    /// Key crew by job ("Directed by", "Created by", "Written by", "Music by", "Cinematography by"),
    /// at most three each, with their photos where known.
    pub crew: Vec<(String, Vec<Person>)>,
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

pub(super) const TMDB_IMG: &str = "https://image.tmdb.org/t/p";

/// The production's own material from one TMDB details answer (see Artwork).
pub(super) fn production(details: &Value, kind: &str, backdrop: Option<&str>) -> Artwork {
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
    let mut crew: Vec<(String, Vec<Person>)> = Vec::new();
    let mut credit = |label: &str, c: &Value| {
        let person = Person { name: s(c, "name"), role: String::new(), photo: img("w185", &c["profile_path"]) };
        if person.name.is_empty() { return; }
        match crew.iter_mut().find(|(l, _)| l == label) {
            Some((_, people)) => if !people.iter().any(|p| p.name == person.name) && people.len() < 3 { people.push(person) },
            None => crew.push((label.to_owned(), vec![person])),
        }
    };
    for c in details["created_by"].as_array().into_iter().flatten() { credit("Created by", c); }
    for c in credits["crew"].as_array().into_iter().flatten() {
        let jobs: Vec<String> = if tv { c["jobs"].as_array().into_iter().flatten().map(|j| s(j, "job")).collect() } else { vec![s(c, "job")] };
        for job in jobs {
            let label = match job.as_str() { "Director" if !tv => "Directed by", "Screenplay" | "Writer" if !tv => "Written by",
                "Original Music Composer" => "Music by", "Director of Photography" if !tv => "Cinematography by", _ => continue };
            credit(label, c);
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

pub(super) static ARTWORK: OnceLock<Mutex<HashMap<String, Artwork>>> = OnceLock::new();

pub(super) fn imdb_of(m: &Media) -> Option<String> {
    Some(m.id.as_str()).filter(|id| id.starts_with("tt"))
        .or_else(|| m.raw["imdb_id"].as_str().filter(|id| id.starts_with("tt")))
        .map(|id| id.split(':').next().unwrap_or(id).to_owned())
}

pub(super) static TMDB_KEY: Mutex<Option<String>> = Mutex::new(None);
pub(super) fn set_tmdb_key(key: Option<String>) { if let Ok(mut k) = TMDB_KEY.lock() { *k = key; } }
pub(super) fn tmdb_key() -> Option<String> { TMDB_KEY.lock().ok().and_then(|k| k.clone()) }

/// GET a TMDB v3 endpoint. The key travels as a query parameter and is never logged.
pub(super) async fn tmdb(path: String, query: Vec<(&'static str, String)>) -> Option<Value> {
    let key = tmdb_key()?;
    on_runtime(async move {
        let response = backend().http.get(format!("https://api.themoviedb.org/3/{path}"))
            .query(&[("api_key", key)]).query(&query).send().await.ok()?.error_for_status().ok()?;
        response.json::<Value>().await.ok()
    }).await.ok().flatten()
}

pub(super) fn best_image(list: &Value, languages: &[Option<&str>]) -> Option<String> {
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
    if let Ok(mut c) = cache.lock() { cache_insert(&mut c, 800, cache_key, art.clone()); }
    Some(art)
}

/// Whether TMDB can be asked (a key is set).
pub fn tmdb_ready() -> bool { tmdb_key().is_some() }

/// TMDB genre ids and names, per "movie" or "tv".
pub(super) type GenreLists = HashMap<String, Vec<(i64, String)>>;
pub(super) static TMDB_GENRES: OnceLock<Mutex<GenreLists>> = OnceLock::new();

/// TMDB's genre list for "movie" or "tv", cached.
pub(super) async fn tmdb_genre_list(kind: &str) -> Vec<(i64, String)> {
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
/// Without a key, Cinemeta's lists answer instead.
pub async fn discover_candidates(genre: Option<String>, kind: Option<String>, style: Style, from: Option<i32>, to: Option<i32>, pages: Vec<u32>) -> Vec<Media> {
    if !tmdb_ready() { return cinemeta_candidates(genre, kind, style, from, to, pages).await; }
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
pub(super) async fn tmdb_titles(tk: &str, results: Vec<Value>, genre: Option<&str>) -> Vec<Media> {
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

/// Titles TMDB recommends for one you kept or liked: the pool grows toward your taste. Without
/// a key, Cinemeta's lists for its genres.
pub async fn related(m: &Media) -> Vec<Media> {
    if !tmdb_ready() { return cinemeta_related(m).await; }
    let Some((kind, id)) = artwork(m).await.and_then(|a| a.tmdb) else { return Vec::new() };
    let Some(v) = tmdb(format!("{kind}/{id}/recommendations"), vec![("page", "1".into())]).await else { return Vec::new() };
    // Formats (reality, news, soap, talk) and kids' series are never offered; skip their lookups.
    let results: Vec<Value> = v["results"].as_array().into_iter().flatten()
        .filter(|r| !r["genre_ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).any(|g| [10764, 10763, 10766, 10767, 10762].contains(&g)))
        .filter(|r| r["vote_count"].as_i64().unwrap_or(0) >= 50)
        .take(12).cloned().collect();
    tmdb_titles(&kind, results, None).await
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
        let score = s["weight"].as_f64().unwrap_or(0.0) + s["rating"]["average"].as_f64().unwrap_or(0.0) * 4.0;
        tvmaze_media(s).map(|m| (score, m))
    }).collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().take(40).map(|(_, m)| m).collect()
}

pub(super) async fn tmdb_by_name(m: &Media) -> Option<(&'static str, i64)> {
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

/// TMDB episode stills for one season: episode number -> image URL. Without a key, TVmaze's.
pub async fn season_stills(m: &Media, season: i64) -> HashMap<i64, String> {
    if !tmdb_ready() { return tvmaze_stills(m, season).await; }
    let Some(Artwork { tmdb: Some((kind, id)), .. }) = artwork(m).await else { return HashMap::new() };
    if kind != "tv" { return HashMap::new(); }
    let Some(v) = tmdb(format!("tv/{id}/season/{season}"), vec![]).await else { return HashMap::new() };
    v["episodes"].as_array().into_iter().flatten().filter_map(|e| {
        Some((e["episode_number"].as_i64()?, format!("https://image.tmdb.org/t/p/w400{}", e["still_path"].as_str()?)))
    }).collect()
}

/// Keywords use the existing in-memory Settings key and fixed TMDB endpoints, else Wikidata. A
/// successful empty list is distinct from a network/auth failure, which enrichment retries later.
pub(crate) async fn picks_keywords(m: &Media) -> Option<Vec<String>> {
    if !tmdb_ready() { return wikidata_keywords(m).await; }
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
