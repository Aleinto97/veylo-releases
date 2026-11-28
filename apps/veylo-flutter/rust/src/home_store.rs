//! Home's last successful catalog and hero responses. Only the local add-on registry
//! supplies routing URLs, and only keyless public artwork is stored here.

use crate::backend;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, LazyLock};

fn path() -> PathBuf {
    backend::backend_data_dir().join("home-cache-v2.json")
}

fn hash(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

fn read() -> Value {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|doc| doc["shelves"].is_object() && doc["artwork"].is_object())
        .unwrap_or_else(|| json!({ "shelves": {}, "artwork": {} }))
}

fn public_image(value: &Value) -> bool {
    let Some(url) = value.as_str().and_then(|s| url::Url::parse(s).ok()) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(
            url.host_str(),
            Some(
                "btttr.cc"
                    | "images.metahub.space"
                    | "image.tmdb.org"
                    | "static.tvmaze.com"
                    | "images.justwatch.com"
                    | "artworks.thetvdb.com"
                    | "cdn.myanimelist.net"
                    | "s4.anilist.co"
                    | "media.kitsu.io"
                    | "upload.wikimedia.org"
            )
        )
}

fn safe_text(value: &mut Value) {
    match value {
        Value::String(text) => *text = backend::redact_diagnostic(text),
        Value::Array(items) => items.iter_mut().for_each(safe_text),
        _ => {}
    }
}

fn store_media(items: &Value) -> Vec<Value> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let mut item = item.as_object()?.clone();
            if ["id", "type"].iter().any(|field| {
                item.get(*field)
                    .and_then(Value::as_str)
                    .is_some_and(|value| backend::redact_diagnostic(value) != value)
            }) {
                return None;
            }
            let key = hash(item.remove("key")?.as_str()?);
            let origin = hash(item.remove("addon_url")?.as_str()?);
            item.remove("raw");
            for field in [
                "name",
                "description",
                "year",
                "genres",
                "reason",
                "episode_label",
                "resume_context",
                "meta_line",
                "meta_line_typed",
                "runtime",
                "rating",
                "last_video_id",
            ] {
                if let Some(value) = item.get_mut(field) {
                    safe_text(value);
                }
            }
            item.insert("key".into(), json!(key));
            item.insert("origin".into(), json!(origin));
            for field in ["poster", "background", "backdrop", "logo"] {
                if item.get(field).is_some_and(|image| !public_image(image)) {
                    item.remove(field);
                }
            }
            if let Some(posters) = item.get_mut("posters").and_then(Value::as_array_mut) {
                posters.retain(public_image);
            }
            Some(Value::Object(item))
        })
        .collect()
}

fn restore_media(items: &Value, origins: &HashMap<String, &str>) -> Vec<Value> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let origin = origins.get(item["origin"].as_str()?)?;
            let key = format!(
                "{}|{}|{origin}",
                item["type"].as_str()?,
                item["id"].as_str()?
            );
            let mut item = item.as_object()?.clone();
            item.remove("origin");
            item.insert("key".into(), json!(key));
            item.insert("addon_url".into(), json!(origin));
            Some(Value::Object(item))
        })
        .collect()
}

pub async fn snapshot() -> anyhow::Result<Value> {
    let (rows, library, addons) = tokio::try_join!(
        backend::home_rows(),
        backend::library(),
        backend::exec(json!({ "op": "addons" })),
    )?;
    let disk = read();
    // Home rows are generated from the installed manifests already on disk. Reattach
    // their live routing fields without storing a manifest URL or catalog argument here.
    let rows_by_hash: HashMap<_, _> = rows.iter().map(|row| (hash(&row.key), row)).collect();
    let origins: HashMap<_, _> = rows
        .iter()
        .map(|row| row.addon_url.as_str())
        .chain(
            addons["addons"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|addon| addon["manifest_url"].as_str()),
        )
        .map(|origin| (hash(origin), origin))
        .collect();
    let mut shelves = Map::new();
    let mut cached_rows = Vec::new();
    for cached in disk["rows"].as_array().into_iter().flatten() {
        let Some(key) = cached["key"].as_str() else {
            continue;
        };
        let Some(row) = rows_by_hash.get(key) else {
            continue;
        };
        let mut row_json = crate::catalog_row_json(row);
        row_json["title"] = cached["title"].clone();
        row_json["subtitle"] = cached["subtitle"].clone();
        cached_rows.push(row_json);
        if let Some(items) = disk["shelves"].get(key) {
            shelves.insert(row.key.clone(), json!(restore_media(items, &origins)));
        }
    }
    let tonight = restore_media(&disk["tonight"], &origins);
    let artwork: Map<_, _> = tonight
        .iter()
        .filter_map(|item| {
            let key = item["key"].as_str()?;
            Some((key.to_owned(), disk["artwork"].get(hash(key))?.clone()))
        })
        .collect();
    Ok(json!({
        "rows": cached_rows,
        "shelves": shelves,
        "tonight": tonight,
        "artwork": artwork,
        "library": library.iter().map(crate::media_to_json).collect::<Vec<_>>(),
    }))
}

