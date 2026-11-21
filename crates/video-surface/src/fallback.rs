//! Which picture paths each platform has, in the order to try them, and the ones that failed on
//! this machine (MediaMP keeps the same memory): a failed path isn't tried again this run, so the
//! next title goes straight to one that works instead of failing the same way first.

use std::sync::Mutex;

/// One way of getting the picture on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Path {
    /// Linux, Wayland: a subsurface under the window, drawn on our own EGL context and thread.
    WaylandSubsurface,
    /// Linux: a GtkGLArea under the web view (X11, and Wayland's fallback).
    GlArea,
    /// Windows: mpv's own child window under the WebView2.
    WindowsChild,
    /// macOS: a Core Animation OpenGL layer under the WKWebView.
    MacosLayer,
}

impl Path {
    /// The name in logs and in CI's picture check.
    pub fn name(self) -> &'static str {
        match self {
            Path::WaylandSubsurface => "wayland-subsurface",
            Path::GlArea => "gl-area",
            Path::WindowsChild => "windows-child",
            Path::MacosLayer => "macos-layer",
        }
    }
}

/// The paths to try, best first. `wayland`: the window is on a Wayland display.
pub fn order(os: &str, wayland: bool) -> &'static [Path] {
    match os {
        "linux" if wayland => &[Path::WaylandSubsurface, Path::GlArea],
        "linux" => &[Path::GlArea],
        "windows" => &[Path::WindowsChild],
        "macos" => &[Path::MacosLayer],
        _ => &[],
    }
}

struct Memory { unavailable: Vec<Path>, active: Option<Path> }
static MEMORY: Mutex<Memory> = Mutex::new(Memory { unavailable: Vec::new(), active: None });

pub(crate) fn is_unavailable(path: Path) -> bool { MEMORY.lock().is_ok_and(|m| m.unavailable.contains(&path)) }

/// Developer and CI option: `VEYLO_DX_SKIP_PICTURE_PATH=gl-area,...` treats those paths as failed,
/// to try the next one on purpose (on Wayland, `wayland-subsurface` tests the GL area fallback).
pub(crate) fn skipped(path: Path) -> bool { skipped_in(std::env::var("VEYLO_DX_SKIP_PICTURE_PATH").ok().as_deref(), path) }

fn skipped_in(list: Option<&str>, path: Path) -> bool {
    list.is_some_and(|l| l.split(',').any(|name| name.trim() == path.name()))
}
pub(crate) fn mark_unavailable(path: Path) {
    if let Ok(mut m) = MEMORY.lock() { if !m.unavailable.contains(&path) { m.unavailable.push(path); } }
}
pub(crate) fn active() -> Option<Path> { MEMORY.lock().ok().and_then(|m| m.active) }
pub(crate) fn set_active(path: Option<Path>) { if let Ok(mut m) = MEMORY.lock() { m.active = path; } }

/// The paths that failed on this machine this run.
pub fn unavailable() -> Vec<Path> { MEMORY.lock().map(|m| m.unavailable.clone()).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_falls_back_to_the_gl_area() {
        assert_eq!(order("linux", true), &[Path::WaylandSubsurface, Path::GlArea]);
        assert_eq!(order("linux", false), &[Path::GlArea]);
        assert_eq!(order("windows", false), &[Path::WindowsChild]);
        assert_eq!(order("macos", false), &[Path::MacosLayer]);
        assert!(order("android", false).is_empty());
    }

    #[test]
    fn a_failed_path_is_remembered_once() {
        assert!(!is_unavailable(Path::WaylandSubsurface));
        mark_unavailable(Path::WaylandSubsurface);
        mark_unavailable(Path::WaylandSubsurface);
        assert!(is_unavailable(Path::WaylandSubsurface));
        assert_eq!(unavailable().iter().filter(|p| **p == Path::WaylandSubsurface).count(), 1);
        assert!(!is_unavailable(Path::GlArea));
    }

    #[test]
    fn the_skip_list_names_paths() {
        assert!(skipped_in(Some("wayland-subsurface"), Path::WaylandSubsurface));
        assert!(skipped_in(Some("gl-area, wayland-subsurface"), Path::WaylandSubsurface));
        assert!(!skipped_in(Some("gl-area"), Path::WaylandSubsurface));
        assert!(!skipped_in(None, Path::GlArea));
    }

    #[test]
    fn names_are_stable() {
        // CI and the logs match on these.
        let names: Vec<_> = [Path::WaylandSubsurface, Path::GlArea, Path::WindowsChild, Path::MacosLayer].map(Path::name).into();
        assert_eq!(names, ["wayland-subsurface", "gl-area", "windows-child", "macos-layer"]);
    }
}
