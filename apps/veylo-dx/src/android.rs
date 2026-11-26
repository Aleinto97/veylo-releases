//! JNI glue for Android: the app's private files folder, the Rust player's video surface
//! (VeyloVideo.kt), Back (VeyloBack.kt), updates (VeyloUpdate.kt) and account sign-in (VeyloAccount.kt).
use anyhow::{anyhow, Result};
use jni::{objects::{JObject, JString, JValue}, JavaVM, JNIEnv};
use std::path::PathBuf;

fn with_activity<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Result<T> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| anyhow!("JNI: {e}"))?;
    let mut env = vm.attach_current_thread().map_err(|e| anyhow!("JNI attach: {e}"))?;
    // The activity is a global reference the activity glue owns; wrap each call in a bounded JNI
    // local frame so local references (JString, JObject, JClass) are always freed before returning.
    let result = env.with_local_frame(32, |env| {
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        f(env, &activity)
    });
    if env.exception_check().unwrap_or(false) { let _ = env.exception_clear(); }
    result.map_err(|e| anyhow!("JNI: {e}"))
}

/// Context.getFilesDir(): private to this app, kept until it is uninstalled.
/// A system property ("ro.product.model"), None when unset.
pub fn property(name: &std::ffi::CStr) -> Option<String> {
    let mut value = [0 as libc::c_char; 92]; // PROP_VALUE_MAX
    // SAFETY: a NUL-terminated name and a PROP_VALUE_MAX buffer, as bionic requires.
    let n = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    (n > 0).then(|| unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }.to_string_lossy().trim().to_owned()).filter(|v| !v.is_empty())
}

/// Whether this device is a television: TVs say so in ro.build.characteristics ("tv").
pub fn is_tv() -> bool {
    static TV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TV.get_or_init(|| property(c"ro.build.characteristics").is_some_and(|v| v.split(',').any(|c| c.trim() == "tv")))
}

pub fn files_dir() -> Result<PathBuf> {
    with_activity(|env, activity| {
        let dir = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
        let path = JString::from(env.call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?);
        let path: String = env.get_string(&path)?.into();
        Ok(PathBuf::from(path))
    })
}

/// Read and clear an incoming `stremio://` or `veylo://` intent URI when Veylo was opened via a link.
pub fn take_deep_link() -> Option<String> {
    with_activity(|env, activity| {
        let intent = env.call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])?.l()?;
        if intent.is_null() { return Ok(None); }
        let data = env.call_method(&intent, "getDataString", "()Ljava/lang/String;", &[])?.l()?;
        if data.is_null() { return Ok(None); }
        let raw: String = env.get_string(&JString::from(data))?.into();
        let parsed = crate::addons::parse_deep_link(&raw);
        if parsed.is_some() {
            let null_uri = JObject::null();
            let _ = env.call_method(&intent, "setData", "(Landroid/net/Uri;)Landroid/content/Intent;", &[JValue::Object(&null_uri)]);
        }
        Ok(parsed)
    }).ok().flatten()
}

/// ACTION_VIEW for an https link the viewer chose to open (a YouTube trailer).
pub fn open_url(url: &str) -> Result<()> {
    with_activity(|env, activity| {
        let uri_str = env.new_string(url)?;
        let uri = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&uri_str)])?.l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;Landroid/net/Uri;)V", &[JValue::Object(&action), JValue::Object(&uri)])?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    })
}

/// Hand a downloaded update APK to Android's installer (VeyloUpdate.kt). Returns "installing",
/// or "permission" when the viewer must first allow Veylo to install apps (Settings is opened).
pub fn install_update(apk: &std::path::Path) -> Result<String> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloUpdate")?;
        let path = env.new_string(apk.to_string_lossy())?;
        let status = env.call_static_method(&class, "install", "(Landroid/app/Activity;Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(activity), JValue::Object(&path)])?.l()?;
        let status: String = env.get_string(&JString::from(status))?.into();
        Ok(status)
    })
}

