//! Veylo shell prototype in Dioxus: Picks and Search on the Rust core.
// Windows release builds are a GUI app: no console window behind Veylo.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod addons;
#[cfg(target_os = "android")]
mod android;
mod backend;
mod brain;
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
mod title;
mod video;
mod updater;
mod display;

use backend::Media;
use base64::Engine;
use dioxus::prelude::*;

const STYLE: &str = include_str!("style.css");
const ARCHIVO: &[u8] = include_bytes!("../assets/fonts/archivo.ttf");

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
pub enum PickCommand { Left, Right, Skip, Neither }

/// One video in the theatre: a YouTube id, its name and kind ("Trailer", "Teaser", ...).
#[derive(Clone, Debug, PartialEq)]
pub struct Clip { pub youtube: String, pub name: String, pub kind: String, pub official: bool }

/// The in-app video player: whose videos they are, the videos and the one playing.
#[derive(Clone, Debug, PartialEq)]
pub struct Theatre { pub about: About, pub clips: Vec<Clip>, pub at: usize }

/// The title the videos belong to, from what its page or card already loaded: its logo and
/// backdrop, year, runtime, certificate and genres, rating, tagline, director and leading cast.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct About {
    pub title: String, pub backdrop: String, pub logo: Option<String>,
    pub facts: Vec<String>, pub rating: Option<String>, pub tagline: Option<String>,
    pub director: Option<(String, Vec<String>)>, pub cast: Vec<String>,
}

impl About {
    pub fn of(media: &Media, meta: Option<&backend::TitleMeta>, art: Option<&backend::Artwork>) -> Self {
        let filled = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
        let genres: Vec<String> = media.genres.iter().take(2).cloned().collect();
        let facts = [filled(&media.year), meta.and_then(|m| filled(&m.runtime)), art.and_then(|a| a.certificate.clone()),
            Some(genres.join(", ")).filter(|g| !g.is_empty())].into_iter().flatten().collect();
        // Key crew from TMDB (directed or created by), else the add-on's directors.
        let director = art.and_then(|a| a.crew.iter().find(|(role, _)| role.starts_with("Directed") || role.starts_with("Created")).cloned())
            .or_else(|| meta.filter(|m| !m.directors.is_empty()).map(|m| ("Directed by".to_owned(), m.directors.iter().take(2).cloned().collect())));
        let cast: Vec<String> = art.map(|a| a.cast.iter().take(4).map(|p| p.name.clone()).collect::<Vec<_>>()).filter(|c| !c.is_empty())
            .or_else(|| meta.map(|m| m.cast.iter().take(4).cloned().collect())).unwrap_or_default();
        About {
            title: media.name.clone(),
            backdrop: art.and_then(|a| a.backdrop.clone()).unwrap_or_else(|| media.backdrop().to_owned()),
            logo: art.and_then(|a| a.logo.clone()).or_else(|| meta.and_then(|m| filled(&m.logo))),
            facts, rating: meta.and_then(|m| filled(&m.rating)), tagline: art.and_then(|a| a.tagline.clone()), director, cast,
        }
    }
}

impl Clip {
    /// What the clip is, in words: "Official trailer", "Teaser", "Behind the scenes".
    pub fn label(&self) -> String {
        let kind = if self.kind == "Behind the Scenes" { "Behind the scenes".to_owned() } else if self.kind.is_empty() { "Video".to_owned() } else { self.kind.clone() };
        if self.official && matches!(self.kind.as_str(), "Trailer" | "Teaser") { format!("Official {}", kind.to_lowercase()) } else { kind }
    }
}

/// Shared shell state, provided once at the root.
#[derive(Clone, Copy)]
pub struct Shell {
    pub tab: Signal<Tab>,
    /// What the player is doing, shown as a status line; None when idle.
    pub player_status: Signal<Option<PlayerStatus>>,
    pub search_open: Signal<bool>,
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
    shell.player_status.set(Some(PlayerStatus::Finding(title)));
    *shell.play_ticket.peek()
}

pub fn still_wanted(shell: Shell, ticket: u64) -> bool { *shell.play_ticket.peek() == ticket }

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

