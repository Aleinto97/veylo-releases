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
    // Set C numeric locale once before spawning any worker threads (libmpv requires it, and
    // calling setlocale after multithreading begins is UB on POSIX).
    unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()); }
    let online = args.iter().any(|a| a == "--online");
    let report = args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1)).cloned();
    // An isolated, unpredictable store, removed afterwards (set before the backend starts).
    let mut rnd = [0u8; 8];
    let _ = getrandom::fill(&mut rnd);
    let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let data = std::env::temp_dir().join(format!("veylo-self-test-{}-{suffix}", std::process::id()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&data);
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(&data);
    }
    std::env::set_var("VEYLO_DX_DATA", &data);

    let mut lines = vec![format!("Veylo {} self-test on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH)];
    println!("{}", lines[0]);
    let mut failed = 0;
    let mut check = |name: &str, result: Result<String>| -> bool {
        let ok = result.is_ok();
        let line = match result {
            Ok(detail) => format!("PASS  {name:<10} {detail}"),
            Err(e) => { failed += 1; format!("FAIL  {name:<10} {e:#}") }
        };
        println!("{line}");
        lines.push(line);
        ok
    };
    check("bundle", bundle());
    let store_ok = check("store", store());
    if store_ok {
        check("display", display_and_autoplay());
        check("picks-ai", picks());
    } else {
        check("display", Err(anyhow!("skipped because store initialization failed")));
        check("picks-ai", Err(anyhow!("skipped because store initialization failed")));
    }
    check("player", player(&data));
    check("hdr", hdr(&data));
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
    let before = backend::load_settings();
    let flipped = !before.picks_hide_watched;
    backend::update_settings(|s| s.picks_hide_watched = flipped);
    let read_back = backend::load_settings().picks_hide_watched;
    backend::save_settings(&before);
    ensure!(read_back == flipped, "settings didn't read back");
    Ok("library saved and read back; settings round-trip".into())
}

/// Display probing and autoplay ranking must work off the Dioxus UI thread (OS thread + Tokio
/// worker pool) without panicking — guards against Windows/macOS `dioxus::desktop::window()` panics.
fn display_and_autoplay() -> Result<String> {
    std::thread::spawn(crate::display::screen)
        .join()
        .map_err(|_| anyhow!("display::screen() panicked on a background thread"))?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(backend::selftest_autoplay_off_thread())
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
    // macOS: the picture goes through mpv's render API on an OpenGL context of the app's (video.rs).
    #[cfg(target_os = "macos")]
    let picture = format!("; {}", crate::video::check(&mpv).map_err(|e| anyhow!("video layer: {e}"))?);
    #[cfg(not(target_os = "macos"))]
    let picture = String::new();
    Ok(format!("{version} (FFmpeg {ffmpeg}) played H.264 {}x{} and AAC to the end{picture}", size.0, size.1))
}

/// One second of HDR10: 10-bit H.264, PQ, BT.2020, with mastering metadata (FFmpeg's testsrc2).
pub const HDR_CLIP: &[u8] = include_bytes!("../assets/selftest-hdr.mp4");

/// HDR video is recognised as HDR (PQ, BT.2020, 10-bit) wherever it plays; on macOS it is also
/// rendered as PQ through the picture's own pipeline, and the screen's EDR headroom is reported.
fn hdr(data: &std::path::Path) -> Result<String> {
    std::fs::create_dir_all(data)?;
    let clip = data.join("selftest-hdr.mp4");
    std::fs::write(&clip, HDR_CLIP)?;
    let mpv = Mpv::with_initializer(|o| {
        for (k, v) in [("config", "no"), ("terminal", "no"), ("vo", "null"), ("ao", "null"), ("keep-open", "yes")] { o.set_property(k, v)?; }
        Ok(())
    }).map_err(|e| anyhow!("mpv couldn't start: {e}"))?;
    mpv.command("loadfile", &[clip.to_string_lossy().as_ref()]).map_err(|e| anyhow!("loadfile: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let params = loop {
        ensure!(Instant::now() < deadline, "the HDR clip didn't start in 20 s");
        match mpv.wait_event(0.5) {
            Some(Ok(Event::VideoReconfig)) => {
                let p = |n: &str| mpv.get_property::<String>(&format!("video-params/{n}")).unwrap_or_default();
                if !p("gamma").is_empty() { break (p("gamma"), p("primaries"), p("pixelformat")); }
            }
            Some(Ok(Event::EndFile(reason))) => {
                return Err(anyhow!("the HDR clip ended before video config (reason {reason})"));
            }
            Some(Err(e)) => return Err(anyhow!("mpv: {e}")),
            _ => {}
        }
    };
    ensure!(params.0 == "pq" && params.1 == "bt.2020", "the HDR10 clip read as {}/{}", params.0, params.1);
    #[cfg(target_os = "macos")]
    return crate::video::hdr_check(&clip.to_string_lossy()).map_err(|e| anyhow!("{e}"));
    #[cfg(not(target_os = "macos"))]
    Ok(format!("HDR10 recognised: PQ, BT.2020, {}", params.2))
}

/// TLS works with the bundled roots, and the release channel answers (and has this platform's file).
/// Uses github.com's own release links: the API's anonymous rate limit is shared by every CI runner.
fn network() -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let base = format!("https://github.com/{}/releases/latest", updater::REPO);
    let (tag, has_asset) = runtime.block_on(async {
        let client = reqwest::Client::builder().user_agent("veylo-dx").timeout(Duration::from_secs(30)).build()?;
        let latest = client.get(&base).send().await?.error_for_status()?;
        let tag = latest.url().path().rsplit('/').next().unwrap_or("?").to_owned();
        let has_asset = match updater::ASSET {
            Some(asset) => client.get(format!("{base}/download/{asset}")).header("Range", "bytes=0-0").send().await?.status().is_success(),
            None => false,
        };
        Ok::<_, anyhow::Error>((tag, has_asset))
    })?;
    Ok(format!("TLS ok; latest release {tag}{}", match updater::ASSET {
        Some(a) if has_asset => format!(" has {a}"),
        Some(a) => format!(" has no {a} yet"),
        None => String::new(),
    }))
}
