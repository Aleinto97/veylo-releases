use crate::config::Config;
use serde_json::{json, Value};

pub fn manifest(config: &Config) -> Value {
    let mut catalogs = Vec::new();
    if config.has_tmdb() {
        // The app shows the first three catalogs on Home: keep one movie,
        // one series and one fresh row visible, and put search catalogs last.
        catalogs.push(catalog("movie", "tmdb.top", "Popular", false));
        catalogs.push(catalog("series", "tmdb.top", "Popular", false));
        catalogs.push(catalog(
            "series",
            "tmdb.airing_today",
            "Airing Today",
            false,
        ));
        catalogs.push(catalog("movie", "tmdb.trending", "Trending", false));
        catalogs.push(catalog("movie", "tmdb.top_rated", "Top Rated", false));
        catalogs.push(catalog("series", "tmdb.anime", "Anime", false));
        catalogs.push(catalog("series", "tmdb.trending", "Trending", false));
        catalogs.push(catalog("series", "tmdb.top_rated", "Top Rated", false));
        catalogs.push(catalog(
            "series",
            "tvmaze.schedule",
            "TVMaze Daily Schedule",
            false,
        ));
        catalogs.push(catalog("movie", "search.movie", "Movies Search", true));
        catalogs.push(catalog("series", "search.series", "Series Search", true));
    } else {
        catalogs.push(catalog("series", "tvmaze.schedule", "Airing Today", false));
        catalogs.push(catalog("series", "search.series", "Series Search", true));
    }
    json!({
        "id": "com.aio.metadata.rs",
        "version": env!("CARGO_PKG_VERSION"),
        "name": "AIO Metadata (Rust)",
        "description": "Local Rust port of AIOMetadata. TMDB and TVMaze metadata for movies, series and anime. Metadata only; installs beside Cinemeta.",
        "types": ["movie", "series"],
        "idPrefixes": ["tt", "tmdb:", "tvdb:", "tvmaze:"],
        "resources": [
            "catalog",
            {"name": "meta", "types": ["movie", "series"], "idPrefixes": ["tt", "tmdb:", "tvdb:", "tvmaze:"]}
        ],
        "catalogs": catalogs,
        "behaviorHints": {"configurable": false}
    })
}

fn catalog(kind: &str, id: &str, name: &str, search: bool) -> Value {
    let extra = if search {
        json!([{"name": "search", "isRequired": true}, {"name": "skip"}])
    } else {
        json!([{"name": "skip"}])
    };
    json!({
        "type": kind,
        "id": id,
        "name": name,
        "pageSize": 20,
        "extra": extra
    })
}
