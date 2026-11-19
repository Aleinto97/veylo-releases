//! Veylo's player controls, in Dioxus: the same glass over the picture on Linux and Android.
//! The picture itself is under the transparent WebView (video.rs); everything here is the
//! app's own design, lit by the title's accent. Keys arrive through the shell (Shell::player_key).
use crate::{backend::{self, OnlineSubtitle}, engine::{self, Finish, Fit, LiveRef, Segment, SegmentKind, State}, icons, Shell};
use dioxus::prelude::*;

/// Subtitles the add-ons offer for this video, fetched the first time the sheet opens.
#[derive(Clone, Debug, PartialEq)]
enum Online { Idle, Loading, Ready(Vec<OnlineSubtitle>), Failed }

/// Downloadable subtitles listed before a language is typed.
const ONLINE_SHOWN: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sheet { Audio, Subtitles, Speed, Picture, Info, Keys }

const SPEEDS: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const KEYS: [(&[&str], &str); 20] = [
    (&["Enter"], "Skip the intro or recap, when offered"),
    (&["Space", "K", "Enter"], "Play or pause"), (&["←", "→"], "Back or forward 10 seconds"), (&["J", "L"], "Back or forward 10 seconds"),
    (&["0–9"], "Jump to 0–90%"), (&["Home"], "Back to the start"), (&["↑", "↓"], "Volume"), (&["M"], "Mute"),
    (&["A"], "Next audio track"), (&["C"], "Next subtitles, then off"), (&["S"], "Subtitles list"), (&["Z"], "Fit, fill, stretch"),
    (&["[", "]", "\\"], "Slower, faster, normal speed"), (&["I"], "Details"), (&["H", "?"], "Show or hide shortcuts"),
    (&["N"], "Next episode"), (&["Esc"], "Close panel, leave fullscreen, or close player"), (&["Q"], "Close player"),
    (&["F"], "Fullscreen"), (&["Tab"], "Next control; Shift+Tab goes back"),
];
const ANDROID: bool = cfg!(target_os = "android");

/// The skip the player offers for a segment: credits only lead to the next episode.
fn offer(segments: &[Segment], position: f64, has_next: bool) -> Option<Segment> {
    engine::skip_offer(segments, position).filter(|g| g.kind != SegmentKind::Credits || has_next)
}

fn skip(live: &engine::Live, g: Segment) {
    if g.kind == SegmentKind::Credits { live.finish(Finish::Next) } else { live.seek_to(g.end) }
}

