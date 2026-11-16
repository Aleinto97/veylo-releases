//! JNI glue for Android: the app's private files folder, the Rust player's video surface
//! (VeyloVideo.kt), Back (VeyloBack.kt) and updates (VeyloUpdate.kt).
use anyhow::{anyhow, Result};
use jni::{objects::{JObject, JString, JValue}, JavaVM, JNIEnv};
use std::path::PathBuf;

fn with_activity<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Result<T> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| anyhow!("JNI: {e}"))?;
    let mut env = vm.attach_current_thread().map_err(|e| anyhow!("JNI attach: {e}"))?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result = f(&mut env, &activity);
    std::mem::forget(activity); // borrowed global reference owned by the activity glue
    if env.exception_check().unwrap_or(false) { let _ = env.exception_clear(); }
    result.map_err(|e| anyhow!("JNI: {e}"))
}

/// Context.getFilesDir(): private to this app, kept until it is uninstalled.
pub fn files_dir() -> Result<PathBuf> {
    with_activity(|env, activity| {
        let dir = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
        let path = JString::from(env.call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?);
        let path: String = env.get_string(&path)?.into();
        Ok(PathBuf::from(path))
    })
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

/// Status bar and navigation bar heights in dp (= CSS px in the WebView).
pub fn system_bar_insets() -> Result<(f32, f32)> {
    with_activity(|env, activity| {
        let res = env.call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])?.l()?;
        let metrics = env.call_method(&res, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[])?.l()?;
        let density = env.get_field(&metrics, "density", "F")?.f()?.max(0.5);
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
pub extern "system" fn Java_dev_veylo_dx_VeyloVideo_nativeSurface(mut env: JNIEnv, _class: JClass, surface: JObject, width: jint, height: jint) {
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
    *slot = Some((global, width, height));
    if let Some(live) = &live { give_surface(live, wid, width, height); }
}

/// One line in logcat under "veylo" (stderr is not shown on Android). Never URLs or keys.
pub fn log(message: &str) {
    extern "C" { fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32; }
    if let Ok(text) = std::ffi::CString::new(message) { unsafe { __android_log_write(4, c"veylo".as_ptr(), text.as_ptr()); } }
}

/// Developer option: files/dev-play.json ({"url", "title", "hdr"}), pushed with `adb shell run-as`,
/// plays once at launch and is deleted. Used to test HDR10/HDR10+ clips on a device.
pub fn take_dev_play() -> Option<serde_json::Value> {
    let path = files_dir().ok()?.join("dev-play.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&text).ok()
}

/// Shipped GLiClass assets, extracted by one background inference worker.
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
