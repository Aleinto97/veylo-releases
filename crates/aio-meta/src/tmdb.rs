use crate::ids::Kind;
use crate::util::send_with_retry;
use anyhow::{anyhow, Context, Result};
use reqwest::{header, Client, StatusCode};
use serde_json::Value;

#[derive(Clone)]
pub struct Tmdb {
    client: Client,
    key: String,
    pub language: String,
}

impl Tmdb {
    pub fn new(client: Client, key: String, language: String) -> Self {
        Self {
            client,
            key,
            language,
        }
    }

    fn bearer(&self) -> bool {
        self.key.starts_with("eyJ")
    }

    async fn get(&self, path: &str, params: Vec<(&str, String)>) -> Result<Value> {
        let url = format!("https://api.themoviedb.org/3{path}");
        let response = send_with_retry("TMDB", || {
            let request = self.client.get(&url);
            let request = if self.bearer() {
                request.header(header::AUTHORIZATION, format!("Bearer {}", self.key))
            } else {
                request.query(&[("api_key", self.key.as_str())])
            };
            request.query(&params)
        })
        .await?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(anyhow!("TMDB id not found"));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(anyhow!("TMDB API key rejected (HTTP {})", status.as_u16()));
        }
        if !status.is_success() {
            return Err(anyhow!("TMDB returned HTTP {}", status.as_u16()));
        }
        response
            .json::<Value>()
            .await
            .context("TMDB returned malformed JSON")
    }

    pub async fn find(&self, kind: Kind, source: &str, value: &str) -> Result<Option<u64>> {
        let value = value.replace(':', "");
        let data = self
            .get(
                &format!("/find/{value}"),
                vec![
                    ("external_source", source.to_owned()),
                    ("language", self.language.clone()),
                ],
            )
            .await?;
        let results = match kind {
            Kind::Movie => data.get("movie_results"),
            Kind::Series => data.get("tv_results"),
        };
        Ok(results
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("id"))
            .and_then(Value::as_u64))
    }

    /// Full details with credits, videos, ratings (`ratings` list), images and keywords.
    async fn details(&self, path: &str, ratings: &str) -> Result<Value> {
        let language = self.language.split('-').next().unwrap_or("en");
        self.get(
            path,
            vec![
                ("language", self.language.clone()),
                (
                    "append_to_response",
                    format!("external_ids,credits,videos,{ratings},images,keywords"),
                ),
                ("include_image_language", format!("{language},en,null")),
            ],
        )
        .await
    }

    pub async fn movie_details(&self, id: u64) -> Result<Value> {
        self.details(&format!("/movie/{id}"), "release_dates").await
    }

    pub async fn tv_details(&self, id: u64) -> Result<Value> {
        self.details(&format!("/tv/{id}"), "content_ratings").await
    }

    pub async fn season(&self, tv_id: u64, season: u64) -> Result<Value> {
        self.get(
            &format!("/tv/{tv_id}/season/{season}"),
            vec![("language", self.language.clone())],
        )
        .await
    }

    pub async fn search(&self, kind: Kind, query: &str, page: u32) -> Result<Vec<Value>> {
        let path = match kind {
            Kind::Movie => "/search/movie",
            Kind::Series => "/search/tv",
        };
        let data = self
            .get(
                path,
                vec![
                    ("query", query.to_owned()),
                    ("language", self.language.clone()),
                    ("page", page.to_string()),
                    ("include_adult", "false".to_owned()),
                ],
            )
            .await?;
        Ok(data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn list(&self, kind: Kind, catalog: &str, page: u32) -> Result<Vec<Value>> {
        let (path, mut params) = match (kind, catalog) {
            (Kind::Movie, "tmdb.top") => ("/movie/popular".to_owned(), vec![]),
            (Kind::Movie, "tmdb.trending") => ("/trending/movie/week".to_owned(), vec![]),
            (Kind::Movie, "tmdb.top_rated") => ("/movie/top_rated".to_owned(), vec![]),
            (Kind::Movie, "tmdb.anime") => (
                "/discover/movie".to_owned(),
                vec![
                    ("with_genres", "16".to_owned()),
                    ("with_original_language", "ja".to_owned()),
                    ("sort_by", "popularity.desc".to_owned()),
                    ("vote_count.gte", "10".to_owned()),
                ],
            ),
            (Kind::Series, "tmdb.top") => ("/tv/popular".to_owned(), vec![]),
            (Kind::Series, "tmdb.trending") => ("/trending/tv/week".to_owned(), vec![]),
            (Kind::Series, "tmdb.top_rated") => ("/tv/top_rated".to_owned(), vec![]),
            (Kind::Series, "tmdb.airing_today") => ("/tv/airing_today".to_owned(), vec![]),
            (Kind::Series, "tmdb.anime") => (
                "/discover/tv".to_owned(),
                vec![
                    ("with_genres", "16".to_owned()),
                    ("with_original_language", "ja".to_owned()),
                    ("sort_by", "popularity.desc".to_owned()),
                    ("vote_count.gte", "10".to_owned()),
                ],
            ),
            _ => return Err(anyhow!("unknown TMDB catalog")),
        };
        params.push(("language", self.language.clone()));
        params.push(("page", page.to_string()));
        params.push(("include_adult", "false".to_owned()));
        let data = self.get(&path, params).await?;
        Ok(data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}
