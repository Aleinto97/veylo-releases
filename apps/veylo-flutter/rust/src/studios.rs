//! Fast, keyless, free studio idents for the title page.
//!
//! Studios (production company P272 / original network P449, with their P154 logo on Wikimedia
//! Commons) used to come only from the big Wikidata SPARQL query, which takes 0.3–5 s. Here they
//! come from Wikidata's regular API instead — served from its edge caches, three tiny requests,
//! ~0.5 s cold — and are kept on disk for good: a film's studios never change, and companies
//! recur across films (a new Warner Bros. film only needs the first two requests).

use crate::backend::{self, Studio};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const API: &str = "https://www.wikidata.org/w/api.php";
const MAX: usize = 4;

#[derive(Default)]
struct Cache {
    /// imdb id -> studios with logos (empty = Wikidata knows none).
    titles: HashMap<String, Vec<(String, String)>>,
    /// company QID -> (name, Commons logo URL if any).
    companies: HashMap<String, (String, Option<String>)>,
    loaded: bool,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn path() -> std::path::PathBuf { backend::backend_data_dir().join("studios-cache.json") }

fn load(c: &mut Cache) {
    if c.loaded { return; }
    c.loaded = true;
    let Ok(text) = std::fs::read_to_string(path()) else { return };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
    if let Some(t) = v["titles"].as_object() {
        for (k, list) in t {
            let studios = list.as_array().into_iter().flatten()
                .filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_str()?.to_owned())))
                .collect();
            c.titles.insert(k.clone(), studios);
        }
    }
    if let Some(t) = v["companies"].as_object() {
        for (k, p) in t {
            if let Some(name) = p[0].as_str() {
                c.companies.insert(k.clone(), (name.to_owned(), p[1].as_str().map(str::to_owned)));
            }
        }
    }
}

fn save(c: &Cache) {
    // Bounded: the newest few thousand titles are plenty for a session cache on disk.
    let titles: serde_json::Map<String, Value> = c.titles.iter().take(4000)
        .map(|(k, v)| (k.clone(), json!(v.iter().map(|(n, l)| [n, l]).collect::<Vec<_>>()))).collect();
    let companies: serde_json::Map<String, Value> = c.companies.iter().take(4000)
        .map(|(k, (n, l))| (k.clone(), json!([n, l]))).collect();
    let text = json!({ "titles": titles, "companies": companies }).to_string();
    let p = path();
    static SAVE: Mutex<u64> = Mutex::new(0);
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        // One writer; an older snapshot never replaces a newer one.
        let Ok(mut last) = SAVE.lock() else { return };
        if *last > seq { return; }
        *last = seq;
        let tmp = p.with_extension(format!("{}.{seq}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() { let _ = std::fs::rename(&tmp, &p); } else { let _ = std::fs::remove_file(&tmp); }
    });
}

/// Same shape as the SPARQL path's logos, so the logo-check verdicts already cached are reused.
fn commons_url(file: &str) -> String {
    let mut enc = String::with_capacity(file.len() * 3);
    for b in file.replace(' ', "_").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { enc.push(b as char) } else { enc.push_str(&format!("%{b:02X}")) }
    }
    format!("https://commons.wikimedia.org/wiki/Special:FilePath/{enc}?width=360")
}

