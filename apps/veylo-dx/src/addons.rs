//! The add-on marketplace: a short curated list of stream add-ons that Veylo sets up itself
//! with the one Real-Debrid key from Settings, so no add-on website is needed.
//!
//! - Torrentio keeps its settings in the install link; Veylo writes it.
//! - AIOStreams (the public ElfHosted instance, which has Torrentio turned off) keeps them on
//!   its server; Veylo creates and updates that configuration through its API.
//!
//! Both ask for every cached Real-Debrid result with no quality filter and no limit: the
//! Play order picks the best one. Install links, passwords and keys live only in the app's
//! own folder (addons.json, settings.json) and are never logged, shown or put in errors.
use crate::backend::{self, exec, on_runtime, s};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

const TORRENTIO: &str = "https://torrentio.strem.fun";
const AIOSTREAMS: &str = "https://aiostreams.elfhosted.com";
/// Where each key is copied from: opened in the browser by "Get your key".
pub const RD_KEY_PAGE: &str = "https://real-debrid.com/apitoken";
pub const TMDB_KEY_PAGE: &str = "https://www.themoviedb.org/settings/api";

// ---------------------------------------------------------------- languages

/// A stream language: Torrentio's option key, the AIOStreams name, the flag both add-ons
/// print and the release tags that name it in a file name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Language {
    pub key: &'static str,
    pub label: &'static str,
    aio: &'static str,
    flag: &'static str,
    tags: &'static [&'static str],
}

impl Language {
    /// Whether a source's label says it has this audio (flag, name or release tag). Subtitle
    /// languages do not count: flags after "Subs" (AIOStreams) or a tag after "Sub" ("Sub ITA").
    pub fn matches(&self, label: &str) -> bool {
        label.lines().any(|line| {
            let audio = line.find("Subs").map_or(line, |i| &line[..i]);
            if audio.contains(self.flag) { return true; }
            let words: Vec<&str> = line.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            words.iter().enumerate().any(|(i, w)| self.tags.iter().any(|t| w.eq_ignore_ascii_case(t))
                && !(i > 0 && ["sub", "subs", "subbed", "subtitles"].iter().any(|s| words[i - 1].eq_ignore_ascii_case(s))))
        })
    }
}

macro_rules! lang { ($key:expr, $label:expr, $flag:expr, [$($tag:expr),*]) => {
    Language { key: $key, label: $label, aio: $label, flag: $flag, tags: &[$($tag),*] }
}; }

pub const LANGUAGES: &[Language] = &[
    lang!("english", "English", "🇬🇧", ["ENG", "ENGLISH"]),
    lang!("italian", "Italian", "🇮🇹", ["ITA", "ITALIAN", "ITALIANO"]),
    lang!("spanish", "Spanish", "🇪🇸", ["SPA", "ESP", "SPANISH", "CASTELLANO", "ESPANOL"]),
    lang!("latino", "Latino", "🇲🇽", ["LATINO", "LAT"]),
    lang!("french", "French", "🇫🇷", ["FRE", "FRA", "FRENCH", "VFF", "VF", "TRUEFRENCH"]),
    lang!("german", "German", "🇩🇪", ["GER", "DEU", "GERMAN"]),
    lang!("portuguese", "Portuguese", "🇵🇹", ["POR", "PORTUGUESE", "DUBLADO"]),
    lang!("dutch", "Dutch", "🇳🇱", ["DUT", "NLD", "DUTCH"]),
    lang!("polish", "Polish", "🇵🇱", ["POL", "POLISH", "PL"]),
    lang!("russian", "Russian", "🇷🇺", ["RUS", "RUSSIAN"]),
    lang!("ukrainian", "Ukrainian", "🇺🇦", ["UKR", "UKRAINIAN"]),
    lang!("turkish", "Turkish", "🇹🇷", ["TUR", "TURKISH"]),
    lang!("greek", "Greek", "🇬🇷", ["GRE", "GREEK"]),
    lang!("swedish", "Swedish", "🇸🇪", ["SWE", "SWEDISH"]),
    lang!("danish", "Danish", "🇩🇰", ["DAN", "DANISH"]),
    lang!("norwegian", "Norwegian", "🇳🇴", ["NOR", "NORWEGIAN"]),
    lang!("finnish", "Finnish", "🇫🇮", ["FIN", "FINNISH"]),
    lang!("czech", "Czech", "🇨🇿", ["CZE", "CES", "CZECH"]),
    lang!("hungarian", "Hungarian", "🇭🇺", ["HUN", "HUNGARIAN"]),
    lang!("romanian", "Romanian", "🇷🇴", ["ROM", "RON", "ROMANIAN"]),
    lang!("arabic", "Arabic", "🇸🇦", ["ARA", "ARABIC"]),
    lang!("hebrew", "Hebrew", "🇮🇱", ["HEB", "HEBREW"]),
    lang!("hindi", "Hindi", "🇮🇳", ["HIN", "HINDI"]),
    lang!("japanese", "Japanese", "🇯🇵", ["JAP", "JPN", "JAPANESE"]),
    lang!("korean", "Korean", "🇰🇷", ["KOR", "KOREAN"]),
    lang!("chinese", "Chinese", "🇨🇳", ["CHI", "CHN", "CHINESE", "MANDARIN"]),
];

