//! Search: a full-window finder. The query is the one bold line; results are
//! posters grouped by type, with the closest title promoted above them.
use crate::{backend::{self, Media, SearchOutcome}, icons, picks::PosterButton, Shell};
use dioxus::prelude::*;

const DEBOUNCE_MS: u64 = 280;

#[component]
pub fn SearchOverlay() -> Element {
    let mut shell = use_context::<Shell>();
    let mut query = use_signal(String::new);
    let mut outcome = use_signal(|| None::<SearchOutcome>);
    let mut searching = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut generation = use_signal(|| 0u64);
    let ideas = use_resource(|| async { backend::picks().await.map(|p| p.items).unwrap_or_default() });

    // Each keystroke supersedes the previous request; only the latest may write.
    let mut run = move |q: String, delay: u64| {
        let id = generation() + 1;
        generation.set(id);
        spawn(async move {
            if delay > 0 { backend::sleep(delay).await; }
            if generation() != id { return; }
            if q.trim().is_empty() { outcome.set(None); searching.set(false); error.set(None); return; }
            searching.set(true);
            let result = backend::search(q).await;
            if generation() != id { return; }
            searching.set(false);
            match result {
                Ok(o) => { error.set(None); outcome.set(Some(o)); }
                Err(e) => { outcome.set(None); error.set(Some(format!("{e:#}"))); }
            }
        });
    };
    let mut remember = move || {
        let q = query();
        if q.trim().chars().count() >= 2 { backend::remember_query(&mut shell.history.write(), &q); }
    };
    let mut close = move || { remember(); shell.search_open.set(false); };
    let mut open = move |m: Media| {
        remember();
        backend::remember_item(&mut shell.history.write(), &m);
        shell.opened.set(Some(m));
        shell.search_open.set(false);
    };
    let mut set_query = move |q: String, delay: u64| { query.set(q.clone()); run(q, delay); };

    // Developer option: VEYLO_DX_SEARCH=<query> opens Search with that query.
    use_hook(move || if let Ok(q) = std::env::var("VEYLO_DX_SEARCH") {
        if !q.trim().is_empty() { spawn(async move { query.set(q.clone()); run(q, 0); }); }
    });
    // A name tapped on a title page arrives as a query.
    use_hook(move || if let Some(q) = shell.search_query.take() { spawn(async move { query.set(q.clone()); run(q, 0); }); });
    let q = query();
    let results = outcome.read().as_ref().map(|o| o.results.clone()).unwrap_or_default();
    let several_addons = {
        let mut names: Vec<&String> = results.iter().map(|(a, _)| a).collect();
        names.dedup();
        names.len() > 1
    };
    let top = results.first().filter(|(_, m)| backend::search_rank(&q, &m.name) <= 3).cloned();
    let mut groups: Vec<(String, Vec<Media>)> = Vec::new();
    for (_, m) in results.iter().filter(|(_, m)| Some(m.key()) != top.as_ref().map(|(_, t)| t.key())) {
        match groups.iter_mut().find(|(k, _)| *k == m.kind) {
            Some((_, list)) => list.push(m.clone()),
            None => groups.push((m.kind.clone(), vec![m.clone()])),
        }
    }
    groups.sort_by_key(|(k, _)| match k.as_str() { "movie" => 0, "series" => 1, "anime" => 2, _ => 3 });
    let failed = outcome.read().as_ref().map(|o| o.failed.clone()).unwrap_or_default();
    let catalogs = outcome.read().as_ref().map_or(1, |o| o.catalogs);
    let history = shell.history.read().clone();
    // Spoken only: the field's sweeping rule shows searching, the groups show counts.
    let status = if searching() { "Searching".to_string() }
        else if outcome.read().is_some() { match results.len() { 0 => String::new(), 1 => "1 title".into(), n => format!("{n} titles") } }
        else { String::new() };

    rsx! {
        div { class: "search", inert: (shell.shortcuts_open)().then_some("true"), role: "dialog", aria_modal: "true", aria_label: "Search",
            // Frosted without backdrop-filter (unsupported in some webviews): its own blurred artwork.
            div { class: "search-ambient", aria_hidden: "true",
                if !(shell.lamp_source)().is_empty() { img { src: "{(shell.lamp_source)()}", alt: "" } }
            }
            div { class: "search-head",
                div { class: if searching() { "search-field glass busy" } else { "search-field glass" },
                    span { class: "search-glyph", icons::Search {} }
                    input {
                        class: "query display",
                        r#type: "search",
                        placeholder: "Search",
                        aria_label: "Search films, series and anime",
                        autocomplete: "off",
                        spellcheck: "false",
                        value: "{q}",
                        onmounted: move |e| async move { let _ = e.set_focus(true).await; },
                        oninput: move |e| set_query(e.value(), DEBOUNCE_MS),
                        onkeydown: move |e| if e.key() == Key::Enter { remember(); run(query(), 0); },
                    }
                    if !q.is_empty() {
                        icons::IconButton { label: "Clear", class: "small", onclick: move |_| set_query(String::new(), 0), icons::Backspace {} }
                    }
                }
                crate::ShortcutButton {}
                icons::IconButton { label: "Close (Esc)", class: "glass", onclick: move |_| close(), icons::Close {} }
            }
            p { class: "visually-hidden", aria_live: "polite", "{status}" }
            div { class: "search-body",
                if q.trim().is_empty() {
                    if !history.queries.is_empty() {
                        div { class: "tokens",
                            for past in history.queries.iter().take(8).cloned() {
                                button { class: "token glass", onclick: move |_| set_query(past.clone(), 0), icons::History {} span { "{past}" } }
                            }
                            icons::IconButton { label: "Clear history", class: "small", onclick: move |_| backend::clear_history(&mut shell.history.write()), icons::Trash {} }
                        }
                    }
                    if !history.items.is_empty() {
                        div { class: "group-head", title: "Opened from search", span { class: "group-icon", icons::History {} } }
                        div { class: "strip",
                            for m in history.items.iter().take(10).cloned() {
                                PosterButton { key: "{m.key()}", media: m.clone(), on_press: move |m| open(m) }
                            }
                        }
                    }
                    if let Some(list) = ideas().map(|l| l.into_iter().filter(|m| !history.items.iter().any(|h| h.key() == m.key())).collect::<Vec<_>>()).filter(|l| !l.is_empty()) {
                        div { class: "group-head", title: "From your picks", span { class: "group-icon lamp", icons::Sparkles {} } }
                        div { class: "strip",
                            for m in list.into_iter().take(10) {
                                PosterButton { key: "{m.key()}", media: m.clone(), on_press: move |m| open(m) }
                            }
                        }
                    }
                } else {
                    if let Some((addon, media)) = top {
                        TopMatch { media: media.clone(), source: several_addons.then_some(addon), on_open: move |m| open(m) }
                    }
                    for (kind, list) in groups {
                        div { class: "group-head", title: "{backend::group_label(&kind)}",
                            span { class: "group-icon",
                                match kind.as_str() {
                                    "movie" => rsx! { icons::Film {} },
                                    "series" => rsx! { icons::Tv {} },
                                    "anime" => rsx! { icons::Blossom {} },
                                    _ => rsx! { icons::Grid {} },
                                }
                            }
                            span { class: "group-name", "{backend::group_label(&kind)}" }
                            span { class: "tnum dim", "{list.len()}" }
                        }
                        div { class: "grid",
                            for m in list {
                                PosterButton { key: "{m.key()}", media: m.clone(), on_press: move |m| open(m) }
                            }
                        }
                    }
                    if !failed.is_empty() || error().is_some() {
                        div { class: "notice glass", role: "alert",
                            span { class: "warn-icon", icons::Alert {} }
                            span { if error().is_some() { "Search unavailable" } else { "{failed.join(\", \")} offline" } }
                            icons::IconButton { label: "Try again", class: "small", onclick: move |_| run(query(), 0), icons::Refresh {} }
                        }
                    }
                    if !searching() && outcome.read().is_some() && results.is_empty() && failed.is_empty() {
                        div { class: "center-note",
                            div { class: "glass panel",
                                span { class: "dim big-icon", icons::Search {} }
                                p { if catalogs == 0 { "No add-on can search" } else { "No results" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TopMatch(media: Media, source: Option<String>, on_open: EventHandler<Media>) -> Element {
    let m = media.clone();
    let artwork = backend::posters(&media);
    let mut index = use_signal(|| 0usize);
    let art = artwork.get(index()).cloned();
    let meta = [media.meta_line(true), source.unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    rsx! {
        button { class: "top-match glass", aria_label: "Open {media.name}", onclick: move |_| on_open.call(m.clone()),
            if !media.background.is_empty() { div { class: "top-art", img { src: "{media.background}", alt: "" } } }
            div { class: "frame top-poster",
                if let Some(src) = art { img { src: "{src}", alt: "", onerror: move |_| index += 1 } }
            }
            div { class: "top-copy",
                span { class: "lamp best", title: "Best match", aria_label: "Best match", icons::Star {} }
                span { class: "display top-title", "{media.name}" }
                span { class: "meta", "{meta}" }
                if !media.description.is_empty() { span { class: "synopsis clamp-2", "{media.description}" } }
            }
        }
    }
}
