use crate::config::Config;
use crate::ids::Kind;
use crate::image;
use crate::model::{tmdb_preview, tvmaze_preview};
use crate::state::State;
use crate::tmdb::Tmdb;
use crate::tvmaze::TvMaze;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

pub async fn catalog(
    state: &State,
    kind: Kind,
    catalog_id: &str,
    extras: &HashMap<String, String>,
) -> Result<(Value, &'static str)> {
    let config = state.config();
    let skip = extras
        .get("skip")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(9_980);
    let query = extras.get("search").map(|value| value.trim().to_owned());
    if catalog_id.starts_with("search.") && query.as_deref().unwrap_or("").is_empty() {
        return Err(anyhow!("search query is required"));
    }
    if let Some(query) = query.as_deref() {
        if query.len() > 80 || query.chars().any(char::is_control) {
            return Err(anyhow!("invalid search query"));
        }
    }
    let cache_key = format!(
        "catalog:{}:{}:{}:{}:{}",
        kind.as_str(),
        catalog_id,
        skip,
        config.language,
        query.as_deref().unwrap_or("")
    );
    if let Some(value) = state.cache.get_fresh(&cache_key) {
        return Ok((value, "hit"));
    }
    match build(
        &state.api,
        &config,
        kind,
        catalog_id,
        skip,
        query.as_deref(),
    )
    .await
    {
        Ok(mut value) => {
            image::rewrite_images(&mut value, &state.base);
            state
                .cache
                .put(cache_key, value.clone(), Duration::from_secs(30 * 60));
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

async fn build(
    api: &reqwest::Client,
    config: &Config,
    kind: Kind,
    catalog_id: &str,
    skip: usize,
    query: Option<&str>,
) -> Result<Value> {
    let tvmaze = TvMaze::new(api.clone(), config.country.clone());
    let tmdb = config.has_tmdb().then(|| {
        Tmdb::new(
            api.clone(),
            config.tmdb_key.clone(),
            config.language.clone(),
        )
    });

    let metas = match (kind, catalog_id) {
        (Kind::Series, "tvmaze.schedule") => schedule_previews(&tvmaze, skip).await?,
        (Kind::Series, "tmdb.airing_today") => {
            let Some(tmdb) = &tmdb else {
                return Err(anyhow!("TMDB API key is not configured"));
            };
            tmdb_metas(tmdb, kind, catalog_id, skip).await?
        }
        (_, "search.movie") | (_, "search.series") => {
            let query = query.ok_or_else(|| anyhow!("search query is required"))?;
            let expected = match catalog_id {
                "search.movie" => Kind::Movie,
                _ => Kind::Series,
            };
            if kind != expected {
                return Err(anyhow!("metadata not found"));
            }
            match (&tmdb, kind) {
                (Some(tmdb), _) => {
                    let results = tmdb.search(kind, query, page(skip)).await?;
                    results
                        .iter()
                        .skip(offset(skip))
                        .filter_map(|item| tmdb_preview(item, kind))
                        .collect::<Vec<_>>()
                }
                (None, Kind::Series) => tvmaze_search_previews(&tvmaze, query, skip).await?,
                (None, Kind::Movie) => return Err(anyhow!("TMDB API key is not configured")),
            }
        }
        (_, "tmdb.anime") | (_, "tmdb.top") | (_, "tmdb.trending") | (_, "tmdb.top_rated") => {
            let Some(tmdb) = &tmdb else {
                return Err(anyhow!("TMDB API key is not configured"));
            };
            tmdb_metas(tmdb, kind, catalog_id, skip).await?
        }
        _ => return Err(anyhow!("metadata not found")),
    };

    Ok(json!({"metas": metas}))
}

const PAGE_SIZE: usize = 20;

fn page(skip: usize) -> u32 {
    (skip / PAGE_SIZE + 1) as u32
}

fn offset(skip: usize) -> usize {
    skip % PAGE_SIZE
}

async fn tmdb_metas(tmdb: &Tmdb, kind: Kind, catalog_id: &str, skip: usize) -> Result<Vec<Value>> {
    let results = tmdb.list(kind, catalog_id, page(skip)).await?;
    Ok(results
        .iter()
        .skip(offset(skip))
        .filter_map(|item| tmdb_preview(item, kind))
        .collect())
}

async fn schedule_previews(tvmaze: &TvMaze, skip: usize) -> Result<Vec<Value>> {
    let entries = tvmaze.schedule().await?;
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for entry in entries {
        let Some(show) = entry
            .get("_embedded")
            .and_then(|value| value.get("show"))
            .or_else(|| entry.get("show"))
        else {
            continue;
        };
        if !schedule_show_allowed(show) {
            continue;
        }
        let Some(id) = show.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        if let Some(preview) = tvmaze_preview(show) {
            let air = entry
                .get("airstamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            items.push((air, preview));
        }
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(items
        .into_iter()
        .skip(skip)
        .take(PAGE_SIZE)
        .map(|(_, preview)| preview)
        .collect())
}

/// Upstream AIOMetadata drops news and talk shows from the daily schedule row.
fn schedule_show_allowed(show: &Value) -> bool {
    !matches!(
        show.get("type")
            .and_then(Value::as_str)
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("news") | Some("talk show")
    )
}

async fn tvmaze_search_previews(tvmaze: &TvMaze, query: &str, skip: usize) -> Result<Vec<Value>> {
    let results = tvmaze.search(query).await?;
    let mut seen = std::collections::HashSet::new();
    let mut items = results
        .iter()
        .filter_map(|entry| {
            let score = entry.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            let preview = tvmaze_preview(entry)?;
            let id = preview.get("id").and_then(Value::as_str)?.to_owned();
            seen.insert(id.clone()).then_some((score, preview))
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(items
        .into_iter()
        .skip(skip)
        .take(PAGE_SIZE)
        .map(|(_, preview)| preview)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schedule_drops_news_and_talk_but_keeps_scripted() {
        assert!(!schedule_show_allowed(&json!({"type": "News"})));
        assert!(!schedule_show_allowed(&json!({"type": "Talk Show"})));
        assert!(schedule_show_allowed(&json!({"type": "Scripted"})));
        assert!(schedule_show_allowed(&json!({})));
    }
}
