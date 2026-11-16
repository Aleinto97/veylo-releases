//! The title page: a full page, not a pop-up. A film has one Play button that picks
//! its source the way the Android app does; a series or anime lists its seasons and
//! episodes, and tapping an episode plays it the same way. Opening the page never
//! plays anything and never starts P2P.
use crate::{backend::{self, Episode, Media, Source, SourceKind, TitleMeta}, icons, player::PlayRequest, PlayerStatus, Shell};
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
enum Load<T> { Loading, Ready(T), Failed(String) }

fn episodic(media: &Media) -> bool { media.kind != "movie" }

/// The manual chooser: which video it lists.
#[derive(Clone, PartialEq)]
struct Chooser { video: String, label: Option<String> }

#[component]
pub fn TitlePage(media: Media) -> Element {
    let mut shell = use_context::<Shell>();
    let mut meta = use_signal(|| Load::<TitleMeta>::Loading);
    let mut season = use_signal(|| None::<i64>);
    let mut liked = use_signal(|| media.liked);
    let mut saved = use_signal(|| media.saved);
    // Films: one "Already watched" switch (marked, or played through). Series: per episode.
    let mut watched = use_signal(|| backend::film_watched(&media));
    let mut seen = use_signal(std::collections::HashSet::<String>::new);
    let for_seen = media.clone();
    use_hook(move || { spawn(async move { seen.set(backend::watched_episodes(&for_seen).await); }); });
    // A title opened from a shelf, Search or Picks carries the add-on's data only: what you did
    // with it (saved, watched, liked) comes from the store.
    // A choice made before the answer arrives wins (`touched`).
    let mut touched = use_signal(|| false);
    let for_store = media.clone();
    use_hook(move || { spawn(async move {
        if let Some(m) = backend::stored(&for_store).await {
            if *touched.peek() { return; }
            saved.set(m.saved);
            liked.set(m.liked);
            if !episodic(&m) { watched.set(backend::film_watched(&m)); }
        }
    }); });
    let mut chooser = use_signal(|| None::<Chooser>);
    let mut logo_failed = use_signal(|| false);
    let mut busy = use_signal(|| None::<String>);

    let for_meta = media.clone();
    use_hook(move || {
        spawn(async move {
            match backend::title_meta(&for_meta).await {
                Ok(m) => {
                    let start = for_meta.last_video_id.as_ref().and_then(|v| m.episodes.iter().find(|e| &e.id == v))
                        .or_else(|| m.episodes.iter().find(|e| e.season > 0)).or(m.episodes.first()).map(|e| e.season);
                    season.set(start);
                    meta.set(Load::Ready(m));
                }
                Err(e) => meta.set(Load::Failed(format!("{e:#}"))),
            }
        });
    });

    let episodes = move || match meta() { Load::Ready(m) => m.episodes, _ => Vec::new() };
    let label_of = move |e: &Episode| format!("S{} E{}", e.season, e.number);

    // Play: the Android app's order, then the one-byte check on at most four candidates.
    let play_media = media.clone();
    let autoplay = use_callback(move |(video, label): (String, Option<String>)| {
        let media = play_media.clone();
        if busy.peek().is_some() { return; }
        let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
        busy.set(Some(video.clone()));
        let ticket = crate::finding(shell, title.clone());
        // Season packs: a labelled size covers the whole season.
        let pack = label.as_ref().and_then(|_| {
            let list = episodes();
            let s = list.iter().find(|e| e.id == video)?.season;
            Some(list.iter().filter(|e| e.season == s).count())
        });
        let next = next_episode(&episodes(), &video);
        spawn(async move {
            let result = backend::sources(&media.kind, &video).await;
            let order = match &result { Ok((list, _)) => backend::play_order(list, pack), Err(_) => Vec::new() };
            let (best, debrid) = backend::first_working(order.clone()).await;
            busy.set(None);
            if !crate::still_wanted(shell, ticket) { return; }
            match best {
                Some(src) => {
                    let rest: Vec<Source> = order.iter().skip_while(|s| s.url != src.url).skip(1).cloned().collect();
                    start_with(shell, &media, &video, &title, src, next.map(|e| (e.id.clone(), Some(label_of(&e)))), rest, None)
                }
                None => {
                    shell.player_status.set(Some(PlayerStatus::Failed(backend::no_source_message(debrid.as_deref()).into())));
                    chooser.set(Some(Chooser { video, label }));
                }
            }
        });
    });

    let (m1, m4) = (media.clone(), media.clone());
    let rate = move |value: i64| {
        let media = m1.clone();
        touched.set(true);
        let before = liked();
        let next = if before == value { 0 } else { value };
        liked.set(next);
        spawn(async move { if backend::feedback(&media, next).await.is_ok() { *shell.picks_dirty.write() += 1; } else { liked.set(before); } });
    };
    let (mut rate_like, mut rate_hide) = (rate.clone(), rate);
    // Save to the Library (S), and the "Already watched" switch (W): the button and the key alike.
    let for_save = media.clone();
    let toggle_save = use_callback(move |_: ()| {
        touched.set(true);
        let next = !*saved.peek();
        saved.set(next);
        let m = for_save.clone();
        spawn(async move { if backend::set_saved(&m, next).await.is_err() { saved.set(!next); } });
    });
    let for_watched = media.clone();
    let toggle_watched = use_callback(move |_: ()| {
        touched.set(true);
        let next = !*watched.peek();
        watched.set(next);
        let m = for_watched.clone();
        spawn(async move { if backend::set_watched(&m, next).await.is_err() { watched.set(!next); } else { *shell.picks_dirty.write() += 1; } });
    });

    // Keys from the app root: P play/resume, L like, D not for me, S save, W already watched, T trailer, O sources.
    let (k_media, k_m2) = (media.clone(), media.clone());
    use_effect(move || {
        let Some(key) = (shell.title_command)() else { return };
        shell.title_command.set(None);
        let film = !episodic(&k_media);
        match key {
            'p' => if film { autoplay.call((k_media.id.clone(), None)) }
                   else if let Some(id) = up_next(&episodes(), &k_media, &seen.peek()) { autoplay.call((id.clone(), backend::episode_label(&id))) },
            'l' => { touched.set(true); let before = *liked.peek(); let next = if before == 1 { 0 } else { 1 }; liked.set(next); let m = k_m2.clone();
                spawn(async move { if backend::feedback(&m, next).await.is_ok() { *shell.picks_dirty.write() += 1; } else { liked.set(before); } }); }
            'd' => { touched.set(true); let before = *liked.peek(); let next = if before == -1 { 0 } else { -1 }; liked.set(next); let m = k_m2.clone();
                spawn(async move { if backend::feedback(&m, next).await.is_ok() { *shell.picks_dirty.write() += 1; } else { liked.set(before); } }); }
            's' => toggle_save.call(()),
            'w' if film => toggle_watched.call(()),
            't' => if let Load::Ready(m) = &*meta.peek() { if !m.trailers.is_empty() { crate::play_trailers(shell, m.trailers.clone()); } },
            'o' if film => chooser.set(Some(Chooser { video: k_media.id.clone(), label: None })),
            _ => {}
        }
    });
    let facts = match meta() { Load::Ready(m) => Some(m), _ => None };
    let year = media.year.clone();
    let film = !episodic(&media);
    let art = crate::home::use_artwork(&media);
    let logo = art().and_then(|a| a.logo)
        .or_else(|| facts.as_ref().map(|m| m.logo.clone()).filter(|l| !l.is_empty()))
        .or_else(|| media.raw["logo"].as_str().map(str::to_owned).filter(|l| !l.is_empty()));
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    // The accent follows the backdrop on screen.
    let mut lamp = shell.lamp_source;
    let lamp_src = backdrop.clone();
    use_effect(use_reactive!(|lamp_src| if *lamp.peek() != lamp_src { lamp.set(lamp_src); }));
    let trailer = facts.as_ref().map(|m| m.trailers.clone()).filter(|t| !t.is_empty());

    rsx! {
        article { class: "title-page", aria_label: "{media.name}",
            section { class: "title-hero",
                if !backdrop.is_empty() { img { class: "title-art", src: "{backdrop}", alt: "" } }
                crate::home::PosterArt { media: media.clone(), class: "title-poster".to_string() }
                div { class: "title-copy",
                    match logo.filter(|_| !logo_failed()) {
                        Some(src) => rsx! { img { class: "title-logo", src: "{src}", alt: "{media.name}", onerror: move |_| logo_failed.set(true) } },
                        None => rsx! { h1 { class: "display title-name", "{media.name}" } },
                    }
                    div { class: "facts",
                        if let Some(r) = facts.as_ref().map(|m| m.rating.clone()).filter(|r| !r.is_empty()) {
                            span { class: "fact", title: "IMDb rating", span { class: "lamp", icons::Star {} } span { class: "tnum", "{r}" } }
                        }
                        if let Some(r) = facts.as_ref().map(|m| m.runtime.clone()).filter(|r| !r.is_empty()) {
                            span { class: "fact", title: "Runtime", icons::Clock {} span { "{r}" } }
                        }
                        if !year.is_empty() { span { class: "fact", title: "Released", icons::Calendar {} span { class: "tnum", "{year}" } } }
                        span { class: "fact", title: "Type",
                            match media.kind.as_str() { "movie" => rsx! { icons::Film {} }, "series" => rsx! { icons::Tv {} }, _ => rsx! { icons::Blossom {} } }
                            span { "{backend::type_label(&media.kind)}" }
                        }
                        if !media.genres.is_empty() { span { class: "fact", title: "Genres", icons::Tag {} span { {media.genres.iter().take(3).cloned().collect::<Vec<_>>().join(", ")} } } }
                    }
                    div { class: "title-actions",
                        // A series carries on where you are: the started episode, else the one after the last you watched.
                        if !film {
                            if let Some(next) = up_next(&episodes(), &media, &seen()) {
                                button { class: "pill accent play-big", disabled: busy().is_some(),
                                    onclick: { let l = next.clone(); move |_| autoplay.call((l.clone(), backend::episode_label(&l))) },
                                    if busy().is_some() { span { class: "spinner", aria_hidden: "true" } span { "Finding source" } }
                                    else { icons::Play {} span { {
                                        let verb = if media.last_video_id.as_deref() == Some(next.as_str()) && backend::in_progress(&media) { "Resume" } else { "Play" };
                                        match backend::episode_label(&next) { Some(l) => format!("{verb} {l}"), None => verb.into() }
                                    } } }
                                }
                            }
                        }
                        if film {
                            button { class: "pill accent play-big", disabled: busy().is_some(),
                                onclick: { let id = media.id.clone(); move |_| autoplay.call((id.clone(), None)) },
                                if busy().is_some() { span { class: "spinner", aria_hidden: "true" } span { "Finding source" } }
                                else { icons::Play {} span { if backend::in_progress(&media) { "Resume" } else { "Play" } } }
                            }
                        }
                        icons::IconButton { label: if saved() { "Saved to Library (S)" } else { "Save to Library (S)" }, class: if saved() { "glass on" } else { "glass" },
                            pressed: saved(), onclick: move |_| toggle_save.call(()), icons::Bookmark {} }
                        icons::IconButton { label: if liked() > 0 { "Liked" } else { "Like" }, class: if liked() > 0 { "glass on" } else { "glass" },
                            pressed: liked() > 0, onclick: move |_| rate_like(1), icons::ThumbUp {} }
                        icons::IconButton { label: "Not for me", class: if liked() < 0 { "glass on" } else { "glass" },
                            pressed: liked() < 0, onclick: move |_| rate_hide(-1), icons::ThumbDown {} }
                        if film {
                            button { class: if watched() { "pill glass watched-switch on" } else { "pill glass watched-switch" }, role: "switch", aria_checked: "{watched()}",
                                title: "Already watched (W)",
                                onclick: move |_| toggle_watched.call(()),
                                if watched() { icons::Check {} } else { icons::EyeOff {} }
                                span { "Already watched" }
                            }
                        }
                        if let Some(t) = trailer {
                            icons::IconButton { label: "Trailer (T)", class: "glass", onclick: move |_| crate::play_trailers(shell, t.clone()), icons::Clapper {} }
                        }
                        if film {
                            icons::IconButton { label: "Choose a source", class: "glass", onclick: { let id = media.id.clone(); move |_| chooser.set(Some(Chooser { video: id.clone(), label: None })) }, icons::List {} }
                        }
                    }
                }
            }

            div { class: "title-body",
                if !media.description.is_empty() { p { class: "title-synopsis", "{media.description}" } }

                match meta() {
                    Load::Loading => rsx! { div { class: "rows", for i in 0..3 { div { key: "{i}", class: "row skeleton-row" } } } },
                    Load::Failed(e) => rsx! { div { class: "notice glass", span { class: "warn-icon", icons::Alert {} } span { "{e}" } } },
                    Load::Ready(m) => rsx! {
                        if !film { Episodes { list: m.episodes.clone(), media: media.clone(), season: season, last: media.last_video_id.clone(), busy: busy(), seen: seen,
                            on_play: move |(id, label): (String, String)| autoplay.call((id, Some(label))),
                            on_sources: move |(id, label): (String, String)| chooser.set(Some(Chooser { video: id, label: Some(label) })) } }
                        Credits { meta: m.clone() }
                    },
                }
            }

            if let Some(c) = chooser() {
                for c in [c] { SourceSheet { key: "{c.video}", media: m4.clone(), chooser: c.clone(), next: next_episode(&episodes(), &c.video).map(|e| (e.id.clone(), Some(label_of(&e)))),
                    on_close: move |_| chooser.set(None) } }
            }
        }
    }
}