/// Developer command: `veylo-dx --sources <type> <video-id> [episodes in the season]` prints the Play order and which
/// source passes the one-byte check. File names, sizes and labels only; never URLs.
#[cfg(not(target_os = "android"))]
fn print_sources(kind: &str, id: &str, pack: Option<usize>) {
    if let Err(e) = backend::init() { eprintln!("{e:#}"); return; }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let (list, failed) = match backend::sources(kind, id).await { Ok(r) => r, Err(e) => { eprintln!("{e:#}"); return; } };
        println!("{} sources, offline add-ons: {failed:?}", list.len());
        let order = backend::play_order(&list, pack);
        for (i, s) in order.iter().take(8).enumerate() {
            println!("{:>2}. [{} {}] {} | {} | {}", i + 1, s.quality.unwrap_or("?"), s.hdr.unwrap_or("SDR"),
                if s.filename.is_empty() { s.description.lines().next().unwrap_or("") } else { &s.filename }, s.size.as_deref().unwrap_or("size?"), s.addon);
        }
        let (best, debrid) = backend::first_working(order).await;
        println!("Play would start: {}", best.map(|s| if s.filename.is_empty() { s.description.lines().next().unwrap_or("").to_owned() } else { s.filename }).unwrap_or_else(|| format!("nothing ({debrid:?})")));
    });
}

/// Developer command: `veylo-dx --addons` runs the start-up add-on setup (key adoption,
/// replacing website installs, update check) and prints names and versions; never links.
#[cfg(not(target_os = "android"))]
fn print_addons() {
    if let Err(e) = backend::init() { eprintln!("{e:#}"); return; }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let updates = addons::startup().await;
        let ov = addons::overview().await;
        println!("Real-Debrid key: {}, updates: {updates}", if backend::load_settings().rd_key.is_some() { "set" } else { "missing" });
        for (c, row) in &ov.curated { println!("{}: {}", c.name(), row.as_ref().map(|r| format!("v{} {:?}", r.version, r.update)).unwrap_or_else(|| "not installed".into())); }
        for r in &ov.others { println!("other: {} v{} {:?}", r.name, r.version, r.update); }
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
    // macOS: the player's Vulkan driver (MoltenVK) travels inside Veylo.app (tools/build-macos.sh).
    #[cfg(target_os = "macos")]
    if let Some(app) = updater::app_bundle() {
        let icd = app.join("Contents/Resources/vulkan/icd.d/MoltenVK_icd.json");
        if icd.is_file() && std::env::var_os("VK_DRIVER_FILES").is_none() && std::env::var_os("VK_ICD_FILENAMES").is_none() {
            std::env::set_var("VK_DRIVER_FILES", &icd);
            std::env::set_var("VK_ICD_FILENAMES", &icd);
        }
    }
    if args.get(1).map(String::as_str) == Some("--sources") && args.len() >= 4 { print_sources(&args[2], &args[3], args.get(4).and_then(|n| n.parse().ok())); return; }
    if args.get(1).map(String::as_str) == Some("--addons") { print_addons(); return; }
    if args.get(1).map(String::as_str) == Some("--self-test") { std::process::exit(selftest::run(&args[2..])); }
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
        .with_transparent(cfg!(any(target_os = "linux", target_os = "windows")));
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
    let data = base64::engine::general_purpose::STANDARD.encode(ARCHIVO);
    format!("@font-face{{font-family:'Archivo';src:url(data:font/ttf;base64,{data}) format('truetype');\
        font-weight:100 900;font-stretch:62% 125%;font-display:block}}")
}

