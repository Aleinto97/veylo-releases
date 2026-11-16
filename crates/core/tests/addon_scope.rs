//! Retained embedded-addon registrations are history identities only: they must
//! never become network targets once another loopback port or revision runs.
mod common;
use common::Data;
use rustmedia_core::Core;
use serde_json::{json, Value};

/// The embedded metadata addon with a search catalog and stream resource.
fn embedded() -> Value {
    json!({"id":"com.aio.metadata.rs","name":"Embedded metadata","version":"1",
        "types":["movie"],"resources":["catalog","stream"],
        "catalogs":[{"type":"movie","id":"search","name":"Search",
            "extra":[{"name":"search","isRequired":true}]}]})
}

fn local_urls(value: &Value, field: &str) -> Vec<String> {
    value[field]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["addon_url"].as_str())
        .filter(|url| url.starts_with("http://127.0.0.1:"))
        .map(str::to_owned)
        .collect()
}
async fn search_locals(core: &Core) -> Vec<String> {
    let plan = core
        .execute(json!({"op":"search_targets","query":"matrix"}))
        .await
        .unwrap();
    local_urls(&plan, "targets")
}
async fn stream_locals(core: &Core, addon: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut request = json!({"op":"streams_targets","type":"movie","id":"tt0133093"});
    if let Some(addon) = addon {
        request["addon_url"] = json!(addon);
    }
    Ok(local_urls(&core.execute(request).await?, "targets"))
}

const OLD: &str = "http://127.0.0.1:7099/manifest.json?v=6";
const RUNNING: &str = "http://127.0.0.1:7201/manifest.json?v=6";

#[tokio::test]
async fn foreign_port_registration_is_never_a_target() {
    let data = Data::new();
    let core = data.core();
    data.install(OLD, &embedded());
    data.install(RUNNING, &embedded());
    core.set_local_addon(Some("http://127.0.0.1:7201/manifest.json"));
    assert_eq!(search_locals(&core).await, [RUNNING]);
    assert_eq!(stream_locals(&core, None).await.unwrap(), [RUNNING]);
    // An explicit request for the retired identity is answered by the running addon.
    assert_eq!(stream_locals(&core, Some(OLD)).await.unwrap(), [RUNNING]);
    // The registration itself stays installed for history cleanup/migration.
    let installed = core.execute(json!({"op":"addons"})).await.unwrap();
    assert!(installed["addons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["manifest_url"] == OLD));
}

#[tokio::test]
async fn only_newest_revision_on_running_port_is_queried() {
    let data = Data::new();
    let core = data.core();
    data.install("http://127.0.0.1:7201/manifest.json?v=5", &embedded());
    data.install(RUNNING, &embedded());
    core.set_local_addon(Some("http://127.0.0.1:7201/manifest.json"));
    assert_eq!(search_locals(&core).await, [RUNNING]);
}

#[tokio::test]
async fn stopped_embedded_addon_is_not_contacted() {
    let data = Data::new();
    let core = data.core();
    data.install(OLD, &embedded());
    core.set_local_addon(None);
    assert!(search_locals(&core).await.is_empty());
    assert!(stream_locals(&core, None).await.unwrap().is_empty());
    let error = stream_locals(&core, Some(OLD)).await.unwrap_err();
    assert!(error.to_string().contains("not running"), "{error}");
}

#[tokio::test]
async fn standalone_core_keeps_legacy_behaviour() {
    let data = Data::new();
    let core = data.core();
    data.install(OLD, &embedded());
    assert_eq!(search_locals(&core).await, [OLD]);
}

#[tokio::test]
async fn purge_removes_only_matching_cached_responses() {
    let data = Data::new();
    let core = data.core();
    {
        let db = data.db();
        for key in [
            "http://127.0.0.1:7201/catalog/movie/top.json",
            "http://127.0.0.1:7201/meta/movie/tt1.json",
            "http://127.0.0.1:72011/meta/movie/tt1.json",
            "https://v3-cinemeta.strem.io/catalog/movie/top.json",
        ] {
            db.execute(
                "INSERT INTO cache(key,body,expires,updated) VALUES(?1,'{}',9999999999,0)",
                [key],
            )
            .unwrap();
        }
    }
    assert_eq!(
        core.purge_cached_responses("http://127.0.0.1:7201/")
            .unwrap(),
        2
    );
    // LIKE wildcards in the prefix are literal.
    assert_eq!(
        core.purge_cached_responses("https://v3_cinemeta").unwrap(),
        0
    );
    let left: i64 = data
        .db()
        .query_row("SELECT COUNT(*) FROM cache", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 2);
}
