//! Picks as a duel: two big cards, keep one. The kept title stays and meets a challenger
//! chosen by your ranked recommendations re-weighted by what you kept; after the set number
//! of rounds the winner is the final pick. Taste is saved until "Start over".
use crate::{backend::{self, Media, Taste, TitleMeta}, icons, picks_rules::{self as rules, Style}, PickCommand, Shell};
use dioxus::prelude::*;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq)]
enum Slot { Genre, Years }

fn year_span(from: &str, to: &str) -> Option<String> { (!from.is_empty() || !to.is_empty()).then(|| format!("{from}–{to}")) }

#[component]
pub fn Picks() -> Element {
    let mut shell = use_context::<Shell>();
    let mut pool = use_signal(Vec::<Media>::new);
    let mut loading = use_signal(|| true);
    let mut taste = use_signal(backend::load_taste);
    let rounds = use_signal(|| backend::load_settings().picks_rounds);
    // A duel in progress comes back as you left it, filters included.
    let saved = use_hook(backend::load_duel);
    let mut restored = use_signal(|| saved.is_some());
    let saved = saved.unwrap_or_default();
    let mut round = use_signal(|| saved.round);
    // Left and right cards; the kept one stays on its side.
    let mut pair = use_signal(|| (saved.left.clone(), saved.right.clone()));
    let mut shown = use_signal(|| saved.shown.clone());
    let mut winner = use_signal(|| saved.winner.clone());
    let mut kind = use_signal(|| saved.kind.clone());
    let mut style = use_signal(|| Style::parse(&saved.style));
    let mut genre = use_signal(|| saved.genre.clone());
    let mut from_text = use_signal(|| saved.from.clone());
    let mut to_text = use_signal(|| saved.to.clone());
    let mut hide_watched = use_signal(|| backend::load_settings().picks_hide_watched);
    let watched = use_resource(|| async { backend::watched_ids().await });
    let mut slot = use_signal(|| None::<Slot>);
    // The app's accent: neutral during a duel (each card has its own), the winner's once chosen.
    let mut lamp = shell.lamp_source;
    use_effect(move || {
        let Some(w) = winner() else { if !lamp.peek().is_empty() { lamp.set(String::new()); } return };
        spawn(async move {
            let src = backend::artwork(&w).await.and_then(|a| a.backdrop).unwrap_or_else(|| w.backdrop().to_owned());
            if winner.peek().as_ref().map(Media::key) == Some(w.key()) && *lamp.peek() != src { lamp.set(src); }
        });
    });

    // Candidates: your ranked recommendations first (the duel starts on them), then the wider
    // catalogue in the background: random pages of popular, well-rated TMDB titles and a TVmaze
    // sample, so every visit brings new titles.
    use_future(move || async move {
        let mut items = backend::picks().await.map(|p| p.items).unwrap_or_default();
        if items.len() < 6 { items.extend(backend::seed_catalogs().await); }
        let mut first = Vec::new();
        rules::merge(&mut first, items);
        pool.set(first);
        loading.set(false);
        let pages: Vec<u32> = (0..2).map(|_| 1 + (backend::rand_u64() % 25) as u32).collect();
        let mut more = backend::tmdb_candidates(None, None, Style::Both, None, None, pages).await;
        more.extend(backend::tvmaze_candidates(None, Style::Both, None, None).await);
        pool.with_mut(|p| rules::merge(p, more));
    });

    let from = use_memo(move || from_text().parse::<i32>().ok().filter(|_| from_text().len() == 4));
    let to = use_memo(move || to_text().parse::<i32>().ok().filter(|_| to_text().len() == 4));
    // Titles checked for a poster and a trailer: only ready ones are drawn when enough are, and a
    // title found lacking either is set aside for the session.
    let mut verified = use_signal(HashSet::<String>::new);
    let mut rejected = use_signal(HashSet::<String>::new);
    let mut checking = use_signal(HashSet::<String>::new);
    let filtered = use_memo(move || {
        let this_year = backend::civil(backend::now_secs()).0 as i32;
        let out = rules::filter_picks(&pool.read(), kind().as_deref(), style(), genre().as_deref(), from(), to(), this_year);
        let gone = rejected.read();
        let seen = if hide_watched() { watched.read().clone().unwrap_or_default() } else { HashSet::new() };
        let finished = |m: &Media| seen.contains(&m.id) || seen.contains(&rules::title_key(m)) || m.raw["imdb_id"].as_str().is_some_and(|i| seen.contains(i));
        out.into_iter().filter(|m| !gone.contains(&m.key()) && !finished(m)).collect::<Vec<_>>()
    });
    // The verified part of a list, when it still offers a choice beyond `exclude`.
    let prefer = move |list: &[Media], exclude: &[String]| -> Vec<Media> {
        let ok = verified.peek();
        let ready: Vec<Media> = list.iter().filter(|m| ok.contains(&m.key())).cloned().collect();
        if ready.iter().filter(|m| !exclude.contains(&m.key())).count() >= 2 { ready } else { list.to_vec() }
    };
    // Check the candidates ahead of the duel, a dozen at a time.
    use_effect(move || {
        let list = filtered.read().clone();
        let todo: Vec<Media> = {
            let (ok, busy) = (verified.peek(), checking.peek());
            list.into_iter().filter(|m| !ok.contains(&m.key()) && !busy.contains(&m.key())).take(12).collect()
        };
        for m in todo {
            checking.write().insert(m.key());
            let all_watched = watched.peek().clone().unwrap_or_default();
            spawn(async move {
                match check(&m, &all_watched).await {
                    Check::Ready => { verified.write().insert(m.key()); }
                    Check::Reject => { rejected.write().insert(m.key()); }
                    Check::Instead(first) => { rejected.write().insert(m.key()); pool.with_mut(|p| rules::merge(p, vec![first])); }
                }
                checking.write().remove(&m.key());
            });
        }
    });
    // A rare genre: ask the catalogs that declare it for more titles.
    let mut fetched_genres = use_signal(HashSet::<String>::new);
    let catalog_genres = use_resource(|| async { backend::catalog_genres().await });
    let mut enriching = use_signal(|| false);
    // Few candidates for these filters: ask the catalogs that declare the genre, then TMDB
    // Discover (any genre, type and years), each once per filter combination.
    use_effect(move || {
        let (g, k, st, f, t) = (genre(), kind(), style(), from(), to());
        if g.is_none() && k.is_none() && st == Style::Both && f.is_none() && t.is_none() { return; }
        let combo = format!("{g:?}|{k:?}|{st:?}|{f:?}|{t:?}");
        if filtered.read().len() >= 8 || fetched_genres.peek().contains(&combo) { return; }
        fetched_genres.write().insert(combo);
        enriching.set(true);
        spawn(async move {
            let mut more = match &g { Some(g) => backend::genre_candidates(g).await, None => Vec::new() };
            more.extend(backend::tmdb_candidates(g.clone(), k.clone(), st, f, t, vec![1, 2]).await);
            if k.as_deref() != Some("movie") { more.extend(backend::tvmaze_candidates(g, st, f, t).await); }
            pool.with_mut(|p| rules::merge(p, more));
            enriching.set(false);
        });
    });

    // A fresh duel whenever the candidates or filters change.
    let mut start_duel = move || {
        let list = filtered.peek().clone();
        let t = taste.peek().clone();
        // Opening pair: drawn at random from the dozen best fits, so no visit starts the same.
        let list = prefer(&list, &[]);
        let first = backend::next_challenger(&list, &t, &[], 12);
        let second = first.as_ref().and_then(|a| {
            // The opening pair differs in genre where possible, so the first choice says something.
            let different: Vec<Media> = list.iter().filter(|m| m.key() != a.key() && !m.genres.iter().any(|g| a.genres.contains(g))).cloned().collect();
            backend::next_challenger(if different.is_empty() { &list } else { &different }, &t, &[a.key()], 12)
        });
        shown.set([&first, &second].iter().filter_map(|m| m.as_ref().map(Media::key)).collect());
        round.set(0);
        // A single match is the answer: show it as the pick rather than "nothing matches".
        if let (Some(only), None) = (&first, &second) { winner.set(Some(only.clone())); pair.set((None, None)); return; }
        pair.set((first, second));
        winner.set(None);
    };
    // A new duel when a filter changes; a restored one is kept until then.
    use_effect(move || {
        let _ = (kind(), style(), genre(), from(), to(), hide_watched());
        if loading() { return; }
        if *restored.peek() { restored.set(false); return; }
        start_duel();
    });
    // Remember the duel as it goes.
    use_effect(move || {
        let (left, right) = pair();
        backend::save_duel(&backend::DuelState { kind: kind(), style: style().as_str().to_owned(), genre: genre(), from: from_text(), to: to_text(), round: round(),
            shown: shown(), left, right, winner: winner() });
    });
    use_effect(move || {
        let n = filtered.read().len();
        let (l, r) = pair.peek().clone();
        let waiting = (l.is_none() || r.is_none()) && winner.peek().is_none();
        let lone = winner.peek().is_some() && *round.peek() == 0;
        if !loading() && n >= 2 && (waiting || lone) { start_duel(); }
    });

    // Keep one side: learn, then bring a challenger to the other side (or crown the winner).
    let mut keep = move |left: bool| {
        let (l, r) = pair.peek().clone();
        let (Some(kept), Some(dropped)) = (if left { (l.clone(), r.clone()) } else { (r.clone(), l.clone()) }) else { return };
        // Not while a card is still being checked (a key press could pick a card you cannot see).
        if !verified.peek().contains(&kept.key()) || !verified.peek().contains(&dropped.key()) { return; }
        let mut t = taste.peek().clone();
        t.learn(&kept, &dropped);
        backend::save_taste(&t);
        taste.set(t.clone());
        let done = *round.peek() + 1;
        round.set(done);
        if done >= *rounds.peek() { winner.set(Some(kept)); return; }
        let mut exclude = shown.peek().clone();
        let challenger = backend::next_challenger(&prefer(&filtered.peek(), &exclude), &t, &exclude, 3);
        if let Some(c) = &challenger { exclude.push(c.key()); }
        shown.set(exclude);
        match challenger {
            Some(c) => pair.set(if left { (Some(kept), Some(c)) } else { (Some(c), Some(kept)) }),
            None => winner.set(Some(kept)),
        }
    };

    // A card on screen that turns out to lack a poster or trailer gives its place to another
    // (not a round).
    use_effect(move || {
        let (l, r) = pair();
        for (left, m) in [(true, l), (false, r)] {
            let Some(m) = m else { continue };
            if verified.peek().contains(&m.key()) { continue; }
            let all_watched = watched.peek().clone().unwrap_or_default();
            spawn(async move {
                match check(&m, &all_watched).await {
                    Check::Ready => { verified.write().insert(m.key()); return; }
                    Check::Reject => { rejected.write().insert(m.key()); }
                    Check::Instead(first) => { rejected.write().insert(m.key()); pool.with_mut(|p| rules::merge(p, vec![first])); }
                }
                let (l, r) = pair.peek().clone();
                let on_screen = if left { l } else { r };
                if on_screen.map(|x| x.key()) != Some(m.key()) { return; }
                let mut exclude = shown.peek().clone();
                let t = taste.peek().clone();
                // Nothing else to offer: show it anyway rather than an empty side.
                let Some(c) = backend::next_challenger(&prefer(&filtered.peek(), &exclude), &t, &exclude, 3) else { verified.write().insert(m.key()); return };
                exclude.push(c.key());
                shown.set(exclude);
                pair.with_mut(|p| if left { p.0 = Some(c) } else { p.1 = Some(c) });
            });
        }
    });

    use_effect(move || {
        let Some(command) = (shell.pick_command)() else { return };
        shell.pick_command.set(None);
        if winner.peek().is_none() { keep(command == PickCommand::Left); }
    });

    // Only genres that can make a duel: at least two titles, or a catalog that can fetch more.
    let genres: Vec<(String, usize, bool)> = {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for m in pool.read().iter() { for g in rules::genre_choices(m) { *counts.entry(g).or_default() += 1; } }
        let fetchable = catalog_genres.read().clone().unwrap_or_default();
        let tmdb = backend::tmdb_ready();
        counts.into_iter().map(|(g, n)| { let more = tmdb || fetchable.contains(&g.to_lowercase()); (g, n, more) })
            .filter(|(_, n, more)| *n >= 2 || *more).collect()
    };
    let years = year_span(&from_text(), &to_text());
    let mut toggle_slot = move |s: Slot| slot.set(if slot() == Some(s) { None } else { Some(s) });
    let type_options: [(Option<&'static str>, &'static str); 3] = [(None, "Everything"), (Some("movie"), "Films"), (Some("series"), "Series")];
    let style_options: [(Style, &'static str); 3] = [(Style::Live, "Live action"), (Style::Animation, "Animation"), (Style::Both, "Live action and animation")];
    let total = rounds();
    let done = round();

    rsx! {
        div { class: "duel",
            div { class: "duel-bar",
                div { class: "duel-progress", aria_label: "Round {done.min(total) + 1} of {total}",
                    span { class: "display duel-round", if winner().is_some() { "Your pick" } else { "Round {done + 1}" } }
                    span { class: "dots", for i in 0..total { i { key: "{i}", class: if i < done { "dot done" } else if i == done && winner().is_none() { "dot now" } else { "dot" } } } }
                }
                div { class: "chips",
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Type",
                        for (value, label) in type_options {
                            button { class: if kind().as_deref() == value { "seg on" } else { "seg" }, role: "radio", aria_checked: "{kind().as_deref() == value}",
                                aria_label: "{label}", title: "{label}", onclick: move |_| kind.set(value.map(str::to_owned)),
                                match value { None => rsx! { icons::Grid {} }, Some("movie") => rsx! { icons::Film {} }, _ => rsx! { icons::Tv {} } }
                            }
                        }
                    }
                    // Style beside type: animated films and series are still films and series.
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Style",
                        for (value, label) in style_options {
                            button { class: if style() == value { "seg on" } else { "seg" }, role: "radio", aria_checked: "{style() == value}",
                                aria_label: "{label}", title: "{label}", onclick: move |_| style.set(value),
                                match value { Style::Live => rsx! { icons::Camera {} }, Style::Animation => rsx! { icons::Blossom {} }, Style::Both => rsx! { icons::Layers {} } }
                            }
                        }
                    }
                    button { class: if slot() == Some(Slot::Genre) || genre().is_some() { "chip-btn glass on" } else { "chip-btn glass" }, aria_label: "Genre", title: "Genre",
                        onclick: move |_| toggle_slot(Slot::Genre), icons::Tag {} if let Some(g) = genre() { span { "{g}" } } }
                    button { class: if slot() == Some(Slot::Years) || years.is_some() { "chip-btn glass on" } else { "chip-btn glass" }, aria_label: "Years", title: "Years",
                        onclick: move |_| toggle_slot(Slot::Years), icons::Calendar {} if let Some(y) = years.clone() { span { class: "tnum", "{y}" } } }
                    button { class: if hide_watched() { "chip-btn glass on" } else { "chip-btn glass" }, role: "switch", aria_checked: "{hide_watched()}",
                        aria_label: "Hide titles you finished", title: if hide_watched() { "Hiding titles you finished" } else { "Showing titles you finished" },
                        onclick: move |_| {
                            let on = !hide_watched();
                            backend::save_settings(&backend::Settings { picks_hide_watched: on, ..backend::load_settings() });
                            hide_watched.set(on);
                        },
                        icons::EyeOff {} }
                    icons::IconButton { label: "Start over: forget what Picks learned", class: "glass small",
                        onclick: move |_| { backend::reset_taste(); taste.set(Taste::default()); start_duel(); }, icons::Undo {} }
                }
            }
            match slot() {
                Some(Slot::Genre) => rsx! {
                    div { class: "popover glass", role: "radiogroup", aria_label: "Genre",
                        button { class: if genre().is_none() { "option on" } else { "option" }, onclick: move |_| { genre.set(None); slot.set(None); }, "Any" }
                        for (g, _n, more) in genres {
                            button { class: if genre().as_deref() == Some(g.as_str()) { "option on" } else { "option" },
                                title: if more { "More from your catalogs" } else { "" },
                                onclick: { let g = g.clone(); move |_| { genre.set(Some(g.clone())); slot.set(None); } },
                                "{g}" }
                        }
                    }
                },
                Some(Slot::Years) => rsx! {
                    div { class: "popover glass years",
                        input { class: "year", r#type: "text", inputmode: "numeric", maxlength: "4", placeholder: "From", aria_label: "From year", value: "{from_text}",
                            onkeydown: move |e| e.stop_propagation(), oninput: move |e| from_text.set(e.value().chars().filter(char::is_ascii_digit).take(4).collect()) }
                        span { class: "dim", "–" }
                        input { class: "year", r#type: "text", inputmode: "numeric", maxlength: "4", placeholder: "To", aria_label: "To year", value: "{to_text}",
                            onkeydown: move |e| e.stop_propagation(), oninput: move |e| to_text.set(e.value().chars().filter(char::is_ascii_digit).take(4).collect()) }
                        icons::IconButton { label: "Done", class: "glass small", onclick: move |_| slot.set(None), icons::Check {} }
                    }
                },
                None => rsx! {},
            }

            if loading() || (enriching() && pair().0.is_none() && winner().is_none()) {
                div { class: "duel-cards", div { class: "duel-card skeleton-card" } div { class: "duel-card skeleton-card" } }
            } else if let Some(pick) = winner() {
                for pick in [pick.clone()] { FinalPick { key: "{pick.key()}", media: pick.clone(),
                    reason: if round() == 0 { vec!["__only".to_string()] } else { taste().top_genres(&pick) },
                    on_continue: move |_| {
                        // Keep refining: the pick stays in play and meets new challengers.
                        let Some(w) = winner.peek().clone() else { return start_duel() };
                        let mut exclude = shown.peek().clone();
                        let challenger = backend::next_challenger(&prefer(&filtered.peek(), &exclude), &taste.peek(), &exclude, 3);
                        let Some(c) = challenger else { return start_duel() };
                        exclude.push(c.key());
                        shown.set(exclude);
                        round.set(0);
                        pair.set((Some(w), Some(c)));
                        winner.set(None);
                    },
                    on_reset: move |_| { backend::reset_taste(); taste.set(Taste::default()); start_duel(); } } }
            } else {
                match pair() {
                    (Some(l), Some(r)) => rsx! {
                        div { class: "duel-cards",
                            // A key only renews a component inside a list: a new title must get a new card.
                            // A card shows once its poster and trailer are confirmed.
                            if verified.read().contains(&l.key()) {
                                for m in [l.clone()] { DuelCard { key: "{m.key()}", media: m.clone(), side: "left", on_keep: move |_| keep(true) } }
                            } else { div { class: "duel-card skeleton-card" } }
                            span { class: "versus display", aria_hidden: "true", "or" }
                            if verified.read().contains(&r.key()) {
                                for m in [r.clone()] { DuelCard { key: "{m.key()}", media: m.clone(), side: "right", on_keep: move |_| keep(false) } }
                            } else { div { class: "duel-card skeleton-card" } }
                        }
                    },
                    _ => rsx! {
                        div { class: "center-note", div { class: "glass panel", span { class: "big-icon dim", icons::Search {} } p { "Nothing matches these filters" }
                            button { class: "pill glass", onclick: move |_| { kind.set(None); genre.set(None); from_text.set(String::new()); to_text.set(String::new()); }, icons::Undo {} span { "Clear filters" } } } }
                    },
                }
            }
        }
    }
}

