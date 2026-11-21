//! Home: each catalog as a quiet poster shelf. What you are watching lives in the Library, and
//! the last thing you played waits top right (main.rs, LastPlayed).
use crate::{backend::{self, CatalogRow, Media}, icons, player::PlayRequest, Shell};
use dioxus::prelude::*;

#[derive(Clone, Debug, PartialEq)]
enum Shelf { Loading, Ready(Vec<Media>), Failed(String) }

#[component]
pub fn Home() -> Element {
    let shell = use_context::<Shell>();
    let mut rows = use_signal(Vec::<(CatalogRow, Shelf)>::new);
    let mut rows_error = use_signal(|| None::<String>);
    let mut library = use_signal(Vec::<Media>::new);
    let mut loading = use_signal(|| true);
    // Tonight's pick and the next few behind it; "Not tonight" steps through them.
    let mut tonight = use_signal(Vec::<Media>::new);
    let mut skipped = use_signal(|| 0usize);

    let load_row = move |row: CatalogRow| {
        spawn(async move {
            let result = backend::catalog(row.clone()).await;
            rows.with_mut(|all| if let Some(slot) = all.iter_mut().find(|(r, _)| r.key == row.key) {
                slot.1 = match result { Ok(items) => Shelf::Ready(items), Err(e) => Shelf::Failed(format!("{e:#}")) };
            });
        });
    };
    let mut opened = shell.opened;
    use_future(move || async move {
        if let Ok(items) = backend::library().await {
            // Developer option: VEYLO_DX_OPEN=<title> opens the first library title containing it.
            if let Ok(want) = std::env::var("VEYLO_DX_OPEN") {
                let want = want.to_lowercase();
                if let Some(m) = items.iter().find(|m| m.name.to_lowercase().contains(&want)) { opened.set(Some(m.clone())); }
            }
            library.set(items);
        }
        match backend::home_rows().await {
            Ok(list) => {
                rows.set(list.iter().cloned().map(|r| (r, Shelf::Loading)).collect());
                for row in list { load_row(row); }
            }
            Err(e) => rows_error.set(Some(format!("{e:#}"))),
        }
        // Tonight's pick: recommendations you have neither watched nor started, in the order of
        // the taste Picks learned (brain.rs), with its reason when it has one.
        if let Ok(p) = backend::picks().await {
            let fresh: Vec<Media> = p.items.into_iter().filter(|m| !m.watched && !backend::in_progress(m)).take(24).collect();
            let known = library.peek().clone();
            let order = fresh.clone();
            let ranked = backend::on_runtime(async move {
                tokio::task::spawn_blocking(move || {
                    let mut taste = crate::brain::load(0);
                    taste.set_facts(crate::brain::facts(&known));
                    // The core's order breaks near-ties: it knows the catalogues' own ranking.
                    let mut scored: Vec<(f64, Media)> = fresh.into_iter().enumerate().map(|(i, mut m)| {
                        if let Some(r) = taste.home_reason(&m) { m.reason = r; }
                        (taste.lasting(&m) - i as f64 * 0.02, m)
                    }).collect();
                    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                    scored.into_iter().map(|(_, m)| m).collect::<Vec<_>>()
                }).await.ok()
            }).await.ok().flatten();
            tonight.set(ranked.unwrap_or(order).into_iter().take(6).collect());
        }
        loading.set(false);
    });

    let continuing: Vec<Media> = library.read().iter().filter(|m| backend::in_progress(m)).cloned().collect();
    // The room takes its light from what you are watching.
    let mut lamp = shell.lamp_source;
    let queue: Vec<Media> = { let all = tonight.read(); if all.is_empty() { Vec::new() } else { (0..all.len()).map(|i| all[(skipped() + i) % all.len()].clone()).collect() } };
    let room = continuing.first().or(queue.first()).map(|m| m.backdrop().to_owned()).unwrap_or_default();
    use_effect(use_reactive!(|room| if *lamp.peek() != room { lamp.set(room); }));

    rsx! {
        div { class: "home",
            div { class: "bar-clearance" }
            div { class: "shelves",
                if let Some(m) = queue.first().cloned() {
                    section { class: "shelf",
                        SectionHeader { title: "Tonight's pick" }
                        for m in [m] {
                            TonightsPick { key: "{m.key()}", media: m.clone(), also: queue.iter().skip(1).take(3).cloned().collect::<Vec<_>>(),
                                can_skip: queue.len() > 1, on_skip: move |_| skipped += 1 }
                        }
                    }
                }

                if loading() && rows.read().is_empty() { SkeletonStrip {} }
                if let Some(error) = rows_error() {
                    section { class: "shelf",
                        div { class: "row-error", span { class: "warn-icon", icons::Alert {} } span { "{error}" } }
                    }
                }
                for (row, state) in rows() {
                    section { class: "shelf", key: "{row.key}",
                        SectionHeader { title: row.title.clone(), meta: row.subtitle.clone() }
                        match state {
                            Shelf::Loading => rsx! { SkeletonStrip {} },
                            Shelf::Failed(error) => rsx! {
                                div { class: "row-error",
                                    span { class: "warn-icon", icons::Alert {} }
                                    span { "{error}" }
                                    icons::IconButton { label: "Try again", class: "glass small", onclick: { let r = row.clone(); move |_| {
                                        rows.with_mut(|all| if let Some(s) = all.iter_mut().find(|(x, _)| x.key == r.key) { s.1 = Shelf::Loading; });
                                        load_row(r.clone());
                                    } }, icons::Refresh {} }
                                }
                            },
                            Shelf::Ready(items) if items.is_empty() => rsx! { p { class: "meta dim", "Empty" } },
                            Shelf::Ready(items) => rsx! {
                                div { class: "strip poster-strip",
                                    for m in items { PosterCard { key: "{m.key()}", media: m.clone() } }
                                }
                            },
                        }
                    }
                }
                DemoNote {}
            }
        }
    }
}

