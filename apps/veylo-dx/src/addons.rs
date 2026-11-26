//! Neutral Stremio add-on management: installed add-ons, update checks, `stremio://` & `veylo://`
//! deep-link installation, `configurable` manifest support, portable add-on bundle import/export,
//! optional one-time import from a user's own Stremio account, and generic user-supplied add-on
//! directory URLs.
//!
//! Veylo ships with zero third-party streaming add-ons or debrid credentials. Every add-on URL
//! stays masked in the UI, stored only in the app's private folder, and is never logged or put in
//! errors.
use crate::backend::{self, exec, on_runtime, s};
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------- languages

/// An audio language: its settings key, display label, flag, common release tags, and ISO 639-1 /
/// 639-2 track codes (mpv's `alang` order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Language {
    pub key: &'static str,
    pub label: &'static str,
    flag: &'static str,
    tags: &'static [&'static str],
    pub codes: &'static [&'static str],
}

impl Language {
    /// Whether a source's label says it has this audio (flag, name or release tag). Subtitle
    /// languages do not count: flags after "Subs" or a tag after "Sub" ("Sub ITA").
    pub fn matches(&self, label: &str) -> bool {
        label.lines().any(|line| {
            let audio = line.find("Subs").map_or(line, |i| &line[..i]);
            if audio.contains(self.flag) { return true; }
            let words: Vec<&str> = audio.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            words.iter().enumerate().any(|(i, w)| self.tags.iter().any(|t| w.eq_ignore_ascii_case(t))
                && !(i > 0 && ["sub", "subs", "subbed", "subtitles"].iter().any(|s| words[i - 1].eq_ignore_ascii_case(s))))
        })
    }
}

macro_rules! lang { ($key:expr, $label:expr, $flag:expr, [$($tag:expr),*], [$($code:expr),*]) => {
    Language { key: $key, label: $label, flag: $flag, tags: &[$($tag),*], codes: &[$($code),*] }
}; }

pub const LANGUAGES: &[Language] = &[
    lang!("english", "English", "🇬🇧", ["ENG", "ENGLISH"], ["en", "eng"]),
    lang!("italian", "Italian", "🇮🇹", ["ITA", "ITALIAN", "ITALIANO"], ["it", "ita"]),
    lang!("spanish", "Spanish", "🇪🇸", ["SPA", "ESP", "SPANISH", "CASTELLANO", "ESPANOL"], ["es", "spa"]),
    lang!("latino", "Latino", "🇲🇽", ["LATINO", "LAT"], ["es-419", "es-mx", "es", "spa"]),
    lang!("french", "French", "🇫🇷", ["FRE", "FRA", "FRENCH", "VFF", "VF", "TRUEFRENCH"], ["fr", "fre", "fra"]),
    lang!("german", "German", "🇩🇪", ["GER", "DEU", "GERMAN"], ["de", "ger", "deu"]),
    lang!("portuguese", "Portuguese", "🇵🇹", ["POR", "PORTUGUESE", "DUBLADO"], ["pt", "por", "pt-br", "pob"]),
    lang!("dutch", "Dutch", "🇳🇱", ["DUT", "NLD", "DUTCH"], ["nl", "dut", "nld"]),
    lang!("polish", "Polish", "🇵🇱", ["POL", "POLISH", "PL"], ["pl", "pol"]),
    lang!("russian", "Russian", "🇷🇺", ["RUS", "RUSSIAN"], ["ru", "rus"]),
    lang!("ukrainian", "Ukrainian", "🇺🇦", ["UKR", "UKRAINIAN"], ["uk", "ukr"]),
    lang!("turkish", "Turkish", "🇹🇷", ["TUR", "TURKISH"], ["tr", "tur"]),
    lang!("greek", "Greek", "🇬🇷", ["GRE", "GREEK"], ["el", "gre", "ell"]),
    lang!("swedish", "Swedish", "🇸🇪", ["SWE", "SWEDISH"], ["sv", "swe"]),
    lang!("danish", "Danish", "🇩🇰", ["DAN", "DANISH"], ["da", "dan"]),
    lang!("norwegian", "Norwegian", "🇳🇴", ["NOR", "NORWEGIAN"], ["no", "nor", "nob", "nb"]),
    lang!("finnish", "Finnish", "🇫🇮", ["FIN", "FINNISH"], ["fi", "fin"]),
    lang!("czech", "Czech", "🇨🇿", ["CZE", "CES", "CZECH"], ["cs", "cze", "ces"]),
    lang!("hungarian", "Hungarian", "🇭🇺", ["HUN", "HUNGARIAN"], ["hu", "hun"]),
    lang!("romanian", "Romanian", "🇷🇴", ["ROM", "RON", "ROMANIAN"], ["ro", "rum", "ron"]),
    lang!("arabic", "Arabic", "🇸🇦", ["ARA", "ARABIC"], ["ar", "ara"]),
    lang!("hebrew", "Hebrew", "🇮🇱", ["HEB", "HEBREW"], ["he", "heb"]),
    lang!("hindi", "Hindi", "🇮🇳", ["HIN", "HINDI"], ["hi", "hin"]),
    lang!("japanese", "Japanese", "🇯🇵", ["JAP", "JPN", "JAPANESE"], ["ja", "jpn"]),
    lang!("korean", "Korean", "🇰🇷", ["KOR", "KOREAN"], ["ko", "kor"]),
    lang!("chinese", "Chinese", "🇨🇳", ["CHI", "CHN", "CHINESE", "MANDARIN"], ["zh", "chi", "zho"]),
];