/// Where a series carries on: the episode you started, unless you have watched it; else the
/// first unwatched one after the furthest you watched. None for a series not begun.
fn up_next(list: &[Episode], media: &Media, seen: &std::collections::HashSet<String>) -> Option<String> {
    if let Some(last) = media.last_video_id.as_ref().filter(|l| !seen.contains(*l) && backend::in_progress(media)) { return Some(last.clone()); }
    let furthest = list.iter().rposition(|e| e.season != 0 && seen.contains(&e.id))?;
    list.iter().skip(furthest + 1).find(|e| e.season != 0 && !seen.contains(&e.id)).map(|e| e.id.clone())
}

fn next_episode(list: &[Episode], video: &str) -> Option<Episode> {
    let at = list.iter().position(|e| e.id == video)?;
    list.get(at + 1).filter(|e| e.season != 0).cloned()
}

/// Start the platform player for a checked source and record progress for this video.
/// `rest`: the Play-order candidates after this one, for "Another source".
fn start(shell: Shell, media: &Media, video: &str, title: &str, src: Source, next: Option<(String, Option<String>)>) {
    start_with(shell, media, video, title, src, next, Vec::new(), None)
}

/// `at`: where the previous, failed source stopped, so the next one carries on from there.
#[allow(clippy::too_many_arguments)]
fn start_with(shell: Shell, media: &Media, video: &str, title: &str, src: Source, next: Option<(String, Option<String>)>, rest: Vec<Source>, at: Option<i64>) {
    let resume = at.filter(|p| *p > 0).unwrap_or_else(|| (backend::in_progress(media) && (media.last_video_id.as_deref() == Some(video) || media.kind == "movie"))
        .then_some(media.position_ms).unwrap_or(0));
    let full_title = title.to_owned();
    let subtitle = if video != media.id { media.name.clone() } else { media.meta_line(true) };
    let title = title.strip_prefix(&format!("{} ", media.name)).filter(|_| video != media.id).unwrap_or(title).to_owned();
    let another = {
        let (media, video, full_title, next) = (media.clone(), video.to_owned(), full_title.clone(), next.clone());
        Box::new(move |mut shell: Shell, position: i64| {
            let (media, video, full_title, next, rest) = (media.clone(), video.clone(), full_title.clone(), next.clone(), rest.clone());
            let ticket = crate::finding(shell, full_title.clone());
            spawn(async move {
                let (best, debrid) = backend::first_working(rest.clone()).await;
                if !crate::still_wanted(shell, ticket) { return; }
                match best {
                    Some(found) => {
                        let after: Vec<Source> = rest.iter().skip_while(|s| s.url != found.url).skip(1).cloned().collect();
                        start_with(shell, &media, &video, &full_title, found, next, after, Some(position));
                    }
                    None => {
                        shell.player_status.set(Some(PlayerStatus::Failed(backend::no_source_message(debrid.as_deref()).into())));
                        if shell.opened.peek().is_none() { shell.opened.set(Some(media.clone())); }
                    }
                }
            });
        }) as Box<dyn Fn(Shell, i64)>
    };
    crate::start_playback_with(shell, PlayRequest {
        title, subtitle, url: src.url, start_ms: resume, headers: src.headers,
        size: src.size.clone(), hdr: src.hdr.map(str::to_owned), quality: src.quality.map(str::to_owned),
        record: Some((media.clone(), (video != media.id).then(|| video.to_owned()))), has_next: next.is_some(),
        ..Default::default()
    }, next.map(|(id, label)| {
        let media = media.clone();
        Box::new(move |shell: Shell| play_next(shell, media.clone(), id.clone(), label.clone())) as Box<dyn Fn(Shell)>
    }), Some(another));
}

