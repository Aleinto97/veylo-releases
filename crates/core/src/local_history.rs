use crate::db;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use url::Url;

pub(crate) fn local_manifest(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.path() == "/manifest.json"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "History migration requires the embedded addon manifest"
    );
    ensure!(
        url.query_pairs().all(|(key, value)| key == "v"
            && !value.is_empty()
            && value.bytes().all(|c| c.is_ascii_digit())),
        "Unsupported embedded addon revision"
    );
    Ok(url)
}

fn rebase(value: &mut Value, from: &Url, to: &Url) {
    match value {
        Value::String(text) => {
            if text == from.as_str() {
                *text = to.to_string();
            } else if let Some(path) =
                text.strip_prefix(&format!("{}/", from.origin().ascii_serialization()))
            {
                *text = format!("{}/{path}", to.origin().ascii_serialization());
            }
        }
        Value::Object(object) => object.values_mut().for_each(|v| rebase(v, from, to)),
        Value::Array(array) => array.iter_mut().for_each(|v| rebase(v, from, to)),
        _ => {}
    }
}

/// Only an explicit embedded-addon upgrade can change an opaque history identity.
pub(crate) fn migrate(
    db: &mut Connection,
    kind: &str,
    id: &str,
    from: &str,
    to: &str,
    meta: &Value,
) -> Result<Value> {
    let from_url = local_manifest(from)?;
    let to_url = local_manifest(to)?;
    ensure!(from != to, "History is already on this addon");
    ensure!(
        meta.is_object() && meta["id"] == id && meta.get("type").is_none_or(|value| value == kind),
        "Metadata identity changed"
    );
    let old_key = db::identity(kind, id, from);
    let new_key = db::identity(kind, id, to);
    let tx = db.transaction()?;
    let Some(source) = db::media_item(&tx, &old_key)? else {
        let item = db::media_item(&tx, &new_key)?.context("History item missing")?;
        ensure!(item.addon == to, "History source changed");
        return Ok(json!({"migrated":false,"item":item.value()}));
    };
    ensure!(
        source.addon == from || (old_key == new_key && source.addon == to),
        "History source changed"
    );
    let target = db::media_item(&tx, &new_key)?;
    let timestamp = |key: &str| -> Result<i64> {
        Ok(
            tx.query_row("SELECT updated FROM media WHERE key=?1", [key], |r| {
                r.get(0)
            })?,
        )
    };
    let source_time = timestamp(&old_key)?;
    let target_time = target
        .as_ref()
        .map(|_| timestamp(&new_key))
        .transpose()?
        .unwrap_or(0);
    // The old artwork refresh also created saved-only duplicates; they must not
    // overwrite the original preferences. Actual destination activity wins by recency.
    let target_has_activity = target.as_ref().is_some_and(|item| {
        item.watched || item.position > 0 || item.watched_ms > 0 || item.liked != 0
    });
    let winner = target
        .as_ref()
        .filter(|_| target_has_activity && target_time >= source_time)
        .unwrap_or(&source);
    let mut metadata = source.meta.clone();
    rebase(&mut metadata, &from_url, &to_url);
    for (key, value) in meta.as_object().unwrap() {
        if !value.is_null() {
            metadata[key] = value.clone();
        }
    }
    db::upsert_meta(&tx, kind, id, to, &metadata)?;
    if old_key != new_key {
        tx.execute(
            "INSERT INTO playback(media_key,video_id,position_ms,duration_ms,watched_ms,updated,watched)
             SELECT ?2,video_id,position_ms,duration_ms,watched_ms,updated,watched FROM playback WHERE media_key=?1
             ON CONFLICT(media_key,video_id) DO UPDATE SET
             position_ms=CASE WHEN excluded.updated>playback.updated THEN excluded.position_ms ELSE playback.position_ms END,
             duration_ms=CASE WHEN excluded.updated>playback.updated THEN excluded.duration_ms ELSE playback.duration_ms END,
             watched_ms=CASE
               WHEN excluded.updated>playback.updated AND excluded.watched=0 AND excluded.watched_ms=0 THEN 0
               WHEN playback.updated>excluded.updated AND playback.watched=0 AND playback.watched_ms=0 THEN 0
               ELSE MAX(playback.watched_ms,excluded.watched_ms)
             END,
             watched=CASE
               WHEN excluded.updated>playback.updated AND excluded.watched=0 AND excluded.watched_ms=0 THEN 0
               WHEN playback.updated>excluded.updated AND playback.watched=0 AND playback.watched_ms=0 THEN 0
               ELSE MAX(playback.watched,excluded.watched)
             END,
             updated=MAX(playback.updated,excluded.updated)",
            params![old_key,new_key],
        )?;
    }
    let latest: Option<(String, i64, i64)> = tx
        .query_row(
            "SELECT video_id,position_ms,duration_ms FROM playback WHERE media_key=?1
         ORDER BY updated DESC,video_id=?2 DESC,video_id LIMIT 1",
            params![new_key, winner.video_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (video, position, duration) = latest.map(|(video, p, d)| (Some(video), p, d)).unwrap_or((
        winner.video_id.clone(),
        winner.position,
        winner.duration,
    ));
    let watched_ms: i64 = tx.query_row(
        "SELECT COALESCE(SUM(watched_ms),?2) FROM playback WHERE media_key=?1",
        params![new_key, winner.watched_ms],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE media SET addon_url=?2,saved=?3,watched=?4,liked=?5,video_id=?6,
         position_ms=?7,duration_ms=?8,watched_ms=?9,updated=?10 WHERE key=?1",
        params![
            new_key,
            to,
            winner.saved,
            winner.watched,
            winner.liked,
            video,
            position,
            duration,
            watched_ms,
            source_time.max(if target_has_activity { target_time } else { 0 })
        ],
    )?;
    if old_key != new_key {
        tx.execute("DELETE FROM media WHERE key=?1", [&old_key])?;
    }
    let item = db::media_item(&tx, &new_key)?.context("Migrated history missing")?;
    tx.commit()?;
    Ok(json!({"migrated":true,"item":item.value()}))
}
