use crate::config::Config;
use crate::ids::{canonical, Id, Kind};
use crate::image;
use crate::model::{movie_meta, series_meta, tmdb_videos, tvmaze_imdb, tvmaze_videos};
use crate::state::State;
use crate::tmdb::Tmdb;
use crate::tvmaze::TvMaze;
use crate::util::iso_date;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::time::Duration;

pub async fn meta(state: &State, kind: Kind, raw_id: &str) -> Result<(Value, &'static str)> {
    let id = Id::parse(raw_id).ok_or_else(|| anyhow!("invalid meta id"))?;
    let config = state.config();
    let cache_key = format!("meta:{}:{}:{}", kind.as_str(), id.base(), config.language);
    if let Some(value) = state.cache.get_fresh(&cache_key) {
        return Ok((value, "hit"));
    }
    let ttl = match kind {
        Kind::Movie => Duration::from_secs(6 * 60 * 60),
        Kind::Series => Duration::from_secs(2 * 60 * 60),
    };
    match fetch(&state.api, &config, kind, &id).await {
        Ok(mut value) => {
            image::rewrite_images(&mut value, &state.base);
            state.cache.put(cache_key, value.clone(), ttl);
            Ok((value, "miss"))
        }
        Err(error) => {
            if let Some(stale) = state.cache.get_stale(&cache_key, crate::cache::MAX_STALE) {
                Ok((stale, "stale"))
            } else {
                Err(error)
            }
        }
    }
}