async fn get(params: Vec<(&'static str, String)>) -> Option<Value> {
    let resp = backend::backend_http().get(API).query(&params).query(&[("format", "json"), ("formatversion", "2")])
        .timeout(Duration::from_secs(5)).send().await.ok()?.error_for_status().ok()?;
    resp.json::<Value>().await.ok()
}

fn claim_ids(v: &Value, prop: &str) -> Vec<String> {
    v["claims"][prop].as_array().into_iter().flatten()
        .filter_map(|c| c["mainsnak"]["datavalue"]["value"]["id"].as_str().map(str::to_owned))
        .collect()
}

/// The title's studios with logos, from the disk cache or Wikidata's API (keyless).
pub async fn studios(imdb: &str) -> Option<Vec<Studio>> {
    if !imdb.starts_with("tt") || !imdb[2..].bytes().all(|b| b.is_ascii_digit()) { return None; }
    {
        let mut c = cache().lock().ok()?;
        load(&mut c);
        if let Some(hit) = c.titles.get(imdb) {
            return Some(hit.iter().map(|(name, logo)| Studio { name: name.clone(), logo: logo.clone() }).collect());
        }
    }
    // 1. IMDb id -> Wikidata item (search index, ~0.2 s).
    let search = format!("haswbstatement:P345={imdb}");
    let found = get(vec![("action", "query".to_string()), ("list", "search".to_string()), ("srsearch", search.to_string()), ("srlimit", "1".to_string())]).await?;
    let Some(qid) = found["query"]["search"][0]["title"].as_str().map(str::to_owned) else {
        // Not on Wikidata (yet): remembered for this session only, so a later edit is picked up.
        if let Ok(mut c) = cache().lock() { c.titles.insert(imdb.to_owned(), Vec::new()); }
        return Some(Vec::new());
    };
    // 2. Production companies and original networks, in parallel.
    let (p272, p449) = tokio::join!(
        get(vec![("action", "wbgetclaims".to_string()), ("entity", qid.to_string()), ("property", "P272".to_string())]),
        get(vec![("action", "wbgetclaims".to_string()), ("entity", qid.to_string()), ("property", "P449".to_string())]),
    );
    // A failed request (or an API error answer) is "unknown", never "no studios".
    let ok = |v: &Option<Value>| v.as_ref().is_some_and(|v| v.get("error").is_none() && v.get("claims").is_some());
    if !ok(&p272) || !ok(&p449) { return None; }
    let mut ids: Vec<String> = Vec::new();
    for id in p272.as_ref().map(|v| claim_ids(v, "P272")).unwrap_or_default().into_iter()
        .chain(p449.as_ref().map(|v| claim_ids(v, "P449")).unwrap_or_default())
    {
        if !ids.contains(&id) { ids.push(id); }
    }
    ids.truncate(8);
    // 3. Names and logos of the companies not seen before (one request for names, one per logo).
    let unknown: Vec<String> = {
        let c = cache().lock().ok()?;
        ids.iter().filter(|q| !c.companies.contains_key(*q)).cloned().collect()
    };
    if !unknown.is_empty() {
        let joined = unknown.join("|");
        let names_fut = get(vec![("action", "wbgetentities".to_string()), ("ids", joined.to_string()), ("props", "labels".to_string()), ("languages", "en|mul".to_string())]);
        let logos_fut = futures_join_all(unknown.iter().map(|q| {
            let q = q.clone();
            async move {
                let v = get(vec![("action", "wbgetclaims".to_string()), ("entity", q.to_string()), ("property", "P154".to_string())]).await;
                // None = the request failed (retry later); Some(None) = the company has no logo.
                let file = v.filter(|v| v.get("claims").is_some())
                    .map(|v| v["claims"]["P154"][0]["mainsnak"]["datavalue"]["value"].as_str().map(str::to_owned));
                (q, file)
            }
        }).collect());
        let (names, logos) = tokio::join!(names_fut, logos_fut);
        if let Ok(mut c) = cache().lock() {
            for (q, file) in logos {
                let Some(file) = file else { continue };
                let ent = names.as_ref().map(|n| &n["entities"][&q]);
                let name = ent.and_then(|e| e["labels"]["en"]["value"].as_str().or_else(|| e["labels"]["mul"]["value"].as_str()))
                    .map(str::to_owned);
                // Only remember a company we could name (a failed request retries next time).
                if let Some(name) = name { c.companies.insert(q, (name, file.as_deref().map(commons_url))); }
            }
        }
    }
    let mut c = cache().lock().ok()?;
    let studios: Vec<(String, String)> = ids.iter()
        .filter_map(|q| c.companies.get(q).and_then(|(n, l)| Some((n.clone(), l.clone()?))))
        .fold(Vec::new(), |mut acc, s| { if acc.len() < MAX && !acc.iter().any(|(n, _)| *n == s.0) { acc.push(s) } acc });
    // Cache the title only once every company resolved, so a transient failure isn't kept.
    if ids.iter().all(|q| c.companies.contains_key(q)) {
        c.titles.insert(imdb.to_owned(), studios.clone());
    }
    save(&c);
    Some(studios.into_iter().map(|(name, logo)| Studio { name, logo }).collect())
}

/// Minimal join_all on tokio (no extra crate).
async fn futures_join_all<F: std::future::Future + Send + 'static>(futs: Vec<F>) -> Vec<F::Output>
where F::Output: Send + 'static {
    let handles: Vec<_> = futs.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles { if let Ok(v) = h.await { out.push(v); } }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commons_url_matches_wikidata_encoding() {
        assert_eq!(commons_url("Warner Bros. logo.svg"),
            "https://commons.wikimedia.org/wiki/Special:FilePath/Warner_Bros._logo.svg?width=360");
    }
}
