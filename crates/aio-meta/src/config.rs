use serde_json::Value;
use std::path::Path;

/// Runtime configuration for the local addon. The defaults keep the metadata
/// language English, matching the rest of the app.
#[derive(Clone, Debug)]
pub struct Config {
    pub tmdb_key: String,
    pub language: String,
    pub country: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tmdb_key: String::new(),
            language: "en-US".to_owned(),
            country: "US".to_owned(),
        }
    }
}

impl Config {
    /// Environment first, then an app-private tmdb.key file next to the core DB.
    pub fn from_env_and_dir(dir: Option<&Path>) -> Self {
        let mut config = Self::default();
        if let Ok(key) =
            std::env::var("VEYLO_META_TMDB_KEY").or_else(|_| std::env::var("TMDB_API_KEY"))
        {
            config.tmdb_key = sanitize_key(&key);
        }
        if config.tmdb_key.is_empty() {
            if let Some(dir) = dir {
                if let Ok(key) = std::fs::read_to_string(dir.join("tmdb.key")) {
                    config.tmdb_key = sanitize_key(&key);
                }
            }
        }
        if let Ok(language) = std::env::var("VEYLO_META_LANGUAGE") {
            if valid_language(&language) {
                config.language = language;
            }
        }
        if let Ok(country) = std::env::var("VEYLO_META_COUNTRY") {
            let country = country.trim().to_ascii_uppercase();
            if country.len() == 2 && country.bytes().all(|b| b.is_ascii_alphabetic()) {
                config.country = country;
            }
        }
        config
    }

    /// Applies a small JSON update from the Android settings screen.
    pub fn apply_json(&mut self, value: &Value) -> bool {
        let before = (self.tmdb_key.clone(), self.language.clone());
        if let Some(key) = value.get("tmdb_key").and_then(Value::as_str) {
            self.tmdb_key = sanitize_key(key);
        }
        if let Some(language) = value.get("language").and_then(Value::as_str) {
            if valid_language(language) {
                self.language = language.to_owned();
            }
        }
        before.0 != self.tmdb_key || before.1 != self.language
    }

    pub fn has_tmdb(&self) -> bool {
        !self.tmdb_key.is_empty()
    }

    pub fn redacted_key(&self) -> String {
        if self.tmdb_key.is_empty() {
            "not configured".to_owned()
        } else {
            format!("configured ({} chars)", self.tmdb_key.len())
        }
    }
}

fn sanitize_key(raw: &str) -> String {
    let key = raw.trim();
    if key.len() > 512 || key.chars().any(char::is_control) {
        String::new()
    } else {
        key.to_owned()
    }
}

fn valid_language(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 16
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
