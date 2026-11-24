//! Home: each catalog as a quiet poster shelf. What you are watching lives in the Library, and
//! the last thing you played waits top right (main.rs, LastPlayed).
use crate::{backend::{self, CatalogRow, Media}, components::{use_artwork, PosterCard}, icons, player::PlayRequest, Shell};
use dioxus::prelude::*;

#[derive(Clone, Debug, PartialEq)]
enum Shelf { Loading, Ready(Vec<Media>), Failed(String) }

#[derive(Clone, Default)]
struct HomeSnapshot {
    rows: Vec<(CatalogRow, Shelf)>,
    library: Vec<Media>,
    tonight: Vec<Media>,
    at: i64,
}

static HOME_SNAPSHOT: std::sync::OnceLock<std::sync::Mutex<HomeSnapshot>> = std::sync::OnceLock::new();
const HOME_TTL_SECS: i64 = 300;

/// Drop cached Home shelves (called when the user switches BetterPosters/add-on settings).
pub fn invalidate_cache() {
    if let Some(m) = HOME_SNAPSHOT.get() {
        if let Ok(mut s) = m.lock() { *s = HomeSnapshot::default(); }
    }
}

fn read_home_snapshot() -> Option<HomeSnapshot> {
    let snap = HOME_SNAPSHOT.get()?.lock().ok()?.clone();
    (!snap.rows.is_empty()).then_some(snap)
}

fn write_home_snapshot(update: impl FnOnce(&mut HomeSnapshot)) {
    if let Ok(mut s) = HOME_SNAPSHOT.get_or_init(Default::default).lock() {
        update(&mut s);
    }
}

