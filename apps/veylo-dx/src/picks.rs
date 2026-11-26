//! Picks: an endless duel with the brain (brain.rs) at its centre. Two cards; keep the one you'd
//! rather watch tonight. Every vote, Neither, play, like and mood teaches one local model, and the
//! next card is the favourite of one plausible version of your taste. A card that keeps winning
//! joins tonight's shortlist. There is no final answer: play anything, any time.
use crate::{backend::{self, Media}, brain::{self, Brain}, icons, picks_rules::{self as rules, Style}, PickCommand, Shell};
use dioxus::prelude::*;
use std::collections::HashSet;

/// Wins in a row that take a card onto the shortlist (and make room for new titles).
const GRADUATE: u32 = 4;
/// Idle time after which Picks starts a new night (tonight's moods and votes are set aside).
const NIGHT_GAP: i64 = 6 * 3600;
/// Extra catalogue pages fetched in one night once these filters run low on unseen titles.
const MAX_GROWTH: u32 = 6;

#[derive(Clone, Copy, PartialEq)]
enum Slot { Filters, Mood }

fn year_span(from: Option<i32>, to: Option<i32>) -> Option<String> {
    match (from, to) {
        (Some(a), Some(b)) => Some(format!("{a}–{b}")),
        (Some(a), None) => Some(format!("{a}–")),
        (None, Some(b)) => Some(format!("–{b}")),
        (None, None) => None,
    }
}
fn new_night_id() -> u64 { backend::rand_u64() | 1 }
fn has(list: &[Media], m: &Media) -> bool { list.iter().any(|x| x.key() == m.key()) }
/// A title you finished, under any of its ids (`backend::watched_ids`).
fn finished(seen: &HashSet<String>, m: &Media) -> bool {
    seen.contains(&m.id) || seen.contains(&rules::title_key(m)) || m.raw["imdb_id"].as_str().is_some_and(|i| seen.contains(i))
}
/// Candidate checks (poster, trailer, embedding) running at once.
const CHECKS_AT_ONCE: usize = 12;
/// How long a duel card stays on screen before its sources are asked for (a card skipped at once costs nothing).
const PREFETCH_AFTER_MS: u64 = 2500;

