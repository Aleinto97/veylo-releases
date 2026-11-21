//! Android: hand our Kotlin bridges (video surface, Back, updates) to the Gradle project dx
//! generates. dx points wry at WRY_ANDROID_KOTLIN_FILES_OUT_DIR; files placed there are compiled too.
fn main() {
    stage_picks_ai();
    println!("cargo:rerun-if-changed=android/kotlin");
    println!("cargo:rerun-if-env-changed=WRY_ANDROID_KOTLIN_FILES_OUT_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") { return windows(); }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") { return macos(); }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") { return; }
    // The Rust player links mpv-android's libmpv (android/fetch-mpv.sh), packaged below.
    let abi_dir = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() { Ok("aarch64") => "arm64-v8a", _ => "" };
    let mpv_libs = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("android/mpv").join(abi_dir);
    assert!(mpv_libs.join("libmpv.so").is_file(), "libmpv for Android is missing: run android/fetch-mpv.sh");
    println!("cargo:rustc-link-search=native={}", mpv_libs.display());
    let Ok(out) = std::env::var("WRY_ANDROID_KOTLIN_FILES_OUT_DIR") else { return };
    let out = std::path::Path::new(&out);
    std::fs::create_dir_all(out).expect("kotlin output dir");
    for entry in std::fs::read_dir("android/kotlin").expect("android/kotlin") {
        let path = entry.expect("kotlin entry").path();
        if path.extension().is_some_and(|e| e == "kt") {
            std::fs::copy(&path, out.join(path.file_name().unwrap())).expect("copy kotlin source");
            println!("cargo:rerun-if-changed={}", out.join(path.file_name().unwrap()).display());
        }
    }

    let abi = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() { Ok("aarch64") => "arm64-v8a", Ok("arm") => "armeabi-v7a", _ => return };

    // Launcher icon and label (branding/): overlay our res tree on the generated one.
    println!("cargo:rerun-if-changed=android/res");
    if let Some(main) = out.ancestors().nth(4) {
        fn overlay(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).expect("res dir");
            for entry in std::fs::read_dir(from).expect("android/res") {
                let path = entry.expect("res entry").path();
                let dest = to.join(path.file_name().unwrap());
                if path.is_dir() { overlay(&path, &dest); continue; }
                std::fs::copy(&path, &dest).expect("copy res file");
                println!("cargo:rerun-if-changed={}", dest.display());
            }
        }
        overlay(std::path::Path::new("android/res"), &main.join("res"));
    }

    // The player's libmpv + FFmpeg (android/fetch-mpv.sh), linked by the Rust engine (src/engine.rs).
    // libplayer.so is mpv-android's own JNI glue, which Veylo doesn't use.
    let mpv = std::path::Path::new("android/mpv");
    println!("cargo:rerun-if-changed=android/mpv");
    if abi == "arm64-v8a" {
        let main = out.ancestors().nth(4).expect("android project layout");
        let libs = main.join("jniLibs").join(abi);
        std::fs::create_dir_all(&libs).expect("jniLibs dir");
        for entry in std::fs::read_dir(mpv.join(abi)).expect("android/mpv libraries") {
            let path = entry.expect("mpv entry").path();
            if path.file_name().is_some_and(|n| n == "libplayer.so") { continue; }
            let dest = libs.join(path.file_name().unwrap());
            std::fs::copy(&path, &dest).expect("copy mpv library");
            println!("cargo:rerun-if-changed={}", dest.display());
        }
        let assets = main.join("assets");
        std::fs::create_dir_all(&assets).expect("assets dir");
        std::fs::copy(mpv.join("cacert.pem"), assets.join("cacert.pem")).expect("copy cacert.pem");
        println!("cargo:rerun-if-changed={}", assets.join("cacert.pem").display());
    }
}

