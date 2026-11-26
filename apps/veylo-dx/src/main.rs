//! Veylo in Dioxus: the shell (top bar, keys, player status), its screens and the in-app player, on the Rust core.
// Windows release builds are a GUI app: no console window behind Veylo.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod addons;
#[cfg(target_os = "android")]
mod android;
mod backend;
mod brain;
mod account_ui;
mod components;
mod engine;
mod home;
mod library;
mod icons;
mod picks;
mod picks_ai;
mod picks_metadata;
mod picks_rules;
mod player;
mod player_rules;
mod player_ui;
mod search;
#[cfg(not(target_os = "android"))]
mod selftest;
mod settings;
mod shortcuts;
mod theatre;
mod title;
mod tracks;
mod video;
mod updater;
mod display;

use backend::Media;
use theatre::Theatre;
use base64::Engine;
use dioxus::prelude::*;

/// The app's styles, in cascade order (src/styles).
const STYLE: &str = concat!(include_str!("styles/base.css"), include_str!("styles/screens.css"), include_str!("styles/sizes.css"),
    include_str!("styles/addons.css"), include_str!("styles/player.css"), include_str!("styles/pages.css"), include_str!("styles/motion.css"),
    include_str!("styles/tv.css"));
const OUTFIT_LATIN: &[u8] = include_bytes!("../assets/fonts/outfit.woff2");
const OUTFIT_EXT: &[u8] = include_bytes!("../assets/fonts/outfit-ext.woff2");
const INTER_LATIN: &[u8] = include_bytes!("../assets/fonts/inter.woff2");
const INTER_EXT: &[u8] = include_bytes!("../assets/fonts/inter-ext.woff2");

/// Whether Veylo runs on a television (Android TV and TVs that say so): Back never leaves the app,
/// shelves are shorter, and tv.js drives focus from the remote.
pub fn is_tv() -> bool {
    #[cfg(target_os = "android")]
    { android::is_tv() }
    #[cfg(not(target_os = "android"))]
    { false }
}

/// Destinations, in the Android app's order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab { Home, Picks, Library, Settings }

impl Tab {
    const ALL: [Tab; 4] = [Tab::Home, Tab::Picks, Tab::Library, Tab::Settings];
    fn title(self) -> &'static str {
        match self { Tab::Home => "Home", Tab::Picks => "Picks", Tab::Library => "Library", Tab::Settings => "Settings" }
    }
}

/// What the status chip shows while the platform player works.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerStatus {
    /// Checking sources before Play.
    Finding(String),
    Opening(String),
    Playing { title: String, codec: String, hardware: Option<String>, facts: player::Facts },
    Failed(String),
}

/// The current accent as CSS hex, for the platform player.
#[derive(Clone, Copy)]
pub struct Accent(pub Signal<String>);

/// Keyboard commands routed from the window to the Picks screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickCommand { Left, Right, Skip, Neither, ClosePopover }

/// Shared shell state, provided once at the root.
#[derive(Clone, Copy)]
pub struct Shell {
    pub tab: Signal<Tab>,
    /// What the player is doing, shown as a status line; None when idle.
    pub player_status: Signal<Option<PlayerStatus>>,
    pub search_open: Signal<bool>,
    /// What is typed in Search, kept so every way of closing it (Esc, Back, Close, a result)
    /// remembers the query.
    pub search_draft: Signal<String>,
    /// A title's videos playing in the in-app theatre (trailers, teasers, featurettes).
    pub trailer: Signal<Option<Theatre>>,
    /// The app's shortcuts sheet.
    pub shortcuts_open: Signal<bool>,
    /// A key pressed on the title page (play, like, save, ...).
    pub title_command: Signal<Option<char>>,
    /// A query Search should open with (a name tapped on a title page).
    pub search_query: Signal<Option<String>>,
    pub opened: Signal<Option<Media>>,
    /// Artwork that lends the accent: the opened title, else the current pick.
    pub lamp_source: Signal<String>,
    pub pick_command: Signal<Option<PickCommand>>,
    pub history: Signal<backend::History>,
    /// Bumped after feedback from outside Picks so it can re-rank.
    pub picks_dirty: Signal<u32>,
    /// Installed add-ons with a newer version you have not applied (a dot on Settings).
    pub addon_updates: Signal<usize>,
    /// The in-app player's session while it is on screen.
    pub player: Signal<Option<engine::LiveRef>>,
    /// A key pressed while the player is on screen, for its controls.
    pub player_key: Signal<Option<String>>,
    /// Bumped when the viewer backs out of the player while its source is still being found.
    pub play_ticket: Signal<u64>,
    /// When true, Picks duels exclusively over titles saved in the user's Library ("Pick from Saved").
    pub picks_saved_only: Signal<bool>,
    /// True while a bottom sheet (`SourceSheet` / `PersonSheet`) is open on top of the Title Page.
    pub title_sheet_open: Signal<bool>,
}

impl Shell {
    /// The player is up but nothing plays yet: a source is being found or opened.
    pub fn pending(&self) -> Option<String> {
        if self.player.peek().is_some() { return None; }
        match &*self.player_status.peek() { Some(PlayerStatus::Finding(t) | PlayerStatus::Opening(t)) => Some(t.clone()), _ => None }
    }
}

/// Open the player at once while a source is found; the finder starts playback only while
/// `still_wanted` holds for the ticket returned here.
pub fn finding(mut shell: Shell, title: String) -> u64 {
    let next = *shell.play_ticket.peek() + 1;
    shell.play_ticket.set(next);
    shell.player_status.set(Some(PlayerStatus::Finding(title)));
    next
}

pub fn still_wanted(shell: Shell, ticket: u64) -> bool { *shell.play_ticket.peek() == ticket }

/// Close Search, remembering what was typed (two characters or more).
pub fn close_search(mut shell: Shell) {
    let draft = shell.search_draft.peek().clone();
    if draft.trim().chars().count() >= 2 { backend::remember_query(&mut shell.history.write(), &draft); }
    if !draft.is_empty() { shell.search_draft.set(String::new()); }
    if *shell.search_open.peek() { shell.search_open.set(false); }
}

/// How long a playback failure stays on screen.
const FAILURE_SHOWN_MS: u64 = 8000;

/// Back out of the player before anything plays: the search in progress is dropped.
pub fn cancel_finding(mut shell: Shell) {
    *shell.play_ticket.write() += 1;
    shell.player_status.set(None);
}

fn main() {
    #[cfg(target_os = "android")]
    { dioxus::launch(App); }
    #[cfg(not(target_os = "android"))]
    launch_desktop();
}