pub fn language(key: &str) -> Option<&'static Language> { LANGUAGES.iter().find(|l| l.key == key) }

fn host_of(url: &str) -> String { url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default() }

fn is_cinemeta(url: &str) -> bool { url.contains("v3-cinemeta.strem.io") }

// ---------------------------------------------------------------- local seen-version record

/// addons.json: the add-on versions you have seen, so a newer one can be announced once.
#[derive(Default)]
struct Store { seen: HashMap<String, String> }

fn store_path() -> std::path::PathBuf { backend::backend_data_dir().join("addons.json") }

fn load() -> Store {
    let v: Value = std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let seen = v["seen"].as_object().cloned().unwrap_or_default()
        .into_iter().filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned()))).collect();
    Store { seen }
}

fn save(store: &Store) {
    let v = json!({"seen": store.seen});
    let _ = backend::write_private_file(&store_path(), v.to_string().as_bytes());
    backend::sync_soon();
}

/// The installed add-ons (core descriptors: the manifest plus `manifest_url`).
async fn installed() -> Vec<Value> {
    exec(json!({"op":"addons"})).await.ok().and_then(|v| v["addons"].as_array().cloned()).unwrap_or_default()
}

// ---------------------------------------------------------------- status for Settings

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub version: String,
    /// A newer version you have not applied yet.
    pub update: Option<String>,
    pub offers: String,
    /// Manifest declares `behaviorHints.configurable == true`.
    pub configurable: bool,
    /// Installed add-on's link: kept inside the app for Configure, Update and Remove, never displayed.
    url: String,
}

impl Row {
    /// Custom user-installed add-ons can be removed; Cinemeta is the built-in metadata core.
    pub fn removable(&self) -> bool { !is_cinemeta(&self.url) }

    /// Standard Stremio `/configure` page URL derived from `manifest_url`, when `configurable`.
    pub fn configure_url(&self) -> Option<String> {
        if !self.configurable { return None; }
        derive_configure_url(&self.url)
    }

    /// Open this add-on's `/configure` page in the system browser on explicit user tap.
    pub fn open_configure(&self) {
        if let Some(url) = self.configure_url() {
            crate::open_external(&url);
        }
    }
}