/// Developer option: VEYLO_DX_TAB=home|picks|library|settings picks the first screen.
fn start_tab() -> Tab {
    match std::env::var("VEYLO_DX_TAB").unwrap_or_default().to_lowercase().as_str() {
        "picks" => Tab::Picks, "library" => Tab::Library, "settings" => Tab::Settings, _ => Tab::Home,
    }
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
    });
    // Linux: the GL area the player draws into goes under the WebView once, before anything plays.
    #[cfg(target_os = "linux")]
    use_hook(video::install);
    // Developer option: VEYLO_DX_PLAY_DEMO=1 (desktop) or files/dev-play.json (Android) plays at launch.
    use_hook(move || {
        #[cfg(target_os = "android")]
        if let Some(v) = android::take_dev_play() {
            let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
            start_playback(shell, player::PlayRequest { title: text("title"), subtitle: "Test player".into(), url: text("url"),
                hdr: Some(text("hdr")).filter(|h| !h.is_empty()), ..Default::default() }, None);
        }
        if std::env::var_os("VEYLO_DX_PLAY_DEMO").is_some() {
            start_playback(shell, player::PlayRequest { title: "Sintel trailer".into(), subtitle: "Test player".into(), url: backend::DEMO_STREAM.into(), ..Default::default() }, None);
        }
    });
    // Once per launch: adopt the debrid key, replace website installs, look for new versions.
    let mut addon_updates = shell.addon_updates;
    use_future(move || async move { addon_updates.set(addons::startup().await); });
    let Shell { tab, mut search_open, mut opened, lamp_source, mut pick_command, player_status, mut shortcuts_open, mut title_command, .. } = shell;
    let lamp = use_resource(move || async move { backend::accent(lamp_source()).await });
    let lamp_css = lamp().flatten().unwrap_or_else(|| "#f2efea".into());
    // Android draws the WebView under its bars: reserve their real heights everywhere.
    let insets = use_hook(|| {
        #[cfg(target_os = "android")]
        { android::system_bar_insets().unwrap_or((24.0, 24.0)) }
        #[cfg(not(target_os = "android"))]
        { (0.0f32, 0.0f32) }
    });

    // Android Back closes a page or Search, like Esc; on a top-level screen it leaves the app.
    #[cfg(target_os = "android")]
    use_effect(move || { let _ = android::set_back_enabled(shortcuts_open() || search_open() || opened.read().is_some() || (shell.player)().is_some()
        || matches!(player_status(), Some(PlayerStatus::Finding(_) | PlayerStatus::Opening(_)))); });
    // Written in an effect, never during render (a write there would re-render forever).
    let mut accent = use_context_provider(|| Accent(Signal::new(String::new()))).0;
    let current = lamp_css.clone();
    use_effect(use_reactive!(|current| if *accent.peek() != current { accent.set(current); }));
    // The pending overlay can finish asynchronously while its help is open.
    use_effect(move || { if (shell.player)().is_some() && *shortcuts_open.peek() { shortcuts_open.set(false); } });

    // App shortcuts (the ? sheet lists them). Typing in Search or a field never triggers them.
    let on_key = move |e: KeyboardEvent| {
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
        let go = |mut tab: Signal<Tab>, mut opened: Signal<Option<Media>>, mut search_open: Signal<bool>, to: Tab| { opened.set(None); search_open.set(false); tab.set(to); };
        match e.key() {
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("k") => { e.prevent_default(); search_open.set(true); }
            Key::Character(c) if ctrl && ["1", "2", "3", "4"].contains(&c.as_str()) => {
                e.prevent_default();
                go(tab, opened, search_open, Tab::ALL[c.parse::<usize>().unwrap_or(1) - 1]);
            }
            Key::Escape if shortcuts_open() && shell.trailer.read().is_some() => shortcuts_open.set(false),
            Key::Escape if shell.trailer.read().is_some() => { let mut t = shell.trailer; t.set(None); }
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
                    " " | "k" => { e.prevent_default(); theatre_command("toggle"); }
                    "j" => theatre_command("back"),
                    "l" => theatre_command("forward"),
                    "m" => theatre_command("mute"),
                    "f" => theatre_command("full"),
                    "y" => if let Some(id) = t.peek().as_ref().and_then(|t| t.clips.get(t.at)).map(|c| c.youtube.clone()) { open_external(&format!("https://www.youtube.com/watch?v={id}")); },
                    "?" => shortcuts_open.set(!shortcuts_open()),
                    d if d.len() == 1 && ("1"..="9").contains(&d) => {
                        let i = d.parse::<usize>().unwrap_or(1) - 1;
                        t.with_mut(|t| if let Some(t) = t { if i < t.clips.len() { t.at = i; } });
                    }
                    _ => {}
                }
            }
            Key::Escape if shortcuts_open() => shortcuts_open.set(false),
            Key::Escape if search_open() => search_open.set(false),
            Key::Escape if opened.read().is_some() => opened.set(None),
            _ if search_open() || shortcuts_open() || shell.trailer.read().is_some() || ctrl || e.modifiers().alt() => {}
            Key::Backspace if shortcuts_open() => shortcuts_open.set(false),
            Key::Backspace if opened.read().is_some() => opened.set(None),
            Key::Character(c) if c == "?" => shortcuts_open.set(!shortcuts_open()),
            Key::Character(c) if c == "/" => { e.prevent_default(); search_open.set(true); }
            // Title page: P play, L like, D not for me, S save, W watched, T trailer, O sources.
            Key::Character(c) if opened.read().is_some() => title_command.set(c.to_lowercase().chars().next()),
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
    // The top bar gathers smoke once the page scrolls under it (style.css, .scrolled). Checked on
    // scroll and, at most once a frame, when the page changes (a new tab or title starts at the top).
    use_hook(|| { document::eval(r#"(() => {
        const sync = () => { const p = document.querySelector('.page'); document.documentElement.classList.toggle('scrolled', !!p && p.scrollTop > 4); };
        let queued = false;
        const later = () => { if (!queued) { queued = true; requestAnimationFrame(() => { queued = false; sync(); }); } };
        document.addEventListener('scroll', e => { if (e.target.classList && e.target.classList.contains('page')) sync(); }, true);
        new MutationObserver(later).observe(document.body, { childList: true, subtree: true });
        // Opening a title: its backdrop opens out of the card you tapped (a clip that widens from
        // the card's place to the whole hero). Nothing is stretched or swapped; the words lift in
        // after (style.css). Remembered on tap, played when the page mounts.
        let from = null;
        document.addEventListener('click', e => {
            const card = e.target.closest && e.target.closest('.card, .week-open');
            const art = card && (card.querySelector('.card-art') || card);
            from = art ? { r: art.getBoundingClientRect(), at: performance.now() } : null;
        }, true);
        const grow = () => {
            const page = document.querySelector('.title-page');
            if (!page || page.dataset.grown) return;
            page.dataset.grown = '1';
            // A title opens at its top, whatever the page behind it was scrolled to.
            const scroller = document.querySelector('.page'); if (scroller) scroller.scrollTop = 0;
            if (!from) return;
            const f = from; from = null;
            const hero = page.querySelector('.title-hero');
            if (!hero || performance.now() - f.at > 1500 || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
            const h = hero.getBoundingClientRect(), clamp = (v, max) => Math.max(0, Math.min(v, max));
            const top = clamp(f.r.top - h.top, h.height - 40), left = clamp(f.r.left - h.left, h.width - 40);
            const bottom = clamp(h.bottom - f.r.bottom, h.height - top - 40), right = clamp(h.right - f.r.right, h.width - left - 40);
            hero.style.clipPath = `inset(${top}px ${right}px ${bottom}px ${left}px round 8px)`;
            hero.getBoundingClientRect();
            hero.style.transition = 'clip-path .6s cubic-bezier(.16,1,.3,1)';
            hero.style.clipPath = 'inset(0 0 0 0 round 0px)';
            setTimeout(() => { hero.style.transition = ''; hero.style.clipPath = ''; }, 650);
        };
        // Selection pills slide: the dock's glass pill and each segmented control's thumb move to the
        // selected item and take its width. Groups whose slider is placed get data-sliding, so the
        // item's own fill steps aside (style.css); without this script the items keep their fill.
        const slide = () => document.querySelectorAll('.dock, .segmented').forEach(g => {
            const s = g.querySelector('.slider'), on = g.querySelector('.dock-tab.on, .seg.on');
            if (!s) return;
            if (!on) { s.style.opacity = '0'; return; }
            const at = [on.offsetLeft, on.offsetTop, on.offsetWidth, on.offsetHeight];
            if (s.dataset.at === at.join()) return;
            const first = !s.dataset.at; s.dataset.at = at.join();
            if (first) s.style.transition = 'none';
            Object.assign(s.style, { left: at[0] + 'px', top: at[1] + 'px', width: at[2] + 'px', height: at[3] + 'px', opacity: '1' });
            g.dataset.sliding = '';
            if (first) requestAnimationFrame(() => requestAnimationFrame(() => { s.style.transition = ''; }));
        });
        addEventListener('resize', () => requestAnimationFrame(slide));
        // A new tab slides in from the side you moved toward.
        document.addEventListener('click', e => {
            const t = e.target.closest && e.target.closest('.dock-tab');
            if (!t) return;
            const tabs = [...document.querySelectorAll('.dock-tab')], from = tabs.findIndex(x => x.classList.contains('on'));
            document.documentElement.style.setProperty('--dir', tabs.indexOf(t) < from ? '-1' : '1');
        }, true);
        // Light on glass: a soft highlight follows the finger or pointer over a poster.
        const glare = e => { const art = e.target.closest && e.target.closest('.card-art');
            if (!art) return; const r = art.getBoundingClientRect();
            art.style.setProperty('--mx', ((e.clientX - r.left) / r.width * 100).toFixed(1) + '%');
            art.style.setProperty('--my', ((e.clientY - r.top) / r.height * 100).toFixed(1) + '%'); };
        document.addEventListener('pointermove', glare, { passive: true });
        document.addEventListener('pointerdown', glare, { passive: true });
        new MutationObserver(() => requestAnimationFrame(() => { grow(); slide(); })).observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class'] });
    })();"#); });

    rsx! {
        style { {fonts} }
        style { {STYLE} }
        div {
            class: format!("app{}{}", if cfg!(target_os = "android") { " android" } else { "" }, if (shell.player)().is_some() { " playing" } else { "" }),
            style: "--lamp: {lamp_css}; --top-inset: {insets.0}px; --bottom-inset: {insets.1}px",
            tabindex: "-1",
            onkeydown: on_key,
            onmounted: move |e| async move { let _ = e.set_focus(true).await; },
            // The light the glass refracts: the title in front, blurred into the room.
            div { class: "ambient", aria_hidden: "true",
                if !lamp_source().is_empty() { img { key: "{lamp_source}", src: "{lamp_source}", alt: "" } }
            }
            // Android's status bar sits on solid black on every screen, above every overlay.
            div { class: "status-guard", aria_hidden: "true" }
            TopBar {}
            // `inert` is a boolean attribute: present means on, even as inert="false". Omit it when unused.
            main { class: "page", key: "{opened.read().as_ref().map(|m| m.key()).unwrap_or_default()}", inert: (search_open() || shortcuts_open()).then_some("true"),
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
            if let Some(live) = (shell.player)() { player_ui::PlayerScreen { live } }
            if search_open() { search::SearchOverlay {} }
            if shortcuts_open() { AppShortcuts {} }
            // Windows, macOS and Android install releases; Linux builds from source.
            if updater::ASSET.is_some() { updater::UpdateNotice {} }
            if let Some(theatre) = (shell.trailer)() { TheatreSheet { theatre } }
        }
    }
}

/// Wordmark, a glass dock of destination icons (the selected one shows its
/// name), and the search orb.
#[component]
fn TopBar() -> Element {
    let mut shell = use_context::<Shell>();
    rsx! {
        header { class: "topbar", inert: (shell.shortcuts_open)().then_some("true"),
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
            ShortcutButton {}
            icons::IconButton { label: "Search (Ctrl+K)", class: "glass", onclick: move |_| shell.search_open.set(true), icons::Search {} }
        }
    }
}

#[component]
fn StatusChip(status: PlayerStatus) -> Element {
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
                PlayerStatus::Failed(error) => rsx! { span { class: "warn-icon", icons::Alert {} } span { "{error}" } },
            }
        }
    }
}