#[component]
pub fn Picks() -> Element {
    let mut shell = use_context::<Shell>();
    // Tonight as you left it, unless the night is over: then only the filters carry on.
    let (start, same_night) = use_hook(|| {
        let saved = backend::load_duel().unwrap_or_default();
        let same = saved.night != 0 && backend::now_secs() - saved.last < NIGHT_GAP;
        let start = if same { saved } else {
            backend::DuelState { kind: saved.kind, style: saved.style, genres: saved.genres, from: saved.from, to: saved.to, night: new_night_id(), ..Default::default() }
        };
        (start, same)
    });
    let mut pool = use_signal(Vec::<Media>::new);
    let mut loading = use_signal(|| true);
    let mut pool_complete = use_signal(|| false);
    let mut night = use_signal(|| start.night);
    let mut restored = use_signal(|| same_night && (start.left.is_some() || start.right.is_some()));
    let mut round = use_signal(|| start.round);
    let mut streak = use_signal(|| start.streak);
    let mut pair = use_signal(|| (start.left.clone(), start.right.clone()));
    let mut shown = use_signal(|| start.shown.clone());
    let mut kept = use_signal(|| start.kept.clone());
    let mut starred = use_signal(|| start.starred.clone());
    let mut kind = use_signal(|| start.kind.clone());
    let mut style = use_signal(|| Style::parse(&start.style));
    let mut selected_genres = use_signal(|| start.genres.clone());
    let mut from_text = use_signal(|| start.from.clone());
    let mut to_text = use_signal(|| start.to.clone());
    let mut hide_watched = use_signal(|| backend::load_settings().picks_hide_watched);
    let watched = use_resource(move || { let _ = (shell.picks_dirty)(); async { backend::watched_ids().await } });
    let library = use_resource(move || { let _ = (shell.picks_dirty)(); async { backend::library().await.unwrap_or_default() } });
    let mut slot = use_signal(|| None::<Slot>);
    let mut arm_night = use_signal(|| false);
    let mut toast = use_signal(|| None::<(u64, String)>);
    let mut mood_text = use_signal(String::new);
    let mut mood_busy = use_signal(|| false);

    // The brain: loaded off the UI thread, then its projection and mood words as they come.
    let mut brain = use_signal(Brain::default);
    let mut brain_ready = use_signal(|| false);
    use_future(move || async move {
        let n = *night.peek();
        let loaded = backend::on_runtime(async move { tokio::task::spawn_blocking(move || brain::load(n)).await.ok() }).await.ok().flatten();
        let mut b = loaded.unwrap_or_default();
        b.set_session(n);
        brain.set(b);
        brain_ready.set(true);
        if !crate::picks_ai::failed() {
            let anchors = brain::anchors().await;
            if !anchors.is_empty() { brain.write().set_anchors(anchors); }
        }
    });

    // The room takes its light from tonight's champion while it is on screen, else the leading
    // card in front of you so Picks never freezes Home's old backdrop or drops to flat black.
    let mut lamp = shell.lamp_source;
    use_effect(move || {
        let champion = kept.read().first().cloned().filter(|_| *streak.read() > 0);
        let (l, r) = pair();
        let on_screen = champion
            .filter(|c| [l.as_ref(), r.as_ref()].into_iter().flatten().any(|m| m.key() == c.key()))
            .or(l)
            .or(r);
        let Some(c) = on_screen else { if !lamp.peek().is_empty() { lamp.set(String::new()); } return };
        let key = c.key();
        if let Some(src) = backend::cached_artwork(&c).and_then(|a| a.backdrop) {
            if *lamp.peek() != src { lamp.set(src); }
            return;
        }
        spawn(async move {
            let src = backend::artwork(&c).await.and_then(|a| a.backdrop).unwrap_or_else(|| c.backdrop().to_owned());
            let still_on = {
                let (l, r) = pair.peek().clone();
                [l, r].iter().flatten().any(|m| m.key() == key)
            };
            if still_on && *lamp.peek() != src { lamp.set(src); }
        });
    });

    // Candidates: your ranked recommendations first, then the wider catalogue in the background:
    // random pages of popular, well-rated titles (TMDB, else Cinemeta) and a TVmaze sample, so every visit brings
    // new titles. Titles you keep pull in more like them as you go (expand, below).
    use_future(move || async move {
        let (picks_res, history_res) = tokio::join!(backend::picks(), backend::viewing_history());
        let mut items = picks_res.map(|p| p.items).unwrap_or_default();
        if items.len() < 6 { items.extend(backend::seed_catalogs().await); }
        // Core discovery omits started titles. Add history back to the pool so unfinished
        // series can actually appear with Resume; Hide watched below removes completed ones.
        items.extend(history_res.unwrap_or_default().into_iter().map(|s| s.media));
        let mut first = Vec::new();
        rules::merge(&mut first, items);
        pool.set(first);
        loading.set(false);
        let pages: Vec<u32> = (0..2).map(|_| 1 + (backend::rand_u64() % 25) as u32).collect();
        let (mut more, tvmaze) = tokio::join!(
            backend::discover_candidates(Vec::new(), None, Style::Both, None, None, pages),
            backend::tvmaze_candidates(Vec::new(), Style::Both, None, None),
        );
        more.extend(tvmaze);
        pool.with_mut(|p| rules::merge(p, more));
        pool_complete.set(true);
    });

    // The library teaches lasting taste: likes and dislikes, finishes, saves, long watches. Its
    // liked and finished titles are embedded once so their story counts, not just their genres.
    let mut facts_tick = use_signal(|| 0u32);
    use_effect(move || {
        let _ = facts_tick();
        let Some(items) = library.read().clone() else { return };
        if !brain_ready() { return; }
        let facts = brain::facts(&items);
        let mut b = brain.write();
        b.set_facts(facts);
        brain::persist(&mut b, None);
    });
    let mut library_embedded = use_signal(|| false);
    use_effect(move || {
        let Some(items) = library.read().clone() else { return };
        if *library_embedded.peek() { return; }
        library_embedded.set(true);
        let todo: Vec<Media> = items.into_iter().filter(|m| (m.liked != 0 || m.watched) && crate::picks_ai::embedding(m).is_none()).take(40).collect();
        if todo.is_empty() { return; }
        spawn(async move {
            for m in todo { if crate::picks_ai::failed() { break; } let _ = crate::picks_ai::enrich_and_embed(&m).await; }
            facts_tick += 1;
        });
    });
    let disliked = use_memo(move || library.read().as_ref().map(|l| l.iter().filter(|m| m.liked < 0).map(brain::id).collect::<HashSet<_>>()).unwrap_or_default());
    let liked_ids = use_memo(move || library.read().as_ref().map(|l| l.iter().filter(|m| m.liked > 0).map(brain::id).collect::<HashSet<_>>()).unwrap_or_default());
    let saved_ids = use_memo(move || library.read().as_ref().map(|l| l.iter().filter(|m| m.saved).map(brain::id).collect::<HashSet<_>>()).unwrap_or_default());

    let from_raw = use_memo(move || from_text().parse::<i32>().ok().filter(|_| from_text().len() == 4));
    let to_raw = use_memo(move || to_text().parse::<i32>().ok().filter(|_| to_text().len() == 4));
    let years_invalid = use_memo(move || from_raw().zip(to_raw()).is_some_and(|(a, b)| a > b));
    let from = use_memo(move || if years_invalid() { None } else { from_raw() });
    let to = use_memo(move || if years_invalid() { None } else { to_raw() });
    // Titles checked for a poster and a trailer: only ready ones are drawn when enough are, and a
    // title found lacking either is set aside for the session.
    let mut verified = use_signal(HashSet::<String>::new);
    let mut rejected = use_signal(HashSet::<String>::new);
    let mut checking = use_signal(HashSet::<String>::new);
    let filtered = use_memo(move || {
        let saved_only = (shell.picks_saved_only)();
        if saved_only {
            let lib = library.read().clone().unwrap_or_default();
            let gone = rejected.read();
            let not_for_me = disliked.read();
            let seen = if hide_watched() { watched.read().clone().unwrap_or_default() } else { HashSet::new() };
            return lib.into_iter()
                .filter(|m| m.saved && !gone.contains(&m.key()) && !finished(&seen, m) && !not_for_me.contains(&brain::id(m)))
                .collect::<Vec<_>>();
        }
        let this_year = backend::civil(backend::now_secs()).0 as i32;
        let out = rules::filter_picks(&pool.read(), kind().as_deref(), style(), &selected_genres(), from(), to(), this_year);
        let gone = rejected.read();
        let not_for_me = disliked.read();
        let seen = if hide_watched() { watched.read().clone().unwrap_or_default() } else { HashSet::new() };
        out.into_iter().filter(|m| !gone.contains(&m.key()) && !finished(&seen, m) && !not_for_me.contains(&brain::id(m))).collect::<Vec<_>>()
    });
    let resolved_pair = use_memo(move || {
        let list = pool.read();
        let resolve = |m: Option<Media>| m.map(|m| list.iter().find(|x| x.key() == m.key() || rules::title_key(x) == rules::title_key(&m)).cloned().unwrap_or(m));
        let (l, r) = pair(); (resolve(l), resolve(r))
    });
    // Embed beyond the first trailer-ready titles, so a matching title deeper in the catalogue
    // can challenge one that happened to finish verification first.
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
    // The projection follows the catalogue: fitted once enough titles are embedded, refitted as
    // it grows (the brain replays everything into the new space).
    let mut space_bucket = use_signal(|| 0usize);
    use_effect(move || {
        let n = verified.read().len();
        if !brain_ready() { return; }
        let bucket = n / 40 + 1;
        if *space_bucket.peek() >= bucket { return; }
        space_bucket.set(bucket);
        let (current, items) = { let b = brain.peek(); (b.space.clone(), b.item_embeddings()) };
        spawn(async move {
            if let Some(space) = brain::refresh_space(current, items).await {
                let list = pool.peek().clone();
                let mut b = brain.write();
                b.set_space(Some(space));
                b.refresh_items(&list);
                brain::persist(&mut b, None);
            }
        });
    });
    // The verified part of a list, when it still offers a choice beyond `exclude`.
    let prefer = move |list: &[Media], exclude: &[String]| -> Vec<Media> {
        let ok = verified.peek();
        let skip: HashSet<&str> = exclude.iter().map(String::as_str).collect();
        let ready: Vec<Media> = list.iter().filter(|m| ok.contains(&m.key())).cloned().collect();
        if ready.iter().filter(|m| !brain::excluded(m, &skip)).count() >= 2 { ready } else { list.to_vec() }
    };
    // Check the candidates ahead of the duel, at most a dozen at a time: a finished check frees a
    // place (the effect follows `checking`), it doesn't add a dozen more.
    // In Saved-only mode, saved titles are always eligible without requiring a trailer or swapping sequels.
    use_effect(move || {
        let saved_only = (shell.picks_saved_only)();
        if saved_only {
            let list = filtered.read().clone();
            let missing: Vec<Media> = {
                let ok = verified.peek();
                list.into_iter().filter(|m| !ok.contains(&m.key())).collect()
            };
            if !missing.is_empty() {
                verified.with_mut(|v| for m in &missing { v.insert(m.key()); });
                spawn(async move {
                    for m in missing.into_iter().take(24) {
                        if crate::picks_ai::failed() { break; }
                        let _ = crate::picks_ai::embed(&m).await;
                    }
                });
            }
            return;
        }
        let in_flight = checking.read().len();
        let room = CHECKS_AT_ONCE.saturating_sub(in_flight);
        if room == 0 { return; }
        let list = filtered.read().clone();
        let todo: Vec<Media> = {
            let (ok, busy) = (verified.peek(), checking.peek());
            list.into_iter().filter(|m| !ok.contains(&m.key()) && !busy.contains(&m.key())).take(room).collect()
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
    let mut fetched = use_signal(HashSet::<String>::new);
    let catalog_genres = use_resource(|| async { backend::catalog_genres().await });
    let mut enriching = use_signal(|| false);
    // Few candidates for these filters: ask the catalogs that declare each chosen genre, then TMDB
    // Discover (any genre combo, type and years; Cinemeta's lists without a key), each once per filter combination.
    use_effect(move || {
        let (gs, k, st, f, t) = (selected_genres(), kind(), style(), from(), to());
        if gs.is_empty() && k.is_none() && st == Style::Both && f.is_none() && t.is_none() { return; }
        let mut sorted = gs.clone();
        sorted.sort();
        let combo = format!("{sorted:?}|{k:?}|{st:?}|{f:?}|{t:?}");
        if filtered.read().len() >= 8 || fetched.peek().contains(&combo) { return; }
        fetched.write().insert(combo);
        enriching.set(true);
        spawn(async move {
            let mut more = Vec::new();
            for g in &gs { more.extend(backend::genre_candidates(g).await); }
            more.extend(backend::discover_candidates(gs.clone(), k.clone(), st, f, t, vec![1, 2]).await);
            if k.as_deref() != Some("movie") { more.extend(backend::tvmaze_candidates(gs, st, f, t).await); }
            pool.with_mut(|p| rules::merge(p, more));
            enriching.set(false);
        });
    });
    // An endless duel needs fresh titles: when few unseen ones are left for these filters, two
    // more random catalogue pages (a few times a night).
    let mut growth = use_signal(|| 0u32);
    use_effect(move || {
        let list = filtered.read();
        let seen = shown.read();
        let unseen = list.iter().filter(|m| !seen.contains(&m.key())).count();
        if loading() || !pool_complete() || enriching() || unseen >= 8 || *growth.peek() >= MAX_GROWTH { return; }
        *growth.write() += 1;
        enriching.set(true);
        let (gs, k, st, f, t) = (selected_genres(), kind(), style(), from(), to());
        spawn(async move {
            let pages: Vec<u32> = (0..2).map(|_| 1 + (backend::rand_u64() % 30) as u32).collect();
            let mut more = backend::discover_candidates(gs.clone(), k.clone(), st, f, t, pages).await;
            if k.as_deref() != Some("movie") { more.extend(backend::tvmaze_candidates(gs, st, f, t).await); }
            pool.with_mut(|p| rules::merge(p, more));
            enriching.set(false);
        });
    });

    let mut notify = move |text: String| {
        let id = backend::rand_u64();
        toast.set(Some((id, text)));
        spawn(async move { backend::sleep(2800).await; if toast.peek().as_ref().map(|t| t.0) == Some(id) { toast.set(None); } });
    };
    // Titles like one you kept, liked or played (TMDB's recommendations, else Cinemeta's lists for
    // its genres): the pool grows toward your taste.
    let mut expanded = use_signal(HashSet::<String>::new);
    let mut expand = move |m: Media| {
        let id = brain::id(&m);
        if expanded.peek().contains(&id) { return; }
        expanded.write().insert(id);
        spawn(async move { let more = backend::related(&m).await; if !more.is_empty() { pool.with_mut(|p| rules::merge(p, more)); } });
    };
    // Never offered again in this run as a new card: what was shown, kept or starred, under any
    // of its names (another source can list the same title under another key). Only a reshuffle,
    // once every title for these filters was shown, or a new night brings them back.
    let avoid = move || -> Vec<String> {
        let mut ex = shown.peek().clone();
        ex.extend(kept.peek().iter().chain(starred.peek().iter()).flat_map(brain::identities));
        ex
    };
    let shortlisted = move || -> Vec<String> { kept.peek().iter().chain(starred.peek().iter()).flat_map(brain::identities).collect() };
    let mut mark_shown = move |m: &Option<Media>| {
        let Some(m) = m else { return };
        let mut list = shown.write();
        for name in brain::identities(m) { if !list.contains(&name) { list.push(name); } }
    };

    // Two fresh cards, each the favourite of one plausible taste. When every title for these
    // filters was shown tonight, another lap begins (the shortlist stays out of it).
    let mut start_duel = move || {
        let list = filtered.peek().clone();
        let ex = avoid();
        let mut next = brain.peek().opening_pair(&prefer(&list, &ex), &ex);
        if next.1.is_none() && *pool_complete.peek() && !*enriching.peek() {
            let lap = shortlisted();
            let again = brain.peek().opening_pair(&prefer(&list, &lap), &lap);
            if again.1.is_some() || next.0.is_none() {
                if !shown.peek().is_empty() { notify("You've seen every title for these filters: reshuffling".into()); }
                shown.set(Vec::new());
                next = again;
            }
        }
        mark_shown(&next.0);
        mark_shown(&next.1);
        streak.set(0);
        pair.set(next);
    };
    // A new duel when a filter or Saved-only mode changes; a restored catalog duel is kept only
    // when entering normal mode. Wait for `watched` when Hide watched is on so completed titles never flash.
    use_effect(move || {
        let saved_only = (shell.picks_saved_only)();
        let hide = hide_watched();
        let watched_ready = !hide || watched.read().is_some();
        let _ = (kind(), style(), selected_genres(), from(), to(), hide, saved_only);
        if saved_only && *slot.peek() == Some(Slot::Filters) { slot.set(None); }
        if loading() || !brain_ready() || !watched_ready { return; }
        if *restored.peek() {
            restored.set(false);
            if !saved_only { return; }
        }
        start_duel();
    });
    // Progress can change while the player or title page is open. Cards on screen must obey
    // Hide watched too, not just newly drawn ones.
    use_effect(move || {
        if !hide_watched() || loading() || !brain_ready() { return; }
        let Some(seen) = watched.read().clone() else { return };
        let (left, right) = pair.peek().clone();
        if left.iter().chain(right.iter()).any(|m| finished(&seen, m)) { start_duel(); }
    });
    // Remember tonight as it goes (never overwrite the catalog duel while in Saved-only mode).
    use_effect(move || {
        if (shell.picks_saved_only)() { return; }
        let (left, right) = pair();
        backend::save_duel(&backend::DuelState { kind: kind(), style: style().as_str().to_owned(), genres: selected_genres(), from: from_text(), to: to_text(),
            night: night(), last: backend::now_secs(), round: round(), streak: streak(), shown: shown(), left, right, kept: kept(), starred: starred() });
    });
    // The first pair, once there is something to choose from (and watched ids are ready if hiding watched).
    use_effect(move || {
        let n = filtered.read().len();
        let watched_ready = !hide_watched() || watched.read().is_some();
        let (l, r) = pair.peek().clone();
        if !loading() && brain_ready() && watched_ready && n >= 1 && l.is_none() && r.is_none() { start_duel(); }
    });
    // Style is an invariant of visible catalog cards, including restored cards enriched later.
    use_effect(move || {
        if (shell.picks_saved_only)() { return; }
        let (l, r) = resolved_pair();
        let st = style();
        if !brain_ready() || !l.iter().chain(r.iter()).any(|m| !rules::style_matches(m, st)) { return; }
        let list = filtered.peek().clone();
        let mut ex = avoid();
        let mut replace = |m: Option<Media>, other: &Option<Media>| {
            if m.as_ref().is_some_and(|m| rules::style_matches(m, st)) { return m; }
            let next = match other { Some(o) => brain.peek().challenger(&list, &ex, o), None => brain.peek().opening_pair(&list, &ex).0 };
            if let Some(n) = &next { ex.extend(brain::identities(n)); }
            next
        };
        let left = replace(l, &r);
        let right = replace(r, &left);
        mark_shown(&left);
        mark_shown(&right);
        pair.set((left, right));
    });

    // Keep one side: the brain learns, then the card you kept meets its strongest plausible
    // rival. A card that keeps winning joins the shortlist and makes room for two new titles.
    let mut keep = move |left: bool| {
        let (l, r) = resolved_pair.peek().clone();
        let (Some(won), Some(lost)) = (if left { (l.clone(), r.clone()) } else { (r.clone(), l.clone()) }) else { return };
        let saved_only = *shell.picks_saved_only.peek();
        if !verified.peek().contains(&won.key()) || !verified.peek().contains(&lost.key())
            || (!saved_only && (!rules::style_matches(&won, *style.peek()) || !rules::style_matches(&lost, *style.peek()))) { return; }
        { let mut b = brain.write(); let e = b.keep(&won, &lost); brain::persist(&mut b, Some(&e)); }
        *round.write() += 1;
        let wins = if kept.peek().first().is_some_and(|k| k.key() == won.key()) { *streak.peek() + 1 } else { 1 };
        kept.with_mut(|k| { k.retain(|x| x.key() != won.key()); k.insert(0, won.clone()); k.truncate(40); });
        if wins == 2 { expand(won.clone()); }
        if wins >= GRADUATE {
            starred.with_mut(|s| if !has(s, &won) { s.insert(0, won.clone()); });
            notify(format!("{} won {GRADUATE} in a row and joins your shortlist", won.name));
            expand(won.clone());
            start_duel();
            return;
        }
        streak.set(wins);
        let ex = avoid();
        let list = filtered.peek().clone();
        let challenger = brain.peek().challenger(&prefer(&list, &ex), &ex, &won);
        mark_shown(&challenger);
        pair.set(if left { (Some(won), challenger) } else { (challenger, Some(won)) });
    };
    // Keeping is seen before it happens: the kept card steps forward, the other steps back
    // (styles/screens.css, .keep-left/.keep-right), then the duel moves on. Presses meanwhile are ignored.
    let mut leaving = use_signal(|| None::<bool>);
    let mut choose = move |left: bool| {
        if leaving.peek().is_some() { return; }
        let saved_only = *shell.picks_saved_only.peek();
        let st = *style.peek();
        let snap = {
            let (l, r) = resolved_pair.peek().clone();
            l.zip(r).filter(|(l, r)| verified.peek().contains(&l.key()) && verified.peek().contains(&r.key())
                && (saved_only || (rules::style_matches(l, st) && rules::style_matches(r, st))))
                .map(|(l, r)| (l.key(), r.key()))
        };
        let Some(expected_keys) = snap else { return };
        let expected_night = *night.peek();
        leaving.set(Some(left));
        spawn(async move {
            backend::sleep(190).await;
            let current_keys = {
                let (l, r) = resolved_pair.peek().clone();
                l.zip(r).map(|(l, r)| (l.key(), r.key()))
            };
            if *night.peek() == expected_night && current_keys == Some(expected_keys) {
                keep(left);
            }
            leaving.set(None);
        });
    };

    // A kept card waiting for an opponent gets one as soon as the catalogue offers it; if every
    // title was shown, another lap.
    use_effect(move || {
        let list = filtered();
        let (complete, growing) = (pool_complete(), enriching());
        let (l, r) = pair();
        if loading() || !brain_ready() { return; }
        let (kept_card, left) = match (l, r) { (Some(m), None) => (m, true), (None, Some(m)) => (m, false), _ => return };
        let ex = avoid();
        let mut challenger = brain.peek().challenger(&prefer(&list, &ex), &ex, &kept_card);
        if challenger.is_none() && complete && !growing {
            let lap = shortlisted();
            challenger = brain.peek().challenger(&list, &lap, &kept_card);
            if challenger.is_some() {
                notify("You've seen every title for these filters: reshuffling".into());
                shown.set(Vec::new());
                mark_shown(&Some(kept_card.clone()));
            }
        }
        let Some(c) = challenger else { return };
        mark_shown(&Some(c.clone()));
        pair.set(if left { (Some(kept_card), Some(c)) } else { (Some(c), Some(kept_card)) });
    });

    // A card on screen that turns out to lack a poster or trailer gives its place to another
    // (not a round). Saved-only cards stay eligible regardless of trailer availability.
    use_effect(move || {
        let (l, r) = pair();
        if (shell.picks_saved_only)() {
            for m in [l, r].into_iter().flatten() {
                if !verified.peek().contains(&m.key()) { verified.write().insert(m.key()); }
            }
            return;
        }
        for (left, m) in [(true, l), (false, r)] {
            let Some(m) = m else { continue };
            if verified.peek().contains(&m.key()) || checking.peek().contains(&m.key()) { continue; }
            checking.write().insert(m.key());
            let all_watched = watched.peek().clone().unwrap_or_default();
            spawn(async move {
                let res = check(&m, &all_watched).await;
                checking.write().remove(&m.key());
                match res {
                    Check::Ready(ready) => { pool.with_mut(|p| rules::merge(p, vec![ready])); verified.write().insert(m.key()); return; }
                    Check::Reject => { rejected.write().insert(m.key()); }
                    Check::Instead(first) => { rejected.write().insert(m.key()); pool.with_mut(|p| rules::merge(p, vec![first])); }
                }
                let (l, r) = pair.peek().clone();
                let on_screen = if left { l.clone() } else { r.clone() };
                if on_screen.map(|x| x.key()) != Some(m.key()) { return; }
                let ex = avoid();
                let list = filtered.peek().clone();
                let opponent = if left { r } else { l };
                let next = match &opponent {
                    Some(o) => brain.peek().challenger(&prefer(&list, &ex), &ex, o),
                    None => brain.peek().opening_pair(&prefer(&list, &ex), &ex).0,
                };
                // Nothing else to offer: show it anyway rather than an empty side.
                let Some(c) = next else { verified.write().insert(m.key()); return };
                mark_shown(&Some(c.clone()));
                pair.with_mut(|p| if left { p.0 = Some(c) } else { p.1 = Some(c) });
            });
        }
    });

    // Skip: two other cards, nothing learned. Neither: both are below what tonight wants (a
    // lesson for tonight only), then two other cards.
    let mut change_pair = move |neither: bool| {
        if loading() || !brain_ready() || leaving.peek().is_some() { return; }
        let (l, r) = resolved_pair.peek().clone();
        if l.iter().chain(r.iter()).any(|m| !verified.peek().contains(&m.key())) { return; }
        if neither { if let (Some(a), Some(b)) = (&l, &r) { let mut br = brain.write(); let e = br.neither(a, b); brain::persist(&mut br, Some(&e)); } }
        start_duel();
    };
    // Playing from Picks is the strongest yes there is.
    let played = move |m: Media| {
        { let mut b = brain.write(); let e = b.play(&m); brain::persist(&mut b, Some(&e)); }
        expand(m);
    };
    // Like and Not for me on a card are the library's own (the same as on the title page). Not
    // for me takes the card away for good; a like also stars it for tonight.
    let rate = move |m: Media, value: i64| {
        spawn(async move {
            if backend::feedback(&m, value).await.is_err() { notify("Couldn't save that. Try again.".into()); return; }
            *shell.picks_dirty.write() += 1;
            if value > 0 {
                starred.with_mut(|s| if !has(s, &m) { s.insert(0, m.clone()); });
                notify(format!("Liked {}: more like it from now on", m.name));
                expand(m.clone());
            } else if value == 0 {
                starred.with_mut(|s| s.retain(|x| x.key() != m.key()));
                notify(format!("Removed like from {}", m.name));
            }
            if value < 0 {
                kept.with_mut(|k| k.retain(|x| x.key() != m.key()));
                starred.with_mut(|s| s.retain(|x| x.key() != m.key()));
                notify(format!("{} won't come back", m.name));
                let (l, r) = pair.peek().clone();
                let side = if l.as_ref().is_some_and(|x| x.key() == m.key()) { Some(true) } else if r.as_ref().is_some_and(|x| x.key() == m.key()) { Some(false) } else { None };
                if let Some(left) = side {
                    let mut ex = avoid();
                    ex.extend(brain::identities(&m));
                    let list: Vec<Media> = filtered.peek().iter().filter(|x| x.key() != m.key()).cloned().collect();
                    let other = if left { r } else { l };
                    let next = match &other {
                        Some(o) => brain.peek().challenger(&prefer(&list, &ex), &ex, o),
                        None => brain.peek().opening_pair(&prefer(&list, &ex), &ex).0,
                    };
                    mark_shown(&next);
                    streak.set(0);
                    pair.with_mut(|p| if left { p.0 = next } else { p.1 = next });
                }
            }
        });
    };
    // A mood is a direction in the titles' space: the words are embedded by the same local model
    // and steer tonight until turned off. The cards are redrawn so the change shows at once.
    let mut toggle_mood = move |words: String, phrase: Option<String>| {
        if *mood_busy.peek() { return; }
        let on = brain.peek().moods().iter().any(|m| m.eq_ignore_ascii_case(&words));
        if on {
            { let mut b = brain.write(); let e = b.set_mood(&words, None); brain::persist(&mut b, Some(&e)); }
            start_duel();
            return;
        }
        mood_busy.set(true);
        spawn(async move {
            match crate::picks_ai::embed_query(phrase.as_deref().unwrap_or(&words)).await {
                Ok(e) => {
                    { let mut b = brain.write(); let ev = b.set_mood(&words, Some(e)); brain::persist(&mut b, Some(&ev)); }
                    start_duel();
                }
                Err(_) => notify("The local AI couldn't read that mood".into()),
            }
            mood_busy.set(false);
        });
    };
    // Every mood off at once (each is set aside in the log; tonight is replayed once).
    let mut clear_moods = move || {
        let events = brain.write().clear_moods();
        if events.is_empty() { return; }
        {
            let mut b = brain.write();
            for e in &events { brain::persist(&mut b, Some(e)); }
        }
        start_duel();
    };
    let mut add_mood = move || {
        let words: String = mood_text.peek().trim().chars().take(80).collect();
        if words.is_empty() { return; }
        mood_text.set(String::new());
        toggle_mood(words, None);
    };
    let save_toggle = move |m: Media, next: bool| {
        spawn(async move {
            if backend::set_saved(&m, next).await.is_err() { notify("Couldn't update Saved. Try again.".into()); return; }
            if !next { crate::library::unsave_from_snapshot(&m.key()); }
            *shell.picks_dirty.write() += 1;
            notify(if next { format!("Saved {} to Library", m.name) } else { format!("Removed {} from Saved", m.name) });
        });
    };
    let mut new_night = move || {
        arm_night.set(false);
        let id = new_night_id();
        night.set(id);
        brain.write().set_session(id);
        kept.set(Vec::new()); starred.set(Vec::new()); shown.set(Vec::new()); round.set(0); streak.set(0);
        slot.set(None);
        notify("A new night: starting from your lasting taste".into());
        start_duel();
    };
    let unstar = move |m: Media| {
        starred.with_mut(|s| s.retain(|x| x.key() != m.key()));
        kept.with_mut(|k| k.retain(|x| x.key() != m.key()));
    };
    use_effect(move || {
        let Some(command) = (shell.pick_command)() else { return };
        shell.pick_command.set(None);
        if command == PickCommand::ClosePopover {
            slot.set(None);
            return;
        }
        if slot.peek().is_some() { return; }
        match command {
            PickCommand::Left => choose(true),
            PickCommand::Right => choose(false),
            PickCommand::Skip => change_pair(false),
            PickCommand::Neither => change_pair(true),
            PickCommand::ClosePopover => {}
        }
    });

    // ------------------------------------------------------------ what the screen shows

    // Only genres that can make a duel: at least two titles, or a catalog that can fetch more.
    let genres: Vec<(String, usize, bool)> = {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for m in pool.read().iter() { for g in rules::genre_choices(m) { *counts.entry(g).or_default() += 1; } }
        let fetchable = catalog_genres.read().clone().unwrap_or_default();
        counts.into_iter().map(|(g, n)| { let more = fetchable.contains(&g.to_lowercase()); (g, n, more) })
            .filter(|(_, n, more)| *n >= 2 || *more).collect()
    };
    let years = year_span(from(), to());
    let year_incomplete = (!from_text().is_empty() && from_raw().is_none()) || (!to_text().is_empty() && to_raw().is_none());
    let saved_only = (shell.picks_saved_only)();
    let type_options: [(Option<&'static str>, &'static str); 3] = [(None, "Everything"), (Some("movie"), "Films"), (Some("series"), "Series")];
    let style_options: [(Style, &'static str); 3] = [(Style::Live, "Live action"), (Style::Animation, "Animation"), (Style::Both, "Live action and animation")];
    // The chip always says what is on screen, the style included (Live action is the default,
    // not "no filter"); it lights up only when something differs from the defaults.
    let chosen_genres = selected_genres();
    let genres_label = (!chosen_genres.is_empty()).then(|| chosen_genres.join(" + "));
    let summary: Vec<String> = [
        kind().map(|k| if k == "movie" { "Films".to_owned() } else { "Series".to_owned() }),
        Some(match style() { Style::Live => "Live action", Style::Animation => "Animation", Style::Both => "Live action and animation" }.to_owned()),
        genres_label, years.clone(),
    ].into_iter().flatten().collect();
    let customised = kind().is_some() || style() != Style::Live || !chosen_genres.is_empty() || years.is_some();
    // Back to the defaults (Live action is the default style); the filters panel and the empty state alike.
    let mut clear_filters = move || { kind.set(None); style.set(Style::Live); selected_genres.set(Vec::new()); from_text.set(String::new()); to_text.set(String::new()); };
    let pair_ready = {
        let (l, r) = resolved_pair();
        let st = style();
        leaving().is_none()
            && l.as_ref().zip(r.as_ref()).is_some_and(|(l, r)| {
                verified.read().contains(&l.key())
                    && verified.read().contains(&r.key())
                    && (saved_only || (rules::style_matches(l, st) && rules::style_matches(r, st)))
            })
    };
    let ai_failed = !verified.read().is_empty() && crate::picks_ai::failed();

    // What the brain says (each title's features cost a hash and a projection) is worked out
    // only when the brain, the candidates, the cards or tonight's picks change: not on every
    // toast, press or finished background check.
    let reference = use_memo(move || brain.read().reference(&filtered.read()));
    let learning = use_memo(move || {
        let b = brain.read();
        if b.votes_tonight() == 0 && b.moods().is_empty() { 0.0 } else { b.learning(&filtered.read(), &reference.read()) }
    });
    let infos = use_memo(move || {
        let b = brain.read();
        let speaks = b.votes_tonight() > 0 || !b.moods().is_empty();
        let kept_now = kept.read();
        let champion = kept_now.first().map(Media::key).filter(|_| streak() > 0);
        let card = |m: &Media| CardInfo {
            matching: speaks.then(|| (b.match_of(m, &reference.read()) * 100.0).round().clamp(1.0, 99.0) as u8),
            why: b.why(m, &kept_now).map(|w| w.text()),
            feels: b.feels(m),
            wins: if champion.as_deref() == Some(&m.key()) { streak() } else { 0 },
            liked: liked_ids.read().contains(&brain::id(m)),
            saved: saved_ids.read().contains(&brain::id(m)),
        };
        let (l, r) = resolved_pair();
        (l.as_ref().map(card), r.as_ref().map(card))
    });
    let shortlist = use_memo(move || {
        let b = brain.read();
        let stars = starred.read();
        let mut all: Vec<Media> = stars.clone();
        for m in kept.read().iter() { if !has(&all, m) { all.push(m.clone()); } }
        b.ranked(&all).into_iter().take(8).map(|m| {
            let pct = (b.match_of(&m, &reference.read()) * 100.0).round().clamp(1.0, 99.0) as u8;
            let star = has(&stars, &m);
            (m, star, pct)
        }).collect::<Vec<(Media, bool, u8)>>()
    });
    let (votes, leaning, moods) = { let b = brain.read(); (b.votes_tonight(), b.leaning().into_iter().map(|(l, _)| l).collect::<Vec<String>>(), b.moods()) };
    let lamps = ((learning() * 5.0).ceil() as usize).min(5);
    let (rl, rr) = resolved_pair();
    let infos = infos();
    let shortlist = shortlist();
    let night_has_state = votes > 0 || !moods.is_empty() || !shortlist.is_empty();
    let waiting = loading() || !brain_ready() || (hide_watched() && watched.read().is_none());
    let growing = !pool_complete() || enriching();

    rsx! {
        div { class: "duel",
            if slot().is_some() {
                div { class: "pop-backdrop", aria_hidden: "true", onclick: move |_| slot.set(None) }
            }
            div { class: "duel-bar",
                div { class: "night",
                    div { class: "night-head",
                        h1 { class: "display night-title", "Tonight" }
                        if votes > 0 { span { class: "night-count tnum", "{votes} " if votes == 1 { "vote" } else { "votes" } } }
                        span { class: "learn", role: "img", aria_label: "How well Picks knows tonight: {lamps} of 5", title: "How well Picks knows tonight",
                            for i in 0..5 { i { class: if i < lamps { "on" } else { "" } } }
                        }
                    }
                    div { class: "night-mood",
                        if !leaning.is_empty() {
                            span { class: "dim", "Leaning" }
                            for l in leaning.iter() { span { class: "lean", "{l}" } }
                        } else if votes == 0 {
                            span { class: "dim", "Keep the one you'd rather watch. Picks learns as you go." }
                        } else {
                            span { class: "dim", "Getting to know tonight…" }
                        }
                        for m in moods.iter() {
                            button { class: "lean set", title: "Drop this mood", onclick: { let m = m.clone(); move |_| toggle_mood(m.clone(), None) },
                                icons::Sparkles {} span { "{m}" } icons::Close {} }
                        }
                    }
                }
                div { class: "chips",
                    if saved_only {
                        button { class: "chip-btn glass on saved-chip", title: "Dueling your Saved watchlist — tap to return to full catalog",
                            onclick: move |_| { shell.picks_saved_only.set(false); },
                            icons::Bookmark {} span { "Saved only" } icons::Close {} }
                    }
                    button { class: if slot() == Some(Slot::Mood) || !moods.is_empty() { "chip-btn glass on" } else { "chip-btn glass" },
                        disabled: ai_failed, title: if ai_failed { "Moods need the local AI" } else { "Set tonight's mood" },
                        onclick: move |_| slot.set(if slot() == Some(Slot::Mood) { None } else { Some(Slot::Mood) }), icons::Sparkles {} span { "Mood" } }
                    if !saved_only {
                        button { class: if slot() == Some(Slot::Filters) || customised { "chip-btn glass on" } else { "chip-btn glass" }, title: "Filters: {summary.join(\", \")}",
                            onclick: move |_| slot.set(if slot() == Some(Slot::Filters) { None } else { Some(Slot::Filters) }),
                            icons::Sliders {}
                            span { class: "filter-summary", {summary.join(" · ")} }
                        }
                    }
                    if arm_night() {
                        span { class: "confirm", role: "group", aria_label: "Start a new night?",
                            button { class: "pill forget-confirm", onclick: move |_| new_night(), icons::Moon {} span { "Reset tonight" } }
                            icons::IconButton { label: "Keep tonight", class: "glass small", onclick: move |_| arm_night.set(false), icons::Close {} }
                        }
                    } else {
                        button { class: "chip-btn glass", title: "New night: forget tonight's votes and moods (lasting taste stays)",
                            onclick: move |_| {
                                if !night_has_state { new_night(); }
                                else { slot.set(None); arm_night.set(true); }
                            },
                            icons::Moon {} span { "New night" } }
                    }
                }
                match slot() {
                    Some(Slot::Mood) => rsx! {
                        div { class: "popover glass mood-pop", role: "group", aria_label: "Tonight's mood",
                            p { class: "pop-note", "Pick a mood or describe one. Picks leans toward it all night; tap again to drop it." }
                            div { class: "pop-options",
                                for (label, phrase) in brain::MOODS {
                                    button { class: if moods.iter().any(|m| m == label) { "option on" } else { "option" }, disabled: mood_busy(),
                                        onclick: move |_| toggle_mood(label.to_owned(), Some(phrase.to_owned())), "{label}" }
                                }
                            }
                            div { class: "mood-input",
                                input { class: "mood-text", r#type: "text", maxlength: "80", placeholder: "Describe a mood: “slow-burn heist in the snow”", aria_label: "Describe a mood", value: "{mood_text}",
                                    onkeydown: move |e| {
                                        e.stop_propagation();
                                        if e.key() == Key::Enter { add_mood(); }
                                        else if e.key() == Key::Escape { slot.set(None); }
                                    },
                                    oninput: move |e| mood_text.set(e.value()) }
                                button { class: "pill accent", disabled: mood_busy() || mood_text().trim().is_empty(), onclick: move |_| add_mood(), icons::Sparkles {} span { "Add" } }
                            }
                            div { class: "pop-row end",
                                if !moods.is_empty() {
                                    button { class: "pill glass", disabled: mood_busy(), onclick: move |_| clear_moods(), icons::Undo {} span { "Clear moods" } }
                                }
                                button { class: "pill accent pop-done", onclick: move |_| slot.set(None), icons::Check {} span { "Done" } }
                            }
                        }
                    },
                    Some(Slot::Filters) => rsx! {
                        div { class: "popover glass filters-pop", role: "group", aria_label: "Filters",
                            div { class: "pop-row",
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
                                button { class: if hide_watched() { "chip-btn glass on" } else { "chip-btn glass" }, role: "switch", aria_checked: "{hide_watched()}",
                                    title: if hide_watched() { "Hiding completed titles; unfinished series stay available" } else { "Showing completed titles too" },
                                    onclick: move |_| {
                                        let on = !hide_watched();
                                        backend::update_settings(|s| s.picks_hide_watched = on);
                                        hide_watched.set(on);
                                    },
                                    icons::EyeOff {} span { "Hide watched" } }
                            }
                            div { class: "pop-row years",
                                span { class: "pop-label", icons::Calendar {} "Years" }
                                input { class: if years_invalid() { "year invalid" } else { "year" }, r#type: "text", inputmode: "numeric", maxlength: "4", placeholder: "From", aria_label: "From year", value: "{from_text}",
                                    onkeydown: move |e| { e.stop_propagation(); if e.key() == Key::Escape { slot.set(None); } },
                                    oninput: move |e| from_text.set(e.value().chars().filter(char::is_ascii_digit).take(4).collect()) }
                                span { class: "dim", "–" }
                                input { class: if years_invalid() { "year invalid" } else { "year" }, r#type: "text", inputmode: "numeric", maxlength: "4", placeholder: "To", aria_label: "To year", value: "{to_text}",
                                    onkeydown: move |e| { e.stop_propagation(); if e.key() == Key::Escape { slot.set(None); } },
                                    oninput: move |e| to_text.set(e.value().chars().filter(char::is_ascii_digit).take(4).collect()) }
                                if years_invalid() {
                                    span { class: "year-hint warn", "From must be ≤ To" }
                                } else if year_incomplete {
                                    span { class: "year-hint dim", "Enter 4 digits" }
                                }
                            }
                            div { class: "pop-options", role: "group", aria_label: "Genres (combine multiple)",
                                button { class: if chosen_genres.is_empty() { "option on" } else { "option" }, aria_pressed: "{chosen_genres.is_empty()}", onclick: move |_| selected_genres.set(Vec::new()), "Any genre" }
                                for (g, _n, more) in genres {
                                    {
                                        let is_on = chosen_genres.iter().any(|x| x.eq_ignore_ascii_case(&g));
                                        let g_click = g.clone();
                                        rsx! {
                                            button { class: if is_on { "option on" } else { "option" },
                                                aria_pressed: "{is_on}",
                                                title: if more { "More from your catalogs" } else { "" },
                                                onclick: move |_| {
                                                    selected_genres.with_mut(|list| {
                                                        if let Some(pos) = list.iter().position(|x| x.eq_ignore_ascii_case(&g_click)) {
                                                            list.remove(pos);
                                                        } else {
                                                            list.push(g_click.clone());
                                                        }
                                                    });
                                                },
                                                "{g}"
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "pop-row end",
                                if customised {
                                    button { class: "pill glass", onclick: move |_| clear_filters(),
                                        icons::Undo {} span { "Clear filters" } }
                                }
                                button { class: "pill accent pop-done", onclick: move |_| slot.set(None), icons::Check {} span { "Done" } }
                            }
                        }
                    },
                    None => rsx! {},
                }
            }

            if ai_failed {
                p { class: "picks-ai-status", role: "status", "Local AI unavailable. Picks is using genres, years and ratings only." }
            }

            if waiting || (rl.is_none() && rr.is_none() && growing && !saved_only) {
                div { class: "duel-cards", div { class: "duel-card skeleton-card" } span { class: "versus display", aria_hidden: "true", "vs" } div { class: "duel-card skeleton-card" } }
            } else if rl.is_none() && rr.is_none() {
                div { class: "center-note",
                    div { class: "glass panel",
                        span { class: "big-icon dim", if saved_only { icons::Bookmark {} } else { icons::Search {} } }
                        p {
                            if saved_only {
                                if hide_watched() { "No unwatched saved titles to duel" } else { "No saved titles to duel" }
                            } else {
                                "Nothing matches these filters"
                            }
                        }
                        if saved_only {
                            button { class: "pill accent", onclick: move |_| shell.picks_saved_only.set(false), icons::Undo {} span { "Return to full catalog" } }
                        } else {
                            button { class: "pill glass", onclick: move |_| clear_filters(), icons::Undo {} span { "Clear filters" } }
                        }
                    }
                }
            } else {
                div { class: match leaving() { Some(true) => "duel-cards keep-left", Some(false) => "duel-cards keep-right", None => "duel-cards" },
                    // A key only renews a component inside a list: a new title must get a new card.
                    // A card shows once its poster and trailer are confirmed.
                    {side(rl.clone(), infos.0.clone(), "left", &verified.read(), style(), saved_only, growing,
                        EventHandler::new(move |_| choose(true)), EventHandler::new(played), EventHandler::new(move |(m, v)| rate(m, v)), EventHandler::new(move |(m, s)| save_toggle(m, s)))}
                    span { class: "versus display", aria_hidden: "true", "vs" }
                    {side(rr.clone(), infos.1.clone(), "right", &verified.read(), style(), saved_only, growing,
                        EventHandler::new(move |_| choose(false)), EventHandler::new(played), EventHandler::new(move |(m, v)| rate(m, v)), EventHandler::new(move |(m, s)| save_toggle(m, s)))}
                }
            }

            div { class: "duel-foot",
                div { class: "duel-moves",
                    button { class: "pill glass", disabled: waiting || !pair_ready, title: "Two other titles; nothing is learned (S)", onclick: move |_| change_pair(false),
                        icons::Next {} span { "Skip" } kbd { class: "keycap small-cap light", "S" } }
                    button { class: "pill glass", disabled: waiting || !pair_ready, title: "Neither appeals tonight: Picks steers away from both (N)", onclick: move |_| change_pair(true),
                        icons::Close {} span { "Neither" } kbd { class: "keycap small-cap light", "N" } }
                }
                div { class: "shortlist", role: "list", aria_label: "Tonight's shortlist",
                    span { class: "shortlist-label", icons::Trophy {} span { "Shortlist" } }
                    if shortlist.is_empty() {
                        span { class: "shortlist-empty dim", "Titles you keep land here, best first." }
                    }
                    for (m, star, pct) in shortlist {
                        ShortItem { key: "{m.key()}", media: m.clone(), star, pct, on_play: played, on_remove: unstar }
                    }
                }
            }

            if let Some((_, text)) = toast() {
                div { class: "toast glass", role: "status", icons::Sparkles {} span { "{text}" } }
            }
        }
    }
}

/// What the brain says about a card.
#[derive(Clone, Debug, PartialEq, Default)]
struct CardInfo { matching: Option<u8>, why: Option<String>, feels: Vec<String>, wins: u32, liked: bool, saved: bool }

/// One side of the duel: the card once checked, else a placeholder (or a note when nothing else
/// matches these filters).
#[allow(clippy::too_many_arguments)]
fn side(m: Option<Media>, info: Option<CardInfo>, side: &'static str, verified: &HashSet<String>, style: Style, saved_only: bool, growing: bool,
    on_keep: EventHandler<()>, on_play: EventHandler<Media>, on_rate: EventHandler<(Media, i64)>, on_save: EventHandler<(Media, bool)>) -> Element {
    match m {
        Some(m) if verified.contains(&m.key()) && (saved_only || rules::style_matches(&m, style)) => rsx! {
            for m in [m] { DuelCard { key: "{m.key()}", media: m.clone(), side, info: info.clone().unwrap_or_default(), on_keep, on_play, on_rate, on_save } }
        },
        None if saved_only || !growing => rsx! {
            div { class: "duel-card lone-note glass",
                p { class: "dim",
                    if saved_only { "Only one saved title is available. Save more or return to the full catalog." }
                    else { "No other title matches these filters. Widen them for a duel." }
                }
            }
        },
        _ => rsx! { div { class: "duel-card skeleton-card" } },
    }
}

/// One side of the duel: artwork, the brain's read on it, facts, synopsis and the ways on.
#[component]
fn DuelCard(media: Media, side: &'static str, info: CardInfo, on_keep: EventHandler<()>, on_play: EventHandler<Media>, on_rate: EventHandler<(Media, i64)>, on_save: EventHandler<(Media, bool)>) -> Element {
    let shell = use_context::<Shell>();
    let init_meta = backend::cached_title_meta(&media);
    let need_meta = init_meta.is_none();
    let mut meta = use_signal(move || init_meta);
    let m = media.clone();
    use_hook(move || { if need_meta { spawn(async move { if let Ok(t) = backend::title_meta(&m).await { meta.set(Some(t)); } }); } });
    let facts = meta();
    let art = crate::components::use_artwork(&media);
    let mut failed_logos = use_signal(HashSet::<String>::new);
    let logo = art().and_then(|a| a.logo)
        .or_else(|| facts.as_ref().map(|f| f.logo.clone()).filter(|l| !l.is_empty()))
        .or_else(|| media.raw["logo"].as_str().map(str::to_owned).filter(|l| !l.is_empty()))
        .filter(|l| !failed_logos.read().contains(l));
    let mut bg_sources = Vec::new();
    if let Some(b) = art().and_then(|a| a.backdrop) { if !b.is_empty() { bg_sources.push(b); } }
    let fb = media.backdrop().to_owned();
    if !fb.is_empty() && !bg_sources.contains(&fb) { bg_sources.push(fb); }
    if !media.poster.is_empty() && !bg_sources.contains(&media.poster) { bg_sources.push(media.poster.clone()); }
    // Each card lends its own accent: its Keep button takes the colour of its artwork.
    let accent_src = bg_sources.first().cloned().unwrap_or_default();
    let accent = use_resource(use_reactive!(|accent_src| async move { backend::accent(accent_src).await }));
    let card_style = accent().flatten().map(|c| format!("--lamp: {c}")).unwrap_or_default();
    // TMDB's official trailers first; the add-on's only where TMDB has none. The theatre also
    // offers the title's other videos.
    let trailer = Some(backend::official_first(&art().map(|a| a.trailers).unwrap_or_default(), &facts.as_ref().map(|f| f.trailers.clone()).unwrap_or_default()))
        .filter(|t| !t.is_empty());
    let videos = art().map(|a| a.videos).unwrap_or_default();
    let about = crate::theatre::About::of(&media, facts.as_ref(), art().as_ref());
    let director = facts.as_ref().and_then(|f| f.directors.first().cloned());
    let imdb = facts.as_ref().map(|f| f.rating.clone()).filter(|r| !r.is_empty())
        .unwrap_or_else(|| backend::s(&media.raw, "imdbRating"));
    let runtime = facts.as_ref().map(|f| f.runtime.clone()).filter(|r| !r.is_empty())
        .or_else(|| Some(backend::s(&media.raw, "runtime")).filter(|r| !r.is_empty()));
    let arrow = if side == "left" { "←" } else { "→" };
    let (open, pre_open, save_m, like, dislike, play) = (media.clone(), media.clone(), media.clone(), media.clone(), media.clone(), media.clone());
    let liked = info.liked;
    let saved = info.saved;
    rsx! {
        article { class: "duel-card {side}", style: "{card_style}", aria_label: "{media.name}",
            div { class: "duel-art", crate::components::FallbackImg { sources: bg_sources } }
            button { class: "duel-hit", aria_label: "Keep {media.name} ({arrow})", onclick: move |_| on_keep.call(()) }
            div { class: "card-top",
                div { class: "card-badges",
                    // The badge stays put while the number moves: the digits roll in and the meter
                    // glides to the new value (no blink).
                    if let Some(p) = info.matching { span { class: "match-badge glass", style: "--fill: {p}%", title: "How likely you'd pick this over a typical title tonight",
                        span { class: "match-num tnum", for v in [p] { span { key: "{v}", class: "roll", "{v}%" } } }
                        span { class: "match-word", "match" }
                        i { class: "match-meter", aria_hidden: "true" } } }
                    if info.wins > 0 {
                        for w in [info.wins] {
                            span { class: "streak", key: "{w}", title: "Wins in a row tonight", icons::Trophy {} span { class: "tnum", "{w}" } }
                        }
                    }
                }
                div { class: "card-rate",
                    icons::IconButton { label: if saved { "Saved to Library: tap to remove" } else { "Save to Library" }, class: if saved { "glass small on" } else { "glass small" }, pressed: saved,
                        onclick: move |e: MouseEvent| { e.stop_propagation(); on_save.call((save_m.clone(), !saved)) }, icons::Bookmark {} }
                    icons::IconButton { label: if liked { "Liked: tap to undo" } else { "Like" }, class: if liked { "glass small on" } else { "glass small" }, pressed: liked,
                        onclick: move |e: MouseEvent| { e.stop_propagation(); on_rate.call((like.clone(), if liked { 0 } else { 1 })) }, icons::ThumbUp {} }
                    icons::IconButton { label: "Not for me: never show it again", class: "glass small",
                        onclick: move |e: MouseEvent| { e.stop_propagation(); on_rate.call((dislike.clone(), -1)) }, icons::ThumbDown {} }
                }
            }
            div { class: "duel-copy",
                if let Some(why) = info.why.clone() { p { class: "card-why", span { class: "lamp", icons::Sparkles {} } span { "{why}" } } }
                match logo {
                    Some(src) => rsx! {
                        img { class: "duel-logo", src: "{src}", alt: "{media.name}",
                            onerror: { let u = src.clone(); move |_| { failed_logos.write().insert(u.clone()); } } }
                    },
                    None => rsx! { h2 { class: "display duel-title", "{media.name}" } },
                }
                div { class: "facts",
                    crate::components::Scores { media: media.clone(), imdb }
                    if let Some(r) = runtime { span { class: "fact", icons::Clock {} span { "{r}" } } }
                    if !media.year.is_empty() { span { class: "fact", icons::Calendar {} span { class: "tnum", "{media.year}" } } }
                    if !media.genres.is_empty() { span { class: "fact genres", icons::Tag {} span { {media.genres.iter().take(2).cloned().collect::<Vec<_>>().join(", ")} } } }
                }
                if !info.feels.is_empty() { p { class: "feels", span { class: "dim", "Feels" } for f in info.feels.iter() { span { class: "feel", "{f}" } } } }
                if let Some(d) = director { p { class: "duel-credit", span { class: "dim", icons::Clapper {} } span { "{d}" } } }
                div { class: "duel-actions",
                    button { class: "pill accent", title: "Keep this ({arrow})", onclick: move |e| { e.stop_propagation(); on_keep.call(()) }, icons::Check {} span { "Keep" } kbd { class: "keycap small-cap", "{arrow}" } }
                    CardPlayButton { media: media.clone(), episodes: facts.as_ref().map(|f| f.episodes.clone()).unwrap_or_default(), on_play: move |_| on_play.call(play.clone()) }
                    if let Some(t) = trailer { icons::IconButton { label: "Trailer and videos", class: "glass",
                        onclick: move |_| crate::theatre::watch_videos(shell, about.clone(), videos.clone(), t.clone(), None), icons::Clapper {} } }
                    button { class: "orb glass", aria_label: "Details", title: "Details",
                        onpointerdown: move |_| backend::prefetch_title(&pre_open),
                        onclick: move |_| { let mut s = shell; s.opened.set(Some(open.clone())) },
                        icons::Info {}
                    }
                }
            }
        }
    }
}

/// One title on tonight's shortlist: its art, how well it matches, and play.
#[component]
fn ShortItem(media: Media, star: bool, pct: u8, on_play: EventHandler<Media>, on_remove: EventHandler<Media>) -> Element {
    let mut shell = use_context::<Shell>();
    let art = crate::components::use_artwork(&media);
    let mut bg_sources = Vec::new();
    if let Some(b) = art().and_then(|a| a.backdrop) { if !b.is_empty() { bg_sources.push(b); } }
    let fb = media.backdrop().to_owned();
    if !fb.is_empty() && !bg_sources.contains(&fb) { bg_sources.push(fb); }
    if !media.poster.is_empty() && !bg_sources.contains(&media.poster) { bg_sources.push(media.poster.clone()); }
    let (open, pre_open, remove, play) = (media.clone(), media.clone(), media.clone(), media.clone());
    rsx! {
        div { class: "short-item", role: "listitem",
            button { class: "short-open", aria_label: "Open {media.name}",
                onpointerdown: move |_| backend::prefetch_title(&pre_open),
                onclick: move |_| shell.opened.set(Some(open.clone())),
                crate::components::FallbackImg { sources: bg_sources, lazy: true }
                span { class: "short-name", if star { span { class: "lamp", icons::Star {} } } span { "{media.name}" } }
                span { class: "short-bar", style: "--fill: {pct}%", aria_label: "{pct}% match" }
            }
            div { class: "short-actions",
                CardPlayButton { media: media.clone(), episodes: Vec::new(), compact: true, on_play: move |_| on_play.call(play.clone()) }
                icons::IconButton { label: "Remove from the shortlist", class: "small", onclick: move |_| on_remove.call(remove.clone()), icons::Close {} }
            }
        }
    }
}

/// One playback control for duel cards and the shortlist. Fresh stored progress wins over
/// catalogue metadata; clicking it never casts a duel vote, but tells the brain (`on_play`).
#[component]
fn CardPlayButton(media: Media, episodes: Vec<backend::Episode>, #[props(default)] compact: bool, #[props(default)] on_play: Option<EventHandler<()>>) -> Element {
    let shell = use_context::<Shell>();
    let progress_media = media.clone();
    let progress = use_resource(move || {
        let _ = (shell.picks_dirty)();
        let m = progress_media.clone();
        async move {
            let stored = backend::stored(&m).await.unwrap_or(m);
            let seen = if stored.kind == "movie" { HashSet::new() } else { backend::watched_episodes(&stored).await };
            (stored, seen)
        }
    });
    // The shortlist has no episode list at hand; the title page resolves the episode instead.
    let target = progress.read().as_ref().and_then(|(m, seen)| rules::card_playback(m, &episodes, seen, backend::now_secs()))
        .or_else(|| (compact && media.kind != "movie").then(|| rules::CardPlayback { video: String::new(), label: "Play".into() }));
    // What a duel card's button would play, asked of the add-ons once the card has stayed on screen
    // a moment (the shortlist doesn't: its titles were asked for while they were duel cards). The
    // episodes arrive after the card does, so they are followed.
    use_effect(use_reactive!(|episodes, compact| {
        if compact { return; }
        let Some(t) = progress.read().as_ref().and_then(|(m, seen)| rules::card_playback(m, &episodes, seen, backend::now_secs()).map(|t| (m.kind.clone(), t.video))) else { return };
        if t.1.is_empty() { return; }
        spawn(async move { backend::sleep(PREFETCH_AFTER_MS).await; backend::prefetch_sources(&t.0, &t.1); });
    }));
    let ready = target.is_some();
    let label = target.as_ref().map(|t| t.label.clone()).unwrap_or_else(|| "Play".into());
    let media = progress.read().as_ref().map(|(m, _)| m.clone()).unwrap_or(media);
    let go = move |e: MouseEvent| {
        e.stop_propagation();
        let Some(target) = &target else { return };
        if let Some(h) = &on_play { h.call(()); }
        if target.video.is_empty() { crate::title::play_or_resume(shell, media.clone()); }
        else { crate::title::play_from_card(shell, media.clone(), target.video.clone()); }
    };
    rsx! {
        if compact {
            icons::IconButton { label: "{label}", class: "small", disabled: !ready, onclick: go, icons::Play {} }
        } else {
            button { class: "pill glass", title: "Play or resume this title", disabled: !ready, onclick: go, icons::Play {} span { "{label}" } }
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