/// Resume from Home or Continue watching: the film, or the series' last episode, starts at once
/// with the automatic choice. Without a known episode the title page opens instead.
pub fn resume_now(mut shell: Shell, media: Media) {
    if media.kind == "movie" { let id = media.id.clone(); play_next(shell, media, id, None); return; }
    match media.last_video_id.clone() {
        Some(video) => {
            // The label comes from the video id's own season/episode numbers (series:season:episode).
            let label = backend::episode_label(&video);
            play_next(shell, media, video, label);
        }
        None => shell.opened.set(Some(media)),
    }
}

/// Picks resolves an explicit first/resume episode from fresh stored progress. Use the same
/// source selection, player lifetime and progress recording as the title page.
pub(crate) fn play_from_card(shell: Shell, media: Media, video: String) {
    let label = backend::episode_label(&video);
    play_next(shell,media,video,label);
}

/// Play one video now with the automatic choice, without the page (Resume, Next episode).
fn play_next(mut shell: Shell, media: Media, video: String, label: Option<String>) {
    let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
    let ticket = crate::finding(shell, title.clone());
    spawn(async move {
        let list = if media.kind == "movie" { Vec::new() } else { backend::title_meta(&media).await.map(|m| m.episodes).unwrap_or_default() };
        let label = label.or_else(|| list.iter().find(|e| e.id == video).map(|e| format!("S{} E{}", e.season, e.number)));
        let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
        let after = next_episode(&list, &video).map(|e| (e.id.clone(), Some(format!("S{} E{}", e.season, e.number))));
        let pack = list.iter().find(|e| e.id == video).map(|e| list.iter().filter(|x| x.season == e.season).count());
        let order = match backend::sources(&media.kind, &video).await { Ok((sources, _)) => backend::play_order(&sources, pack), Err(_) => Vec::new() };
        let (best, debrid) = backend::first_working(order.clone()).await;
        if !crate::still_wanted(shell, ticket) { return; }
        match best {
            Some(src) => {
                let rest: Vec<Source> = order.iter().skip_while(|s| s.url != src.url).skip(1).cloned().collect();
                start_with(shell, &media, &video, &title, src, after, rest, None)
            }
            None => {
                // Nothing played: say why and open the title page, where every source can be chosen.
                shell.player_status.set(Some(PlayerStatus::Failed(backend::no_source_message(debrid.as_deref()).into())));
                if shell.opened.peek().is_none() { shell.opened.set(Some(media.clone())); }
            }
        }
    });
}