#[component]
pub fn Home() -> Element {
    let shell = use_context::<Shell>();
    let snap = read_home_snapshot();
    let fresh_rows = snap.as_ref().is_some_and(|s| backend::now_secs() - s.at < HOME_TTL_SECS
        && s.rows.iter().all(|(_, st)| matches!(st, Shelf::Ready(_))));
    let init_rows = snap.as_ref().map(|s| s.rows.clone()).unwrap_or_default();
    let init_lib = snap.as_ref().map(|s| s.library.clone()).unwrap_or_default();
    let init_tonight = snap.as_ref().map(|s| s.tonight.clone()).unwrap_or_default();
    let mut rows = use_signal(move || init_rows);
    let mut rows_error = use_signal(|| None::<String>);
    let mut library = use_signal(move || init_lib);
    let mut loading = use_signal(move || snap.is_none());
    // Tonight's pick and the next few behind it; "Not tonight" steps through them.
    let mut tonight = use_signal(move || init_tonight);
    let mut skipped = use_signal(|| 0usize);

    let load_row = move |row: CatalogRow| {
        spawn(async move {
            let result = backend::catalog(row.clone()).await;
            let next_state = match result { Ok(items) => Shelf::Ready(items), Err(e) => Shelf::Failed(format!("{e:#}")) };
            rows.with_mut(|all| if let Some(slot) = all.iter_mut().find(|(r, _)| r.key == row.key) {
                slot.1 = next_state.clone();
            });
            write_home_snapshot(|s| {
                if let Some(slot) = s.rows.iter_mut().find(|(r, _)| r.key == row.key) {
                    slot.1 = next_state;
                }
                s.at = backend::now_secs();
            });
        });
    };
    let mut opened = shell.opened;
    use_future(move || async move {
        // Always refresh library state silently (Continue Watching / New for your shows);
        // skip refetching catalog shelves & Tonight's pick when our session snapshot is fresh.
        if fresh_rows {
            if let Ok(items) = backend::library().await {
                library.set(items.clone());
                write_home_snapshot(|s| s.library = items);
            }
            loading.set(false);
            return;
        }
        // Fetch library, catalog shelf definitions, and Picks recommendations concurrently.
        let (lib_res, rows_res, picks_res) = tokio::join!(backend::library(), backend::home_rows(), backend::picks());
        if let Ok(items) = lib_res {
            // Developer option: VEYLO_DX_OPEN=<title> opens the first library title containing it.
            if let Ok(want) = std::env::var("VEYLO_DX_OPEN") {
                let want = want.to_lowercase();
                if let Some(m) = items.iter().find(|m| m.name.to_lowercase().contains(&want)) { opened.set(Some(m.clone())); }
            }
            library.set(items.clone());
            write_home_snapshot(|s| s.library = items);
        }
        match rows_res {
            Ok(list) => {
                // Keep any already-loaded shelf items visible while refreshing so nothing blinks.
                let prev: std::collections::HashMap<String, Shelf> = rows.peek().iter().cloned().map(|(r, st)| (r.key, st)).collect();
                let next: Vec<(CatalogRow, Shelf)> = list.iter().cloned().map(|r| {
                    let st = prev.get(&r.key).cloned().unwrap_or(Shelf::Loading);
                    (r, st)
                }).collect();
                rows.set(next.clone());
                write_home_snapshot(|s| { s.rows = next; s.at = backend::now_secs(); });
                for row in list { load_row(row); }
            }
            Err(e) => if rows.peek().is_empty() { rows_error.set(Some(format!("{e:#}"))); },
        }
        // Tonight's pick: recommendations you have neither watched nor started, in the order of
        // the taste Picks learned (brain.rs), with its reason when it has one.
        if let Ok(p) = picks_res {
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
            let chosen: Vec<Media> = ranked.unwrap_or(order).into_iter().take(6).collect();
            tonight.set(chosen.clone());
            write_home_snapshot(|s| s.tonight = chosen);
        }
        loading.set(false);
    });

    let continuing: Vec<Media> = library.read().iter().filter(|m| backend::in_progress(m)).cloned().collect();
    let disliked: std::collections::HashSet<String> = library.read().iter().filter(|m| m.liked < 0).map(|m| m.id.clone()).collect();
    // Tracked shows from the user's library that have a newly aired episode in TVmaze's schedule.
    let mut new_for_you = use_signal(Vec::<Media>::new);
    use_effect(move || {
        let lib = library.read();
        let tracked: std::collections::HashSet<String> = lib.iter()
            .filter(|m| m.kind == "series" && (m.saved || backend::in_progress(m) || m.position_ms > 0))
            .map(|m| m.id.clone()).collect();
        if tracked.is_empty() { return; }
        let matched: Vec<Media> = rows.read().iter()
            .find(|(r, _)| r.id == "new-episodes")
            .and_then(|(_, st)| match st { Shelf::Ready(items) => Some(items), _ => None })
            .map(|items| items.iter().filter(|m| tracked.contains(&m.id)).cloned().collect())
            .unwrap_or_default();
        if *new_for_you.peek() != matched { new_for_you.set(matched); }
    });
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

                if !new_for_you.read().is_empty() {
                    section { class: "shelf",
                        SectionHeader { title: "New for your shows", meta: Some("Tracked series with newly aired episodes".into()) }
                        div { class: "strip poster-strip",
                            for m in new_for_you.read().iter() { PosterCard { key: "nfy-{m.key()}", media: m.clone() } }
                        }
                    }
                }

                if loading() && rows.read().is_empty() { SkeletonStrip {} }
                if let Some(error) = rows_error() {
                    section { class: "shelf",
                        div { class: "row-error", span { class: "warn-icon", icons::Alert {} } span { "{error}" } }
                    }
                }
                // Read in place: the shelves (every title's metadata) aren't copied for each render.
                for (row, state) in rows.read().iter() {
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
                                    for m in items.iter().filter(|m| !disliked.contains(&m.id)).take(shelf_len()) { PosterCard { key: "{m.key()}", media: m.clone() } }
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

/// How many posters a shelf shows. Bounding each shelf keeps the Home DOM lean (~300 cards max
/// instead of 1,500+), cutting memory and scroll layout cost; Search finds anything deeper.
fn shelf_len() -> usize {
    if crate::is_tv() { 36 } else { 40 }
}

/// One title Picks chose for you: its backdrop and logo, and why. The card opens it; the button
/// goes on to Picks.
#[component]
fn TonightsPick(media: Media, also: Vec<Media>, can_skip: bool, on_skip: EventHandler<()>) -> Element {
    let mut shell = use_context::<Shell>();
    let art = use_artwork(&media);
    let mut bg_sources = Vec::new();
    if let Some(b) = art().and_then(|a| a.backdrop) { if !b.is_empty() { bg_sources.push(b); } }
    let fallback_bg = media.backdrop().to_owned();
    if !fallback_bg.is_empty() && !bg_sources.contains(&fallback_bg) { bg_sources.push(fallback_bg); }
    if !media.poster.is_empty() && !bg_sources.contains(&media.poster) { bg_sources.push(media.poster.clone()); }
    let logo = art().and_then(|a| a.logo).or_else(|| media.raw["logo"].as_str().map(str::to_owned)).filter(|l| !l.is_empty());
    let mut logo_failed = use_signal(|| false);
    // Play here starts at once: its sources are asked for while the pick is on screen.
    let ahead = media.clone();
    use_hook(move || crate::title::prefetch_resume(&ahead));
    // The film's own line when it has one, else the start of its synopsis.
    let blurb = art().and_then(|a| a.tagline).filter(|t| !t.is_empty()).unwrap_or_else(|| media.description.clone());
    let runtime = backend::s(&media.raw, "runtime");
    let rating = ["imdbRating", "tmdbRating", "tvmazeRating"].iter()
        .map(|k| backend::s(&media.raw, k)).find(|r| !r.is_empty()).unwrap_or_default();
    let facts: Vec<String> = [media.year.clone(), media.genres.first().cloned().unwrap_or_default(), runtime]
        .into_iter().filter(|s| !s.is_empty()).collect();
    let (open, open_art, play, pre1, pre2) = (media.clone(), media.clone(), media.clone(), media.clone(), media.clone());
    rsx! {
        div { class: "tonight glass",
            button { class: "tonight-art card-art wide", aria_label: "Open {media.name}",
                onpointerdown: move |_| backend::prefetch_title(&pre1),
                onclick: move |_| shell.opened.set(Some(open_art.clone())),
                crate::components::FallbackImg { sources: bg_sources }
            }
            div { class: "tonight-copy",
                if !media.reason.is_empty() { span { class: "tonight-why", icons::Sparkles {} span { "{media.reason}" } } }
                button { class: "bare tonight-title", aria_label: "Open {media.name}",
                    onpointerdown: move |_| backend::prefetch_title(&pre2),
                    onclick: move |_| shell.opened.set(Some(open.clone())),
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
                    button { class: "orb glass", aria_label: "Not for me", title: "Not for me",
                        onclick: { let m = media.clone(); move |_| {
                            let m = m.clone();
                            spawn(async move {
                                if backend::feedback(&m, -1).await.is_ok() { *shell.picks_dirty.write() += 1; }
                            });
                            if can_skip { on_skip.call(()); }
                        } },
                        icons::ThumbDown {}
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
    let m = media.clone();
    let pre = media.clone();
    rsx! {
        button { class: "card-art tonight-alt", aria_label: "Open {media.name}", title: "{media.name}",
            onpointerdown: move |_| backend::prefetch_title(&pre),
            onclick: move |_| shell.opened.set(Some(m.clone())),
            crate::components::FallbackImg { sources: backend::posters(&media), lazy: true, span { class: "initial", "{media.name}" } }
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