/// Status bar and navigation bar heights in dp (= CSS px in the WebView): the window's own insets
/// (they follow gesture navigation, rotation and split screen), ignoring whether the bars are
/// hidden at the moment (the player hides them). Before the window has insets, the system's
/// nominal bar heights.
pub fn system_bar_insets() -> Result<(f32, f32)> {
    with_activity(|env, activity| {
        let res = env.call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])?.l()?;
        let metrics = env.call_method(&res, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[])?.l()?;
        let density = env.get_field(&metrics, "density", "F")?.f()?.max(0.5);
        let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
        let decor = env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
        let insets = env.call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?.l()?;
        if !insets.is_null() {
            let sdk = env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()?;
            let (top, bottom) = if sdk >= 30 {
                let bars = env.call_static_method("android/view/WindowInsets$Type", "systemBars", "()I", &[])?.i()?;
                let i = env.call_method(&insets, "getInsetsIgnoringVisibility", "(I)Landroid/graphics/Insets;", &[JValue::Int(bars)])?.l()?;
                (env.get_field(&i, "top", "I")?.i()?, env.get_field(&i, "bottom", "I")?.i()?)
            } else {
                (env.call_method(&insets, "getStableInsetTop", "()I", &[])?.i()?, env.call_method(&insets, "getStableInsetBottom", "()I", &[])?.i()?)
            };
            return Ok((top as f32 / density, bottom as f32 / density));
        }
        let mut dp = |name: &str| -> jni::errors::Result<f32> {
            let (n, kind, pkg) = (env.new_string(name)?, env.new_string("dimen")?, env.new_string("android")?);
            let id = env.call_method(&res, "getIdentifier", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
                &[JValue::Object(&n), JValue::Object(&kind), JValue::Object(&pkg)])?.i()?;
            if id == 0 { return Ok(0.0); }
            Ok(env.call_method(&res, "getDimensionPixelSize", "(I)I", &[JValue::Int(id)])?.i()? as f32 / density)
        };
        Ok((dp("status_bar_height")?, dp("navigation_bar_height")?))
    })
}

fn load_class<'a>(env: &mut JNIEnv<'a>, activity: &JObject, name: &str) -> jni::errors::Result<jni::objects::JClass<'a>> {
    let loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
    let name = env.new_string(name)?;
    let class = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?.l()?;
    Ok(jni::objects::JClass::from(class))
}

/// Google sign-in through Android's account sheet (VeyloAccount.kt); the answer comes later
/// through `account_result`.
pub fn google_id_token(web_client: &str) -> Result<()> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloAccount")?;
        let client = env.new_string(web_client)?;
        env.call_static_method(&class, "signInWithGoogle", "(Landroid/app/Activity;Ljava/lang/String;)V", &[JValue::Object(activity), JValue::Object(&client)])?;
        Ok(())
    })
}

/// Google's QR scanner (VeyloAccount.kt), for a computer's sign-in code; the answer comes later
/// through `account_result`.
pub fn scan_code() -> Result<()> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloAccount")?;
        env.call_static_method(&class, "scanCode", "(Landroid/app/Activity;)V", &[JValue::Object(activity)])?;
        Ok(())
    })
}

/// The account sheet's or the scanner's answer, once it has one: "token:…", "code:…", "cancel:"
/// or "error:…".
pub fn account_result() -> Option<String> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloAccount")?;
        let value = env.call_static_method(&class, "take", "()Ljava/lang/String;", &[])?.l()?;
        if value.is_null() { return Ok(None); }
        Ok(Some(env.get_string(&JString::from(value))?.into()))
    }).ok().flatten()
}

/// While a page or Search is open, Back closes it (VeyloBack.kt) instead of leaving the app.
pub fn set_back_enabled(enabled: bool) -> Result<()> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloBack")?;
        env.call_static_method(&class, "setEnabled", "(Landroid/app/Activity;Z)V", &[JValue::Object(activity), JValue::Bool(enabled.into())])?;
        Ok(())
    })
}

// ---------------------------------------------------------------- Rust player surface (VeyloVideo.kt)

use jni::objects::{GlobalRef, JClass};
use jni::sys::jint;
use std::sync::Mutex;

/// The SurfaceView's surface while it exists, as mpv's `wid` (a JNI global reference).
static SURFACE: Mutex<Option<(GlobalRef, i32, i32)>> = Mutex::new(None);

fn video_call(name: &str, sig: &str, args: &[JValue]) -> Result<()> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        let mut all = vec![JValue::Object(activity)];
        all.extend_from_slice(args);
        env.call_static_method(&class, name, sig, &all)?;
        Ok(())
    })
}