/// Play through the platform player and narrate what it is doing.
/// `on_next` plays the next episode; `on_another` tries the next source when this one failed.
pub fn start_playback(shell: Shell, request: player::PlayRequest, on_next: Option<Box<dyn Fn(Shell)>>) {
    start_playback_with(shell, request, on_next, None)
}

pub fn start_playback_with(mut shell: Shell, mut request: player::PlayRequest, on_next: Option<Box<dyn Fn(Shell)>>, on_another: Option<Box<dyn Fn(Shell, i64)>>) {
    if request.accent.is_empty() { request.accent = try_consume_context::<Accent>().map(|a| a.0.peek().clone()).unwrap_or_default(); }
    let title = request.title.clone();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    shell.player_status.set(Some(PlayerStatus::Opening(title.clone())));
    player::play(request, tx);
    follow_playback_events(shell, title, rx, on_next, on_another);
}

fn follow_playback_events(mut shell: Shell, title: String, mut rx: tokio::sync::mpsc::UnboundedReceiver<player::PlayerEvent>,
    on_next: Option<Box<dyn Fn(Shell)>>, on_another: Option<Box<dyn Fn(Shell, i64)>>) {
    // Playback belongs to the app, even when its source chooser closes immediately.
    dioxus::core::spawn_forever(async move {
        // This playback's in-app session: only it may take the player off screen.
        let mut mine: Option<engine::LiveRef> = None;
        let mut clear = move |mine: &Option<engine::LiveRef>| if mine.is_some() && *shell.player.peek() == *mine { shell.player.set(None); };
        while let Some(event) = rx.recv().await {
            match event {
                player::PlayerEvent::Started { decoder, codec, facts } =>
                    shell.player_status.set(Some(PlayerStatus::Playing { title: title.clone(), codec, hardware: decoder, facts })),
                player::PlayerEvent::Attached(live) => { mine = Some(live.clone()); shell.player.set(Some(live)); }
                player::PlayerEvent::Failed(error) => { clear(&mine); shell.player_status.set(Some(PlayerStatus::Failed(error))); break; }
                player::PlayerEvent::Finished { next, another, position_ms, .. } => {
                    clear(&mine);
                    shell.player_status.set(None);
                    backend::flush_progress().await;
                    *shell.picks_dirty.write() += 1;
                    if next { if let Some(play_next) = &on_next { play_next(shell); } }
                    if another { if let Some(try_another) = &on_another { try_another(shell, position_ms); } }
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
    let last = use_resource(move || { let _ = (shell.picks_dirty)(); async { backend::viewing_history().await.ok().and_then(|l| l.into_iter().next()) } });
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

/// The same discoverable control in the top bar and Search; player uses its own sheet.
#[component]
fn ShortcutButton() -> Element {
    let mut shell = use_context::<Shell>();
    rsx! { icons::IconButton { label: "Page shortcuts (?)", class: "glass", onclick: move |_| shell.shortcuts_open.set(true), icons::Keyboard {} } }
}

/// Only commands that apply here, followed by shared app navigation.
#[component]
fn AppShortcuts() -> Element {
    let mut shell = use_context::<Shell>();
    let mut rows: Vec<(&[&str], &str)> = Vec::new();
    let context = if shell.pending().is_some() {
        rows.push((&["Esc"], "Cancel opening")); "Opening"
    } else if let Some(t) = (shell.trailer)() {
        rows.extend([(&["Space", "K"][..], "Play or pause"), (&["J", "L"][..], "10 seconds back or on"), (&["M"][..], "Mute or unmute"), (&["F"][..], "Full screen")]);
        if t.clips.len() > 1 { rows.extend([(&["←", "→"][..], "Previous or next video"), (&["1–9"][..], "Jump to a video")]); }
        rows.extend([(&["Y"][..], "Open on YouTube"), (&["Esc"][..], "Close the videos")]); "Videos"
    } else if (shell.search_open)() {
        rows.extend([(&["Enter"][..], "Search"), (&["Esc"][..], "Close search")]); "Search"
    } else if let Some(media) = shell.opened.read().as_ref() {
        rows.extend([(&["P"][..], "Play or resume"), (&["L"][..], "Like"), (&["D"][..], "Not for me"), (&["S"][..], "Save"), (&["T"][..], "Trailer")]);
        if media.kind == "movie" { rows.extend([(&["W"][..], "Watched"), (&["O"][..], "Choose source")]); }
        rows.push((&["Esc"], "Back")); "Title"
    } else {
        if (shell.tab)() == Tab::Picks {
            rows.extend([(&["←", "→"][..], "Keep the left or right card"), (&["S"][..], "Skip: two other titles, nothing learned"),
                (&["N"][..], "Neither: steer away from both")]);
        }
        (shell.tab)().title()
    };
    if shell.pending().is_none() {
        rows.extend([(&["Ctrl", "K"][..], "Search"), (&["Ctrl", "1–4"][..], "Home / Picks / Library / Settings")]);
        if !(shell.search_open)() { rows.push((&["/"], "Search")); }
    }
    rows.extend([(&["Tab"][..], "Next control; Shift+Tab goes back"), (&["Enter", "Space"][..], "Activate a focused button"), (&["?"][..], "Show or hide shortcuts")]);
    rsx! {
        div { class: "scrim shortcuts-scrim", onclick: move |_| shell.shortcuts_open.set(false),
            section { class: "shortcuts glass", role: "dialog", aria_modal: "true", aria_label: "{context} shortcuts", tabindex: "-1", onmounted: move |e| async move { let _ = e.set_focus(true).await; }, onclick: move |e| e.stop_propagation(),
                div { class: "sheet-head",
                    span { class: "group-icon", icons::Keyboard {} }
                    span { class: "group-name", "{context} shortcuts" }
                    icons::IconButton { label: "Close (Esc)", class: "glass", onclick: move |_| shell.shortcuts_open.set(false), icons::Close {} }
                }
                div { class: "shortcut-list",
                    for (keys, what) in rows {
                        div { class: "shortcut",
                            span { class: "caps", for k in keys.iter() { kbd { class: "keycap", "{k}" } } }
                            span { "{what}" }
                        }
                    }
                }
            }
        }
    }
}

/// A title's videos in the in-app theatre, starting at `start` (a YouTube id) or the best one.
/// TMDB's videos lead (official trailers first); the add-on's YouTube trailers follow as
/// alternatives. With no YouTube video at all, an add-on's direct trailer file plays in the player.
pub fn watch_videos(mut shell: Shell, about: About, videos: Vec<backend::Video>, trailers: Vec<backend::Trailer>, start: Option<String>) {
    let mut clips: Vec<Clip> = videos.into_iter().map(|v| Clip { youtube: v.youtube, name: v.name, kind: v.kind, official: v.official }).collect();
    for id in trailers.iter().filter_map(|t| t.youtube.clone()) {
        if !clips.iter().any(|c| c.youtube == id) {
            let n = clips.iter().filter(|c| c.kind == "Trailer").count();
            clips.push(Clip { youtube: id, name: if n == 0 { "Trailer".into() } else { format!("Trailer {}", n + 1) }, kind: "Trailer".into(), official: false });
        }
    }
    if clips.is_empty() {
        if let Some(url) = trailers.iter().find_map(|t| t.url.clone()) {
            start_playback(shell, player::PlayRequest { title: format!("{}: trailer", about.title), url, ..Default::default() }, None);
        }
        return;
    }
    let at = start.and_then(|s| clips.iter().position(|c| c.youtube == s)).unwrap_or(0);
    shell.trailer.set(Some(Theatre { about, clips, at }));
}

/// Drive the theatre's YouTube embed through its player API (postMessage). The embed reports its
/// state, time and mute back (`theatre_listen`), so toggles and seeks start from what it is doing.
fn theatre_command(command: &'static str) {
    document::eval(&format!(r#"(() => {{
        const f = document.querySelector('.theatre iframe'); if (!f || !f.contentWindow) return;
        const s = window.__veyloYt || (window.__veyloYt = {{}});
        const post = (func, args) => f.contentWindow.postMessage(JSON.stringify({{ event: 'command', func, args: args || [] }}), '*');
        switch ('{command}') {{
            case 'toggle': post(s.state === 1 ? 'pauseVideo' : 'playVideo'); s.state = s.state === 1 ? 2 : 1; break;
            case 'back': post('seekTo', [Math.max(0, (s.time || 0) - 10), true]); s.time = Math.max(0, (s.time || 0) - 10); break;
            case 'forward': post('seekTo', [(s.time || 0) + 10, true]); s.time = (s.time || 0) + 10; break;
            case 'mute': post(s.muted ? 'unMute' : 'mute'); s.muted = !s.muted; break;
            case 'full': {{ const go = f.requestFullscreen || f.webkitRequestFullscreen; if (go) go.call(f); break; }}
        }}
    }})()"#));
}

/// Listen to the embed's reports once per app, and keep asking the current embed to report
/// (a new video is a new embed).
fn theatre_listen() {
    document::eval(r#"(() => {
        if (window.__veyloYtListening) return;
        window.__veyloYtListening = true;
        window.__veyloYt = {};
        window.addEventListener('message', e => {
            if (!/youtube/.test(e.origin || '')) return;
            let d; try { d = typeof e.data === 'string' ? JSON.parse(e.data) : e.data; } catch (_) { return; }
            const s = window.__veyloYt;
            if (d && d.event === 'onStateChange') s.state = d.info;
            if (d && d.event === 'infoDelivery' && d.info) {
                if ('playerState' in d.info) s.state = d.info.playerState;
                if ('currentTime' in d.info) s.time = d.info.currentTime;
                if ('muted' in d.info) s.muted = d.info.muted;
            }
        });
        setInterval(() => {
            const f = document.querySelector('.theatre iframe');
            if (f && f.contentWindow) f.contentWindow.postMessage(JSON.stringify({ event: 'listening', id: 'veylo' }), '*');
        }, 1000);
    })()"#);
}

/// The theatre: the official YouTube embed large, what is playing and its kind, and every video of
/// the title as a strip of thumbnails to switch between. Nothing autoplays until it is opened, and
/// closing it stops it.
#[component]
fn TheatreSheet(theatre: Theatre) -> Element {
    let mut shell = use_context::<Shell>();
    let n = theatre.clips.len();
    let at = theatre.at.min(n.saturating_sub(1));
    let clip = theatre.clips[at].clone();
    let mut go = move |to: usize| shell.trailer.with_mut(|t| if let Some(t) = t { t.at = to; });
    let link = format!("https://www.youtube.com/watch?v={}", clip.youtube);
    let about = theatre.about.clone();
    let mut logo_failed = use_signal(|| false);
    use_hook(theatre_listen);
    rsx! {
        div { class: "scrim theatre-scrim", onclick: move |_| shell.trailer.set(None),
            if !about.backdrop.is_empty() { img { class: "theatre-glow", src: "{about.backdrop}", alt: "" } }
            section { class: "theatre glass", role: "dialog", aria_modal: "true", aria_label: "Videos: {about.title}", onclick: move |e| e.stop_propagation(),
                // The video is the main actor: it takes all the room the side column leaves.
                div { class: "theatre-stage",
                    div { class: "trailer-frame",
                        // A new video gets a new frame (a key only renews an element inside a list).
                        for id in [clip.youtube.clone()] {
                            iframe { key: "{id}", src: "https://www.youtube-nocookie.com/embed/{id}?autoplay=1&playsinline=1&rel=0&modestbranding=1&enablejsapi=1", title: "{clip.name}",
                                allow: "autoplay; encrypted-media; picture-in-picture; fullscreen", allowfullscreen: "true", referrerpolicy: "strict-origin-when-cross-origin" }
                        }
                    }
                }
                aside { class: "theatre-side",
                    div { class: "theatre-tools",
                        if n > 1 {
                            icons::IconButton { label: "Previous video (←)", class: "glass small", onclick: move |_| go((at + n - 1) % n), icons::ArrowLeft {} }
                            span { class: "theatre-count tnum", "{at + 1} / {n}" }
                            icons::IconButton { label: "Next video (→)", class: "glass small", onclick: move |_| go((at + 1) % n), icons::ArrowRight {} }
                        }
                        span { class: "theatre-spacer" }
                        icons::IconButton { label: "Open on YouTube", class: "glass small", onclick: move |_| open_external(&link), icons::Link {} }
                        icons::IconButton { label: "Close (Esc)", class: "glass small", onclick: move |_| shell.trailer.set(None), icons::Close {} }
                    }
                    div { class: "theatre-titles",
                        match about.logo.clone().filter(|_| !logo_failed()) {
                            Some(src) => rsx! { img { class: "theatre-logo", src: "{src}", alt: "{about.title}", onerror: move |_| logo_failed.set(true) } },
                            None => rsx! { h2 { class: "display theatre-title", "{about.title}" } },
                        }
                        div { class: "theatre-facts",
                            if let Some(r) = about.rating.clone() { span { class: "theatre-rating", icons::Star {} span { class: "tnum", "{r}" } } }
                            for f in about.facts.iter() { span { "{f}" } }
                        }
                    }
                    div { class: "theatre-now",
                        span { class: if clip.official { "theatre-kind official" } else { "theatre-kind" },
                            if clip.official { icons::Check {} } else { icons::Clapper {} } span { {clip.label()} } }
                        if !clip.name.is_empty() && clip.name != clip.label() { span { class: "theatre-name", "{clip.name}" } }
                    }
                    if let Some(t) = about.tagline.clone() { p { class: "theatre-tagline", "“{t}”" } }
                    if about.director.is_some() || !about.cast.is_empty() {
                        div { class: "theatre-credits",
                            if let Some((role, names)) = about.director.clone() { p { span { class: "dim", "{role} " } {names.join(", ")} } }
                            if !about.cast.is_empty() { p { span { class: "dim", "With " } {about.cast.join(", ")} } }
                        }
                    }
                    if n > 1 {
                        div { class: "theatre-list", role: "list", aria_label: "All videos",
                            for (i, c) in theatre.clips.iter().cloned().enumerate() {
                                button { key: "{c.youtube}", class: if i == at { "theatre-clip on" } else { "theatre-clip" }, role: "listitem",
                                    aria_current: if i == at { "true" } else { "false" }, aria_label: "{c.label()}: {c.name}", onclick: move |_| go(i),
                                    span { class: "theatre-thumb",
                                        img { src: "https://i.ytimg.com/vi/{c.youtube}/mqdefault.jpg", alt: "", loading: "lazy" }
                                        if i == at { span { class: "now-playing", icons::Play {} } }
                                        if i < 9 { kbd { class: "keycap clip-key", aria_hidden: "true", "{i + 1}" } }
                                    }
                                    span { class: "theatre-clip-text",
                                        span { class: "theatre-clip-name", "{c.name}" }
                                        span { class: if c.official { "theatre-clip-kind official" } else { "theatre-clip-kind" }, {c.label()} }
                                    }
                                }
                            }
                        }
                    }
                    // The keys, at a glance (? lists them all).
                    div { class: "theatre-keys", aria_hidden: "true",
                        span { kbd { class: "keycap", "Space" } "Play" }
                        span { kbd { class: "keycap", "J" } kbd { class: "keycap", "L" } "10 s" }
                        span { kbd { class: "keycap", "M" } "Mute" }
                        span { kbd { class: "keycap", "F" } "Full" }
                        if n > 1 { span { kbd { class: "keycap", "←" } kbd { class: "keycap", "→" } "Videos" } }
                    }
                }
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
                search_open: Signal::new(false), trailer: Signal::new(None), shortcuts_open: Signal::new(false),
                title_command: Signal::new(None), search_query: Signal::new(None), opened: Signal::new(None),
                lamp_source: Signal::new(String::new()), pick_command: Signal::new(None),
                history: Signal::new(backend::History::default()), picks_dirty: Signal::new(0), addon_updates: Signal::new(0),
                player: Signal::new(None), player_key: Signal::new(None), play_ticket: Signal::new(0),
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
            follow_playback_events(shell, "Test film".into(), rx, None, None);
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
