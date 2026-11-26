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
    let cached_meta = backend::cached_title_meta(&media);
    let init_season = cached_meta.as_ref().and_then(|m| {
        media.last_video_id.as_ref().and_then(|v| m.episodes.iter().find(|e| &e.id == v))
            .or_else(|| m.episodes.iter().find(|e| e.season > 0)).or(m.episodes.first()).map(|e| e.season)
    });
    let need_meta = cached_meta.is_none();
    let mut meta = use_signal(move || match cached_meta {
        Some(m) => Load::Ready(m),
        None => Load::Loading,
    });
    let mut season = use_signal(move || init_season);
    let mut live_media = use_signal(|| media.clone());
    let mut liked = use_signal(|| media.liked);
    let mut saved = use_signal(|| media.saved);
    // Films: one "Already watched" switch (marked, or played through). Series: per episode + whole series.
    let mut watched = use_signal(|| media.watched);
    let mut seen = use_signal(std::collections::HashSet::<String>::new);
    let mut ep_progress = use_signal(std::collections::HashMap::<String, (i64, i64, bool)>::new);
    // Up next is only named once the watched episodes are known (no "S1 E1" flashing first).
    let mut seen_ready = use_signal(|| false);
    // Track whether the user manually chose a season so background refreshes don't jump away.
    let season_picked = use_signal(|| false);
    // Collection (franchise parts) and "More like this" shelves loaded lazily below credits.
    let mut collection = use_signal(|| None::<backend::CollectionShelf>);
    let mut similar = use_signal(Vec::<Media>::new);
    let for_rel = media.clone();
    use_hook(move || { spawn(async move {
        let (col, sim) = tokio::join!(backend::collection_parts(&for_rel), backend::similar_titles(&for_rel));
        collection.set(col);
        similar.set(sim);
    }); });
    let mut person_sheet = use_signal(|| None::<backend::Person>);
    let mut dl_toast = use_signal(|| None::<(u64, String)>);
    // A title opened from a shelf, Search or Picks carries the add-on's data only: what you did
    // with it (saved, watched, liked, progress and last episode) comes from the store and also
    // refreshes whenever playback finishes (`shell.picks_dirty`). Per-field touched flags ensure
    // editing one field doesn't freeze the others, and `store_gen` drops stale out-of-order loads.
    let mut touched_saved = use_signal(|| false);
    let mut touched_liked = use_signal(|| false);
    let mut touched_watched = use_signal(|| false);
    let mut store_gen = use_signal(|| 0u64);
    let for_store = media.clone();
    use_effect(move || {
        let _ = (shell.picks_dirty)();
        let gen = *store_gen.peek() + 1;
        store_gen.set(gen);
        let m_req = for_store.clone();
        spawn(async move {
            let (stored_res, w, prog) = tokio::join!(
                backend::stored(&m_req),
                backend::watched_episodes(&m_req),
                backend::episode_playback_map(&m_req),
            );
            if *store_gen.peek() != gen { return; }
            seen.set(w);
            ep_progress.set(prog);
            if let Some(fresh) = stored_res {
                let ts = *touched_saved.peek();
                let tl = *touched_liked.peek();
                let tw = *touched_watched.peek();
                live_media.with_mut(|cur| {
                    cur.position_ms = fresh.position_ms;
                    cur.duration_ms = fresh.duration_ms;
                    cur.last_video_id = fresh.last_video_id.clone();
                    cur.video_watched = fresh.video_watched;
                    if !ts { cur.saved = fresh.saved; }
                    if !tl { cur.liked = fresh.liked; }
                    if !tw { cur.watched = fresh.watched; }
                });
                if !ts { saved.set(fresh.saved); }
                if !tl { liked.set(fresh.liked); }
                if !tw && !episodic(&fresh) { watched.set(fresh.watched); }
                if !*season_picked.peek() {
                    if let Some(last_v) = fresh.last_video_id.as_deref() {
                        if let Load::Ready(ref tm) = *meta.peek() {
                            if let Some(ep) = tm.episodes.iter().find(|e| e.id == last_v) {
                                season.set(Some(ep.season));
                            }
                        }
                    }
                }
            }
            seen_ready.set(true);
        });
    });
    let mut chooser = use_signal(|| None::<Chooser>);
    let mut failed_logos = use_signal(std::collections::HashSet::<String>::new);
    let mut failed_frames = use_signal(std::collections::HashSet::<String>::new);
    // Keep Shell aware of open bottom sheets so Esc/Back closes the sheet before leaving TitlePage.
    use_effect(move || {
        let open = chooser().is_some() || person_sheet().is_some();
        if *shell.title_sheet_open.peek() != open { shell.title_sheet_open.set(open); }
    });
    use_drop(move || { shell.title_sheet_open.set(false); });
    // Two-slot hero crossfade state:
    // - `shown`:    currently displayed (fully decoded) backdrop URL on top.
    // - `under`:    previous backdrop held solid underneath during a 1.4s crossfade.
    // - `seq`:      monotonically increases on every real image change so the incoming `top` slot
    //               always gets a fresh DOM key and replays its smooth animation cleanly.
    // - `top_ready`: true once the current `top` image has fired `onload` (never fades a half-decoded image).
    let mut seq = use_signal(|| 0usize);
    let mut top_url = use_signal(String::new);
    let mut under_url = use_signal(String::new);
    let mut top_ready = use_signal(|| false);
    // Grace window on cold start: let the clean widescreen backdrop + logo arrive (~150-450ms)
    // so the hero fades in ONCE with the real art instead of flashing a promotional backdrop or
    // plain text and swapping it half a second later.
    let mut allow_fallback = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            backend::sleep(600).await;
            allow_fallback.set(true);
        });
    });
    let mut busy = use_signal(|| None::<(String, u64)>);
    // Backing out of PendingPlayer (Esc/Back) bumps play_ticket: clear the local Play spinner at once.
    use_effect(move || {
        let cur_ticket = (shell.play_ticket)();
        let stale = busy.peek().as_ref().is_some_and(|(_, t)| *t != cur_ticket);
        if stale { busy.set(None); }
    });
    // Its place in today's chart (BetterPosters), shown as a fact.
    let mut rank = use_signal(|| None::<u64>);
    let for_rank = media.clone();
    use_hook(move || { spawn(async move { rank.set(backend::today_rank(&for_rank).await); }); });
    // The backdrop dissolves through the production's own textless frames every 9 seconds,
    // paused whenever there is only one frame or the player is active.
    let mut frame = use_signal(|| 0usize);
    let mut frames_count = use_signal(|| 0usize);
    use_future(move || async move {
        let mut waited = 0;
        while !*top_ready.peek() && waited < 10_000 { backend::sleep(100).await; waited += 100; }
        loop {
            backend::sleep(9000).await;
            if *frames_count.peek() > 1 && shell.player.peek().is_none() {
                *frame.write() += 1;
            }
        }
    });

    let for_meta = media.clone();
    use_hook(move || {
        if !need_meta { return; }
        spawn(async move {
            match backend::title_meta(&for_meta).await {
                Ok(m) => {
                    let cur = live_media.peek().clone();
                    let start = cur.last_video_id.as_ref().and_then(|v| m.episodes.iter().find(|e| &e.id == v))
                        .or_else(|| m.episodes.iter().find(|e| e.season > 0)).or(m.episodes.first()).map(|e| e.season);
                    if !*season_picked.peek() { season.set(start); }
                    meta.set(Load::Ready(m));
                }
                Err(e) => meta.set(Load::Failed(format!("{e:#}"))),
            }
        });
    });

    let episodes = move || match meta() { Load::Ready(m) => m.episodes, _ => Vec::new() };

    // What the Play button would start, asked of the add-ons now: Play then only checks sources.
    use_effect(move || {
        let cur = live_media();
        let id = if !episodic(&cur) { Some(cur.id.clone()) } else if seen_ready() { up_next(&episodes(), &cur, &seen()) } else { None };
        if let Some(id) = id { backend::prefetch_sources(&cur.kind, &id); }
    });

    // Play: check offline downloads first, otherwise the automatic Play order + probe.
    let autoplay = use_callback(move |(video, label): (String, Option<String>)| {
        let media = live_media.peek().clone();
        if busy.peek().is_some() { return; }
        let all_eps = episodes();
        let next = next_episode(&all_eps, &video);
        let prev = prev_episode(&all_eps, &video);
        let season_eps: Vec<Episode> = all_eps.iter().find(|e| e.id == video)
            .map(|cur| all_eps.iter().filter(|e| e.season == cur.season).cloned().collect())
            .unwrap_or_default();
        if let Some(offline) = backend::find_download(&media.key(), &video) {
            play_offline(shell, offline);
            return;
        }
        let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
        let ticket = crate::finding(shell, title.clone());
        busy.set(Some((video.clone(), ticket)));
        // Season packs: a labelled size covers the whole season.
        let pack = label.as_ref().map(|_| season_eps.len()).filter(|&n| n > 0);
        spawn(async move {
            let ctx = backend::autoplay_context_for_media(&media, &video, pack);
            let pick = backend::autoplay_for(&media.kind, &video, ctx).await;
            if busy.peek().as_ref().is_some_and(|(_, t)| *t == ticket) { busy.set(None); }
            if !crate::still_wanted(shell, ticket) { return; }
            // Nothing played: every source can be chosen by hand.
            if !play_pick(shell, &media, &video, &title, pick, pack,
                next.map(|e| (e.id.clone(), Some(e.label()))),
                prev.map(|e| (e.id.clone(), Some(e.label()))),
                season_eps) {
                chooser.set(Some(Chooser { video, label }));
            }
        });
    });

    // One-tap offline download for a movie or episode using the automatic source selector.
    let quick_download = use_callback(move |(video, label): (String, Option<String>)| {
        let media = live_media.peek().clone();
        let all_eps = episodes();
        let pack = label.as_ref().and_then(|_| {
            let s = all_eps.iter().find(|e| e.id == video)?.season;
            Some(all_eps.iter().filter(|e| e.season == s).count())
        });
        let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
        let note_id = backend::rand_u64();
        dl_toast.set(Some((note_id, format!("Finding best source to download {title}…"))));
        spawn(async move {
            let ctx = backend::autoplay_context_for_media(&media, &video, pack);
            let pick = backend::autoplay_for(&media.kind, &video, ctx).await;
            let msg = match pick.best {
                Some(src) => {
                    let sub = [src.quality.map(str::to_owned), src.hdr.map(str::to_owned)].into_iter().flatten().collect::<Vec<_>>().join(" ");
                    let fname = (!src.filename.is_empty()).then(|| src.filename.clone());
                    match backend::start_download(media.clone(), video, title.clone(), if sub.is_empty() { src.addon.clone() } else { sub }, src.url, src.headers, fname) {
                        Ok(()) => format!("Downloading {title} — see Library → Downloads."),
                        Err(e) => format!("Download couldn't start: {e}"),
                    }
                }
                None => backend::no_source_message(pick.debrid.as_deref()).into(),
            };
            dl_toast.set(Some((note_id, msg)));
            backend::sleep(5000).await;
            if dl_toast.peek().as_ref().map(|t| t.0) == Some(note_id) { dl_toast.set(None); }
        });
    });

    let m1 = media.clone();
    // Like (1) or Not for me (-1); the same again takes it back. The buttons and the L and D keys.
    let rate = use_callback(move |value: i64| {
        let media = m1.clone();
        touched_liked.set(true);
        let before = *liked.peek();
        let next = if before == value { 0 } else { value };
        liked.set(next);
        spawn(async move { if backend::feedback(&media, next).await.is_ok() { *shell.picks_dirty.write() += 1; } else { liked.set(before); } });
    });
    // Save to the Library (S), and the "Already watched" switch (W): the button and the key alike.
    let for_save = media.clone();
    let toggle_save = use_callback(move |_: ()| {
        touched_saved.set(true);
        let next = !*saved.peek();
        saved.set(next);
        let m = for_save.clone();
        spawn(async move { if backend::set_saved(&m, next).await.is_err() { saved.set(!next); } });
    });
    let for_watched = media.clone();
    let toggle_watched = use_callback(move |_: ()| {
        touched_watched.set(true);
        let next = !*watched.peek();
        watched.set(next);
        let m = for_watched.clone();
        spawn(async move { if backend::set_watched(&m, next).await.is_err() { watched.set(!next); } else { *shell.picks_dirty.write() += 1; } });
    });
    let toggle_series_watched = use_callback(move |_: ()| {
        let eps = episodes();
        let all_reg: Vec<String> = crate::picks_rules::regular_episodes(&eps, backend::now_secs()).into_iter().map(|e| e.id.clone()).collect();
        if all_reg.is_empty() { return; }
        let all_done = all_reg.iter().all(|id| seen.peek().contains(id));
        let next = !all_done;
        touched_watched.set(true);
        seen.with_mut(|s| for id in &all_reg { if next { s.insert(id.clone()); } else { s.remove(id); } });
        watched.set(next);
        let m = live_media.peek().clone();
        spawn(async move {
            let _ = backend::set_episodes_watched(&m, all_reg, next).await;
            let _ = backend::set_watched(&m, next).await;
            *shell.picks_dirty.write() += 1;
        });
    });

    let art = crate::components::use_title_artwork(&media);
    // A series' next episode, or that it ended (TVmaze).
    let mut status = use_signal(|| None::<String>);
    let for_status = media.clone();
    use_hook(move || { if for_status.kind != "movie" { spawn(async move { status.set(backend::series_status(&for_status).await); }); } });
    // Keys from the app root: P play/resume, L like, D not for me, S save, W already watched, T trailer, O sources.
    use_effect(move || {
        let Some(key) = (shell.title_command)() else { return };
        shell.title_command.set(None);
        let k_media = live_media.peek().clone();
        let film = !episodic(&k_media);
        match key {
            '\x1b' => { chooser.set(None); person_sheet.set(None); }
            'p' => if film { autoplay.call((k_media.id.clone(), None)) }
                   else if let Some(id) = up_next(&episodes(), &k_media, &seen.peek()).filter(|_| *seen_ready.peek()) { autoplay.call((id.clone(), backend::episode_label(&id))) },
            'l' => rate.call(1),
            'd' => rate.call(-1),
            's' => toggle_save.call(()),
            'w' => if film { toggle_watched.call(()) } else { toggle_series_watched.call(()) },
            't' => if let Load::Ready(m) = &*meta.peek() {
                let videos = art.peek().clone().map(|a| a.videos).unwrap_or_default();
                if !m.trailers.is_empty() || !videos.is_empty() {
                    let about = crate::theatre::About::of(&k_media, Some(m), art.peek().as_ref());
                    crate::theatre::watch_videos(shell, about, videos, m.trailers.clone(), None);
                }
            },
            'o' if film => chooser.set(Some(Chooser { video: k_media.id.clone(), label: None })),
            _ => {}
        }
    });
    let current_media = live_media();
    let facts = match meta() { Load::Ready(m) => Some(m), _ => None };
    let year = media.year.clone();
    let film = !episodic(&media);
    let logo = art().and_then(|a| a.logo)
        .or_else(|| facts.as_ref().map(|m| m.logo.clone()).filter(|l| !l.is_empty()))
        .or_else(|| media.raw["logo"].as_str().map(str::to_owned).filter(|l| !l.is_empty()))
        .filter(|l| !failed_logos.read().contains(l));
    let backdrop = art().and_then(|a| a.backdrop).unwrap_or_else(|| media.backdrop().to_owned());
    let production = art().unwrap_or_default();
    // Combine add-on trailers with free/TMDB artwork trailers without blocking `title_meta`.
    let trailer = {
        let mut all = facts.as_ref().map(|m| m.trailers.clone()).unwrap_or_default();
        for key in &production.trailers {
            let t = backend::Trailer { youtube: Some(key.clone()), url: None };
            if !all.contains(&t) { all.push(t); }
        }
        (!all.is_empty() || !production.videos.is_empty()).then_some(all)
    };
    // Build the clean backdrop rotation list. Only use a true widescreen `media.background` as
    // fallback (never stretch a vertical `media.poster` across the 16:9 hero!). The poster still
    // seeds the blurred `.ambient` room glow immediately on cold start so the screen feels alive.
    let fallback_bg = use_hook(|| media.background.clone());
    let ambient_fallback = use_hook(|| media.backdrop().to_owned());
    let has_art_backdrop = production.backdrop.is_some();
    // BetterPosters catalog items already carry a w1280 backdrop: show it on Frame 0.
    let instant_bp_bg = fallback_bg.starts_with("https://image.tmdb.org/t/p/");
    let mut frames: Vec<String> = if has_art_backdrop {
        let mut list = vec![backdrop.clone()];
        for s in &production.stills { list.push(s.clone()); }
        list
    } else if (allow_fallback() || instant_bp_bg) && !fallback_bg.is_empty() {
        vec![fallback_bg.clone()]
    } else {
        Vec::new()
    };
    let mut unique = std::collections::HashSet::new();
    frames.retain(|f| unique.insert(f.clone()));
    {
        let broken = failed_frames.read();
        frames.retain(|f| !f.is_empty() && !broken.contains(f));
    }
    let active_idx = if frames.is_empty() { 0 } else { frame() % frames.len() };
    let n_frames = frames.len();
    if *frames_count.peek() != n_frames { frames_count.set(n_frames); }
    let desired = frames.get(active_idx).cloned().unwrap_or_default();
    let preload_next = if frames.len() > 1 {
        frames.get((active_idx + 1) % frames.len()).cloned().unwrap_or_default()
    } else {
        String::new()
    };
    // Promote `desired` into the two-slot crossfade whenever the target URL changes.
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
    // The ambient glow follows the hero's backdrop (resized to w300 in main.rs for 120fps blur),
    // and can immediately warm the room with the poster's colors while the widescreen hero loads.
    let mut lamp = shell.lamp_source;
    let lamp_src = if has_art_backdrop { backdrop.clone() } else { ambient_fallback.clone() };
    use_effect(use_reactive!(|lamp_src| if !lamp_src.is_empty() && *lamp.peek() != lamp_src { lamp.set(lamp_src); }));
    // Only logos that read as white idents (see backend::idents); generation-guarded against stale races.
    let mut idents = use_signal(Vec::<backend::Studio>::new);
    let mut idents_gen = use_signal(|| 0u64);
    let studios = production.studios.clone();
    use_effect(use_reactive!(|studios| {
        let gen = *idents_gen.peek() + 1;
        idents_gen.set(gen);
        spawn(async move {
            let found = backend::idents(studios).await;
            if *idents_gen.peek() == gen { idents.set(found); }
        });
    }));
    // Use card metadata immediately on Frame 0 so facts don't pop in after `title_meta`.
    let imdb_rating = facts.as_ref().map(|m| m.rating.clone()).filter(|r| !r.is_empty())
        .unwrap_or_else(|| backend::s(&media.raw, "imdbRating"));
    let runtime = facts.as_ref().map(|m| m.runtime.clone()).filter(|r| !r.is_empty())
        .or_else(|| Some(backend::s(&media.raw, "runtime")).filter(|r| !r.is_empty()));
    let cur_top = top_url();
    let cur_under = under_url();
    let cur_seq = seq();
    let cur_ready = top_ready();
    let is_initial = cur_under.is_empty();
    let logo_pending = art().is_none() && facts.is_none() && !allow_fallback();
    let sheet_open = chooser().is_some() || person_sheet().is_some();

    rsx! {
        article { class: "title-page", aria_label: "{media.name}",
            section { class: "title-hero", inert: sheet_open.then_some("true"),
                div { class: "title-art-stack",
                    if !cur_under.is_empty() {
                        for k in [format!("u-{cur_under}")] {
                            img {
                                key: "{k}",
                                class: "title-art under",
                                src: "{cur_under}",
                                alt: "",
                                decoding: "async",
                            }
                        }
                    }
                    if !cur_top.is_empty() {
                        for k in [format!("t-{cur_seq}")] {
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
                                    // Retire the old `under` layer after the 0.95s crossfade completes.
                                    if !under_url.peek().is_empty() {
                                        spawn(async move {
                                            backend::sleep(1050).await;
                                            if *seq.peek() == my_seq {
                                                under_url.set(String::new());
                                            }
                                        });
                                    }
                                },
                                onerror: {
                                    let failed_src = cur_top.clone();
                                    move |_| { failed_frames.write().insert(failed_src.clone()); }
                                },
                            }
                        }
                    }
                    if !preload_next.is_empty() && preload_next != cur_top && preload_next != cur_under {
                        for k in [format!("p-{preload_next}")] {
                            img {
                                key: "{k}",
                                class: "title-art preload",
                                src: "{preload_next}",
                                alt: "",
                                aria_hidden: "true",
                            }
                        }
                    }
                }
                div { class: "title-copy",
                    match logo {
                        Some(src) => rsx! {
                            img { class: "title-logo", src: "{src}", alt: "{media.name}",
                                onerror: { let u = src.clone(); move |_| { failed_logos.write().insert(u.clone()); } } }
                        },
                        None if logo_pending => rsx! { div { class: "title-logo", style: "height: 76px" } },
                        None => rsx! { h1 { class: "display title-name", "{media.name}" } },
                    }
                    if let Some(t) = production.tagline.clone() { p { class: "tagline", "{t}" } }
                    div { class: "facts",
                        if let Some(r) = rank() { span { class: "fact today", title: "Most watched today on TMDB (BetterPosters)", icons::Trophy {} span { class: "tnum", "#{r} today" } } }
                        crate::components::Scores { media: media.clone(), imdb: imdb_rating }
                        if let Some(r) = runtime {
                            span { class: "fact", title: "Runtime", icons::Clock {} span { "{r}" } }
                        }
                        if !year.is_empty() { span { class: "fact", title: "Released", icons::Calendar {} span { class: "tnum", "{year}" } } }
                        // A series that is on says when its next episode airs (or that it ended) in place of its type.
                        match status() {
                            Some(t) => rsx! { span { class: "fact", title: "Series (TVmaze)", icons::Tv {} span { "{t}" } } },
                            None => rsx! { span { class: "fact", title: "Type",
                                match media.kind.as_str() { "movie" => rsx! { icons::Film {} }, "series" => rsx! { icons::Tv {} }, _ => rsx! { icons::Blossom {} } }
                                span { "{backend::type_label(&media.kind)}" }
                            } },
                        }
                        if !media.genres.is_empty() { span { class: "fact genres", title: media.genres.iter().take(3).cloned().collect::<Vec<_>>().join(", "), icons::Tag {} span { {media.genres.iter().take(3).cloned().collect::<Vec<_>>().join(", ")} } } }
                        if let Some(c) = production.certificate.clone() { span { class: "fact cert", title: "Certificate", "{c}" } }
                    }
                    div { class: "title-actions",
                        // A series carries on where you are: the started episode, else the one after the last you watched.
                        if !film {
                            if let Some(next) = up_next(&episodes(), &current_media, &seen()).filter(|_| seen_ready()) {
                                button { class: "pill accent play-big", disabled: busy().is_some(),
                                    onclick: { let l = next.clone(); move |_| autoplay.call((l.clone(), backend::episode_label(&l))) },
                                    if busy().is_some() { span { class: "spinner", aria_hidden: "true" } span { "Finding source" } }
                                    else { icons::Play {} span { {
                                        let verb = if current_media.last_video_id.as_deref() == Some(next.as_str()) && backend::in_progress(&current_media) { "Resume" } else { "Play" };
                                        match backend::episode_label(&next) { Some(l) => format!("{verb} {l}"), None => verb.into() }
                                    } } }
                                }
                            } else if !seen_ready() || matches!(meta(), Load::Loading) {
                                button { class: "pill accent play-big", disabled: true,
                                    icons::Play {} span { "Play" }
                                }
                            }
                        }
                        if film {
                            button { class: "pill accent play-big", disabled: busy().is_some(),
                                onclick: { let id = current_media.id.clone(); move |_| autoplay.call((id.clone(), None)) },
                                if busy().is_some() { span { class: "spinner", aria_hidden: "true" } span { "Finding source" } }
                                else { icons::Play {} span { if backend::in_progress(&current_media) { "Resume" } else { "Play" } } }
                            }
                        }
                        icons::IconButton { label: if saved() { "Saved to Library (S)" } else { "Save to Library (S)" }, class: if saved() { "glass on" } else { "glass" },
                            pressed: saved(), onclick: move |_| toggle_save.call(()), icons::Bookmark {} }
                        icons::IconButton { label: if liked() > 0 { "Liked" } else { "Like" }, class: if liked() > 0 { "glass on" } else { "glass" },
                            pressed: liked() > 0, onclick: move |_| rate.call(1), icons::ThumbUp {} }
                        icons::IconButton { label: "Not for me", class: if liked() < 0 { "glass on" } else { "glass" },
                            pressed: liked() < 0, onclick: move |_| rate.call(-1), icons::ThumbDown {} }
                        if film {
                            button { class: if watched() { "pill glass watched-switch on" } else { "pill glass watched-switch" }, role: "switch", aria_checked: "{watched()}",
                                title: "Already watched (W)",
                                onclick: move |_| toggle_watched.call(()),
                                if watched() { icons::Check {} } else { icons::EyeOff {} }
                                span { "Already watched" }
                            }
                        }
                        if let Some(t) = trailer {
                            icons::IconButton { label: "Trailer and videos (T)", class: "glass",
                                onclick: { let (about, videos) = (crate::theatre::About::of(&media, facts.as_ref(), Some(&production)), production.videos.clone());
                                    move |_| crate::theatre::watch_videos(shell, about.clone(), videos.clone(), t.clone(), None) }, icons::Clapper {} }
                        }
                        if film {
                            icons::IconButton { label: "Download for offline playback", class: "glass",
                                onclick: { let id = media.id.clone(); move |_| quick_download.call((id.clone(), None)) }, icons::Download {} }
                            icons::IconButton { label: "Choose a source (O)", class: "glass", onclick: { let id = media.id.clone(); move |_| chooser.set(Some(Chooser { video: id.clone(), label: None })) }, icons::Layers {} }
                        }
                        if !film && !episodes().is_empty() {
                            {
                                let eps = episodes();
                                let all_reg: Vec<String> = crate::picks_rules::regular_episodes(&eps, backend::now_secs()).into_iter().map(|e| e.id.clone()).collect();
                                let all_done = !all_reg.is_empty() && all_reg.iter().all(|id| seen.read().contains(id));
                                rsx! {
                                    button { class: if all_done { "pill glass watched-switch on" } else { "pill glass watched-switch" },
                                        role: "switch", aria_checked: "{all_done}", title: "Mark entire series watched (W)",
                                        onclick: move |_| toggle_series_watched.call(()),
                                        if all_done { icons::Check {} } else { icons::EyeOff {} }
                                        span { if all_done { "Series watched" } else { "Mark series watched" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "title-body", inert: sheet_open.then_some("true"),
                if !media.description.is_empty() { p { class: "title-synopsis", "{media.description}" } }
                else if let Some(d) = facts.as_ref().map(|m| m.description.clone()).filter(|d| !d.is_empty()) { p { class: "title-synopsis", "{d}" } }
                // The studios and networks behind it, as quiet idents.
                if !idents().is_empty() {
                    div { class: "studios", aria_label: "Made by",
                        for st in idents() { img { key: "{st.logo}", src: "{st.logo}", alt: "{st.name}", title: "{st.name}" } }
                    }
                }

                match meta() {
                    Load::Loading => rsx! {
                        if !film { div { class: "rows", for i in 0..3 { div { key: "{i}", class: "row skeleton-row" } } } }
                    },
                    Load::Failed(e) => rsx! { div { class: "notice glass", span { class: "warn-icon", icons::Alert {} } span { "{e}" } } },
                    Load::Ready(m) => rsx! {
                        if !film { Episodes { list: m.episodes.clone(), media: current_media.clone(), season: season, season_picked: season_picked, last: current_media.last_video_id.clone(), busy: busy().map(|(v, _)| v), seen: seen,
                            watched: watched,
                            progress: ep_progress,
                            posters: production.season_posters.clone(),
                            on_play: move |(id, label): (String, String)| autoplay.call((id, Some(label))),
                            on_download: move |(id, label): (String, String)| quick_download.call((id, Some(label))),
                            on_sources: move |(id, label): (String, String)| chooser.set(Some(Chooser { video: id, label: Some(label) })) } }
                        Videos { videos: production.videos.clone(), about: crate::theatre::About::of(&media, Some(&m), Some(&production)), trailers: m.trailers.clone() }
                        Credits { meta: m.clone(), production: production.clone(), on_person: move |p: backend::Person| person_sheet.set(Some(p)) }
                        if let Some(col) = collection() {
                            section { class: "shelf title-shelf",
                                div { class: "group-head", span { class: "group-icon", icons::Stack {} } span { class: "group-name", "{col.name}" }
                                    span { class: "tnum dim", "{col.parts.len()}" } }
                                crate::components::ShelfStrip {
                                    for item in col.parts { crate::components::PosterCard { key: "col-{item.key()}", media: item } }
                                }
                            }
                        }
                        if !similar.read().is_empty() {
                            section { class: "shelf title-shelf",
                                div { class: "group-head", span { class: "group-icon lamp", icons::Sparkles {} } span { class: "group-name", "More like this" } }
                                crate::components::ShelfStrip {
                                    for item in similar.read().iter().cloned() { crate::components::PosterCard { key: "sim-{item.key()}", media: item } }
                                }
                            }
                        }
                    },
                }
            }

            if let Some(c) = chooser() {
                for c in [c] { SourceSheet { key: "{c.video}", media: current_media.clone(), chooser: c.clone(),
                    next: next_episode(&episodes(), &c.video).map(|e| (e.id.clone(), Some(e.label()))),
                    prev: prev_episode(&episodes(), &c.video).map(|e| (e.id.clone(), Some(e.label()))),
                    season_eps: {
                        let all = episodes();
                        all.iter().find(|e| e.id == c.video).map(|cur| all.iter().filter(|e| e.season == cur.season).cloned().collect()).unwrap_or_default()
                    },
                    on_close: move |_| chooser.set(None) } }
            }
            if let Some(p) = person_sheet() {
                for p in [p] { PersonSheet { key: "{p.name}", person: p, on_close: move |_| person_sheet.set(None) } }
            }
            if let Some((_, msg)) = dl_toast() {
                div { class: "toast glass", role: "status",
                    span { class: "lamp", icons::Download {} }
                    span { class: "toast-title", "{msg}" }
                    icons::IconButton { label: "Dismiss", class: "small", onclick: move |_| dl_toast.set(None), icons::Close {} }
                }
            }
        }
    }
}

/// Where a series carries on: the episode you started, unless you have watched it; else the
/// first unwatched released one after the furthest you watched; a series not begun starts at its
/// first released regular episode. None once everything released after the furthest watched is seen.
fn up_next(list: &[Episode], media: &Media, seen: &std::collections::HashSet<String>) -> Option<String> {
    if let Some(last) = media.last_video_id.as_ref().filter(|l| !seen.contains(*l) && backend::in_progress(media)) { return Some(last.clone()); }
    let released = crate::picks_rules::regular_episodes(list, backend::now_secs());
    let start = released.iter().rposition(|e| seen.contains(&e.id)).map_or(0, |i| i + 1);
    released.iter().skip(start).find(|e| !seen.contains(&e.id)).map(|e| e.id.clone())
}

fn next_episode(list: &[Episode], video: &str) -> Option<Episode> {
    let at = list.iter().position(|e| e.id == video)?;
    list.get(at + 1).filter(|e| e.season != 0).cloned()
}

fn prev_episode(list: &[Episode], video: &str) -> Option<Episode> {
    let at = list.iter().position(|e| e.id == video)?;
    (at > 0).then(|| list.get(at - 1).filter(|e| e.season != 0).cloned()).flatten()
}

/// Start the platform player for a manually chosen source, keeping remaining direct candidates
/// available for in-player switching and fallback.
#[allow(clippy::too_many_arguments)]
fn start(
    shell: Shell,
    media: &Media,
    video: &str,
    title: &str,
    src: Source,
    all_direct: Vec<Source>,
    next: Option<(String, Option<String>)>,
    prev: Option<(String, Option<String>)>,
    season_eps: Vec<Episode>,
) {
    let rest: Vec<Source> = all_direct.into_iter().filter(|s| s.url != src.url).collect();
    start_with(shell, media, video, title, src, None, next, prev, season_eps, rest, None, 0)
}

/// How many times an automatic choice may move on because its audio is not in your language.
const LANGUAGE_RETRIES: u8 = 2;

/// `at`: where the previous, failed source stopped, so the next one carries on from there.
/// `checks`: while above 0 (automatic choices), a file whose tagged audio lacks the Audio-language
/// setting's language makes way for the next source instead of turning subtitles on.
#[allow(clippy::too_many_arguments)]
fn start_with(
    shell: Shell,
    media: &Media,
    video: &str,
    title: &str,
    src: Source,
    pack: Option<usize>,
    next: Option<(String, Option<String>)>,
    prev: Option<(String, Option<String>)>,
    season_eps: Vec<Episode>,
    mut rest: Vec<Source>,
    at: Option<i64>,
    checks: u8,
) {
    // The same file from another scraper would fail the same way.
    if !src.filename.is_empty() { rest.retain(|s| s.filename != src.filename); }
    let mut all_sources = Vec::with_capacity(1 + rest.len().min(19));
    all_sources.push(src.clone());
    all_sources.extend(rest.iter().take(19).cloned());
    let language_check = checks > 0 && rest.iter().any(backend::claims_language);
    let resume = at.filter(|p| *p > 0).unwrap_or_else(|| if backend::in_progress(media) && (media.last_video_id.as_deref() == Some(video) || media.kind == "movie") { media.position_ms } else { 0 });
    let full_title = title.to_owned();
    let subtitle = if video != media.id { media.name.clone() } else { media.meta_line(true) };
    let short_title = title.strip_prefix(&format!("{} ", media.name)).filter(|_| video != media.id).unwrap_or(title).to_owned();
    let next_video = next.as_ref().map(|(id, _)| (media.kind.clone(), id.clone()));
    let next_label = next.as_ref().and_then(|(_, label)| label.clone());
    let filename = (!src.filename.is_empty()).then(|| src.filename.clone());
    let another = {
        let (media, video, full_title, next, prev, season_eps, rest) = (
            media.clone(), video.to_owned(), full_title.clone(), next.clone(), prev.clone(), season_eps.clone(), rest.clone(),
        );
        Box::new(move |shell: Shell, position: i64, language: bool| {
            let (media, video, full_title, next, prev, season_eps, rest) = (
                media.clone(), video.clone(), full_title.clone(), next.clone(), prev.clone(), season_eps.clone(), rest.clone(),
            );
            let ticket = crate::finding(shell, full_title.clone());
            spawn(async move {
                // Not in your language: the sources whose label names it, then the rest.
                let (claimed, others): (Vec<Source>, Vec<Source>) = rest.into_iter().partition(|s| language && backend::claims_language(s));
                let mut pick = backend::pick_source(claimed, pack).await;
                if pick.best.is_none() { pick = backend::pick_source(others, pack).await; } else { pick.rest.extend(others); }
                backend::dev_log(&format!("another source{}: {}", if language { " (language)" } else { "" },
                    pick.best.as_ref().map(backend::source_summary).unwrap_or_else(|| "nothing".into())));
                if !crate::still_wanted(shell, ticket) { return; }
                match pick.best {
                    Some(found) => start_with(shell, &media, &video, &full_title, found, pack, next, prev, season_eps, pick.rest, Some(position), if pick.audio_known { 0 } else { checks.saturating_sub(1) }),
                    None => { no_source(shell, pick.debrid.as_deref()); open_page(shell, &media); }
                }
            });
        }) as crate::TryAnother
    };
    let on_switch_source = {
        let (media, video, full_title, next, prev, season_eps, all_sources) = (
            media.clone(), video.to_owned(), full_title.clone(), next.clone(), prev.clone(), season_eps.clone(), all_sources.clone(),
        );
        Box::new(move |shell: Shell, idx: usize, position: i64| {
            let Some(chosen) = all_sources.get(idx).cloned() else { return };
            let others: Vec<Source> = all_sources.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, s)| s.clone()).collect();
            start_with(shell, &media, &video, &full_title, chosen, pack, next.clone(), prev.clone(), season_eps.clone(), others, Some(position), 0);
        }) as Box<dyn Fn(Shell, usize, i64)>
    };
    let on_switch_episode = {
        let media = media.clone();
        Box::new(move |shell: Shell, ep_id: String| {
            let label = backend::episode_label(&ep_id);
            play_next(shell, media.clone(), ep_id, label);
        }) as Box<dyn Fn(Shell, String)>
    };
    let hooks = crate::PlaybackHooks {
        on_next: next.map(|(id, label)| {
            let media = media.clone();
            Box::new(move |shell: Shell| play_next(shell, media.clone(), id.clone(), label.clone())) as Box<dyn Fn(Shell)>
        }),
        on_prev: prev.clone().map(|(id, label)| {
            let media = media.clone();
            Box::new(move |shell: Shell| play_next(shell, media.clone(), id.clone(), label.clone())) as Box<dyn Fn(Shell)>
        }),
        on_another: Some(another),
        on_switch_source: Some(on_switch_source),
        on_switch_episode: Some(on_switch_episode),
    };
    let extra_audio_tracks = if src.extra_audio_tracks.is_empty() {
        backend::cached_extra_audio_tracks(&src)
    } else {
        src.extra_audio_tracks.clone()
    };
    crate::start_playback_hooks(shell, PlayRequest {
        title: short_title, subtitle, url: src.url, filename, start_ms: resume, headers: src.headers,
        size: src.size.clone(), hdr: src.hdr.map(str::to_owned), quality: src.quality.map(str::to_owned),
        record: Some((media.clone(), (video != media.id).then(|| video.to_owned()))),
        has_next: hooks.on_next.is_some(),
        has_prev: prev.is_some(),
        next_label, next_video,
        sources: all_sources,
        episodes: season_eps,
        current_video_id: (video != media.id).then(|| video.to_owned()),
        language_check,
        extra_audio_tracks,
        ..Default::default()
    }, hooks);
}

/// Ask for the sources `resume_now` would play while its button is on screen: the film, or the
/// series' unfinished episode.
pub fn prefetch_resume(media: &Media) {
    let id = if media.kind == "movie" { Some(media.id.clone()) } else { media.last_video_id.clone().filter(|_| !media.video_watched) };
    if let Some(id) = id { backend::prefetch_sources(&media.kind, &id); }
}

/// Play a title directly from Home Hero: resumes if started, or resolves the first unwatched
/// regular episode for unstarted series so Play actually starts playback instead of opening Details.
pub fn play_or_resume(mut shell: Shell, media: Media) {
    if media.kind == "movie" || media.last_video_id.is_some() {
        resume_now(shell, media);
        return;
    }
    let ticket = crate::finding(shell, media.name.clone());
    spawn(async move {
        let (meta_res, seen) = tokio::join!(backend::title_meta(&media), backend::watched_episodes(&media));
        if !crate::still_wanted(shell, ticket) { return; }
        let list = meta_res.map(|m| m.episodes).unwrap_or_default();
        let next = up_next(&list, &media, &seen)
            .or_else(|| crate::picks_rules::regular_episodes(&list, backend::now_secs()).first().map(|e| e.id.clone()));
        match next {
            Some(video) => {
                let label = backend::episode_label(&video);
                play_next(shell, media, video, label);
            }
            None => {
                shell.player_status.set(None);
                shell.opened.set(Some(media));
            }
        }
    });
}

/// Resume from Home or Continue watching: the film, or the series' last episode, starts at once
/// with the automatic choice. Without a known episode the title page opens instead.
pub fn resume_now(mut shell: Shell, media: Media) {
    if media.kind == "movie" { let id = media.id.clone(); play_next(shell, media, id, None); return; }
    match media.last_video_id.clone() {
        // The last episode is finished: carry on with the one after it, not that episode again.
        Some(video) if media.video_watched => {
            let ticket = crate::finding(shell, media.name.clone());
            spawn(async move {
                let list = backend::title_meta(&media).await.map(|m| m.episodes).unwrap_or_default();
                if !crate::still_wanted(shell, ticket) { return; }
                let next = next_episode(&list, &video).map(|e| e.id).unwrap_or(video);
                let label = backend::episode_label(&next);
                play_next(shell, media, next, label);
            });
        }
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
    play_next(shell, media, video, label);
}

/// Play an offline downloaded file immediately with zero network access, reading fresh SQLite
/// resume position and chaining to the next downloaded episode when present.
pub fn play_offline(shell: Shell, entry: backend::DownloadEntry) {
    spawn(async move {
        let resume = if entry.video_id != entry.media.id {
            let (pos, dur, watched) = backend::single_episode_playback(&entry.media, &entry.video_id).await;
            if !watched && pos > 5_000 && (dur <= 0 || pos < dur - 15_000) { pos } else { 0 }
        } else if let Some(fresh) = backend::stored(&entry.media).await {
            if backend::in_progress(&fresh) { fresh.position_ms } else { 0 }
        } else if backend::in_progress(&entry.media) {
            entry.media.position_ms
        } else {
            0
        };
        // Find the next downloaded episode for this series (by season:episode suffix) for offline Up Next.
        let next_dl = if entry.video_id != entry.media.id {
            let parts: Vec<&str> = entry.video_id.split(':').collect();
            if parts.len() >= 3 {
                if let (Ok(s), Ok(e)) = (parts[parts.len() - 2].parse::<i64>(), parts[parts.len() - 1].parse::<i64>()) {
                    let prefix = parts[..parts.len() - 2].join(":");
                    let next_same = format!("{prefix}:{s}:{}", e + 1);
                    let next_season = format!("{prefix}:{}:1", s + 1);
                    backend::find_download(&entry.media.key(), &next_same)
                        .or_else(|| backend::find_download(&entry.media.key(), &next_season))
                } else { None }
            } else { None }
        } else { None };
        let next_label = next_dl.as_ref().map(|d| d.title.clone());
        let has_next = next_dl.is_some();
        let on_next = next_dl.map(|next_entry| {
            Box::new(move |shell: Shell| play_offline(shell, next_entry.clone())) as Box<dyn Fn(Shell)>
        });
        let filename = entry.path.file_name().map(|n| n.to_string_lossy().into_owned());
        crate::start_playback(shell, PlayRequest {
            title: entry.title,
            subtitle: format!("Offline · {}", entry.subtitle),
            url: entry.path.to_string_lossy().into_owned(),
            filename,
            file_size: Some(entry.bytes),
            start_ms: resume,
            size: Some(backend::file_size_label(entry.bytes)),
            record: Some((entry.media.clone(), (entry.video_id != entry.media.id).then_some(entry.video_id))),
            has_next,
            next_label,
            ..Default::default()
        }, on_next);
    });
}

/// Play one video now with the automatic choice, without the page (Resume, Next episode).
fn play_next(shell: Shell, media: Media, video: String, label: Option<String>) {
    if let Some(offline) = backend::find_download(&media.key(), &video) {
        play_offline(shell, offline);
        return;
    }
    let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
    let ticket = crate::finding(shell, title.clone());
    spawn(async move {
        let list = if media.kind == "movie" { Vec::new() } else { backend::title_meta(&media).await.map(|m| m.episodes).unwrap_or_default() };
        let label = label.or_else(|| list.iter().find(|e| e.id == video).map(Episode::label));
        let title = label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
        let after = next_episode(&list, &video).map(|e| (e.id.clone(), Some(e.label())));
        let before = prev_episode(&list, &video).map(|e| (e.id.clone(), Some(e.label())));
        let season_eps: Vec<Episode> = list.iter().find(|e| e.id == video)
            .map(|cur| list.iter().filter(|x| x.season == cur.season).cloned().collect())
            .unwrap_or_default();
        let pack = (!season_eps.is_empty()).then_some(season_eps.len());
        let ctx = backend::autoplay_context_for_media(&media, &video, pack);
        let pick = backend::autoplay_for(&media.kind, &video, ctx).await;
        if !crate::still_wanted(shell, ticket) { return; }
        // Nothing played: the title page, where every source can be chosen.
        if !play_pick(shell, &media, &video, &title, pick, pack, after, before, season_eps) { open_page(shell, &media); }
    });
}

/// Play Play's automatic choice; false (having said why) when there is none.
#[allow(clippy::too_many_arguments)]
fn play_pick(
    shell: Shell,
    media: &Media,
    video: &str,
    title: &str,
    pick: backend::Pick,
    pack: Option<usize>,
    next: Option<(String, Option<String>)>,
    prev: Option<(String, Option<String>)>,
    season_eps: Vec<Episode>,
) -> bool {
    match pick.best {
        Some(src) => { start_with(shell, media, video, title, src, pack, next, prev, season_eps, pick.rest, None, if pick.audio_known { 0 } else { LANGUAGE_RETRIES }); true }
        None => { no_source(shell, pick.debrid.as_deref()); false }
    }
}

/// Say why nothing could play (the player status line).
fn no_source(mut shell: Shell, debrid: Option<&str>) {
    shell.player_status.set(Some(PlayerStatus::Failed(backend::no_source_message(debrid).into())));
}

/// The title page, unless one is open already.
fn open_page(mut shell: Shell, media: &Media) {
    if shell.opened.peek().is_none() { shell.opened.set(Some(media.clone())); }
}

/// Episodes shown at once; a longer season gets range chips (divisible by 2, 3 and 4 columns).
const EPISODE_PAGE: usize = 24;

/// Season chips, then the season's episodes as a grid. Tapping an episode plays it with the
/// automatic choice; its list icon opens the manual chooser instead.
#[component]
fn Episodes(list: Vec<Episode>, media: Media, season: Signal<Option<i64>>, season_picked: Signal<bool>, last: Option<String>, busy: Option<String>,
            seen: Signal<std::collections::HashSet<String>>,
            watched: Signal<bool>,
            progress: Signal<std::collections::HashMap<String, (i64, i64, bool)>>,
            on_play: EventHandler<(String, String)>,
            on_download: EventHandler<(String, String)>,
            on_sources: EventHandler<(String, String)>,
            #[props(default)] posters: std::collections::HashMap<i64, String>) -> Element {
    let mut shell = use_context::<Shell>();
    let mut season = season;
    let mut season_picked = season_picked;
    // Free-first episode stills (TVmaze / ani.zip, then TMDB fallback) keyed by the active season.
    let mut stills = use_signal(|| (None::<i64>, std::collections::HashMap::<i64, String>::new()));
    // The range of a long season you chose, for that season; otherwise the one with your next episode.
    let mut range = use_signal(|| None::<(Option<i64>, usize)>);
    let for_stills = media.clone();
    use_effect(move || {
        let Some(n) = season() else { return };
        let m = for_stills.clone();
        spawn(async move {
            let got = backend::season_stills(&m, n).await;
            if *season.peek() == Some(n) {
                stills.set((Some(n), got));
            }
        });
    });
    let mut seasons: Vec<i64> = list.iter().map(|e| e.season).collect();
    seasons.sort_by_key(|s| if *s == 0 { i64::MAX } else { *s });
    seasons.dedup();
    let shown: Vec<Episode> = list.iter().filter(|e| Some(e.season) == season()).cloned().collect();
    // Marking an episode updates watched state; Shift+click marks up to that episode in its season.
    // The series counts as watched once every released regular episode is watched.
    let mut seen = seen;
    let mut series_done = watched;
    let (for_mark, all) = (media.clone(), list.clone());
    let mark = use_callback(move |(ids, on): (Vec<String>, bool)| {
        if ids.is_empty() { return; }
        let before = seen.peek().clone();
        seen.with_mut(|s| for id in &ids { if on { s.insert(id.clone()); } else { s.remove(id); } });
        let complete = crate::picks_rules::series_complete(&all,&seen.peek(),backend::now_secs());
        let m = for_mark.clone();
        let changed = complete != *series_done.peek();
        if changed { series_done.set(complete); }
        spawn(async move {
            if backend::set_episodes_watched(&m, ids.clone(), on).await.is_ok() {
                if changed { let _ = backend::set_watched(&m, complete).await; }
                *shell.picks_dirty.write() += 1;
            } else {
                // Not saved: the marks go back to what they were.
                seen.with_mut(|s| for id in &ids { if before.contains(id) { s.insert(id.clone()); } else { s.remove(id); } });
                if changed { series_done.set(!complete); }
            }
        });
    });
    let season_ids: Vec<String> = shown.iter().map(|e| e.id.clone()).collect();
    let season_done = !season_ids.is_empty() && season_ids.iter().all(|id| seen.read().contains(id));
    let season_name = match season() { Some(0) => "Extras".to_string(), Some(n) => format!("Season {n}"), None => String::new() };
    // Where you are in this season: the episode you stopped in, else the one after the last you
    // watched, else the first you haven't seen.
    // Only the series' true next episode is marked "Up next": the same one Play resumes, not the first
    // unseen episode of whichever season is open.
    let global_next = up_next(&list, &media, &seen.read());
    let next_in_season = global_next.as_ref().and_then(|id| shown.iter().position(|e| &e.id == id));
    let next_index = next_in_season.or_else(|| { let s = seen.read(); let at = last.as_ref().and_then(|l| shown.iter().position(|e| &e.id == l));
        match at { Some(i) if !s.contains(&shown[i].id) => Some(i), Some(i) if i + 1 < shown.len() => Some(i + 1), _ => shown.iter().position(|e| !s.contains(&e.id)) } });
    let next_id = global_next.clone();
    // Per season: regular seasons count released regular episodes only (specials and unaired ones don't
    // hold "Watched" back); Extras count everything in season 0.
    let released = crate::picks_rules::regular_episodes(&list, backend::now_secs());
    // (counted, watched, still to air, done)
    let season_stats = |n: i64| -> (usize, usize, usize, bool) {
        let s = seen.read();
        let (total, watched, future) = if n > 0 {
            let counted: Vec<&Episode> = released.iter().copied().filter(|e| e.season == n).collect();
            let all_regular = list.iter().filter(|e| e.season == n && e.number > 0).count();
            (counted.len(), counted.iter().filter(|e| s.contains(&e.id)).count(), all_regular.saturating_sub(counted.len()))
        } else {
            (list.iter().filter(|e| e.season == 0).count(), list.iter().filter(|e| e.season == 0 && s.contains(&e.id)).count(), 0)
        };
        (total, watched, future, total > 0 && watched == total)
    };
    let pages = shown.len().div_ceil(EPISODE_PAGE);
    let current = match range() { Some((s, p)) if s == season() && p < pages => p, _ => next_index.map(|i| i / EPISODE_PAGE).unwrap_or(0) };
    let visible: Vec<Episode> = shown.iter().skip(current * EPISODE_PAGE).take(EPISODE_PAGE).cloned().collect();
    rsx! {
        section { class: "episodes-block",
            // Each season by its own poster when the production has them, else a chip.
            if seasons.len() > 1 && seasons.iter().any(|n| posters.contains_key(n)) {
                div { class: "season-cards", role: "radiogroup", aria_label: "Season",
                    for n in seasons.clone() {
                        {
                            let (s_total, s_seen, s_future, s_done) = season_stats(n);
                            let s_pct = if s_total > 0 { (s_seen as f64 / s_total as f64 * 100.0).clamp(0.0, 100.0) } else { 0.0 };
                            let sub_text = if s_done && s_future > 0 {
                                "Caught up".to_string()
                            } else if s_done {
                                "Watched".to_string()
                            } else if s_total == 0 && s_future > 0 {
                                "Coming soon".to_string()
                            } else if s_total == 1 {
                                "1 episode".to_string()
                            } else {
                                format!("{s_total} episodes")
                            };
                            rsx! {
                                button { key: "{n}", class: if season() == Some(n) { "season-card on" } else { "season-card" },
                                    role: "radio", aria_checked: "{season() == Some(n)}", onclick: move |_| { season_picked.set(true); season.set(Some(n)); },
                                    div { class: "sp",
                                        if let Some(src) = posters.get(&n) {
                                            crate::components::FallbackImg {
                                                sources: vec![src.clone()],
                                                lazy: true,
                                                span { class: "display", if n == 0 { "Extras" } else { "{n}" } }
                                            }
                                        }
                                        else { span { class: "display", if n == 0 { "Extras" } else { "{n}" } } }
                                        if s_total > 0 {
                                            span { class: if s_done { "sp-count tnum done" } else { "sp-count tnum" }, aria_hidden: "true",
                                                if s_done { icons::Check {} span { "{s_total}" } }
                                                else if s_seen > 0 { span { "{s_seen}/{s_total}" } }
                                                else { span { "{s_total}" } }
                                            }
                                        }
                                        if s_seen > 0 {
                                            div { class: "sp-progress", div { style: "width: {s_pct:.1}%" } }
                                        }
                                    }
                                    span { class: "season-name", if n == 0 { "Extras" } else { "Season {n}" } }
                                    if s_total > 0 || s_future > 0 { span { class: "season-sub dim tnum", "{sub_text}" } }
                                }
                            }
                        }
                    }
                }
            } else if seasons.len() > 1 {
                div { class: "season-chips", role: "radiogroup", aria_label: "Season",
                    for n in seasons {
                        {
                            let (c_total, c_seen, c_future, c_done) = season_stats(n);
                            let name = if n == 0 { "Extras".to_string() } else { format!("Season {n}") };
                            let label = if c_done && c_future > 0 { format!("{name}, caught up") } else if c_done { format!("{name}, watched") }
                                else if c_seen > 0 { format!("{name}, {c_seen} of {c_total} watched") } else { name };
                            rsx! {
                                button { key: "{n}", class: if season() == Some(n) { "chip-btn glass on small-chip" } else { "chip-btn glass small-chip" },
                                    role: "radio", aria_checked: "{season() == Some(n)}", aria_label: "{label}",
                                    onclick: move |_| { season_picked.set(true); season.set(Some(n)); },
                                    if n == 0 { icons::Sparkles {} } else { span { class: "tnum", "S{n}" } }
                                    if c_done { span { class: "chip-count done", aria_hidden: "true", icons::Check {} } }
                                    else if c_seen > 0 { span { class: "chip-count tnum", aria_hidden: "true", "{c_seen}/{c_total}" } } }
                            }
                        }
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
            if pages > 1 {
                // A long season in ranges of 24; the dot marks the range holding your next episode.
                div { class: "range-chips", role: "radiogroup", aria_label: "Episodes",
                    for p in 0..pages {
                        {
                            let chunk = &shown[p * EPISODE_PAGE..((p + 1) * EPISODE_PAGE).min(shown.len())];
                            let (first, to) = (chunk[0].number, chunk[chunk.len() - 1].number);
                            let here = next_in_season.is_some_and(|i| i / EPISODE_PAGE == p);
                            rsx! {
                                button { class: if p == current { "range on" } else { "range" }, role: "radio", aria_checked: "{p == current}",
                                    aria_label: "Episodes {first} to {to}", onclick: move |_| range.set(Some((season(), p))),
                                    span { class: "tnum", "{first}–{to}" }
                                    if here { i { class: "range-dot", title: "Up next is here" } }
                                }
                            }
                        }
                    }
                }
            }
            // Keyed by season and range, so a new page of episodes lifts in.
            for k in [format!("{:?}-{current}", season())] {
            div { key: "{k}", class: "ep-grid",
                for (i, e) in visible.iter().cloned().enumerate() {
                    {
                    let done = seen.read().contains(&e.id);
                    let is_last = last.as_deref() == Some(e.id.as_str());
                    let is_next = next_id.as_deref() == Some(e.id.as_str());
                    let is_busy = busy.as_deref() == Some(e.id.as_str());
                    let ids: Vec<String> = shown.iter().filter(|x| if done { x.number >= e.number } else { x.number <= e.number }).map(|x| x.id.clone()).collect();
                    let special = e.number == 0;
                    let ep_num_label = if special { "SP".to_string() } else if (1..10).contains(&e.number) { format!("0{}", e.number) } else { format!("{}", e.number) };
                    let ep_name = if special { "special".to_string() } else { format!("episode {}", e.number) };
                    let (frac, left_label) = if done {
                        (0.0, None)
                    } else {
                        progress.read().get(&e.id)
                            .filter(|(pos, dur, w)| !*w && *pos > 5_000 && *dur > 0)
                            .map(|(pos, dur, _)| {
                                let f = (*pos as f64 / *dur as f64).clamp(0.0, 1.0);
                                let left = (*dur - *pos).max(0);
                                let mins = (left + 30_000) / 60_000;
                                let txt = if mins >= 60 {
                                    format!("{}h {}m left", mins / 60, mins % 60)
                                } else if mins > 0 {
                                    format!("{mins}m left")
                                } else {
                                    "< 1m left".to_string()
                                };
                                (f, Some(txt))
                            })
                            .unwrap_or((0.0, None))
                    };
                    rsx! {
                    div { key: "{e.id}", style: "--i: {i.min(8)}",
                        class: format!("ep-tile{}{}{}{}", if is_next { " next" } else { "" }, if done { " seen" } else { "" }, if is_last { " last" } else { "" }, if is_busy { " busy" } else { "" }),
                        button { class: "ep-main", disabled: busy.is_some(),
                            aria_label: "Play {ep_name}: {e.title}",
                            onclick: { let (id, l) = (e.id.clone(), e.label()); move |_| on_play.call((id.clone(), l.clone())) },
                            div { class: "ep-still",
                                {
                                    let mut candidates = Vec::new();
                                    {
                                        let st = stills.read();
                                        if st.0 == Some(e.season) {
                                            if let Some(s) = st.1.get(&e.number) {
                                                if !s.is_empty() { candidates.push(s.clone()); }
                                            }
                                        }
                                    }
                                    if !e.thumbnail.is_empty() && !candidates.contains(&e.thumbnail) {
                                        candidates.push(e.thumbnail.clone());
                                    }
                                    rsx! {
                                        crate::components::FallbackImg {
                                            sources: candidates,
                                            lazy: true,
                                            div { class: "ep-still-empty",
                                                span { class: "ep-still-num display tnum", "{ep_num_label}" }
                                            }
                                        }
                                    }
                                }
                                div { class: "ep-top",
                                    span { class: "ep-badge tnum", aria_hidden: "true", if special { "Special" } else { "EP {ep_num_label}" } }
                                    if is_next { span { class: "ep-next", aria_hidden: "true", "Up next" } }
                                    else if done { span { class: "ep-done-pill", aria_hidden: "true", icons::Check {} span { "Watched" } } }
                                }
                                span { class: "ep-play", if is_busy { span { class: "spinner" } } else { icons::Play {} } }
                                if let Some(left) = left_label {
                                    span { class: "ep-left tnum", "{left}" }
                                }
                                if frac > 0.0 {
                                    div { class: "card-progress ep-progress", div { style: "width: {frac * 100.0:.1}%" } }
                                }
                            }
                            div { class: "ep-copy",
                                div { class: "ep-head",
                                    span { class: "ep-name", "{e.title}" }
                                    if is_last { span { class: "ep-last-icon lamp", title: "Last watched", icons::History {} } }
                                }
                                if !e.overview.is_empty() { p { class: "ep-overview clamp-2", "{e.overview}" } }
                            }
                        }
                        div { class: "ep-foot",
                            span { class: "ep-date dim tnum", if !e.released.is_empty() { "{backend::date_label(&e.released)}" } }
                            // One steady name; the pressed state says whether it is on.
                            icons::IconButton { label: if special { "Special watched".to_string() } else { format!("Episode {} watched (Shift+click marks up to episode {})", e.number, e.number) },
                                class: if done { "small ep-seen on" } else { "small ep-seen" }, pressed: done,
                                onclick: {
                                    let ep_id = e.id.clone();
                                    move |ev: MouseEvent| {
                                        if ev.modifiers().shift() { mark.call((ids.clone(), !done)); }
                                        else { mark.call((vec![ep_id.clone()], !done)); }
                                    }
                                }, icons::Check {} }
                            icons::IconButton { label: format!("Download {ep_name}"), class: "small",
                                onclick: { let (id, l) = (e.id.clone(), e.label()); move |_| on_download.call((id.clone(), l.clone())) },
                                icons::Download {} }
                            icons::IconButton { label: format!("Choose a source for {ep_name}"), class: "small",
                                onclick: { let (id, l) = (e.id.clone(), e.label()); move |_| on_sources.call((id.clone(), l.clone())) },
                                icons::Layers {} }
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

#[component]
fn Credits(meta: TitleMeta, #[props(default)] production: backend::Artwork, on_person: EventHandler<backend::Person>) -> Element {
    let row = |label: &'static str, icon: Element, names: Vec<String>| rsx! {
        if !names.is_empty() {
            div { class: "credit-row",
                span { class: "credit-icon", title: label, aria_label: label, {icon} }
                div { class: "credit-chips",
                    for n in names.into_iter().take(12) {
                        button { class: "name-chip", title: "View filmography: {n}",
                            onclick: { let n = n.clone(); move |_| on_person.call(backend::Person { name: n.clone(), ..Default::default() }) }, "{n}" }
                    }
                }
            }
        }
    };
    let fact = |label: &'static str, icon: Element, value: String| rsx! {
        if !value.is_empty() { div { class: "credit-row", span { class: "credit-icon", title: label, aria_label: label, {icon} } span { class: "credit-text", "{value}" } } }
    };
    // The real cast, with their photos and parts, when TMDB (or TVmaze, Wikidata) has them; else
    // the add-on's names. The add-on's top-billed names lead, for sources that keep no billing order.
    let faces = production.cast.iter().any(|p| p.photo.is_some());
    let mut people = production.cast.clone();
    people.sort_by_key(|p| meta.cast.iter().position(|n| n.eq_ignore_ascii_case(&p.name)).unwrap_or(usize::MAX));
    people.truncate(15);
    // Who made it leads the row as cards of their own: a film's directors, a series' creators.
    let mut makers: Vec<backend::Person> = Vec::new();
    for (label, crew_people) in &production.crew {
        let role = match label.as_str() { "Directed by" => "Director", "Created by" => "Creator", _ => continue };
        for p in crew_people {
            if !makers.iter().any(|m| m.name == p.name) { makers.push(backend::Person { role: role.into(), ..p.clone() }); }
        }
    }
    let cards = faces || makers.iter().any(|p| p.photo.is_some());
    if !cards { makers.clear(); }
    // The rest of the crew as names (and the makers too when nobody has a photo); each one searches.
    let crew_rows: Vec<(String, Vec<backend::Person>)> = production.crew.iter()
        .filter(|(l, _)| !(cards && matches!(l.as_str(), "Directed by" | "Created by"))).cloned().collect();
    let crew = !crew_rows.is_empty();
    // The add-on's director and writers stay unless the crew list names them (a keyless source
    // may know only the composer).
    let has = |jobs: &[&str]| production.crew.iter().any(|(l, _)| jobs.contains(&l.as_str()));
    let (directed, written) = (has(&["Directed by", "Created by"]), has(&["Written by"]));
    let language = production.language.clone().unwrap_or_else(|| meta.language.clone());
    rsx! {
        if cards {
            section { class: "people-block",
                div { class: "group-head", span { class: "group-icon", icons::Users {} } span { class: "group-name", if makers.is_empty() { "Cast" } else { "Cast & crew" } } }
                div { class: "people",
                    for p in makers.into_iter().chain(if faces { people } else { Vec::new() }) {
                        button { key: "{p.role}|{p.name}", class: "person", title: "View filmography: {p.name}",
                            onclick: { let p = p.clone(); move |_| on_person.call(p.clone()) },
                            span { class: "face",
                                if let Some(src) = p.photo.clone() {
                                    crate::components::FallbackImg {
                                        sources: vec![src],
                                        lazy: true,
                                        span { class: "display", "{p.name.chars().next().unwrap_or(' ')}" }
                                    }
                                }
                                else { span { class: "display", "{p.name.chars().next().unwrap_or(' ')}" } }
                            }
                            span { class: "person-name", "{p.name}" }
                            if !p.role.is_empty() { span { class: "person-role dim", "{p.role}" } }
                        }
                    }
                }
            }
        }
        if crew {
            dl { class: "crew",
                for (label, crew_people) in crew_rows {
                    div { key: "{label}", dt { "{label}" }
                        dd {
                            for (i, p) in crew_people.into_iter().enumerate() {
                                span { key: "{p.name}",
                                    if i > 0 { ", " }
                                    button { class: "crew-name", title: "View filmography: {p.name}",
                                        onclick: { let p = p.clone(); move |_| on_person.call(p.clone()) }, "{p.name}" }
                                }
                            }
                        }
                    }
                }
            }
        }
        section { class: "credits",
            if !directed { {row("Director", rsx! { icons::Clapper {} }, meta.directors.clone())} }
            if !faces { {row("Cast", rsx! { icons::Users {} }, meta.cast.clone())} }
            if !written { {row("Writers", rsx! { icons::Pen {} }, meta.writers.clone())} }
            {fact("Released", rsx! { icons::Calendar {} }, meta.released.clone())}
            {fact("Country", rsx! { icons::Globe {} }, meta.country.clone())}
            {fact("Language", rsx! { icons::Speech {} }, language)}
            {fact("Awards", rsx! { icons::Trophy {} }, meta.awards.clone())}
        }
    }
}

/// The production's videos (trailers, featurettes, behind the scenes) as a row of thumbnails.
#[component]
fn Videos(videos: Vec<backend::Video>, about: crate::theatre::About, trailers: Vec<backend::Trailer>) -> Element {
    let shell = use_context::<Shell>();
    rsx! {
        if !videos.is_empty() {
            section { class: "videos",
                div { class: "group-head", span { class: "group-icon", icons::Clapper {} } span { class: "group-name", "Videos" } }
                div { class: "video-row",
                    for v in videos.iter().cloned() {
                        button { key: "{v.youtube}", class: "video", aria_label: "Play {v.name}",
                            onclick: { let (k, all, a, tr) = (v.youtube.clone(), videos.clone(), about.clone(), trailers.clone());
                                move |_| crate::theatre::watch_videos(shell, a.clone(), all.clone(), tr.clone(), Some(k.clone())) },
                            span { class: "video-thumb",
                                crate::components::FallbackImg {
                                    sources: vec![
                                        format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", v.youtube),
                                        format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", v.youtube),
                                    ],
                                    lazy: true,
                                }
                                span { class: "ep-play", icons::Play {} }
                            }
                            span { class: "video-name", "{v.name}" }
                            span { class: if v.official { "video-kind official" } else { "video-kind dim" },
                                {crate::theatre::Clip { youtube: String::new(), name: String::new(), kind: v.kind.clone(), official: v.official }.label()} }
                        }
                    }
                }
            }
        }
    }
}

/// Manual choice: every add-on's sources for one video, in a sheet from the bottom.
#[component]
fn SourceSheet(
    media: Media,
    chooser: Chooser,
    next: Option<(String, Option<String>)>,
    prev: Option<(String, Option<String>)>,
    season_eps: Vec<Episode>,
    on_close: EventHandler<()>,
) -> Element {
    let shell = use_context::<Shell>();
    let mut sources = use_signal(|| Load::<(Vec<Source>, Vec<String>)>::Loading);
    let mut expanded = use_signal(|| None::<usize>);
    let mut pressed_backdrop = use_signal(|| false);
    let (kind, video, media_for_sources) = (media.kind.clone(), chooser.video.clone(), media.clone());
    use_hook(move || { spawn(async move {
        sources.set(match backend::sources(&kind, &video).await {
            Ok((mut list, failed)) => {
                backend::classify_manual_sources(&mut list, &media_for_sources, &video);
                Load::Ready((list, failed))
            }
            Err(e) => Load::Failed(format!("{e:#}")),
        });
    }); });
    let title = chooser.label.as_ref().map(|l| format!("{} {l}", media.name)).unwrap_or_else(|| media.name.clone());
    let (m, t, v) = (media.clone(), title.clone(), chooser.video.clone());
    let play = use_callback(move |src: Source| {
        let direct_list: Vec<Source> = match sources.peek().clone() {
            Load::Ready((list, _)) => list.into_iter()
                .filter(|s| s.kind == SourceKind::Direct && (src.off_title || !s.off_title))
                .collect(),
            _ => Vec::new(),
        };
        start(shell, &m, &v, &t, src, direct_list, next.clone(), prev.clone(), season_eps.clone());
        on_close.call(());
    });
    let mut dl_note = use_signal(|| None::<(u64, String)>);
    let (dm, dt, dv) = (media.clone(), title.clone(), chooser.video.clone());
    let save_offline = use_callback(move |src: Source| {
        let sub = [src.quality.map(str::to_owned), src.hdr.map(str::to_owned)].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let fname = (!src.filename.is_empty()).then(|| src.filename.clone());
        let id = backend::rand_u64();
        let msg = match backend::start_download(dm.clone(), dv.clone(), dt.clone(), if sub.is_empty() { src.addon.clone() } else { sub }, src.url, src.headers, fname) {
            Ok(()) => "Saving for offline playback — see Library → Downloads.".into(),
            Err(e) => format!("Download couldn't start: {e}"),
        };
        dl_note.set(Some((id, msg)));
        spawn(async move {
            backend::sleep(5000).await;
            if dl_note.peek().as_ref().map(|n| n.0) == Some(id) { dl_note.set(None); }
        });
    });
    rsx! {
        div { class: "scrim bottom",
            onpointerdown: move |_| pressed_backdrop.set(true),
            onclick: move |_| { if pressed_backdrop() { on_close.call(()); } pressed_backdrop.set(false); },
            section { class: "bottom-sheet glass", role: "dialog", aria_modal: "true", aria_label: "Sources for {title}",
                onpointerdown: move |e| e.stop_propagation(), onclick: move |e| e.stop_propagation(),
                div { class: "sheet-head",
                    span { class: "group-icon", icons::Layers {} }
                    span { class: "group-name", "{title}" }
                    if let Load::Ready((list, _)) = sources() { span { class: "tnum dim", "{list.len()}" } }
                    icons::IconButton { label: "Close", class: "glass", onclick: move |_| on_close.call(()), icons::Close {} }
                }
                div { class: "sheet-scroll",
                    if let Some((_, note)) = dl_note() {
                        div { class: "notice glass sheet-notice", role: "status",
                            span { class: "group-icon", icons::Download {} }
                            span { "{note}" }
                            span { class: "topbar-spacer" }
                            icons::IconButton { label: "Dismiss", class: "small", onclick: move |_| dl_note.set(None), icons::Close {} }
                        }
                    }
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
                                        on_play: move |s: Source| play.call(s),
                                        on_download: move |s: Source| save_offline.call(s) }
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
fn SourceRow(source: Source, open: bool, on_details: EventHandler<()>, on_play: EventHandler<Source>, on_download: EventHandler<Source>) -> Element {
    let playable = source.kind == SourceKind::Direct;
    let label = if !source.filename.is_empty() { source.filename.clone() }
        else { source.description.lines().next().unwrap_or(&source.name).to_owned() };
    let why = match source.kind { SourceKind::Direct => None,
        SourceKind::Torrent => Some("P2P streaming isn't supported yet"),
        SourceKind::External => Some("Links that open outside Veylo aren't supported yet") };
    let src = source.clone();
    let src_dl = source.clone();
    rsx! {
        div { class: if playable && !source.off_title { "source" } else { "source muted" },
            button { class: "source-main", disabled: !playable, title: why.unwrap_or(if source.off_title { "Release name doesn't match this title" } else { "" }),
                aria_label: "Play {label} from {source.addon}", onclick: move |_| on_play.call(src.clone()),
                div { class: "source-tags",
                    span { class: "tag q", {source.quality.unwrap_or("?")} }
                    if let Some(h) = source.hdr { span { class: "tag hdr", "{h}" } }
                    if source.off_title { span { class: "tag warn", "Other?" } }
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
            if playable {
                icons::IconButton { label: "Download for offline playback", class: "small", onclick: move |_| on_download.call(src_dl.clone()), icons::Download {} }
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

/// Filmography sheet for a tapped actor, director, or crew member.
#[component]
fn PersonSheet(person: backend::Person, on_close: EventHandler<()>) -> Element {
    let mut shell = use_context::<Shell>();
    let mut pressed_backdrop = use_signal(|| false);
    let mut loading = use_signal(|| true);
    let mut filmography = use_signal(|| backend::Filmography { person: person.clone(), ..Default::default() });
    let p_fetch = person.clone();
    use_hook(move || {
        spawn(async move {
            let f = backend::person_filmography(&p_fetch).await;
            filmography.set(f);
            loading.set(false);
        });
    });
    let f = filmography();
    let search_name = person.name.clone();
    rsx! {
        div { class: "scrim bottom",
            onpointerdown: move |_| pressed_backdrop.set(true),
            onclick: move |_| { if pressed_backdrop() { on_close.call(()); } pressed_backdrop.set(false); },
            section { class: "bottom-sheet glass", role: "dialog", aria_modal: "true", aria_label: "{f.person.name}",
                onpointerdown: move |e| e.stop_propagation(), onclick: move |e| e.stop_propagation(),
                div { class: "sheet-head",
                    span { class: "group-icon", icons::Users {} }
                    span { class: "group-name", "{f.person.name}" }
                    span { class: "topbar-spacer" }
                    button { class: "pill glass small person-search-btn",
                        aria_label: "Search catalogs for {search_name}",
                        onclick: move |_| {
                            shell.search_query.set(Some(search_name.clone()));
                            shell.search_open.set(true);
                            on_close.call(());
                        },
                        icons::Search {} span { "Search catalogs" }
                    }
                    icons::IconButton { label: "Close", class: "glass", onclick: move |_| on_close.call(()), icons::Close {} }
                }
                div { class: "sheet-scroll",
                    if !f.bio.is_empty() { p { class: "dim person-bio", "{f.bio}" } }
                    if loading() {
                        div { class: "rows", for i in 0..3 { div { key: "{i}", class: "row skeleton-row" } } }
                    } else if f.credits.is_empty() {
                        p { class: "dim", "No filmography found from metadata sources. Try “Search catalogs” above." }
                    } else {
                        for (section_name, items) in f.credits {
                            div { key: "{section_name}", class: "person-section",
                                div { class: "group-head",
                                    span { class: "group-name", "{section_name}" }
                                    span { class: "tnum dim", "{items.len()}" }
                                }
                                div { class: "strip poster-strip",
                                    for item in items {
                                        crate::components::PosterButton {
                                            key: "pf-{item.key()}",
                                            media: item.clone(),
                                            on_press: move |m: Media| {
                                                on_close.call(());
                                                shell.opened.set(Some(m));
                                            },
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
}
