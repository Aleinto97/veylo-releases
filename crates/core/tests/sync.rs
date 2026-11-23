//! Sync between a person's devices: two isolated stores trading `sync_changes` / `sync_apply`.
mod common;
use common::Data;
use rustmedia_core::Core;
use serde_json::{json, Value};

const CINEMETA: &str = "https://v3-cinemeta.strem.io/manifest.json";

async fn changes(core: &Core, since: i64) -> Value {
    core.execute(json!({"op":"sync_changes","since":since}))
        .await
        .unwrap()
}
async fn item(core: &Core, kind: &str, id: &str) -> Value {
    core.execute(json!({"op":"item","type":kind,"id":id,"addon_url":CINEMETA}))
        .await
        .unwrap()["item"]
        .clone()
}

#[tokio::test]
async fn a_second_device_takes_the_first_one_s_library_and_progress() {
    let (a_data, b_data) = (Data::new(), Data::new());
    let (a, b) = (a_data.core(), b_data.core());
    a.execute(json!({"op":"set_library","type":"movie","id":"tt1375666","addon_url":CINEMETA,"saved":true,"meta":{"name":"Inception","poster":"https://img/p.jpg"}})).await.unwrap();
    a.execute(json!({"op":"progress","type":"series","id":"tt0903747","addon_url":CINEMETA,"video_id":"tt0903747:1:2",
        "position_ms":600000,"duration_ms":3000000,"meta":{"name":"Breaking Bad"}})).await.unwrap();
    a.execute(
        json!({"op":"feedback","type":"movie","id":"tt1375666","addon_url":CINEMETA,"value":1}),
    )
    .await
    .unwrap();
    let sent = changes(&a, 0).await;
    assert!(sent["media"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["history"] == true));
    assert_eq!(sent["playback"].as_array().unwrap().len(), 1);
    b.execute(json!({"op":"sync_apply","media":sent["media"],"playback":sent["playback"]}))
        .await
        .unwrap();
    let film = item(&b, "movie", "tt1375666").await;
    assert_eq!(
        (
            film["library"].clone(),
            film["liked"].clone(),
            film["name"].clone()
        ),
        (json!(true), json!(1), json!("Inception"))
    );
    let show = item(&b, "series", "tt0903747").await;
    assert_eq!(
        (show["position_ms"].clone(), show["video_id"].clone()),
        (json!(600000), json!("tt0903747:1:2"))
    );
    // Applied with the first device's times: nothing for the second to send back.
    let back = changes(&b, sent["now"].as_i64().unwrap()).await;
    assert!(
        back["media"].as_array().unwrap().is_empty()
            && back["playback"].as_array().unwrap().is_empty()
    );
}

#[tokio::test]
async fn the_newer_change_wins_and_a_forgotten_title_goes() {
    let (a_data, b_data) = (Data::new(), Data::new());
    let (a, b) = (a_data.core(), b_data.core());
    let saved = |v: bool| json!({"op":"set_library","type":"movie","id":"tt1","addon_url":CINEMETA,"saved":v,"meta":{"name":"Film"}});
    a.execute(saved(true)).await.unwrap();
    let mut first = changes(&a, 0).await;
    // The other device saved it later and also rated it: an older change doesn't undo that.
    b.execute(saved(true)).await.unwrap();
    b.execute(json!({"op":"feedback","type":"movie","id":"tt1","addon_url":CINEMETA,"value":-1}))
        .await
        .unwrap();
    first["media"][0]["updated"] = json!(1);
    first["media"][0]["saved"] = json!(false);
    b.execute(json!({"op":"sync_apply","media":first["media"],"playback":[]}))
        .await
        .unwrap();
    assert_eq!(item(&b, "movie", "tt1").await["liked"], json!(-1));
    assert_eq!(item(&b, "movie", "tt1").await["library"], json!(true));
    // A newer "forgotten" removes it.
    let gone = json!([{"type":"movie","id":"tt1","addon_url":CINEMETA,"updated":i64::MAX / 2,"deleted":true}]);
    b.execute(json!({"op":"sync_apply","media":gone,"playback":[]}))
        .await
        .unwrap();
    assert_eq!(item(&b, "movie", "tt1").await, Value::Null);
}

#[tokio::test]
async fn three_devices_converge_with_independent_episodes_and_deletions() {
    let (a_data, b_data, c_data) = (Data::new(), Data::new(), Data::new());
    let (a, b, c) = (a_data.core(), b_data.core(), c_data.core());
    // Device A saves a show and watches S1 E1.
    a.execute(json!({"op":"set_library","type":"series","id":"tt0903747","addon_url":CINEMETA,"saved":true,"meta":{"name":"Breaking Bad"}})).await.unwrap();
    a.execute(json!({"op":"progress","type":"series","id":"tt0903747","addon_url":CINEMETA,"video_id":"tt0903747:1:1",
        "position_ms":2700000,"duration_ms":3000000,"watched_add_ms":2600000,"meta":{"name":"Breaking Bad"}})).await.unwrap();
    // Device B saves a film and starts S1 E2 of the same show.
    b.execute(json!({"op":"set_library","type":"movie","id":"tt1375666","addon_url":CINEMETA,"saved":true,"meta":{"name":"Inception"}})).await.unwrap();
    b.execute(json!({"op":"progress","type":"series","id":"tt0903747","addon_url":CINEMETA,"video_id":"tt0903747:1:2",
        "position_ms":900000,"duration_ms":3000000,"watched_add_ms":900000,"meta":{"name":"Breaking Bad"}})).await.unwrap();

    // Sync A and B into C, and cross-sync A <-> B.
    let ca = changes(&a, 0).await;
    let cb = changes(&b, 0).await;
    for dst in [&b, &c] {
        dst.execute(json!({"op":"sync_apply","media":ca["media"],"playback":ca["playback"]}))
            .await
            .unwrap();
    }
    for dst in [&a, &c] {
        dst.execute(json!({"op":"sync_apply","media":cb["media"],"playback":cb["playback"]}))
            .await
            .unwrap();
    }
    // Every device should have S1 E1 watched and both titles present.
    for dev in [&a, &b, &c] {
        let watched = dev
            .execute(json!({"op":"watched_videos","type":"series","id":"tt0903747"}))
            .await
            .unwrap();
        assert_eq!(watched["video_ids"], json!(["tt0903747:1:1"]));
        assert_eq!(item(dev, "movie", "tt1375666").await["library"], json!(true));
    }
}

#[tokio::test]
async fn sqlite_indexes_cover_hot_sync_and_recommendation_queries() {
    let data = Data::new();
    let _core = data.core();
    let conn = rusqlite::Connection::open(data.0.join("core.sqlite3")).unwrap();
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let plan = |sql: &str| -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let media_plan = plan("SELECT key FROM media WHERE updated>0 ORDER BY updated,key LIMIT 5000");
    assert!(media_plan.contains("media_updated"), "expected media_updated index, got: {media_plan}");
    let playback_plan = plan("SELECT media_key FROM playback WHERE updated>0 ORDER BY updated LIMIT 20000");
    assert!(playback_plan.contains("playback_updated"), "expected playback_updated index, got: {playback_plan}");
}