/// One title Picks chose for you: its backdrop and logo, and why. The card opens it; the button
/// goes on to Picks.
#[component]
fn TonightsPick(media: Media, also: Vec<Media>, can_skip: bool, on_skip: EventHandler<()>) -> Element {
    let mut shell = use_context::<Shell>();
    let art = use_artwork(&media);
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    let logo = art().and_then(|a| a.logo).or_else(|| media.raw["logo"].as_str().map(str::to_owned)).filter(|l| !l.is_empty());
    let mut logo_failed = use_signal(|| false);
    // The film's own line when it has one, else the start of its synopsis.
    let blurb = art().and_then(|a| a.tagline).filter(|t| !t.is_empty()).unwrap_or_else(|| media.description.clone());
    let runtime = backend::s(&media.raw, "runtime");
    let rating = backend::s(&media.raw, "imdbRating");
    let facts: Vec<String> = [media.year.clone(), media.genres.first().cloned().unwrap_or_default(), runtime]
        .into_iter().filter(|s| !s.is_empty()).collect();
    let (open, open_art, play) = (media.clone(), media.clone(), media.clone());
    rsx! {
        div { class: "tonight glass",
            button { class: "tonight-art card-art wide", aria_label: "Open {media.name}", onclick: move |_| shell.opened.set(Some(open_art.clone())),
                if !backdrop.is_empty() { img { src: "{backdrop}", alt: "" } }
            }
            div { class: "tonight-copy",
                if !media.reason.is_empty() { span { class: "tonight-why", icons::Sparkles {} span { "{media.reason}" } } }
                button { class: "bare tonight-title", aria_label: "Open {media.name}", onclick: move |_| shell.opened.set(Some(open.clone())),
                    match logo.filter(|_| !logo_failed()) {
                        Some(src) => rsx! { img { class: "tonight-logo", src: "{src}", alt: "{media.name}", onerror: move |_| logo_failed.set(true) } },
                        None => rsx! { span { class: "display tonight-name", "{media.name}" } },
                    }
                }
                span { class: "tonight-facts",
                    for f in facts { span { "{f}" } }
                    if !rating.is_empty() { span { class: "tonight-rating", icons::Star {} "{rating}" } }
                }
                if !blurb.is_empty() { p { class: "tonight-blurb", "{blurb}" } }
                div { class: "tonight-actions",
                    button { class: "pill tonight-play", onclick: move |_| crate::title::resume_now(shell, play.clone()), icons::Play {} span { "Play" } }
                    if can_skip {
                        button { class: "orb glass", aria_label: "Not tonight", title: "Not tonight", onclick: move |_| on_skip.call(()), icons::Refresh {} }
                    }
                    button { class: "orb glass", aria_label: "More picks", title: "More picks", onclick: move |_| { shell.opened.set(None); shell.tab.set(crate::Tab::Picks); }, icons::ArrowRight {} }
                }
            }
            if !also.is_empty() {
                div { class: "tonight-also",
                    span { class: "micro dim", "Or" }
                    div { class: "tonight-also-row",
                        for m in also {
                            AlsoTonight { key: "{m.key()}", media: m.clone() }
                        }
                    }
                }
            }
        }
    }
}