/// Season chips, then the season's episodes. Tapping an episode plays it with the
/// automatic choice; its list icon opens the manual chooser instead.
#[component]
fn Episodes(list: Vec<Episode>, media: Media, season: Signal<Option<i64>>, last: Option<String>, busy: Option<String>,
            seen: Signal<std::collections::HashSet<String>>, on_play: EventHandler<(String, String)>, on_sources: EventHandler<(String, String)>) -> Element {
    let mut shell = use_context::<Shell>();
    let mut season = season;
    // TMDB stills for the shown season, used where the add-on gives no thumbnail.
    let mut stills = use_signal(std::collections::HashMap::<i64, String>::new);
    let for_stills = media.clone();
    use_effect(move || {
        let Some(n) = season() else { return };
        let m = for_stills.clone();
        spawn(async move { stills.set(backend::season_stills(&m, n).await); });
    });
    let mut seasons: Vec<i64> = list.iter().map(|e| e.season).collect();
    seasons.sort_by_key(|s| if *s == 0 { i64::MAX } else { *s });
    seasons.dedup();
    let shown: Vec<Episode> = list.iter().filter(|e| Some(e.season) == season()).cloned().collect();
    // Marking an episode watched marks the ones before it in its season ("season 1 up to
    // episode 3"); unmarking clears it and the ones after. The series counts as watched once
    // every episode is.
    let mut seen = seen;
    let (for_mark, all) = (media.clone(), list.clone());
    let mut series_done = use_signal(|| media.watched);
    let mark = use_callback(move |(ids, on): (Vec<String>, bool)| {
        if ids.is_empty() { return; }
        seen.with_mut(|s| for id in &ids { if on { s.insert(id.clone()); } else { s.remove(id); } });
        let complete = crate::picks_rules::series_complete(&all,&seen.peek(),backend::now_secs());
        let m = for_mark.clone();
        let changed = complete != *series_done.peek();
        if changed { series_done.set(complete); }
        spawn(async move {
            if backend::set_episodes_watched(&m, ids, on).await.is_ok() {
                if changed { let _ = backend::set_watched(&m, complete).await; }
                *shell.picks_dirty.write() += 1;
            }
        });
    });
    let season_ids: Vec<String> = shown.iter().map(|e| e.id.clone()).collect();
    let season_done = !season_ids.is_empty() && season_ids.iter().all(|id| seen.read().contains(id));
    let season_name = match season() { Some(0) => "Extras".to_string(), Some(n) => format!("Season {n}"), None => String::new() };
    rsx! {
        section { class: "episodes-block",
            if seasons.len() > 1 {
                div { class: "season-chips", role: "radiogroup", aria_label: "Season",
                    for n in seasons {
                        button { class: if season() == Some(n) { "chip-btn glass on small-chip" } else { "chip-btn glass small-chip" },
                            role: "radio", aria_checked: "{season() == Some(n)}", aria_label: if n == 0 { "Extras".to_string() } else { format!("Season {n}") },
                            onclick: move |_| season.set(Some(n)),
                            if n == 0 { icons::Sparkles {} } else { span { class: "tnum", "S{n}" } } }
                    }
                }
            }
            if !season_ids.is_empty() {
                button { class: if season_done { "pill glass watched-switch on" } else { "pill glass watched-switch" }, role: "switch", aria_checked: "{season_done}",
                    onclick: { let ids = season_ids.clone(); move |_| mark.call((ids.clone(), !season_done)) },
                    if season_done { icons::Check {} } else { icons::EyeOff {} }
                    span { "{season_name} watched" }
                }
            }
            div { class: "ep-list",
                for e in shown.clone() {
                    div { key: "{e.id}", class: if last.as_deref() == Some(e.id.as_str()) { "ep-card glass last" } else { "ep-card glass" },
                        button { class: "ep-main", disabled: busy.is_some(),
                            aria_label: "Play episode {e.number}: {e.title}",
                            onclick: { let (id, l) = (e.id.clone(), format!("S{} E{}", e.season, e.number)); move |_| on_play.call((id.clone(), l.clone())) },
                            div { class: "ep-still",
                                if let Some(src) = Some(e.thumbnail.clone()).filter(|t| !t.is_empty()).or_else(|| stills.read().get(&e.number).cloned()) {
                                    img { src: "{src}", alt: "", loading: "lazy" }
                                }
                                span { class: "ep-play", if busy.as_deref() == Some(e.id.as_str()) { span { class: "spinner" } } else { icons::Play {} } }
                            }
                            div { class: "ep-copy",
                                div { class: "ep-head",
                                    span { class: "tag q tnum", "E{e.number}" }
                                    span { class: "ep-name", "{e.title}" }
                                    if last.as_deref() == Some(e.id.as_str()) { span { class: "lamp", title: "Last watched", icons::History {} } }
                                }
                                if !e.overview.is_empty() { p { class: "ep-overview clamp-2", "{e.overview}" } }
                                if !e.released.is_empty() { span { class: "ep-date dim tnum", "{backend::date_label(&e.released)}" } }
                            }
                        }
                        {
                            let done = seen.read().contains(&e.id);
                            let ids: Vec<String> = shown.iter().filter(|x| if done { x.number >= e.number } else { x.number <= e.number }).map(|x| x.id.clone()).collect();
                            rsx! {
                                icons::IconButton { label: if done { format!("Watched E{}: unmark it and later episodes", e.number) } else { format!("Watched up to E{}", e.number) },
                                    class: if done { "small ep-seen on" } else { "small ep-seen" }, pressed: done,
                                    onclick: move |_| mark.call((ids.clone(), !done)), icons::Check {} }
                            }
                        }
                        icons::IconButton { label: "Choose a source", class: "small",
                            onclick: { let (id, l) = (e.id.clone(), format!("S{} E{}", e.season, e.number)); move |_| on_sources.call((id.clone(), l.clone())) },
                            icons::List {} }
                    }
                }
            }
        }
    }
}

