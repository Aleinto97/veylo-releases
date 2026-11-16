use crate::cache::Cache;
use crate::config::Config;
use crate::image::ImageCache;
use std::sync::{Arc, RwLock};

pub(crate) struct State {
    pub config: Arc<RwLock<Config>>,
    pub cache: Cache,
    pub base: String,
    pub images: ImageCache,
    /// Connection-pooled client shared by every upstream API call.
    pub api: reqwest::Client,
    /// Image downloads: longer timeout, no redirects (URL is allowlisted once).
    pub http: reqwest::Client,
}

impl State {
    pub fn config(&self) -> Config {
        self.config
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
}
