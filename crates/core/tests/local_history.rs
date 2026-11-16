mod common;
use common::Data;
use rustmedia_core::Core;
use serde_json::{json, Value};

/// A metadata-only addon registration with the given manifest id.
fn embedded(id: &str) -> Value {
    json!({"id":id,"name":"Embedded metadata","version":"1","types":["movie","series"],
        "resources":[{"name":"meta","types":["movie","series"],"idPrefixes":["tt","tmdb:","tvmaze:"]}],"catalogs":[]})
}

async fn progress(
    core: &Core,
    addon: &str,
    kind: &str,
    id: &str,
    video: &str,
    position: i64,
    watched: i64,
) {
    core.execute(json!({"op":"progress","type":kind,"id":id,"addon_url":addon,"video_id":video,
        "position_ms":position,"duration_ms":100000,"watched_ms":watched,
        "meta":{"id":id,"type":kind,"name":"Original","background":format!("{}/poster-cache/background",addon.split("/manifest").next().unwrap()),
        "videos":[{"id":video,"thumbnail":format!("{}/poster-cache/episode",addon.split("/manifest").next().unwrap())}]}})).await.unwrap();
}
fn migration(kind: &str, id: &str, old: &str, new: &str) -> Value {
    json!({"op":"migrate_local_history","type":kind,"id":id,"from_addon_url":old,"addon_url":new,
        "meta":{"id":id,"type":kind,"name":"Refreshed","poster":format!("{}/poster-cache/new",new.split("/manifest").next().unwrap())}})
}
async fn playback(core: &Core, addon: &str, kind: &str, id: &str, video: &str) -> Value {
    core.execute(json!({"op":"playback","type":kind,"id":id,"addon_url":addon,"video_id":video}))
        .await
        .unwrap()
}
async fn library(core: &Core) -> Vec<Value> {
    core.execute(json!({"op":"library"})).await.unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn local_history_unsaved_movie_survives_revision_port_and_metadata_destination() {
    for new in [
        "http://127.0.0.1:7099/manifest.json?v=6",
        "http://127.0.0.1:7100/manifest.json?v=6",
    ] {
        for id in ["tmdb:603", "tt0133093"] {
            let data = Data::new();
            let core = data.core();
            let old = "http://127.0.0.1:7099/manifest.json?v=5";
            data.install(old, &embedded("com.aio.metadata.rs"));
            data.install(new, &embedded("com.aio.metadata.rs"));
            progress(&core, old, "movie", id, id, 43210, 40000).await;
            core.execute(
                json!({"op":"feedback","type":"movie","id":id,"addon_url":old,"value":-1}),
            )
            .await
            .unwrap();
            let before = playback(&core, old, "movie", id, id).await;
            // A catalog can precreate the destination; the old buggy refresh also forced saved=true.
            // Shared IMDb keys have no duplicate, so emulate a harmless metadata refresh there.
            core.execute(
                json!({"op":"set_library","type":"movie","id":id,"addon_url":new,"saved":id.starts_with("tmdb:"),
                "meta":{"id":id,"type":"movie","name":"Current catalog"}}),
            )
            .await
            .unwrap();
            let result = core
                .execute(migration("movie", id, old, new))
                .await
                .unwrap();
            let item = &result["item"];
            assert_eq!(item["addon_url"], new);
            assert_eq!(item["position_ms"], 43210);
            assert_eq!(item["duration_ms"], 100000);
            assert_eq!(item["video_id"], id);
            assert_eq!(item["library"], false);
            assert_eq!(item["watched"], false);
            assert_eq!(item["liked"], -1);
            assert_eq!(item["name"], "Refreshed");
            let origin = new.split("/manifest").next().unwrap();
            assert_eq!(item["poster"], format!("{origin}/poster-cache/new"));
            assert_eq!(
                item["background"],
                format!("{origin}/poster-cache/background")
            );
            assert_eq!(
                item["videos"][0]["thumbnail"],
                format!("{origin}/poster-cache/episode")
            );
            assert_eq!(playback(&core, new, "movie", id, id).await, before);
            assert_eq!(library(&core).await.len(), 1);
            let again = core
                .execute(migration("movie", id, old, new))
                .await
                .unwrap();
            assert_eq!(again["item"], *item);
            assert_eq!(
                core.execute(json!({"op":"diagnostics"})).await.unwrap()["playback_entries"],
                1
            );
            if id.starts_with("tmdb:") {
                assert_eq!(
                    playback(&core, old, "movie", id, id).await["position_ms"],
                    0
                );
            }
            drop(core);
            let core = data.core();
            assert_eq!(playback(&core, new, "movie", id, id).await, before);
        }
    }
}

#[tokio::test]
async fn local_history_preserves_all_episode_rows_and_explicit_flags_after_old_addon_removal() {
    let data = Data::new();
    let core = data.core();
    let old = "http://127.0.0.1:7099/manifest.json?v=5";
    let new = "http://127.0.0.1:7201/manifest.json?v=6";
    data.install(old, &embedded("com.aio.metadata.rs"));
    data.install(new, &embedded("com.aio.metadata.rs"));
    progress(&core, old, "series", "tvmaze:9", "ep1", 95000, 85000).await;
    progress(&core, old, "series", "tvmaze:9", "ep2", 25000, 20000).await;
    for (op, key, value) in [
        ("set_library", "saved", json!(true)),
        ("mark_watched", "watched", json!(true)),
        ("feedback", "value", json!(1)),
    ] {
        let mut req = json!({"op":op,"type":"series","id":"tvmaze:9","addon_url":old});
        req[key] = value;
        core.execute(req).await.unwrap();
    }
    let first = playback(&core, old, "series", "tvmaze:9", "ep1").await;
    let second = playback(&core, old, "series", "tvmaze:9", "ep2").await;
    core.execute(json!({"op":"remove_addon","url":old}))
        .await
        .unwrap();
    let result = core
        .execute(migration("series", "tvmaze:9", old, new))
        .await
        .unwrap();
    assert_eq!(result["item"]["library"], true);
    assert_eq!(result["item"]["watched"], true);
    assert_eq!(result["item"]["liked"], 1);
    assert_eq!(result["item"]["video_id"], "ep2");
    assert_eq!(result["item"]["watched_ms"], 105000);
    assert_eq!(result["item"]["video_watched"], false);
    assert_eq!(
        playback(&core, new, "series", "tvmaze:9", "ep1").await,
        first
    );
    assert_eq!(
        playback(&core, new, "series", "tvmaze:9", "ep2").await,
        second
    );
    assert_eq!(library(&core).await.len(), 1);
    let other = "http://127.0.0.1:7999/manifest.json?v=6";
    assert_eq!(
        playback(&core, other, "series", "tvmaze:9", "ep2").await["position_ms"],
        0,
        "Opaque IDs must not become globally canonical"
    );
}

#[tokio::test]
async fn local_history_merges_existing_current_history_without_duplicate_evidence() {
    let data = Data::new();
    let core = data.core();
    let old = "http://127.0.0.1:7099/manifest.json?v=5";
    let new = "http://127.0.0.1:7099/manifest.json?v=6";
    data.install(new, &embedded("com.aio.metadata.rs"));
    progress(&core, old, "series", "tmdb:9", "ep1", 95000, 85000).await;
    progress(&core, old, "series", "tmdb:9", "ep2", 25000, 20000).await;
    core.execute(
        json!({"op":"set_library","type":"series","id":"tmdb:9","addon_url":old,"saved":true}),
    )
    .await
    .unwrap();
    progress(&core, new, "series", "tmdb:9", "ep1", 10000, 30000).await;
    progress(&core, new, "series", "tmdb:9", "ep3", 50000, 40000).await;
    core.execute(json!({"op":"feedback","type":"series","id":"tmdb:9","addon_url":new,"value":-1}))
        .await
        .unwrap();
    data.db()
        .execute(
            "UPDATE media SET updated=CASE WHEN addon_url=?1 THEN 100 ELSE 200 END",
            [old],
        )
        .unwrap();
    data.db()
        .execute(
            "UPDATE playback SET updated=CASE WHEN media_key=?1 THEN 100 ELSE 200 END",
            [json!(["series", "tmdb:9", old]).to_string()],
        )
        .unwrap();
    let item = core
        .execute(migration("series", "tmdb:9", old, new))
        .await
        .unwrap()["item"]
        .clone();
    assert_eq!(
        item["library"], false,
        "The newer destination's explicit flags win conflicts"
    );
    assert_eq!(item["liked"], -1);
    assert_eq!(item["video_id"], "ep3");
    assert_eq!(item["watched_ms"], 145000);
    let first = playback(&core, new, "series", "tmdb:9", "ep1").await;
    assert_eq!(first["position_ms"], 10000);
    assert_eq!(first["watched_ms"], 85000);
    assert_eq!(first["watched"], true);
    assert_eq!(
        playback(&core, new, "series", "tmdb:9", "ep2").await["position_ms"],
        25000
    );
    assert_eq!(library(&core).await.len(), 1);
    assert_eq!(
        core.execute(json!({"op":"diagnostics"})).await.unwrap()["playback_entries"],
        3
    );
}

#[tokio::test]
async fn local_history_failure_rolls_back_metadata_rekey_and_all_episode_rows() {
    let data = Data::new();
    let core = data.core();
    let old = "http://127.0.0.1:7099/manifest.json?v=5";
    let new = "http://127.0.0.1:7100/manifest.json?v=6";
    data.install(new, &embedded("com.aio.metadata.rs"));
    progress(&core, old, "series", "tmdb:9", "ep1", 95000, 85000).await;
    progress(&core, old, "series", "tmdb:9", "ep2", 25000, 20000).await;
    let before = library(&core).await;
    let key = json!(["series", "tmdb:9", old]).to_string();
    data.db().execute_batch("CREATE TRIGGER refuse_history_delete BEFORE DELETE ON media BEGIN SELECT RAISE(ABORT,'test migration failure'); END;").unwrap();
    assert!(core
        .execute(migration("series", "tmdb:9", old, new))
        .await
        .is_err());
    assert_eq!(library(&core).await, before);
    assert_eq!(
        playback(&core, new, "series", "tmdb:9", "ep1").await["position_ms"],
        0
    );
    assert_eq!(
        core.execute(json!({"op":"diagnostics"})).await.unwrap()["items"],
        1
    );
    assert_eq!(
        data.db()
            .query_row(
                "SELECT COUNT(*) FROM playback WHERE media_key=?1",
                [key],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    data.db()
        .execute_batch("DROP TRIGGER refuse_history_delete;")
        .unwrap();
    assert!(core
        .execute(migration("series", "tmdb:9", old, new))
        .await
        .is_ok());
}

#[tokio::test]
async fn local_history_rejects_non_embedded_urls_and_changed_metadata_identity() {
    let data = Data::new();
    let core = data.core();
    let old = "http://127.0.0.1:7099/manifest.json?v=5";
    let new = "http://127.0.0.1:7100/manifest.json?v=6";
    data.install(new, &embedded("com.aio.metadata.rs"));
    progress(&core, old, "movie", "tmdb:9", "tmdb:9", 12345, 10000).await;
    let before = library(&core).await;
    for invalid in [
        "https://remote.test/manifest.json",
        "http://127.0.0.1:7099/custom/manifest.json",
        "http://127.0.0.1:7099/manifest.json?token=secret",
    ] {
        assert!(core
            .execute(migration("movie", "tmdb:9", invalid, new))
            .await
            .is_err());
    }
    let mut wrong = migration("movie", "tmdb:9", old, new);
    wrong["meta"]["id"] = json!("tmdb:10");
    assert!(core.execute(wrong).await.is_err());
    let unrelated = "http://127.0.0.1:7999/manifest.json?v=6";
    data.install(unrelated, &embedded("unrelated.local.addon"));
    assert!(core
        .execute(migration("movie", "tmdb:9", old, unrelated))
        .await
        .is_err());
    assert_eq!(library(&core).await, before);
}