/// One side of the duel: artwork, facts, synopsis, trailer, keep.
#[component]
fn DuelCard(media: Media, side: &'static str, on_keep: EventHandler<()>) -> Element {
    let shell = use_context::<Shell>();
    let mut meta = use_signal(|| None::<TitleMeta>);
    let m = media.clone();
    use_hook(move || { spawn(async move { if let Ok(t) = backend::title_meta(&m).await { meta.set(Some(t)); } }); });
    let facts = meta();
    let art = crate::home::use_artwork(&media);
    let logo = art().and_then(|a| a.logo).or_else(|| facts.as_ref().map(|f| f.logo.clone()).filter(|l| !l.is_empty()));
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    let mut logo_failed = use_signal(|| false);
    // Each card lends its own accent: its Keep button takes the colour of its artwork.
    let accent_src = backdrop.clone();
    let accent = use_resource(use_reactive!(|accent_src| async move { backend::accent(accent_src).await }));
    let card_style = accent().flatten().map(|c| format!("--lamp: {c}")).unwrap_or_default();
    let trailer = facts.as_ref().map(|f| f.trailers.clone()).filter(|t| !t.is_empty())
        .or_else(|| art().map(|a| a.trailers.into_iter().map(|k| backend::Trailer { youtube: Some(k), url: None }).collect::<Vec<_>>()).filter(|t| !t.is_empty()));
    let director = facts.as_ref().and_then(|f| f.directors.first().cloned());
    let arrow = if side == "left" { "←" } else { "→" };
    let open = media.clone();
    rsx! {
        article { class: "duel-card {side}", style: "{card_style}", aria_label: "{media.name}",
            if !backdrop.is_empty() { img { class: "duel-art", src: "{backdrop}", alt: "" } }
            button { class: "duel-hit", aria_label: "Keep {media.name} ({arrow})", onclick: move |_| on_keep.call(()) }
            div { class: "duel-copy",
                crate::home::PosterArt { media: media.clone(), class: "duel-poster".to_string() }
                match logo.filter(|_| !logo_failed()) {
                    Some(src) => rsx! { img { class: "duel-logo", src: "{src}", alt: "{media.name}", onerror: move |_| logo_failed.set(true) } },
                    None => rsx! { h2 { class: "display duel-title", "{media.name}" } },
                }
                div { class: "facts",
                    if let Some(r) = facts.as_ref().map(|f| f.rating.clone()).filter(|r| !r.is_empty()) { span { class: "fact", span { class: "lamp", icons::Star {} } span { class: "tnum", "{r}" } } }
                    if let Some(r) = facts.as_ref().map(|f| f.runtime.clone()).filter(|r| !r.is_empty()) { span { class: "fact", icons::Clock {} span { "{r}" } } }
                    if !media.year.is_empty() { span { class: "fact", icons::Calendar {} span { class: "tnum", "{media.year}" } } }
                    if !media.genres.is_empty() { span { class: "fact", icons::Tag {} span { {media.genres.iter().take(2).cloned().collect::<Vec<_>>().join(", ")} } } }
                }
                if !media.description.is_empty() { p { class: "duel-synopsis clamp-3", "{media.description}" } }
                if let Some(d) = director { p { class: "duel-credit", span { class: "dim", icons::Clapper {} } span { "{d}" } } }
                div { class: "duel-actions",
                    button { class: "pill accent", onclick: move |e| { e.stop_propagation(); on_keep.call(()) }, icons::Check {} span { "Keep this" } kbd { class: "keycap small-cap", "{arrow}" } }
                    if let Some(t) = trailer { icons::IconButton { label: "Trailer", class: "glass", onclick: move |_| crate::play_trailers(shell, t.clone()), icons::Clapper {} } }
                    icons::IconButton { label: "Details", class: "glass", onclick: move |_| { let mut s = shell; s.opened.set(Some(open.clone())) }, icons::Info {} }
                }
            }
        }
    }
}

