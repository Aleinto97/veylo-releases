//! Picks as a duel: two big cards, keep one. The kept title stays and meets a challenger
//! chosen by local semantic taste. Independent pre-vote confirmations suggest a pick. Taste is saved until "Start over".
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
    let mut pool_complete = use_signal(|| false);
    let mut taste = use_signal(backend::load_taste);
    // A duel in progress comes back as you left it, filters included.
    let saved = use_hook(backend::load_duel);
    let mut restored = use_signal(|| saved.is_some());
    let saved = saved.unwrap_or_default();
    use_hook(|| { let mut t = taste.write(); t.session = saved.session.clone(); t.session_vibe = saved.session_vibe.clone(); });
    let mut round = use_signal(|| saved.round);
    let mut evidence = use_signal(|| saved.evidence.clone());
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
    let watched = use_resource(move || { let _ = (shell.picks_dirty)(); async { backend::watched_ids().await } });
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
        // Core discovery omits started titles. Add history back to the pool so unfinished
        // series can actually appear with Resume; Hide watched below removes completed ones.
        items.extend(backend::viewing_history().await.unwrap_or_default().into_iter().map(|s| s.media));
        let mut first = Vec::new();
        rules::merge(&mut first, items);
        pool.set(first);
        loading.set(false);
        let pages: Vec<u32> = (0..2).map(|_| 1 + (backend::rand_u64() % 25) as u32).collect();
        let mut more = backend::tmdb_candidates(None, None, Style::Both, None, None, pages).await;
        more.extend(backend::tvmaze_candidates(None, Style::Both, None, None).await);
        pool.with_mut(|p| rules::merge(p, more));
        pool_complete.set(true);
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
    let resolved_pair = use_memo(move || {
        let list = pool.read();
        let resolve = |m: Option<Media>| m.map(|m| list.iter().find(|x| x.key() == m.key() || rules::title_key(x) == rules::title_key(&m)).cloned().unwrap_or(m));
        let (l,r) = pair(); (resolve(l),resolve(r))
    });
    // Embed beyond the first trailer-ready titles. Otherwise a matching title deeper in the
    // catalogue cannot challenge one that happened to finish verification first.
    let _embedding_prefetch = use_resource(move || {
        let candidates = filtered();
        async move {
            for batch in candidates.into_iter().take(96).collect::<Vec<_>>().chunks(4) {
                if crate::picks_ai::failed() { break; }
                let batch = batch.to_vec();
                let _ = backend::on_runtime(async move {
                    let mut tasks = tokio::task::JoinSet::new();
                    for media in batch { tasks.spawn(async move { let _ = crate::picks_ai::enrich_and_embed(&media).await; }); }
                    while tasks.join_next().await.is_some() {}
                }).await;
            }
        }
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
                    Check::Ready(ready) => { pool.with_mut(|p| rules::merge(p, vec![ready])); verified.write().insert(m.key()); }
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
        { let mut t = taste.write(); t.session.clear(); t.session_vibe.clear(); }
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
        evidence.set(Default::default());
        crate::picks_decision::audit("start", None, None, &evidence.peek(), None, list.len());
        // A single match is the answer: show it as the pick rather than "nothing matches".
        if let (Some(only), None) = (&first, &second) {
            if *pool_complete.peek() && !*enriching.peek() { winner.set(Some(only.clone())); pair.set((None, None)); return; }
        }
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
    // Progress can change while the player or title page is open. A restored pair or final
    // winner must obey Hide watched too, not just newly drawn challengers.
    use_effect(move || {
        if !hide_watched() || loading() { return; }
        let Some(seen) = watched.read().clone() else { return };
        let finished = |m: &Media| seen.contains(&m.id) || seen.contains(&rules::title_key(m))
            || m.raw["imdb_id"].as_str().is_some_and(|id| seen.contains(id));
        let (left,right) = pair.peek().clone();
        let final_pick = winner.peek().clone();
        if left.iter().chain(right.iter()).chain(final_pick.iter()).any(finished) { start_duel(); }
    });
    // Remember the duel as it goes.
    use_effect(move || {
        let (left, right) = pair();
        backend::save_duel(&backend::DuelState { kind: kind(), style: style().as_str().to_owned(), genre: genre(), from: from_text(), to: to_text(), round: round(),
            evidence: evidence(), session: taste().session, session_vibe: taste().session_vibe, shown: shown(), left, right, winner: winner() });
    });
    use_effect(move || {
        let n = filtered.read().len();
        let complete = pool_complete();
        let growing = enriching();
        let (l, r) = pair.peek().clone();
        let waiting = (l.is_none() || r.is_none()) && winner.peek().is_none();
        let lone = winner.peek().is_some() && *round.peek() == 0;
        if !loading() && ((n >= 2 && ((waiting && shown.peek().len() <= 1) || lone))
            || (n == 1 && complete && !growing && waiting && *round.peek() == 0)) { start_duel(); }
    });

    // Style is an invariant of visible cards, including restored cards enriched later.
    use_effect(move || {
        let list = filtered();
        let (l,r) = resolved_pair();
        let st = style();
        let invalid = l.iter().chain(r.iter()).any(|m| !rules::style_matches(m,st));
        let final_invalid = winner().is_some_and(|m| {
            let all = pool.read();
            let canonical = all.iter().find(|x| x.key() == m.key() || rules::title_key(x) == rules::title_key(&m)).unwrap_or(&m);
            !rules::style_matches(canonical,st)
        });
        let waiting_empty = l.is_none() && r.is_none() && winner.peek().is_none() && *round.peek() > 0;
        if !invalid && !final_invalid && !waiting_empty { return; }
        let mut exclude = shown.peek().clone();
        let mut replace = |m: Option<Media>| {
            if m.as_ref().is_some_and(|m| rules::style_matches(m,st)) { return m; }
            let next = backend::next_challenger(&list,&taste.peek(),&exclude,3);
            if let Some(m) = &next { exclude.push(m.key()); }
            next
        };
        let (l,r) = (replace(l),replace(r));
        shown.set(exclude);pair.set((l,r));
        if final_invalid { winner.set(None); }
    });

    // Keep one side: learn, then bring a challenger to the other side (or crown the winner).
    let mut keep = move |left: bool| {
        let (l, r) = resolved_pair.peek().clone();
        let (Some(kept), Some(dropped)) = (if left { (l.clone(), r.clone()) } else { (r.clone(), l.clone()) }) else { return };
        // Not while a card is still being checked (a key press could pick a card you cannot see).
        if !verified.peek().contains(&kept.key()) || !verified.peek().contains(&dropped.key())
            || !rules::style_matches(&kept,*style.peek()) || !rules::style_matches(&dropped,*style.peek()) { return; }
        let mut t = taste.peek().clone();
        let mut e = evidence.peek().clone();
        let candidates = filtered.peek().clone();
        let (suggested, before) = e.choose(&mut t, &kept, &dropped, &candidates, *pool_complete.peek() && !*enriching.peek());
        backend::save_taste(&t);
        taste.set(t.clone());
        let done = (*round.peek()).saturating_add(1);
        round.set(done);
        crate::picks_decision::audit("keep", Some(&kept), Some(&dropped), &e, before, candidates.len());
        evidence.set(e);
        if suggested { winner.set(Some(kept)); return; }
        let mut exclude = shown.peek().clone();
        let challenger = backend::challenger_against(&filtered.peek(), &t, &exclude, &kept, done);
        if let Some(c) = &challenger { exclude.push(c.key()); }
        shown.set(exclude);
        match challenger {
            Some(c) => pair.set(if left { (Some(kept), Some(c)) } else { (Some(c), Some(kept)) }),
            // A background catalogue fetch can still provide the next opponent. Do not
            // turn a temporarily small pool into an early result.
            None => pair.set(if left { (Some(kept), None) } else { (None, Some(kept)) }),
        }
    };
    // Keeping is seen before it happens: the kept card steps forward, the other steps back
    // (style.css, .keep-left/.keep-right), then the duel moves on. Presses meanwhile are ignored.
    let mut leaving = use_signal(|| None::<bool>);
    let mut choose = move |left: bool| {
        if leaving.peek().is_some() { return; }
        let ready = { let (l, r) = resolved_pair.peek().clone();
            l.zip(r).is_some_and(|(l, r)| verified.peek().contains(&l.key()) && verified.peek().contains(&r.key())) };
        if !ready { return; }
        leaving.set(Some(left));
        spawn(async move { backend::sleep(280).await; keep(left); leaving.set(None); });
    };

    // A kept title waiting for the catalogue to catch up retains all duel evidence.
    use_effect(move || {
        let list = filtered();
        let complete = pool_complete();
        let growing = enriching();
        let (l,r) = pair();
        if loading() || winner().is_some() || round() == 0 { return; }
        let (kept,left) = match (l,r) { (Some(m),None) => (m,true), (None,Some(m)) => (m,false), _ => return };
        let mut exclude = shown.peek().clone();
        if let Some(c) = backend::challenger_against(&list, &taste.peek(), &exclude, &kept, *round.peek()) {
            exclude.push(c.key()); shown.set(exclude);
            pair.set(if left { (Some(kept),Some(c)) } else { (Some(c),Some(kept)) });
        } else if complete && !growing {
            evidence.write().reason = "exhausted".into();
            crate::picks_decision::audit("exhausted", Some(&kept), None, &evidence.peek(), None, list.len());
            winner.set(Some(kept));
        }
    });

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
                    Check::Ready(ready) => { pool.with_mut(|p| rules::merge(p, vec![ready])); verified.write().insert(m.key()); return; }
                    Check::Reject => { rejected.write().insert(m.key()); }
                    Check::Instead(first) => { rejected.write().insert(m.key()); pool.with_mut(|p| rules::merge(p, vec![first])); }
                }
                let (l, r) = pair.peek().clone();
                let on_screen = if left { l } else { r };
                if on_screen.map(|x| x.key()) != Some(m.key()) { return; }
                let mut exclude = shown.peek().clone();
                let t = taste.peek().clone();
                // Nothing else to offer: show it anyway rather than an empty side.
                let opponent = if left { pair.peek().1.clone() } else { pair.peek().0.clone() };
                let challenger = match opponent {
                    Some(opponent) => backend::challenger_against(&filtered.peek(), &t, &exclude, &opponent, *round.peek()),
                    None => backend::next_challenger(&prefer(&filtered.peek(), &exclude), &t, &exclude, 3),
                };
                let Some(c) = challenger else { verified.write().insert(m.key()); return };
                exclude.push(c.key());
                shown.set(exclude);
                pair.with_mut(|p| if left { p.0 = Some(c) } else { p.1 = Some(c) });
            });
        }
    });

    // Skipping is not a dislike. Neither clears tonight's direction, leaving lasting taste intact.
    let mut change_pair = move |neither: bool| {
        if winner.peek().is_some() || loading() { return; }
        let (left, right) = pair.peek().clone();
        if left.iter().chain(right.iter()).any(|m| !verified.peek().contains(&m.key())) { return; }
        if neither { let mut t = taste.write(); t.session.clear(); t.session_vibe.clear(); }
        let t = taste.peek().clone();
        let list = filtered.peek().clone();
        let mut exclude = shown.peek().clone();
        let current: Vec<String> = left.iter().chain(right.iter()).map(Media::key).collect();
        let alternative: Vec<Media> = if neither {
            list.iter().filter(|m| !left.iter().chain(right.iter()).any(|old| old.genres.iter().any(|g| m.genres.contains(g)))).cloned().collect()
        } else { list.clone() };
        let first = backend::next_challenger(&alternative, &t, &exclude, 12)
            .or_else(|| backend::next_challenger(&list, &t, &exclude, 12))
            .or_else(|| backend::next_challenger(&list, &t, &current, 12));
        // All other titles exhausted: leave the visible pair available rather than replaying it as a vote.
        let Some(first) = first else { return };
        if !exclude.contains(&first.key()) { exclude.push(first.key()); }
        let second = backend::next_challenger(&alternative, &t, &exclude, 12)
            .or_else(|| backend::next_challenger(&list, &t, &exclude, 12))
            .or_else(|| { let mut avoid = current; avoid.push(first.key()); backend::next_challenger(&list, &t, &avoid, 12) });
        if let Some(m) = &second { if !exclude.contains(&m.key()) { exclude.push(m.key()); } }
        shown.set(exclude);
        // A skipped comparison supplies no stability evidence.
        let choices = evidence.peek().choices;
        evidence.set(crate::picks_decision::Evidence { choices, ..Default::default() });
        crate::picks_decision::audit(if neither { "neither" } else { "skip" }, None, None, &evidence.peek(), None, list.len());
        match second {
            Some(second) => pair.set((Some(first), Some(second))),
            None => { evidence.write().reason = "exhausted".into(); winner.set(Some(first)); }
        }
    };
    let mut continue_duel = move || {
        let Some(w) = winner.peek().clone() else { return };
        let mut exclude = shown.peek().clone();
        let challenger = backend::challenger_against(&filtered.peek(), &taste.peek(), &exclude, &w, 0);
        let Some(c) = challenger else {
            if !*pool_complete.peek() || *enriching.peek() {
                evidence.write().resume();
                crate::picks_decision::audit("continue_wait", Some(&w), None, &evidence.peek(), None, filtered.peek().len());
                pair.set((Some(w),None)); winner.set(None);
                return;
            }
            evidence.write().reason = "exhausted".into();
            crate::picks_decision::audit("continue_exhausted", Some(&w), None, &evidence.peek(), None, filtered.peek().len());
            return;
        };
        exclude.push(c.key());
        shown.set(exclude);
        evidence.write().resume();
        crate::picks_decision::audit("continue", Some(&w), None, &evidence.peek(), None, filtered.peek().len());
        pair.set((Some(w), Some(c)));
        winner.set(None);
    };
    use_effect(move || {
        let Some(command) = (shell.pick_command)() else { return };
        shell.pick_command.set(None);
        if slot.peek().is_some() { return; }
        match command {
            PickCommand::Left if winner.peek().is_none() => choose(true),
            PickCommand::Right if winner.peek().is_none() => choose(false),
            PickCommand::Skip => change_pair(false),
            PickCommand::Neither => change_pair(true),
            PickCommand::Continue => continue_duel(),
            _ => {}
        }
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
    let pair_ready = { let (l,r) = pair(); l.as_ref().zip(r.as_ref()).is_some_and(|(l,r)| verified.read().contains(&l.key()) && verified.read().contains(&r.key())) };
    let style_options: [(Style, &'static str); 3] = [(Style::Live, "Live action"), (Style::Animation, "Animation"), (Style::Both, "Live action and animation")];

    rsx! {
        div { class: "duel",
            div { class: "duel-bar",
                div { class: "duel-progress",
                    if winner().is_some() { span { class: "display duel-round", "Your pick" } }
                    if winner().is_none() {
                        icons::IconButton { label: "Change pair without voting (S)", class: "glass small", disabled: loading() || !pair_ready, onclick: move |_| change_pair(false), icons::Refresh {} }
                        icons::IconButton { label: "Neither: change direction (N)", class: "glass small", disabled: loading() || !pair_ready, onclick: move |_| change_pair(true), icons::Close {} }
                    }
                }
                div { class: "chips",
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Type",
                        i { class: "slider", aria_hidden: "true" }
                        for (value, label) in type_options {
                            button { class: if kind().as_deref() == value { "seg on" } else { "seg" }, role: "radio", aria_checked: "{kind().as_deref() == value}",
                                aria_label: "{label}", title: "{label}", onclick: move |_| kind.set(value.map(str::to_owned)),
                                match value { None => rsx! { icons::Grid {} }, Some("movie") => rsx! { icons::Film {} }, _ => rsx! { icons::Tv {} } }
                            }
                        }
                    }
                    // Style beside type: animated films and series are still films and series.
                    div { class: "segmented glass", role: "radiogroup", aria_label: "Style",
                        i { class: "slider", aria_hidden: "true" }
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
                        aria_label: "Hide completed titles", title: if hide_watched() { "Hiding completed titles; unfinished series stay available" } else { "Showing completed titles too" },
                        onclick: move |_| {
                            let on = !hide_watched();
                            backend::save_settings(&backend::Settings { picks_hide_watched: on, ..backend::load_settings() });
                            hide_watched.set(on);
                        },
                        icons::EyeOff {} }
                    StartOver { on_reset: move |_| { backend::reset_taste(); taste.set(Taste::default()); start_duel(); } }
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

            if !verified.read().is_empty() && crate::picks_ai::failed() {
                p { class: "picks-ai-status", role: "status", "Local AI unavailable. Using catalogue preferences." }
            }

            if loading() || (hide_watched() && watched.read().is_none())
                || (winner().is_none() && ((pair().0.is_some() != pair().1.is_some())
                    || ((!pool_complete() || enriching()) && pair().0.is_none()))) {
                div { class: "duel-cards", div { class: "duel-card skeleton-card" } div { class: "duel-card skeleton-card" } }
            } else if let Some(pick) = winner() {
                for pick in [pick.clone()] { FinalPick { key: "{pick.key()}", media: pick.clone(),
                    reason: match evidence().reason.as_str() {
                        "pause" => "Your choice so far. Keep exploring if it’s not quite right.",
                        "stable" => "Your choices keep pointing here.",
                        "exhausted" => "No new challengers in these filters.",
                        _ if round() == 0 => "The only match for these filters.",
                        _ => "Your choice from the titles explored.",
                    }.to_owned(),
                    on_continue: move |_| continue_duel(),
                    on_reset: move |_| { backend::reset_taste(); taste.set(Taste::default()); start_duel(); } } }
            } else {
                match resolved_pair() {
                    (Some(l), Some(r)) => rsx! {
                        div { class: match leaving() { Some(true) => "duel-cards keep-left", Some(false) => "duel-cards keep-right", None => "duel-cards" },
                            // A key only renews a component inside a list: a new title must get a new card.
                            // A card shows once its poster and trailer are confirmed.
                            if verified.read().contains(&l.key()) && rules::style_matches(&l,style()) {
                                for m in [l.clone()] { DuelCard { key: "{m.key()}", media: m.clone(), side: "left", on_keep: move |_| choose(true) } }
                            } else { div { class: "duel-card skeleton-card" } }
                            span { class: "versus display", aria_hidden: "true", "or" }
                            if verified.read().contains(&r.key()) && rules::style_matches(&r,style()) {
                                for m in [r.clone()] { DuelCard { key: "{m.key()}", media: m.clone(), side: "right", on_keep: move |_| choose(false) } }
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
                    button { class: "pill accent", title: "Keep this ({arrow})", onclick: move |e| { e.stop_propagation(); on_keep.call(()) }, icons::Check {} span { "Keep this" } kbd { class: "keycap small-cap", "{arrow}" } }
                    CardPlayButton { media: media.clone(), episodes: facts.as_ref().map(|f| f.episodes.clone()).unwrap_or_default() }
                    if let Some(t) = trailer { icons::IconButton { label: "Trailer", class: "glass", onclick: move |_| crate::play_trailers(shell, t.clone()), icons::Clapper {} } }
                    icons::IconButton { label: "Details", class: "glass", onclick: move |_| { let mut s = shell; s.opened.set(Some(open.clone())) }, icons::Info {} }
                }
            }
        }
    }
}

/// One playback control for both duel cards and the final pick. Fresh stored progress wins
/// over catalogue metadata; clicking it never casts a duel vote or changes the selected card.
#[component]
fn CardPlayButton(media: Media, episodes: Vec<backend::Episode>, #[props(default)] accent: bool) -> Element {
    let shell = use_context::<Shell>();
    let progress_media = media.clone();
    let progress = use_resource(move || {
        let _ = (shell.picks_dirty)();
        let m = progress_media.clone();
        async move {
            let stored = backend::stored(&m).await.unwrap_or(m);
            let seen = if stored.kind == "movie" { HashSet::new() } else { backend::watched_episodes(&stored).await };
            (stored,seen)
        }
    });
    let target = progress.read().as_ref().and_then(|(m,seen)| rules::card_playback(m,&episodes,seen,backend::now_secs()));
    let label = target.as_ref().map(|t| t.label.clone()).unwrap_or_else(|| "Play".into());
    let media = progress.read().as_ref().map(|(m,_)| m.clone()).unwrap_or(media);
    rsx! {
        button { class: if accent { "pill accent play-big" } else { "pill glass" }, disabled: target.is_none(),
            title: "Play or resume this title", onclick: move |e| { e.stop_propagation();
                if let Some(target) = &target { crate::title::play_from_card(shell,media.clone(),target.video.clone()); }
            }, icons::Play {} span { "{label}" }
        }
    }
}

/// The winner, with why, and the ways on: watch it, or keep refining, or start over.
#[component]
fn FinalPick(media: Media, reason: String, on_continue: EventHandler<()>, on_reset: EventHandler<()>) -> Element {
    let mut shell = use_context::<Shell>();
    let mut meta = use_signal(|| None::<TitleMeta>);
    let m = media.clone();
    use_hook(move || { spawn(async move { if let Ok(t) = backend::title_meta(&m).await { meta.set(Some(t)); } }); });
    let art = crate::home::use_artwork(&media);
    let trailer = meta().map(|f| f.trailers).filter(|t| !t.is_empty())
        .or_else(|| art().map(|a| a.trailers.into_iter().map(|k| backend::Trailer { youtube: Some(k), url: None }).collect::<Vec<_>>()).filter(|t| !t.is_empty()));
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    let (play, open) = (media.clone(), media.clone());
    rsx! {
        article { class: "final-pick", aria_label: "Your pick: {media.name}",
            if !backdrop.is_empty() { img { class: "final-art", src: "{backdrop}", alt: "" } }
            div { class: "final-copy",
                p { class: "reason", span { class: "lamp", icons::Sparkles {} } span { "{reason}" } }
                h1 { class: "display pick-title", "{media.name}" }
                p { class: "meta", "{media.meta_line(true)}" if !media.genres.is_empty() { span { class: "dim", ", {media.genres.iter().take(3).cloned().collect::<Vec<_>>().join(\", \")}" } } }
                if !media.description.is_empty() { p { class: "synopsis clamp-3", "{media.description}" } }
                div { class: "title-actions",
                    CardPlayButton { media: play.clone(), episodes: meta().map(|m| m.episodes).unwrap_or_default(), accent: true }
                    if let Some(t) = trailer { icons::IconButton { label: "Trailer", class: "glass", onclick: move |_| crate::play_trailers(shell, t.clone()), icons::Clapper {} } }
                    icons::IconButton { label: "Details", class: "glass", onclick: move |_| shell.opened.set(Some(open.clone())), icons::Info {} }
                    icons::IconButton { label: "Keep exploring (C)", class: "glass", onclick: move |_| on_continue.call(()), icons::ArrowRight {} }
                    StartOver { on_reset: move |_| on_reset.call(()) }
                }
            }
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
            span { class: "poster-meta", "{media.meta_line(false)}" }
        }
    }
}

/// What a candidate check found.
enum Check { Ready(Media), Reject, Instead(Media) }

/// A title can stand in a duel with a poster and a trailer; a later film in a collection gives
/// way to the first part you have not seen.
async fn check(m: &Media, watched: &HashSet<String>) -> Check {
    let Some(ready) = backend::duel_ready(m).await else { return Check::Reject };
    match backend::earlier_part(&ready, watched).await {
        Some(first) => Check::Instead(first),
        None => { let _ = crate::picks_ai::embed(&ready).await; Check::Ready(ready) }
    }
}
