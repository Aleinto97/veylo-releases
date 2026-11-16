//! Library as a viewing history: the last week as big cards, then every month as a grid.
use crate::{backend::{self, Media, Seen}, home::PosterCard, icons, Shell};
use dioxus::prelude::*;

#[component]
pub fn Library() -> Element {
    // No title in front here: the accent goes back to neutral.
    let mut lamp = use_context::<Shell>().lamp_source;
    use_hook(move || lamp.set(String::new()));
    let history = use_resource(|| async { backend::viewing_history().await });
    // Saved to watch (S or the bookmark on a title page), newest first as the store keeps them.
    let saved = use_resource(|| async { backend::library().await.map(|l| l.into_iter().filter(|m| m.saved).collect::<Vec<_>>()).unwrap_or_default() });
    // Titles removed this visit vanish at once; the store already forgot them.
    let gone = use_signal(std::collections::HashSet::<String>::new);
    use_context_provider(|| Gone(gone));
    let now = backend::now_secs();
    rsx! {
        div { class: "library",
            h1 { class: "display title", "Library" }
            if let Some(list) = saved() {
                if !list.is_empty() {
                    section { class: "lib-section",
                        div { class: "group-head", span { class: "group-icon", icons::Bookmark {} } span { class: "group-name", "Saved" }
                            span { class: "tnum dim", "{list.len()}" } }
                        div { class: "lib-grid",
                            for m in list { div { key: "saved-{m.key()}", class: "lib-item", PosterCard { media: m.clone() } } }
                        }
                    }
                }
            }
            match &*history.read() {
                None => rsx! { div { class: "week-strip", for i in 0..3 { div { key: "{i}", class: "week-card skeleton-card" } } } },
                Some(Err(e)) => rsx! { div { class: "notice glass", span { class: "warn-icon", icons::Alert {} } span { "{e}" } } },
                Some(Ok(list)) if list.is_empty() && saved().is_some_and(|s| !s.is_empty()) => rsx! {},
                Some(Ok(list)) if list.is_empty() => rsx! {
                    div { class: "center-note", div { class: "glass panel", span { class: "big-icon dim", icons::Stack {} } p { "Nothing watched yet" } } }
                },
                Some(Ok(list)) => {
                    let list: Vec<Seen> = list.iter().filter(|s| !gone.read().contains(&s.media.key())).cloned().collect();
                    let week: Vec<Seen> = list.iter().filter(|s| now - s.at < 7 * 86_400).cloned().collect();
                    // Older viewing, grouped by calendar month, latest month first.
                    let mut months: Vec<((i64, u32), Vec<Seen>)> = Vec::new();
                    for s in list.iter().filter(|s| now - s.at >= 7 * 86_400) {
                        let (y, m, _, _) = backend::civil(s.at);
                        match months.last_mut() { Some((k, v)) if *k == (y, m) => v.push(s.clone()), _ => months.push(((y, m), vec![s.clone()])) }
                    }
                    let this_year = backend::civil(now).0;
                    rsx! {
                        if !week.is_empty() {
                            section { class: "lib-section",
                                div { class: "group-head", span { class: "group-icon", icons::Clock {} } span { class: "group-name", "This week" }
                                    span { class: "tnum dim", "{week.len()}" } }
                                div { class: "week-strip",
                                    for s in week { WeekCard { key: "{s.media.key()}", seen: s.clone(), now } }
                                }
                            }
                        }
                        for ((y, m), items) in months {
                            section { class: "lib-section", key: "{y}-{m}",
                                div { class: "group-head",
                                    span { class: "group-icon", icons::Calendar {} }
                                    span { class: "group-name", if y == this_year { "{backend::month_name(m)}" } else { "{backend::month_name(m)} {y}" } }
                                    span { class: "tnum dim", "{items.len()}" }
                                }
                                div { class: "lib-grid",
                                    for s in items { div { key: "{s.media.key()}", class: "lib-item", PosterCard { media: s.media.clone() } Forget { media: s.media.clone() } } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A wide card for this week: artwork, title, where you are, when you watched.
#[component]
fn WeekCard(seen: Seen, now: i64) -> Element {
    let mut shell = use_context::<Shell>();
    let m = seen.media.clone();
    let art = crate::home::use_artwork(&m);
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| m.backdrop().to_owned());
    let fraction = backend::progress_fraction(&m);
    // With the episode shown as a tag, the text only says what is left.
    let context = backend::resume_context(&m);
    let context = match &seen.episode { Some(ep) => context.strip_prefix(ep.as_str()).map(|r| r.trim_start_matches(", ").to_owned()).unwrap_or(context), None => context };
    let done = if m.kind == "movie" { backend::film_watched(&m) } else { m.watched };
    let state = if done { "Watched".to_string() } else if backend::in_progress(&m) { context } else { "Started".into() };
    let (open, resume) = (m.clone(), m.clone());
    rsx! {
        div { class: "week-card",
            button { class: "week-open", aria_label: "Open {m.name}", onclick: move |_| shell.opened.set(Some(open.clone())),
                if !backdrop.is_empty() { img { src: "{backdrop}", alt: "", loading: "lazy" } }
                div { class: "week-copy",
                    span { class: "week-when", "{backend::when_label(seen.at, now)}" }
                    span { class: "display week-title", "{m.name}" }
                    span { class: "week-state",
                        if let Some(ep) = &seen.episode { span { class: "tag q tnum", "{ep}" } }
                        if done { span { class: "week-check", icons::Check {} } }
                        span { "{state}" }
                    }
                }
                if fraction > 0.0 && !done { div { class: "card-progress", div { style: "width: {fraction * 100.0}%" } } }
            }
            Forget { media: m.clone() }
            if !done {
                icons::IconButton { label: "Resume {m.name}", class: "glass week-play", onclick: move |_| crate::title::resume_now(shell, resume.clone()), icons::Play {} }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Gone(Signal<std::collections::HashSet<String>>);

/// Remove from the Library: a first press asks, a second deletes the title's history.
#[component]
fn Forget(media: Media) -> Element {
    let Gone(mut gone) = use_context::<Gone>();
    let mut armed = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let name = media.name.clone();
    rsx! {
        div { class: if armed() { "forget armed" } else { "forget" },
            onmouseleave: move |_| armed.set(false),
            if armed() {
                button { class: "pill forget-confirm", disabled: busy(), aria_label: "Remove {name} and its history",
                    onclick: move |e| {
                        e.stop_propagation();
                        let m = media.clone();
                        busy.set(true);
                        spawn(async move {
                            if backend::forget(&m).await.is_ok() { gone.write().insert(m.key()); }
                            busy.set(false);
                            armed.set(false);
                        });
                    },
                    icons::Trash {} span { "Remove" }
                }
                icons::IconButton { label: "Keep", class: "glass small", onclick: move |e: Event<MouseData>| { e.stop_propagation(); armed.set(false) }, icons::Close {} }
            } else {
                icons::IconButton { label: "Remove from Library", class: "glass small forget-btn", onclick: move |e: Event<MouseData>| { e.stop_propagation(); armed.set(true) }, icons::Trash {} }
            }
        }
    }
}