/// Windows: link the libmpv fetched by windows/fetch-mpv.ps1 (or VEYLO_MPV_DIR) and put
/// libmpv-2.dll beside the executable, where Windows looks for it first.
fn windows() {
    use std::path::{Path, PathBuf};
    println!("cargo:rerun-if-changed=windows/veylo.rc");
    println!("cargo:rerun-if-changed=windows/veylo.ico");
    embed_resource::compile("windows/veylo.rc", embed_resource::NONE).manifest_optional().expect("compile the Windows icon resource");
    println!("cargo:rerun-if-env-changed=VEYLO_MPV_DIR");
    println!("cargo:rerun-if-changed=windows/mpv");
    let dir = std::env::var_os("VEYLO_MPV_DIR").map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows/mpv"));
    let dll = dir.join("libmpv-2.dll");
    assert!(dll.is_file(), "libmpv for Windows is missing: run windows/fetch-mpv.ps1 (or set VEYLO_MPV_DIR)");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // libmpv2-sys links "mpv": MSVC looks for mpv.lib, GNU for libmpv.dll.a; the release's import library serves both.
    if !dir.join("mpv.lib").is_file() && dir.join("libmpv.dll.a").is_file() {
        std::fs::copy(dir.join("libmpv.dll.a"), out.join("mpv.lib")).expect("copy the mpv import library");
        println!("cargo:rustc-link-search=native={}", out.display());
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
    // OUT_DIR sits under <target>/<profile>/build/ (as build/veylo-dx-<hash>/out or build/veylo-dx/<hash>/out):
    // the exe lives in <profile>, test binaries in <profile>/deps.
    if let Some(profile) = out.ancestors().find(|a| a.file_name().is_some_and(|n| n == "build")).and_then(Path::parent) {
        for to in [profile.to_path_buf(), profile.join("deps")] {
            let dest = to.join("libmpv-2.dll");
            let stale = std::fs::metadata(&dest).and_then(|m| m.modified()).ok() < std::fs::metadata(&dll).and_then(|m| m.modified()).ok();
            if stale && std::fs::create_dir_all(&to).is_ok() { std::fs::copy(&dll, &dest).expect("copy libmpv-2.dll beside the executable"); }
        }
    }
}

/// macOS: link Homebrew's libmpv (`brew install mpv`), or VEYLO_MPV_DIR. A packaged Veylo.app
/// carries its own copy and every library it needs (tools/build-macos.sh).
fn macos() {
    println!("cargo:rerun-if-env-changed=VEYLO_MPV_DIR");
    println!("cargo:rerun-if-env-changed=HOMEBREW_PREFIX");
    let dirs: Vec<std::path::PathBuf> = match std::env::var_os("VEYLO_MPV_DIR") {
        Some(dir) => vec![dir.into()],
        None => std::env::var_os("HOMEBREW_PREFIX").map(|p| std::path::PathBuf::from(p).join("lib")).into_iter()
            .chain(["/opt/homebrew/lib", "/usr/local/lib"].map(std::path::PathBuf::from)).collect(),
    };
    let found = dirs.iter().find(|d| d.join("libmpv.dylib").exists() || d.join("libmpv.2.dylib").exists());
    let dir = found.unwrap_or_else(|| panic!("libmpv for macOS is missing: run `brew install mpv` (or set VEYLO_MPV_DIR)"));
    println!("cargo:rustc-link-search=native={}", dir.display());
}

/// The APK carries the same model/tokenizer as desktop, plus the Android CPU runtime.
/// Fail the build if assets are absent: released apps must never silently omit local AI.
fn stage_picks_ai() {
    use std::path::{Path, PathBuf};
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target = match (os.as_str(), arch.as_str()) {
        ("android", "aarch64") => "android-arm64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "x86_64") => "linux-x64",
        ("windows", "x86_64") => "windows-x64",
        ("macos", "aarch64") => "macos-arm64",
        _ => panic!("Picks inference runtime has not been packaged for {os}/{arch}"),
    };
    let root = Path::new("picks-ai");
    let native = root.join(target);
    let library = match os.as_str() { "windows" => "onnxruntime.dll", "macos" => "libonnxruntime.dylib", _ => "libonnxruntime.so" };
    for file in [root.join("model/model.onnx"), root.join("model/tokenizer.json"), native.join(library)] {
        assert!(file.is_file(), "Missing {}: run python3 ../../tools/picks-ai/fetch.py {target} from apps/veylo-dx", file.display());
    }
    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("Picks assets directory");
        for entry in std::fs::read_dir(from).expect("Picks assets") {
            let path = entry.unwrap().path();
            if !path.is_file() { continue; }
            println!("cargo:rerun-if-changed={}", path.display());
            let dest = to.join(path.file_name().unwrap());
            let changed = std::fs::metadata(&dest).map(|m| m.len() != std::fs::metadata(&path).unwrap().len() || m.modified().ok() < std::fs::metadata(&path).unwrap().modified().ok()).unwrap_or(true);
            if changed { std::fs::copy(path, dest).expect("Copy Picks asset"); }
        }
    }
    if os == "android" {
        println!("cargo:rerun-if-env-changed=WRY_ANDROID_KOTLIN_FILES_OUT_DIR");
        if let Ok(out) = std::env::var("WRY_ANDROID_KOTLIN_FILES_OUT_DIR") {
            let main = Path::new(&out).ancestors().nth(4).expect("Android layout");
            copy_dir(&root.join("model"), &main.join("assets/picks-ai"));
            // Notices are assets too; only native code belongs in jniLibs.
            copy_dir(&native, &main.join("assets/picks-ai/runtime"));
            let libs = main.join("jniLibs/arm64-v8a");
            std::fs::create_dir_all(&libs).expect("Picks JNI directory");
            std::fs::copy(native.join(library), libs.join(library)).expect("Copy ONNX Runtime");
            // The native library is already in jniLibs, avoid an extra copy in APK assets.
            let duplicate = main.join("assets/picks-ai/runtime").join(library);
            if duplicate.exists() { std::fs::remove_file(duplicate).expect("Remove duplicated runtime"); }
        }
    } else {
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let profile = out.ancestors().find(|p| p.file_name().is_some_and(|n| n == "build")).and_then(Path::parent).expect("Cargo profile directory");
        for dest in [profile.join("picks-ai"), profile.join("deps/picks-ai")] {
            copy_dir(&root.join("model"), &dest);
            copy_dir(&native, &dest.join("runtime"));
        }
    }
}