/// Developer command: `veylo-dx --sources <type> <video-id> [episodes in the season] [title] [year]`
/// prints the Play order and which source passes the check. File names, sizes and labels only; never URLs.
#[cfg(not(target_os = "android"))]
fn print_sources(kind: &str, id: &str, pack: Option<usize>, title: Option<&str>, year: Option<i32>) {
    if let Err(e) = backend::init() { eprintln!("{e:#}"); return; }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let (mut list, failed) = match backend::sources(kind, id).await { Ok(r) => r, Err(e) => { eprintln!("{e:#}"); return; } };
        println!("{} sources, offline add-ons: {failed:?}", list.len());
        let mut ctx = backend::AutoplayContext::simple(title.unwrap_or(""), year, pack);
        ctx.episode_code = backend::parse_video_episode(id);
        let pick = if title.is_some() {
            let order = backend::play_order_for_cli(&mut list, &ctx);
            for (i, s) in order.iter().take(20).enumerate() {
                println!("{:>2}. [{} {}{}] {} | {} | {}", i + 1, s.quality.unwrap_or("?"), s.hdr.unwrap_or("SDR"),
                    if s.off_title { " OFF-TITLE" } else { "" },
                    if s.filename.is_empty() { s.description.lines().next().unwrap_or("") } else { &s.filename }, s.size.as_deref().unwrap_or("size?"), s.addon);
            }
            let exact: Vec<_> = order.into_iter().filter(|s| !s.off_title).collect();
            if exact.is_empty() {
                backend::Pick { debrid: Some("no-matching-title".into()), ..Default::default() }
            } else {
                backend::pick_source_with(exact, &ctx).await
            }
        } else {
            let order = backend::play_order(&list, pack);
            for (i, s) in order.iter().take(20).enumerate() {
                println!("{:>2}. [{} {}] {} | {} | {}", i + 1, s.quality.unwrap_or("?"), s.hdr.unwrap_or("SDR"),
                    if s.filename.is_empty() { s.description.lines().next().unwrap_or("") } else { &s.filename }, s.size.as_deref().unwrap_or("size?"), s.addon);
            }
            backend::pick_source_with(order, &ctx).await
        };
        let (best, debrid) = (pick.best, pick.debrid);
        println!("Play would start: {}", best.map(|s| if s.filename.is_empty() { s.description.lines().next().unwrap_or("").to_owned() } else { s.filename }).unwrap_or_else(|| format!("nothing ({debrid:?})")));
    });
}

/// `veylo-dx --check-picture <screenshot.png> [--report <file>]`: does a screenshot taken while the
/// colour-bar clip plays (VEYLO_DX_PLAY_DEMO=bars) show its six bars? CI's picture test
/// (tools/ci/launch-check.*). The report file is for Windows, where a GUI build's output can get lost.
#[cfg(not(target_os = "android"))]
fn check_picture(path: &str, report: Option<&str>) -> i32 {
    let result = image::open(path).map_err(|e| format!("can't read {path}: {e}")).and_then(|image| {
        let image = image.to_rgb8();
        let (w, h) = image.dimensions();
        veylo_picture_check::find_bars(image.as_raw(), w as usize, h as usize, 3)
    });
    let (line, code) = match result {
        Ok(found) => (format!("PASS  picture    {}", found.summary()), 0),
        Err(e) => (format!("FAIL  picture    {e}"), 1),
    };
    println!("{line}");
    if let Some(report) = report { let _ = std::fs::write(report, format!("{line}\n")); }
    code
}

/// Deep-link manifest URL passed on launch (`stremio://...` or `veylo://install?url=...`).
static PENDING_DEEP_LINK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn take_pending_deep_link() -> Option<String> {
    if let Some(url) = PENDING_DEEP_LINK.lock().ok().and_then(|mut s| s.take()) {
        return Some(url);
    }
    #[cfg(target_os = "android")]
    { android::take_deep_link() }
    #[cfg(not(target_os = "android"))]
    { None }
}

/// Developer command: `veylo-dx --addons` checks installed add-on versions and prints names and
/// versions; never links.
#[cfg(not(target_os = "android"))]
fn print_addons() {
    if let Err(e) = backend::init() { eprintln!("{e:#}"); return; }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let updates = addons::startup().await;
        let ov = addons::overview().await;
        println!("Installed add-ons: {}, updates: {updates}", ov.addons.len());
        for r in &ov.addons { println!("  {} v{} {:?}", r.name, r.version, r.update); }
    });
}

#[cfg(not(target_os = "android"))]
fn launch_desktop() {
    let args: Vec<String> = std::env::args().collect();
    // Windows GUI builds have no console of their own: the developer commands print to the terminal that started them.
    #[cfg(windows)]
    if args.len() > 1 {
        extern "system" { fn AttachConsole(process: u32) -> i32; }
        const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS); }
    }
    if args.get(1).map(String::as_str) == Some("--sources") && args.len() >= 4 {
        print_sources(&args[2], &args[3], args.get(4).and_then(|n| n.parse().ok()), args.get(5).map(String::as_str), args.get(6).and_then(|y| y.parse().ok()));
        return;
    }
    if args.get(1).map(String::as_str) == Some("--addons") { print_addons(); return; }
    if args.get(1).map(String::as_str) == Some("--self-test") { std::process::exit(selftest::run(&args[2..])); }
    if args.get(1).map(String::as_str) == Some("--check-picture") && args.len() >= 3 {
        let report = args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1));
        std::process::exit(check_picture(&args[2], report.map(String::as_str)));
    }
    if let Some(url) = args.iter().skip(1).find_map(|a| addons::parse_deep_link(a)) {
        if let Ok(mut slot) = PENDING_DEEP_LINK.lock() { *slot = Some(url); }
    }
    use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
    // WebKitGTK's DMA-BUF renderer repaints only damaged tiles of a transparent page, so the
    // controls over the player's picture would tear; its shared-memory renderer repaints cleanly.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1"); }
    let window = WindowBuilder::new()
        .with_title("Veylo")
        .with_inner_size(LogicalSize::new(1200.0, 768.0))
        .with_min_inner_size(LogicalSize::new(360.0, 560.0))
        // WebKitGTK draws a transparent page only in a window with an alpha visual, chosen at creation:
        // the player's picture shows through it (video.rs). The page paints its own black otherwise.
        // macOS: a transparent window is what stops the WKWebView drawing a background of its own.
        .with_transparent(cfg!(any(target_os = "linux", target_os = "windows", target_os = "macos")));
    // Windows: the title bar and taskbar show the app icon (the exe carries it for Explorer: windows/veylo.rc).
    #[cfg(windows)]
    let window = window.with_window_icon(window_icon());
    dioxus::LaunchBuilder::desktop()
        .with_cfg(Config::new().with_window(window).with_menu(None).with_background_color((0, 0, 0, 255)))
        .launch(App);
}

/// The app icon (the Android launcher icon) for the window and taskbar.
#[cfg(windows)]
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    let image = image::load_from_memory(include_bytes!("../../../branding/veylo-icon.png")).ok()?.thumbnail(256, 256).to_rgba8();
    let (width, height) = image.dimensions();
    dioxus::desktop::tao::window::Icon::from_rgba(image.into_raw(), width, height).ok()
}

fn font_css() -> String {
    let enc = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let latin = "U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD";
    let ext = "U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF";
    format!(
        "@font-face{{font-family:'Outfit';font-style:normal;font-weight:100 900;font-display:block;src:url(data:font/woff2;base64,{}) format('woff2');unicode-range:{latin}}}\
         @font-face{{font-family:'Outfit';font-style:normal;font-weight:100 900;font-display:block;src:url(data:font/woff2;base64,{}) format('woff2');unicode-range:{ext}}}\
         @font-face{{font-family:'Inter';font-style:normal;font-weight:100 900;font-display:block;src:url(data:font/woff2;base64,{}) format('woff2');unicode-range:{latin}}}\
         @font-face{{font-family:'Inter';font-style:normal;font-weight:100 900;font-display:block;src:url(data:font/woff2;base64,{}) format('woff2');unicode-range:{ext}}}",
        enc(OUTFIT_LATIN),
        enc(OUTFIT_EXT),
        enc(INTER_LATIN),
        enc(INTER_EXT),
    )
}

