//! Home, ported from HomeScreen.kt: the featured title's marquee, Continue
//! watching, then each add-on catalog as a quiet poster shelf.
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
    let mut picks = use_signal(Vec::<Media>::new);
    let mut loading = use_signal(|| true);

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
        if let Ok(p) = backend::picks().await { picks.set(p.items); }
        loading.set(false);
    });

    let continuing: Vec<Media> = library.read().iter().filter(|m| backend::in_progress(m)).cloned().collect();
    // What you are watching, else a pick for you, else the first catalog title (featuredTitle in Kotlin).
    let featured = continuing.first().cloned()
        .or_else(|| picks.read().first().cloned())
        .or_else(|| rows.read().iter().find_map(|(_, s)| match s { Shelf::Ready(items) => items.first().cloned(), _ => None }));
    let featured_resumes = featured.as_ref().is_some_and(|f| continuing.first().is_some_and(|c| c.key() == f.key()));

    let mut lamp = shell.lamp_source;
    // The marquee lends the accent from the artwork it shows; with none, the lamp goes neutral.
    let none_featured = featured.is_none();
    use_effect(use_reactive!(|none_featured| if none_featured && !lamp.peek().is_empty() { lamp.set(String::new()); }));

    rsx! {
        div { class: "home",
            if let Some(media) = featured.clone() {
                for media in [media] { Marquee { key: "{media.key()}", media, resumes: featured_resumes } }
            } else {
                div { class: "bar-clearance" }
            }
            div { class: "shelves",
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

#[component]
fn SectionHeader(title: String, #[props(default)] meta: Option<String>) -> Element {
    rsx! {
        div { class: "section-header",
            h2 { class: "display section", "{title}" }
            if let Some(m) = meta { p { class: "micro dim", "{m}" } }
        }
    }
}

/// The one bold moment of the app: the featured backdrop edge to edge, its logo
/// (or its name in Archivo) bottom-left with what is needed to carry on.
#[component]
fn Marquee(media: Media, resumes: bool) -> Element {
    let mut shell = use_context::<Shell>();
    let art = use_artwork(&media);
    let logo = art().and_then(|a| a.logo).unwrap_or_else(|| media.raw["logo"].as_str().unwrap_or_default().to_owned());
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    // The accent comes from the very image on screen (TMDB's once it arrives).
    let mut lamp = shell.lamp_source;
    use_effect(use_reactive!(|backdrop| if *lamp.peek() != backdrop { lamp.set(backdrop); }));
    let mut logo_failed = use_signal(|| false);
    let context = if resumes { backend::resume_context(&media) } else {
        [media.year.clone(), backend::type_label(&media.kind).to_lowercase()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")
    };
    let fraction = backend::progress_fraction(&media);
    let open = media.clone();
    let resume = media.clone();
    rsx! {
        section { class: "marquee", aria_label: "Featured",
            if !backdrop.is_empty() { img { class: "marquee-art", src: "{backdrop}", alt: "" } }
            div { class: "marquee-copy",
                if !logo.is_empty() && !logo_failed() {
                    img { class: "marquee-logo", src: "{logo}", alt: "{media.name}", onerror: move |_| logo_failed.set(true) }
                } else {
                    h1 { class: "display marquee-title", "{media.name}" }
                }
                if !context.is_empty() { p { class: "marquee-meta", "{context}" } }
                if resumes && media.duration_ms > 0 {
                    div { class: "progress-line", role: "progressbar", aria_valuenow: "{(fraction * 100.0).round()}", aria_valuemin: "0", aria_valuemax: "100",
                        div { style: "width: {fraction * 100.0}%" } }
                }
                div { class: "actions",
                    if resumes {
                        // Resume plays at once (the automatic choice); Details opens the page.
                        button { class: "pill accent", onclick: move |_| crate::title::resume_now(shell, resume.clone()), icons::Play {} span { "Resume" } }
                        icons::IconButton { label: "Details", class: "glass", onclick: move |_| shell.opened.set(Some(open.clone())), icons::Info {} }
                    } else {
                        button { class: "pill accent", onclick: move |_| shell.opened.set(Some(open.clone())), icons::Info {} span { "Details" } }
                    }
                }
            }
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
                    None => rsx! { span { class: "initial display", "{media.name.chars().next().unwrap_or(' ').to_uppercase()}" } },
                }
                if done { span { class: "watched", aria_label: "Watched", icons::Check {} } }
                if fraction > 0.0 && !done { div { class: "card-progress", div { style: "width: {fraction * 100.0}%" } } }
            }
            span { class: "card-title", "{media.name}" }
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