async fn fetch(api: &reqwest::Client, config: &Config, kind: Kind, id: &Id) -> Result<Value> {
    let tmdb = config.has_tmdb().then(|| {
        Tmdb::new(
            api.clone(),
            config.tmdb_key.clone(),
            config.language.clone(),
        )
    });
    let tvmaze = TvMaze::new(api.clone(), config.country.clone());

    if kind == Kind::Movie {
        let tmdb_id = match id {
            Id::Tmdb(value) => Some(*value),
            Id::Imdb(value) => match &tmdb {
                Some(tmdb) => tmdb.find(Kind::Movie, "imdb_id", value).await?,
                None => None,
            },
            Id::Tvdb(value) => match &tmdb {
                Some(tmdb) => {
                    tmdb.find(Kind::Movie, "tvdb_id", &value.to_string())
                        .await?
                }
                None => None,
            },
            Id::Tvmaze(_) => None,
        };
        let Some(tmdb) = tmdb else {
            return Err(anyhow!("TMDB API key is not configured"));
        };
        let Some(tmdb_id) = tmdb_id else {
            return Err(anyhow!("metadata not found"));
        };
        let details = tmdb.movie_details(tmdb_id).await.map_err(|error| {
            if format!("{error}").contains("not found") {
                anyhow!("metadata not found")
            } else {
                error
            }
        })?;
        return Ok(movie_meta(
            &id.base(),
            &details,
            &config.language,
            &config.country,
        ));
    }

    let tmdb_id = match id {
        Id::Tmdb(value) => Some(*value),
        Id::Imdb(value) => match &tmdb {
            Some(tmdb) => tmdb
                .find(Kind::Series, "imdb_id", value)
                .await
                .ok()
                .flatten(),
            None => None,
        },
        Id::Tvdb(value) => match &tmdb {
            Some(tmdb) => tmdb
                .find(Kind::Series, "tvdb_id", &value.to_string())
                .await
                .ok()
                .flatten(),
            None => None,
        },
        Id::Tvmaze(_) => None,
    };
    let mut tmdb_details = None;
    let mut tmdb_error = None;
    if let Some(value) = tmdb_id {
        if let Some(tmdb) = &tmdb {
            match tmdb.tv_details(value).await {
                Ok(details) => tmdb_details = Some(details),
                Err(error) => tmdb_error = Some(error),
            }
        }
    }

    let tmdb_tvdb = tmdb_details
        .as_ref()
        .and_then(|value| value.get("external_ids"))
        .and_then(|value| value.get("tvdb_id"))
        .and_then(Value::as_u64);
    let tmdb_imdb = tmdb_details
        .as_ref()
        .and_then(|value| value.get("external_ids"))
        .and_then(|value| value.get("imdb_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    // TVMaze is an enrichment source here: a timeout must not fail a series
    // that TMDB can already describe. Keep the first error for the 502 path.
    let mut tvmaze_error = None;
    let show = match id {
        Id::Tvmaze(value) => ignore_error(tvmaze.show_episodes(*value).await, &mut tvmaze_error)
            .await
            .flatten(),
        _ => {
            let mut show = None;
            if let Some(tvdb) = tmdb_tvdb {
                show = ignore_error(tvmaze.lookup_tvdb(tvdb).await, &mut tvmaze_error)
                    .await
                    .flatten();
            }
            if show.is_none() {
                if let Some(imdb) = tmdb_imdb.as_deref().or(match id {
                    Id::Imdb(value) => Some(value.as_str()),
                    _ => None,
                }) {
                    show = ignore_error(tvmaze.lookup_imdb(imdb).await, &mut tvmaze_error)
                        .await
                        .flatten();
                }
            }
            if show.is_none() {
                if let Id::Tvdb(value) = id {
                    show = ignore_error(tvmaze.lookup_tvdb(*value).await, &mut tvmaze_error)
                        .await
                        .flatten();
                }
            }
            if let Some(show) = show {
                let show_id = show.get("id").and_then(Value::as_u64);
                match show_id {
                    Some(show_id) => {
                        ignore_error(tvmaze.show_episodes(show_id).await, &mut tvmaze_error)
                            .await
                            .flatten()
                    }
                    None => Some(show),
                }
            } else {
                None
            }
        }
    };

    let show_imdb = show.as_ref().and_then(tvmaze_imdb);
    if tmdb_details.is_none() {
        if let (Some(tmdb), Some(imdb)) = (&tmdb, show_imdb.as_deref()) {
            if let Some(value) = tmdb
                .find(Kind::Series, "imdb_id", imdb)
                .await
                .ok()
                .flatten()
            {
                match tmdb.tv_details(value).await {
                    Ok(details) => tmdb_details = Some(details),
                    Err(error) => tmdb_error = Some(error),
                }
            }
        }
    }

    if tmdb_details.is_none() && show.is_none() {
        if let Some(error) = tmdb_error {
            return Err(error);
        }
        if let Some(error) = tvmaze_error {
            return Err(error);
        }
        return Err(anyhow!("metadata not found"));
    }

    let base = canonical(
        tmdb_details
            .as_ref()
            .and_then(|value| value.get("external_ids"))
            .and_then(|value| value.get("imdb_id"))
            .and_then(Value::as_str)
            .or(show_imdb.as_deref()),
        &id.base(),
    );

    let mut videos = show
        .as_ref()
        .map(|show| tvmaze_videos(show, &base))
        .unwrap_or_default();
    if videos.is_empty() {
        if let (Some(tmdb), Some(details)) = (&tmdb, tmdb_details.as_ref()) {
            videos = tmdb_series_videos(tmdb, details, &base).await;
        }
    }

    Ok(series_meta(
        &id.base(),
        tmdb_details.as_ref(),
        show.as_ref(),
        videos,
        &config.language,
        &config.country,
    ))
}

async fn tmdb_series_videos(tmdb: &Tmdb, details: &Value, base: &str) -> Vec<Value> {
    let fallback = iso_date(details.get("first_air_date").and_then(Value::as_str));
    let tv_id = details.get("id").and_then(Value::as_u64).unwrap_or(0);
    let seasons = details
        .get("seasons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut set = tokio::task::JoinSet::new();
    for season in seasons.iter().take(30) {
        let Some(number) = season.get("season_number").and_then(Value::as_u64) else {
            continue;
        };
        let tmdb = tmdb.clone();
        set.spawn(async move { tmdb.season(tv_id, number).await });
    }
    let mut episodes = Vec::new();
    while let Some(result) = set.join_next().await {
        if let Ok(Ok(value)) = result {
            if let Some(items) = value.get("episodes").and_then(Value::as_array) {
                episodes.extend(items.iter().cloned());
            }
        }
    }
    tmdb_videos(&episodes, base, fallback.as_deref())
}

/// Swallow an upstream error into `slot` so the other provider can still answer.
async fn ignore_error<T>(result: anyhow::Result<T>, slot: &mut Option<anyhow::Error>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            *slot = Some(error);
            None
        }
    }
}
