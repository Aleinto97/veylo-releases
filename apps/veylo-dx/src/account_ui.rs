//! Settings → Account: sign in (Google, email and password, or a computer linked by scanning its
//! QR code with a signed-in phone or tablet), and what sync is doing. All of it in the app.
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
    if !backend::accounts_available() { return rsx! {}; }

    // One account job at a time; its error is said in the app's words.
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
            div { class: "group-head", span { class: "group-icon", icons::Users {} } span { class: "group-name", "Account" } }
            match account() {
                Some(a) => rsx! {
                    div { class: "setting-row glass",
                        div { class: "setting-copy",
                            span { class: "setting-name", if !a.name.is_empty() { "{a.name}" } else if !a.email.is_empty() { "{a.email}" } else { "Signed in" } }
                            span { class: "dim setting-about",
                                if !a.email.is_empty() && !a.name.is_empty() { "{a.email} · " }
                                if a.provider == "google" { "Google · " }
                                {synced_label(last())}
                            }
                        }
                        button { class: "pill glass", disabled: working.is_some(),
                            onclick: move |_| run("sync", Box::pin(async { let (took, sent) = backend::sync_now().await?;
                                Ok(Some(if took + sent == 0 { "Everything is up to date.".into() } else { format!("Synced: {took} in, {sent} out.") })) })),
                            icons::Refresh {} span { if working == Some("sync") { "Syncing…" } else { "Sync now" } } }
                    }
                    div { class: "setting-row glass",
                        div { class: "setting-copy", span { class: "setting-name", "What syncs" }
                            span { class: "dim setting-about", "Your library, where you are in each film and episode, what you watched, your likes, API keys and add-ons. Every device signed in to this account shares them automatically." } }
                    }
                    if cfg!(target_os = "android") && !crate::is_tv() {
                        div { class: "setting-row glass",
                            div { class: "setting-copy", span { class: "setting-name", "Sign in on a computer or TV" }
                                span { class: "dim setting-about", "On the computer or TV: Settings → Account → Sign in with your phone. Then scan the code it shows." } }
                            button { class: "pill glass", disabled: working.is_some(),
                                onclick: move |_| run("scan", Box::pin(async { scan().await?; Ok(Some("The device is signed in.".into())) })),
                                icons::Camera {} span { if working == Some("scan") { "Scanning…" } else { "Scan its code" } } }
                        }
                    }
                    div { class: "setting-row glass",
                        div { class: "setting-copy", span { class: "setting-name", "Sign out" }
                            span { class: "dim setting-about", "This device stops syncing. Its library stays." } }
                        button { class: "pill glass", disabled: working.is_some(), onclick: move |_| { backend::sign_out(); account.set(None); note.set(None); }, span { "Sign out" } }
                    }
                    div { class: "setting-row glass",
                        div { class: "setting-copy", span { class: "setting-name", "Delete account" }
                            span { class: "dim setting-about", "Deletes the account and everything synced to it. Each device keeps its own library." } }
                        if confirm_delete() {
                            span { class: "confirm", role: "group", aria_label: "Delete the account?",
                                button { class: "pill forget-confirm", disabled: working.is_some(),
                                    onclick: move |_| { confirm_delete.set(false); run("delete", Box::pin(async { backend::delete_account().await?; Ok(Some("Account deleted.".into())) })); },
                                    icons::Trash {} span { "Delete for good" } }
                                icons::IconButton { label: "Keep it", class: "glass small", onclick: move |_| confirm_delete.set(false), icons::Close {} }
                            }
                        } else {
                            button { class: "pill glass", disabled: working.is_some(), onclick: move |_| confirm_delete.set(true), icons::Trash {} span { "Delete account" } }
                        }
                    }
                },
                None => rsx! {
                    div { class: "setting-row glass",
                        div { class: "setting-copy", span { class: "setting-name", "Sync your devices" }
                            span { class: "dim setting-about", "Sign in to share your library, progress and likes between your phone, tablet and computer." } }
                    }
                    div { class: "account-choices",
                        if backend::google_available() {
                            button { class: "pill accent", disabled: working.is_some(),
                                onclick: move |_| run("google", Box::pin(async { backend::sign_in_google().await?; Ok(None) })),
                                span { class: "google-g", aria_hidden: "true", "G" }
                                span { if working == Some("google") { if cfg!(target_os = "android") { "Signing in…" } else { "Finish in your browser…" } } else { "Continue with Google" } } }
                        }
                        button { class: if panel() == Panel::Email { "pill glass on" } else { "pill glass" }, disabled: working.is_some(),
                            onclick: move |_| panel.set(if panel() == Panel::Email { Panel::Choices } else { Panel::Email }), icons::Pen {} span { "Use email" } }
                        if !cfg!(target_os = "android") || crate::is_tv() {
                            button { class: if panel() == Panel::Qr { "pill glass on" } else { "pill glass" }, disabled: working.is_some() && panel() != Panel::Qr,
                                onclick: move |_| panel.set(if panel() == Panel::Qr { Panel::Choices } else { Panel::Qr }), icons::Grid {} span { "Sign in with your phone" } }
                        }
                    }
                    if panel() == Panel::Email {
                        div { class: "account-form glass",
                            input { class: "key-field", r#type: "email", placeholder: "Email", aria_label: "Email", autocomplete: "email",
                                value: "{email}",
                                onkeydown: move |e: KeyboardEvent| { e.stop_propagation(); if e.key() == Key::Enter { submit_sign_in(); } },
                                oninput: move |e| email.set(e.value()) }
                            input { class: "key-field", r#type: "password", placeholder: "Password", aria_label: "Password", autocomplete: "current-password",
                                value: "{password}",
                                onkeydown: move |e: KeyboardEvent| { e.stop_propagation(); if e.key() == Key::Enter { submit_sign_in(); } },
                                oninput: move |e| password.set(e.value()) }
                            div { class: "account-actions",
                                button { class: "pill accent", disabled: working.is_some() || creds_empty,
                                    onclick: move |_| submit_sign_in(),
                                    span { if working == Some("in") { "Signing in…" } else { "Sign in" } } }
                                button { class: "pill glass", disabled: working.is_some() || creds_empty,
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
                    if panel() == Panel::Qr { LinkPanel { on_signed_in: move |_| { account.set(backend::account()); panel.set(Panel::Choices); } } }
                },
            }
            if let Some(p) = problem() { p { class: "addon-problem", role: "alert", icons::Alert {} span { "{p}" } } }
            if let Some(n) = note() { p { class: "account-note", role: "status", icons::Check {} span { "{n}" } } }
        }
    }
}

#[cfg(target_os = "android")]
async fn scan() -> anyhow::Result<()> { backend::scan_and_link().await }
#[cfg(not(target_os = "android"))]
async fn scan() -> anyhow::Result<()> { anyhow::bail!("Scanning is on phones and tablets.") }

fn synced_label(at: i64) -> String {
    if at == 0 { return "Not synced yet".into(); }
    let ago = (backend::now_secs() - at).max(0);
    match ago { 0..=59 => "Synced just now".into(), 60..=3599 => format!("Synced {} min ago", ago / 60), _ => format!("Synced {} h ago", ago / 3600) }
}

/// The computer's QR code, renewed every ten minutes, and the wait for a phone to answer it.
#[component]
fn LinkPanel(on_signed_in: EventHandler<()>) -> Element {
    let mut offer = use_signal(|| None::<backend::LinkOffer>);
    let mut problem = use_signal(|| None::<String>);
    let mut round = use_signal(|| 0u32);
    use_effect(move || {
        let _ = round();
        spawn(async move {
            offer.set(None);
            problem.set(None);
            let made = match backend::offer_link().await { Ok(o) => o, Err(e) => { problem.set(Some(format!("{e:#}"))); return } };
            offer.set(Some(made.clone()));
            let started = backend::now_secs();
            while backend::now_secs() - started < 600 {
                backend::sleep(2000).await;
                // Closed, or a new code meanwhile: this wait is over.
                if offer.peek().as_ref() != Some(&made) { return; }
                match backend::take_link(&made).await {
                    Ok(Some(_)) => { on_signed_in.call(()); return; }
                    Ok(None) => {}
                    Err(e) => { problem.set(Some(format!("{e:#}"))); return; }
                }
            }
            round += 1;
        });
    });
    let qr = offer().and_then(|o| backend::qr_svg(&o.code));
    rsx! {
        div { class: "account-form glass link-panel",
            match qr {
                Some(src) => rsx! { img { class: "link-qr", src: "{src}", alt: "Sign-in code" } },
                None => rsx! { div { class: "link-qr skeleton-card" } },
            }
            div { class: "setting-copy",
                span { class: "setting-name", "Scan with Veylo on your phone or tablet" }
                span { class: "dim setting-about", "There: Settings → Account → Scan its code. This device signs in to the same account." }
                if let Some(p) = problem() { span { class: "addon-problem", icons::Alert {} span { "{p}" } } }
                button { class: "pill glass", onclick: move |_| round += 1, icons::Refresh {} span { "New code" } }
            }
        }
    }
}

/// Confirmation toast when your synced API keys and add-ons arrive automatically on this device.
#[component]
pub fn KeysPrompt() -> Element {
    let mut arrived = use_signal(|| None::<String>);
    use_future(move || async move {
        loop {
            if let Some(from) = backend::take_keys_arrived() {
                arrived.set(Some(from));
                spawn(async move { backend::sleep(7000).await; arrived.set(None); });
            }
            backend::sleep(2000).await;
        }
    });
    rsx! {
        if let Some(from) = arrived() {
            div { class: "toast glass", role: "status",
                span { class: "lamp", icons::Check {} }
                span { class: "toast-title",
                    if from.is_empty() { "Your keys and add-ons synced to this device" }
                    else { "Your keys and add-ons arrived from {from}" }
                }
            }
        }
    }
}
