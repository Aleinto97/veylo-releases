//! Where the picture goes: under the app's WebView, which turns transparent while a title plays, so
//! the Dioxus controls (player_ui.rs) float over the video on every platform.
//! Desktop: crates/video-surface puts it there (a Wayland subsurface or a GL area on Linux, mpv's
//!   child window on Windows, a Core Animation OpenGL layer with EDR on macOS); this file hands it
//!   the window and keeps what belongs to Dioxus (the page's background, full screen).
//! Android: a SurfaceView below the WebView (VeyloVideo.kt), handed to mpv as its window.
use crate::engine::Live;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};

/// Tracks which `Live::session_id` owns the attached surface so an old `PlayerScreen`'s `use_drop`
/// (which in Dioxus 0.7 can run *after* the replacement `PlayerScreen`'s `use_hook`) never tears
/// down the newly attached playback session.
static ATTACHED_SESSION: AtomicU64 = AtomicU64::new(0);

/// Put the session's picture on screen (call on the UI thread).
pub fn attach(live: &Arc<Live>) {
    ATTACHED_SESSION.store(live.session_id, Ordering::Release);
    imp::attach(live);
}
/// Take the picture away and give the app its opaque background back, only if `session_id` is
/// still active. True when this call took it away (the player's surroundings may be reset then).
pub fn detach(session_id: u64) -> bool {
    if ATTACHED_SESSION.compare_exchange(session_id, 0, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return false;
    }
    imp::detach();
    true
}
/// Take away a picture that was kept up while the player moved to another episode or source and
/// nothing took over (the next one failed or was cancelled). Does nothing when none is attached.
pub fn release() {
    if ATTACHED_SESSION.swap(0, Ordering::AcqRel) != 0 { imp::detach(); }
}
/// The picture's aspect or fit changed (any thread).
pub fn layout(live: &Live) { imp::layout(live) }
/// Hint the content frame rate (e.g. 23.976 Hz) to the display surface where supported (Android 11+).
pub fn set_frame_rate(fps: f64) { imp::set_frame_rate(fps) }
/// Desktop window fullscreen; Android is always full screen while playing.
pub fn set_fullscreen(on: bool) { imp::set_fullscreen(on) }
/// Cache the native window handle / display state on the Dioxus UI thread so background threads
/// (Tokio autoplay probes, mpv player thread, telemetry) can read display capabilities without
/// ever touching `dioxus::desktop::window()` outside a Dioxus runtime.
pub fn register_window() {
    #[cfg(target_os = "windows")]
    {
        let _ = hwnd();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = potential_headroom();
    }
}
/// Windows: the app window, for the screen's HDR state (display.rs). Safe from any thread once
/// `register_window()` has run on the UI thread.
#[cfg(target_os = "windows")]
pub fn hwnd() -> Option<isize> {
    use std::sync::atomic::{AtomicIsize, Ordering};
    static CACHED: AtomicIsize = AtomicIsize::new(0);
    let h = CACHED.load(Ordering::Relaxed);
    if h != 0 { return Some(h); }
    dioxus::core::Runtime::try_current()?; // off the UI thread window() would panic: use the cache
    use dioxus::desktop::tao::platform::windows::WindowExtWindows;
    let h = dioxus::desktop::window().window.hwnd() as isize;
    if h != 0 { CACHED.store(h, Ordering::Relaxed); Some(h) } else { None }
}
/// macOS: the most EDR headroom the window's screen can reach (1.0: SDR only; 0: not read yet).
/// Safe from any thread: queries live on the Dioxus UI thread and returns the cached value off-thread.
#[cfg(target_os = "macos")]
pub fn potential_headroom() -> f64 {
    veylo_video_surface::potential_headroom(|| dioxus::core::Runtime::try_current().map(|_| imp::host()))
}
/// macOS: the self-tests of the picture's pipeline, and the transfer it goes out with ("pq" in HDR).
#[cfg(target_os = "macos")]
pub use veylo_video_surface::{check, hdr_check, sent_transfer};

#[cfg(not(target_os = "android"))]
impl veylo_video_surface::Player for Live {
    fn mpv(&self) -> &libmpv2::Mpv { &self.mpv }
}