/// A small poster behind Tonight's pick. The poster carries the title; no name under it.
#[component]
fn AlsoTonight(media: Media) -> Element {
    let mut shell = use_context::<Shell>();
    let src = backend::posters(&media).into_iter().next().unwrap_or_default();
    let m = media.clone();
    rsx! {
        button { class: "card-art tonight-alt", aria_label: "Open {media.name}", title: "{media.name}", onclick: move |_| shell.opened.set(Some(m.clone())),
            if !src.is_empty() { img { src: "{src}", alt: "" } } else { span { class: "initial", "{media.name}" } }
        }
    }
}

#[component]
fn SectionHeader(title: String, #[props(default)] meta: Option<String>) -> Element {
    rsx! {
        div { class: "section-header",
            h2 { class: "display section", "{title}" }
            if let Some(m) = meta { p { class: "micro dim", "{m}" } }
        }
    }
}


/// Upright poster with the name under it (PosterCard on touch screens). A broken
/// image falls back to the other artwork, then to the initial: never an empty card.
#[component]
pub fn PosterCard(media: Media) -> Element {
    let mut shell = use_context::<Shell>();
    let artwork = backend::posters(&media);
    let mut index = use_signal(|| 0usize);
    let art = artwork.get(index()).cloned();
    let fraction = backend::progress_fraction(&media);
    let done = if media.kind == "movie" { backend::film_watched(&media) } else { media.watched };
    let m = media.clone();
    rsx! {
        button { class: "card", aria_label: "Open {media.name}", onclick: move |_| shell.opened.set(Some(m.clone())),
            div { class: "card-art tall",
                match art {
                    Some(src) => rsx! { img { src: "{src}", alt: "", loading: "lazy", onerror: move |_| index += 1 } },
                    // The poster carries the title; only when no image works is the name written.
                    None => rsx! { span { class: "initial display", "{media.name}" } },
                }
                if done { span { class: "watched", aria_label: "Watched", icons::Check {} } }
                if fraction > 0.0 && !done { div { class: "card-progress", div { style: "width: {fraction * 100.0}%" } } }
            }
        }
    }
}

#[component]
fn SkeletonStrip() -> Element {
    rsx! {
        div { class: "strip poster-strip", aria_hidden: "true",
            for i in 0..6 { div { key: "{i}", class: "card", div { class: "card-art tall skeleton-card" } } }
        }
    }
}

/// The legal player test, as in the Android app; here it opens libmpv.
#[component]
fn DemoNote() -> Element {
    let shell = use_context::<Shell>();
    rsx! {
        div { class: "demo",
            button { class: "demo-row glass", aria_label: "Test the player with the Sintel trailer",
                onclick: move |_| crate::start_playback(shell, PlayRequest {
                    title: "Sintel trailer".into(), subtitle: "Test player".into(), url: backend::DEMO_STREAM.into(), ..Default::default() }, None),
                span { class: "lamp", icons::PlayCircle {} }
                span { class: "demo-copy",
                    span { class: "body-strong", "Test player" }
                    // Attribution is a licence requirement, not decoration.
                    span { class: "credit dim", "Sintel, CC BY 3.0 Blender Foundation" }
                }
            }
        }
    }
}

/// A poster on its own (BetterPosters first, then the add-on's), for pages that also show
/// wide artwork: the title page, Picks. Nothing is drawn when no image works.
#[component]
pub fn PosterArt(media: Media, class: String) -> Element {
    let art = use_artwork(&media);
    let mut artwork: Vec<String> = backend::posters(&media).into_iter().filter(|u| *u != media.background).collect();
    // TMDB's poster is the last resort for titles no other source has.
    if let Some(p) = art().and_then(|a| a.poster) { artwork.push(p); }
    let mut index = use_signal(|| 0usize);
    match artwork.get(index()).cloned() {
        Some(src) => rsx! { div { class: "poster-art {class}", img { src: "{src}", alt: "{media.name} poster", onerror: move |_| index += 1 } } },
        None => rsx! {},
    }
}

/// TMDB artwork for a title (text-free backdrop, logo), loaded once; None until it arrives
/// or when there is no key, in which case the add-on's images stay.
pub fn use_artwork(media: &Media) -> Signal<Option<backend::Artwork>> {
    let mut art = use_signal(|| None::<backend::Artwork>);
    let m = media.clone();
    use_hook(move || { spawn(async move { if let Some(a) = backend::artwork(&m).await { art.set(Some(a)); } }); });
    art
}
