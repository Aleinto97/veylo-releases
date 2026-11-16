//! Local Rust port of the AIOMetadata core metadata addon.
//!
//! The addon is intentionally embedded in the Android app: it listens only on
//! loopback and speaks the Stremio addon protocol, so the existing addon client
//! can install it beside Cinemeta and Kitsu without changing the core.
mod cache;
mod catalog;
mod config;
mod http;
mod ids;
mod image;
mod manifest;
mod meta;
mod model;
mod state;
mod tmdb;
mod tvmaze;
mod util;

pub use config::Config;
use image::ImageCache;
use state::State;
use std::sync::{Arc, RwLock};
use tokio::net::TcpListener;

pub struct Handle {
    pub url: String,
    config: Arc<RwLock<Config>>,
    cache: cache::Cache,
    state: Arc<State>,
}

impl Handle {
    pub fn set_config(&self, config: Config) -> bool {
        let changed = match self.config.write() {
            Ok(mut current) => {
                let changed =
                    current.tmdb_key != config.tmdb_key || current.language != config.language;
                *current = config;
                changed
            }
            Err(_) => false,
        };
        if changed {
            self.cache.clear();
        }
        changed
    }

    pub fn config(&self) -> Config {
        self.state.config()
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

/// Binds loopback and serves the addon. Port 7099 is preferred so the installed
/// addon URL stays stable across app restarts; an ephemeral port is the fallback.
pub async fn start(config: Config) -> anyhow::Result<Handle> {
    let listener = match TcpListener::bind("127.0.0.1:7099").await {
        Ok(listener) => listener,
        Err(_) => TcpListener::bind("127.0.0.1:0").await?,
    };
    let address = listener.local_addr()?;
    let base = format!("http://{address}");
    let url = format!("{base}/manifest.json");
    let cache = cache::Cache::default();
    let config = Arc::new(RwLock::new(config));
    // One pooled client for every TMDB/TVmaze call; a fresh client per request
    // paid a new DNS/TLS handshake each time.
    let api = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("VeyloAIOMetadata/0.3 (local addon)")
        .build()?;
    // Artwork never needs redirects: the allowlist is checked on this URL only.
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("VeyloAIOMetadata/0.3 (image proxy)")
        .build()?;
    let state = Arc::new(State {
        config: Arc::clone(&config),
        cache: cache.clone(),
        base,
        images: ImageCache::default(),
        api,
        http,
    });
    let app = http::router(Arc::clone(&state));
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("[aio-meta] server stopped: {error}");
        }
    });
    Ok(Handle {
        url,
        config,
        cache,
        state,
    })
}

pub async fn serve_forever(config: Config) -> anyhow::Result<()> {
    let handle = start(config).await?;
    println!("AIO Metadata (Rust) listening at {}", handle.url);
    tokio::signal::ctrl_c().await?;
    Ok(())
}
