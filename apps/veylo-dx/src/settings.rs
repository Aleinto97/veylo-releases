//! Settings: organized into five cohesive smoked-glass cards (Account, Playback, Add-ons,
//! Display & Discovery, Privacy & About) with quick category filtering and unified control rhythm.
//! Add-ons are listed by name only; keys and URLs stay masked and are never logged.
use crate::{addons, backend, icons};
use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section { All, Account, Playback, Addons, Display, Privacy }

impl Section {
    fn shows(self, target: Section) -> bool { self == Section::All || self == target }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddonSetupTab { Link, Bundle, Stremio, Directory }

#[component]
pub fn SettingsPage() -> Element {
    let mut lamp = use_context::<crate::Shell>().lamp_source;
    use_effect(move || { if !lamp.peek().is_empty() { lamp.set(String::new()); } });
    let mut settings = use_signal(backend::load_settings);
    let mut shell = use_context::<crate::Shell>();
    let mut section = use_signal(|| Section::All);
    let mut overview = use_resource(move || async move {
        let ov = addons::overview().await;
        shell.addon_updates.set(ov.updates);
        ov
    });

    // Add-ons state
    let mut busy = use_signal(|| None::<String>);
    let mut problem = use_signal(|| None::<String>);
    let mut check_note = use_signal(|| None::<String>);
    let mut arm_remove = use_signal(|| None::<usize>);
    let mut setup_tab = use_signal(|| AddonSetupTab::Link);
    let mut custom_url = use_signal(String::new);
    // The add-on URL stays masked unless asked for.
    let mut show_url = use_signal(|| false);
    let mut bundle_text = use_signal(String::new);
    let mut stremio_email = use_signal(String::new);
    let mut stremio_pass = use_signal(String::new);
    let mut dir_url = use_signal(String::new);
    let mut dir_entries = use_signal(Vec::<addons::DirectoryEntry>::new);

    // Privacy, data & diagnostics state
    let mut import_open = use_signal(|| false);
    let mut import_text = use_signal(String::new);
    let mut data_note = use_signal(|| None::<String>);
    let mut diag_open = use_signal(|| false);
    let mut diag_text = use_signal(String::new);
    // None: idle; Some(true): copied; Some(false): the clipboard refused.
    let mut diag_copied = use_signal(|| None::<bool>);
    let mut bug_open = use_signal(|| false);
    let mut bug_kind = use_signal(|| "bug".to_owned());
    let mut bug_text = use_signal(String::new);
    let mut bug_sending = use_signal(|| false);
    let mut bug_note = use_signal(|| None::<String>);
    let mut remote_open = use_signal(|| false);
    let mut remote_os = use_signal(String::new);
    let mut remote_loading = use_signal(|| false);
    let mut remote_reports = use_signal(Vec::<backend::DiagReport>::new);
    let mut remote_err = use_signal(|| None::<String>);
    let mut legal_open = use_signal(|| false);

    let mut run = move |label: String, job: std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>>>>| {
        if busy.peek().is_some() { return; }
        busy.set(Some(label));
        problem.set(None);
        check_note.set(None);
        spawn(async move {
            if let Err(e) = job.await { problem.set(Some(format!("{e:#}"))); }
            else { crate::home::invalidate_cache(); }
            busy.set(None);
            overview.restart();
        });
    };
    let mut choose_language = move |key: String| {
        let key = if addons::language(&key).is_some() { key } else { backend::DEFAULT_LANGUAGE.to_owned() };
        settings.set(backend::update_settings(|s| s.stream_language = Some(key)));
    };

    let cur = section();
    let copy_label = match diag_copied() { Some(true) => "Copied", Some(false) => "Couldn't copy", None => "Copy" };
    let updates_count = (shell.addon_updates)();
    let key_needed = backend::sync_key_needed();

    rsx! {
        div { class: "settings",
            div { class: "settings-head",
                h1 { class: "display title", "Settings" }
                span { class: "settings-ver tnum", "v{env!(\"CARGO_PKG_VERSION\")}" }
            }

            nav { class: "settings-nav", role: "tablist", aria_label: "Settings categories",
                for (tab, label) in [
                    (Section::All, "All"),
                    (Section::Account, "Account"),
                    (Section::Playback, "Playback"),
                    (Section::Addons, "Add-ons"),
                    (Section::Display, "Display"),
                    (Section::Privacy, "Privacy & About"),
                ] {
                    if tab != Section::Account || backend::accounts_available() {
                        button {
                            class: if cur == tab { "settings-nav-pill glass on" } else { "settings-nav-pill glass" },
                            role: "tab",
                            aria_selected: "{cur == tab}",
                            onclick: move |_| section.set(tab),
                            match tab {
                                Section::All => rsx! { icons::Sliders {} },
                                Section::Account => rsx! { icons::Users {} },
                                Section::Playback => rsx! { icons::Play {} },
                                Section::Addons => rsx! { icons::Stack {} },
                                Section::Display => rsx! { icons::Tv {} },
                                Section::Privacy => rsx! { icons::Lock {} },
                            }
                            span { "{label}" }
                            if tab == Section::Account && key_needed { i { class: "nav-dot warn", aria_hidden: "true" } }
                            if tab == Section::Addons && updates_count > 0 { i { class: "nav-dot", aria_hidden: "true" } }
                        }
                    }
                }
            }

            if cur.shows(Section::Account) {
                crate::account_ui::AccountBlock {}
            }

            if cur.shows(Section::Playback) {
                section { class: "settings-block",
                    div { class: "group-head",
                        span { class: "group-icon", icons::Play {} }
                        span { class: "group-name", "Playback" }
                    }
                    div { class: "settings-group glass",
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Audio language" }
                                span { class: "setting-about", "Smart Play prefers streams and embedded audio tracks in this language." }
                            }
                            select { class: "select-field", aria_label: "Audio language",
                                onkeydown: move |e| { e.stop_propagation(); },
                                onchange: move |e| choose_language(e.value()),
                                for l in addons::LANGUAGES {
                                    option { value: "{l.key}", selected: settings().stream_language.as_deref() == Some(l.key), "{l.label}" }
                                }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Max streaming bitrate" }
                                span { class: "setting-about",
                                    match (settings().max_bitrate_mbps, backend::connection_mbps()) {
                                        (None, Some(mbps)) => format!("Auto: matches your connection (~{mbps:.0} Mbps measured) so playback won't buffer."),
                                        (None, None) => "Auto: measures your connection speed and picks the highest quality that won't buffer.".into(),
                                        (Some(0), _) => "Unlimited: always picks the highest quality source regardless of connection speed.".into(),
                                        (Some(cap), _) => format!("Prefers sources up to {cap} Mbps average bitrate."),
                                    }
                                }
                            }
                            select { class: "select-field", aria_label: "Max streaming bitrate",
                                onkeydown: move |e| { e.stop_propagation(); },
                                onchange: move |e| {
                                    let val = e.value();
                                    let cap = match val.as_str() { "auto" => None, n => n.parse::<u32>().ok() };
                                    settings.set(backend::update_settings(|s| s.max_bitrate_mbps = cap));
                                },
                                option { value: "auto", selected: settings().max_bitrate_mbps.is_none(), "Auto (match speed)" }
                                option { value: "0", selected: settings().max_bitrate_mbps == Some(0), "Unlimited" }
                                option { value: "60", selected: settings().max_bitrate_mbps == Some(60), "60 Mbps (4K high)" }
                                option { value: "30", selected: settings().max_bitrate_mbps == Some(30), "30 Mbps (4K)" }
                                option { value: "15", selected: settings().max_bitrate_mbps == Some(15), "15 Mbps (1080p high)" }
                                option { value: "8", selected: settings().max_bitrate_mbps == Some(8), "8 Mbps (data saver)" }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Up Next countdown" }
                                span { class: "setting-about", "Show a short countdown to the next episode at credits or episode end." }
                            }
                            div { class: "segmented", role: "radiogroup", aria_label: "Up Next countdown",
                                i { class: "slider", aria_hidden: "true" }
                                for (on, name) in [(true, "On"), (false, "Off")] {
                                    button { class: if settings().auto_next == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().auto_next == on}",
                                        onclick: move |_| settings.set(backend::update_settings(|s| s.auto_next = on)), "{name}" }
                                }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "HDMI audio passthrough" }
                                span { class: "setting-about", "Send Dolby Digital, DTS, and TrueHD bitstreams directly to an AVR or soundbar." }
                            }
                            div { class: "segmented", role: "radiogroup", aria_label: "HDMI audio passthrough",
                                i { class: "slider", aria_hidden: "true" }
                                for (on, name) in [(true, "On"), (false, "Off")] {
                                    button { class: if settings().audio_passthrough == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().audio_passthrough == on}",
                                        onclick: move |_| settings.set(backend::update_settings(|s| s.audio_passthrough = on)), "{name}" }
                                }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                div { class: "setting-title-line",
                                    span { class: "setting-name", "Prefetch sources while browsing" }
                                    if !settings().prefetch_sources { span { class: "status-pill muted", "Private default" } }
                                }
                                span { class: "setting-about", "When off, stream add-ons are only queried after you press Play." }
                            }
                            div { class: "segmented", role: "radiogroup", aria_label: "Prefetch sources while browsing",
                                i { class: "slider", aria_hidden: "true" }
                                for (on, name) in [(true, "On"), (false, "Off")] {
                                    button { class: if settings().prefetch_sources == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().prefetch_sources == on}",
                                        onclick: move |_| settings.set(backend::update_settings(|s| s.prefetch_sources = on)), "{name}" }
                                }
                            }
                        }
                    }
                }
            }

            if cur.shows(Section::Addons) {
                section { class: "settings-block",
                    div { class: "group-head",
                        span { class: "group-icon", icons::Stack {} }
                        span { class: "group-name", "Add-ons" }
                        span { class: "topbar-spacer" }
                        button { class: "pill glass small", disabled: busy().is_some(),
                            onclick: move |_| run("check".into(), Box::pin(async move { check_note.set(Some(addons::check().await.1)); Ok(()) })),
                            icons::Refresh {} span { if busy().as_deref() == Some("check") { "Checking…" } else { "Check updates" } } }
                    }
                    div { class: "settings-group glass",
                        match overview() {
                            None => rsx! { div { class: "setting-row", div { class: "row skeleton-row", style: "width: 100%; min-height: 36px;" } } },
                            Some(ov) => rsx! {
                                for (idx, r) in ov.addons.clone().into_iter().enumerate() {
                                    div { class: "addon-item",
                                        div { class: "addon-item-copy",
                                            div { class: "setting-title-line",
                                                span { class: "addon-name", "{r.name}" }
                                                if !r.version.is_empty() { span { class: "status-pill muted tnum", "v{r.version}" } }
                                                if !r.removable() { span { class: "status-pill muted", "Core" } }
                                            }
                                            if !r.offers.is_empty() { span { class: "addon-offers", "{r.offers}" } }
                                        }
                                        div { class: "addon-item-actions",
                                            if let Some(v) = r.update.clone() {
                                                { let r = r.clone(); let label = r.name.clone(); rsx! {
                                                    button { class: "pill accent small", disabled: busy().is_some(),
                                                        onclick: move |_| { let r = r.clone(); run(label.clone(), Box::pin(async move { addons::update(&r).await })) },
                                                        icons::Refresh {} span { "Update {v}" } }
                                                } }
                                            }
                                            if r.configurable {
                                                { let r = r.clone(); rsx! {
                                                    button { class: "pill glass small", disabled: busy().is_some(),
                                                        onclick: move |_| r.open_configure(),
                                                        icons::Sliders {} span { "Configure" } }
                                                } }
                                            }
                                            if r.removable() {
                                                { let r = r.clone(); let label = r.name.clone(); let armed = arm_remove() == Some(idx); rsx! {
                                                    if armed {
                                                        span { class: "confirm", role: "group", aria_label: "Remove {label}?",
                                                            button { class: "pill forget-confirm small", disabled: busy().is_some(),
                                                                onclick: move |_| { arm_remove.set(None); let r = r.clone(); run(label.clone(), Box::pin(async move { addons::remove(&r).await })) },
                                                                icons::Trash {} span { "Confirm" } }
                                                            icons::IconButton { label: "Cancel", class: "glass small", onclick: move |_| arm_remove.set(None), icons::Close {} }
                                                        }
                                                    } else {
                                                        button { class: "pill glass small danger", disabled: busy().is_some(),
                                                            onclick: move |_| arm_remove.set(Some(idx)),
                                                            icons::Trash {} span { "Remove" } }
                                                    }
                                                } }
                                            }
                                        }
                                    }
                                }
                                div { class: "setting-drawer",
                                    div { class: "addon-setup-head",
                                        div { class: "setting-copy",
                                            span { class: "setting-name", "Add or import add-ons" }
                                            span { class: "setting-about", "Links stay masked and are never logged. Clicking any stremio:// link also installs in one tap." }
                                        }
                                        div { class: "addon-mode-tabs", role: "tablist", aria_label: "Add-on setup mode",
                                            for (mode, label) in [
                                                (AddonSetupTab::Link, "Link"),
                                                (AddonSetupTab::Bundle, "Bundle"),
                                                (AddonSetupTab::Stremio, "Stremio"),
                                                (AddonSetupTab::Directory, "Directory"),
                                            ] {
                                                button {
                                                    class: if setup_tab() == mode { "addon-mode-tab on" } else { "addon-mode-tab" },
                                                    role: "tab",
                                                    aria_selected: "{setup_tab() == mode}",
                                                    onclick: move |_| setup_tab.set(mode),
                                                    "{label}"
                                                }
                                            }
                                        }
                                    }
                                    match setup_tab() {
                                        AddonSetupTab::Link => rsx! {
                                            div { class: "setting-input-group",
                                                input { class: "key-field", r#type: if show_url() { "text" } else { "password" }, placeholder: "stremio://… or https://…/manifest.json", aria_label: "Stremio add-on URL", autocomplete: "off", spellcheck: "false",
                                                    value: "{custom_url}", disabled: busy().is_some(),
                                                    oninput: move |e| custom_url.set(e.value()),
                                                    onkeydown: move |e| {
                                                        e.stop_propagation();
                                                        if e.key() == Key::Enter {
                                                            let url = custom_url.peek().trim().to_owned();
                                                            if !url.is_empty() {
                                                                run("custom".into(), Box::pin(async move {
                                                                    let name = addons::install_custom(url).await?;
                                                                    custom_url.set(String::new());
                                                                    check_note.set(Some(format!("Installed {name}.")));
                                                                    Ok(())
                                                                }));
                                                            }
                                                        }
                                                    },
                                                }
                                                button { class: "pill glass small", aria_label: if show_url() { "Hide add-on URL" } else { "Show add-on URL" },
                                                    onclick: move |_| show_url.set(!show_url()),
                                                    span { if show_url() { "Hide" } else { "Show" } }
                                                }
                                                button { class: "pill accent small", disabled: busy().is_some() || custom_url().trim().is_empty(),
                                                    onclick: move |_| {
                                                        let url = custom_url.peek().trim().to_owned();
                                                        run("custom".into(), Box::pin(async move {
                                                            let name = addons::install_custom(url).await?;
                                                            custom_url.set(String::new());
                                                            check_note.set(Some(format!("Installed {name}.")));
                                                            Ok(())
                                                        }));
                                                    },
                                                    icons::Plus {} span { "Install" }
                                                }
                                            }
                                        },
                                        AddonSetupTab::Bundle => rsx! {
                                            textarea { class: "import-area", rows: "4", placeholder: "Paste a Veylo/Stremio add-ons JSON bundle or one https:// or stremio:// manifest URL per line…",
                                                value: "{bundle_text}",
                                                oninput: move |e| bundle_text.set(e.value()),
                                                onkeydown: move |e| e.stop_propagation(),
                                            }
                                            div { class: "account-actions",
                                                button { class: "pill accent small", disabled: busy().is_some() || bundle_text().trim().is_empty(),
                                                    onclick: move |_| {
                                                        let raw = bundle_text.peek().clone();
                                                        run("bundle-in".into(), Box::pin(async move {
                                                            let (ok, failed) = addons::import_bundle(&raw).await?;
                                                            bundle_text.set(String::new());
                                                            check_note.set(Some(if failed == 0 {
                                                                format!("Installed {ok} add-on(s) from bundle.")
                                                            } else {
                                                                format!("Installed {ok} add-on(s); {failed} couldn't be reached.")
                                                            }));
                                                            Ok(())
                                                        }));
                                                    },
                                                    icons::Check {} span { "Install bundle" }
                                                }
                                                button { class: "pill glass small", disabled: busy().is_some(),
                                                    onclick: move |_| {
                                                        run("bundle-out".into(), Box::pin(async move {
                                                            let (count, _text) = addons::export_bundle().await?;
                                                            check_note.set(Some(format!("Saved {count} add-on(s) to veylo-addons-bundle.json in Veylo's private folder.")));
                                                            Ok(())
                                                        }));
                                                    },
                                                    icons::Download {} span { "Export bundle file" }
                                                }
                                            }
                                        },
                                        AddonSetupTab::Stremio => rsx! {
                                            p { class: "setting-about", "One-time on-device import of your own Stremio add-on list. Credentials and auth tokens are discarded immediately." }
                                            div { class: "setting-input-group",
                                                input { class: "key-field", r#type: "email", placeholder: "Stremio email", aria_label: "Stremio email", autocomplete: "off",
                                                    value: "{stremio_email}",
                                                    oninput: move |e| stremio_email.set(e.value()),
                                                    onkeydown: move |e| e.stop_propagation(),
                                                }
                                                input { class: "key-field", r#type: "password", placeholder: "Stremio password", aria_label: "Stremio password", autocomplete: "off",
                                                    value: "{stremio_pass}",
                                                    oninput: move |e| stremio_pass.set(e.value()),
                                                    onkeydown: move |e| e.stop_propagation(),
                                                }
                                                button { class: "pill accent small", disabled: busy().is_some() || stremio_email().trim().is_empty() || stremio_pass().is_empty(),
                                                    onclick: move |_| {
                                                        let (e, p) = (stremio_email.peek().clone(), stremio_pass.peek().clone());
                                                        stremio_pass.set(String::new());
                                                        run("stremio".into(), Box::pin(async move {
                                                            let (ok, failed) = addons::import_from_stremio(e, p).await?;
                                                            stremio_email.set(String::new());
                                                            check_note.set(Some(if failed == 0 {
                                                                format!("Imported {ok} add-on(s) from your Stremio account.")
                                                            } else {
                                                                format!("Imported {ok} add-on(s); {failed} couldn't be reached.")
                                                            }));
                                                            Ok(())
                                                        }));
                                                    },
                                                    icons::Check {} span { if busy().as_deref() == Some("stremio") { "Importing…" } else { "Import add-ons" } }
                                                }
                                            }
                                        },
                                        AddonSetupTab::Directory => rsx! {
                                            p { class: "setting-about", "Load any Stremio-compatible directory JSON URL you choose. Veylo includes no pre-installed streaming directories." }
                                            div { class: "setting-input-group",
                                                input { class: "key-field", r#type: "url", placeholder: "https://…/addons.json", aria_label: "Add-on directory URL", autocomplete: "off",
                                                    value: "{dir_url}",
                                                    oninput: move |e| dir_url.set(e.value()),
                                                    onkeydown: move |e| e.stop_propagation(),
                                                }
                                                button { class: "pill accent small", disabled: busy().is_some() || dir_url().trim().is_empty(),
                                                    onclick: move |_| {
                                                        let u = dir_url.peek().clone();
                                                        run("directory".into(), Box::pin(async move {
                                                            let entries = addons::fetch_directory(u).await?;
                                                            dir_entries.set(entries);
                                                            Ok(())
                                                        }));
                                                    },
                                                    icons::Refresh {} span { "Load directory" }
                                                }
                                            }
                                            if !dir_entries.read().is_empty() {
                                                div { class: "market",
                                                    for entry in dir_entries.read().clone() {
                                                        div { class: if entry.installed { "market-card glass on" } else { "market-card glass" },
                                                            div { class: "market-head",
                                                                span { class: "market-name display", "{entry.name}" }
                                                                if !entry.version.is_empty() { span { class: "dim tnum market-version", "v{entry.version}" } }
                                                            }
                                                            if !entry.description.is_empty() { p { class: "market-about", "{entry.description}" } }
                                                            if !entry.offers.is_empty() { p { class: "dim market-about", "{entry.offers}" } }
                                                            div { class: "market-actions",
                                                                if entry.installed {
                                                                    span { class: "market-status", icons::Check {} span { "Installed" } }
                                                                } else {
                                                                    { let u = entry.manifest_url.clone(); let name = entry.name.clone(); rsx! {
                                                                        button { class: "pill accent small", disabled: busy().is_some(),
                                                                            onclick: move |_| {
                                                                                let u = u.clone(); let name = name.clone();
                                                                                run(name.clone(), Box::pin(async move {
                                                                                    addons::install_custom(u).await?;
                                                                                    check_note.set(Some(format!("Installed {name}.")));
                                                                                    Ok(())
                                                                                }));
                                                                            },
                                                                            icons::Plus {} span { "Install" }
                                                                        }
                                                                    } }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        },
                                    }
                                }
                            },
                        }
                        if let Some(note) = check_note() { p { class: "check-note", role: "status", icons::Check {} span { "{note}" } } }
                        if let Some(p) = problem() { p { class: "addon-problem", role: "alert", icons::Alert {} span { "{p}" } } }
                    }
                }
            }

            if cur.shows(Section::Display) {
                section { class: "settings-block",
                    div { class: "group-head",
                        span { class: "group-icon", icons::Tv {} }
                        span { class: "group-name", "Display & Discovery" }
                    }
                    div { class: "settings-group glass",
                        ScreenRows { settings }
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Charts & posters" }
                                span { class: "setting-about",
                                    if settings().better_posters { "BetterPosters charts, rating & 4K/HD/Dolby quality badges, and fuller metadata; hides movies without digital HD/4K quality from Home." }
                                    else { "Use your installed add-ons' own catalogs, posters and metadata." }
                                }
                            }
                            div { class: "segmented", role: "radiogroup", aria_label: "Charts and posters source",
                                i { class: "slider", aria_hidden: "true" }
                                for (on, name) in [(true, "BetterPosters"), (false, "Add-on")] {
                                    button { class: if settings().better_posters == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().better_posters == on}",
                                        onclick: move |_| {
                                            settings.set(backend::update_settings(|s| s.better_posters = on));
                                            backend::clear_artwork_caches();
                                            crate::home::invalidate_cache();
                                        }, "{name}" }
                                }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                div { class: "setting-title-line",
                                    span { class: "setting-name", "Picks on-device AI" }
                                    span { class: "status-pill ok", "Local only" }
                                }
                                span { class: "setting-about", "Runs bge-small locally on your CPU; learned from your duels, plays, likes and moods." }
                            }
                            crate::components::StartOver { labelled: true, on_reset: move |_| crate::brain::reset() }
                        }
                    }
                }
            }

            if cur.shows(Section::Privacy) {
                section { class: "settings-block",
                    div { class: "group-head",
                        span { class: "group-icon", icons::Lock {} }
                        span { class: "group-name", "Privacy, Data & About" }
                    }
                    div { class: "settings-group glass",
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Watch history & caches" }
                                span { class: "setting-about", "Import Trakt JSON, IMDb CSV, or a Veylo backup. Exports save privately to disk (zero keys or add-on links)." }
                            }
                            div { class: "setting-controls",
                                button { class: "pill glass small", onclick: move |_| import_open.set(!import_open()), icons::Upload {} span { if import_open() { "Close" } else { "Import" } } }
                                button { class: "pill glass small", onclick: move |_| {
                                    data_note.set(None);
                                    spawn(async move {
                                        match backend::export_watch_history().await {
                                            Ok(_) => data_note.set(Some("Saved veylo-history-export.json in Veylo's private folder (not copied to clipboard).".into())),
                                            Err(e) => data_note.set(Some(format!("Export failed: {e}"))),
                                        }
                                    });
                                }, icons::Download {} span { "Export" } }
                                button { class: "pill glass small", onclick: move |_| {
                                    data_note.set(None);
                                    spawn(async move {
                                        match backend::clear_subtitle_cache().await {
                                            Ok(()) => data_note.set(Some("Cached subtitle files cleared.".into())),
                                            Err(e) => data_note.set(Some(format!("Couldn't clear subtitle cache: {e}"))),
                                        }
                                    });
                                }, icons::Trash {} span { "Subtitles" } }
                            }
                        }
                        if import_open() {
                            div { class: "setting-drawer",
                                textarea { class: "import-area", rows: "4", placeholder: "Paste Trakt watched JSON, Veylo export JSON, or IMDb CSV…",
                                    value: "{import_text}",
                                    oninput: move |e| import_text.set(e.value()),
                                    onkeydown: move |e| e.stop_propagation(),
                                }
                                div { class: "account-actions",
                                    button { class: "pill accent small", disabled: import_text().trim().is_empty(),
                                        onclick: move |_| {
                                            let raw = import_text.peek().clone();
                                            data_note.set(None);
                                            spawn(async move {
                                                match backend::import_watch_history(&raw).await {
                                                    Ok(n) => {
                                                        import_text.set(String::new());
                                                        import_open.set(false);
                                                        data_note.set(Some(format!("Imported {n} title(s) into your Library.")));
                                                    }
                                                    Err(e) => data_note.set(Some(format!("{e}"))),
                                                }
                                            });
                                        },
                                        icons::Check {} span { "Import titles" }
                                    }
                                }
                            }
                        }
                        if let Some(note) = data_note() { p { class: "check-note", role: "status", icons::Check {} span { "{note}" } } }

                        div { class: "setting-row",
                            div { class: "setting-copy",
                                div { class: "setting-title-line",
                                    span { class: "setting-name", "Automatic crash & error reporting" }
                                    if !settings().telemetry_enabled { span { class: "status-pill muted", "Off by default" } }
                                }
                                span { class: "setting-about", "Opt-in only. Sends redacted crashes and playback/UI errors with generic OS/arch tags—never hostnames, keys, URLs, emails, or watch history." }
                            }
                            div { class: "segmented", role: "radiogroup", aria_label: "Automatic issue reporting",
                                i { class: "slider", aria_hidden: "true" }
                                for (on, name) in [(true, "On"), (false, "Off")] {
                                    button { class: if settings().telemetry_enabled == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().telemetry_enabled == on}",
                                        onclick: move |_| settings.set(backend::update_settings(|s| s.telemetry_enabled = on)), "{name}" }
                                }
                            }
                        }

                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Diagnostics & bug reports" }
                                span { class: "setting-about", "Inspect redacted local logs, copy a safe snapshot, or submit a manual issue report." }
                            }
                            div { class: "setting-controls",
                                button { class: "pill glass small", onclick: move |_| {
                                    if !diag_open() { diag_text.set(backend::diagnostic_report()); }
                                    diag_open.set(!diag_open());
                                }, span { if diag_open() { "Hide log" } else { "View log" } } }
                                button { class: "pill glass small", onclick: move |_| {
                                    let report = backend::diagnostic_report();
                                    diag_text.set(report.clone());
                                    // "Copied" only once the clipboard accepted it; otherwise the log opens to select by hand.
                                    let js = format!("try {{ await navigator.clipboard.writeText({}); return true; }} catch (e) {{ return false; }}", serde_json::to_string(&report).unwrap_or_default());
                                    spawn(async move {
                                        let ok = document::eval(&js).await.ok().and_then(|v| v.as_bool()).unwrap_or(false);
                                        if !ok { diag_open.set(true); }
                                        diag_copied.set(Some(ok));
                                        backend::sleep(2200).await;
                                        diag_copied.set(None);
                                    });
                                }, icons::Copy {} span { "{copy_label}" } }
                                button { class: "pill glass small", onclick: move |_| bug_open.set(!bug_open()), icons::Alert {} span { if bug_open() { "Close" } else { "Report" } } }
                                if backend::account().is_some() {
                                    button { class: "pill glass small", disabled: remote_loading(), onclick: move |_| {
                                        let next = !remote_open();
                                        remote_open.set(next);
                                        if next {
                                            remote_loading.set(true);
                                            remote_err.set(None);
                                            let filter = remote_os.peek().clone();
                                            spawn(async move {
                                                let f = (!filter.is_empty()).then_some(filter.as_str());
                                                match backend::list_remote_reports(f).await {
                                                    Ok(list) => remote_reports.set(list),
                                                    Err(e) => remote_err.set(Some(format!("{e}"))),
                                                }
                                                remote_loading.set(false);
                                            });
                                        }
                                    }, span { if remote_open() { "Hide reports" } else { "My reports" } } }
                                }
                            }
                        }
                        if diag_open() {
                            div { class: "setting-drawer",
                                pre { class: "diag-log", "{diag_text}" }
                            }
                        }
                        if bug_open() {
                            div { class: "setting-drawer",
                                div { class: "account-actions",
                                    select { class: "select-field", aria_label: "Issue type",
                                        value: "{bug_kind}",
                                        onkeydown: move |e| e.stop_propagation(),
                                        onchange: move |e| bug_kind.set(e.value()),
                                        option { value: "bug", "Bug" }
                                        option { value: "ui_error", "UI / layout error" }
                                        option { value: "performance", "Performance / stutter" }
                                        option { value: "playback_error", "Playback issue" }
                                    }
                                }
                                textarea { class: "import-area", rows: "3", placeholder: "Describe what happened…",
                                    value: "{bug_text}",
                                    oninput: move |e| bug_text.set(e.value()),
                                    onkeydown: move |e| e.stop_propagation(),
                                }
                                div { class: "account-actions",
                                    button { class: "pill accent small", disabled: bug_sending() || bug_text().trim().is_empty(),
                                        onclick: move |_| {
                                            let kind = bug_kind.peek().clone();
                                            let msg = bug_text.peek().clone();
                                            bug_sending.set(true);
                                            bug_note.set(None);
                                            spawn(async move {
                                                match backend::send_manual_report(&kind, &msg).await {
                                                    Ok(()) => {
                                                        bug_text.set(String::new());
                                                        bug_open.set(false);
                                                        bug_note.set(Some("Report sent with redacted diagnostics.".into()));
                                                    }
                                                    Err(e) => bug_note.set(Some(format!("Couldn't send report: {e}"))),
                                                }
                                                bug_sending.set(false);
                                            });
                                        },
                                        icons::Upload {} span { if bug_sending() { "Sending…" } else { "Send report" } }
                                    }
                                }
                            }
                        }
                        if let Some(note) = bug_note() { p { class: "check-note", role: "status", icons::Check {} span { "{note}" } } }
                        if remote_open() {
                            div { class: "setting-drawer",
                                div { class: "lib-filters",
                                    for (key, label) in [("", "All OS"), ("android", "Android"), ("android-tv", "Android TV"), ("linux", "Linux"), ("windows", "Windows"), ("macos", "macOS")] {
                                        button { key: "{label}", class: if remote_os() == key { "pill accent small" } else { "pill glass small" },
                                            onclick: move |_| {
                                                remote_os.set(key.to_owned());
                                                remote_loading.set(true);
                                                remote_err.set(None);
                                                let want = key.to_owned();
                                                spawn(async move {
                                                    let f = (!want.is_empty()).then_some(want.as_str());
                                                    match backend::list_remote_reports(f).await {
                                                        Ok(list) => remote_reports.set(list),
                                                        Err(e) => remote_err.set(Some(format!("{e}"))),
                                                    }
                                                    remote_loading.set(false);
                                                });
                                            },
                                            "{label}"
                                        }
                                    }
                                }
                                if remote_loading() { p { class: "setting-about", "Loading reports…" } }
                                else if let Some(err) = remote_err() { p { class: "addon-problem", "{err}" } }
                                else if remote_reports.read().is_empty() { p { class: "setting-about", "No issues reported for this filter yet." } }
                                else {
                                    div { class: "diag-reports",
                                        for r in remote_reports.read().iter() {
                                            div { key: "{r.id}", class: "diag-report-item",
                                                div { class: "diag-report-head",
                                                    span { class: "tag q", "{r.os}" }
                                                    span { class: "tag hdr", "{r.kind}" }
                                                    span { class: "dim tnum", "v{r.version} · {r.device} ({r.arch})" }
                                                }
                                                p { class: "diag-report-summary", "{r.summary}" }
                                                if !r.details.is_empty() {
                                                    p { class: "dim diag-report-detail", "{r.details.lines().take(3).collect::<Vec<_>>().join(\" · \")}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        div { class: "setting-row",
                            div { class: "setting-copy",
                                span { class: "setting-name", "Attributions & licences" }
                                span { class: "setting-about", "Keyless metadata from BetterPosters, ElfHosted, TVmaze (CC BY-SA), Wikidata (CC0), Wikipedia (CC BY-SA) and AniList." }
                            }
                            div { class: "setting-controls",
                                button { class: "pill glass small", onclick: move |_| legal_open.set(!legal_open()),
                                    icons::Info {} span { if legal_open() { "Hide" } else { "Privacy & licences" } } }
                            }
                        }
                        if legal_open() {
                            div { class: "setting-drawer",
                                div { class: "legal-text",
                                    p { "Privacy: Veylo is local-first, runs Picks AI on-device, disables telemetry and stream browsing-prefetch by default, and end-to-end encrypts optional cloud sync with your 256-bit Sync Key (see PRIVACY.md)." }
                                    p { "Open-Source Licences: Veylo source is MIT-licensed. Distributed binaries link GPL-licensed libmpv and FFmpeg (combined packages governed by GPL; source links and licence files are bundled in licenses/ and THIRD_PARTY_LICENSES.md). Outfit and Inter fonts are licensed under SIL OFL 1.1; Lucide icons are ISC-licensed. ONNX Runtime and bge-small-en-v1.5 are MIT-licensed." }
                                }
                                div { class: "credit-pills",
                                    LinkButton { url: "https://www.tvmaze.com/", label: "TVmaze" }
                                    LinkButton { url: "https://www.wikidata.org/", label: "Wikidata" }
                                    LinkButton { url: "https://www.wikipedia.org/", label: "Wikipedia" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A link that opens in the browser on explicit user tap.
#[component]
fn LinkButton(url: &'static str, label: &'static str) -> Element {
    rsx! {
        button { class: "link-btn", title: "{url}", onclick: move |_| crate::open_external(url),
            icons::Link {} span { "{label}" } }
    }
}

/// Screen HDR capabilities and desktop HDR output toggle inside the Display & Discovery card.
#[component]
fn ScreenRows(settings: Signal<backend::Settings>) -> Element {
    let mut screen = use_signal(crate::display::screen);
    let mut checked_flash = use_signal(|| false);
    let (title, about) = crate::display::summary(screen(), settings().hdr_output);
    rsx! {
        div { class: "setting-row",
            div { class: "setting-copy",
                div { class: "setting-title-line",
                    span { class: "setting-name", "Display capabilities" }
                    span { class: "status-pill muted", "{title}" }
                }
                span { class: "setting-about", "{about}" }
            }
            div { class: "setting-controls",
                button { class: "pill glass small", onclick: move |_| {
                    screen.set(crate::display::screen());
                    checked_flash.set(true);
                    spawn(async move { backend::sleep(1800).await; checked_flash.set(false); });
                }, if checked_flash() { icons::Check {} } else { icons::Refresh {} } span { if checked_flash() { "Checked" } else { "Check" } } }
            }
        }
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            div { class: "setting-row",
                div { class: "setting-copy",
                    span { class: "setting-name", "HDR output" }
                    span { class: "setting-about", "Automatic sends HDR when system HDR/EDR is active; Always SDR tone-maps on washed-out displays." }
                }
                div { class: "segmented", role: "radiogroup", aria_label: "HDR output",
                    i { class: "slider", aria_hidden: "true" }
                    for (on, name) in [(true, "Automatic"), (false, "Always SDR")] {
                        button { class: if settings().hdr_output == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().hdr_output == on}",
                            onclick: move |_| settings.set(backend::update_settings(|s| s.hdr_output = on)), "{name}" }
                    }
                }
            }
        }
    }
}