/// The winner, with why, and the ways on: watch it, or keep refining, or start over.
#[component]
fn FinalPick(media: Media, reason: Vec<String>, on_continue: EventHandler<()>, on_reset: EventHandler<()>) -> Element {
    let mut shell = use_context::<Shell>();
    let mut meta = use_signal(|| None::<TitleMeta>);
    let m = media.clone();
    use_hook(move || { spawn(async move { if let Ok(t) = backend::title_meta(&m).await { meta.set(Some(t)); } }); });
    let art = crate::home::use_artwork(&media);
    let trailer = meta().map(|f| f.trailers).filter(|t| !t.is_empty())
        .or_else(|| art().map(|a| a.trailers.into_iter().map(|k| backend::Trailer { youtube: Some(k), url: None }).collect::<Vec<_>>()).filter(|t| !t.is_empty()));
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    let (play, open) = (media.clone(), media.clone());
    let why = if reason.first().map(String::as_str) == Some("__only") { "The only match for these filters".to_string() }
        else if reason.is_empty() { "It won every duel".to_string() } else { format!("You keep choosing {}", reason.join(" and ").to_lowercase()) };
    rsx! {
        article { class: "final-pick", aria_label: "Your pick: {media.name}",
            if !backdrop.is_empty() { img { class: "final-art", src: "{backdrop}", alt: "" } }
            crate::home::PosterArt { media: media.clone(), class: "final-poster".to_string() }
            div { class: "final-copy",
                p { class: "reason", span { class: "lamp", icons::Sparkles {} } span { "{why}" } }
                h1 { class: "display pick-title", "{media.name}" }
                p { class: "meta", "{media.meta_line(true)}" if !media.genres.is_empty() { span { class: "dim", ", {media.genres.iter().take(3).cloned().collect::<Vec<_>>().join(\", \")}" } } }
                if !media.description.is_empty() { p { class: "synopsis clamp-3", "{media.description}" } }
                div { class: "title-actions",
                    button { class: "pill accent play-big", onclick: move |_| crate::title::resume_now(shell, play.clone()), icons::Play {} span { if media.kind == "movie" { "Play" } else { "Watch" } } }
                    if let Some(t) = trailer { icons::IconButton { label: "Trailer", class: "glass", onclick: move |_| crate::play_trailers(shell, t.clone()), icons::Clapper {} } }
                    icons::IconButton { label: "Details", class: "glass", onclick: move |_| shell.opened.set(Some(open.clone())), icons::Info {} }
                    button { class: "pill glass", title: "Keep this pick and face new challengers", onclick: move |_| on_continue.call(()), icons::ArrowRight {} span { "Continue" } }
                    button { class: "pill glass", title: "Forget what Picks learned", onclick: move |_| on_reset.call(()), icons::Undo {} span { "Start over" } }
                }
            }
        }
    }
}