pub fn clock(seconds: f64) -> String {
    let t = seconds.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[component]
pub fn PlayerScreen(live: LiveRef) -> Element {
    let mut shell = use_context::<Shell>();
    let mut state = use_signal(|| live.0.state());
    let mut shown = use_signal(|| true);
    let mut activity = use_signal(|| 0u64);
    let mut sheet = use_signal(|| None::<Sheet>);
    let mut scrub = use_signal(|| None::<f64>);
    let mut fullscreen = use_signal(|| false);
    let mut online = use_signal(|| Online::Idle);
    let sub_query = use_signal(String::new);
    let mut automatic_language = use_signal(|| None::<String>);
    // The download in progress, and what each downloaded subtitle became (its track id).
    let mut sub_busy = use_signal(|| None::<String>);
    let mut sub_added = use_signal(std::collections::HashMap::<String, i64>::new);
    // The video whose subtitles can be downloaded: the episode, or the film itself.
    let wanted = live.0.request.record.as_ref().map(|(m, v)| (m.kind.clone(), v.clone().unwrap_or_else(|| m.id.clone())));
    use_effect(move || {
        // Reopening the sheet retries a failed lookup, without a retry loop while it stays open.
        if sheet() == Some(Sheet::Subtitles) && *online.peek() == Online::Failed { online.set(Online::Idle); }
    });
    let target = wanted.clone();
    use_effect(move || {
        if sheet() != Some(Sheet::Subtitles) && automatic_language().is_none() { return; }
        if *online.peek() != Online::Idle { return; }
        let Some((kind, video)) = target.clone() else { return };
        online.set(Online::Loading);
        spawn(async move {
            online.set(match backend::online_subtitles(&kind, &video).await { Ok(list) => Online::Ready(list), Err(_) => Online::Failed });
        });
    });

    // Intro times from IntroDB or AniSkip, asked once the episode's length is known (AniSkip
    // matches its entries to it). Films and previews have no episode to look up.
    let episode = live.0.request.record.as_ref().and_then(|(_, v)| v.clone());
    let mut looked_up = use_signal(|| false);
    let ask = live.clone();
    use_effect(move || {
        let duration = state().duration;
        if duration <= 0.0 || *looked_up.peek() { return; }
        let Some(video) = episode.clone() else { return };
        looked_up.set(true);
        let live = ask.0.clone();
        spawn(async move { live.set_online_segments(backend::online_segments(&video, duration).await); });
    });

    // The engine publishes its state; the controls follow it.
    let feed = live.clone();
    use_future(move || {
        let live = feed.clone();
        async move {
            let mut rx = live.0.subscribe();
            state.set(rx.borrow_and_update().clone());
            while rx.changed().await.is_ok() { let s = rx.borrow_and_update().clone(); state.set(s); }
        }
    });
    // The picture goes under the page while this screen exists.
    let attach = live.clone();
    use_hook(move || crate::video::attach(&attach.0));
    use_drop(crate::video::detach);

    // Controls fade three seconds after the last touch, key or pointer movement.
    let mut poke = move || {
        shown.set(true);
        let n = *activity.peek() + 1;
        activity.set(n);
        spawn(async move { backend::sleep(3000).await; if *activity.peek() == n { shown.set(false); } });
    };
    use_hook(poke);

    let s: State = state();
    let l = live.0.clone();
    let req = &l.request;
    let visible = shown() || s.paused || sheet().is_some() || scrub().is_some() || s.error.is_some() || !s.loaded;
    let position = scrub().unwrap_or(s.position);
    let duration = s.duration.max(0.0);
    let pct = |v: f64| if duration > 0.0 { (v / duration * 100.0).clamp(0.0, 100.0) } else { 0.0 };
    let (played, buffered) = (pct(position), pct(position + s.buffered));
    let facts = engine::facts(&l, &s);
    let hw = engine::hardware(&s);
    // The facts line says plainly when HDR does not reach the panel ("HDR10+ → SDR").
    let mapped = facts.picture.contains("tone-mapped");
    let short_picture = { let name = facts.picture.split(',').next().unwrap_or_default();
        if facts.picture.contains("to SDR") { format!("{name} → SDR") } else { name.to_owned() } };
    let hdr = facts.picture.starts_with("HDR") || facts.picture.starts_with("HLG") || facts.picture.starts_with("Dolby");
    let has_subs = s.tracks.iter().any(|t| !t.audio) || wanted.is_some();
    let sub_on = s.tracks.iter().any(|t| !t.audio && t.selected);
    // Direct output: mpv can't draw into the picture, so text subtitles are drawn here.
    let caption = (s.direct && sub_on && !s.subtitle.trim().is_empty()).then(|| s.subtitle.clone());
    let skippable = offer(&s.skips(), s.position, req.has_next).filter(|_| sheet().is_none() && s.error.is_none() && scrub().is_none());

    // Keys routed from the shell.
    // A short word on screen for what a key just did ("Fill", "Speed 1.25×").
    let mut osd = use_signal(|| None::<(String, u64)>);
    let mut say = move |text: String| {
        let n = osd.peek().as_ref().map(|o| o.1 + 1).unwrap_or(0);
        osd.set(Some((text, n)));
        spawn(async move { backend::sleep(1400).await; if osd.peek().as_ref().is_some_and(|o| o.1 == n) { osd.set(None); } });
    };

    // Tap a downloadable subtitle: download it, add it as a track and turn it on.
    let get = live.clone();
    // `tapped`: chosen in the sheet (which then closes), not added on its own by the fallback.
    let add_online = use_callback(move |(sub, tapped): (OnlineSubtitle, bool)| {
        let live = get.0.clone();
        if let Some(id) = sub_added.peek().get(&sub.id).copied() { live.select(false, Some(id)); if tapped { sheet.set(None); } return; }
        if sub_busy.peek().is_some() { return; }
        sub_busy.set(Some(sub.id.clone()));
        let mut say = say;
        spawn(async move {
            let file = backend::fetch_subtitle(&sub.url).await;
            let (title, code) = (sub.language.clone(), sub.code.clone());
            let added = backend::on_runtime(async move { tokio::task::spawn_blocking(move || live.add_subtitle(&file, &title, &code)).await }).await;
            sub_busy.set(None);
            match added {
                Ok(Ok(Ok(id))) => {
                    sub_added.write().insert(sub.id.clone(), id);
                    if tapped { sheet.set(None); say(format!("Subtitles: {}", sub.language)); }
                    else { say(format!("No {0} audio: {0} subtitles on", sub.language)); }
                }
                _ => say("Those subtitles couldn’t be loaded".into()),
            }
        });
    });
    let download = use_callback(move |sub: OnlineSubtitle| add_online.call((sub, true)));

    // Subtitles follow the Audio language setting, once per video: off when that language is in
    // the audio (or the setting is Any), otherwise the file's own full subtitles in that language,
    // else the best download. Untagged audio says nothing, so it is left as is.
    let mut fallback_done = use_signal(|| false);
    // Re-runs only when the video becomes ready, not on every position tick.
    let ready = use_memo(move || { let s = state.read(); s.loaded && !s.tracks.is_empty() });
    let (fb_live, fb_wanted) = (live.clone(), wanted.clone());
    use_effect(move || {
        if !ready() || *fallback_done.peek() { return; }
        fallback_done.set(true);
        let s = state.peek().clone();
        let key = backend::stream_language();
        let name = key.as_deref().map(backend::subtitle_language_of).unwrap_or_default();
        let mut say = say;
        match fallback(&s.tracks, key.as_deref()) {
            Fallback::Off(audio) => {
                // mpv's alang normally picked this already; make it explicit when it did not.
                if let Some(id) = audio {
                    if !s.tracks.iter().any(|t| t.audio && t.selected && t.id == id) { fb_live.0.select(true, Some(id)); }
                }
                if s.tracks.iter().any(|t| !t.audio && t.selected) { fb_live.0.select(false, None); }
            }
            Fallback::Embedded(id) => {
                fb_live.0.select(false, Some(id));
                say(format!("No {name} audio: {name} subtitles on"));
            }
            Fallback::Download => {
                let Some(key) = key else { return };
                if fb_wanted.is_none() { return; }
                automatic_language.set(Some(key));
            }
            Fallback::Nothing => {}
        }
    });
    // One lookup feeds both the sheet and the automatic fallback, whichever starts first.
    use_effect(move || {
        let Some(key) = automatic_language() else { return };
        match online() {
            Online::Ready(list) => {
                automatic_language.set(None);
                // A user's download takes precedence over automatic selection.
                if sub_busy.peek().is_some() { return; }
                let name = backend::subtitle_language_of(&key);
                match list.into_iter().find(|s| backend::track_in_language(&s.code, &key) || s.language == name) {
                    Some(sub) => add_online.call((sub, false)),
                    None => say(format!("No {name} audio or subtitles for this video")),
                }
            }
            Online::Failed => { automatic_language.set(None); say("Subtitles couldn’t be looked up right now".into()); }
            _ => {}
        }
    });

    // Keys routed from the shell; the same set as the classic Android player.
    let keys = live.clone();
    use_effect(move || {
        let Some(key) = (shell.player_key)() else { return };
        shell.player_key.set(None);
        let live = &keys.0;
        let now = state.peek().clone();
        let speed = |v: f64| { let v = (v * 100.0).round() / 100.0; live.set_speed(v); if v == 1.0 { "Normal speed".to_string() } else { format!("Speed {v}×") } };
        // Reading shortcuts must not seek, change tracks or close playback accidentally.
        if sheet() == Some(Sheet::Keys) && !matches!(key.as_str(), "h" | "H" | "?" | "Escape" | "Backspace") { return; }
        match key.as_str() {
            "Enter" if offer(&now.skips(), now.position, live.request.has_next).is_some() => {
                if let Some(g) = offer(&now.skips(), now.position, live.request.has_next) { skip(live, g); }
            }
            " " | "k" | "K" | "Enter" | "MediaPlayPause" => { live.toggle_pause(); poke(); }
            "MediaPlay" => { if now.paused { live.toggle_pause(); } }
            "MediaPause" => { if !now.paused { live.toggle_pause(); } poke(); }
            "ArrowLeft" | "j" | "J" | "MediaRewind" => { live.seek_by(-10.0); say("−10 s".into()); }
            "ArrowRight" | "l" | "L" | "MediaFastForward" => { live.seek_by(10.0); say("+10 s".into()); }
            "Home" => { live.seek_to(0.0); say("Start".into()); }
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => { let p = (d.as_bytes()[0] - b'0') as f64 * 10.0; live.seek_percent(p); say(format!("{p}%")); }
            "ArrowUp" | "AudioVolumeUp" => { let v = (now.volume + 5.0).min(130.0); live.set_volume(v); say(format!("Volume {v:.0}")); }
            "ArrowDown" | "AudioVolumeDown" => { let v = (now.volume - 5.0).max(0.0); live.set_volume(v); say(format!("Volume {v:.0}")); }
            "m" | "M" | "AudioVolumeMute" => { live.toggle_mute(); say(if now.muted { "Sound on".into() } else { "Muted".into() }); }
            "f" | "F" if !ANDROID => { let on = !fullscreen(); fullscreen.set(on); crate::video::set_fullscreen(on); }
            "a" | "A" => { let t = live.cycle_track(true); say(format!("Audio: {t}")); }
            "c" | "C" => { let t = live.cycle_track(false); say(format!("Subtitles: {t}")); }
            "s" | "S" => sheet.set(Some(Sheet::Subtitles)),
            "z" | "Z" => {
                let next = Fit::ALL[(Fit::ALL.iter().position(|f| *f == now.fit).unwrap_or(0) + 1) % Fit::ALL.len()];
                live.set_fit(next); say(next.name().into());
            }
            "i" | "I" => sheet.set(if sheet() == Some(Sheet::Info) { None } else { Some(Sheet::Info) }),
            "h" | "H" | "?" => sheet.set(if sheet() == Some(Sheet::Keys) { None } else { Some(Sheet::Keys) }),
            "n" | "N" | "MediaTrackNext" if live.request.has_next => live.finish(Finish::Next),
            "[" => say(speed((now.speed - 0.25).max(0.25))),
            "]" => say(speed((now.speed + 0.25).min(3.0))),
            "\\" => say(speed(1.0)),
            "Escape" | "Backspace" if sheet().is_some() => sheet.set(None),
            "Escape" if fullscreen() => { fullscreen.set(false); crate::video::set_fullscreen(false); }
            "Escape" | "Backspace" | "q" | "Q" | "MediaStop" => live.finish(Finish::Close),
            _ => {}
        }
    });

    let lc = || live.0.clone();
    rsx! {
        div { class: if visible { "player" } else { "player calm" },
            // A tap also sends a synthetic mousemove first: on touch screens only the tap counts.
            onmousemove: move |_| if !ANDROID { poke() },
            // The picture: a tap shows the controls; with them up, a click pauses (desktop) or hides them (touch).
            div { class: "player-surface",
                onclick: { let live = lc(); move |_| {
                    if sheet().is_some() { sheet.set(None); }
                    else if !visible { poke(); }
                    else if ANDROID { shown.set(false); }
                    else { live.toggle_pause(); poke(); }
                } },
                ondoubleclick: move |_| if !ANDROID { let on = !fullscreen(); fullscreen.set(on); crate::video::set_fullscreen(on); },
            }
            if let Some(text) = caption {
                div { class: if visible { "player-caption lifted" } else { "player-caption" }, aria_live: "polite",
                    for line in text.lines().map(str::to_owned).collect::<Vec<_>>() { span { "{line}" } }
                }
            }
            if let Some((text, n)) = osd() { div { key: "{n}", class: "player-osd", role: "status", "{text}" } }
            // Over the picture even when the controls have faded, as long as the segment lasts.
            if let Some(g) = skippable {
                button { class: if visible { "pill glass player-skip lifted" } else { "pill glass player-skip" }, title: "Enter",
                    onclick: { let live = lc(); move |_| skip(&live, g) },
                    span { match g.kind { SegmentKind::Intro => "Skip intro", SegmentKind::Recap => "Skip recap", SegmentKind::Credits => "Next episode" } }
                    icons::Next {}
                }
            }
            if let Some(error) = s.error.clone() {
                div { class: "player-center",
                    div { class: "glass panel player-error", role: "alert",
                        span { class: "warn-icon big-icon", icons::Alert {} }
                        p { class: "body-strong", "{error}" }
                        p { class: "dim", "Another source may play; this one can also be tried again." }
                        div { class: "player-error-actions",
                            button { class: "pill accent", onclick: { let live = lc(); move |_| live.finish(Finish::Another) }, icons::Next {} span { "Try another source" } }
                            button { class: "pill glass", onclick: { let live = lc(); move |_| live.retry() }, icons::Refresh {} span { "Retry" } }
                            button { class: "pill glass", onclick: { let live = lc(); move |_| live.finish(Finish::Close) }, icons::Close {} span { "Close" } }
                        }
                    }
                }
            } else if s.buffering || !s.loaded {
                div { class: "player-center", aria_label: "Loading", div { class: "player-spinner" } }
            }

            div { class: "player-chrome", inert: (!visible || sheet() == Some(Sheet::Keys)).then_some("true"),
                onkeydown: move |e: KeyboardEvent| if matches!(e.key(), Key::Enter | Key::Tab) || e.key() == Key::Character(" ".into()) { e.stop_propagation(); },
                div { class: "player-top",
                    button { class: "bare", aria_label: "Back (Esc)", title: "Back (Esc)", onclick: { let live = lc(); move |_| live.finish(Finish::Close) }, icons::ArrowLeft {} }
                    div { class: "player-heading",
                        span { class: "player-title display", "{req.title}" }
                        // One quiet line of facts: what is decoded, how, and what the panel shows.
                        span { class: "player-facts-line",
                            if !req.subtitle.is_empty() { span { "{req.subtitle}" } }
                            if !facts.resolution.is_empty() { span { "{facts.resolution}" } }
                            if !facts.size.is_empty() { span { class: "tnum", title: "File size", "{facts.size}" } }
                            if !facts.picture.is_empty() { span { class: if hdr && !mapped { "lit" } else { "" }, title: "{facts.picture}", "{short_picture}" } }
                            if s.loaded && !s.codec.is_empty() {
                                span { class: if hw.is_some() { "" } else { "warn" }, title: if hw.is_some() { "Hardware decoding" } else { "Software decoding" },
                                    "{hw.clone().unwrap_or_else(|| \"Software\".into())}" }
                            }
                        }
                    }
                    span { class: "topbar-spacer" }
                    button { class: "bare", aria_label: "Player shortcuts (H / ?)", title: "Player shortcuts (H / ?)", aria_pressed: "{sheet() == Some(Sheet::Keys)}",
                        onclick: move |_| sheet.set(if sheet() == Some(Sheet::Keys) { None } else { Some(Sheet::Keys) }), icons::Keyboard {} }
                    button { class: "bare", aria_label: "Details (I)", title: "Details (I)", aria_pressed: "{sheet() == Some(Sheet::Info)}",
                        onclick: move |_| sheet.set(if sheet() == Some(Sheet::Info) { None } else { Some(Sheet::Info) }), icons::Info {} }
                }

                if s.loaded && s.error.is_none() {
                    div { class: "player-mid",
                        button { class: "bare", aria_label: "Back 10 seconds (←)", title: "Back 10 seconds (←)", onclick: { let live = lc(); move |_| { live.seek_by(-10.0); poke(); } }, icons::Rewind10 {} }
                        button { class: "bare player-big", aria_label: if s.paused { "Play (Space)" } else { "Pause (Space)" }, title: if s.paused { "Play (Space)" } else { "Pause (Space)" },
                            onclick: { let live = lc(); move |_| { live.toggle_pause(); poke(); } },
                            if s.paused { icons::Play {} } else { icons::Pause {} } }
                        button { class: "bare", aria_label: "Forward 10 seconds (→)", title: "Forward 10 seconds (→)", onclick: { let live = lc(); move |_| { live.seek_by(10.0); poke(); } }, icons::Forward10 {} }
                    }
                }

                div { class: "player-bottom",
                    span { class: "player-time tnum", "{clock(position)}" }
                    input { class: "scrubber", r#type: "range", aria_label: "Position", min: "0", max: "{duration.max(1.0)}", step: "0.5",
                        value: "{position}", disabled: duration <= 0.0, style: "--played: {played}%; --buffered: {buffered}%",
                        oninput: move |e| { scrub.set(e.value().parse().ok()); poke(); },
                        onchange: { let live = lc(); move |e: FormEvent| { if let Ok(v) = e.value().parse::<f64>() { live.seek_to(v); } scrub.set(None); } },
                    }
                    span { class: "player-time tnum dim", if duration > 0.0 { "-{clock(duration - position)}" } }
                    div { class: "player-actions",
                        button { class: "bare", aria_label: "Audio (A)", title: "Audio (A)", aria_pressed: "{sheet() == Some(Sheet::Audio)}", onclick: move |_| sheet.set(Some(Sheet::Audio)), icons::Audio {} }
                        button { class: "bare", aria_label: "Subtitles (C)", title: "Subtitles (C)", disabled: !has_subs, aria_pressed: "{sheet() == Some(Sheet::Subtitles)}", onclick: move |_| sheet.set(Some(Sheet::Subtitles)), icons::Subtitles {} }
                        button { class: "bare", aria_label: "Speed ([ ])", title: "Speed ([ ])", aria_pressed: "{sheet() == Some(Sheet::Speed)}", onclick: move |_| sheet.set(Some(Sheet::Speed)), icons::Gauge {} }
                        button { class: "bare", aria_label: "Picture (Z)", title: "Picture (Z)", aria_pressed: "{sheet() == Some(Sheet::Picture)}", onclick: move |_| sheet.set(Some(Sheet::Picture)), icons::Resize {} }
                        if !ANDROID {
                            button { class: "bare", aria_label: if s.muted { "Unmute (M)" } else { "Mute (M)" }, title: if s.muted { "Unmute (M)" } else { "Mute (M)" },
                                onclick: { let live = lc(); move |_| live.toggle_mute() }, if s.muted || s.volume <= 0.0 { icons::Muted {} } else { icons::Audio {} } }
                            input { class: "scrubber volume", r#type: "range", aria_label: "Volume", min: "0", max: "100", step: "1", value: "{s.volume.min(100.0)}",
                                style: "--played: {s.volume.min(100.0)}%; --buffered: 0%",
                                oninput: { let live = lc(); move |e: FormEvent| if let Ok(v) = e.value().parse::<f64>() { live.set_volume(v); } } }
                            button { class: "bare", aria_label: if fullscreen() { "Leave full screen (F)" } else { "Full screen (F)" }, title: if fullscreen() { "Leave full screen (F)" } else { "Full screen (F)" },
                                onclick: move |_| { let on = !fullscreen(); fullscreen.set(on); crate::video::set_fullscreen(on); }, icons::Expand {} }
                        }
                        if req.has_next {
                            button { class: "bare player-next", title: "Next episode (N)", onclick: { let live = lc(); move |_| live.finish(Finish::Next) }, span { "Next" } icons::Next {} }
                        }
                    }
                }
            }

            if let Some(which) = sheet() {
                section { class: "player-sheet glass", role: "dialog", aria_label: sheet_name(which), aria_modal: (which == Sheet::Keys).then_some("true"),
                    tabindex: (which == Sheet::Keys).then_some("-1"), onmounted: move |e| async move { if which == Sheet::Keys { let _ = e.set_focus(true).await; } },
                    onkeydown: move |e: KeyboardEvent| if matches!(e.key(), Key::Enter | Key::Tab) || e.key() == Key::Character(" ".into()) { e.stop_propagation(); },
                    div { class: "sheet-head",
                        span { class: "group-name", "{sheet_name(which)}" }
                        icons::IconButton { label: "Close (Esc)", class: "glass", onclick: move |_| sheet.set(None), icons::Close {} }
                    }
                    div { class: "player-options",
                        match which {
                            Sheet::Audio | Sheet::Subtitles => {
                                let audio = which == Sheet::Audio;
                                let tracks: Vec<engine::Track> = s.tracks.iter().filter(|t| t.audio == audio).cloned().collect();
                                rsx! {
                                    if !audio && !tracks.is_empty() { p { class: "player-options-head", "In this video" } }
                                    if !audio {
                                        OptionRow { on: !sub_on, label: "Off", about: String::new(), pick: { let live = lc(); move |_| { live.select(false, None); sheet.set(None); } } }
                                    }
                                    for t in tracks {
                                        OptionRow { key: "{t.id}", on: t.selected, label: t.label.clone(),
                                            about: if audio || !s.direct { String::new() } else if t.image { "Image subtitles: drawn by mpv, off the direct path".to_string() }
                                                else if t.styled { "Styled: full look on SDR, plain text over HDR".to_string() } else { String::new() },
                                            pick: { let live = lc(); let id = t.id; move |_| { live.select(audio, Some(id)); sheet.set(None); } } }
                                    }
                                    if audio && s.tracks.iter().all(|t| !t.audio) { p { class: "dim player-empty", "No audio tracks." } }
                                    if !audio && wanted.is_some() {
                                        OnlineSubtitles { online: online(), query: sub_query, busy: sub_busy(), added: sub_added(),
                                            selected: s.tracks.iter().find(|t| !t.audio && t.selected).map(|t| t.id), pick: download }
                                    }
                                }
                            }
                            Sheet::Speed => rsx! {
                                for v in SPEEDS {
                                    OptionRow { key: "{v}", on: (s.speed - v).abs() < 0.01, label: if v == 1.0 { "Normal".to_string() } else { format!("{v}×") }, about: String::new(),
                                        pick: { let live = lc(); move |_| { live.set_speed(v); sheet.set(None); } } }
                                }
                            },
                            Sheet::Picture => rsx! {
                                for f in Fit::ALL {
                                    OptionRow { key: "{f.name()}", on: s.fit == f, label: f.name().to_string(), about: f.about().to_string(),
                                        pick: { let live = lc(); move |_| live.set_fit(f) } }
                                }
                                if ANDROID {
                                    p { class: "player-options-head", "Output" }
                                    OptionRow { on: s.direct, label: "Direct to the display".to_string(), about: "MediaCodec straight to the screen: real HDR10, HLG and HDR10+, lowest power".to_string(),
                                        pick: { let live = lc(); move |_| live.set_direct(true) } }
                                    OptionRow { on: !s.direct, label: "Rendered by mpv".to_string(), about: "Full ASS and image subtitles; HDR is tone-mapped to SDR".to_string(),
                                        pick: { let live = lc(); move |_| live.set_direct(false) } }
                                }
                            },
                            Sheet::Keys => rsx! {
                                div { class: "shortcut-list player-keys",
                                    for (k, what) in KEYS.into_iter().filter(|(k,_)| !(ANDROID && k == &["F"]) && (req.has_next || k != &["N"])) { div { class: "shortcut", span { class: "caps", for c in k.iter() { kbd { class: "keycap", "{c}" } } } span { "{what}" } } }
                                }
                            },
                            Sheet::Info => {
                                let rows = details(&l, &s, &facts, hw.as_deref());
                                rsx! {
                                    dl { class: "player-facts",
                                        for (k, v) in rows { div { key: "{k}", dt { "{k}" } dd { "{v}" } } }
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

/// What to do when a video starts, for the audio-language setting (`None` is "Any language").
#[derive(Debug, PartialEq)]
enum Fallback {
    /// No audio tracks, or untagged audio that could be yours: leave the file as it plays.
    Nothing,
    /// Your language's audio is there (this track, when it was not already on), or the setting is
    /// Any: subtitles off.
    Off(Option<i64>),
    /// No audio in your language: this embedded subtitle track on.
    Embedded(i64),
    /// No audio and no embedded subtitles in your language: download them.
    Download,
}

/// The Audio language setting decides subtitles: off when its language is in the main audio (a
/// file can flag a default subtitle regardless), otherwise full subtitles in that language (text
/// before image, never forced-only signs), else a download. Untagged audio says nothing, so the
/// file's own choice stays.
fn fallback(tracks: &[engine::Track], key: Option<&str>) -> Fallback {
    let Some(key) = key else { return Fallback::Off(None) };
    let in_language = |t: &engine::Track| backend::track_in_language(&t.lang, key);
    let audio: Vec<&engine::Track> = tracks.iter().filter(|t| t.audio && !engine::extra_audio(t)).collect();
    if audio.is_empty() { return Fallback::Nothing; }
    if let Some(t) = audio.iter().find(|t| in_language(t)) { return Fallback::Off(Some(t.id)); }
    if audio.iter().any(|t| t.lang.is_empty()) { return Fallback::Nothing; }
    if tracks.iter().any(|t| !t.audio && t.selected && !t.forced && in_language(t)) { return Fallback::Nothing; }
    let full = |t: &&engine::Track| !t.audio && !t.forced && in_language(t);
    match tracks.iter().filter(full).find(|t| !t.image).or_else(|| tracks.iter().find(full)) {
        Some(t) => Fallback::Embedded(t.id),
        None => Fallback::Download,
    }
}

fn sheet_name(s: Sheet) -> &'static str {
    match s { Sheet::Audio => "Audio", Sheet::Subtitles => "Subtitles", Sheet::Speed => "Speed", Sheet::Picture => "Picture", Sheet::Info => "Details", Sheet::Keys => "Shortcuts" }
}

/// File details, plainly: what is decoded, how, and what the screen shows.
fn details(live: &engine::Live, s: &State, facts: &crate::player::Facts, hw: Option<&str>) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    if s.width > 0 { rows.push(("Resolution", format!("{}×{}{}", s.width, s.height, if facts.resolution.is_empty() { String::new() } else { format!(" ({})", facts.resolution) }))); }
    if !s.codec.is_empty() { rows.push(("Video", engine::codec_name(&s.codec))); }
    if !facts.picture.is_empty() { rows.push(("Picture", facts.picture.clone())); }
    rows.push(("Decoding", hw.map(|h| format!("Hardware, {h}")).unwrap_or_else(|| if s.loaded { "Software".into() } else { "Starting".into() })));
    rows.push(("Output", if ANDROID {
        if s.direct { "Direct to the display (MediaCodec)".into() } else if hw.is_none() && s.dv_profile == 5 { "Rendered by mpv, Dolby Vision reshaping".into() } else { "Rendered by mpv (gpu-next)".into() }
    } else if cfg!(target_os = "windows") { "In the app, rendered by mpv (Direct3D 11)".into() } else { "In the app, rendered by mpv (OpenGL)".into() }));
    // What the screen declared when this video started (Linux: always SDR).
    rows.push(("Screen", crate::display::summary(live.screen, backend::hdr_output()).0));
    if !s.target_transfer.is_empty() && !ANDROID { rows.push(("Sent to the screen", if s.target_transfer == "pq" { "HDR10".into() } else { "SDR".into() })); }
    if let Some(label) = &live.request.hdr { rows.push(("Source says", label.clone())); }
    if s.dv_profile > 0 { rows.push(("Dolby Vision", format!("Profile {}", s.dv_profile))); }
    if s.loaded { rows.push(("Dropped frames", s.dropped.to_string())); }
    if !facts.size.is_empty() { rows.push(("File size", facts.size.clone())); }
    if let Some(a) = s.tracks.iter().find(|t| t.audio && t.selected) { rows.push(("Audio", a.label.clone())); }
    rows
}

/// "Download": the add-ons' subtitles for this video, narrowed by language as you type.
#[component]
fn OnlineSubtitles(online: Online, query: Signal<String>, busy: Option<String>, added: std::collections::HashMap<String, i64>,
                   selected: Option<i64>, pick: EventHandler<OnlineSubtitle>) -> Element {
    let q = query();
    let list = match &online { Online::Ready(list) => list.clone(), _ => Vec::new() };
    // The languages on offer, in the list's order (preferred first), as quick filters.
    let mut languages: Vec<String> = Vec::new();
    for s in &list { if !languages.contains(&s.language) { languages.push(s.language.clone()); } }
    let matching: Vec<OnlineSubtitle> = list.iter().filter(|s| backend::subtitle_matches(s, &q)).cloned().collect();
    let more = matching.len().saturating_sub(ONLINE_SHOWN);
    // "Italian 1", "Italian 2": the add-ons give no other name worth showing.
    let mut counts = std::collections::HashMap::<String, usize>::new();
    let rows: Vec<(OnlineSubtitle, usize)> = matching.into_iter().take(ONLINE_SHOWN)
        .map(|s| { let n = counts.entry(s.language.clone()).or_default(); *n += 1; let n = *n; (s, n) }).collect();
    rsx! {
        p { class: "player-options-head", "Download" }
        div { class: "sub-search",
            span { class: "sub-search-icon", aria_hidden: "true", icons::Search {} }
            input { r#type: "search", placeholder: "Search by language", aria_label: "Search subtitles by language", value: "{q}",
                autocomplete: "off", spellcheck: "false",
                oninput: move |e| query.set(e.value()),
                // Typing stays in the field; Esc still closes the sheet.
                onkeydown: move |e: KeyboardEvent| if e.key() != Key::Escape { e.stop_propagation(); },
                onkeyup: move |e: KeyboardEvent| e.stop_propagation(),
            }
            if !q.is_empty() { button { class: "bare sub-search-clear", aria_label: "Clear", onclick: move |_| query.set(String::new()), icons::Close {} } }
        }
        if languages.len() > 1 {
            div { class: "sub-langs", role: "group", aria_label: "Languages",
                for l in languages.into_iter().take(10) {
                    button { key: "{l}", class: if q.eq_ignore_ascii_case(&l) { "sub-lang on" } else { "sub-lang" }, aria_pressed: "{q.eq_ignore_ascii_case(&l)}",
                        onclick: { let l = l.clone(); move |_| { let same = query.peek().eq_ignore_ascii_case(&l); query.set(if same { String::new() } else { l.clone() }); } },
                        "{l}" }
                }
            }
        }
        match online {
            Online::Idle | Online::Loading => rsx! { div { class: "player-empty sub-loading dim", span { class: "spinner", aria_hidden: "true" } span { "Looking for subtitles" } } },
            Online::Failed => rsx! { p { class: "dim player-empty", "Subtitles couldn’t be found right now." } },
            Online::Ready(all) if all.is_empty() => rsx! { p { class: "dim player-empty", "No add-on has subtitles for this video." } },
            Online::Ready(_) => rsx! {
                if rows.is_empty() { p { class: "dim player-empty", "No subtitles in “{q}”." } }
                for (sub, n) in rows {
                    {
                        let track = added.get(&sub.id).copied();
                        let loading = busy.as_deref() == Some(sub.id.as_str());
                        let about = if loading { "Downloading…".to_string() } else if track.is_some() { "Downloaded".to_string() } else { sub.release.clone().unwrap_or_else(|| sub.code.clone()) };
                        rsx! {
                            button { key: "{sub.id}", class: if track.is_some() && track == selected { "player-option on" } else { "player-option" },
                                role: "menuitemradio", aria_checked: "{track.is_some() && track == selected}", disabled: busy.is_some() && !loading,
                                onclick: { let sub = sub.clone(); move |_| pick.call(sub.clone()) },
                                span { class: "player-option-text",
                                    span { class: "player-option-label", "{sub.language} {n}" }
                                    span { class: "player-option-about dim sub-release", title: "{about}", "{about}" }
                                }
                                if loading { span { class: "spinner", aria_hidden: "true" } }
                                else if track.is_some() && track == selected { span { class: "player-option-check", icons::Check {} } }
                                else if track.is_none() { span { class: "player-option-check dim", icons::Download {} } }
                            }
                        }
                    }
                }
                if more > 0 { p { class: "dim player-empty", "{more} more: type a language to narrow the list." } }
            },
        }
    }
}

/// The player before a source is found: it opens with the tap, and Back stops the search.
#[component]
pub fn PendingPlayer(title: String, step: String) -> Element {
    let mut shell = use_context::<Shell>();
    rsx! {
        div { class: "player pending", role: "dialog", aria_label: "{title}",
            div { class: "player-top", onkeydown: move |e: KeyboardEvent| if matches!(e.key(), Key::Enter | Key::Tab) || e.key() == Key::Character(" ".into()) { e.stop_propagation(); },
                button { class: "bare", aria_label: "Back (Esc)", title: "Back (Esc)", onclick: move |_| crate::cancel_finding(shell), icons::ArrowLeft {} }
                div { class: "player-heading",
                    span { class: "player-title display", "{title}" }
                    span { class: "player-facts-line", span { "{step}" } }
                }
                span { class: "topbar-spacer" }
                button { class: "bare", aria_label: "Page shortcuts (?)", title: "Page shortcuts (?)", onclick: move |_| shell.shortcuts_open.set(true), icons::Keyboard {} }
            }
            div { class: "player-center", role: "status", aria_label: "{step}",
                div { class: "player-pending-note", div { class: "player-spinner" } span { class: "dim", "{step}" } }
            }
        }
    }
}

#[component]
fn OptionRow(on: bool, label: String, about: String, pick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button { class: if on { "player-option on" } else { "player-option" }, role: "menuitemradio", aria_checked: "{on}", onclick: move |e| pick.call(e),
            span { class: "player-option-text",
                span { class: "player-option-label", "{label}" }
                if !about.is_empty() { span { class: "player-option-about dim", "{about}" } }
            }
            if on { span { class: "player-option-check", icons::Check {} } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fallback, Fallback};
    use crate::engine::Track;
    fn track(id: i64, audio: bool, lang: &str) -> Track { Track { id, audio, lang: lang.into(), ..Default::default() } }

    #[test]
    fn subtitles_follow_the_audio_language_setting() {
        let it = Some("italian");
        // MULTI with Italian audio: mpv's alang picks it, and a flagged subtitle is turned off.
        assert_eq!(fallback(&[track(1, true, "eng"), track(2, true, "ita")], it), Fallback::Off(Some(2)));
        assert_eq!(fallback(&[track(1, true, "eng"), track(2, true, "ita"), track(3, false, "eng")], it), Fallback::Off(Some(2)));
        assert_eq!(fallback(&[track(1, true, "eng"), track(2, true, "ita"), Track { selected: true, ..track(3, false, "eng") }], it), Fallback::Off(Some(2)));
        // "Any language": the file's default audio, no subtitles even when one is flagged.
        assert_eq!(fallback(&[track(1, true, "eng"), track(2, true, "ita"), track(3, false, "eng")], None), Fallback::Off(None));
        // Untagged audio can't be told apart, alone or next to another language: left alone.
        assert_eq!(fallback(&[track(1, true, "")], it), Fallback::Nothing);
        assert_eq!(fallback(&[track(1, true, ""), track(2, true, "eng")], it), Fallback::Nothing);
        // ...but it does not hide your language when that track is there too.
        assert_eq!(fallback(&[track(1, true, ""), track(2, true, "ita")], it), Fallback::Off(Some(2)));
        // Forced Italian (signs only) is skipped for the full track; an image track only if nothing else.
        let forced = Track { forced: true, ..track(5, false, "ita") };
        let pgs = Track { image: true, ..track(6, false, "ita") };
        assert_eq!(fallback(&[track(1, true, "eng"), forced.clone(), pgs.clone(), track(7, false, "ita")], it), Fallback::Embedded(7));
        assert_eq!(fallback(&[track(1, true, "eng"), forced.clone(), pgs], it), Fallback::Embedded(6));
        assert_eq!(fallback(&[track(1, true, "eng"), forced], it), Fallback::Download);
        // English only, with Italian subtitles in the file: those.
        assert_eq!(fallback(&[track(1, true, "eng"), track(3, false, "eng"), track(4, false, "ita")], it), Fallback::Embedded(4));
        // English only, no Italian subtitles in the file: download.
        assert_eq!(fallback(&[track(1, true, "eng"), track(3, false, "eng")], it), Fallback::Download);
        // Commentary in your language does not suppress subtitles over the main foreign audio.
        let commentary = Track { label: "English director commentary".into(), ..track(2, true, "eng") };
        assert_eq!(fallback(&[track(1, true, "jpn"), commentary], Some("english")), Fallback::Download);
        // Italian subtitles already on (forced by the file): nothing.
        assert_eq!(fallback(&[track(1, true, "eng"), Track { selected: true, ..track(4, false, "it") }], it), Fallback::Nothing);
        // A subtitle language tag with a region still counts.
        assert_eq!(fallback(&[track(1, true, "eng"), track(2, false, "it-IT")], it), Fallback::Embedded(2));
    }
    #[test]
    fn clock_reads_like_a_player() {
        assert_eq!(super::clock(5.0), "0:05");
        assert_eq!(super::clock(605.4), "10:05");
        assert_eq!(super::clock(3725.0), "1:02:05");
        assert_eq!(super::clock(-3.0), "0:00");
    }
}
