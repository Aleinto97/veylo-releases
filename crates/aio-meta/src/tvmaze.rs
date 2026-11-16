use crate::util::send_with_retry;
use anyhow::{anyhow, Context, Result};
use reqwest::{Client, StatusCode};
use serde_json::Value;

#[derive(Clone)]
pub struct TvMaze {
    client: Client,
    pub country: String,
}

impl TvMaze {
    pub fn new(client: Client, country: String) -> Self {
        Self { client, country }
    }

    async fn get(&self, path: &str, params: Vec<(&str, String)>) -> Result<Option<Value>> {
        let url = format!("https://api.tvmaze.com{path}");
        let response = send_with_retry("TVmaze", || self.client.get(&url).query(&params)).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(anyhow!(
                "TVmaze returned HTTP {}",
                response.status().as_u16()
            ));
        }
        Ok(Some(
            response
                .json::<Value>()
                .await
                .context("TVmaze returned malformed JSON")?,
        ))
    }

    pub async fn lookup_imdb(&self, imdb: &str) -> Result<Option<Value>> {
        self.get("/lookup/shows", vec![("imdb", imdb.to_owned())])
            .await
    }

    pub async fn lookup_tvdb(&self, tvdb: u64) -> Result<Option<Value>> {
        self.get("/lookup/shows", vec![("thetvdb", tvdb.to_string())])
            .await
    }

    pub async fn show_episodes(&self, id: u64) -> Result<Option<Value>> {
        self.get(
            &format!("/shows/{id}"),
            vec![("embed", "episodes".to_owned())],
        )
        .await
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Value>> {
        let Some(data) = self
            .get("/search/shows", vec![("q", query.to_owned())])
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(data.as_array().cloned().unwrap_or_default())
    }

    pub async fn schedule(&self) -> Result<Vec<Value>> {
        let Some(data) = self
            .get("/schedule", vec![("country", self.country.clone())])
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(data.as_array().cloned().unwrap_or_default())
    }
}