/// Derive a Stremio add-on's `/configure` URL from its manifest URL.
pub fn derive_configure_url(manifest_url: &str) -> Option<String> {
    let parsed = url::Url::parse(manifest_url).ok()?;
    if parsed.scheme() != "https" { return None; }
    let trimmed = manifest_url.strip_suffix("/manifest.json").unwrap_or(manifest_url.trim_end_matches('/'));
    Some(format!("{trimmed}/configure"))
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Overview {
    /// All installed add-ons (built-in Cinemeta plus user-installed add-ons).
    pub addons: Vec<Row>,
    pub updates: usize,
}

use crate::updater::newer;

fn seen_key(addon: &Value) -> String { format!("{}@{}", s(addon, "id"), host_of(&s(addon, "manifest_url"))) }

/// Installed add-ons with their versions; the first sighting of an add-on is recorded silently,
/// later a higher version shows as an update until you apply it.
pub async fn overview() -> Overview {
    let addons = installed().await;
    let mut store = load();
    let mut changed = false;
    let mut rows = Vec::new();
    for a in &addons {
        let url = s(a, "manifest_url");
        if crate::backend::retired_addon(&url) { continue; }
        let version = s(a, "version");
        let key = seen_key(a);
        let update = match store.seen.get(&key) {
            Some(seen) if newer(&version, seen) => Some(version.clone()),
            Some(_) => None,
            None => { store.seen.insert(key, version.clone()); changed = true; None }
        };
        let offers = a["resources"].as_array().into_iter().flatten()
            .filter_map(|r| r.as_str().map(str::to_owned).or_else(|| r["name"].as_str().map(str::to_owned))).collect::<Vec<_>>().join(", ");
        let configurable = a["behaviorHints"]["configurable"].as_bool().unwrap_or(false)
            && derive_configure_url(&url).is_some();
        rows.push(Row { name: s(a, "name"), version, update, offers, configurable, url });
    }
    if changed { save(&store); }
    let updates = rows.iter().filter(|r| r.update.is_some()).count();
    Overview { addons: rows, updates }
}

/// Ask every installed add-on for its manifest again (new versions), then count updates.
pub async fn check_updates() -> usize { check().await.0 }

/// The check, with what to tell you: updates found, and add-ons that didn't answer.
pub async fn check() -> (usize, String) {
    let refreshed = exec(json!({"op":"refresh_addons"})).await.ok();
    let unreachable: Vec<String> = refreshed.iter().flat_map(|v| v["results"].as_array().cloned().unwrap_or_default())
        .filter(|r| r.get("error").is_some()).map(|r| s(&r["addon"], "name")).collect();
    let ov = overview().await;
    let total = ov.addons.len();
    let mut note = match ov.updates {
        0 => format!("All {total} add-ons are on their latest version."),
        1 => "1 update available: tap Update on it.".to_owned(),
        n => format!("{n} updates available: tap Update on each."),
    };
    if !unreachable.is_empty() { note.push_str(&format!(" Couldn't reach {}.", unreachable.join(", "))); }
    (ov.updates, note)
}

fn mark_seen(store: &mut Store, addon: &Value) {
    store.seen.insert(seen_key(addon), s(addon, "version"));
}

// ---------------------------------------------------------------- install, update, remove

static CHANGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn remove(row: &Row) -> Result<()> {
    let _one = CHANGING.lock().await;
    exec(json!({"op":"remove_addon","url":row.url})).await?;
    backend::forget_sources();
    backend::sync_soon();
    Ok(())
}

/// Extract a Stremio manifest URL from an OS deep link (`stremio://...` or `veylo://install?url=...`).
pub fn parse_deep_link(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("stremio://") {
        return normalize_custom_addon_url(trimmed).ok();
    }
    if let Ok(parsed) = url::Url::parse(trimmed) {
        if parsed.scheme().eq_ignore_ascii_case("veylo") {
            if let Some((_, target)) = parsed.query_pairs().find(|(k, _)| k == "url" || k == "manifest") {
                return normalize_custom_addon_url(&target).ok();
            }
            if parsed.host_str().is_some_and(|h| !h.eq_ignore_ascii_case("install")) {
                let rest = trimmed.strip_prefix("veylo://").unwrap_or_default();
                return normalize_custom_addon_url(&format!("https://{rest}")).ok();
            }
        }
    }
    None
}

/// Normalize a user-provided Stremio add-on URL (`stremio://` -> `https://`, appending
/// `/manifest.json` if omitted). Rejects non-HTTPS remote URLs; never echoes the URL in errors.
pub fn normalize_custom_addon_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    ensure!(!trimmed.is_empty(), "Paste a Stremio add-on manifest URL.");
    let normalized = if let Some(rest) = trimmed.strip_prefix("stremio://") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("veylo://") {
        if let Ok(parsed) = url::Url::parse(trimmed) {
            if let Some((_, target)) = parsed.query_pairs().find(|(k, _)| k == "url" || k == "manifest") {
                return normalize_custom_addon_url(&target);
            }
        }
        format!("https://{rest}")
    } else {
        trimmed.to_owned()
    };
    let parsed = url::Url::parse(&normalized).map_err(|_| anyhow::anyhow!("That doesn't look like a valid add-on URL."))?;
    let host = parsed.host_str().unwrap_or_default();
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    ensure!(parsed.scheme() == "https" || (parsed.scheme() == "http" && is_local),
        "Add-on links must start with https:// (or stremio://).");
    let mut url = normalized;
    if !parsed.path().ends_with("/manifest.json") {
        let base = url.trim_end_matches('/');
        url = format!("{base}/manifest.json");
    }
    Ok(url)
}

