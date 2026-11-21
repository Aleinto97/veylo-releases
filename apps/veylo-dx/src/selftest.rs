//! `veylo-dx --self-test [--online] [--report <file>]`: checks that this copy of Veylo works on this
//! machine, without a window: the files it ships with, its store, the local Picks model, the player's
//! libmpv and FFmpeg, and (with --online) TLS and the update channel. Prints one line per check and
//! exits 1 if any failed. CI runs it on the installed build of every desktop OS (.github/workflows/ci.yml).
//! It works in a temporary data folder of its own: your library and settings are never touched.
use crate::{backend, picks_ai, updater};
use anyhow::{anyhow, ensure, Context, Result};
use libmpv2::{events::Event, mpv_end_file_reason, Mpv};
use std::time::{Duration, Instant};

pub fn run(args: &[String]) -> i32 {
    let online = args.iter().any(|a| a == "--online");
    let report = args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1)).cloned();
    // An isolated store, removed afterwards (set before the backend starts: it reads VEYLO_DX_DATA once).
    let data = std::env::temp_dir().join(format!("veylo-self-test-{}", std::process::id()));
    std::env::set_var("VEYLO_DX_DATA", &data);

    let mut lines = vec![format!("Veylo {} self-test on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH)];
    println!("{}", lines[0]);
    let mut failed = 0;
    let mut check = |name: &str, result: Result<String>| {
        let line = match result {
            Ok(detail) => format!("PASS  {name:<10} {detail}"),
            Err(e) => { failed += 1; format!("FAIL  {name:<10} {e:#}") }
        };
        println!("{line}");
        lines.push(line);
    };
    check("bundle", bundle());
    check("store", store());
    check("picks-ai", picks());
    check("player", player(&data));
    if online {
        check("network", network());
    }
    let _ = std::fs::remove_dir_all(&data);

    let summary = if failed == 0 { "All checks passed.".to_owned() } else { format!("{failed} check(s) failed.") };
    println!("{summary}");
    lines.push(summary);
    if let Some(path) = report { let _ = std::fs::write(path, lines.join("\n") + "\n"); }
    if failed == 0 { 0 } else { 1 }
}

/// The files this copy loads at run time are where it looks for them.
fn bundle() -> Result<String> {
    let files = picks_ai::asset_files()?;
    for file in &files {
        ensure!(file.is_file(), "missing {}", file.display());
    }
    #[cfg(windows)]
    {
        let dir = std::env::current_exe()?.parent().context("executable folder")?.to_path_buf();
        ensure!(dir.join("libmpv-2.dll").is_file(), "libmpv-2.dll is not beside veylo-dx.exe");
    }
    #[cfg(target_os = "macos")]
    if let Some(app) = updater::app_bundle() {
        for part in ["Contents/Info.plist", "Contents/Resources/veylo.icns"] {
            ensure!(app.join(part).is_file(), "Veylo.app has no {part}");
        }
        let frameworks = app.join("Contents/Frameworks");
        let mpv = std::fs::read_dir(&frameworks).with_context(|| "Veylo.app has no Frameworks folder")?
            .flatten().any(|e| e.file_name().to_string_lossy().starts_with("libmpv"));
        ensure!(mpv, "Veylo.app carries no libmpv");
    }
    Ok(format!("model, tokenizer and ONNX Runtime found{}", updater::ASSET.map(|a| format!("; updates from {a}")).unwrap_or_default()))
}

/// The library store (SQLite) and settings open, write and read back.
fn store() -> Result<String> {
    backend::init()?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let media = backend::Media::from_json(&serde_json::json!({
        "id": "tt0000001", "type": "movie", "name": "Self-test film", "addon_url": "https://self-test.invalid/manifest.json"
    }), "").context("test title")?;
    let saved = runtime.block_on(async {
        backend::set_saved(&media, true).await?;
        backend::library().await
    })?;
    ensure!(saved.iter().any(|m| m.id == media.id && m.saved), "a saved title didn't come back from the library");
    let mut settings = backend::load_settings();
    settings.picks_rounds = 7;
    backend::save_settings(&settings);
    ensure!(backend::load_settings().picks_rounds == 7, "settings didn't read back");
    Ok("library saved and read back; settings round-trip".into())
}

/// The bundled model runs on this CPU and its embeddings mean something.
fn picks() -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let started = Instant::now();
    let (space, galaxy, baking) = runtime.block_on(async {
        Ok::<_, anyhow::Error>((
            picks_ai::embed_query("space adventure").await?,
            picks_ai::embed_query("astronauts explore a distant galaxy").await?,
            picks_ai::embed_query("a cooking show about baking bread").await?,
        ))
    })?;
    ensure!(space.len() == picks_ai::DIM, "embedding has {} dimensions, expected {}", space.len(), picks_ai::DIM);
    ensure!(space.iter().all(|x| x.is_finite()), "embedding has non-finite values");
    let (near, far) = (picks_ai::cosine(&space, &galaxy), picks_ai::cosine(&space, &baking));
    ensure!(near > far, "similar texts aren't closer ({near:.3} vs {far:.3})");
    Ok(format!("{}-d embeddings in {} ms; related {near:.2} > unrelated {far:.2}", space.len(), started.elapsed().as_millis()))
}

