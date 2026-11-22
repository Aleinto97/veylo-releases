//! The audio tracks a video file declares in its header, read from its first bytes before it
//! plays: Matroska (EBML Tracks) and MP4 (moov). Play uses them to choose a file that really has
//! the audio language you set, as labels written by add-ons and scrapers are often wrong.

/// One audio track: its language tag as the file has it ("eng", "it", "" when untagged or "und")
/// and its name ("English commentary").
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack { pub lang: String, pub name: String }

impl AudioTrack {
    /// Commentary or audio description: not the film's own sound (as `engine::extra_audio`).
    pub fn extra(&self) -> bool {
        let n = self.name.to_lowercase();
        n.contains("commentary") || n.contains("description") || n.contains("descriptive")
    }
}

/// Whether a file's main audio is in the Audio-language setting's language: Some(true) or
/// Some(false) when the tracks say so, None when they can't (an untagged track, no audio).
pub fn in_language(tracks: &[AudioTrack], key: &str) -> Option<bool> {
    let main: Vec<&AudioTrack> = tracks.iter().filter(|t| !t.extra()).collect();
    if main.iter().any(|t| crate::backend::track_in_language(&t.lang, key)) { return Some(true); }
    if main.is_empty() || main.iter().any(|t| t.lang.is_empty()) { return None; }
    Some(false)
}

/// Whether the file's first main audio track (what plays by default, usually the release's own
/// language) is in the setting's language: an original release rather than a dub collection.
pub fn primary_in_language(tracks: &[AudioTrack], key: &str) -> bool {
    tracks.iter().find(|t| !t.extra()).is_some_and(|t| crate::backend::track_in_language(&t.lang, key))
}

/// How many bytes from the start of a file are read for its header.
pub const HEAD_BYTES: usize = 1 << 20;

/// The audio tracks in a file's first bytes; None when the header isn't there (MP4 with its index
/// at the end, Matroska with large attachments first) or the format is another (m2ts, AVI).
pub fn audio_tracks(head: &[u8]) -> Option<Vec<AudioTrack>> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) { matroska(head) } else if head.get(4..8) == Some(b"ftyp") { mp4(head) } else { None }
}

// ---------------------------------------------------------------- Matroska

/// An EBML variable-size integer: (value, length). `keep_marker` for element IDs.
fn vint(b: &[u8], keep_marker: bool) -> Option<(u64, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || b.len() < len { return None; }
    let mut v = if keep_marker { first as u64 } else { (first as u64) & (0xFF >> len) };
    for x in &b[1..len] { v = (v << 8) | *x as u64; }
    Some((v, len))
}

/// The elements in `b`: (id, payload start, payload end, whole); an unknown size runs to the
/// end, and an element cut by the end of `b` is not whole.
fn elements(b: &[u8]) -> Vec<(u64, usize, usize, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let Some((id, il)) = vint(&b[at..], true) else { break };
        let Some((size, sl)) = vint(&b[at + il..], false) else { break };
        let start = at + il + sl;
        let unknown = size == (1u64 << (7 * sl)) - 1;
        let end = if unknown { b.len() } else { start.saturating_add(size as usize) };
        if end > b.len() && !unknown { out.push((id, start.min(b.len()), b.len(), false)); break; }
        out.push((id, start, end, true));
        if unknown { break; }
        at = end;
    }
    out
}