/// Fetch basic public metadata (name, description, version, resources) of a manifest URL before
/// confirming installation from a deep link, without installing it yet.
#[derive(Clone, Debug, PartialEq)]
pub struct ManifestPreview {
    pub url: String,
    pub host: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub offers: String,
}

pub async fn preview_manifest(raw: &str) -> Result<ManifestPreview> {
    let url = normalize_custom_addon_url(raw)?;
    let host = host_of(&url);
    let fetch_url = url.clone();
    let v: Value = on_runtime(async move {
        let resp = backend::backend_http()
            .get(&fetch_url)
            .timeout(std::time::Duration::from_secs(12))
            .send().await
            .map_err(|_| anyhow::anyhow!("Couldn't reach that add-on server."))?;
        ensure!(resp.status().is_success(), "That add-on server didn't return a manifest.");
        resp.json::<Value>().await.map_err(|_| anyhow::anyhow!("That link didn't return a valid Stremio manifest."))
    }).await??;
    let name = s(&v, "name");
    ensure!(!name.is_empty(), "That link didn't return a valid Stremio manifest.");
    let offers = v["resources"].as_array().into_iter().flatten()
        .filter_map(|r| r.as_str().map(str::to_owned).or_else(|| r["name"].as_str().map(str::to_owned)))
        .collect::<Vec<_>>().join(", ");
    Ok(ManifestPreview {
        url,
        host,
        name,
        version: s(&v, "version"),
        description: s(&v, "description").chars().take(240).collect(),
        offers,
    })
}

/// Install any user-supplied Stremio add-on manifest URL. Never logs or echoes the URL.
pub async fn install_custom(raw: String) -> Result<String> {
    let url = normalize_custom_addon_url(&raw)?;
    let _one = CHANGING.lock().await;
    let addon = exec(json!({"op":"install_addon","url":url})).await
        .map_err(|_| anyhow::anyhow!("That add-on couldn't be installed. Check the link and your connection."))?;
    let mut store = load();
    mark_seen(&mut store, &addon);
    save(&store);
    backend::forget_sources();
    backend::sync_soon();
    let name = s(&addon, "name");
    Ok(if name.is_empty() { "Add-on".into() } else { name })
}

/// Refresh a single installed add-on's manifest.
pub async fn update(row: &Row) -> Result<()> {
    let _one = CHANGING.lock().await;
    let addon = exec(json!({"op":"install_addon","url":row.url})).await?;
    let mut store = load();
    mark_seen(&mut store, &addon);
    save(&store);
    backend::forget_sources();
    Ok(())
}

// ---------------------------------------------------------------- bundle import / export & Stremio import

