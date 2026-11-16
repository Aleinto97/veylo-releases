//! Settings. Add-ons are listed by name only; their URLs can
//! hold keys and are never shown. Keys are masked while typed and never displayed again.
use crate::{addons, backend, icons};
use dioxus::prelude::*;

#[component]
pub fn SettingsPage() -> Element {
    let mut lamp = use_context::<crate::Shell>().lamp_source;
    use_hook(move || lamp.set(String::new()));
    let mut settings = use_signal(backend::load_settings);
    let mut shell = use_context::<crate::Shell>();
    let mut overview = use_resource(move || async move {
        let ov = addons::overview().await;
        shell.addon_updates.set(ov.updates);
        ov
    });
    // Which add-on action is running ("Torrentio", "key", ...), and the last problem to report.
    let mut busy = use_signal(|| None::<String>);
    let mut problem = use_signal(|| None::<String>);
    let mut rd_account = use_signal(|| None::<String>);
    // The answer of the last "Check for updates".
    let mut check_note = use_signal(|| None::<String>);
    // Run one add-on job, then reload the list; errors carry no links or keys.
    let mut run = move |label: String, job: std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>>>>| {
        if busy.peek().is_some() { return; }
        busy.set(Some(label));
        problem.set(None);
        check_note.set(None);
        spawn(async move {
            if let Err(e) = job.await { problem.set(Some(format!("{e:#}"))); }
            busy.set(None);
            overview.restart();
        });
    };
    let mut save_rd_key = move |key: String| {
        if busy.peek().is_some() { return; }
        busy.set(Some("key".into()));
        problem.set(None);
        spawn(async move {
            match addons::check_rd_key(key.clone()).await {
                Ok(account) => {
                    let next = backend::Settings { rd_key: Some(key), rd_key_removed: false, ..backend::load_settings() };
                    backend::save_settings(&next);
                    settings.set(next);
                    rd_account.set(Some(account));
                    if let Err(e) = addons::reconfigure().await { problem.set(Some(format!("{e:#}"))); }
                }
                Err(e) => problem.set(Some(format!("{e:#}"))),
            }
            busy.set(None);
            overview.restart();
        });
    };
    let mut choose_language = move |key: String| {
        let next = backend::Settings { stream_language: Some(key).filter(|k| addons::language(k).is_some()), ..backend::load_settings() };
        backend::save_settings(&next);
        settings.set(next);
        run("language".into(), Box::pin(async { addons::reconfigure().await }));
    };
    let mut rounds = move |n: u32| {
        let next = backend::Settings { picks_rounds: n, ..backend::load_settings() };
        backend::save_settings(&next);
        settings.set(next);
    };
    rsx! {
        div { class: "settings",
            h1 { class: "display title", "Settings" }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Grid {} } span { class: "group-name", "Posters" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Poster source" }
                        span { class: "dim setting-about", "BetterPosters adds rating and genre tags; the add-on's poster is used when it has none" } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Poster source",
                        for (on, name) in [(true, "BetterPosters"), (false, "Add-on")] {
                            button { class: if settings().better_posters == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().better_posters == on}",
                                onclick: move |_| { let next = backend::Settings { better_posters: on, ..backend::load_settings() }; backend::save_settings(&next); settings.set(next); }, "{name}" }
                        }
                    }
                }
            }

            ScreenBlock { settings }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Globe {} } span { class: "group-name", "Artwork" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "TMDB" }
                        span { class: "dim setting-about", if settings().tmdb_key.is_some() { "Connected: text-free backdrops, logos and episode stills" } else { "Add a free TMDB key for backdrops, logos and episode stills" } } }
                    if settings().tmdb_key.is_some() {
                        button { class: "pill glass", onclick: move |_| { let next = backend::Settings { tmdb_key: None, ..backend::load_settings() }; backend::save_settings(&next); settings.set(next); }, icons::Trash {} span { "Remove key" } }
                    } else {
                        GetKey { url: addons::TMDB_KEY_PAGE, label: "Get a TMDB key (API Key, not the Read Access Token)" }
                        // Masked while typing; saved on Enter.
                        input { class: "key-field", r#type: "password", placeholder: "TMDB API key", aria_label: "TMDB API key", autocomplete: "off",
                            onkeydown: move |e| { e.stop_propagation(); },
                            onchange: move |e| { let v = e.value().trim().to_owned(); if !v.is_empty() { let next = backend::Settings { tmdb_key: Some(v), ..backend::load_settings() }; backend::save_settings(&next); settings.set(next); } } }
                    }
                }
            }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Sparkles {} } span { class: "group-name", "Picks" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Rounds" } span { class: "dim setting-about", "Duels before the final pick" } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Rounds",
                        for n in [3u32, 5, 7, 10] {
                            button { class: if settings().picks_rounds == n { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().picks_rounds == n}",
                                onclick: move |_| rounds(n), span { class: "tnum", "{n}" } }
                        }
                    }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Learned taste" } span { class: "dim setting-about", "What Picks learned from your duels" } }
                    button { class: "pill glass", onclick: move |_| backend::reset_taste(), icons::Undo {} span { "Start over" } }
                }
            }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Magnet {} } span { class: "group-name", "Streaming" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Real-Debrid" }
                        span { class: "dim setting-about",
                            if busy().as_deref() == Some("key") { "Checking your key…" }
                            else if let Some(account) = rd_account() { "Connected: {account}. Every add-on below uses it." }
                            else if settings().rd_key.is_some() { "Connected. Every add-on below uses this key." }
                            else { "Paste your API token once; every add-on below uses it." } } }
                    if settings().rd_key.is_some() {
                        button { class: "pill glass", disabled: busy().is_some(), title: "Removes the key and the add-ons Veylo set up with it",
                            onclick: move |_| { rd_account.set(None); run("key".into(), Box::pin(async move {
                                let done = addons::remove_rd_key().await; settings.set(backend::load_settings()); done })); },
                            icons::Trash {} span { "Remove key" } }
                    } else {
                        GetKey { url: addons::RD_KEY_PAGE, label: "Get your Real-Debrid key" }
                        input { class: "key-field", r#type: "password", placeholder: "Real-Debrid API token", aria_label: "Real-Debrid API token", autocomplete: "off",
                            disabled: busy().is_some(),
                            onkeydown: move |e| { e.stop_propagation(); },
                            onchange: move |e| { let v = e.value().trim().to_owned(); if !v.is_empty() { save_rd_key(v); } } }
                    }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Audio language" }
                        span { class: "dim setting-about",
                            if busy().as_deref() == Some("language") { "Updating your add-ons…" }
                            else { "Play picks sources in this language first; the add-ons rank it first too" } } }
                    select { class: "select-field", aria_label: "Audio language", disabled: busy().is_some(),
                        onkeydown: move |e| { e.stop_propagation(); },
                        onchange: move |e| choose_language(e.value()),
                        option { value: "", selected: settings().stream_language.is_none(), "Any language" }
                        for l in addons::LANGUAGES {
                            option { value: "{l.key}", selected: settings().stream_language.as_deref() == Some(l.key), "{l.label}" }
                        }
                    }
                }
                if let Some(p) = problem() { p { class: "addon-problem", role: "alert", icons::Alert {} span { "{p}" } } }
            }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Stack {} } span { class: "group-name", "Add-ons" }
                    span { class: "topbar-spacer" }
                    button { class: "pill glass small", disabled: busy().is_some(),
                        onclick: move |_| run("check".into(), Box::pin(async move { check_note.set(Some(addons::check().await.1)); Ok(()) })),
                        icons::Refresh {} span { if busy().as_deref() == Some("check") { "Checking…" } else { "Check for updates" } } }
                }
                if let Some(note) = check_note() { p { class: "check-note", role: "status", icons::Check {} span { "{note}" } } }
                match overview() {
                    None => rsx! { div { class: "addon-list", div { class: "row skeleton-row" } } },
                    Some(ov) => rsx! {
                        div { class: "market",
                            for (c, row) in ov.curated.clone() {
                                div { class: if row.is_some() { "market-card glass on" } else { "market-card glass" },
                                    div { class: "market-head",
                                        span { class: "market-name display", "{c.name()}" }
                                        if let Some(r) = &row { span { class: "dim tnum market-version", "v{r.version}" } }
                                    }
                                    p { class: "market-about", "{c.about()}" }
                                    p { class: "dim market-about", "Cached Real-Debrid results only, no filters" }
                                    div { class: "market-actions",
                                        if busy().as_deref() == Some(c.name()) {
                                            span { class: "pill static glass", "Setting up…" }
                                        } else if let Some(r) = row.clone() {
                                            if let Some(v) = r.update.clone() {
                                                { let r = r.clone(); rsx! { button { class: "pill accent", disabled: busy().is_some(), onclick: move |_| { let r = r.clone(); run(c.name().into(), Box::pin(async move { addons::update(&r).await })) }, icons::Refresh {} span { "Update to {v}" } } } }
                                            } else { span { class: "pill static glass", icons::Check {} span { "Installed" } } }
                                            button { class: "pill glass", disabled: busy().is_some(), onclick: move |_| { let r = r.clone(); run(c.name().into(), Box::pin(async move { addons::remove(&r).await })) }, icons::Trash {} span { "Remove" } }
                                        } else {
                                            button { class: "pill accent", disabled: busy().is_some() || settings().rd_key.is_none(),
                                                title: if settings().rd_key.is_none() { "Add your Real-Debrid key first" } else { "" },
                                                onclick: move |_| run(c.name().into(), Box::pin(async move { addons::install(c).await })),
                                                icons::Magnet {} span { "Install" } }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "addon-list",
                            for r in ov.others.clone() {
                                div { class: "addon-row glass",
                                    span { class: "addon-name", "{r.name}" }
                                    if !r.offers.is_empty() { span { class: "dim addon-offers", "{r.offers}" } }
                                    span { class: "topbar-spacer" }
                                    if let Some(v) = r.update.clone() {
                                        { let r = r.clone(); let label = r.name.clone(); rsx! { button { class: "pill accent small", disabled: busy().is_some(), onclick: move |_| { let r = r.clone(); run(label.clone(), Box::pin(async move { addons::update(&r).await })) }, icons::Refresh {} span { "Update to {v}" } } } }
                                    } else if !r.version.is_empty() { span { class: "dim tnum addon-offers", "v{r.version}" } }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// "Get your key": opens the provider's key page in the browser, to copy and paste back here.
#[component]
fn GetKey(url: &'static str, label: &'static str) -> Element {
    rsx! {
        button { class: "link-btn", title: "{url}", onclick: move |_| crate::open_external(url),
            icons::Link {} span { "{label}" } }
    }
}

/// What this screen can show (read from the system, again on "Check again"), and on Windows
/// whether HDR goes to it. The player reads the screen itself at every start.
#[component]
fn ScreenBlock(settings: Signal<backend::Settings>) -> Element {
    let mut screen = use_signal(crate::display::screen);
    let (title, about) = crate::display::summary(screen());
    rsx! {
        section { class: "settings-block",
            div { class: "group-head", span { class: "group-icon", icons::Tv {} } span { class: "group-name", "Screen" } }
            div { class: "setting-row glass",
                div { class: "setting-copy", span { class: "setting-name", "This screen: {title}" }
                    span { class: "dim setting-about", "{about}" } }
                button { class: "pill glass", onclick: move |_| screen.set(crate::display::screen()), icons::Refresh {} span { "Check again" } }
            }
            if cfg!(target_os = "windows") {
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "HDR output" }
                        span { class: "dim setting-about", "Automatic sends HDR videos to the screen in HDR when Windows HDR is on. Always SDR tone-maps them, for screens whose HDR looks washed out." } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "HDR output",
                        for (on, name) in [(true, "Automatic"), (false, "Always SDR")] {
                            button { class: if settings().hdr_output == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().hdr_output == on}",
                                onclick: move |_| { let next = backend::Settings { hdr_output: on, ..backend::load_settings() }; backend::save_settings(&next); settings.set(next); }, "{name}" }
                        }
                    }
                }
            }
        }
    }
}