#[cfg(target_os = "android")]
mod imp {
    use super::*;
    use crate::engine::Fit;
    pub fn attach(live: &Arc<Live>) {
        let _ = crate::android::video_show();
        let _ = crate::android::video_media_meta(&live.request.title, &live.request.subtitle);
        crate::android::attach_surface(live);
    }
    pub fn detach() { let _ = crate::android::video_hide(); }
    pub fn layout(live: &Live) {
        let s = live.state();
        let fit = match s.fit { Fit::Fit => 0, Fit::Fill => 1, Fit::Stretch => 2 };
        let _ = crate::android::video_layout(s.aspect, fit);
    }
    pub fn set_frame_rate(fps: f64) { let _ = crate::android::video_frame_rate(fps); }
    pub fn set_fullscreen(_on: bool) {}
}

#[cfg(not(target_os = "android"))]
mod imp {
    use super::*;
    use veylo_video_surface::Host;

    /// The window the picture goes under, from Dioxus (UI thread).
    pub fn host() -> Host {
        let ctx = dioxus::desktop::window();
        #[cfg(target_os = "linux")]
        {
            use dioxus::desktop::{tao::platform::unix::WindowExtUnix, wry::WebViewExtUnix};
            use gtk::prelude::*;
            let web: gtk::Widget = ctx.webview.webview().upcast();
            Host { window: ctx.window.gtk_window().clone().upcast(), web, vbox: ctx.window.default_vbox().cloned() }
        }
        #[cfg(target_os = "macos")]
        {
            use dioxus::desktop::tao::platform::macos::WindowExtMacOS;
            Host { ns_window: ctx.window.ns_window() }
        }
        #[cfg(target_os = "windows")]
        {
            use dioxus::desktop::tao::platform::windows::WindowExtWindows;
            Host { hwnd: ctx.window.hwnd() as isize }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        { let _ = ctx; Host }
    }

    /// The page turns see-through over the picture where the picture is a separate layer under the
    /// WebView's own background (macOS: the window is transparent and the page paints its own black).
    const CLEAR_PAGE: bool = cfg!(any(target_os = "linux", target_os = "windows"));

    /// Keep the desktop display awake while video plays; released automatically on detach.
    mod wake_lock {
        #[cfg(target_os = "windows")]
        pub fn acquire() {
            #[link(name = "kernel32")]
            extern "system" { fn SetThreadExecutionState(flags: u32) -> u32; }
            const ES_CONTINUOUS: u32 = 0x8000_0000;
            const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
            const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
            unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED); }
        }
        #[cfg(target_os = "windows")]
        pub fn release() {
            #[link(name = "kernel32")]
            extern "system" { fn SetThreadExecutionState(flags: u32) -> u32; }
            const ES_CONTINUOUS: u32 = 0x8000_0000;
            unsafe { SetThreadExecutionState(ES_CONTINUOUS); }
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        static CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

        #[cfg(target_os = "macos")]
        pub fn acquire() {
            release();
            let pid = std::process::id().to_string();
            if let Ok(child) = std::process::Command::new("/usr/bin/caffeinate")
                .args(["-d", "-i", "-w", &pid])
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .spawn()
            {
                if let Ok(mut g) = CHILD.lock() { *g = Some(child); }
            }
        }
        #[cfg(target_os = "linux")]
        pub fn acquire() {
            release();
            // Hold stdin open to `cat`: closing stdin or dying causes `cat` (and systemd-inhibit)
            // to exit immediately without ever orphaning a `sleep infinity` child process.
            if let Ok(child) = std::process::Command::new("systemd-inhibit")
                .args(["--what=idle", "--who=Veylo", "--why=Playing video", "--mode=block", "cat"])
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .spawn()
            {
                if let Ok(mut g) = CHILD.lock() { *g = Some(child); }
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        pub fn release() {
            if let Ok(mut g) = CHILD.lock() {
                if let Some(mut c) = g.take() {
                    drop(c.stdin.take());
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        pub fn acquire() {}
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        pub fn release() {}
    }

    pub fn attach(live: &Arc<Live>) {
        wake_lock::acquire();
        let path = veylo_video_surface::attach(&host(), live.clone());
        if path.is_some() && CLEAR_PAGE { let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 0)); }
        // Without a picture mpv still plays the sound.
        live.mark_ready();
    }
    pub fn detach() {
        wake_lock::release();
        veylo_video_surface::detach();
        if CLEAR_PAGE { let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 255)); }
        set_fullscreen(false);
    }
    pub fn layout(live: &Live) { veylo_video_surface::layout(live, crate::backend::hdr_output()); }
    pub fn set_frame_rate(_fps: f64) {}
    pub fn set_fullscreen(on: bool) {
        let w = dioxus::desktop::window();
        w.window.set_fullscreen(on.then_some(dioxus::desktop::tao::window::Fullscreen::Borderless(None)));
    }
}
