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
    // arm64 phones and tablets; 32-bit ARM for the many Android TVs that run a 32-bit system.
    let abi_dir = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() { Ok("aarch64") => "arm64-v8a", Ok("arm") => "armeabi-v7a", _ => "" };
    let mpv_libs = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("android/mpv").join(abi_dir);
    assert!(mpv_libs.join("libmpv.so").is_file(), "libmpv for Android is missing: run android/fetch-mpv.sh");
    println!("cargo:rustc-link-search=native={}", mpv_libs.display());
    let Ok(out) = std::env::var("WRY_ANDROID_KOTLIN_FILES_OUT_DIR") else { return };
    let out = std::path::Path::new(&out);
    std::fs::create_dir_all(out).expect("kotlin output dir");
    for entry in std::fs::read_dir("android/kotlin").expect("android/kotlin") {
        let path = entry.expect("kotlin entry").path();
        if path.extension().is_some_and(|e| e == "kt" || e == "pro") {
            std::fs::copy(&path, out.join(path.file_name().unwrap())).expect("copy kotlin/proguard file");
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
        tv_launcher(&main.join("AndroidManifest.xml"));
    }

    // The player's libmpv + FFmpeg (android/fetch-mpv.sh), linked by the Rust engine (src/engine.rs).
    // libplayer.so is mpv-android's own JNI glue, which Veylo doesn't use.
    let mpv = std::path::Path::new("android/mpv");
    println!("cargo:rerun-if-changed=android/mpv");
    {
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
        // Bundle Veylo's MIT license, SIL OFL 1.1 (Archivo), Privacy Policy, Third-Party Notices,
        // and mpv-android's license into APK assets for open-source license compliance.
        let licenses = assets.join("licenses");
        std::fs::create_dir_all(&licenses).expect("licenses dir");
        for (src, name) in [
            ("../../LICENSE", "LICENSE"),
            ("../../PRIVACY.md", "PRIVACY.md"),
            ("../../THIRD_PARTY_LICENSES.md", "THIRD_PARTY_LICENSES.md"),
            ("assets/fonts/OFL.txt", "OFL.txt"),
            ("android/mpv/LICENSE-mpv-android", "LICENSE-mpv-android"),
        ] {
            let p = std::path::Path::new(src);
            if p.is_file() {
                let dest = licenses.join(name);
                std::fs::copy(p, &dest).expect("copy license file");
                println!("cargo:rerun-if-changed={src}");
            }
        }
    }
}

/// Android TV: the TV home screen lists only apps whose activity is in LEANBACK_LAUNCHER, shown by
/// their banner (android/res/drawable-xhdpi/tv_banner.png, from branding/veylo-tv-banner.svg).
/// Also registers `stremio://` and `veylo://` deep links on the main activity.
/// dx 0.7 can't add these to its activity or application, so patch the manifest it generates.
fn tv_launcher(manifest: &std::path::Path) {
    let Ok(xml) = std::fs::read_to_string(manifest) else { return };
    let mut patched = xml.clone();
    if !patched.contains("android:banner=") {
        patched = patched.replacen("<application ", "<application android:banner=\"@drawable/tv_banner\" ", 1);
    }
    if !patched.contains("LEANBACK_LAUNCHER") {
        patched = patched.replacen("<category android:name=\"android.intent.category.LAUNCHER\" />",
            "<category android:name=\"android.intent.category.LAUNCHER\" />\n                <category android:name=\"android.intent.category.LEANBACK_LAUNCHER\" />", 1);
    }
    if !patched.contains("supportsPictureInPicture") {
        patched = patched.replacen("<activity ", "<activity android:supportsPictureInPicture=\"true\" ", 1);
    }
    if !patched.contains("smallestScreenSize") {
        if let Some(pos) = patched.find("android:configChanges=\"") {
            let insert_at = pos + "android:configChanges=\"".len();
            patched.insert_str(insert_at, "smallestScreenSize|");
        }
    }
    if !patched.contains("android:scheme=\"stremio\"") {
        let deep_link_filter = concat!(
            "</intent-filter>\n",
            "            <intent-filter>\n",
            "                <action android:name=\"android.intent.action.VIEW\" />\n",
            "                <category android:name=\"android.intent.category.DEFAULT\" />\n",
            "                <category android:name=\"android.intent.category.BROWSABLE\" />\n",
            "                <data android:scheme=\"stremio\" />\n",
            "                <data android:scheme=\"veylo\" />\n",
            "            </intent-filter>"
        );
        patched = patched.replacen("</intent-filter>", deep_link_filter, 1);
    }
    if patched != xml { std::fs::write(manifest, patched).expect("patch AndroidManifest.xml"); }
    println!("cargo:rerun-if-changed={}", manifest.display());
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
    println!("cargo:rerun-if-changed={}", dll.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // libmpv2-sys links "mpv": MSVC looks for mpv.lib, GNU for libmpv.dll.a; the release's import library serves both.
    let import_lib = dir.join("mpv.lib");
    let gnu_lib = dir.join("libmpv.dll.a");
    if import_lib.is_file() {
        println!("cargo:rerun-if-changed={}", import_lib.display());
    } else if gnu_lib.is_file() {
        println!("cargo:rerun-if-changed={}", gnu_lib.display());
        std::fs::copy(&gnu_lib, out.join("mpv.lib")).expect("copy the mpv import library");
        println!("cargo:rustc-link-search=native={}", out.display());
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
    // OUT_DIR sits under <target>/<profile>/build/ (as build/veylo-dx-<hash>/out or build/veylo-dx/<hash>/out):
    // the exe lives in <profile>, test binaries in <profile>/deps.
    if let Some(profile) = out.ancestors().find(|a| a.file_name().is_some_and(|n| n == "build")).and_then(Path::parent) {
        let src_meta = std::fs::metadata(&dll).ok();
        let src_id = dll.display().to_string();
        for to in [profile.to_path_buf(), profile.join("deps")] {
            let dest = to.join("libmpv-2.dll");
            let marker = to.join("libmpv-2.dll.src");
            let dest_meta = std::fs::metadata(&dest).ok();
            let same_source = std::fs::read_to_string(&marker).ok().as_deref() == Some(src_id.as_str());
            let same_file = match (&src_meta, &dest_meta) {
                (Some(s), Some(d)) => s.len() == d.len() && s.modified().ok() == d.modified().ok(),
                _ => false,
            };
            if (!same_source || !same_file) && std::fs::create_dir_all(&to).is_ok() {
                std::fs::copy(&dll, &dest).expect("copy libmpv-2.dll beside the executable");
                let _ = std::fs::write(&marker, &src_id);
            }
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
        ("android", "arm") => "android-armv7",
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
            let abi = if arch == "arm" { "armeabi-v7a" } else { "arm64-v8a" };
            let libs = main.join("jniLibs").join(abi);
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