/// Developer option: VEYLO_DX_TAB=home|picks|library|settings picks the first screen.
fn start_tab() -> Tab {
    match std::env::var("VEYLO_DX_TAB").unwrap_or_default().to_lowercase().as_str() {
        "picks" => Tab::Picks, "library" => Tab::Library, "settings" => Tab::Settings, _ => Tab::Home,
    }
}

/// The system bars' heights in CSS px (top, bottom); nothing to reserve on the desktop.
fn system_bar_insets() -> (f32, f32) {
    #[cfg(target_os = "android")]
    { android::system_bar_insets().unwrap_or((24.0, 24.0)) }
    #[cfg(not(target_os = "android"))]
    { (0.0, 0.0) }
}

/// The backend starts once the window exists: on Android the JNI context
/// (files folder, player) is only available from then on.
#[component]
fn App() -> Element {
    let started = use_hook(|| backend::init().map_err(|e| format!("{e:#}")));
    match started {
        Ok(()) => rsx! { Shell_ {} },
        Err(error) => rsx! { style { {STYLE} } div { class: "center-note", div { class: "glass panel", p { "Veylo couldn’t start" } p { class: "dim", "{error}" } } } },
    }
}

#[component]
#[allow(non_snake_case)]
fn Shell_() -> Element {
    let fonts = use_hook(font_css);
    let shell = use_context_provider(|| Shell {
        tab: Signal::new(start_tab()),
        player_status: Signal::new(None),
        search_open: Signal::new(std::env::var_os("VEYLO_DX_SEARCH").is_some()),
        search_draft: Signal::new(String::new()),
        search_query: Signal::new(None),
        shortcuts_open: Signal::new(false),
        trailer: Signal::new(None),
        title_command: Signal::new(None),
        opened: Signal::new(None),
        lamp_source: Signal::new(String::new()),
        pick_command: Signal::new(None),
        history: Signal::new(backend::load_history()),
        picks_dirty: Signal::new(0),
        addon_updates: Signal::new(0),
        player: Signal::new(None),
        player_key: Signal::new(None),
        play_ticket: Signal::new(0),
        picks_saved_only: Signal::new(false),
        title_sheet_open: Signal::new(false),
    });
    // Developer option: VEYLO_DX_PLAY_DEMO=1 (desktop) or files/dev-play.json (Android) plays at launch.
    use_hook(move || {
        video::register_window();
        #[cfg(target_os = "android")]
        if let Some(list) = android::take_dev_autoplay() { spawn(backend::dev_autoplay(list)); }
        // Desktop: VEYLO_DX_DEV_AUTOPLAY names the same kind of list.
        if let Some(list) = std::env::var_os("VEYLO_DX_DEV_AUTOPLAY").and_then(|p| std::fs::read_to_string(p).ok()) { spawn(backend::dev_autoplay(list)); }
        #[cfg(target_os = "android")]
        if let Some(v) = android::take_dev_play() {
            let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
            start_playback(shell, player::PlayRequest { title: text("title"), subtitle: "Test player".into(), url: text("url"),
                hdr: Some(text("hdr")).filter(|h| !h.is_empty()), ..Default::default() }, None);
        }
        // VEYLO_DX_PLAY_DEMO=hdr plays the self-test's HDR10 clip instead (macOS: with VEYLO_DX_EDR=4 on a Mac without HDR).
        #[cfg(not(target_os = "android"))]
        if std::env::var("VEYLO_DX_PLAY_DEMO").as_deref() == Ok("hdr") {
            let clip = std::env::temp_dir().join("veylo-demo-hdr.mp4");
            if std::fs::write(&clip, selftest::HDR_CLIP).is_ok() {
                start_playback(shell, player::PlayRequest { title: "HDR10 test clip".into(), subtitle: "Test player".into(), url: clip.to_string_lossy().into_owned(), ..Default::default() }, None);
            }
            return;
        }
        // VEYLO_DX_PLAY_DEMO=bars plays six colour bars, for CI's picture test (--check-picture).
        if std::env::var("VEYLO_DX_PLAY_DEMO").as_deref() == Ok("bars") {
            let clip = std::env::temp_dir().join("veylo-colour-bars.y4m");
            if std::fs::write(&clip, veylo_picture_check::y4m(180)).is_ok() {
                start_playback(shell, player::PlayRequest { title: "Colour bars".into(), subtitle: "Picture test".into(), url: clip.to_string_lossy().into_owned(), ..Default::default() }, None);
            }
            return;
        }
        if std::env::var_os("VEYLO_DX_PLAY_DEMO").is_some() {
            start_playback(shell, player::PlayRequest { title: "Sintel trailer".into(), subtitle: "Test player".into(), url: backend::DEMO_STREAM.into(), ..Default::default() }, None);
        }
    });
    // Once per launch: check for new versions of user-installed add-ons.
    let mut addon_updates = shell.addon_updates;
    use_future(move || async move { addon_updates.set(addons::startup().await); });
    let mut deep_link = use_signal(take_pending_deep_link);
    let Shell { tab, mut search_open, mut opened, lamp_source, mut pick_command, player_status, mut shortcuts_open, mut title_command, .. } = shell;
    let lamp = use_resource(move || async move { backend::accent(lamp_source()).await });
    let lamp_css = lamp().flatten().unwrap_or_else(|| "#f2efea".into());
    // Android draws the WebView under its bars: reserve their real heights everywhere, read again
    // whenever the window changes size (rotation, split screen, a new navigation mode).
    let mut insets = use_signal(system_bar_insets);

    // Android Back closes a page, Search or the videos, like Esc; on a top-level screen it leaves the app.
    // On a TV it never leaves: Back only goes back (to Home at most), and the remote's Home key leaves.
    #[cfg(target_os = "android")]
    use_effect(move || { let _ = android::set_back_enabled(is_tv() || deep_link.read().is_some() || shortcuts_open() || search_open() || opened.read().is_some() || (shell.player)().is_some()
        || (shell.trailer)().is_some() || matches!(player_status(), Some(PlayerStatus::Finding(_) | PlayerStatus::Opening(_)))); });
    // A failure says why once; it doesn't stay over the app (a new playback replaces it sooner).
    let mut status = shell.player_status;
    use_effect(move || {
        let Some(PlayerStatus::Failed(error)) = player_status() else { return };
        spawn(async move {
            backend::sleep(FAILURE_SHOWN_MS).await;
            if matches!(&*status.peek(), Some(PlayerStatus::Failed(e)) if *e == error) { status.set(None); }
        });
    });
    // Written in an effect, never during render (a write there would re-render forever).
    let mut accent = use_context_provider(|| Accent(Signal::new(String::new()))).0;
    let current = lamp_css.clone();
    use_effect(use_reactive!(|current| if *accent.peek() != current { accent.set(current); }));
    // The pending overlay can finish asynchronously while its help is open.
    use_effect(move || { if (shell.player)().is_some() && *shortcuts_open.peek() { shortcuts_open.set(false); } });

    // App shortcuts (the ? sheet lists them). Typing in Search or a field never triggers them.
    let on_key = move |e: KeyboardEvent| {
        if deep_link.read().is_some() {
            if matches!(e.key(), Key::Escape | Key::Backspace) {
                e.prevent_default();
                deep_link.set(None);
            }
            return;
        }
        if shortcuts_open() {
            if matches!(e.key(), Key::Escape | Key::Backspace) || e.key() == Key::Character("?".into()) {
                e.prevent_default(); shortcuts_open.set(false);
            }
            return;
        }
        // The player's controls take every key while it is on screen.
        if shell.player.peek().is_some() {
            if e.key() == Key::Tab { return; }
            e.prevent_default();
            let mut key = shell.player_key;
            key.set(Some(e.key().to_string()));
            return;
        }
        // The player waiting for its source: Esc or Back leaves it, other keys wait.
        if shell.pending().is_some() {
            if e.key() == Key::Tab { return; }
            e.prevent_default();
            if matches!(e.key(), Key::Escape | Key::Backspace) { cancel_finding(shell); }
            else if e.key() == Key::Character("?".into()) { shortcuts_open.set(true); }
            return;
        }
        let ctrl = e.modifiers().ctrl() || e.modifiers().meta();
        let go = |mut tab: Signal<Tab>, mut opened: Signal<Option<Media>>, to: Tab| { close_search(shell); opened.set(None); tab.set(to); };
        match e.key() {
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("k") => { e.prevent_default(); search_open.set(true); }
            Key::Character(c) if ctrl && ["1", "2", "3", "4"].contains(&c.as_str()) => {
                e.prevent_default();
                go(tab, opened, Tab::ALL[c.parse::<usize>().unwrap_or(1) - 1]);
            }
            Key::Escape if shell.trailer.read().is_some() => { let mut t = shell.trailer; t.set(None); }
            Key::MediaPlayPause if shell.trailer.read().is_some() => { e.prevent_default(); theatre::command("toggle"); }
            // In the theatre, ← and → step through the title's videos.
            Key::ArrowLeft | Key::ArrowRight if shell.trailer.read().is_some() => {
                e.prevent_default();
                let back = e.key() == Key::ArrowLeft;
                let mut t = shell.trailer;
                t.with_mut(|t| if let Some(t) = t { let n = t.clips.len().max(1); t.at = if back { (t.at + n - 1) % n } else { (t.at + 1) % n }; });
            }
            // The rest are YouTube's own keys, so they behave the same with focus inside the video:
            // Space or K play/pause, J and L ten seconds back and on, M mute, F full screen; plus
            // 1–9 to jump to a video, Y to open it on YouTube and ? for the list.
            Key::Character(c) if shell.trailer.read().is_some() && !ctrl && !e.modifiers().alt() => {
                let key = c.to_lowercase();
                let mut t = shell.trailer;
                match key.as_str() {
                    " " | "k" => { e.prevent_default(); theatre::command("toggle"); }
                    "j" => theatre::command("back"),
                    "l" => theatre::command("forward"),
                    "m" => theatre::command("mute"),
                    "f" => theatre::command("full"),
                    "y" => if let Some(id) = t.peek().as_ref().and_then(|t| t.clips.get(t.at)).map(|c| c.youtube.clone()) { open_external(&format!("https://www.youtube.com/watch?v={id}")); },
                    "?" => shortcuts_open.set(!shortcuts_open()),
                    d if d.len() == 1 && ("1"..="9").contains(&d) => {
                        let i = d.parse::<usize>().unwrap_or(1) - 1;
                        t.with_mut(|t| if let Some(t) = t { if i < t.clips.len() { t.at = i; } });
                    }
                    _ => {}
                }
            }
            Key::Escape if search_open() => close_search(shell),
            Key::Escape if opened.read().is_some() => {
                if *shell.title_sheet_open.peek() { title_command.set(Some('\x1b')); }
                else { opened.set(None); }
            }
            Key::Escape if !is_tv() && tab() == Tab::Picks && shell.trailer.read().is_none() => {
                pick_command.set(Some(PickCommand::ClosePopover));
            }
            // A TV's Back on another tab goes to Home (it never leaves the app).
            Key::Escape if is_tv() && tab() != Tab::Home => go(tab, opened, Tab::Home),
            _ if search_open() || shell.trailer.read().is_some() || ctrl || e.modifiers().alt() => {}
            Key::Backspace if opened.read().is_some() => {
                if *shell.title_sheet_open.peek() { title_command.set(Some('\x1b')); }
                else { opened.set(None); }
            }
            Key::Character(c) if c == "?" => shortcuts_open.set(!shortcuts_open()),
            Key::Character(c) if c == "/" => { e.prevent_default(); search_open.set(true); }
            // Title page: P play, L like, D not for me, S save, W watched, T trailer, O sources.
            Key::Character(c) if opened.read().is_some() => {
                if !*shell.title_sheet_open.peek() { title_command.set(c.to_lowercase().chars().next()); }
            }
            _ if tab() != Tab::Picks || e.is_auto_repeating() || e.is_composing() => {}
            Key::ArrowLeft => { e.prevent_default(); pick_command.set(Some(PickCommand::Left)); }
            Key::ArrowRight => { e.prevent_default(); pick_command.set(Some(PickCommand::Right)); }
            Key::Character(c) if c.eq_ignore_ascii_case("s") => { e.prevent_default(); pick_command.set(Some(PickCommand::Skip)); }
            Key::Character(c) if c.eq_ignore_ascii_case("n") => { e.prevent_default(); pick_command.set(Some(PickCommand::Neither)); }
            _ => {}
        }
    };
    // Keys pressed while nothing in the app has focus still reach the app's shortcuts.
    use_hook(|| { document::eval(r#"window.addEventListener('keydown', e => {
        if (e.target === document.body) { const a = document.querySelector('.app'); if (a) { a.focus(); a.dispatchEvent(new KeyboardEvent('keydown', e)); } }
    });"#); });
    // Android TV: the remote moves focus and focus lights the room (tv.js, styles/tv.css).
    use_hook(|| if cfg!(target_os = "android") { document::eval(include_str!("tv.js")); });
    // Lightweight WebView monitor: forwards JS runtime errors and sheet/dialog horizontal layout
    // overflows to Firebase diagnostics when telemetry is enabled.
    use_future(|| async move {
        let mut eval = document::eval(r#"(() => {
            const seen = new Set();
            const send = (summary, details) => {
                const key = summary.slice(0, 80);
                if (seen.has(key) || seen.size > 10) return;
                seen.add(key);
                dioxus.send({ summary, details });
            };
            window.addEventListener('error', e => {
                send('JS error: ' + (e.message || 'unknown'), (e.filename || '') + ':' + (e.lineno || 0) + ':' + (e.colno || 0));
            });
            window.addEventListener('unhandledrejection', e => {
                const msg = e.reason ? String(e.reason) : 'unhandled rejection';
                send('Unhandled promise: ' + msg.slice(0, 140), msg);
            });
            let timer = null;
            const checkLayout = () => {
                const sheets = document.querySelectorAll('.bottom-sheet, .player-sheet, .panel');
                if (!sheets.length) return;
                const vw = window.innerWidth || 0;
                if (vw <= 0) return;
                for (const el of sheets) {
                    const r = el.getBoundingClientRect();
                    if (r.width === 0 || r.height === 0) continue;
                    if (r.right > vw + 16 || r.left < -16) {
                        send('UI overflow on .' + (el.className || '').split(' ')[0],
                            'class=' + el.className + ' rect=' + Math.round(r.left) + '..' + Math.round(r.right) + ' width=' + Math.round(r.width) + ' viewport=' + vw);
                        break;
                    }
                }
            };
            new MutationObserver(() => {
                clearTimeout(timer);
                timer = setTimeout(checkLayout, 450);
            }).observe(document.body, { childList: true, subtree: true });
        })();"#);
        while let Ok(v) = eval.recv::<serde_json::Value>().await {
            let summary = v["summary"].as_str().unwrap_or_default();
            let details = v["details"].as_str().unwrap_or_default();
            if !summary.is_empty() {
                backend::report_issue_bg("ui_error", summary.to_owned(), details.to_owned());
            }
        }
    });
    // Scroll shadow, per-tab scroll restoration, title scroll-to-top, pill slider, and mouse glare:
    // combined into one rAF-throttled MutationObserver instead of multiple observers.
    use_hook(|| { document::eval(r#"(() => {
        const savedScroll = Object.create(null);
        let lastScrolled = null;
        let lastHeroHost = null;
        let lastHeroY = -1;
        const screenOf = p => {
            if (!p || !p.firstElementChild) return null;
            const cls = p.firstElementChild.classList;
            for (const c of ['home', 'library', 'settings', 'duel']) {
                if (cls.contains(c)) return c;
            }
            return null;
        };
        const sync = () => {
            const p = document.querySelector('.page');
            const y = p ? p.scrollTop : 0;
            const scrolled = y > 4;
            if (scrolled !== lastScrolled) {
                lastScrolled = scrolled;
                document.documentElement.classList.toggle('scrolled', scrolled);
            }
            // Smooth GPU hero parallax on TitlePage and Home: drives a single clamped CSS var with zero reflow.
            const first = p && p.firstElementChild;
            const heroHost = first && (first.classList.contains('title-page') || first.classList.contains('home')) ? first : null;
            if (heroHost) {
                const hy = Math.min(320, Math.max(0, Math.round(y)));
                if (heroHost !== lastHeroHost || hy !== lastHeroY) {
                    lastHeroHost = heroHost;
                    lastHeroY = hy;
                    heroHost.style.setProperty('--hero-y', hy + 'px');
                }
            } else {
                lastHeroHost = null;
                lastHeroY = -1;
            }
            const s = screenOf(p);
            if (s && p) savedScroll[s] = y;
        };
        let scrollRaf = false;
        const queueSync = () => {
            if (scrollRaf) return;
            scrollRaf = true;
            requestAnimationFrame(() => { scrollRaf = false; sync(); });
        };
        // Horizontal shelf edge-fades and desktop scroll chevrons.
        const syncStrip = wrap => {
            const st = wrap.querySelector(':scope > .strip');
            if (!st) return;
            const left = st.scrollLeft > 4;
            const right = st.scrollLeft + st.clientWidth < st.scrollWidth - 4;
            if (wrap.hasAttribute('data-left') !== left) wrap.toggleAttribute('data-left', left);
            if (wrap.hasAttribute('data-right') !== right) wrap.toggleAttribute('data-right', right);
        };
        const syncStrips = () => document.querySelectorAll('.strip-wrap').forEach(syncStrip);
        const pendingStrips = new Set();
        let stripRaf = false;
        document.addEventListener('scroll', e => {
            const t = e.target;
            if (!t || !t.classList) return;
            if (t.classList.contains('page')) {
                queueSync();
            } else if (t.classList.contains('strip') && t.parentElement && t.parentElement.classList.contains('strip-wrap')) {
                pendingStrips.add(t.parentElement);
                if (!stripRaf) {
                    stripRaf = true;
                    requestAnimationFrame(() => {
                        stripRaf = false;
                        pendingStrips.forEach(syncStrip);
                        pendingStrips.clear();
                    });
                }
            }
        }, { capture: true, passive: true });
        document.addEventListener('click', e => {
            const btn = e.target.closest && e.target.closest('.strip-arrow');
            if (!btn) return;
            const wrap = btn.closest('.strip-wrap');
            const st = wrap && wrap.querySelector(':scope > .strip');
            if (!st) return;
            const dir = Number(btn.dataset.dir || '1');
            st.scrollBy({ left: dir * Math.max(240, st.clientWidth * 0.78), behavior: 'smooth' });
        }, true);
        const restoreScroll = () => {
            const p = document.querySelector('.page');
            if (!p) return;
            const title = p.querySelector('.title-page');
            if (title) {
                if (!title.dataset.topDone) { title.dataset.topDone = '1'; p.scrollTop = 0; sync(); }
                return;
            }
            const s = screenOf(p);
            const el = s && p.querySelector('.' + s);
            if (el && !el.dataset.restored) {
                el.dataset.restored = '1';
                p.scrollTop = savedScroll[s] || 0;
                sync();
            }
        };
        // Selection pills slide: batch all layout reads before writes so no read forces a reflow.
        const slide = () => {
            const plans = [];
            document.querySelectorAll('.dock, .segmented').forEach(g => {
                const s = g.querySelector('.slider'), on = g.querySelector('.dock-tab.on, .seg.on');
                if (!s) return;
                if (!on) { plans.push({ g, s, hide: true }); return; }
                const at = [on.offsetLeft, on.offsetTop, on.offsetWidth, on.offsetHeight];
                const key = at.join();
                if (s.dataset.at === key) return;
                plans.push({ g, s, at, key, first: !s.dataset.at });
            });
            for (const p of plans) {
                if (p.hide) { p.s.style.opacity = '0'; continue; }
                p.s.dataset.at = p.key;
                if (p.first) p.s.style.transition = 'none';
                Object.assign(p.s.style, {
                    transform: `translate3d(${p.at[0]}px, ${p.at[1]}px, 0)`,
                    width: p.at[2] + 'px',
                    height: p.at[3] + 'px',
                    opacity: '1',
                });
                p.g.dataset.sliding = '';
                if (p.first) requestAnimationFrame(() => requestAnimationFrame(() => { p.s.style.transition = ''; }));
            }
        };
        addEventListener('resize', () => requestAnimationFrame(() => { slide(); syncStrips(); }));
        // A new tab slides in from the side you moved toward.
        document.addEventListener('click', e => {
            const t = e.target.closest && e.target.closest('.dock-tab');
            if (!t) return;
            const tabs = [...document.querySelectorAll('.dock-tab')], from = tabs.findIndex(x => x.classList.contains('on'));
            document.documentElement.style.setProperty('--dir', tabs.indexOf(t) < from ? '-1' : '1');
        }, true);
        // Light on glass: mouse-only rAF-throttled glare so touch scrolling never touches layout.
        let glareArt = null, glareX = 0, glareY = 0, glareRaf = false;
        const glare = e => {
            if (e.pointerType !== 'mouse') return;
            const art = e.target.closest && e.target.closest('.card-art');
            if (!art) return;
            glareArt = art; glareX = e.clientX; glareY = e.clientY;
            if (glareRaf) return;
            glareRaf = true;
            requestAnimationFrame(() => {
                glareRaf = false;
                if (!glareArt || !glareArt.isConnected) return;
                const r = glareArt.getBoundingClientRect();
                if (r.width <= 0 || r.height <= 0) return;
                glareArt.style.setProperty('--mx', ((glareX - r.left) / r.width * 100).toFixed(1) + '%');
                glareArt.style.setProperty('--my', ((glareY - r.top) / r.height * 100).toFixed(1) + '%');
            });
        };
        document.addEventListener('pointermove', glare, { passive: true });
        // Single rAF-throttled observer for scroll shadow, title scroll-reset, pill sliders, and shelf fades.
        // Skips all work while the player is active and only refreshes strip fades when DOM children change.
        let framed = false, dirtyStrips = false;
        new MutationObserver(mutations => {
            if (document.querySelector('.app.playing')) return;
            if (mutations.some(m => m.type === 'childList')) dirtyStrips = true;
            if (framed) return;
            framed = true;
            requestAnimationFrame(() => {
                framed = false;
                if (document.querySelector('.app.playing')) { dirtyStrips = false; return; }
                const needStrips = dirtyStrips;
                dirtyStrips = false;
                restoreScroll(); sync(); slide();
                if (needStrips) syncStrips();
            });
        }).observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class'] });
    })();"#); });

    // The ambient light keeps the previous picture under the new one while it fades in, then
    // prunes back to one layer after 1.1s so scrolling never blurs two full-screen images at once.
    let mut lamp_layers = use_signal(Vec::<String>::new);
    use_effect(move || {
        let raw = lamp_source();
        if raw.is_empty() {
            if !lamp_layers.peek().is_empty() { lamp_layers.set(Vec::new()); }
            return;
        }
        let now = backend::ambient_url(&raw);
        let mut layers = lamp_layers.peek().clone();
        if layers.last() == Some(&now) { return; }
        layers.retain(|s| s != &now);
        layers.push(now.clone());
        if layers.len() > 2 { layers.remove(0); }
        let need_prune = layers.len() > 1;
        lamp_layers.set(layers);
        if need_prune {
            spawn(async move {
                backend::sleep(1100).await;
                if lamp_layers.peek().last() == Some(&now) {
                    lamp_layers.set(vec![now]);
                }
            });
        }
    });

    rsx! {
        style { {fonts} }
        style { {STYLE} }
        div {
            class: format!("app{}{}", if cfg!(target_os = "android") { " android" } else { "" }, if (shell.player)().is_some() { " playing" } else { "" }),
            style: "--lamp: {lamp_css}; --top-inset: {insets().0}px; --bottom-inset: {insets().1}px",
            onresize: move |_| if cfg!(target_os = "android") { let now = system_bar_insets(); if *insets.peek() != now { insets.set(now); } },
            tabindex: "-1",
            onkeydown: on_key,
            onmounted: move |e| async move { let _ = e.set_focus(true).await; },
            // The light the glass refracts: the title in front, blurred into the room.
            div { class: "ambient", aria_hidden: "true",
                for src in lamp_layers() { img { key: "{src}", src: "{src}", alt: "" } }
            }
            // Android's status bar sits on solid black on every screen, above every overlay.
            div { class: "status-guard", aria_hidden: "true" }
            TopBar { deep_link_open: deep_link.read().is_some() }
            // `inert` is a boolean attribute: present means on, even as inert="false". Omit it when unused.
            main { class: "page", key: "{opened.read().as_ref().map(|m| m.key()).unwrap_or_default()}",
                inert: (search_open() || shortcuts_open() || shell.trailer.read().is_some() || deep_link.read().is_some()).then_some("true"),
                if let Some(media) = opened() { for media in [media] { title::TitlePage { key: "{media.key()}", media } } }
                else { match tab() {
                    Tab::Home => rsx! { home::Home {} },
                    Tab::Picks => rsx! { picks::Picks {} },
                    Tab::Settings => rsx! { settings::SettingsPage {} },
                    Tab::Library => rsx! { library::Library {} },
                } }
            }
            if let Some(status) = player_status() { if (shell.player)().is_none() {
                match status {
                    PlayerStatus::Finding(title) => rsx! { player_ui::PendingPlayer { title, step: "Finding a source" } },
                    PlayerStatus::Opening(title) => rsx! { player_ui::PendingPlayer { title, step: "Opening" } },
                    status => rsx! { StatusChip { status } },
                }
            } }
            if let Some(live) = (shell.player)() { for live in [live] { player_ui::PlayerScreen { key: "{live.0.session_id}", live } } }
            // Another of your devices asking for your keys, or yours arriving here.
            if (shell.player)().is_none() { account_ui::KeysPrompt {} }
            if search_open() { search::SearchOverlay {} }
            if shortcuts_open() { shortcuts::AppShortcuts {} }
            // Windows, macOS and Android install releases; Linux builds from source.
            // Keep mounted so "Later" and active downloads survive opening Search, Player, or Theatre.
            if updater::ASSET.is_some() {
                div {
                    style: if (shell.player)().is_some() || shell.pending().is_some() || search_open() || shell.trailer.read().is_some() { "display: none;" } else { "" },
                    updater::UpdateNotice {}
                }
            }
            if let Some(theatre) = (shell.trailer)() { theatre::TheatreSheet { theatre } }
            if let Some(url) = deep_link() {
                DeepLinkInstallModal { url, on_close: move |_| deep_link.set(None) }
            }
        }
    }
}

/// Confirmation dialog when Veylo is opened with a `stremio://` or `veylo://` add-on link.
/// Never auto-fetches untrusted deep-link URLs on mount: the user inspects the host first.
#[component]
fn DeepLinkInstallModal(url: String, on_close: EventHandler<()>) -> Element {
    let host = addons::host_of(&url);
    let mut inspecting = use_signal(|| false);
    let mut preview = use_signal(|| None::<Result<addons::ManifestPreview, String>>);
    let mut installing = use_signal(|| false);
    let mut err = use_signal(|| None::<String>);
    let inspect_url = url.clone();
    let mut do_inspect = move || {
        if *inspecting.peek() { return; }
        let u = inspect_url.clone();
        inspecting.set(true);
        preview.set(None);
        spawn(async move {
            let res = addons::preview_manifest(&u).await.map_err(|e| format!("{e:#}"));
            preview.set(Some(res));
            inspecting.set(false);
        });
    };
    rsx! {
        div { class: "scrim", role: "dialog", aria_modal: "true", aria_label: "Install Stremio add-on",
            div { class: "modal-card glass",
                div { class: "modal-head",
                    span { class: "group-icon", icons::Stack {} }
                    div { class: "setting-copy",
                        span { class: "group-name", "Install add-on?" }
                        span { class: "dim setting-about", "An external link asked Veylo to add an add-on from {host}." }
                    }
                }
                match preview() {
                    None if !inspecting() => rsx! {
                        div { class: "modal-preview",
                            p { class: "dim setting-about", "Host: {host}" }
                            p { class: "setting-about", "Inspect the add-on's manifest before deciding whether to install it." }
                        }
                        div { class: "modal-actions",
                            button { class: "pill glass small",
                                onmounted: move |e| async move { let _ = e.set_focus(true).await; },
                                onclick: move |_| on_close.call(()), span { "Cancel" } }
                            button { class: "pill accent small", onclick: move |_| do_inspect(), icons::Search {} span { "Inspect add-on" } }
                        }
                    },
                    None => rsx! {
                        p { class: "dim setting-about", "Reading add-on manifest from {host}…" }
                        div { class: "modal-actions",
                            button { class: "pill glass small", onclick: move |_| on_close.call(()), span { "Cancel" } }
                        }
                    },
                    Some(Err(e)) => rsx! {
                        p { class: "addon-problem", role: "alert", icons::Alert {} span { "{e}" } }
                        div { class: "modal-actions",
                            button { class: "pill glass small", onclick: move |_| on_close.call(()), span { "Close" } }
                        }
                    },
                    Some(Ok(info)) => rsx! {
                        div { class: "modal-preview",
                            div { class: "setting-title-line",
                                span { class: "setting-name", "{info.name}" }
                                if !info.version.is_empty() { span { class: "status-pill muted tnum", "v{info.version}" } }
                            }
                            p { class: "dim setting-about", "Host: {info.host}" if !info.offers.is_empty() { " · {info.offers}" } }
                            if !info.description.is_empty() {
                                p { class: "setting-about", "{info.description}" }
                            }
                        }
                        if let Some(e) = err() {
                            p { class: "addon-problem", role: "alert", icons::Alert {} span { "{e}" } }
                        }
                        div { class: "modal-actions",
                            button { class: "pill glass small", disabled: installing(), onclick: move |_| on_close.call(()), span { "Cancel" } }
                            button { class: "pill accent small", disabled: installing(),
                                onclick: move |_| {
                                    let target = info.url.clone();
                                    installing.set(true);
                                    err.set(None);
                                    spawn(async move {
                                        match addons::install_custom(target).await {
                                            Ok(_) => {
                                                crate::home::invalidate_cache();
                                                on_close.call(());
                                            }
                                            Err(e) => {
                                                err.set(Some(format!("{e:#}")));
                                                installing.set(false);
                                            }
                                        }
                                    });
                                },
                                icons::Plus {} span { if installing() { "Installing…" } else { "Install" } }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Wordmark, a glass dock of destination icons (the selected one shows its
/// name), and the search orb.
#[component]
fn TopBar(deep_link_open: bool) -> Element {
    let mut shell = use_context::<Shell>();
    let bar_inert = deep_link_open || (shell.shortcuts_open)() || (shell.search_open)() || shell.trailer.read().is_some() || (shell.title_sheet_open)();
    rsx! {
        header { class: "topbar", inert: bar_inert.then_some("true"),
            if shell.opened.read().is_some() {
                icons::IconButton { label: "Back", class: "glass", onclick: move |_| shell.opened.set(None), icons::ArrowLeft {} }
            } else {
            span { class: "wordmark display", "Veylo" }
            nav { class: "dock glass", role: "tablist", aria_label: "Destinations",
                // The pill that slides to the selected tab (positioned in the motion script below).
                i { class: "slider", aria_hidden: "true" }
                for t in Tab::ALL {
                    button {
                        class: if (shell.tab)() == t { "dock-tab on" } else { "dock-tab" },
                        role: "tab",
                        aria_selected: "{(shell.tab)() == t}",
                        aria_label: "{t.title()}",
                        title: "{t.title()}",
                        onclick: move |_| shell.tab.set(t),
                        match t {
                            Tab::Home => rsx! { icons::House {} },
                            Tab::Picks => rsx! { icons::Sparkles {} },
                            Tab::Library => rsx! { icons::Stack {} },
                            Tab::Settings => rsx! { icons::Sliders {} },
                        }
                        if (shell.tab)() == t { span { "{t.title()}" } }
                        if t == Tab::Settings && (shell.addon_updates)() > 0 { i { class: "tab-dot", aria_label: "Add-on updates" } }
                    }
                }
            }
            }
            span { class: "topbar-spacer" }
            LastPlayed {}
            shortcuts::ShortcutButton {}
            icons::IconButton { label: "Search (Ctrl+K)", class: "glass", onclick: move |_| shell.search_open.set(true), icons::Search {} }
        }
    }
}

#[component]
fn StatusChip(status: PlayerStatus) -> Element {
    let mut current = use_context::<Shell>().player_status;
    rsx! {
        div { class: "toast glass", role: "status",
            match status {
                PlayerStatus::Finding(title) => rsx! { span { class: "spinner", aria_hidden: "true" } span { class: "toast-title", "{title}" } span { class: "chip", "Finding a source" } },
                PlayerStatus::Opening(title) => rsx! { span { class: "lamp", icons::Play {} } span { class: "toast-title", "{title}" } },
                PlayerStatus::Playing { title, codec, hardware, facts } => {
                    let lit = hardware.is_some();
                    let decoder = hardware.unwrap_or_else(|| "software".into());
                    let hdr = facts.picture != "SDR" && !facts.picture.is_empty();
                    rsx! {
                    span { class: "lamp", icons::Play {} }
                    span { class: "toast-title", "{title}" }
                    if !facts.resolution.is_empty() { span { class: "chip", "{facts.resolution}" } }
                    if !codec.is_empty() { span { class: "chip", "{codec}" } }
                    if !facts.picture.is_empty() { span { class: if hdr { "chip lit" } else { "chip" }, title: "Picture", "{facts.picture}" } }
                    if !facts.size.is_empty() { span { class: "chip tnum", title: "File size", "{facts.size}" } }
                    span { class: if lit { "chip hw" } else { "chip sw" },
                        title: if lit { "Hardware decoding" } else { "Software decoding" },
                        icons::Cpu {} span { "{decoder}" } }
                }},
                PlayerStatus::Failed(error) => rsx! {
                    span { class: "warn-icon", icons::Alert {} } span { "{error}" }
                    icons::IconButton { label: "Dismiss", class: "small", onclick: move |_| current.set(None), icons::Close {} }
                },
            }
        }
    }
}

/// Tries the next source from where the failed one stopped, told whether it was for the audio language.
pub type TryAnother = Box<dyn Fn(Shell, i64, bool)>;
pub type SwitchSource = Box<dyn Fn(Shell, usize, i64)>;
pub type SwitchEpisode = Box<dyn Fn(Shell, String)>;

/// Optional callbacks fired when playback finishes with a navigation or source-switch action.
#[derive(Default)]
pub struct PlaybackHooks {
    pub on_next: Option<Box<dyn Fn(Shell)>>,
    pub on_prev: Option<Box<dyn Fn(Shell)>>,
    pub on_another: Option<TryAnother>,
    pub on_switch_source: Option<SwitchSource>,
    pub on_switch_episode: Option<SwitchEpisode>,
}

/// Play through the platform player and narrate what it is doing. `on_next` plays the next episode.
pub fn start_playback(shell: Shell, request: player::PlayRequest, on_next: Option<Box<dyn Fn(Shell)>>) {
    start_playback_hooks(shell, request, PlaybackHooks { on_next, ..Default::default() })
}

/// `start_playback`, with full navigation and source-switching hooks.
pub fn start_playback_hooks(mut shell: Shell, mut request: player::PlayRequest, hooks: PlaybackHooks) {
    if request.accent.is_empty() { request.accent = try_consume_context::<Accent>().map(|a| a.0.peek().clone()).unwrap_or_default(); }
    let title = request.title.clone();
    let ticket = *shell.play_ticket.peek() + 1;
    shell.play_ticket.set(ticket);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    shell.player_status.set(Some(PlayerStatus::Opening(title.clone())));
    player::play(request, tx);
    follow_playback_events(shell, title, ticket, rx, hooks);
}

fn follow_playback_events(mut shell: Shell, title: String, ticket: u64, mut rx: tokio::sync::mpsc::UnboundedReceiver<player::PlayerEvent>,
    hooks: PlaybackHooks) {
    // Playback belongs to the app, even when its source chooser closes immediately.
    dioxus::core::spawn_forever(async move {
        // This playback's in-app session: only it may update status or take the player off screen.
        let mut mine: Option<engine::LiveRef> = None;
        let is_current = move |mine: &Option<engine::LiveRef>| {
            *shell.play_ticket.peek() == ticket || (mine.is_some() && *shell.player.peek() == *mine)
        };
        let mut clear = move |mine: &Option<engine::LiveRef>| if mine.is_some() && *shell.player.peek() == *mine { shell.player.set(None); };
        while let Some(event) = rx.recv().await {
            match event {
                player::PlayerEvent::Started { decoder, codec, facts } => {
                    if is_current(&mine) {
                        shell.player_status.set(Some(PlayerStatus::Playing { title: title.clone(), codec, hardware: decoder, facts }));
                    }
                }
                player::PlayerEvent::Attached(live) => {
                    if *shell.play_ticket.peek() == ticket {
                        mine = Some(live.clone());
                        shell.player.set(Some(live));
                    } else {
                        live.0.finish(engine::Finish::Close);
                    }
                }
                player::PlayerEvent::Failed(error) => {
                    clear(&mine);
                    if is_current(&mine) {
                        shell.player_status.set(Some(PlayerStatus::Failed(error)));
                    }
                    break;
                }
                player::PlayerEvent::Finished { next, prev, another, language, switch_source, switch_episode, position_ms, .. } => {
                    let current = is_current(&mine);
                    clear(&mine);
                    if current { shell.player_status.set(None); }
                    backend::flush_progress().await;
                    *shell.picks_dirty.write() += 1;
                    if current {
                        if let Some(idx) = switch_source { if let Some(cb) = &hooks.on_switch_source { cb(shell, idx, position_ms); } }
                        else if let Some(ep) = switch_episode { if let Some(cb) = &hooks.on_switch_episode { cb(shell, ep); } }
                        else if next { if let Some(play_next) = &hooks.on_next { play_next(shell); } }
                        else if prev { if let Some(play_prev) = &hooks.on_prev { play_prev(shell); } }
                        else if another { if let Some(try_another) = &hooks.on_another { try_another(shell, position_ms, language); } }
                    }
                    break;
                }
            }
        }
    });
}

/// Open a link outside Veylo, only ever on an explicit tap (trailers on YouTube).
pub fn open_external(url: &str) {
    if !url.starts_with("https://") { return; }
    #[cfg(target_os = "android")]
    { let _ = android::open_url(url); }
    #[cfg(target_os = "linux")]
    { let _ = std::process::Command::new("xdg-open").arg(url).spawn(); }
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg(url).spawn(); }
    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn(); }
}

/// The last thing you played, always top right: a play button ringed by how far you got (its
/// name in the tooltip). One tap carries on at once (the player closes when you leave it).
#[component]
fn LastPlayed() -> Element {
    let shell = use_context::<Shell>();
    let last = use_resource(move || { let _ = (shell.picks_dirty)(); async { backend::recent_viewing(4).await.ok().and_then(|l| l.into_iter().next()) } });
    match last() {
        Some(Some(seen)) => rsx! { for m in [seen.media] { LastPlayedButton { key: "{m.key()}", media: m.clone() } } },
        _ => rsx! {},
    }
}

#[component]
fn LastPlayedButton(media: Media) -> Element {
    let shell = use_context::<Shell>();
    // A thin ring around the play button: how far you got. Only a title you are partway
    // through resumes, so only then does the ring show progress.
    const AROUND: f64 = 141.37; // 2π × 22.5
    let resumes = backend::in_progress(&media);
    let done = if resumes { (backend::progress_fraction(&media) * AROUND).clamp(0.0, AROUND) } else { 0.0 };
    // One quiet line under the name: where you are, or what a tap will do.
    let line = if resumes { Some(backend::resume_context(&media)).filter(|c| !c.is_empty() && c != "In progress").unwrap_or_else(|| "Carry on".into()) }
        else if media.kind == "movie" { "Watch again".into() }
        else if media.video_watched { "Next episode".into() }
        else { "Carry on".into() };
    let label = format!("{line}: {}", media.name);
    let m = media.clone();
    let ahead = media.clone();
    use_hook(move || title::prefetch_resume(&ahead));
    rsx! {
        button { class: "last-played glass", title: "{label}", aria_label: "{label}",
            onclick: move |_| title::resume_now(shell, m.clone()),
            span { class: "lp-orb",
                svg { class: "lp-ring", view_box: "0 0 48 48", "aria-hidden": "true",
                    circle { class: "lp-track", cx: "24", cy: "24", r: "22.5" }
                    if done > 0.0 { circle { class: "lp-done", cx: "24", cy: "24", r: "22.5", style: "stroke-dasharray: {done:.1} 200" } }
                }
                icons::Play {}
            }
            span { class: "lp-text",
                span { class: "lp-name", "{media.name}" }
                span { class: "lp-line", "{line}" }
            }
        }
    }
}

#[cfg(test)]
mod playback_lifetime_tests {
    use super::*;
    use std::{cell::{Cell, RefCell}, rc::Rc};

    #[derive(Clone, Default)]
    struct Probe {
        show: Rc<Cell<Option<Signal<bool>>>>,
        status: Rc<Cell<Option<Signal<Option<PlayerStatus>>>>>,
        sender: Rc<RefCell<Option<tokio::sync::mpsc::UnboundedSender<player::PlayerEvent>>>>,
    }

    fn harness(probe: Probe) -> Element {
        let show = use_signal(|| true);
        probe.show.set(Some(show));
        use_context_provider(|| probe.clone());
        use_context_provider(|| {
            let status = Signal::new(None);
            probe.status.set(Some(status));
            Shell {
                tab: Signal::new(Tab::Home), player_status: status,
                search_open: Signal::new(false), search_draft: Signal::new(String::new()), trailer: Signal::new(None), shortcuts_open: Signal::new(false),
                title_command: Signal::new(None), search_query: Signal::new(None), opened: Signal::new(None),
                lamp_source: Signal::new(String::new()), pick_command: Signal::new(None),
                history: Signal::new(backend::History::default()), picks_dirty: Signal::new(0), addon_updates: Signal::new(0),
                player: Signal::new(None), player_key: Signal::new(None), play_ticket: Signal::new(0), picks_saved_only: Signal::new(false), title_sheet_open: Signal::new(false),
            }
        });
        rsx! { if show() { TestSourceChooser {} } }
    }

    #[component]
    fn TestSourceChooser() -> Element {
        let shell = use_context::<Shell>();
        let probe = use_context::<Probe>();
        use_hook(move || {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            *probe.sender.borrow_mut() = Some(tx);
            let ticket = *shell.play_ticket.peek();
            follow_playback_events(shell, "Test film".into(), ticket, rx, PlaybackHooks::default());
        });
        rsx! { div {} }
    }

    #[test]
    fn playback_events_survive_closing_the_source_chooser() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let probe = Probe::default();
            let mut dom = VirtualDom::new_with_props(harness, probe.clone());
            dom.rebuild_in_place();
            probe.show.get().unwrap().set(false);
            dom.render_immediate_to_vec();
            // The real event receiver must remain alive after the chooser is unmounted.
            probe.sender.borrow().as_ref().unwrap().send(player::PlayerEvent::Started {
                decoder: None, codec: "HEVC".into(), facts: Default::default(),
            }).expect("closing the chooser must not cancel playback");
            let _ = tokio::time::timeout(std::time::Duration::from_millis(20), dom.wait_for_work()).await;
            assert!(matches!(&*probe.status.get().unwrap().peek(), Some(PlayerStatus::Playing { title, .. }) if title == "Test film"));
        });
    }
}
