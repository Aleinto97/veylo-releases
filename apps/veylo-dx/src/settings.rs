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
    let mut tmdb_checking = use_signal(|| false);
    let mut tmdb_problem = use_signal(|| None::<String>);
    let mut custom_url = use_signal(String::new);
    let mut import_open = use_signal(|| false);
    let mut import_text = use_signal(String::new);
    let mut data_note = use_signal(|| None::<String>);
    let mut diag_open = use_signal(|| false);
    let mut diag_text = use_signal(String::new);
    let mut diag_copied = use_signal(|| false);
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
    // Run one add-on job, then reload the list; errors carry no links or keys.
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
    let mut save_rd_key = move |key: String| {
        if busy.peek().is_some() { return; }
        busy.set(Some("key".into()));
        problem.set(None);
        spawn(async move {
            match addons::check_rd_key(key.clone()).await {
                Ok(account) => {
                    settings.set(backend::update_settings(|s| { s.rd_key = Some(key); s.rd_key_removed = false; }));
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
        let key = if addons::language(&key).is_some() { key } else { backend::DEFAULT_LANGUAGE.to_owned() };
        settings.set(backend::update_settings(|s| s.stream_language = Some(key)));
        run("language".into(), Box::pin(async { addons::reconfigure().await }));
    };
    rsx! {
        div { class: "settings",
            h1 { class: "display title", "Settings" }

            crate::account_ui::AccountBlock {}

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Grid {} } span { class: "group-name", "Charts and posters" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Source" }
                        span { class: "dim setting-about",
                            if settings().better_posters { "BetterPosters: Trakt and TMDB charts on Home, posters with rating and genre tags, and fuller details. Cinemeta fills in when it can't answer." }
                            else { "Your add-ons' own catalogs, posters and details" } } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Charts and posters source",
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
            }

            ScreenBlock { settings }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Globe {} } span { class: "group-name", "Artwork" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "TMDB" }
                        span { class: "dim setting-about",
                            if tmdb_checking() { "Checking your key…" }
                            else if settings().tmdb_key.is_some() { "Connected: free sources run first; TMDB adds textless backdrops, actor/producer photos, videos and gap-fill stills" }
                            else { "Optional. Free sources (BetterPosters, TVmaze, AniList, Wikidata) always run first; TMDB adds textless backdrops, actor/producer photos, taglines and videos" } } }
                    if settings().tmdb_key.is_some() {
                        button { class: "pill glass", onclick: move |_| {
                            settings.set(backend::update_settings(|s| s.tmdb_key = None));
                            backend::clear_artwork_caches();
                        }, icons::Trash {} span { "Remove key" } }
                    } else {
                        LinkButton { url: addons::TMDB_KEY_PAGE, label: "Get a TMDB key (API Key, not the Read Access Token)" }
                        // Masked while typing; checked with TMDB, then saved, on Enter.
                        input { class: "key-field", r#type: "password", placeholder: "TMDB API key", aria_label: "TMDB API key", autocomplete: "off",
                            disabled: tmdb_checking(),
                            onkeydown: move |e| { e.stop_propagation(); },
                            onchange: move |e| {
                                let key = e.value().trim().to_owned();
                                if key.is_empty() || tmdb_checking() { return; }
                                tmdb_checking.set(true);
                                tmdb_problem.set(None);
                                spawn(async move {
                                    match backend::check_tmdb_key(key.clone()).await {
                                        Ok(()) => {
                                            settings.set(backend::update_settings(|s| s.tmdb_key = Some(key)));
                                            backend::clear_artwork_caches();
                                        }
                                        Err(e) => tmdb_problem.set(Some(format!("{e:#}"))),
                                    }
                                    tmdb_checking.set(false);
                                });
                            } }
                    }
                }
                if let Some(p) = tmdb_problem() { p { class: "addon-problem", role: "alert", icons::Alert {} span { "{p}" } } }
            }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::Sparkles {} } span { class: "group-name", "Picks" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Local AI" }
                        span { class: "dim setting-about", "The bge-small model runs on this device and compares synopses. Keywords from TMDB, or Wikidata without a key, enrich its input." } }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Series data" }
                        span { class: "dim setting-about", "Series casts, schedules and some Picks series come from TVmaze, under CC BY-SA." } }
                    LinkButton { url: "https://www.tvmaze.com/", label: "TVmaze" }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Open data" }
                        span { class: "dim setting-about", "Credits, critics' scores and keywords come from Wikidata (CC0); cast photos and studio logos from Wikimedia Commons, under their authors' licences." } }
                    LinkButton { url: "https://www.wikidata.org/", label: "Wikidata" }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Learned taste" } span { class: "dim setting-about", "What Picks learned from your duels, plays, likes and moods" } }
                    crate::components::StartOver { labelled: true, on_reset: move |_| crate::brain::reset() }
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
                        LinkButton { url: addons::RD_KEY_PAGE, label: "Get your Real-Debrid key" }
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
                        for l in addons::LANGUAGES {
                            option { value: "{l.key}", selected: settings().stream_language.as_deref() == Some(l.key), "{l.label}" }
                        }
                    }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Up Next countdown" }
                        span { class: "dim setting-about", "Show a short countdown to the next episode when credits begin or an episode ends" } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Up Next countdown",
                        i { class: "slider", aria_hidden: "true" }
                        for (on, name) in [(true, "On"), (false, "Off")] {
                            button { class: if settings().auto_next == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().auto_next == on}",
                                onclick: move |_| settings.set(backend::update_settings(|s| s.auto_next = on)), "{name}" }
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
                                    p { class: "dim market-about", "{c.detail()}" }
                                    div { class: "market-actions",
                                        if busy().as_deref() == Some(c.name()) {
                                            span { class: "pill static glass", "Setting up…" }
                                        } else if let Some(r) = row.clone() {
                                            if let Some(v) = r.update.clone() {
                                                { let r = r.clone(); rsx! { button { class: "pill accent", disabled: busy().is_some(), onclick: move |_| { let r = r.clone(); run(c.name().into(), Box::pin(async move { addons::update(&r).await })) }, icons::Refresh {} span { "Update to {v}" } } } }
                                            } else { span { class: "market-status", icons::Check {} span { "Installed" } } }
                                            button { class: "pill glass small", disabled: busy().is_some(), onclick: move |_| { let r = r.clone(); run(c.name().into(), Box::pin(async move { addons::remove(&r).await })) }, icons::Trash {} span { "Remove" } }
                                        } else {
                                            button { class: "pill accent", disabled: busy().is_some() || (c.needs_key() && settings().rd_key.is_none()),
                                                title: if c.needs_key() && settings().rd_key.is_none() { "Add your Real-Debrid key first" } else { "" },
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
                                    if r.removable() {
                                        { let r = r.clone(); let label = r.name.clone(); rsx! { button { class: "pill glass small", disabled: busy().is_some(), onclick: move |_| { let r = r.clone(); run(label.clone(), Box::pin(async move { addons::remove(&r).await })) }, icons::Trash {} span { "Remove" } } } }
                                    }
                                }
                            }
                        }
                        div { class: "setting-row glass", style: "margin-top: 12px",
                            div { class: "setting-copy", span { class: "setting-name", "Custom Stremio add-on" }
                                span { class: "dim setting-about", "Install any Stremio manifest URL (TorBox, AllDebrid, Premiumize, self-hosted AIOStreams, or community add-ons). Links stay masked and are never logged." } }
                            input { class: "key-field", r#type: "password", placeholder: "https://…/manifest.json", aria_label: "Custom Stremio add-on URL", autocomplete: "off",
                                value: "{custom_url}", disabled: busy().is_some(),
                                oninput: move |e| custom_url.set(e.value()),
                                onkeydown: move |e| { e.stop_propagation(); },
                            }
                            button { class: "pill accent small", disabled: busy().is_some() || custom_url().trim().is_empty(),
                                onclick: move |_| {
                                    let url = custom_url.peek().trim().to_owned();
                                    custom_url.set(String::new());
                                    run("custom".into(), Box::pin(async move {
                                        let name = addons::install_custom(url).await?;
                                        check_note.set(Some(format!("Installed {name}.")));
                                        Ok(())
                                    }));
                                },
                                icons::Plus {} span { "Install" }
                            }
                        }
                    },
                }
            }

            section { class: "settings-block",
                div { class: "group-head", span { class: "group-icon", icons::History {} } span { class: "group-name", "Library data & diagnostics" } }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Import or export watch history" }
                        span { class: "dim setting-about", "Bring in a Trakt watched JSON, IMDb CSV, or Veylo backup to warm up Picks right away. Exports include no keys or add-on links." } }
                    div { class: "account-actions",
                        button { class: "pill glass small", onclick: move |_| import_open.set(!import_open()), icons::Upload {} span { if import_open() { "Hide import" } else { "Import" } } }
                        button { class: "pill glass small", onclick: move |_| {
                            data_note.set(None);
                            spawn(async move {
                                match backend::export_watch_history().await {
                                    Ok(json_text) => {
                                        let js = format!("navigator.clipboard?.writeText({}).catch(()=>{{}})", serde_json::to_string(&json_text).unwrap_or_default());
                                        let _ = document::eval(&js);
                                        data_note.set(Some("Watch history exported to veylo-history-export.json and copied to clipboard.".into()));
                                    }
                                    Err(e) => data_note.set(Some(format!("Export failed: {e}"))),
                                }
                            });
                        }, icons::Download {} span { "Export JSON" } }
                    }
                }
                if import_open() {
                    div { class: "account-form glass",
                        textarea { class: "import-area", rows: "5", placeholder: "Paste Trakt watched JSON, Veylo export JSON, or IMDb CSV…",
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
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Automatic issue & performance reporting" }
                        span { class: "dim setting-about", "Enabled by default. Sends redacted crashes, UI layout errors, playback failures, and frame-drop spikes to Firebase so issues can be tracked per OS. Never sends keys, links, or watch history." } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Automatic issue reporting",
                        i { class: "slider", aria_hidden: "true" }
                        for (on, name) in [(true, "On"), (false, "Off")] {
                            button { class: if settings().telemetry_enabled == on { "seg wide on" } else { "seg wide" }, role: "radio", aria_checked: "{settings().telemetry_enabled == on}",
                                onclick: move |_| settings.set(backend::update_settings(|s| s.telemetry_enabled = on)), "{name}" }
                        }
                    }
                }
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Report a bug or UI issue" }
                        span { class: "dim setting-about", "Send a quick note with your OS, device, screen, and redacted diagnostic log directly to Firebase." } }
                    div { class: "account-actions",
                        button { class: "pill accent small", onclick: move |_| bug_open.set(!bug_open()), icons::Alert {} span { if bug_open() { "Close form" } else { "Report issue" } } }
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
                            }, span { if remote_open() { "Hide OS issues" } else { "View OS issues" } } }
                        }
                    }
                }
                if bug_open() {
                    div { class: "account-form glass",
                        div { class: "account-actions",
                            select { class: "select-field", aria_label: "Issue type",
                                value: "{bug_kind}",
                                onchange: move |e| bug_kind.set(e.value()),
                                option { value: "bug", "Bug" }
                                option { value: "ui_error", "UI / layout error" }
                                option { value: "performance", "Performance / stutter" }
                                option { value: "playback_error", "Playback issue" }
                            }
                        }
                        textarea { class: "import-area", rows: "4", placeholder: "What happened? (e.g. Sources sheet shifted right, video stuttered on 4K HDR…)",
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
                                                bug_note.set(Some("Report sent to Firebase with redacted diagnostics.".into()));
                                            }
                                            Err(e) => bug_note.set(Some(format!("Couldn't send report: {e}"))),
                                        }
                                        bug_sending.set(false);
                                    });
                                },
                                icons::Upload {} span { if bug_sending() { "Sending…" } else { "Send to Firebase" } }
                            }
                        }
                    }
                }
                if let Some(note) = bug_note() { p { class: "check-note", role: "status", icons::Check {} span { "{note}" } } }
                if remote_open() {
                    div { class: "account-form glass",
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
                        if remote_loading() { p { class: "dim", "Loading reports from Firebase…" } }
                        else if let Some(err) = remote_err() { p { class: "addon-problem", "{err}" } }
                        else if remote_reports.read().is_empty() { p { class: "dim", "No issues reported for this filter yet." } }
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
                div { class: "setting-row glass",
                    div { class: "setting-copy", span { class: "setting-name", "Diagnostic log" }
                        span { class: "dim setting-about", "Recent playback and sync events with URLs, API keys, and tokens automatically redacted." } }
                    div { class: "account-actions",
                        button { class: "pill glass small", onclick: move |_| {
                            let report = backend::diagnostic_report();
                            diag_text.set(report.clone());
                            let js = format!("navigator.clipboard?.writeText({}).catch(()=>{{}})", serde_json::to_string(&report).unwrap_or_default());
                            let _ = document::eval(&js);
                            diag_copied.set(true);
                        }, icons::Copy {} span { if diag_copied() { "Copied" } else { "Copy log" } } }
                        button { class: "pill glass small", onclick: move |_| {
                            if !diag_open() { diag_text.set(backend::diagnostic_report()); }
                            diag_open.set(!diag_open());
                        }, span { if diag_open() { "Hide" } else { "View" } } }
                    }
                }
                if diag_open() {
                    pre { class: "diag-log glass", "{diag_text}" }
                }
            }
        }
    }
}

/// A link that opens in the browser: a provider's key page (to copy the key and paste it back
/// here) or a source's own site.
#[component]
fn LinkButton(url: &'static str, label: &'static str) -> Element {
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
    let (title, about) = crate::display::summary(screen(), settings().hdr_output);
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
                        span { class: "dim setting-about", "Automatic sends HDR videos to the screen in HDR when Windows HDR is on. Always SDR tone-maps them, for screens whose HDR looks washed out. Applies from the next video." } }
                    div { class: "segmented glass", role: "radiogroup", aria_label: "HDR output",
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
}