#[component]
fn Credits(meta: TitleMeta) -> Element {
    let mut shell = use_context::<Shell>();
    let row = |label: &'static str, icon: Element, names: Vec<String>| rsx! {
        if !names.is_empty() {
            div { class: "credit-row",
                span { class: "credit-icon", title: label, aria_label: label, {icon} }
                div { class: "credit-chips",
                    for n in names.into_iter().take(12) {
                        button { class: "name-chip glass", title: "Search {n}",
                            onclick: { let n = n.clone(); move |_| { shell.search_query.set(Some(n.clone())); shell.search_open.set(true); } }, "{n}" }
                    }
                }
            }
        }
    };
    let fact = |label: &'static str, icon: Element, value: String| rsx! {
        if !value.is_empty() { div { class: "credit-row", span { class: "credit-icon", title: label, aria_label: label, {icon} } span { class: "credit-text", "{value}" } } }
    };
    rsx! {
        section { class: "credits",
            {row("Director", rsx! { icons::Clapper {} }, meta.directors.clone())}
            {row("Cast", rsx! { icons::Users {} }, meta.cast.clone())}
            {row("Writers", rsx! { icons::Pen {} }, meta.writers.clone())}
            {fact("Released", rsx! { icons::Calendar {} }, meta.released.clone())}
            {fact("Country", rsx! { icons::Globe {} }, meta.country.clone())}
            {fact("Language", rsx! { icons::Speech {} }, meta.language.clone())}
            {fact("Awards", rsx! { icons::Trophy {} }, meta.awards.clone())}
        }
    }
}

