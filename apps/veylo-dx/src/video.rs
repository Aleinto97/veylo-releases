//! Where the picture goes: under the app's WebView, which turns transparent while a title plays, so
//! the Dioxus controls (player_ui.rs) float over the video on every platform.
//! Desktop: crates/video-surface puts it there (a Wayland subsurface or a GL area on Linux, mpv's
//!   child window on Windows, a Core Animation OpenGL layer with EDR on macOS); this file hands it
//!   the window and keeps what belongs to Dioxus (the page's background, full screen).
//! Android: a SurfaceView below the WebView (VeyloVideo.kt), handed to mpv as its window.
use crate::engine::Live;
use std::sync::Arc;

/// Put the session's picture on screen (call on the UI thread).
pub fn attach(live: &Arc<Live>) { imp::attach(live) }
/// Take the picture away and give the app its opaque background back.
pub fn detach() { imp::detach() }
/// The picture's aspect or fit changed (any thread).
pub fn layout(live: &Live) { imp::layout(live) }
/// Hint the content frame rate (e.g. 23.976 Hz) to the display surface where supported (Android 11+).
pub fn set_frame_rate(fps: f64) { imp::set_frame_rate(fps) }
/// Desktop window fullscreen; Android is always full screen while playing.
pub fn set_fullscreen(on: bool) { imp::set_fullscreen(on) }
/// Windows: the app window, for the screen's HDR state (display.rs).
#[cfg(target_os = "windows")]
pub fn hwnd() -> Option<isize> {
    use dioxus::desktop::tao::platform::windows::WindowExtWindows;
    Some(dioxus::desktop::window().window.hwnd() as isize)
}
/// macOS: the most EDR headroom the window's screen can reach (1.0: SDR only; 0: not read yet).
#[cfg(target_os = "macos")]
pub fn potential_headroom() -> f64 { veylo_video_surface::potential_headroom(|| Some(imp::host())) }
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

    pub fn attach(live: &Arc<Live>) {
        let path = veylo_video_surface::attach(&host(), live.clone());
        if path.is_some() && CLEAR_PAGE { let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 0)); }
        // Without a picture mpv still plays the sound.
        live.mark_ready();
    }
    pub fn detach() {
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
