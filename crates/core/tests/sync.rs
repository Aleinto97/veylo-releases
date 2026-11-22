//! Sync between a person's devices: two isolated stores trading `sync_changes` / `sync_apply`.
mod common;
use common::Data;
use rustmedia_core::Core;
use serde_json::{json, Value};

const CINEMETA: &str = "https://v3-cinemeta.strem.io/manifest.json";

async fn changes(core: &Core, since: i64) -> Value { core.execute(json!({"op":"sync_changes","since":since})).await.unwrap() }
async fn item(core: &Core, kind: &str, id: &str) -> Value {
    core.execute(json!({"op":"item","type":kind,"id":id,"addon_url":CINEMETA})).await.unwrap()["item"].clone()
}

#[tokio::test]
async fn a_second_device_takes_the_first_one_s_library_and_progress() {
    let (a_data, b_data) = (Data::new(), Data::new());
    let (a, b) = (a_data.core(), b_data.core());
    a.execute(json!({"op":"set_library","type":"movie","id":"tt1375666","addon_url":CINEMETA,"saved":true,"meta":{"name":"Inception","poster":"https://img/p.jpg"}})).await.unwrap();
    a.execute(json!({"op":"progress","type":"series","id":"tt0903747","addon_url":CINEMETA,"video_id":"tt0903747:1:2",
        "position_ms":600000,"duration_ms":3000000,"meta":{"name":"Breaking Bad"}})).await.unwrap();
    a.execute(json!({"op":"feedback","type":"movie","id":"tt1375666","addon_url":CINEMETA,"value":1})).await.unwrap();
    let sent = changes(&a, 0).await;
    assert!(sent["media"].as_array().unwrap().iter().all(|m| m["history"] == true));
    assert_eq!(sent["playback"].as_array().unwrap().len(), 1);
    b.execute(json!({"op":"sync_apply","media":sent["media"],"playback":sent["playback"]})).await.unwrap();
    let film = item(&b, "movie", "tt1375666").await;
    assert_eq!((film["library"].clone(), film["liked"].clone(), film["name"].clone()), (json!(true), json!(1), json!("Inception")));
    let show = item(&b, "series", "tt0903747").await;
    assert_eq!((show["position_ms"].clone(), show["video_id"].clone()), (json!(600000), json!("tt0903747:1:2")));
    // Applied with the first device's times: nothing for the second to send back.
    let back = changes(&b, sent["now"].as_i64().unwrap()).await;
    assert!(back["media"].as_array().unwrap().is_empty() && back["playback"].as_array().unwrap().is_empty());
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
    b.execute(json!({"op":"feedback","type":"movie","id":"tt1","addon_url":CINEMETA,"value":-1})).await.unwrap();
    first["media"][0]["updated"] = json!(1);
    first["media"][0]["saved"] = json!(false);
    b.execute(json!({"op":"sync_apply","media":first["media"],"playback":[]})).await.unwrap();
    assert_eq!(item(&b, "movie", "tt1").await["liked"], json!(-1));
    assert_eq!(item(&b, "movie", "tt1").await["library"], json!(true));
    // A newer "forgotten" removes it.
    let gone = json!([{"type":"movie","id":"tt1","addon_url":CINEMETA,"updated":i64::MAX / 2,"deleted":true}]);
    b.execute(json!({"op":"sync_apply","media":gone,"playback":[]})).await.unwrap();
    assert_eq!(item(&b, "movie", "tt1").await, Value::Null);
}