/// Export all user-installed add-ons (excluding built-in Cinemeta) as a portable JSON bundle.
/// Saved to `veylo-addons-bundle.json` with owner-only permissions; returns the count exported.
pub async fn export_bundle() -> Result<(usize, String)> {
    let all = installed().await;
    let urls: Vec<String> = all.iter()
        .map(|a| s(a, "manifest_url"))
        .filter(|u| !u.is_empty() && !is_cinemeta(u) && !crate::backend::retired_addon(u))
        .collect();
    let count = urls.len();
    let payload = json!({
        "veylo_addons_bundle": 1,
        "addons": urls.iter().map(|u| json!({"transportUrl": u})).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&payload)?;
    let _ = backend::write_private_file(&backend::backend_data_dir().join("veylo-addons-bundle.json"), text.as_bytes());
    Ok((count, text))
}

/// Parse manifest URLs from a Veylo bundle JSON, a Stremio `addonCollection` JSON array, a JSON
/// array of strings, or a newline-separated list of manifest URLs.
pub fn parse_bundle_urls(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |candidate: &str| {
        if let Ok(url) = normalize_custom_addon_url(candidate) {
            if !is_cinemeta(&url) && seen.insert(url.clone()) {
                out.push(url);
            }
        }
    };
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        let items = v.as_array()
            .or_else(|| v["addons"].as_array())
            .or_else(|| v["result"]["addons"].as_array());
        if let Some(arr) = items {
            for entry in arr {
                if let Some(u) = entry.as_str() { push(u); }
                else if let Some(u) = entry["transportUrl"].as_str().or_else(|| entry["manifestUrl"].as_str()).or_else(|| entry["url"].as_str()) {
                    push(u);
                }
            }
            return out;
        }
    }
    for line in trimmed.lines() {
        let t = line.trim();
        if !t.is_empty() && !t.starts_with('#') { push(t); }
    }
    out
}

/// Install all valid add-on URLs from a pasted bundle or URL list. Returns `(installed, failed)`.
pub async fn import_bundle(raw: &str) -> Result<(usize, usize)> {
    let urls = parse_bundle_urls(raw);
    ensure!(!urls.is_empty(), "No valid add-on URLs found. Paste a bundle JSON or one https:// or stremio:// link per line.");
    let _one = CHANGING.lock().await;
    let here: HashSet<String> = installed().await.iter().map(|a| s(a, "manifest_url")).collect();
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut store = load();
    for url in urls {
        if here.contains(&url) { continue; }
        match exec(json!({"op":"install_addon","url":url})).await {
            Ok(addon) => { mark_seen(&mut store, &addon); ok += 1; }
            Err(_) => { failed += 1; }
        }
    }
    save(&store);
    backend::forget_sources();
    backend::sync_soon();
    Ok((ok, failed))
}

/// One-time on-device import of the user's own add-ons from their Stremio account.
/// Credentials and the Stremio auth key are used only in memory for this single call and discarded.
pub async fn import_from_stremio(email: String, password: String) -> Result<(usize, usize)> {
    let email = email.trim().to_owned();
    ensure!(!email.is_empty() && !password.is_empty(), "Enter your Stremio email and password.");
    let urls: Vec<String> = on_runtime(async move {
        let http = backend::backend_http();
        let login_resp = http.post("https://api.strem.io/api/login")
            .timeout(std::time::Duration::from_secs(15))
            .json(&json!({"type": "Login", "email": email, "password": password}))
            .send().await
            .map_err(|_| anyhow::anyhow!("Couldn't reach Stremio's account server."))?;
        let login_json: Value = login_resp.json().await.unwrap_or_default();
        if let Some(err) = login_json["error"]["message"].as_str() {
            bail!("Stremio sign-in failed: {}", err.chars().take(120).collect::<String>());
        }
        let auth_key = login_json["result"]["authKey"].as_str().unwrap_or_default().to_owned();
        ensure!(!auth_key.is_empty(), "Stremio sign-in failed. Check your email and password.");
        let col_resp = http.post("https://api.strem.io/api/addonCollectionGet")
            .timeout(std::time::Duration::from_secs(15))
            .json(&json!({"type": "AddonCollectionGet", "authKey": auth_key, "update": true}))
            .send().await
            .map_err(|_| anyhow::anyhow!("Couldn't fetch your Stremio add-on collection."))?;
        let col_json: Value = col_resp.json().await.unwrap_or_default();
        // Best-effort logout so the temporary authKey is invalidated immediately.
        let _ = http.post("https://api.strem.io/api/logout")
            .timeout(std::time::Duration::from_secs(5))
            .json(&json!({"type": "Logout", "authKey": auth_key}))
            .send().await;
        let list = col_json["result"]["addons"].as_array().cloned().unwrap_or_default();
        let mut urls = Vec::new();
        for item in list {
            if let Some(u) = item["transportUrl"].as_str() {
                if let Ok(norm) = normalize_custom_addon_url(u) {
                    if !is_cinemeta(&norm) { urls.push(norm); }
                }
            }
        }
        Ok::<Vec<String>, anyhow::Error>(urls)
    }).await??;
    ensure!(!urls.is_empty(), "No custom add-ons were found in that Stremio account.");
    let bundle = json!({"addons": urls.iter().map(|u| json!({"transportUrl": u})).collect::<Vec<_>>()}).to_string();
    import_bundle(&bundle).await
}