pub fn language(key: &str) -> Option<&'static Language> { LANGUAGES.iter().find(|l| l.key == key) }

// ---------------------------------------------------------------- catalogue

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Curated { Torrentio, AioStreams }

impl Curated {
    pub const ALL: [Curated; 2] = [Curated::Torrentio, Curated::AioStreams];
    fn id(self) -> &'static str { match self { Curated::Torrentio => "torrentio", Curated::AioStreams => "aiostreams" } }
    pub fn name(self) -> &'static str { match self { Curated::Torrentio => "Torrentio", Curated::AioStreams => "AIOStreams" } }
    pub fn about(self) -> &'static str {
        match self {
            Curated::Torrentio => "Torrents from every public tracker, ready on Real-Debrid.",
            Curated::AioStreams => "Comet, MediaFusion and StremThru together, on ElfHosted's public server.",
        }
    }
    fn host(self) -> &'static str { match self { Curated::Torrentio => "torrentio.strem.fun", Curated::AioStreams => "aiostreams.elfhosted.com" } }
}

fn host_of(url: &str) -> String { url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default() }

// ---------------------------------------------------------------- the app's own record

/// addons.json: what Veylo set up (links, AIOStreams login) and the add-on versions you
/// have seen, so a newer one can be announced once.
#[derive(Default)]
struct Store { managed: HashMap<String, Value>, seen: HashMap<String, String> }

fn store_path() -> std::path::PathBuf { backend::backend_data_dir().join("addons.json") }

fn load() -> Store {
    let v: Value = std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let map = |k: &str| v[k].as_object().cloned().unwrap_or_default();
    Store {
        managed: map("managed").into_iter().collect(),
        seen: map("seen").into_iter().filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned()))).collect(),
    }
}

