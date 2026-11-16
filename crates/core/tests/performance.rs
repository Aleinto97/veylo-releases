//! Optional local-only, non-asserting timing benchmark. See the core README.
use rusqlite::{params, Connection};
use rustmedia_core::Core;
use serde_json::{json, Value};
use std::{hint::black_box, path::PathBuf, time::Instant};

struct Store(PathBuf);
impl Drop for Store {
    fn drop(&mut self) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench-data");
        assert!(self.0.is_absolute() && self.0.parent() == Some(root.as_path()));
        let intended = root
            .canonicalize()
            .unwrap()
            .join(self.0.file_name().unwrap());
        assert_eq!(self.0.canonicalize().unwrap(), intended);
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
#[ignore = "local SQLite timing benchmark; run explicitly with --ignored --nocapture"]
async fn local_sqlite_hot_paths() {
    const ROWS: usize = 6000;
    const PROGRESS: usize = 100;
    const PERSONAL: usize = 8;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench-data");
    let path = root.join(format!("{}", std::process::id()));
    assert!(!path.exists(), "benchmark store must start empty");
    // Never acquire cleanup ownership of a pre-existing store.
    std::fs::create_dir_all(&path).unwrap();
    let store = Store(path);
    let core = Core::open(&store.0).unwrap();
    let mut db = Connection::open(store.0.join("core.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    for i in 0..ROWS {
        let id = format!("tt{}", 100000 + i);
        let meta = json!({"id":id,"type":"movie","name":format!("Title {i}"),
            "genres":["Drama",format!("Genre {}",i%12)],"director":[format!("Director {}",i%50)],
            "cast":[format!("Actor {}",i%60)],"description":"Local benchmark metadata. ".repeat(30),
            "poster":format!("https://example.test/{i}.jpg")});
        tx.execute("INSERT INTO media(key,type,raw_id,addon_url,meta,saved,liked,updated) VALUES(?1,'movie',?2,?3,?4,?5,?6,?7)",
            params![json!(["movie",id,""]).to_string(),id,"https://example.test/manifest.json",meta.to_string(),i==0,if i==1 {-1} else {0},i as i64]).unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    let progress = json!({"op":"progress","type":"movie","id":"tt100000",
        "addon_url":"https://example.test/manifest.json","position_ms":1000,"duration_ms":1000000});
    // Warm the API and file cache; setup is excluded from timed sections.
    core.execute(progress.clone()).await.unwrap();
    let started = Instant::now();
    for i in 0..PROGRESS {
        let mut req = progress.clone();
        req["position_ms"] = json!(1000 + i);
        if i % 2 == 0 {
            req["meta"] = json!({});
        }
        let result = core.execute(req).await.unwrap();
        assert_eq!(result["item"]["position_ms"], json!(1000 + i));
        black_box(result);
    }
    let elapsed = started.elapsed();
    println!(
        "BENCH rows={ROWS} progress_calls={PROGRESS} total_ms={:.3} us_per_call={:.3}",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1e6 / PROGRESS as f64
    );
    let mut old = Value::Null;
    let started = Instant::now();
    for _ in 0..PERSONAL {
        let library = core.execute(json!({"op":"library"})).await.unwrap();
        let recommendations = core
            .execute(json!({"op":"recommendations","limit":20}))
            .await
            .unwrap();
        let collections = core.execute(json!({"op":"collections"})).await.unwrap();
        old = json!({"library":library["items"],"recommendations":recommendations["metas"],
            "collections":collections["collections"],"needs_history":recommendations["needs_history"]});
        black_box(&old);
    }
    let elapsed = started.elapsed();
    println!(
        "BENCH rows={ROWS} old_personal_calls={PERSONAL} total_ms={:.3} ms_per_refresh={:.3}",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / PERSONAL as f64
    );
    if std::env::var_os("CORE_BENCH_PERSONAL").is_some() {
        let started = Instant::now();
        for _ in 0..PERSONAL {
            let result = core
                .execute(json!({"op":"personal","limit":20}))
                .await
                .unwrap();
            assert_eq!(result, old);
            black_box(result);
        }
        let elapsed = started.elapsed();
        println!(
            "BENCH rows={ROWS} personal_calls={PERSONAL} total_ms={:.3} ms_per_refresh={:.3}",
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1000.0 / PERSONAL as f64
        );
    }
}

/// A realistic positive-history set catches repeated vocabulary decoding costs.
#[tokio::test]
#[ignore = "local history-heavy timing benchmark; run explicitly with --ignored --nocapture"]
async fn history_heavy_recommendations() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench-data");
    let path = root.join(format!("{}-history", std::process::id()));
    assert!(!path.exists());
    std::fs::create_dir_all(&path).unwrap();
    let store = Store(path);
    let core = Core::open(&store.0).unwrap();
    let mut db = Connection::open(store.0.join("core.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    for i in 0..1200 {
        let id = format!("tt{}", 8_000_000 + i);
        let meta = json!({"id":id,"type":"movie","name":format!("Title {i}"),
            "genres":["Drama","Comedy"],"director":["Director One"],"cast":["Actor One","Actor Two"]});
        tx.execute("INSERT INTO media(key,type,raw_id,addon_url,meta,saved,updated) VALUES(?1,'movie',?2,'https://fixture.invalid/manifest.json',?3,?4,?5)",
            params![json!(["movie",id,""]).to_string(),id,meta.to_string(),i<400,i]).unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    let mut times = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        let result = core
            .execute(json!({"op":"personal","limit":20}))
            .await
            .unwrap();
        assert_eq!(result["library"].as_array().unwrap().len(), 400);
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        black_box(result);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "BENCH rows=1200 saved=400 personal_median_ms={:.3}",
        times[1]
    );
}