// ---------------------------------------------------------------- generic add-on directory reader

#[derive(Clone, Debug, PartialEq)]
pub struct DirectoryEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub offers: String,
    pub manifest_url: String,
    pub installed: bool,
}

/// Read any user-supplied Stremio add-on directory JSON URL (e.g. a community catalog list or
/// self-hosted directory). Veylo ships with zero pre-installed stream directories.
pub async fn fetch_directory(raw_url: String) -> Result<Vec<DirectoryEntry>> {
    let trimmed = raw_url.trim().to_owned();
    let parsed = url::Url::parse(&trimmed).map_err(|_| anyhow::anyhow!("That doesn't look like a valid directory URL."))?;
    let host = parsed.host_str().unwrap_or_default();
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    ensure!(parsed.scheme() == "https" || (parsed.scheme() == "http" && is_local),
        "Directory URLs must start with https://.");
    let v: Value = on_runtime(async move {
        let resp = backend::backend_http()
            .get(&trimmed)
            .timeout(std::time::Duration::from_secs(15))
            .send().await
            .map_err(|_| anyhow::anyhow!("Couldn't reach that directory URL."))?;
        ensure!(resp.status().is_success(), "That directory server returned an error.");
        resp.json::<Value>().await.map_err(|_| anyhow::anyhow!("That URL didn't return a valid JSON directory."))
    }).await??;
    let here: HashSet<String> = installed().await.iter().map(|a| s(a, "manifest_url")).collect();
    let arr = v.as_array().or_else(|| v["addons"].as_array()).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for item in arr {
        let Some(raw) = item["transportUrl"].as_str().or_else(|| item["manifestUrl"].as_str()).or_else(|| item["url"].as_str()) else { continue };
        let Ok(manifest_url) = normalize_custom_addon_url(raw) else { continue };
        if is_cinemeta(&manifest_url) { continue; }
        let m = if item["manifest"].is_object() { &item["manifest"] } else { &item };
        let name = s(m, "name");
        if name.is_empty() { continue; }
        let offers = m["resources"].as_array().into_iter().flatten()
            .filter_map(|r| r.as_str().map(str::to_owned).or_else(|| r["name"].as_str().map(str::to_owned)))
            .collect::<Vec<_>>().join(", ");
        out.push(DirectoryEntry {
            name,
            version: s(m, "version"),
            description: s(m, "description").chars().take(180).collect(),
            offers,
            installed: here.contains(&manifest_url),
            manifest_url,
        });
    }
    ensure!(!out.is_empty(), "No Stremio add-ons were found in that directory.");
    Ok(out)
}

// ---------------------------------------------------------------- cross-device sync & startup

/// User-installed add-on URLs for encrypted cross-device sync (backend/account.rs).
/// Travels only inside the end-to-end encrypted sync envelope.
pub async fn for_handoff() -> Value {
    let urls: Vec<String> = installed().await.iter()
        .map(|a| s(a, "manifest_url"))
        .filter(|u| !u.is_empty() && !is_cinemeta(u) && !crate::backend::retired_addon(u))
        .collect();
    json!({"urls": urls})
}

