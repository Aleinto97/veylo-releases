//! What the player decides, without any UI: the subtitle choice that follows the Audio
//! language setting, the skip it offers, the details sheet and the clock. Shared by the
//! Dioxus controls (player_ui.rs).
use crate::{backend, engine::{self, Finish, Segment, SegmentKind, State}};

/// Downloadable subtitles listed before a language is typed.
pub const ONLINE_SHOWN: usize = 60;

pub const SPEEDS: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
pub const KEYS: [(&[&str], &str); 20] = [
    (&["Enter"], "Skip the intro or recap, when offered"),
    (&["Space", "K", "Enter"], "Play or pause"), (&["←", "→"], "Back or forward 10 seconds"), (&["J", "L"], "Back or forward 10 seconds"),
    (&["0–9"], "Jump to 0–90%"), (&["Home"], "Back to the start"), (&["↑", "↓"], "Volume"), (&["M"], "Mute"),
    (&["A"], "Next audio track"), (&["C"], "Next subtitles, then off"), (&["S"], "Subtitles list"), (&["Z"], "Fit, fill, stretch"),
    (&["[", "]", "\\"], "Slower, faster, normal speed"), (&["I"], "Details"), (&["H", "?"], "Show or hide shortcuts"),
    (&["N"], "Next episode"), (&["Esc"], "Close panel, leave fullscreen, or close player"), (&["Q"], "Close player"),
    (&["F"], "Fullscreen"), (&["Tab"], "Next control; Shift+Tab goes back"),
];
pub const ANDROID: bool = cfg!(target_os = "android");

/// The skip the player offers for a segment: credits only lead to the next episode.
pub fn offer(segments: &[Segment], position: f64, has_next: bool) -> Option<Segment> {
    engine::skip_offer(segments, position).filter(|g| g.kind != SegmentKind::Credits || has_next)
}

pub fn skip(live: &engine::Live, g: Segment) {
    if g.kind == SegmentKind::Credits { live.finish(Finish::Next) } else { live.seek_to(g.end) }
}

pub fn clock(seconds: f64) -> String {
    let t = seconds.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// What to do when a video starts, for the audio-language setting (`None` is "Any language").
#[derive(Debug, PartialEq)]
pub enum Fallback {
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
pub fn fallback(tracks: &[engine::Track], key: Option<&str>) -> Fallback {
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

/// File details, plainly: what is decoded, how, and what the screen shows.
pub fn details(live: &engine::Live, s: &State, facts: &crate::player::Facts, hw: Option<&str>) -> Vec<(&'static str, String)> {
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
