//! Pieces more than one screen uses: images that fall back through a title's artwork, posters,
//! TMDB artwork for a title, critics' scores with their sources' logos, and the confirm-first
//! "Start over".
use crate::{backend::{self, Media}, icons, Shell};
use dioxus::prelude::*;

/// The first of `sources` that loads, each broken image giving way to the next; `children` when
/// none does. Key it (or its parent) by the title where the title can change, so a new title
/// starts again from its first image.
#[component]
pub fn FallbackImg(sources: Vec<String>, #[props(default)] lazy: bool, children: Element) -> Element {
    let mut index = use_signal(|| 0usize);
    match sources.get(index()) {
        Some(src) => rsx! { img { src: "{src}", alt: "", loading: if lazy { "lazy" } else { "eager" }, onerror: move |_| index += 1 } },
        None => children,
    }
}

/// Upright poster with the name under it (PosterCard on touch screens). A broken
/// image falls back to the other artwork, then to the initial: never an empty card.
#[component]
pub fn PosterCard(media: Media) -> Element {
    let mut shell = use_context::<Shell>();
    let fraction = backend::progress_fraction(&media);
    let done = media.watched;
    let m = media.clone();
    rsx! {
        button { class: "card", aria_label: "Open {media.name}", onclick: move |_| shell.opened.set(Some(m.clone())),
            div { class: "card-art tall",
                // The poster carries the title; only when no image works is the name written.
                FallbackImg { sources: backend::posters(&media), lazy: true, span { class: "initial display", "{media.name}" } }
                if done { span { class: "watched", aria_label: "Watched", icons::Check {} } }
                if fraction > 0.0 && !done { div { class: "card-progress", div { style: "width: {fraction * 100.0}%" } } }
            }
        }
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

/// The title page's artwork: use_artwork's, and without a TMDB key the keyless credits, studios
/// and season posters (backend::open_production) laid over it as they arrive. Each half shows
/// as soon as it comes; neither waits for the other.
pub fn use_title_artwork(media: &Media) -> Memo<Option<backend::Artwork>> {
    let art = use_artwork(media);
    let mut extra = use_signal(|| None::<backend::Artwork>);
    let m = media.clone();
    use_hook(move || { spawn(async move { if let Some(x) = backend::open_production(&m).await { extra.set(Some(x)); } }); });
    use_memo(move || match (art(), extra()) {
        (base, Some(x)) => Some(backend::with_extras(base.unwrap_or_default(), x)),
        (base, None) => base,
    })
}

fn svg_data(svg: &str) -> String {
    use base64::Engine;
    format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg))
}
// The scores' own logos (assets/ratings/NOTICE.md says where they come from).
static IMDB_LOGO: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| svg_data(include_str!("../assets/ratings/imdb.svg")));
static METACRITIC_LOGO: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| svg_data(include_str!("../assets/ratings/metacritic.svg")));
static FRESH_LOGO: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| svg_data(include_str!("../assets/ratings/rotten-tomatoes-fresh.svg")));
static ROTTEN_LOGO: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| svg_data(include_str!("../assets/ratings/rotten-tomatoes-rotten.svg")));

/// A title's scores in one fact, each beside its source's logo: IMDb's (from the add-on,
/// `imdb`), and where Wikidata has them Rotten Tomatoes' Tomatometer (the tomato from 60%, the
/// splat below, as on their site) and the Metascore. Key it by the title.
#[component]
pub fn Scores(media: Media, imdb: String) -> Element {
    let mut scores = use_signal(backend::Scores::default);
    let m = media.clone();
    use_hook(move || { spawn(async move { scores.set(backend::review_scores(&m).await); }); });
    let s = scores();
    let mut said = Vec::new();
    if !imdb.is_empty() { said.push(format!("IMDb {imdb}")); }
    if let Some(t) = s.tomatometer { said.push(format!("Rotten Tomatoes {t}%")); }
    if let Some(n) = s.metascore { said.push(format!("Metascore {n}")); }
    if said.is_empty() { return rsx! {}; }
    let label = said.join(", ");
    rsx! {
        span { class: "fact scores", title: "{label}", aria_label: "{label}",
            if !imdb.is_empty() { span { class: "score", img { class: "score-logo imdb", src: IMDB_LOGO.as_str(), alt: "" } span { class: "tnum", "{imdb}" } } }
            if let Some(t) = s.tomatometer {
                span { class: "score", img { class: "score-logo", src: if t >= 60 { FRESH_LOGO.as_str() } else { ROTTEN_LOGO.as_str() }, alt: "" } span { class: "tnum", "{t}%" } }
            }
            if let Some(n) = s.metascore { span { class: "score", img { class: "score-logo", src: METACRITIC_LOGO.as_str(), alt: "" } span { class: "tnum", "{n}" } } }
        }
    }
}

/// "Start over" forgets everything Picks learned and can't be undone, so the first press only
/// asks. `labelled` is the Settings row's worded button; elsewhere an icon.
#[component]
pub fn StartOver(on_reset: EventHandler<()>, #[props(default)] labelled: bool) -> Element {
    let mut armed = use_signal(|| false);
    rsx! {
        if armed() {
            span { class: "confirm", role: "group", aria_label: "Forget what Picks learned?",
                button { class: "pill forget-confirm", onclick: move |_| { armed.set(false); on_reset.call(()) }, icons::Trash {} span { "Forget taste" } }
                icons::IconButton { label: "Keep it", class: "glass small", onclick: move |_| armed.set(false), icons::Close {} }
            }
        } else if labelled {
            button { class: "pill glass", onclick: move |_| armed.set(true), icons::Undo {} span { "Start over" } }
        } else {
            icons::IconButton { label: "Start over: forget what Picks learned", class: "glass small", onclick: move |_| armed.set(true), icons::Trash {} }
        }
    }
}

/// A poster with the name under it; used by Search.
#[component]
pub fn PosterButton(media: Media, on_press: EventHandler<Media>) -> Element {
    let m = media.clone();
    rsx! {
        button { class: "poster", aria_label: "{media.name}", onclick: move |_| on_press.call(m.clone()),
            div { class: "frame",
                FallbackImg { sources: backend::posters(&media), lazy: true, span { class: "frame-name", "{media.name}" } }
            }
            span { class: "poster-meta", "{media.meta_line(false)}" }
        }
    }
}
