//! Settings → Account: sign in (Google, email and password, or a computer linked by scanning its
//! QR code with a signed-in phone or tablet), zero-knowledge E2EE Recovery Key, and sync status.
use crate::{backend, icons};
use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum Panel { Choices, Email, Qr }

#[component]
pub fn AccountBlock() -> Element {
    let mut account = use_signal(backend::account);
    let mut busy = use_signal(|| None::<&'static str>);
    let mut problem = use_signal(|| None::<String>);
    let mut note = use_signal(|| None::<String>);
    let mut panel = use_signal(|| Panel::Choices);
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut last = use_signal(backend::last_synced);
    let mut confirm_delete = use_signal(|| false);
    let mut show_key = use_signal(|| false);
    // Some(true) once the clipboard took the key; Some(false) when it refused.
    let mut key_copied = use_signal(|| None::<bool>);
    // The Recovery Key is read from its file once, off the render path, and again only when the
    // account, the key or what sync found out about it may have changed.
    let mut recovery_epoch = use_signal(|| 0u32);
    let recovery = use_resource(move || async move {
        let _ = recovery_epoch();
        account()?;
        if backend::sync_key_needed() { return None; }
        backend::on_runtime(async { tokio::task::spawn_blocking(backend::sync_recovery_code).await.ok().flatten() }).await.ok().flatten()
    });
    let mut recovery_draft = use_signal(String::new);
    let mut enter_key_open = use_signal(backend::sync_key_needed);
    let mut pending_link = use_signal(|| None::<backend::PendingLink>);
    let mut last_uid = use_signal(|| account.peek().as_ref().map(|a| a.uid.clone()));
    use_effect(move || {
        let uid = account().map(|a| a.uid);
        if *last_uid.peek() != uid {
            last_uid.set(uid);
            show_key.set(false);
            confirm_delete.set(false);
            key_copied.set(None);
            password.set(String::new());
            recovery_draft.set(String::new());
            pending_link.set(None);
            enter_key_open.set(backend::sync_key_needed());
        }
    });
    if !backend::accounts_available() { return rsx! {}; }

    let mut run = move |label: &'static str, job: std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>>>>| {
        if busy.peek().is_some() { return; }
        busy.set(Some(label));
        problem.set(None);
        note.set(None);
        spawn(async move {
            match job.await { Ok(n) => note.set(n), Err(e) => problem.set(Some(format!("{e:#}"))) }
            busy.set(None);
            account.set(backend::account());
            last.set(backend::last_synced());
            // A sync may have found the key wrong (or right): the Recovery Key shown follows it.
            recovery_epoch += 1;
        });
    };
    let working = busy();
    let creds_empty = email().trim().is_empty() || password().is_empty();
    let mut submit_sign_in = move || {
        let (e, p) = (email.peek().clone(), password.peek().clone());
        if e.trim().is_empty() || p.is_empty() { return; }
        run("in", Box::pin(async move { backend::sign_in(e, p).await?; Ok(None) }));
    };

    rsx! {
        section { class: "settings-block account-block",
            div { class: "group-head",
                span { class: "group-icon", icons::Users {} }
                span { class: "group-name", "Account & Sync" }
            }
            div { class: "settings-group glass",
                match account() {
                    Some(a) => {
                        let display = if !a.name.is_empty() { a.name.clone() } else if !a.email.is_empty() { a.email.clone() } else { "Signed in".to_owned() };
                        let initial = display.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "V".to_owned());
                        let key_needed = backend::sync_key_needed();
                        // Only while the key is this account's own: another device's data cannot be
                        // opened with a fresh one, so showing or passing it on would mislead.
                        let recovery_code: Option<String> = if key_needed { None } else { (*recovery.read()).clone().flatten() };
                        rsx! {
                            div { class: "setting-row",
                                div { class: "account-hero",
                                    span { class: "account-avatar", aria_hidden: "true", "{initial}" }
                                    div { class: "setting-copy",
                                        div { class: "setting-title-line",
                                            span { class: "setting-name", "{display}" }
                                            if key_needed {
                                                span { class: "status-pill warn", icons::Alert {} span { "Key needed" } }
                                            } else {
                                                span { class: "status-pill ok", icons::Lock {} span { "E2EE active" } }
                                            }
                                        }
                                        span { class: "setting-about",
                                            if !a.email.is_empty() && !a.name.is_empty() { "{a.email} · " }
                                            if a.provider == "google" { "Google · " }
                                            {synced_label(last())}
                                        }
                                    }
                                }
                                div { class: "setting-controls",
                                    button { class: "pill glass small", disabled: working.is_some(),
                                        onclick: move |_| run("sync", Box::pin(async { let (took, sent) = backend::sync_now().await?;
                                            Ok(Some(if took + sent == 0 { "Everything is up to date.".into() } else { format!("Synced: {took} in, {sent} out.") })) })),
                                        icons::Refresh {} span { if working == Some("sync") { "Syncing…" } else { "Sync now" } } }
                                }
                            }
                            div { class: "setting-row",
                                div { class: "setting-copy",
                                    span { class: "setting-name", "End-to-end encryption key" }
                                    span { class: "setting-about",
                                        if key_needed { "This account has encrypted cloud data from another device. Link via QR or enter its Recovery Key to unlock sync." }
                                        else { "Library, progress, likes, preferences and add-ons are AES-256-GCM encrypted on-device. Keep a backup of your Recovery Key for new devices." }
                                    }
                                }
                                div { class: "setting-controls",
                                    if let Some(code) = recovery_code.clone() {
                                        button { class: "pill glass small", onclick: move |_| show_key.set(!show_key()),
                                            span { if show_key() { "Hide" } else { "Reveal" } } }
                                        button { class: "pill glass small", onclick: move |_| {
                                            // "Copied" only once the clipboard accepted it.
                                            let js = format!("try {{ await navigator.clipboard.writeText({}); return true; }} catch (e) {{ return false; }}", serde_json::to_string(&code).unwrap_or_default());
                                            spawn(async move {
                                                let ok = document::eval(&js).await.ok().and_then(|v| v.as_bool()).unwrap_or(false);
                                                key_copied.set(Some(ok));
                                                backend::sleep(2200).await;
                                                key_copied.set(None);
                                            });
                                        }, icons::Copy {} span { match key_copied() { Some(true) => "Copied", Some(false) => "Couldn't copy", None => "Copy" } } }
                                    }
                                    button { class: if key_needed { "pill accent small" } else { "pill glass small" },
                                        onclick: move |_| enter_key_open.set(!enter_key_open()),
                                        span { if enter_key_open() { "Close" } else { "Enter key" } } }
                                }
                            }
                            if show_key() && !key_needed {
                                if let Some(code) = recovery_code.clone() {
                                    div { class: "setting-drawer",
                                        span { class: "setting-about", "Your 256-bit Sync Recovery Key (never shared with Firebase):" }
                                        pre { class: "diag-log", "{code}" }
                                    }
                                }
                            }
                            if enter_key_open() || key_needed {
                                div { class: "setting-drawer",
                                    div { class: "setting-input-group",
                                        input { class: "key-field", r#type: "password", placeholder: "VEYLO-… recovery key", aria_label: "Sync recovery key", autocomplete: "off",
                                            value: "{recovery_draft}",
                                            onkeydown: move |e: KeyboardEvent| { e.stop_propagation(); },
                                            oninput: move |e| recovery_draft.set(e.value()),
                                        }
                                        button { class: "pill accent small", disabled: working.is_some() || recovery_draft().trim().is_empty(),
                                            onclick: move |_| {
                                                let code = recovery_draft.peek().clone();
                                                run("unlock", Box::pin(async move {
                                                    let (took, sent) = backend::set_sync_recovery_code(&code).await?;
                                                    recovery_draft.set(String::new());
                                                    show_key.set(false);
                                                    enter_key_open.set(false);
                                                    Ok(Some(format!("Recovery key applied ({took} in, {sent} out).")))
                                                }));
                                            },
                                            icons::Unlock {} span { if working == Some("unlock") { "Unlocking…" } else { "Unlock & sync" } }
                                        }
                                    }
                                }
                            }
                            if cfg!(target_os = "android") && !crate::is_tv() {
                                div { class: "setting-row",
                                    div { class: "setting-copy",
                                        span { class: "setting-name", "Link a computer or TV" }
                                        span { class: "setting-about",
                                            if key_needed { "Enter this account's Recovery Key first: until this device can open its data, it has no working key to hand on." }
                                            else { "Scan the QR code shown on another device, confirm the 6-digit code, then approve to transfer your session and encryption key." }
                                        }
                                    }
                                    div { class: "setting-controls",
                                        button { class: "pill glass small", disabled: working.is_some() || key_needed,
                                            onclick: move |_| {
                                                pending_link.set(None);
                                                run("scan", Box::pin(async move {
                                                    let scanned = scan().await?;
                                                    pending_link.set(Some(scanned));
                                                    Ok(None)
                                                }));
                                            },
                                            icons::Camera {} span { if working == Some("scan") { "Scanning…" } else { "Scan QR code" } } }
                                    }
                                }
                                if let Some(link) = pending_link().filter(|_| !key_needed) {
                                    div { class: "setting-drawer",
                                        span { class: "setting-name", "Approve sign-in on another device?" }
                                        span { class: "setting-about", "Check that the other screen shows verification code {link.check_code}. Approving transfers your account session and E2EE Recovery Key." }
                                        div { class: "account-actions",
                                            button { class: "pill accent small", disabled: working.is_some() || key_needed,
                                                onclick: {
                                                    let code = link.code.clone();
                                                    move |_| {
                                                        let code = code.clone();
                                                        run("link", Box::pin(async move {
                                                            backend::answer_link(&code).await?;
                                                            pending_link.set(None);
                                                            Ok(Some("The device is signed in.".into()))
                                                        }));
                                                    }
                                                },
                                                icons::Check {} span { if working == Some("link") { "Approving…" } else { "Approve & link" } } }
                                            button { class: "pill glass small", disabled: working.is_some(),
                                                onclick: move |_| pending_link.set(None),
                                                span { "Cancel" } }
                                        }
                                    }
                                }
                            }
                            div { class: "setting-row",
                                div { class: "setting-copy",
                                    span { class: "setting-name", "Session & cloud data" }
                                    span { class: "setting-about", "Signing out keeps your local library. Deleting removes your account and all encrypted cloud data." }
                                }
                                div { class: "setting-controls",
                                    button { class: "pill glass small", disabled: working.is_some(),
                                        onclick: move |_| {
                                            backend::sign_out();
                                            account.set(None);
                                            note.set(None);
                                            problem.set(None);
                                            show_key.set(false);
                                            confirm_delete.set(false);
                                            enter_key_open.set(false);
                                            key_copied.set(None);
                                            password.set(String::new());
                                            recovery_draft.set(String::new());
                                            pending_link.set(None);
                                        },
                                        span { "Sign out" } }
                                    if confirm_delete() {
                                        span { class: "confirm", role: "group", aria_label: "Delete the account?",
                                            button { class: "pill forget-confirm", disabled: working.is_some(),
                                                onclick: move |_| { confirm_delete.set(false); run("delete", Box::pin(async { backend::delete_account().await?; Ok(Some("Account deleted.".into())) })); },
                                                icons::Trash {} span { "Confirm delete" } }
                                            icons::IconButton { label: "Keep it", class: "glass small", onclick: move |_| confirm_delete.set(false), icons::Close {} }
                                        }
                                    } else {
                                        button { class: "pill glass small danger", disabled: working.is_some(),
                                            onclick: move |_| confirm_delete.set(true),
                                            icons::Trash {} span { "Delete account" } }
                                    }
                                }
                            }
                        }
                    },
                    None => rsx! {
                        div { class: "setting-row",
                            div { class: "setting-copy",
                                div { class: "setting-title-line",
                                    span { class: "setting-name", "Zero-knowledge cloud sync" }
                                    span { class: "status-pill muted", "Optional" }
                                }
                                span { class: "setting-about", "Sign in to end-to-end encrypt and sync your library, progress, likes, preferences and add-ons across devices." }
                            }
                            div { class: "account-choices",
                                if backend::google_available() {
                                    button { class: "pill accent small", disabled: working.is_some(),
                                        onclick: move |_| run("google", Box::pin(async { backend::sign_in_google().await?; Ok(None) })),
                                        span { class: "google-g", aria_hidden: "true", "G" }
                                        span { if working == Some("google") { if cfg!(target_os = "android") { "Signing in…" } else { "Finish in browser…" } } else { "Google" } } }
                                }
                                button { class: if panel() == Panel::Email { "pill glass small on" } else { "pill glass small" }, disabled: working.is_some(),
                                    onclick: move |_| panel.set(if panel() == Panel::Email { Panel::Choices } else { Panel::Email }), icons::Pen {} span { "Email" } }
                                if !cfg!(target_os = "android") || crate::is_tv() {
                                    button { class: if panel() == Panel::Qr { "pill glass small on" } else { "pill glass small" }, disabled: working.is_some() && panel() != Panel::Qr,
                                        onclick: move |_| panel.set(if panel() == Panel::Qr { Panel::Choices } else { Panel::Qr }), icons::Grid {} span { "Phone QR" } }
                                }
                            }
                        }
                        if panel() == Panel::Email {
                            div { class: "setting-drawer",
                                input { class: "key-field", r#type: "email", placeholder: "Email", aria_label: "Email", autocomplete: "email",
                                    value: "{email}",
                                    onkeydown: move |e: KeyboardEvent| { e.stop_propagation(); if e.key() == Key::Enter { submit_sign_in(); } },
                                    oninput: move |e| email.set(e.value()) }
                                input { class: "key-field", r#type: "password", placeholder: "Password", aria_label: "Password", autocomplete: "current-password",
                                    value: "{password}",
                                    onkeydown: move |e: KeyboardEvent| { e.stop_propagation(); if e.key() == Key::Enter { submit_sign_in(); } },
                                    oninput: move |e| password.set(e.value()) }
                                div { class: "account-actions",
                                    button { class: "pill accent small", disabled: working.is_some() || creds_empty,
                                        onclick: move |_| submit_sign_in(),
                                        span { if working == Some("in") { "Signing in…" } else { "Sign in" } } }
                                    button { class: "pill glass small", disabled: working.is_some() || creds_empty,
                                        onclick: move |_| { let (e, p) = (email(), password()); run("up", Box::pin(async move { backend::sign_up(e, p).await?; Ok(None) })); },
                                        span { if working == Some("up") { "Creating…" } else { "Create account" } } }
                                    button { class: "link-btn", disabled: working.is_some(),
                                        onclick: move |_| { let e = email(); run("reset", Box::pin(async move {
                                            anyhow::ensure!(!e.trim().is_empty(), "Type your email first.");
                                            backend::reset_password(e).await?; Ok(Some("Check your email for a link to choose a new password.".into())) })); },
                                        span { "Forgot password?" } }
                                }
                            }
                        }
                        if panel() == Panel::Qr {
                            div { class: "setting-drawer",
                                LinkPanel { on_signed_in: move |_| { account.set(backend::account()); panel.set(Panel::Choices); } }
                            }
                        }
                    },
                }
                if let Some(p) = problem() { p { class: "addon-problem", role: "alert", icons::Alert {} span { "{p}" } } }
                if let Some(n) = note() { p { class: "account-note", role: "status", icons::Check {} span { "{n}" } } }
            }
        }
    }
}

