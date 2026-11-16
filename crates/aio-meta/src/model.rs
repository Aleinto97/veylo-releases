use crate::ids::{canonical, Kind};
use crate::util::{
    dedupe_videos, iso_date, iso_utc, rating, strip_html, tmdb_image, truncate, year,
};
use serde_json::{json, Map, Value};

pub fn genre_name(id: u64) -> Option<&'static str> {
    Some(match id {
        28 => "Action",
        12 => "Adventure",
        16 => "Animation",
        35 => "Comedy",
        80 => "Crime",
        99 => "Documentary",
        18 => "Drama",
        10751 => "Family",
        14 => "Fantasy",
        36 => "History",
        27 => "Horror",
        10402 => "Music",
        9648 => "Mystery",
        10749 => "Romance",
        878 => "Science Fiction",
        10770 => "TV Movie",
        53 => "Thriller",
        10752 => "War",
        37 => "Western",
        10759 => "Action & Adventure",
        10762 => "Kids",
        10763 => "News",
        10764 => "Reality",
        10765 => "Sci-Fi & Fantasy",
        10766 => "Soap",
        10767 => "Talk",
        10768 => "War & Politics",
        _ => return None,
    })
}

pub fn genre_names_from_ids(ids: Option<&Value>) -> Vec<String> {
    ids.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_u64)
                .filter_map(genre_name)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn genre_names(details: &Value) -> Vec<String> {
    details
        .get("genres")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn best_image(details: &Value, section: &str, size: &str, language: &str) -> Option<String> {
    let images = details.get("images")?.get(section)?.as_array()?;
    let base = language.split('-').next().unwrap_or(language);
    let rank = |item: &Value| -> (u8, f64, u64) {
        let language_score = match item.get("iso_639_1").and_then(Value::as_str) {
            Some(value) if value == base => 3,
            Some("en") => 2,
            None => 1,
            Some(_) => 0,
        };
        let vote = item
            .get("vote_average")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let count = item.get("vote_count").and_then(Value::as_u64).unwrap_or(0);
        (language_score, vote, count)
    };
    images
        .iter()
        .filter(|item| item.get("file_path").and_then(Value::as_str).is_some())
        .max_by(|left, right| {
            rank(left)
                .partial_cmp(&rank(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .and_then(|item| item.get("file_path").and_then(Value::as_str))
        .and_then(|path| tmdb_image(Some(path), size))
}

pub fn backdrop_image(details: &Value, size: &str, language: &str) -> Option<String> {
    let images = details.get("images")?.get("backdrops")?.as_array()?;
    let language = language.split('-').next().unwrap_or(language);
    let chosen = images
        .iter()
        .find(|item| item.get("iso_639_1").and_then(Value::as_str) == Some("xx"))
        .or_else(|| {
            images
                .iter()
                .find(|item| item.get("iso_639_1").is_some_and(Value::is_null))
        })
        .or_else(|| {
            images.iter().find(|item| {
                item.get("iso_639_1")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(language))
            })
        })
        .or_else(|| images.first());
    chosen
        .and_then(|item| item.get("file_path").and_then(Value::as_str))
        .and_then(|path| tmdb_image(Some(path), size))
}

pub fn tmdb_preview(item: &Value, kind: Kind) -> Option<Value> {
    let id = item.get("id").and_then(Value::as_u64)?;
    let name = match kind {
        Kind::Movie => item.get("title"),
        Kind::Series => item.get("name"),
    }
    .and_then(Value::as_str)
    .filter(|name| !name.is_empty())?;
    let date = match kind {
        Kind::Movie => item.get("release_date"),
        Kind::Series => item.get("first_air_date"),
    }
    .and_then(Value::as_str);
    let mut meta = Map::new();
    meta.insert("id".into(), json!(format!("tmdb:{id}")));
    meta.insert("type".into(), json!(kind.as_str()));
    meta.insert("name".into(), json!(name));
    if let Some(poster) = tmdb_image(
        item.get("poster_path").and_then(Value::as_str),
        "w600_and_h900_bestv2",
    ) {
        meta.insert("poster".into(), json!(poster));
    }
    if let Some(background) = tmdb_image(item.get("backdrop_path").and_then(Value::as_str), "w780")
    {
        meta.insert("background".into(), json!(background));
    }
    if let Some(date) = year(date) {
        meta.insert("releaseInfo".into(), json!(date));
    }
    if let Some(overview) = item.get("overview").and_then(Value::as_str) {
        if !overview.is_empty() {
            meta.insert("description".into(), json!(truncate(overview, 240)));
        }
    }
    let genres = genre_names_from_ids(item.get("genre_ids"));
    if !genres.is_empty() {
        meta.insert("genres".into(), json!(genres));
    }
    if let Some(value) = rating(item.get("vote_average").and_then(Value::as_f64)) {
        meta.insert("imdbRating".into(), json!(value));
    }
    Some(Value::Object(meta))
}

pub fn tvmaze_show(item: &Value) -> Option<&Value> {
    item.get("show")
        .or_else(|| item.get("_embedded").and_then(|value| value.get("show")))
        .or_else(|| (!item.get("name").is_none()).then_some(item))
}

pub fn tvmaze_imdb(show: &Value) -> Option<String> {
    show.get("externals")
        .and_then(|value| value.get("imdb"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub fn tvmaze_preview(item: &Value) -> Option<Value> {
    let show = tvmaze_show(item)?;
    let id = show.get("id").and_then(Value::as_u64)?;
    let name = show
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let imdb = tvmaze_imdb(show);
    let preview_id = imdb.clone().unwrap_or_else(|| format!("tvmaze:{id}"));
    let mut meta = Map::new();
    meta.insert("id".into(), json!(preview_id));
    meta.insert("type".into(), json!("series"));
    meta.insert("name".into(), json!(name));
    if let Some(poster) = show
        .get("image")
        .and_then(|value| value.get("medium").or_else(|| value.get("original")))
        .and_then(Value::as_str)
    {
        meta.insert("poster".into(), json!(poster));
    }
    if let Some(premiered) = year(show.get("premiered").and_then(Value::as_str)) {
        meta.insert("releaseInfo".into(), json!(premiered));
    }
    if let Some(summary) = show.get("summary").and_then(Value::as_str) {
        let summary = strip_html(summary);
        if !summary.is_empty() {
            meta.insert("description".into(), json!(truncate(&summary, 240)));
        }
    }
    let genres = tvmaze_genres(show).unwrap_or_default();
    if !genres.is_empty() {
        meta.insert("genres".into(), json!(genres));
    }
    Some(Value::Object(meta))
}

/// TVmaze lists genres as plain strings.
fn tvmaze_genres(show: &Value) -> Option<Vec<String>> {
    let items = show.get("genres")?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

pub fn logo(details: &Value, language: &str) -> Option<String> {
    best_image(details, "logos", "original", language)
}

/// The `results` entry for `country` in a TMDB rating list, else the US one.
fn country_entry<'a>(details: &'a Value, list: &str, country: &str) -> Option<&'a Value> {
    let results = details.get(list)?.get("results")?.as_array()?;
    let entry = |code: &str| {
        results
            .iter()
            .find(|item| item.get("iso_3166_1").and_then(Value::as_str) == Some(code))
    };
    entry(country).or_else(|| entry("US"))
}

pub fn certification(details: &Value, kind: Kind, country: &str) -> Option<String> {
    match kind {
        Kind::Movie => country_entry(details, "release_dates", country)?
            .get("release_dates")?
            .as_array()?
            .iter()
            .find_map(|item| item.get("certification").and_then(Value::as_str))
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        Kind::Series => country_entry(details, "content_ratings", country)?
            .get("rating")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    }
}

pub fn trailers(details: &Value) -> Vec<Value> {
    details
        .get("videos")
        .and_then(|value| value.get("results"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("site").and_then(Value::as_str) == Some("YouTube"))
                .filter(|item| {
                    matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("Trailer") | Some("Teaser")
                    )
                })
                .take(2)
                .filter_map(|item| {
                    Some(json!({
                        "source": item.get("key")?.as_str()?,
                        "type": "Trailer"
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn credits(details: &Value, kind: Kind) -> (Vec<String>, Vec<String>, Vec<String>) {
    let cast = details
        .get("credits")
        .and_then(|value| value.get("cast"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(10)
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let crew = details
        .get("credits")
        .and_then(|value| value.get("crew"))
        .and_then(Value::as_array);
    let director = crew
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("job").and_then(Value::as_str) == Some("Director"))
                .take(3)
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let created_by = details
        .get("created_by")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let writer = if kind == Kind::Series && !created_by.is_empty() {
        created_by
    } else {
        crew.map(|items| {
            items
                .iter()
                .filter(|item| {
                    matches!(
                        item.get("job").and_then(Value::as_str),
                        Some("Writer") | Some("Screenplay") | Some("Story")
                    )
                })
                .take(3)
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    };
    (cast, director, writer)
}

/// Tagline, first production country and first spoken language: shared by
/// movie and series metadata.
fn insert_production_details(meta: &mut Map<String, Value>, details: &Value) {
    let first = |list: &str, field: &str| {
        details
            .get(list)
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get(field))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if let Some(value) = details
        .get("tagline")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        meta.insert("tagline".into(), json!(value));
    }
    if let Some(value) = first("production_countries", "name") {
        meta.insert("country".into(), json!(value));
    }
    if let Some(value) = first("spoken_languages", "english_name") {
        meta.insert("language".into(), json!(value));
    }
}

pub fn movie_meta(requested: &str, details: &Value, language: &str, country: &str) -> Value {
    let imdb = details
        .get("external_ids")
        .and_then(|value| value.get("imdb_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let id = canonical(imdb, requested);
    let title = details
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| details.get("original_title").and_then(Value::as_str))
        .unwrap_or("Untitled");
    let mut meta = Map::new();
    meta.insert("id".into(), json!(id));
    meta.insert("type".into(), json!("movie"));
    meta.insert("name".into(), json!(title));
    let poster = best_image(details, "posters", "w780", language)
        .or_else(|| tmdb_image(details.get("poster_path").and_then(Value::as_str), "w500"));
    if let Some(value) = poster {
        meta.insert("poster".into(), json!(value));
    }
    let background = backdrop_image(details, "original", language).or_else(|| {
        tmdb_image(
            details.get("backdrop_path").and_then(Value::as_str),
            "w1280",
        )
    });
    if let Some(value) = background {
        meta.insert("background".into(), json!(value));
    }
    if let Some(value) = logo(details, language) {
        meta.insert("logo".into(), json!(value));
    }
    if let Some(value) = details
        .get("overview")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        meta.insert("description".into(), json!(truncate(value, 2000)));
    }
    if let Some(value) = year(details.get("release_date").and_then(Value::as_str)) {
        meta.insert("releaseInfo".into(), json!(value));
        meta.insert("year".into(), json!(value));
    }
    if let Some(value) = iso_date(details.get("release_date").and_then(Value::as_str)) {
        meta.insert("released".into(), json!(value));
    }
    if let Some(runtime) = details
        .get("runtime")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
    {
        meta.insert("runtime".into(), json!(format!("{runtime} min")));
    }
    let genres = genre_names(details);
    if !genres.is_empty() {
        meta.insert("genres".into(), json!(genres.clone()));
        meta.insert("genre".into(), json!(genres));
    }
    let (cast, director, writer) = credits(details, Kind::Movie);
    if !cast.is_empty() {
        meta.insert("cast".into(), json!(cast));
    }
    if !director.is_empty() {
        meta.insert("director".into(), json!(director));
    }
    if !writer.is_empty() {
        meta.insert("writer".into(), json!(writer));
    }
    if let Some(value) = rating(details.get("vote_average").and_then(Value::as_f64)) {
        meta.insert("imdbRating".into(), json!(value));
    }
    if let Some(value) = imdb {
        meta.insert("imdb_id".into(), json!(value));
    }
    if let Some(value) = details.get("id").and_then(Value::as_u64) {
        meta.insert("moviedb_id".into(), json!(value));
    }
    if let Some(value) = details
        .get("external_ids")
        .and_then(|value| value.get("tvdb_id"))
        .and_then(Value::as_u64)
    {
        meta.insert("tvdb_id".into(), json!(value));
    }
    if let Some(value) = details.get("status").and_then(Value::as_str) {
        meta.insert("status".into(), json!(value));
    }
    insert_production_details(&mut meta, details);
    if let Some(value) = certification(details, Kind::Movie, country) {
        meta.insert("ageRating".into(), json!(value));
    }
    let trailers = trailers(details);
    if !trailers.is_empty() {
        meta.insert("trailers".into(), json!(trailers));
    }
    if let Some(imdb) = imdb {
        meta.insert(
            "links".into(),
            json!([{ "name": "IMDb", "category": "imdb", "url": format!("https://imdb.com/title/{imdb}") }]),
        );
    }
    meta.insert(
        "behaviorHints".into(),
        json!({"defaultVideoId": id, "hasScheduledVideos": false}),
    );
    Value::Object(meta)
}

pub fn series_meta(
    requested: &str,
    tmdb: Option<&Value>,
    tvmaze: Option<&Value>,
    videos: Vec<Value>,
    language: &str,
    country: &str,
) -> Value {
    let tmdb_imdb = tmdb
        .and_then(|value| value.get("external_ids"))
        .and_then(|value| value.get("imdb_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let tvmaze_imdb = tvmaze.and_then(tvmaze_imdb);
    let id = canonical(tmdb_imdb.or(tvmaze_imdb.as_deref()), requested);
    let name = tmdb
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            tvmaze
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("Untitled");
    let mut meta = Map::new();
    meta.insert("id".into(), json!(id));
    meta.insert("type".into(), json!("series"));
    meta.insert("name".into(), json!(name));
    let poster = tmdb
        .and_then(|value| best_image(value, "posters", "w780", language))
        .or_else(|| {
            tmdb.and_then(|value| {
                tmdb_image(value.get("poster_path").and_then(Value::as_str), "w500")
            })
        })
        .or_else(|| {
            tvmaze
                .and_then(|value| value.get("image"))
                .and_then(|value| value.get("original").or_else(|| value.get("medium")))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    if let Some(value) = poster {
        meta.insert("poster".into(), json!(value));
    }
    let background = tmdb
        .and_then(|value| backdrop_image(value, "original", language))
        .or_else(|| {
            tmdb.and_then(|value| {
                tmdb_image(value.get("backdrop_path").and_then(Value::as_str), "w1280")
            })
        });
    if let Some(value) = background {
        meta.insert("background".into(), json!(value));
    }
    if let Some(value) = tmdb.and_then(|value| logo(value, language)) {
        meta.insert("logo".into(), json!(value));
    }
    let description = tmdb
        .and_then(|value| value.get("overview").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(|value| truncate(value, 2000))
        .or_else(|| {
            tvmaze
                .and_then(|value| value.get("summary").and_then(Value::as_str))
                .map(strip_html)
                .filter(|value| !value.is_empty())
                .map(|value| truncate(&value, 2000))
        });
    if let Some(value) = description {
        meta.insert("description".into(), json!(value));
    }
    let first = tmdb
        .and_then(|value| value.get("first_air_date").and_then(Value::as_str))
        .or_else(|| tvmaze.and_then(|value| value.get("premiered").and_then(Value::as_str)));
    let last = tmdb
        .and_then(|value| value.get("last_air_date").and_then(Value::as_str))
        .or_else(|| tvmaze.and_then(|value| value.get("ended").and_then(Value::as_str)));
    let status = tmdb
        .and_then(|value| value.get("status").and_then(Value::as_str))
        .or_else(|| tvmaze.and_then(|value| value.get("status").and_then(Value::as_str)));
    let release = series_release_info(first, last, status);
    if let Some(value) = release {
        meta.insert("releaseInfo".into(), json!(value));
    }
    if let Some(value) = year(first) {
        meta.insert("year".into(), json!(value));
    }
    if let Some(value) = iso_date(first) {
        meta.insert("released".into(), json!(value));
    }
    let runtime = tmdb
        .and_then(|value| value.get("episode_run_time").and_then(Value::as_array))
        .and_then(|items| items.first().and_then(Value::as_u64))
        .or_else(|| {
            tvmaze
                .and_then(|value| value.get("averageRuntime").or_else(|| value.get("runtime")))
                .and_then(Value::as_u64)
        });
    if let Some(runtime) = runtime.filter(|value| *value > 0) {
        meta.insert("runtime".into(), json!(format!("{runtime} min")));
    }
    let genres = tmdb
        .map(genre_names)
        .filter(|items| !items.is_empty())
        .or_else(|| tvmaze.and_then(tvmaze_genres))
        .unwrap_or_default();
    if !genres.is_empty() {
        meta.insert("genres".into(), json!(genres.clone()));
        meta.insert("genre".into(), json!(genres));
    }
    if let Some(details) = tmdb {
        let (cast, _, writer) = credits(details, Kind::Series);
        if !cast.is_empty() {
            meta.insert("cast".into(), json!(cast));
        }
        if !writer.is_empty() {
            meta.insert("writer".into(), json!(writer));
        }
        if let Some(value) = rating(details.get("vote_average").and_then(Value::as_f64)) {
            meta.insert("imdbRating".into(), json!(value));
        }
        if let Some(value) = certification(details, Kind::Series, country) {
            meta.insert("ageRating".into(), json!(value));
        }
        insert_production_details(&mut meta, details);
        let trailers = trailers(details);
        if !trailers.is_empty() {
            meta.insert("trailers".into(), json!(trailers));
        }
        if let Some(value) = details
            .get("external_ids")
            .and_then(|value| value.get("tvdb_id"))
            .and_then(Value::as_u64)
        {
            meta.insert("tvdb_id".into(), json!(value));
        }
        if let Some(value) = details.get("id").and_then(Value::as_u64) {
            meta.insert("moviedb_id".into(), json!(value));
        }
    } else if let Some(show) = tvmaze {
        if let Some(value) = show
            .get("network")
            .and_then(|value| value.get("country"))
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
        {
            meta.insert("country".into(), json!(value));
        }
        if let Some(value) = show.get("language").and_then(Value::as_str) {
            meta.insert("language".into(), json!(value));
        }
        if let Some(value) = show
            .get("externals")
            .and_then(|value| value.get("thetvdb"))
            .and_then(Value::as_u64)
        {
            meta.insert("tvdb_id".into(), json!(value));
        }
    }
    if let Some(value) = tmdb_imdb.or(tvmaze_imdb.as_deref()) {
        meta.insert("imdb_id".into(), json!(value));
        meta.insert(
            "links".into(),
            json!([{ "name": "IMDb", "category": "imdb", "url": format!("https://imdb.com/title/{value}") }]),
        );
    }
    meta.insert("videos".into(), json!(videos));
    // No defaultVideoId: upstream AIOMetadata and Cinemeta leave it unset for
    // series, so clients pick the first/next episode instead of the newest one.
    meta.insert("behaviorHints".into(), json!({"hasScheduledVideos": false}));
    Value::Object(meta)
}

pub fn series_release_info(
    first: Option<&str>,
    last: Option<&str>,
    status: Option<&str>,
) -> Option<String> {
    let first = year(first)?;
    let last = year(last);
    let ended = matches!(status, Some("Ended") | Some("Canceled") | Some("Cancelled"));
    match last {
        Some(last) if ended && last == first => Some(first),
        Some(last) if ended => Some(format!("{first}-{last}")),
        _ => Some(format!("{first}-")),
    }
}

/// One Stremio video entry; untitled episodes are named "Episode N".
fn episode_video(
    episode: &Value,
    base: &str,
    season: u64,
    number: u64,
    released: Option<String>,
    overview: Option<String>,
    thumbnail: Option<impl Into<Value>>,
) -> Value {
    let title = episode
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Episode {number}"));
    let mut video = Map::new();
    video.insert("id".into(), json!(format!("{base}:{season}:{number}")));
    video.insert("title".into(), json!(title));
    if let Some(released) = released {
        video.insert("released".into(), json!(released));
    }
    video.insert("season".into(), json!(season));
    video.insert("episode".into(), json!(number));
    if let Some(value) = overview {
        video.insert("overview".into(), json!(value));
    }
    if let Some(value) = thumbnail {
        video.insert("thumbnail".into(), value.into());
    }
    Value::Object(video)
}

pub fn video_from_tvmaze(
    episode: &Value,
    base: &str,
    fallback_released: Option<&str>,
) -> Option<Value> {
    let season = episode.get("season").and_then(Value::as_u64)?;
    let number = episode.get("number").and_then(Value::as_u64).unwrap_or(0);
    if number == 0 && episode.get("number").is_some_and(Value::is_null) {
        return None;
    }
    let released = episode
        .get("airstamp")
        .and_then(Value::as_str)
        .map(iso_utc)
        .or_else(|| iso_date(episode.get("airdate").and_then(Value::as_str)))
        .or_else(|| fallback_released.map(str::to_owned));
    let overview = episode
        .get("summary")
        .and_then(Value::as_str)
        .map(strip_html)
        .filter(|value| !value.is_empty())
        .map(|value| truncate(&value, 400));
    let thumbnail = episode
        .get("image")
        .and_then(|value| value.get("medium").or_else(|| value.get("original")))
        .and_then(Value::as_str);
    Some(episode_video(
        episode, base, season, number, released, overview, thumbnail,
    ))
}

pub fn video_from_tmdb(
    episode: &Value,
    base: &str,
    fallback_released: Option<&str>,
) -> Option<Value> {
    let season = episode.get("season_number").and_then(Value::as_u64)?;
    let number = episode.get("episode_number").and_then(Value::as_u64)?;
    let released = iso_date(episode.get("air_date").and_then(Value::as_str))
        .or_else(|| fallback_released.map(str::to_owned));
    let overview = episode
        .get("overview")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| truncate(value, 400));
    let thumbnail = tmdb_image(episode.get("still_path").and_then(Value::as_str), "w500");
    Some(episode_video(
        episode, base, season, number, released, overview, thumbnail,
    ))
}

pub fn tvmaze_videos(show: &Value, base: &str) -> Vec<Value> {
    let fallback = iso_date(show.get("premiered").and_then(Value::as_str));
    let mut videos = show
        .get("_embedded")
        .and_then(|value| value.get("episodes"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|episode| video_from_tvmaze(episode, base, fallback.as_deref()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    dedupe_videos(&mut videos);
    videos
}

pub fn tmdb_videos(episodes: &[Value], base: &str, fallback: Option<&str>) -> Vec<Value> {
    let mut videos = episodes
        .iter()
        .filter_map(|episode| video_from_tmdb(episode, base, fallback))
        .collect::<Vec<_>>();
    dedupe_videos(&mut videos);
    videos
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_textless_backdrop_and_original_art() {
        let details = json!({
            "images": {
                "backdrops": [
                    {"file_path": "/en.jpg", "iso_639_1": "en", "vote_average": 9.0},
                    {"file_path": "/xx.jpg", "iso_639_1": "xx", "vote_average": 1.0},
                    {"file_path": "/null.jpg", "iso_639_1": null}
                ],
                "logos": [
                    {"file_path": "/logo.png", "iso_639_1": "en", "vote_average": 2.0}
                ]
            }
        });
        assert_eq!(
            backdrop_image(&details, "original", "en-US").as_deref(),
            Some("https://image.tmdb.org/t/p/original/xx.jpg")
        );
        assert_eq!(
            logo(&details, "en-US").as_deref(),
            Some("https://image.tmdb.org/t/p/original/logo.png")
        );
    }

    #[test]
    fn catalog_posters_use_high_quality_size() {
        let item = json!({"id": 1, "title": "Example", "poster_path": "/p.jpg"});
        let preview = tmdb_preview(&item, Kind::Movie).expect("preview");
        assert_eq!(
            preview["poster"].as_str(),
            Some("https://image.tmdb.org/t/p/w600_and_h900_bestv2/p.jpg")
        );
    }
    #[test]
    fn ended_single_year_series_reports_one_year() {
        assert_eq!(
            series_release_info(Some("2019-05-06"), Some("2019-06-04"), Some("Ended")).as_deref(),
            Some("2019")
        );
        assert_eq!(
            series_release_info(Some("2011-04-17"), Some("2019-05-19"), Some("Ended")).as_deref(),
            Some("2011-2019")
        );
        assert_eq!(
            series_release_info(Some("2022-08-21"), Some("2022-10-23"), Some("Running")).as_deref(),
            Some("2022-")
        );
    }

    #[test]
    fn videos_omit_unknown_release_and_normalize_utc() {
        let no_date = video_from_tvmaze(
            &json!({"season": 1, "number": 3, "name": "Pilot"}),
            "tt1",
            None,
        )
        .expect("video");
        assert!(no_date.get("released").is_none());
        let dated = video_from_tvmaze(
            &json!({"season": 1, "number": 3, "name": "Pilot", "airstamp": "2019-05-07T01:00:00+00:00"}),
            "tt1",
            None,
        )
        .expect("video");
        assert_eq!(dated["released"].as_str(), Some("2019-05-07T01:00:00Z"));
        let tmdb = video_from_tmdb(
            &json!({"season_number": 1, "episode_number": 3, "name": "Pilot"}),
            "tt1",
            None,
        )
        .expect("video");
        assert!(tmdb.get("released").is_none());
    }

    #[test]
    fn teasers_are_sent_as_trailer_type() {
        let details = json!({"videos": {"results": [
            {"site": "YouTube", "type": "Teaser", "key": "abc"},
            {"site": "YouTube", "type": "Clip", "key": "def"}
        ]}});
        assert_eq!(
            trailers(&details),
            vec![json!({"source": "abc", "type": "Trailer"})]
        );
    }

    #[test]
    fn series_hints_have_no_default_video() {
        let meta = series_meta("tmdb:1", None, None, Vec::new(), "en-US", "US");
        assert!(meta["behaviorHints"].get("defaultVideoId").is_none());
        assert!(meta["videos"].as_array().is_some());
    }
}