/// Put the SurfaceView under the WebView; mpv gets it through [nativeSurface].
pub fn video_show() -> Result<()> { video_call("show", "(Landroid/app/Activity;)V", &[]) }
pub fn video_hide() -> Result<()> { video_call("hide", "(Landroid/app/Activity;)V", &[]) }
pub fn video_layout(aspect: f64, fit: i32) -> Result<()> {
    video_call("layout", "(Landroid/app/Activity;DI)V", &[JValue::Double(aspect), JValue::Int(fit)])
}
pub fn video_frame_rate(fps: f64) -> Result<()> {
    video_call("setFrameRate", "(Landroid/app/Activity;D)V", &[JValue::Double(fps)])
}
pub fn video_media_meta(title: &str, subtitle: &str) -> Result<()> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        let t = env.new_string(title)?;
        let s = env.new_string(subtitle)?;
        env.call_static_method(&class, "setMediaMetadata", "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(activity), JValue::Object(&t), JValue::Object(&s)])?;
        Ok(())
    })
}
pub fn video_media_state(playing: bool, position_ms: i64, speed: f32) -> Result<()> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        env.call_static_method(&class, "setMediaState", "(ZJF)V",
            &[JValue::Bool(playing.into()), JValue::Long(position_ms), JValue::Float(speed)])?;
        Ok(())
    })
}
pub fn video_get_brightness() -> f32 {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        env.call_static_method(&class, "getBrightness", "(Landroid/app/Activity;)F", &[JValue::Object(activity)])?.f()
    }).unwrap_or(0.5)
}
pub fn video_set_brightness(level: f32) -> Result<()> {
    video_call("setBrightness", "(Landroid/app/Activity;F)V", &[JValue::Float(level)])
}
pub fn video_get_volume() -> f32 {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        env.call_static_method(&class, "getVolume", "(Landroid/app/Activity;)F", &[JValue::Object(activity)])?.f()
    }).unwrap_or(0.5)
}
pub fn video_set_volume(level: f32) -> Result<()> {
    video_call("setVolume", "(Landroid/app/Activity;F)V", &[JValue::Float(level)])
}
pub fn video_set_rotation_lock(locked: bool) -> Result<()> {
    video_call("setRotationLock", "(Landroid/app/Activity;Z)V", &[JValue::Bool(locked.into())])
}
pub fn video_enter_pip(w: i32, h: i32) -> bool {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        env.call_static_method(&class, "enterPip", "(Landroid/app/Activity;II)Z",
            &[JValue::Object(activity), JValue::Int(w), JValue::Int(h)])?.z()
    }).unwrap_or(false)
}

/// What the panel declares (Display.HdrCapabilities).
pub fn screen() -> Result<crate::engine::Screen> {
    let mask = with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        env.call_static_method(&class, "hdrTypes", "(Landroid/app/Activity;)I", &[JValue::Object(activity)])?.i()
    })?;
    Ok(crate::engine::Screen { hdr10: mask & 1 != 0, hlg: mask & 2 != 0, hdr10_plus: mask & 4 != 0, dolby_vision: mask & 8 != 0, peak_nits: 0 })
}

/// Before mpv starts: FFmpeg's MediaCodec decoders need the Java VM and the app context (what
/// mpv-android's own glue does), and TLS needs the CA bundle. Returns the bundle's path.
pub fn prepare_mpv() -> Result<String> {
    static JVM: std::sync::Once = std::sync::Once::new();
    JVM.call_once(|| unsafe {
        type SetVm = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32;
        let lib = libc::dlopen(c"libavcodec.so".as_ptr(), libc::RTLD_NOW);
        if lib.is_null() { return; }
        let set_vm = libc::dlsym(lib, c"av_jni_set_java_vm".as_ptr());
        let set_ctx = libc::dlsym(lib, c"av_jni_set_android_app_ctx".as_ptr());
        if !set_vm.is_null() {
            let set_vm: SetVm = std::mem::transmute(set_vm);
            set_vm(ndk_context::android_context().vm(), std::ptr::null_mut());
        }
        if !set_ctx.is_null() {
            let app = with_activity(|env, activity| {
                let app = env.call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?.l()?;
                env.new_global_ref(app)
            });
            if let Ok(app) = app {
                let set_ctx: SetVm = std::mem::transmute(set_ctx);
                set_ctx(app.as_obj().as_raw().cast(), std::ptr::null_mut());
                std::mem::forget(app); // FFmpeg keeps it for the life of the process
            }
        }
    });
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloVideo")?;
        let path = env.call_static_method(&class, "prepare", "(Landroid/app/Activity;)Ljava/lang/String;", &[JValue::Object(activity)])?.l()?;
        let path: String = env.get_string(&JString::from(path))?.into();
        Ok(path)
    })
}

/// Hand the current surface (if any) to a session that has just started.
pub fn attach_surface(live: &crate::engine::Live) {
    if let Ok(slot) = SURFACE.lock() {
        if let Some((surface, w, h)) = slot.as_ref() { give_surface(live, surface.as_obj().as_raw() as i64, *w, *h); }
    }
}

fn give_surface(live: &crate::engine::Live, wid: i64, w: i32, h: i32) {
    let mpv = &live.mpv;
    let _ = mpv.set_property("wid", wid);
    if w > 0 && h > 0 { let _ = mpv.set_property("android-surface-size", format!("{w}x{h}").as_str()); }
    let _ = mpv.set_property("force-window", "yes");
    let _ = mpv.set_property("vo", if live.state().direct { "mediacodec_embed" } else { "gpu-next" });
    live.mark_ready();
}