#[cfg(target_os = "android")]
async fn scan() -> anyhow::Result<backend::PendingLink> { backend::scan_link_code().await }
#[cfg(not(target_os = "android"))]
async fn scan() -> anyhow::Result<backend::PendingLink> { anyhow::bail!("Scanning is on phones and tablets.") }

fn synced_label(at: i64) -> String {
    if at == 0 { return "Not synced yet".into(); }
    let ago = (backend::now_secs() - at).max(0);
    match ago { 0..=59 => "Synced just now".into(), 60..=3599 => format!("Synced {} min ago", ago / 60), _ => format!("Synced {} h ago", ago / 3600) }
}

/// The computer's QR code, renewed every ten minutes, and the wait for a phone to answer it.
/// An offer nobody answered is taken down (its request and the passing account that waited with
/// it) when a new code replaces it or the panel closes.
#[component]
fn LinkPanel(on_signed_in: EventHandler<()>) -> Element {
    let mut offer = use_signal(|| None::<backend::LinkOffer>);
    let mut problem = use_signal(|| None::<String>);
    let mut round = use_signal(|| 0u32);
    // The offer in use, outside the signals: a closing panel can no longer read those.
    let live = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(None::<backend::LinkOffer>)));
    use_drop({
        let live = live.clone();
        move || { if let Some(old) = live.borrow_mut().take() { backend::cancel_link_soon(old); } }
    });
    use_effect(move || {
        let _ = round();
        if let Some(old) = live.borrow_mut().take() { backend::cancel_link_soon(old); }
        let live = live.clone();
        spawn(async move {
            offer.set(None);
            problem.set(None);
            let made = match backend::offer_link().await { Ok(o) => o, Err(e) => { problem.set(Some(format!("{e:#}"))); return } };
            *live.borrow_mut() = Some(made.clone());
            offer.set(Some(made.clone()));
            let started = backend::now_secs();
            let mut hiccups = 0u32;
            while backend::now_secs() - started < 600 {
                backend::sleep(2000).await;
                if offer.peek().as_ref() != Some(&made) { return; }
                match backend::take_link(&made).await {
                    Ok(Some(_)) => { live.borrow_mut().take(); on_signed_in.call(()); return; }
                    Ok(None) => { hiccups = 0; problem.set(None); }
                    // A dropped connection or a busy service is not the end of the wait: keep asking.
                    Err(e) if backend::is_transient(&e) && hiccups < 90 => { hiccups += 1; problem.set(Some(format!("{e:#}"))); }
                    Err(e) => { problem.set(Some(format!("{e:#}"))); return; }
                }
            }
            round += 1;
        });
    });
    let check_code = offer().map(|o| o.check_code());
    let qr = offer().and_then(|o| backend::qr_svg(&o.code));
    rsx! {
        div { class: "link-panel",
            match qr {
                Some(src) => rsx! { img { class: "link-qr", src: "{src}", alt: "Sign-in code" } },
                None => rsx! { div { class: "link-qr skeleton-card" } },
            }
            div { class: "setting-copy",
                span { class: "setting-name", "Scan with Veylo on your phone or tablet" }
                span { class: "setting-about", "Open Settings → Account → Scan QR code, verify the code below matches, and approve to transfer your account and E2EE key." }
                if let Some(code) = check_code {
                    span { class: "status-pill ok tnum", "Verification code: {code}" }
                }
                if let Some(p) = problem() { span { class: "addon-problem", icons::Alert {} span { "{p}" } } }
                div { class: "account-actions",
                    button { class: "pill glass small", onclick: move |_| round += 1, icons::Refresh {} span { "New code" } }
                }
            }
        }
    }
}

/// Confirmation toast when your synced settings and add-ons arrive automatically on this device.
#[component]
pub fn KeysPrompt() -> Element {
    let mut arrived = use_signal(|| None::<(u64, String)>);
    use_future(move || async move {
        let mut seq = 0u64;
        loop {
            if let Some(from) = backend::take_keys_arrived() {
                seq = seq.wrapping_add(1);
                let id = seq;
                arrived.set(Some((id, from)));
                spawn(async move {
                    backend::sleep(7000).await;
                    if arrived.peek().as_ref().map(|t| t.0) == Some(id) {
                        arrived.set(None);
                    }
                });
            }
            backend::sleep(2000).await;
        }
    });
    rsx! {
        if let Some((_, from)) = arrived() {
            div { class: "toast glass keys-toast", role: "status",
                span { class: "lamp", icons::Check {} }
                span { class: "toast-title",
                    if from.is_empty() { "Your settings and add-ons synced to this device" }
                    else { "Your settings and add-ons arrived from {from}" }
                }
            }
        }
    }
}