/// Adopt user-installed add-ons synced from another of your devices.
/// Supports both the current `{"urls": [...]}` shape and legacy `{"others": [...]}`.
pub async fn adopt(handoff: &Value) {
    let _one = CHANGING.lock().await;
    let here: HashSet<String> = installed().await.iter().map(|a| s(a, "manifest_url")).collect();
    let iter = handoff["urls"].as_array().into_iter().flatten()
        .chain(handoff["others"].as_array().into_iter().flatten())
        .filter_map(Value::as_str);
    for raw in iter {
        let Ok(url) = normalize_custom_addon_url(raw) else { continue };
        if !is_cinemeta(&url) && !here.contains(&url) {
            let _ = exec(json!({"op":"install_addon","url":url})).await;
        }
    }
    backend::forget_sources();
}

/// At start: check for new versions on user-installed add-ons (never auto-installs anything).
pub async fn startup() -> usize {
    check_updates().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_match_flags_and_release_tags() {
        let it = language("italian").unwrap();
        assert!(it.matches("Inception 2010 2160p\n🇬🇧 / 🇮🇹"));
        assert!(it.matches("Inception.2010.BDRemux.Ita.Eng.x265"));
        assert!(!it.matches("Inception.2010.2160p.WEB-DL.Italic.Subs"));
        assert!(!it.matches("Inception 2010 2160p 🇬🇧"));
        assert!(!it.matches("Film.2160p\n💾 79 GiB\n🇬🇧 / 🇯🇵 Subs / 🇬🇧 / 🇮🇹"));
        assert!(!it.matches("Film 2010 AC3 5.1 ENG 2160p sub ITA"));
        assert!(it.matches("Film (2010) AC3 5.1 ITA.ENG 2160p H265 sub NUita.eng"));
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.0.16", "0.0.15"));
        assert!(newer("2.40.0", "2.38.1"));
        assert!(!newer("2.38.1", "2.38.1"));
        assert!(!newer("1.9", "1.10"));
    }

    #[test]
    fn normalizes_validates_and_parses_deep_links_and_configure_urls() {
        assert_eq!(normalize_custom_addon_url("stremio://example.com/cfg").unwrap(), "https://example.com/cfg/manifest.json");
        assert_eq!(normalize_custom_addon_url("https://example.com/manifest.json").unwrap(), "https://example.com/manifest.json");
        assert_eq!(normalize_custom_addon_url("http://127.0.0.1:7000/").unwrap(), "http://127.0.0.1:7000/manifest.json");
        assert!(normalize_custom_addon_url("http://remote.example.com/manifest.json").is_err());
        assert!(normalize_custom_addon_url("not-a-url").is_err());

        assert_eq!(parse_deep_link("stremio://addons.example/my-addon/manifest.json").as_deref(),
            Some("https://addons.example/my-addon/manifest.json"));
        assert_eq!(parse_deep_link("veylo://install?url=https%3A%2F%2Faddons.example%2Fcfg%2Fmanifest.json").as_deref(),
            Some("https://addons.example/cfg/manifest.json"));
        assert_eq!(parse_deep_link("https://example.com/not-a-deep-link"), None);

        assert_eq!(derive_configure_url("https://addons.example/cfg/manifest.json").as_deref(),
            Some("https://addons.example/cfg/configure"));
        assert_eq!(derive_configure_url("http://127.0.0.1:7000/manifest.json"), None);
    }

    #[test]
    fn parses_stremio_veylo_and_plain_addon_bundles() {
        let stremio = r#"{"addons":[{"transportUrl":"https://v3-cinemeta.strem.io/manifest.json"},{"transportUrl":"https://a.example/manifest.json"},{"transportUrl":"stremio://b.example/cfg"}]}"#;
        assert_eq!(parse_bundle_urls(stremio), vec![
            "https://a.example/manifest.json",
            "https://b.example/cfg/manifest.json",
        ]);
        let plain = "# comment\nhttps://a.example/manifest.json\nstremio://b.example\nhttp://insecure.example/manifest.json\n";
        assert_eq!(parse_bundle_urls(plain), vec![
            "https://a.example/manifest.json",
            "https://b.example/manifest.json",
        ]);
    }
}