/// One second of H.264 and AAC in MP4 (320x240, a test pattern and a tone; made with FFmpeg's testsrc2 and sine).
const CLIP: &[u8] = include_bytes!("../assets/selftest.mp4");

/// libmpv starts and FFmpeg demuxes and decodes a real clip to the end (no window, no sound device).
fn player(data: &std::path::Path) -> Result<String> {
    std::fs::create_dir_all(data)?;
    let clip = data.join("selftest.mp4");
    std::fs::write(&clip, CLIP)?;
    unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()); }
    let mpv = Mpv::with_initializer(|o| {
        o.set_property("config", false)?;
        o.set_property("terminal", false)?;
        o.set_property("vo", "null")?;
        o.set_property("ao", "null")?;
        o.set_property("idle", true)?;
        Ok(())
    }).map_err(|e| anyhow!("mpv couldn't start: {e}"))?;
    let version: String = mpv.get_property("mpv-version").unwrap_or_default();
    let ffmpeg: String = mpv.get_property("ffmpeg-version").unwrap_or_default();
    mpv.command("loadfile", &[clip.to_string_lossy().as_ref(), "replace"]).map_err(|e| anyhow!("loadfile: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut size, mut codecs) = ((0, 0), (String::new(), String::new()));
    loop {
        ensure!(Instant::now() < deadline, "the test clip didn't finish in 30 s");
        match mpv.wait_event(0.5) {
            Some(Ok(Event::FileLoaded)) => {
                codecs = (mpv.get_property("current-tracks/video/codec").unwrap_or_default(), mpv.get_property("current-tracks/audio/codec").unwrap_or_default());
            }
            Some(Ok(Event::VideoReconfig)) => {
                let w = mpv.get_property::<i64>("video-params/w").unwrap_or(0);
                if w > 0 { size = (w, mpv.get_property::<i64>("video-params/h").unwrap_or(0)); }
            }
            Some(Ok(Event::EndFile(reason))) => {
                ensure!(reason == mpv_end_file_reason::Eof, "the test clip ended early (reason {reason})");
                break;
            }
            Some(Err(e)) => return Err(anyhow!("mpv: {e}")),
            _ => {}
        }
    }
    ensure!(codecs == ("h264".into(), "aac".into()), "tracks are {codecs:?}, expected h264 and aac");
    ensure!(size == (320, 240), "decoded picture is {}x{}, expected 320x240", size.0, size.1);
    Ok(format!("{version} (FFmpeg {ffmpeg}) played H.264 {}x{} and AAC to the end", size.0, size.1))
}

/// TLS works with the bundled roots, and the release channel answers (and has this platform's file).
fn network() -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let body: serde_json::Value = runtime.block_on(async {
        let response = reqwest::Client::builder().user_agent("veylo-dx").timeout(Duration::from_secs(20)).build()?
            .get(format!("https://api.github.com/repos/{}/releases/latest", updater::REPO))
            .header("Accept", "application/vnd.github+json").send().await?.error_for_status()?;
        Ok::<_, anyhow::Error>(response.json().await?)
    })?;
    let tag = body["tag_name"].as_str().unwrap_or("?");
    let has_asset = updater::ASSET.is_some_and(|a| body["assets"].as_array().into_iter().flatten().any(|x| x["name"] == a));
    Ok(format!("TLS ok; latest release {tag}{}", match updater::ASSET {
        Some(a) if has_asset => format!(" has {a}"),
        Some(a) => format!(" has no {a} yet"),
        None => String::new(),
    }))
}