/// Manual choice: every add-on's sources for one video, in a sheet from the bottom.
#[component]
fn SourceSheet(media: Media, chooser: Chooser, next: Option<(String, Option<String>)>, on_close: EventHandler<()>) -> Element {
    let shell = use_context::<Shell>();
    let mut sources = use_signal(|| Load::<(Vec<Source>, Vec<String>)>::Loading);
    let mut expanded = use_signal(|| None::<usize>);
    let mut pressed_backdrop = use_signal(|| false);
    let (kind, video) = (media.kind.clone(), chooser.video.clone());
    use_hook(move || { spawn(async move {
        sources.set(match backend::sources(&kind, &video).await { Ok(r) => Load::Ready(r), Err(e) => Load::Failed(format!("{e:#}")) });
    }); });
    let title = chooser.label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
    let (m, t, v) = (media.clone(), title.clone(), chooser.video.clone());
    let play = use_callback(move |src: Source| { start(shell, &m, &v, &t, src, next.clone()); on_close.call(()); });
    rsx! {
        div { class: "scrim bottom",
            onpointerdown: move |_| pressed_backdrop.set(true),
            onclick: move |_| { if pressed_backdrop() { on_close.call(()); } pressed_backdrop.set(false); },
            section { class: "bottom-sheet glass", role: "dialog", aria_modal: "true", aria_label: "Sources for {title}",
                onpointerdown: move |e| e.stop_propagation(), onclick: move |e| e.stop_propagation(),
                div { class: "sheet-head",
                    span { class: "group-icon", icons::List {} }
                    span { class: "group-name", "{title}" }
                    if let Load::Ready((list, _)) = sources() { span { class: "tnum dim", "{list.len()}" } }
                    icons::IconButton { label: "Close", class: "glass", onclick: move |_| on_close.call(()), icons::Close {} }
                }
                div { class: "sheet-scroll",
                    match sources() {
                        Load::Loading => rsx! { div { class: "rows", for i in 0..4 { div { key: "{i}", class: "row skeleton-row" } } } },
                        Load::Failed(e) => rsx! { div { class: "notice glass", span { class: "warn-icon", icons::Alert {} } span { "{e}" } } },
                        Load::Ready((list, failed)) => rsx! {
                            if !failed.is_empty() { div { class: "notice glass", role: "alert", span { class: "warn-icon", icons::Alert {} } span { "{failed.join(\", \")} offline" } } }
                            if list.is_empty() { p { class: "dim", "No sources" } }
                            div { class: "rows",
                                for (i, src) in list.into_iter().enumerate() {
                                    SourceRow { key: "{i}", source: src.clone(), open: expanded() == Some(i),
                                        on_details: move |_| expanded.set(if expanded() == Some(i) { None } else { Some(i) }),
                                        on_play: move |s: Source| play.call(s) }
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// One source: quality and HDR chips, the file, its size and add-on; Details shows
/// the add-on's own text verbatim. Torrents and external links are shown, not played.
#[component]
fn SourceRow(source: Source, open: bool, on_details: EventHandler<()>, on_play: EventHandler<Source>) -> Element {
    let playable = source.kind == SourceKind::Direct;
    let label = if !source.filename.is_empty() { source.filename.clone() }
        else { source.description.lines().next().unwrap_or(&source.name).to_owned() };
    let why = match source.kind { SourceKind::Direct => None,
        SourceKind::Torrent => Some("P2P needs your consent; not in this prototype"),
        SourceKind::External => Some("Opens outside Veylo; not in this prototype") };
    let src = source.clone();
    rsx! {
        div { class: if playable { "source glass" } else { "source glass muted" },
            button { class: "source-main", disabled: !playable, title: why.unwrap_or(""),
                aria_label: "Play {label} from {source.addon}", onclick: move |_| on_play.call(src.clone()),
                div { class: "source-tags",
                    span { class: "tag q", {source.quality.unwrap_or("?")} }
                    if let Some(h) = source.hdr { span { class: "tag hdr", "{h}" } }
                }
                div { class: "source-text",
                    span { class: "source-file", "{label}" }
                    span { class: "source-meta dim", "{source.addon}" if let Some(size) = &source.size { span { class: "tnum", ", {size}" } } }
                }
                span { class: "source-go", match source.kind {
                    SourceKind::Direct => rsx! { icons::Play {} },
                    SourceKind::Torrent => rsx! { icons::Magnet {} },
                    SourceKind::External => rsx! { icons::Link {} },
                } }
            }
            icons::IconButton { label: if open { "Hide details" } else { "Details" }, class: "small", pressed: open, onclick: move |_| on_details.call(()), icons::Info {} }
            if open {
                div { class: "source-details",
                    if !source.name.is_empty() { p { "{source.name}" } }
                    if !source.description.is_empty() { p { class: "dim", "{source.description}" } }
                    if !source.filename.is_empty() { p { class: "dim", "{source.filename}" } }
                }
            }
        }
    }
}