enum Update {
    Rows(Vec<Value>),
    Shelf(String, Vec<Value>),
    Tonight(Vec<Value>),
    Artwork(String, Value),
}

// The worker applies operations in queue order and coalesces results that arrive together.
// The file is replaced only after the new document has been written in full.
static WRITER: LazyLock<mpsc::Sender<Update>> = LazyLock::new(|| {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let path = path();
        let mut disk = read();
        for (seq, update) in (0..).zip(receive.iter()) {
            apply(&mut disk, update);
            for next in receive.try_iter() {
                apply(&mut disk, next);
            }
            let tmp = path.with_extension(format!("{}.{seq}.tmp", std::process::id()));
            if std::fs::write(&tmp, disk.to_string())
                .and_then(|()| std::fs::rename(&tmp, &path))
                .is_err()
            {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    });
    send
});

fn apply(disk: &mut Value, update: Update) {
    match update {
        Update::Rows(rows) => {
            if let Some(shelves) = disk["shelves"].as_object_mut() {
                shelves.retain(|key, _| rows.iter().any(|row| row["key"] == *key));
            }
            disk["rows"] = json!(rows);
        }
        Update::Shelf(key, items) => disk["shelves"][key] = json!(items),
        Update::Tonight(items) => {
            if let Some(art) = disk["artwork"].as_object_mut() {
                art.retain(|key, _| items.iter().any(|item| item["key"] == *key));
            }
            disk["tonight"] = json!(items);
        }
        Update::Artwork(key, art) => {
            if disk["tonight"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["key"] == key))
            {
                disk["artwork"][key] = art;
            }
        }
    }
}

pub fn put(op: &str, key: Option<&str>, data: &Value) {
    let update = match op {
        "home_rows" => Update::Rows(
            data["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| {
                    let mut title = row["title"].clone();
                    let mut subtitle = row["subtitle"].clone();
                    safe_text(&mut title);
                    safe_text(&mut subtitle);
                    Some(json!({ "key": hash(row["key"].as_str()?), "title": title, "subtitle": subtitle }))
                })
                .collect(),
        ),
        "catalog" => {
            let Some(key) = key else { return };
            Update::Shelf(hash(key), store_media(&data["items"]))
        }
        "tonight_picks" => Update::Tonight(store_media(&data["items"])),
        "artwork" => {
            let Some(key) = key else { return };
            let Some(source) = data["artwork"].as_object() else {
                return;
            };
            let mut art = Map::new();
            for name in ["backdrop", "logo"] {
                if let Some(image) = source.get(name).filter(|image| public_image(image)) {
                    art.insert(name.into(), image.clone());
                }
            }
            if let Some(tagline) = source.get("tagline").filter(|value| value.is_string()) {
                let mut tagline = tagline.clone();
                safe_text(&mut tagline);
                art.insert("tagline".into(), tagline);
            }
            if art.is_empty() {
                return;
            }
            Update::Artwork(hash(key), Value::Object(art))
        }
        _ => return,
    };
    let _ = WRITER.send(update);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_media_keeps_public_art_and_recovers_private_routing_from_registry() {
        let origin = "https://addon.example/token=private-example/manifest.json";
        let key = format!("movie|tt1234567|{origin}");
        let public = "https://image.tmdb.org/t/p/w1280/example.jpg";
        let private = "https://image.tmdb.org/t/p/w1280/example.jpg?token=private-example";
        let data = json!([{
            "key": key, "addon_url": origin, "id": "tt1234567", "type": "movie",
            "name": "A title", "description": format!("See {origin}"),
            "background": private, "posters": [public, private],
            "raw": { "video": { "url": origin } },
        }]);

        let stored = json!(store_media(&data));
        assert!(!stored.to_string().contains("private-example"));
        assert!(stored[0].get("raw").is_none());
        assert_eq!(stored[0]["key"], hash(&key));
        assert_eq!(stored[0]["posters"], json!([public]));
        assert!(restore_media(&stored, &HashMap::new()).is_empty());

        let restored = restore_media(&stored, &HashMap::from([(hash(origin), origin)]));
        assert_eq!(restored[0]["key"], key);
        assert_eq!(restored[0]["addon_url"], origin);
    }
}