fn matroska(head: &[u8]) -> Option<Vec<AudioTrack>> {
    const SEGMENT: u64 = 0x1853_8067;
    const TRACKS: u64 = 0x1654_AE6B;
    const CLUSTER: u64 = 0x1F43_B675;
    let (_, seg_start, seg_end, _) = elements(head).into_iter().find(|e| e.0 == SEGMENT)?;
    let segment = &head[seg_start..seg_end];
    for (id, start, end, whole) in elements(segment) {
        if id == CLUSTER { return None; }
        if id != TRACKS { continue; }
        // A cut Tracks element would hide tracks.
        if !whole { return None; }
        return Some(elements(&segment[start..end]).into_iter().filter(|e| e.0 == 0xAE && e.3).filter_map(|(_, s, e, _)| {
            let entry = &segment[start + s..start + e];
            let mut audio = false;
            let (mut lang, mut bcp47, mut name) = (None::<String>, None::<String>, String::new());
            for (id, s, e, _) in elements(entry) {
                let v = &entry[s..e];
                match id {
                    0x83 => audio = v.iter().fold(0u64, |a, x| (a << 8) | *x as u64) == 2,
                    0x22B59C => lang = Some(text(v)),
                    0x22B59D => bcp47 = Some(text(v)),
                    0x536E => name = text(v),
                    _ => {}
                }
            }
            // Matroska's default language is English.
            let lang = bcp47.or(lang).unwrap_or_else(|| "eng".into());
            audio.then(|| AudioTrack { lang: if lang == "und" { String::new() } else { lang }, name })
        }).collect());
    }
    None
}

fn text(v: &[u8]) -> String { String::from_utf8_lossy(v).trim_end_matches('\0').trim().to_owned() }

// ---------------------------------------------------------------- MP4

/// The boxes in `b`: (type, payload start, payload end).
fn boxes(b: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= b.len() {
        let size = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = b[at + 4..at + 8].try_into().unwrap();
        let (header, size) = match size {
            1 if at + 16 <= b.len() => (16, u64::from_be_bytes(b[at + 8..at + 16].try_into().unwrap())),
            0 => (8, (b.len() - at) as u64),
            n => (8, n),
        };
        if size < header as u64 { break; }
        let end = at.saturating_add(size as usize);
        out.push((kind, at + header, end.min(b.len())));
        if end > b.len() { break; }
        at = end;
    }
    out
}