/// A poster with the name under it; used by Search.
#[component]
pub fn PosterButton(media: Media, on_press: EventHandler<Media>, #[props(default)] badge: Option<String>, #[props(default)] pressed: Option<bool>) -> Element {
    let m = media.clone();
    let artwork = backend::posters(&media);
    let mut index = use_signal(|| 0usize);
    let art = artwork.get(index()).cloned();
    rsx! {
        button {
            class: if pressed == Some(true) { "poster on" } else { "poster" },
            aria_label: "{media.name}",
            aria_pressed: pressed.map(|p| p.to_string()),
            onclick: move |_| on_press.call(m.clone()),
            div { class: "frame",
                match art {
                    Some(src) => rsx! { img { src: "{src}", alt: "", loading: "lazy", onerror: move |_| index += 1 } },
                    None => rsx! { span { class: "frame-name", "{media.name}" } },
                }
                if badge.is_some() { span { class: "badge", aria_hidden: "true", icons::Heart {} } }
            }
            span { class: "poster-name", "{media.name}" }
            span { class: "poster-meta", "{media.meta_line(false)}" }
        }
    }
}

/// What a candidate check found.
enum Check { Ready, Reject, Instead(Media) }

/// A title can stand in a duel with a poster and a trailer; a later film in a collection gives
/// way to the first part you have not seen.
async fn check(m: &Media, watched: &HashSet<String>) -> Check {
    if !backend::duel_ready(m).await { return Check::Reject; }
    match backend::earlier_part(m, watched).await { Some(first) => Check::Instead(first), None => Check::Ready }
}
