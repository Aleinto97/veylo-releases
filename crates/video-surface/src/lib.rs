//! Where libmpv's picture goes on the desktop: under the app's web view, which turns transparent
//! while a title plays, so the page's own controls float over the video. One path per platform:
//!
//! - Linux, Wayland: a subsurface of the window placed below it, filled by mpv's render API on its
//!   own EGL context and thread; the compositor blends the page over the picture (no copies).
//!   If that can't be set up, the GL area below is tried instead.
//! - Linux, X11: a GtkGLArea below the WebKit view, put in place when the first title plays: a window
//!   holding a GL area is composited through GL, and where GL can't draw (software GL, some VMs) the
//!   whole page would stay black.
//! - Windows: mpv's own child window inside the app window, kept below the WebView2.
//! - macOS: a Core Animation OpenGL layer below the WKWebView, filled by mpv's render API; EDR (HDR)
//!   on screens with headroom.
//!
//! (Android's picture is a SurfaceView owned by the app's own Kotlin activity, so it stays in the app.)
//!
//! The crate knows nothing of the app's UI toolkit: the app hands it its window ([`Host`]) and its
//! player ([`Player`]), and keeps what belongs to the toolkit (the web view's background, full screen).
//!
//! Paths are chosen at [`attach`], before mpv opens the file, and never changed while a title plays:
//! freeing mpv's renderer mid-playback turns its video off until the player is recreated. A path that
//! fails on this machine is remembered and skipped from then on ([`fallback`]).

use std::sync::Arc;

pub mod fallback;
pub use fallback::Path;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as imp;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod none;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use none as imp;

/// The playing session. The crate keeps an `Arc` of it while the picture is up, so mpv's core
/// outlives its renderer (mpv requires that order).
pub trait Player: Send + Sync + 'static {
    fn mpv(&self) -> &libmpv2::Mpv;
}

/// The app window the picture goes under, from the app's UI toolkit.
#[cfg(target_os = "linux")]
pub struct Host {
    /// The top-level window.
    pub window: gtk::Window,
    /// The web view showing the page (the picture goes below it).
    pub web: gtk::Widget,
    /// The box holding the web view, for the GL area path (it puts an overlay in the web view's place).
    pub vbox: Option<gtk::Box>,
}

/// The app window the picture goes under, from the app's UI toolkit.
#[cfg(target_os = "macos")]
pub struct Host {
    /// The `NSWindow*`, valid while the window lives.
    pub ns_window: *mut std::ffi::c_void,
}

/// The app window the picture goes under, from the app's UI toolkit.
#[cfg(target_os = "windows")]
pub struct Host {
    /// The top-level `HWND`.
    pub hwnd: isize,
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub struct Host;

/// Put the player's picture under the window (call on the UI thread, before mpv opens the file).
/// Tries this platform's paths in order, skipping any that failed before on this machine; the one
/// now in use, or None when there is no picture (mpv still plays the sound).
pub fn attach(host: &Host, player: Arc<dyn Player>) -> Option<Path> {
    detach();
    let usable = |path: Path| !fallback::is_unavailable(path) && !fallback::skipped(path);
    let candidates = imp::candidates();
    for (i, &path) in candidates.iter().enumerate() {
        if fallback::is_unavailable(path) { continue; }
        if fallback::skipped(path) {
            eprintln!("veylo: picture path {} skipped (VEYLO_DX_SKIP_PICTURE_PATH)", path.name());
            continue;
        }
        // The only path left has nothing to fall back to: a failure may be a passing one (the window
        // not realized yet, a GL context that wasn't ready), so try it once more before giving up on it.
        let sole = !candidates[i + 1..].iter().any(|&p| usable(p));
        let mut opened = imp::open(path, host, &player);
        if sole {
            if let Err(first) = &opened {
                eprintln!("veylo: picture path {} failed: {first}; trying it once more", path.name());
                imp::close(path);
                opened = imp::open(path, host, &player);
            }
        }
        match opened {
            Ok(()) => {
                failures(path, false);
                fallback::set_active(Some(path));
                eprintln!("veylo: picture path: {}", path.name());
                return Some(path);
            }
            Err(e) => {
                imp::close(path);
                // With another path to try, this one is dropped at once; the sole path only after it
                // has failed on several titles in a row, so one bad moment doesn't mean no picture all run.
                if !sole || failures(path, true) >= SOLE_PATH_FAILURES {
                    fallback::mark_unavailable(path);
                    eprintln!("veylo: picture path {} failed: {e}; not trying it again this run", path.name());
                } else {
                    eprintln!("veylo: picture path {} failed: {e}; it will be tried again with the next title", path.name());
                }
            }
        }
    }
    eprintln!("veylo: no picture path worked; playing without a picture");
    None
}

/// How many titles in a row the sole picture path may fail on (two attempts each) before it is dropped.
const SOLE_PATH_FAILURES: u32 = 2;

/// Counts a path's consecutive failed attaches (`failed`), or forgets them after a good one.
fn failures(path: Path, failed: bool) -> u32 {
    static COUNTS: std::sync::Mutex<Vec<(Path, u32)>> = std::sync::Mutex::new(Vec::new());
    let mut counts = COUNTS.lock().unwrap_or_else(|p| p.into_inner());
    let before = counts.iter().find(|(p, _)| *p == path).map_or(0, |(_, n)| *n);
    counts.retain(|(p, _)| *p != path);
    if !failed { return 0; }
    counts.push((path, before + 1));
    before + 1
}

/// Take the picture away (UI thread).
pub fn detach() {
    if let Some(path) = fallback::active() {
        imp::close(path);
        fallback::set_active(None);
    }
}

/// The path showing the picture now.
pub fn active() -> Option<Path> { fallback::active() }

/// The video's format is known or changed (any thread). macOS sends HDR video to an EDR screen as
/// PQ when `hdr_output` (the app's setting) allows it; elsewhere nothing changes here.
pub fn layout(player: &dyn Player, hdr_output: bool) { imp::layout(player, hdr_output) }

#[cfg(target_os = "macos")]
pub use macos::{check, hdr_check, potential_headroom, sent_transfer};