fn mp4(head: &[u8]) -> Option<Vec<AudioTrack>> {
    let top = boxes(head);
    let (_, ms, me) = *top.iter().find(|b| &b.0 == b"moov")?;
    // The index must be whole.
    let declared = u32::from_be_bytes(head[ms - 8..ms - 4].try_into().ok()?) as usize;
    if declared > 1 && ms - 8 + declared > head.len() { return None; }
    let moov = &head[ms..me];
    Some(boxes(moov).into_iter().filter(|b| &b.0 == b"trak").filter_map(|(_, ts, te)| {
        let trak = &moov[ts..te];
        let (_, ds, de) = boxes(trak).into_iter().find(|b| &b.0 == b"mdia")?;
        let mdia = &trak[ds..de];
        let parts = boxes(mdia);
        let (_, hs, he) = *parts.iter().find(|b| &b.0 == b"hdlr")?;
        let hdlr = &mdia[hs..he];
        if hdlr.get(8..12) != Some(b"soun") { return None; }
        let name = hdlr.get(24..).map(text).unwrap_or_default();
        let elng = parts.iter().find(|b| &b.0 == b"elng").and_then(|(_, s, e)| mdia.get(s + 4..*e)).map(text);
        let packed = parts.iter().find(|b| &b.0 == b"mdhd").and_then(|(_, s, _)| {
            let at = s + if mdia.get(*s) == Some(&1) { 32 } else { 20 };
            Some(u16::from_be_bytes(mdia.get(at..at + 2)?.try_into().ok()?))
        });
        let lang = elng.filter(|l| !l.is_empty()).or_else(|| packed.map(|p| (0..3).rev().map(|i| (((p >> (5 * i)) & 0x1F) as u8 + 0x60) as char).collect()))
            .unwrap_or_default();
        Some(AudioTrack { lang: if lang == "und" || !lang.chars().all(|c| c.is_ascii_alphabetic() || c == '-') { String::new() } else { lang }, name })
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend([0x01, 0, 0, 0, 0, 0, 0, 0]);
        v[id.len() + 1..id.len() + 8].copy_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
        v.extend(payload);
        v
    }

    fn mkv(entries: &[(u8, Option<&str>, &str)]) -> Vec<u8> {
        let tracks: Vec<u8> = entries.iter().flat_map(|(kind, lang, name)| {
            let mut e = el(&[0x83], &[*kind]);
            if let Some(l) = lang { e.extend(el(&[0x22, 0xB5, 0x9C], l.as_bytes())); }
            if !name.is_empty() { e.extend(el(&[0x53, 0x6E], name.as_bytes())); }
            el(&[0xAE], &e)
        }).collect();
        let mut segment = el(&[0x15, 0x49, 0xA9, 0x66], b"info");
        segment.extend(el(&[0x16, 0x54, 0xAE, 0x6B], &tracks));
        segment.extend(el(&[0x1F, 0x43, 0xB6, 0x75], &[0; 64]));
        let mut file = el(&[0x1A, 0x45, 0xDF, 0xA3], b"header");
        // A live segment: unknown size.
        file.extend([0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        file.extend(segment);
        file
    }

    #[test]
    fn matroska_audio_tracks() {
        let file = mkv(&[(1, None, ""), (2, Some("kor"), ""), (2, None, "English commentary"), (2, Some("und"), ""), (17, Some("eng"), "")]);
        assert_eq!(audio_tracks(&file).unwrap(), [
            AudioTrack { lang: "kor".into(), name: "".into() },
            AudioTrack { lang: "eng".into(), name: "English commentary".into() },
            AudioTrack { lang: "".into(), name: "".into() },
        ]);
        // Cut before the tracks end: unknown, not "no audio".
        assert_eq!(audio_tracks(&file[..60]), None);
        assert_eq!(audio_tracks(b"\x47\x40\x00\x10 m2ts"), None);
    }

    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(payload);
        v
    }

    fn trak(handler: &[u8; 4], lang: &str, name: &str) -> Vec<u8> {
        let l = lang.as_bytes();
        let packed = l.iter().fold(0u16, |a, c| (a << 5) | (*c - 0x60) as u16);
        let mut mdhd = vec![0u8; 20];
        mdhd.extend(packed.to_be_bytes());
        mdhd.extend([0, 0]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(handler);
        hdlr.extend([0u8; 12]);
        hdlr.extend(name.as_bytes());
        hdlr.push(0);
        let mut mdia = bx(b"mdhd", &mdhd);
        mdia.extend(bx(b"hdlr", &hdlr));
        bx(b"trak", &bx(b"mdia", &mdia))
    }

    #[test]
    fn language_from_the_tracks() {
        let t = |lang: &str, name: &str| AudioTrack { lang: lang.into(), name: name.into() };
        assert_eq!(in_language(&[t("kor", ""), t("fre", "")], "english"), Some(false));
        assert_eq!(in_language(&[t("kor", ""), t("eng", "Director's commentary")], "english"), Some(false));
        assert_eq!(in_language(&[t("kor", ""), t("en", "")], "english"), Some(true));
        assert_eq!(in_language(&[t("kor", ""), t("", "")], "english"), None);
        assert_eq!(in_language(&[], "english"), None);
        assert_eq!(in_language(&[t("ita", ""), t("eng", "")], "italian"), Some(true));
        assert!(primary_in_language(&[t("eng", "Commentary"), t("en", "")], "english"));
        assert!(!primary_in_language(&[t("rus", ""), t("eng", "")], "english"));
    }

    #[test]
    fn mp4_audio_tracks() {
        let mut moov = trak(b"vide", "und", "Video");
        moov.extend(trak(b"soun", "eng", "Stereo"));
        moov.extend(trak(b"soun", "ita", ""));
        let mut file = bx(b"ftyp", b"isomiso2");
        file.extend(bx(b"moov", &moov));
        file.extend(bx(b"mdat", &[0; 32]));
        assert_eq!(audio_tracks(&file).unwrap(), [AudioTrack { lang: "eng".into(), name: "Stereo".into() }, AudioTrack { lang: "ita".into(), name: "".into() }]);
        // The index at the end (not fast-start): unknown.
        let mut late = bx(b"ftyp", b"isomiso2");
        late.extend(bx(b"mdat", &[0; 32]));
        assert_eq!(audio_tracks(&late), None);
    }
}