fn save(store: &Store) {
    let v = json!({"managed": store.managed, "seen": store.seen});
    let _ = std::fs::write(store_path(), v.to_string());
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
    /// The curated entry this is, when Veylo set it up.
    pub curated: Option<Curated>,
    /// Installed add-on's link: kept inside the app for Update and Remove, never displayed.
    url: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Overview {
    /// Curated entries with their installed row, if any.
    pub curated: Vec<(Curated, Option<Row>)>,
    /// Everything else that is installed.
    pub others: Vec<Row>,
    pub updates: usize,
}

fn version_parts(v: &str) -> Vec<u64> { v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect() }
fn newer(candidate: &str, than: &str) -> bool { version_parts(candidate) > version_parts(than) }

fn seen_key(addon: &Value) -> String { format!("{}@{}", s(addon, "id"), host_of(&s(addon, "manifest_url"))) }

/// Installed add-ons with their versions; the first sighting of an add-on is recorded silently,
/// later a higher version shows as an update until you apply it.
pub async fn overview() -> Overview {
    let addons = installed().await;
    let mut store = load();
    let mut changed = false;
    let mut out = Overview::default();
    let managed_url = |c: Curated, store: &Store| store.managed.get(c.id()).map(|m| s(m, "url"));
    let mut rows = Vec::new();
    for a in &addons {
        let version = s(a, "version");
        let key = seen_key(a);
        let update = match store.seen.get(&key) {
            Some(seen) if newer(&version, seen) => Some(version.clone()),
            Some(_) => None,
            None => { store.seen.insert(key, version.clone()); changed = true; None }
        };
        let url = s(a, "manifest_url");
        let curated = Curated::ALL.into_iter().find(|c| managed_url(*c, &store).as_deref() == Some(url.as_str()));
        let offers = a["resources"].as_array().into_iter().flatten()
            .filter_map(|r| r.as_str().map(str::to_owned).or_else(|| r["name"].as_str().map(str::to_owned))).collect::<Vec<_>>().join(", ");
        rows.push(Row { name: s(a, "name"), version, update, offers, curated, url });
    }
    if changed { save(&store); }
    out.updates = rows.iter().filter(|r| r.update.is_some()).count();
    for c in Curated::ALL {
        out.curated.push((c, rows.iter().find(|r| r.curated == Some(c)).cloned()));
    }
    out.others = rows.into_iter().filter(|r| r.curated.is_none()).collect();
    out
}

/// Ask every installed add-on for its manifest again (new versions), then count updates.
pub async fn check_updates() -> usize { check().await.0 }

/// The check, with what to tell you: updates found, and add-ons that didn't answer.
pub async fn check() -> (usize, String) {
    let refreshed = exec(json!({"op":"refresh_addons"})).await.ok();
    let unreachable: Vec<String> = refreshed.iter().flat_map(|v| v["results"].as_array().cloned().unwrap_or_default())
        .filter(|r| r.get("error").is_some()).map(|r| s(&r["addon"], "name")).collect();
    let ov = overview().await;
    let total = ov.others.len() + ov.curated.iter().filter(|(_, r)| r.is_some()).count();
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

fn rd_key() -> Result<String> {
    backend::load_settings().rd_key.context("Add your Real-Debrid key first")
}

/// Torrentio: all trackers, every cached Real-Debrid result (no "download to debrid" links),
/// no quality filter or limit, no Downloads catalogue; your language first when it has one.
fn torrentio_url(key: &str, language: Option<&Language>) -> String {
    let mut opts = Vec::new();
    if let Some(l) = language.filter(|l| l.key != "english") { opts.push(format!("language={}", l.key)); }
    opts.push("debridoptions=nodownloadlinks,nocatalog".into());
    opts.push(format!("realdebrid={key}"));
    format!("{TORRENTIO}/{}/manifest.json", opts.join("|"))
}

/// AIOStreams configuration: three debrid scrapers on Real-Debrid, cached only, streams only
/// (no extra catalogues), the Torrentio-style labels the Play order reads, no filters.
fn aiostreams_config(key: &str, language: Option<&Language>) -> Value {
    let preset = |kind: &str, name: &str| json!({"type": kind, "instanceId": kind, "enabled": true,
        "options": {"name": name, "timeout": 5000, "resources": ["stream"], "services": ["realdebrid"], "mediaTypes": [], "includeP2P": false}});
    let mut sort = vec![json!({"key": "cached", "direction": "desc"})];
    if language.is_some() { sort.push(json!({"key": "language", "direction": "desc"})); }
    sort.extend([json!({"key": "resolution", "direction": "desc"}), json!({"key": "size", "direction": "desc"})]);
    let mut config = json!({
        "services": [{"id": "realdebrid", "enabled": true, "credentials": {"apiKey": key}}],
        "presets": [preset("comet", "Comet"), preset("mediafusion", "MediaFusion"), preset("stremthruTorz", "StremThru Torz")],
        "formatter": {"id": "torrentio"},
        "excludeUncached": true,
        "sortCriteria": {"global": sort},
    });
    if let Some(l) = language { config["preferredLanguages"] = json!([l.aio]); }
    config
}

fn random_password() -> Result<String> {
    let mut bytes = [0u8; 18];
    // The OS generator on every platform (Windows has no /dev/urandom).
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("Couldn't make a password for AIOStreams"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// AIOStreams answers `{success, data, error:{message}}`; its messages hold no secrets.
async fn aiostreams_call(method: reqwest::Method, body: Value, login: Option<(String, String)>) -> Result<Value> {
    on_runtime(async move {
        let mut req = backend::backend_http().request(method, format!("{AIOSTREAMS}/api/v1/user"))
            .timeout(std::time::Duration::from_secs(20))
            .header("Content-Type", "application/json").body(body.to_string());
        if let Some((user, password)) = login { req = req.basic_auth(user, Some(password)); }
        let resp = req.send().await.map_err(|_| anyhow::anyhow!("AIOStreams didn't answer"))?;
        let v: Value = serde_json::from_str(&resp.text().await.unwrap_or_default()).unwrap_or_default();
        if v["success"] != true {
            let message = v["error"]["message"].as_str().unwrap_or("its server refused the settings");
            bail!("AIOStreams: {}", message.chars().take(200).collect::<String>());
        }
        Ok(v["data"].clone())
    }).await?
}

/// Writes the AIOStreams configuration (updating the one Veylo made before, if it still
/// exists) and returns its install link with the login to keep.
async fn aiostreams_link(key: &str, language: Option<&Language>, previous: Option<&Value>) -> Result<(String, Value)> {
    let config = aiostreams_config(key, language);
    if let Some(prev) = previous {
        let (uuid, password, url) = (s(prev, "uuid"), s(prev, "password"), s(prev, "url"));
        if !uuid.is_empty() && !password.is_empty() && !url.is_empty()
            && aiostreams_call(reqwest::Method::PUT, json!({"config": config}), Some((uuid.clone(), password.clone()))).await.is_ok() {
            return Ok((url.clone(), json!({"url": url, "uuid": uuid, "password": password})));
        }
    }
    let password = random_password()?;
    let data = aiostreams_call(reqwest::Method::POST, json!({"config": config, "password": password}), None).await?;
    let (uuid, encrypted) = (s(&data, "uuid"), s(&data, "encryptedPassword"));
    ensure!(!uuid.is_empty() && !encrypted.is_empty(), "AIOStreams didn't return a configuration");
    let url = format!("{AIOSTREAMS}/stremio/{uuid}/{encrypted}/manifest.json");
    Ok((url.clone(), json!({"url": url, "uuid": uuid, "password": password})))
}

/// Install (or set up again) a curated add-on with the current key and language. Any other
/// copy of it (one installed from its website) is replaced.
pub async fn install(c: Curated) -> Result<()> {
    let key = rd_key()?;
    let settings = backend::load_settings();
    let language = settings.stream_language.as_deref().and_then(language);
    let mut store = load();
    // An AIOStreams login made by an install that then failed is reused first.
    let pending = format!("{}:pending", c.id());
    let previous = store.managed.get(&pending).or_else(|| store.managed.get(c.id())).cloned();
    let (url, record) = match c {
        Curated::Torrentio => { let url = torrentio_url(&key, language); (url.clone(), json!({"url": url})) }
        Curated::AioStreams => {
            let link = aiostreams_link(&key, language, previous.as_ref()).await?;
            // Keep the new login aside before installing: if the install fails, a retry updates
            // this configuration instead of making another, and the add-on still installed keeps
            // its record (so it stays Veylo's, and Remove key still finds it).
            store.managed.insert(pending.clone(), link.1.clone());
            save(&store);
            link
        }
    };
    let addon = exec(json!({"op":"install_addon","url":url})).await.map_err(|_| anyhow::anyhow!("{} couldn't be installed. Check your connection and try again.", c.name()))?;
    let new_url = s(&addon, "manifest_url");
    // Replace older copies: the previous Veylo link and any same-host install.
    for a in installed().await {
        let u = s(&a, "manifest_url");
        if u != new_url && host_of(&u) == c.host() {
            let _ = exec(json!({"op":"remove_addon","url":u})).await;
        }
    }
    let mut record = record;
    record["url"] = json!(new_url);
    store.managed.insert(c.id().into(), record);
    store.managed.remove(&pending);
    mark_seen(&mut store, &addon);
    save(&store);
    Ok(())
}

pub async fn remove(row: &Row) -> Result<()> {
    // The AIOStreams login stays in addons.json so a later install updates the same configuration.
    exec(json!({"op":"remove_addon","url":row.url})).await?;
    Ok(())
}

/// Apply a newer version: curated add-ons are set up again (so new defaults apply), others
/// fetch their manifest again.
pub async fn update(row: &Row) -> Result<()> {
    if let Some(c) = row.curated { return install(c).await; }
    let addon = exec(json!({"op":"install_addon","url":row.url})).await?;
    let mut store = load();
    mark_seen(&mut store, &addon);
    save(&store);
    Ok(())
}

/// After the key or language changes: set up every curated add-on that is installed again.
pub async fn reconfigure() -> Result<()> {
    let ov = overview().await;
    let mut first_error = None;
    for (c, row) in ov.curated {
        if row.is_some() { if let Err(e) = install(c).await { first_error.get_or_insert(e); } }
    }
    first_error.map_or(Ok(()), Err)
}

/// "Remove key": forget the Real-Debrid key, and take out the add-ons Veylo set up with it
/// (their links carry the key). Add-ons you installed yourself stay.
pub async fn remove_rd_key() -> Result<()> {
    // The add-ons go first: if one can't be removed, the key stays so Remove key can be tried again.
    for row in overview().await.curated.into_iter().filter_map(|(_, row)| row) {
        exec(json!({"op":"remove_addon","url":row.url})).await
            .map_err(|_| anyhow::anyhow!("{} couldn't be removed, so the key was kept. Try again.", row.name))?;
    }
    // Veylo's own records of those add-ons hold the key too (Torrentio's link, the AIOStreams
    // login of a configuration that has the key): delete the AIOStreams configuration on its
    // server where it still answers, and forget all of them.
    let mut store = load();
    for c in Curated::ALL {
        for record in [store.managed.remove(c.id()), store.managed.remove(&format!("{}:pending", c.id()))].into_iter().flatten() {
            let (uuid, password) = (s(&record, "uuid"), s(&record, "password"));
            if c == Curated::AioStreams && !uuid.is_empty() && !password.is_empty() {
                let _ = aiostreams_call(reqwest::Method::DELETE, json!({}), Some((uuid, password))).await;
            }
        }
    }
    save(&store);
    let settings = backend::Settings { rd_key: None, rd_key_removed: true, ..backend::load_settings() };
    backend::save_settings(&settings);
    Ok(())
}

/// Real-Debrid account check for a pasted key: "Premium until 2027-03-12".
pub async fn check_rd_key(key: String) -> Result<String> {
    on_runtime(async move {
        let resp = backend::backend_http().get("https://api.real-debrid.com/rest/1.0/user")
            .bearer_auth(&key).send().await.map_err(|_| anyhow::anyhow!("Real-Debrid didn't answer. Check your connection."))?;
        ensure!(resp.status().is_success(), "Real-Debrid didn't accept this key");
        let v: Value = serde_json::from_str(&resp.text().await.unwrap_or_default()).unwrap_or_default();
        let until = s(&v, "expiration").chars().take(10).collect::<String>();
        Ok(match s(&v, "type").as_str() {
            "premium" if !until.is_empty() => format!("Premium until {until}"),
            "premium" => "Premium".to_owned(),
            _ => "Free account: Real-Debrid streams need premium".to_owned(),
        })
    }).await?
}

/// At start: adopt the Real-Debrid key from a Torrentio link installed from its website,
/// replace website installs of curated add-ons with Veylo's own, then look for new versions.
pub async fn startup() -> usize {
    let mut settings = backend::load_settings();
    let addons = installed().await;
    // A key you removed in Settings is not taken back from an add-on's link.
    if settings.rd_key.is_none() && !settings.rd_key_removed {
        let key = addons.iter().map(|a| s(a, "manifest_url")).filter(|u| host_of(u) == Curated::Torrentio.host())
            .find_map(|u| u.split(['/', '|']).find_map(|p| p.strip_prefix("realdebrid=").map(str::to_owned)))
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()));
        if let Some(key) = key {
            settings.rd_key = Some(key);
            backend::save_settings(&settings);
        }
    }
    if settings.rd_key.is_some() {
        let store = load();
        for c in Curated::ALL {
            let managed = store.managed.get(c.id()).map(|m| s(m, "url"));
            let foreign = addons.iter().any(|a| { let u = s(a, "manifest_url"); host_of(&u) == c.host() && Some(&u) != managed.as_ref() });
            if foreign { let _ = install(c).await; }
        }
    }
    check_updates().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrentio_asks_for_everything_cached() {
        let url = torrentio_url("KEY", language("italian"));
        assert_eq!(url, "https://torrentio.strem.fun/language=italian|debridoptions=nodownloadlinks,nocatalog|realdebrid=KEY/manifest.json");
        assert!(!url.contains("qualityfilter") && !url.contains("limit"));
        // English is Torrentio's default order, not an option.
        assert_eq!(torrentio_url("KEY", language("english")), "https://torrentio.strem.fun/debridoptions=nodownloadlinks,nocatalog|realdebrid=KEY/manifest.json");
    }

    #[test]
    fn aiostreams_config_is_cached_only_without_filters() {
        let c = aiostreams_config("KEY", language("italian"));
        assert_eq!(c["excludeUncached"], true);
        assert_eq!(c["preferredLanguages"], json!(["Italian"]));
        assert_eq!(c["services"][0]["credentials"]["apiKey"], "KEY");
        assert!(c["presets"].as_array().unwrap().iter().all(|p| p["options"]["resources"] == json!(["stream"])));
        assert!(c.get("excludedResolutions").is_none() && c.get("resultLimits").is_none());
    }

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
}