/// VeyloVideo.nativeSurface: the SurfaceView's surface was created, resized, or is going away.
/// Runs on the UI thread; mpv must let go of a dying surface before this returns.
#[no_mangle]
pub extern "system" fn Java_dev_veylo_dx_VeyloVideo_nativeSurface(env: JNIEnv, _class: JClass, surface: JObject, width: jint, height: jint) {
    let Ok(mut slot) = SURFACE.lock() else { return };
    let live = crate::engine::active();
    if surface.is_null() {
        if let Some(live) = &live {
            // Leaving the app pauses; the picture comes back with the surface.
            let _ = live.mpv.set_property("pause", true);
            let _ = live.mpv.set_property("vo", "null");
            let _ = live.mpv.set_property("force-window", "no");
            let _ = live.mpv.set_property("wid", 0i64);
        }
        *slot = None;
        return;
    }
    let same = slot.as_ref().is_some_and(|(g, _, _)| env.is_same_object(g.as_obj(), &surface).unwrap_or(false));
    if same {
        if let Some((_, w, h)) = slot.as_mut() { *w = width; *h = height; }
        if let Some(live) = &live { let _ = live.mpv.set_property("android-surface-size", format!("{width}x{height}").as_str()); }
        return;
    }
    let Ok(global) = env.new_global_ref(&surface) else { return };
    let wid = global.as_obj().as_raw() as i64;
    // Keep the old surface's GlobalRef alive until AFTER mpv has switched `wid` to the new surface.
    let _old = slot.replace((global, width, height));
    if let Some(live) = &live { give_surface(live, wid, width, height); }
    drop(_old);
}

/// VeyloVideo.nativeCommand: playback commands from AudioFocus, headphone disconnect, or MediaSession.
#[no_mangle]
pub extern "system" fn Java_dev_veylo_dx_VeyloVideo_nativeCommand(mut env: JNIEnv, _class: JClass, cmd: JString) {
    let Ok(cmd_str) = env.get_string(&cmd) else { return };
    let cmd: String = cmd_str.into();
    let Some(live) = crate::engine::active() else { return };
    match cmd.as_str() {
        "play" => live.set_paused(false),
        "pause" => live.set_paused(true),
        "toggle" => live.toggle_pause(),
        "seek_fwd" => live.seek_by(10.0),
        "seek_back" => live.seek_by(-10.0),
        "next" if live.request.has_next => live.finish(crate::engine::Finish::Next),
        "stop" => live.finish(crate::engine::Finish::Close),
        _ => {}
    }
}

/// One line in logcat under "veylo" (stderr is not shown on Android). Never URLs or keys.
/// Some builds (Honor, Huawei) hide app logs, so the line also goes, timed, into the app's own
/// files/dev-log.txt (`adb shell run-as dev.veylo.dx cat files/dev-log.txt`), kept under 64 KiB.
pub fn log(message: &str) {
    extern "C" { fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32; }
    if let Ok(text) = std::ffi::CString::new(message) { unsafe { __android_log_write(4, c"veylo".as_ptr(), text.as_ptr()); } }
    // Only a found folder is kept: a line logged before the activity exists must not turn the file off.
    static FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let path = match FILE.get() {
        Some(path) => path,
        None => match files_dir() { Ok(dir) => FILE.get_or_init(|| dir.join("dev-log.txt")), Err(_) => return },
    };
    static LOG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOG_LOCK.lock();
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 64 * 1024) { let _ = std::fs::rename(path, path.with_extension("old.txt")); }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) { let _ = writeln!(f, "{ms} {message}"); }
}

/// Developer option: files/dev-play.json ({"url", "title", "hdr"}), pushed with `adb shell run-as`,
/// plays once at launch and is deleted. Used to test HDR10/HDR10+ clips on a device.
pub fn take_dev_play() -> Option<serde_json::Value> {
    let path = files_dir().ok()?.join("dev-play.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&text).ok()
}

/// Developer option: files/dev-autoplay.txt (lines "kind/id/title/year"), pushed with `adb shell
/// run-as`, is read once at launch and deleted; `backend::dev_autoplay` times Play's choice for each.
pub fn take_dev_autoplay() -> Option<String> {
    let path = files_dir().ok()?.join("dev-autoplay.txt");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(text)
}

/// Shipped embedding model assets, extracted by one background inference worker.
pub fn picks_ai_paths() -> Result<(PathBuf, PathBuf)> {
    with_activity(|env, activity| {
        let class = load_class(env, activity, "dev.veylo.dx.VeyloPicks")?;
        let mut call = |name: &str| -> jni::errors::Result<PathBuf> {
            let value = env.call_static_method(&class, name, "(Landroid/app/Activity;)Ljava/lang/String;", &[JValue::Object(activity)])?.l()?;
            let path: String = env.get_string(&JString::from(value))?.into();
            Ok(PathBuf::from(path))
        };
        Ok((call("modelDir")?, call("runtimePath")?))
    })
}
