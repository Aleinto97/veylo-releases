//! Veylo shell prototype in Dioxus: Picks and Search on the Rust core.
// Windows release builds are a GUI app: no console window behind Veylo.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod addons;
#[cfg(target_os = "android")]
mod android;
mod backend;
mod engine;
mod home;
mod library;
mod icons;
mod picks;
mod picks_ai;
mod picks_metadata;
mod picks_rules;
mod player;
mod player_ui;
mod search;
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
pub enum PickCommand { Left, Right }

/// Shared shell state, provided once at the root.
#[derive(Clone, Copy)]
pub struct Shell {
    pub tab: Signal<Tab>,
    /// What the player is doing, shown as a status line; None when idle.
    pub player_status: Signal<Option<PlayerStatus>>,
    pub search_open: Signal<bool>,
    /// A YouTube trailer playing in the in-app trailer sheet (video id).
    pub trailer: Signal<Option<(Vec<String>, usize)>>,
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
    if args.get(1).map(String::as_str) == Some("--sources") && args.len() >= 4 { print_sources(&args[2], &args[3], args.get(4).and_then(|n| n.parse().ok())); return; }
    if args.get(1).map(String::as_str) == Some("--addons") { print_addons(); return; }
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
    use_effect(move || { let _ = android::set_back_enabled(search_open() || opened.read().is_some() || (shell.player)().is_some()
        || matches!(player_status(), Some(PlayerStatus::Finding(_) | PlayerStatus::Opening(_)))); });
    // Written in an effect, never during render (a write there would re-render forever).
    let mut accent = use_context_provider(|| Accent(Signal::new(String::new()))).0;
    let current = lamp_css.clone();
    use_effect(use_reactive!(|current| if *accent.peek() != current { accent.set(current); }));
    // App shortcuts (the ? sheet lists them). Typing in Search or a field never triggers them.
    let on_key = move |e: KeyboardEvent| {
        // The player's controls take every key while it is on screen.
        if shell.player.peek().is_some() {
            e.prevent_default();
            let mut key = shell.player_key;
            key.set(Some(e.key().to_string()));
            return;
        }
        // The player waiting for its source: Esc or Back leaves it, other keys wait.
        if shell.pending().is_some() {
            e.prevent_default();
            if matches!(e.key(), Key::Escape | Key::Backspace) { cancel_finding(shell); }
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
            Key::Escape if shell.trailer.read().is_some() => { let mut t = shell.trailer; t.set(None); }
            Key::Escape if shortcuts_open() => shortcuts_open.set(false),
            Key::Escape if search_open() => search_open.set(false),
            Key::Escape if opened.read().is_some() => opened.set(None),
            _ if search_open() || ctrl => {}
            Key::Backspace if shortcuts_open() => shortcuts_open.set(false),
            Key::Backspace if opened.read().is_some() => opened.set(None),
            Key::Character(c) if c == "?" => shortcuts_open.set(!shortcuts_open()),
            Key::Character(c) if c == "/" => { e.prevent_default(); search_open.set(true); }
            // Title page: P play, L like, D not for me, S save, W watched, T trailer, O sources.
            Key::Character(c) if opened.read().is_some() => title_command.set(c.to_lowercase().chars().next()),
            _ if tab() != Tab::Picks => {}
            Key::ArrowLeft => pick_command.set(Some(PickCommand::Left)),
            Key::ArrowRight => pick_command.set(Some(PickCommand::Right)),
            _ => {}
        }
    };
    // Keys pressed while nothing in the app has focus still reach the app's shortcuts.
    use_hook(|| { document::eval(r#"window.addEventListener('keydown', e => {
        if (e.target === document.body) { const a = document.querySelector('.app'); if (a) { a.focus(); a.dispatchEvent(new KeyboardEvent('keydown', e)); } }
    });"#); });

    rsx! {
        style { {fonts} }
        style { {STYLE} }
        div {
            class: format!("app{}{}", if cfg!(target_os = "android") { " android" } else { "" }, if (shell.player)().is_some() { " playing" } else { "" }),
            style: "--lamp: {lamp_css}; --top-inset: {insets.0}px; --bottom-inset: {insets.1}px",
            tabindex: "-1",
            onkeydown: on_key,
            onkeyup: move |e: KeyboardEvent| if shell.player.peek().is_some() { e.prevent_default(); },
            onmounted: move |e| async move { let _ = e.set_focus(true).await; },
            // The light the glass refracts: the title in front, blurred into the room.
            div { class: "ambient", aria_hidden: "true",
                if !lamp_source().is_empty() { img { key: "{lamp_source}", src: "{lamp_source}", alt: "" } }
            }
            // Android's status bar sits on solid black on every screen, above every overlay.
            div { class: "status-guard", aria_hidden: "true" }
            TopBar {}
            // `inert` is a boolean attribute: present means on, even as inert="false". Omit it when unused.
            main { class: "page", key: "{opened.read().as_ref().map(|m| m.key()).unwrap_or_default()}", inert: search_open().then_some("true"),
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
            // Windows and Android install releases; Linux builds from source.
            if cfg!(any(windows, target_os = "android")) { updater::UpdateNotice {} }
            if let Some((ids, at)) = (shell.trailer)() { for id in [ids[at.min(ids.len() - 1)].clone()] { TrailerSheet { key: "{id}", id, more: ids.len() > 1 } } }
        }
    }
}

/// Wordmark, a glass dock of destination icons (the selected one shows its
/// name), and the search orb.
#[component]
fn TopBar() -> Element {
    let mut shell = use_context::<Shell>();
    rsx! {
        header { class: "topbar",
            if shell.opened.read().is_some() {
                icons::IconButton { label: "Back", class: "glass", onclick: move |_| shell.opened.set(None), icons::ArrowLeft {} }
            } else {
            span { class: "wordmark display", "Veylo" }
            nav { class: "dock glass", role: "tablist", aria_label: "Destinations",
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

/// Every app shortcut, as key caps (the player has its own sheet: H or ?).
#[component]
fn AppShortcuts() -> Element {
    let mut shell = use_context::<Shell>();
    let rows: [(&[&str], &str); 12] = [
        (&["Ctrl", "K"], "Search"), (&["/"], "Search"), (&["Ctrl", "1"], "Home"), (&["Ctrl", "2"], "Picks"),
        (&["Ctrl", "3"], "Library"), (&["Ctrl", "4"], "Settings"),
        (&["Esc"], "Back"), (&["P"], "Title page: play or resume"), (&["L", "D"], "Title page: like, not for me"),
        (&["S", "W"], "Title page: save, watched"), (&["T", "O"], "Title page: trailer, sources"), (&["←", "→"], "Picks: keep the left or right card"),
    ];
    rsx! {
        div { class: "scrim", onclick: move |_| shell.shortcuts_open.set(false),
            section { class: "shortcuts glass", role: "dialog", aria_label: "Shortcuts", onclick: move |e| e.stop_propagation(),
                div { class: "sheet-head",
                    span { class: "group-icon", icons::Grid {} }
                    span { class: "group-name", "Shortcuts" }
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

/// Trailers play in the app: a direct file in the player, else YouTube in the trailer sheet,
/// which can move on to the next one (some are blocked in some countries).
pub fn play_trailers(mut shell: Shell, trailers: Vec<backend::Trailer>) {
    if let Some(url) = trailers.iter().find_map(|t| t.url.clone()) {
        start_playback(shell, player::PlayRequest { title: "Trailer".into(), url, ..Default::default() }, None); return;
    }
    let ids: Vec<String> = trailers.into_iter().filter_map(|t| t.youtube).collect();
    if !ids.is_empty() { shell.trailer.set(Some((ids, 0))); }
}

/// A YouTube trailer inside the app: the official embed in a glass sheet; nothing autoplays
/// until it is opened, and closing it stops it.
#[component]
fn TrailerSheet(id: String, more: bool) -> Element {
    let mut shell = use_context::<Shell>();
    let src = format!("https://www.youtube-nocookie.com/embed/{id}?autoplay=1&playsinline=1&rel=0&modestbranding=1");
    rsx! {
        div { class: "scrim trailer-scrim", onclick: move |_| shell.trailer.set(None),
            section { class: "trailer-sheet glass", role: "dialog", aria_label: "Trailer", onclick: move |e| e.stop_propagation(),
                div { class: "trailer-frame",
                    iframe { src: "{src}", title: "Trailer", allow: "autoplay; encrypted-media; picture-in-picture; fullscreen", allowfullscreen: "true",
                        referrerpolicy: "strict-origin-when-cross-origin" }
                }
                div { class: "sheet-head",
                    span { class: "group-icon", icons::Clapper {} }
                    span { class: "group-name", "Trailer" }
                    if more {
                        button { class: "pill glass", title: "Try the next trailer", onclick: move |_| shell.trailer.with_mut(|t| if let Some((ids, at)) = t { *at = (*at + 1) % ids.len(); }),
                            icons::Next {} span { "Another trailer" } }
                    }
                    icons::IconButton { label: "Open on YouTube", class: "glass", onclick: { let id = id.clone(); move |_| open_external(&format!("https://www.youtube.com/watch?v={id}")) }, icons::Link {} }
                    icons::IconButton { label: "Close (Esc)", class: "glass", onclick: move |_| shell.trailer.set(None), icons::Close {} }
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
