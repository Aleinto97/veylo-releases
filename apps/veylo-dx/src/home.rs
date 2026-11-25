//! Home: a full-bleed cinematic hero for Tonight's pick, Continue watching in widescreen,
//! and each catalog as a quiet poster shelf.
use crate::{backend::{self, Artwork, CatalogRow, Media}, components::{use_artwork, PosterCard, ShelfStrip}, icons, player::PlayRequest, Shell};
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};

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

/// Remove a forgotten title from Home's cached library list so Continue watching never flashes stale rows.
pub fn forget_from_snapshot(key: &str) {
    if let Some(m) = HOME_SNAPSHOT.get() {
        if let Ok(mut s) = m.lock() { s.library.retain(|x| x.key() != key); }
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
    let mut shell = use_context::<Shell>();
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
    // Tonight's pick and the next few behind it; the hero rail switches between them live.
    let mut tonight = use_signal(move || init_tonight);
    let mut active_idx = use_signal(|| 0usize);

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
        // Always refresh library state silently (Continue watching);
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
    // Featured picks for the full-bleed hero switcher (disliked titles drop out immediately).
    let hero_picks: Vec<Media> = tonight.read().iter().filter(|m| !disliked.contains(&m.id)).take(4).cloned().collect();
    let hero_idx = if hero_picks.is_empty() { 0 } else { active_idx() % hero_picks.len() };
    let has_hero = !hero_picks.is_empty() || (loading() && tonight.read().is_empty());
    // When there is no hero pick, fall back to lighting the room from what you are watching.
    let mut lamp = shell.lamp_source;
    let fallback_room = if hero_picks.is_empty() {
        continuing.first().map(|m| m.backdrop().to_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    use_effect(use_reactive!(|fallback_room| if !fallback_room.is_empty() && *lamp.peek() != fallback_room { lamp.set(fallback_room); }));

    rsx! {
        div { class: "home",
            if !hero_picks.is_empty() {
                HomeHero {
                    items: hero_picks,
                    active: hero_idx,
                    on_select: move |idx| active_idx.set(idx),
                    on_dislike: move |m: Media| {
                        let key = m.key();
                        tonight.with_mut(|list| list.retain(|x| x.key() != key));
                        write_home_snapshot(|s| s.tonight.retain(|x| x.key() != key));
                        spawn(async move {
                            if backend::feedback(&m, -1).await.is_ok() { *shell.picks_dirty.write() += 1; }
                        });
                    },
                }
            } else if loading() && tonight.read().is_empty() {
                section { class: "home-hero hero-skeleton", aria_hidden: "true" }
            } else {
                div { class: "bar-clearance" }
            }

            div { class: if has_hero { "shelves after-hero" } else { "shelves" },
                if !continuing.is_empty() {
                    section { class: "shelf",
                        SectionHeader { title: "Continue watching" }
                        ShelfStrip { class: "strip cw-strip".to_string(),
                            for (i, m) in continuing.iter().enumerate() {
                                ContinueCard {
                                    key: "cw-{m.key()}",
                                    media: m.clone(),
                                    ahead: i < 2,
                                    on_forget: move |gone: Media| {
                                        let key = gone.key();
                                        library.with_mut(|list| list.retain(|x| x.key() != key));
                                        forget_from_snapshot(&key);
                                        crate::library::forget_from_snapshot(&key);
                                        *shell.picks_dirty.write() += 1;
                                    },
                                }
                            }
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
                                ShelfStrip {
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

fn pick_backdrop(media: &Media, map: &HashMap<String, Artwork>, broken: &HashSet<String>) -> String {
    let mut candidates = Vec::new();
    if let Some(b) = map.get(&media.key()).and_then(|a| a.backdrop.as_ref()) {
        if !b.is_empty() { candidates.push(b.clone()); }
    }
    if !media.background.is_empty() && !candidates.contains(&media.background) {
        candidates.push(media.background.clone());
    }
    let fb = media.backdrop().to_owned();
    if !fb.is_empty() && !candidates.contains(&fb) {
        candidates.push(fb);
    }
    candidates.into_iter().find(|u| !broken.contains(u)).unwrap_or_default()
}

/// Full-bleed Home hero: widescreen artwork melting under the transparent top bar with two-slot
/// crossfade, Ken Burns settle, scroll parallax, and a live pick switcher dock.
#[component]
fn HomeHero(
    items: Vec<Media>,
    active: usize,
    on_select: EventHandler<usize>,
    on_dislike: EventHandler<Media>,
) -> Element {
    let mut shell = use_context::<Shell>();
    let idx = active.min(items.len().saturating_sub(1));
    let media = items[idx].clone();

    let mut art_map = use_signal(|| {
        let mut map = HashMap::<String, Artwork>::new();
        for m in &items {
            if let Some(a) = backend::cached_artwork(m) { map.insert(m.key(), a); }
        }
        map
    });
    // Pre-warm artwork for all switcher picks so changing hero slides is instantaneous.
    let item_keys: Vec<String> = items.iter().map(Media::key).collect();
    let items_for_art = items.clone();
    use_effect(use_reactive!(|item_keys| {
        let _ = item_keys;
        for m in items_for_art.clone() {
            let key = m.key();
            if art_map.peek().contains_key(&key) { continue; }
            if let Some(cached) = backend::cached_artwork(&m) {
                art_map.write().insert(key, cached);
                continue;
            }
            spawn(async move {
                if let Some(a) = backend::artwork(&m).await {
                    art_map.write().insert(key, a);
                }
            });
        }
    }));

    // Prefetch sources for the active pick so Play starts immediately.
    let active_key = media.key();
    let active_for_prefetch = media.clone();
    use_effect(use_reactive!(|active_key| {
        let _ = active_key;
        crate::title::prefetch_resume(&active_for_prefetch);
    }));

    // Two-slot crossfade state (same smooth zero-flash architecture as TitlePage).
    let mut seq = use_signal(|| 0usize);
    let mut top_url = use_signal(String::new);
    let mut under_url = use_signal(String::new);
    let mut top_ready = use_signal(|| false);
    let mut failed_urls = use_signal(HashSet::<String>::new);
    let mut failed_logos = use_signal(HashSet::<String>::new);
    // Brief grace window on cold start so a clean logo fades in once instead of flashing <h1> text first.
    let mut allow_text = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            backend::sleep(320).await;
            allow_text.set(true);
        });
    });

    let cur_art = art_map.read().get(&media.key()).cloned();
    let desired = pick_backdrop(&media, &art_map.read(), &failed_urls.read());
    let preload_next = if items.len() > 1 {
        pick_backdrop(&items[(idx + 1) % items.len()], &art_map.read(), &failed_urls.read())
    } else {
        String::new()
    };

    use_effect(use_reactive!(|desired| {
        if desired.is_empty() { return; }
        let prev = top_url.peek().clone();
        if prev == desired { return; }
        if !prev.is_empty() && *top_ready.peek() {
            under_url.set(prev);
        }
        top_ready.set(false);
        top_url.set(desired);
        *seq.write() += 1;
    }));

    // The room's ambient light and --lamp follow the hero pick live as you switch.
    let mut lamp = shell.lamp_source;
    let lamp_src = if !desired.is_empty() { desired.clone() } else { media.backdrop().to_owned() };
    use_effect(use_reactive!(|lamp_src| if !lamp_src.is_empty() && *lamp.peek() != lamp_src { lamp.set(lamp_src); }));

    let logo = cur_art.as_ref().and_then(|a| a.logo.clone())
        .or_else(|| media.raw["logo"].as_str().map(str::to_owned))
        .filter(|l| !l.is_empty() && !failed_logos.read().contains(l));
    let blurb = cur_art.as_ref().and_then(|a| a.tagline.clone()).filter(|t| !t.is_empty())
        .unwrap_or_else(|| media.description.clone());
    let runtime = backend::s(&media.raw, "runtime");
    let rating = ["imdbRating", "tmdbRating", "tvmazeRating"].iter()
        .map(|k| backend::s(&media.raw, k)).find(|r| !r.is_empty()).unwrap_or_default();
    let facts: Vec<String> = [media.year.clone(), media.genres.first().cloned().unwrap_or_default(), runtime]
        .into_iter().filter(|s| !s.is_empty()).collect();

    let cur_top = top_url();
    let cur_under = under_url();
    let cur_seq = seq();
    let cur_ready = top_ready();
    let is_initial = cur_under.is_empty();

    rsx! {
        section { class: "home-hero", aria_label: "Tonight's pick: {media.name}",
            div { class: "home-art-stack", aria_hidden: "true",
                if !cur_under.is_empty() {
                    for k in [format!("hu-{cur_under}")] {
                        img { key: "{k}", class: "title-art under", src: "{cur_under}", alt: "", decoding: "async" }
                    }
                }
                if !cur_top.is_empty() {
                    for k in [format!("ht-{cur_seq}")] {
                        img {
                            key: "{k}",
                            class: format!(
                                "title-art top{}{}",
                                if is_initial { " initial" } else { "" },
                                if cur_ready { " ready" } else { "" }
                            ),
                            src: "{cur_top}",
                            alt: "",
                            decoding: "async",
                            onload: move |_| {
                                let my_seq = cur_seq;
                                top_ready.set(true);
                                if !under_url.peek().is_empty() {
                                    spawn(async move {
                                        backend::sleep(1050).await;
                                        if *seq.peek() == my_seq { under_url.set(String::new()); }
                                    });
                                }
                            },
                            onerror: {
                                let failed_src = cur_top.clone();
                                move |_| { failed_urls.write().insert(failed_src.clone()); }
                            },
                        }
                    }
                }
                if !preload_next.is_empty() && preload_next != cur_top && preload_next != cur_under {
                    for k in [format!("hp-{preload_next}")] {
                        img { key: "{k}", class: "title-art preload", src: "{preload_next}", alt: "", aria_hidden: "true" }
                    }
                }
            }

            for k in [media.key()] {
                {
                    let (open_title, open_btn, play, pre1, pre2, dislike_m) = (
                        media.clone(), media.clone(), media.clone(), media.clone(), media.clone(), media.clone(),
                    );
                    rsx! {
                        div { key: "{k}", class: "home-hero-copy",
                            span { class: "hero-eyebrow glass",
                                span { class: "hero-eyebrow-icon", icons::Sparkles {} }
                                span { class: "hero-eyebrow-label", "Tonight's pick" }
                                if !media.reason.is_empty() {
                                    span { class: "hero-eyebrow-sep", "·" }
                                    span { class: "hero-eyebrow-why", "{media.reason}" }
                                }
                            }
                            button { class: "bare hero-title-btn", aria_label: "Open {media.name}",
                                onpointerdown: move |_| backend::prefetch_title(&pre1),
                                onclick: move |_| shell.opened.set(Some(open_title.clone())),
                                match logo.clone() {
                                    Some(src) => rsx! {
                                        img {
                                            class: "hero-logo",
                                            src: "{src}",
                                            alt: "{media.name}",
                                            onerror: { let u = src.clone(); move |_| { failed_logos.write().insert(u.clone()); } },
                                        }
                                    },
                                    None if cur_art.is_none() && !allow_text() => rsx! { div { class: "hero-logo", style: "height: 68px" } },
                                    None => rsx! { h1 { class: "display hero-name", "{media.name}" } },
                                }
                            }
                            if !facts.is_empty() || !rating.is_empty() {
                                span { class: "tonight-facts hero-facts",
                                    if !rating.is_empty() { span { class: "tonight-rating", icons::Star {} "{rating}" } }
                                    for f in facts.clone() { span { "{f}" } }
                                }
                            }
                            if !blurb.is_empty() { p { class: "hero-blurb", "{blurb}" } }
                            div { class: "hero-actions",
                                button { class: "pill accent play-big",
                                    onclick: move |_| crate::title::play_or_resume(shell, play.clone()),
                                    icons::Play {} span { "Play" }
                                }
                                button { class: "pill glass",
                                    onpointerdown: move |_| backend::prefetch_title(&pre2),
                                    onclick: move |_| shell.opened.set(Some(open_btn.clone())),
                                    icons::Info {} span { "Details" }
                                }
                                button { class: "orb glass", aria_label: "Not for me", title: "Not for me",
                                    onclick: move |_| on_dislike.call(dislike_m.clone()),
                                    icons::ThumbDown {}
                                }
                                button { class: "orb glass", aria_label: "More picks", title: "More picks",
                                    onclick: move |_| { shell.opened.set(None); shell.tab.set(crate::Tab::Picks); },
                                    icons::ArrowRight {}
                                }
                            }
                        }
                    }
                }
            }

            if items.len() > 1 {
                div { class: "hero-rail glass", role: "tablist", aria_label: "Featured picks",
                    for (i, item) in items.iter().enumerate() {
                        {
                            let is_on = i == idx;
                            let open_m = item.clone();
                            let pre_m = item.clone();
                            let label = if is_on { format!("Open {}", item.name) } else { format!("Preview {}", item.name) };
                            rsx! {
                                button {
                                    key: "{item.key()}",
                                    class: if is_on { "hero-thumb on" } else { "hero-thumb" },
                                    role: "tab",
                                    aria_selected: "{is_on}",
                                    aria_label: "{label}",
                                    title: "{item.name}",
                                    onpointerdown: move |_| backend::prefetch_title(&pre_m),
                                    onclick: move |_| {
                                        if is_on { shell.opened.set(Some(open_m.clone())); }
                                        else { on_select.call(i); }
                                    },
                                    crate::components::FallbackImg {
                                        sources: backend::posters(item),
                                        lazy: false,
                                        span { class: "initial display", "{item.name}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Widescreen 16:9 Continue Watching card: backdrop, episode badge, remaining time, progress bar,
/// a remove control, and a direct Resume orb.
#[component]
fn ContinueCard(media: Media, ahead: bool, on_forget: EventHandler<Media>) -> Element {
    let mut shell = use_context::<Shell>();
    let mut armed = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let art = use_artwork(&media);
    let mut bg_sources = Vec::new();
    if let Some(b) = art().and_then(|a| a.backdrop) { if !b.is_empty() { bg_sources.push(b); } }
    let fb = media.backdrop().to_owned();
    if !fb.is_empty() && !bg_sources.contains(&fb) { bg_sources.push(fb); }
    if !media.poster.is_empty() && !bg_sources.contains(&media.poster) { bg_sources.push(media.poster.clone()); }
    let fraction = backend::progress_fraction(&media);
    let ep = media.last_video_id.as_deref().and_then(backend::episode_label);
    let context = backend::resume_context(&media);
    let remaining = match &ep {
        Some(tag) => context.strip_prefix(tag.as_str()).map(|r| r.trim_start_matches(", ").to_owned()).unwrap_or(context),
        None => context,
    };
    let (open, resume, pre, for_ahead, for_forget) = (media.clone(), media.clone(), media.clone(), media.clone(), media.clone());
    use_hook(move || if ahead { crate::title::prefetch_resume(&for_ahead) });
    rsx! {
        div { class: "cw-card",
            button { class: "cw-open", aria_label: "Open {media.name}",
                onpointerdown: move |_| backend::prefetch_title(&pre),
                onclick: move |_| shell.opened.set(Some(open.clone())),
                crate::components::FallbackImg { sources: bg_sources, lazy: true }
                if let Some(tag) = ep {
                    span { class: "cw-ep tnum", "{tag}" }
                }
                div { class: "cw-copy",
                    span { class: "display cw-title", "{media.name}" }
                    if !remaining.is_empty() { span { class: "cw-meta tnum", "{remaining}" } }
                }
                if fraction > 0.0 { div { class: "card-progress cw-progress", div { style: "width: {fraction * 100.0}%" } } }
            }
            div { class: if armed() { "forget armed" } else { "forget" },
                if armed() {
                    button { class: "pill forget-confirm", disabled: busy(), aria_label: "Remove {media.name} from Continue watching",
                        onclick: move |e: Event<MouseData>| {
                            e.stop_propagation();
                            let m = for_forget.clone();
                            busy.set(true);
                            spawn(async move {
                                let ok = backend::forget(&m).await.is_ok();
                                busy.set(false);
                                armed.set(false);
                                if ok { on_forget.call(m); }
                            });
                        },
                        icons::Trash {} span { "Remove" }
                    }
                    icons::IconButton { label: "Keep", class: "glass small",
                        onclick: move |e: Event<MouseData>| { e.stop_propagation(); armed.set(false); },
                        icons::Close {}
                    }
                } else {
                    icons::IconButton { label: "Remove {media.name} from Continue watching", class: "glass small forget-btn",
                        onclick: move |e: Event<MouseData>| { e.stop_propagation(); armed.set(true); },
                        icons::Trash {}
                    }
                }
            }
            icons::IconButton {
                label: "Resume {media.name}",
                class: "glass cw-play",
                onclick: move |_| crate::title::resume_now(shell, resume.clone()),
                icons::Play {}
            }
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

