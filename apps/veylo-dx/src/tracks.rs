//! What a video file declares in its header, read from its first bytes before it plays:
//! Matroska (EBML Info and Tracks) and MP4 (moov). Play uses its audio tracks to choose a file
//! that really has the audio language you set, and its picture (size, HDR, Dolby Vision,
//! length) to rank files by what they are, as labels written by add-ons and scrapers are often
//! wrong.

/// One audio track: its language tag as the file has it ("eng", "it", "" when untagged or "und"),
/// its name ("English commentary"), and whether the container flagged it as commentary.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack { pub lang: String, pub name: String, pub commentary: bool }

/// True when an audio track title names a commentary, audio-description, or isolated-score/M&E
/// track rather than the film or episode's main dialogue audio.
pub fn is_extra_audio_name(name: &str) -> bool {
    let n = name.to_lowercase();
    const PHRASES: &[&str] = &[
        "commentary", "audio description", "audio-description", "audiodescription",
        "descriptive", "description", "visually impaired", "visual impaired",
        "(ad)", "[ad]", "(vi)", "[vi]", "(comm)", "[comm]", "audio comm",
        "cast & crew", "cast and crew", "cast/crew", "film historian", "trivia track",
        "director's track", "directors track", "with director", "with the director",
        "by director", "by the director", "isolated score", "score only", "music only",
        "music & effects", "music and effects", "m&e",
        "commento", "commenti", "audiodescrizione", "audio descrizione",
        "comentario", "comentarios", "audiodescripcion", "audiodescripción",
        "audio descripcion", "audio descripción",
        "commentaire", "commentaires",
        "kommentar", "kommentare", "audiodeskription", "hörfilm", "hoerfilm", "bildbeschreibung",
        "comentário", "comentários", "audiodescrição", "audiodescricao",
        "commentaar", "audiobeschrijving",
        "komentarz", "audiodeskrypcja",
        "комментар", "тифлокоммент",
        "コメンタリー", "音声解説", "副音声",
        "코멘터리", "음성 해설",
        "评论音轨", "評論音軌", "导评", "導評", "口述影像",
        "sesli betimleme", "yönetmen yorumu",
    ];
    if PHRASES.iter().any(|p| n.contains(p)) { return true; }
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    words.iter().enumerate().any(|(i, w)| {
        matches!(*w, "comm" | "comment" | "dvs")
            || (*w == "ad" && (words.len() == 1
                || i + 1 == words.len()
                || words[i + 1].as_bytes().first().is_some_and(u8::is_ascii_digit)
                || matches!(words[i + 1], "stereo" | "mono" | "surround" | "dolby" | "ac3" | "eac3" | "aac" | "dd" | "ddp" | "track" | "audio")))
    })
}

impl AudioTrack {
    /// Commentary or audio description: not the film's own sound (as `engine::extra_audio`).
    pub fn extra(&self) -> bool {
        self.commentary || is_extra_audio_name(&self.name)
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

/// How the picture's brightness is coded: the HDR question. Unknown when the header doesn't say
/// (many files leave it out; their label then decides).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Transfer { #[default] Unknown, Sdr, Pq, Hlg }

impl Transfer {
    /// ITU-T H.273 transfer characteristics, as Matroska and MP4 write them.
    fn of(code: u64) -> Self { match code { 16 => Self::Pq, 18 => Self::Hlg, 1 | 6 | 14 | 15 => Self::Sdr, _ => Self::Unknown } }
}

/// A file's video as its header declares it.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub transfer: Transfer,
    /// The Dolby Vision profile when there is a Dolby Vision layer (5: Dolby Vision alone, with
    /// no HDR10 picture under it; 7 and 8: over an HDR10 one).
    pub dolby_vision: Option<u8>,
    /// Its length, when the header gives a plausible one.
    pub seconds: Option<f64>,
}

impl Picture {
    /// The Play order's HDR scale (`hdr_tier`: HDR10+ 3, HDR 2, Dolby Vision alone 1, SDR 0) as far
    /// as the header can tell; HDR10+ isn't in it (its metadata is in the frames). None: it can't.
    pub fn hdr_tier(&self) -> Option<u8> {
        match (self.transfer, self.dolby_vision) {
            (_, Some(5)) => Some(1),
            (Transfer::Pq | Transfer::Hlg, _) => Some(2),
            (Transfer::Sdr, None) => Some(0),
            _ => None,
        }
    }
}

/// The picture in a file's first bytes; None when its header isn't there or names no video.
pub fn picture(head: &[u8]) -> Option<Picture> {
    let p = if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) { matroska_picture(head) } else if head.get(4..8) == Some(b"ftyp") { mp4_picture(head) } else { None }?;
    (p.width > 0 && p.height > 0).then_some(p)
}

/// A length only when it is a film's or an episode's: a minute to six hours.
fn plausible(seconds: f64) -> Option<f64> { (60.0..=6.0 * 3600.0).contains(&seconds).then_some(seconds) }

/// A Dolby Vision configuration record's profile.
fn dv_profile(record: &[u8]) -> Option<u8> { record.get(2).map(|b| b >> 1) }

/// How many bytes from the start of a file are read for its header.
pub const HEAD_BYTES: usize = 1 << 20;

/// The audio tracks in a file's first bytes; None when the header isn't there (MP4 with its index
/// at the end, Matroska with large attachments first) or the format is another (m2ts, AVI).
pub fn audio_tracks(head: &[u8]) -> Option<Vec<AudioTrack>> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) { matroska(head) } else if head.get(4..8) == Some(b"ftyp") { mp4(head) } else { None }
}

/// True when `head` begins like an MP4 whose `moov` index is placed after `mdat` (not fast-start),
/// so a single tail range request can recover its audio tracks, resolution, HDR, and duration.
pub fn mp4_moov_at_end(head: &[u8]) -> bool {
    if head.get(4..8) != Some(b"ftyp") { return false; }
    let top = boxes(head);
    let has_moov = top.iter().any(|b| &b.0 == b"moov");
    let has_mdat = top.iter().any(|b| &b.0 == b"mdat");
    !has_moov && has_mdat
}

/// Parse audio tracks, picture, and video codec from the tail bytes of a non-fast-start MP4 file.
pub fn parse_mp4_tail(tail: &[u8]) -> (Option<Vec<AudioTrack>>, Option<Picture>, Option<&'static str>) {
    // Scan for a plausible top-level `moov` box header (`[size: u32 BE] "moov"`) whose declared
    // size fits inside the remainder of `tail`. Parse ONLY the declared box extent and require
    // real track metadata so an accidental `moov` signature inside media payload never short-circuits.
    let mut inspected = 0usize;
    for i in 4..tail.len().saturating_sub(4) {
        if &tail[i..i + 4] != b"moov" { continue; }
        let start = i - 4;
        let size = u32::from_be_bytes(tail[start..i].try_into().unwrap()) as usize;
        let end = if size == 1 && start + 16 <= tail.len() {
            let sz64 = u64::from_be_bytes(tail[start + 8..start + 16].try_into().unwrap());
            usize::try_from(sz64).ok().filter(|&n| n >= 24).and_then(|n| start.checked_add(n)).filter(|&e| e <= tail.len())
        } else if size >= 16 {
            start.checked_add(size).filter(|&e| e <= tail.len())
        } else {
            None
        };
        let Some(end) = end else { continue };
        inspected += 1;
        if inspected > 32 { break; }
        let slice = &tail[start..end];
        let tracks = mp4(slice).filter(|t| !t.is_empty());
        let pic = mp4_picture(slice).filter(|p| p.width > 0 && p.height > 0);
        let codec = mp4_video_codec(slice);
        if tracks.is_some() || pic.is_some() || codec.is_some() {
            return (tracks, pic, codec);
        }
    }
    (None, None, None)
}

/// Video codec declared in the container header ("av1", "hevc", "avc"), when readable.
pub fn video_codec(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        matroska_video_codec(head)
    } else if head.get(4..8) == Some(b"ftyp") || head.get(4..8) == Some(b"moov") {
        mp4_video_codec(head)
    } else {
        None
    }
}

fn matroska_video_codec(head: &[u8]) -> Option<&'static str> {
    const SEGMENT: u64 = 0x1853_8067;
    const TRACKS: u64 = 0x1654_AE6B;
    const CLUSTER: u64 = 0x1F43_B675;
    let (_, seg_start, seg_end, _) = elements(head).into_iter().find(|e| e.0 == SEGMENT)?;
    let segment = &head[seg_start..seg_end];
    for (id, start, end, whole) in elements(segment) {
        if id == CLUSTER { return None; }
        if id != TRACKS { continue; }
        if !whole { return None; }
        let tracks = &segment[start..end];
        for (_, s, e, _) in elements(tracks).filter(|e| e.0 == 0xAE && e.3) {
            let entry = &tracks[s..e];
            let mut video = false;
            let mut codec_id = None;
            for (id, s, e, _) in elements(entry) {
                match id {
                    0x83 => video = uint(&entry[s..e]) == 1,
                    0x86 => codec_id = Some(text(&entry[s..e])),
                    _ => {}
                }
            }
            if video {
                return match codec_id.as_deref() {
                    Some(c) if c.contains("AV1") => Some("av1"),
                    Some(c) if c.contains("HEVC") => Some("hevc"),
                    Some(c) if c.contains("AVC") => Some("avc"),
                    _ => None,
                };
            }
        }
        return None;
    }
    None
}

fn mp4_video_codec(head: &[u8]) -> Option<&'static str> {
    let top = boxes(head);
    let (_, ms, me, whole) = *top.iter().find(|b| &b.0 == b"moov")?;
    if !whole { return None; }
    let moov = &head[ms..me];
    let child = |b: &[u8], kind: &[u8; 4]| boxes(b).into_iter().find(|x| &x.0 == kind).map(|(_, s, e, _)| (s, e));
    boxes(moov).into_iter().filter(|b| &b.0 == b"trak").find_map(|(_, ts, te, _)| {
        let trak = &moov[ts..te];
        let (ds, de) = child(trak, b"mdia")?;
        let mdia = &trak[ds..de];
        let (hs, he) = child(mdia, b"hdlr")?;
        if mdia[hs..he].get(8..12) != Some(b"vide") { return None; }
        let (ns, ne) = child(mdia, b"minf")?;
        let minf = &mdia[ns..ne];
        let (ls, le) = child(minf, b"stbl")?;
        let stbl = &minf[ls..le];
        let (ss, se) = child(stbl, b"stsd")?;
        let entries = stbl[ss..se].get(8..)?;
        let (fourcc, _, _, _) = *boxes(entries).first()?;
        match &fourcc {
            b"av01" => Some("av1"),
            b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => Some("hevc"),
            b"avc1" | b"avc3" => Some("avc"),
            _ => None,
        }
    })
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

/// Lazily walk sibling EBML elements without allocating a Vec (capped to bound hostile headers).
struct EbmlIter<'a> { b: &'a [u8], at: usize, left: usize, done: bool }
impl Iterator for EbmlIter<'_> {
    type Item = (u64, usize, usize, bool);
    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.left == 0 || self.at >= self.b.len() { return None; }
        self.left -= 1;
        let (id, il) = vint(&self.b[self.at..], true)?;
        let (size, sl) = vint(&self.b[self.at + il..], false)?;
        let start = self.at + il + sl;
        let unknown = size == (1u64 << (7 * sl)) - 1;
        let size_usize = usize::try_from(size).unwrap_or(usize::MAX);
        let end = if unknown { self.b.len() } else { start.saturating_add(size_usize) };
        if end > self.b.len() && !unknown {
            self.done = true;
            return Some((id, start.min(self.b.len()), self.b.len(), false));
        }
        if unknown { self.done = true; } else { self.at = end; }
        Some((id, start, end, true))
    }
}

fn elements(b: &[u8]) -> EbmlIter<'_> {
    EbmlIter { b, at: 0, left: 4096, done: false }
}

fn matroska(head: &[u8]) -> Option<Vec<AudioTrack>> {
    const SEGMENT: u64 = 0x1853_8067;
    const TRACKS: u64 = 0x1654_AE6B;
    const CLUSTER: u64 = 0x1F43_B675;
    let (_, seg_start, seg_end, _) = elements(head).find(|e| e.0 == SEGMENT)?;
    let segment = &head[seg_start..seg_end];
    for (id, start, end, whole) in elements(segment) {
        if id == CLUSTER { return None; }
        if id != TRACKS { continue; }
        // A cut Tracks element would hide tracks.
        if !whole { return None; }
        return Some(elements(&segment[start..end]).filter(|e| e.0 == 0xAE && e.3).filter_map(|(_, s, e, _)| {
            let entry = &segment[start + s..start + e];
            let mut audio = false;
            let mut commentary = false;
            let (mut lang, mut bcp47, mut name) = (None::<String>, None::<String>, String::new());
            for (id, s, e, _) in elements(entry) {
                let v = &entry[s..e];
                match id {
                    0x83 => audio = v.iter().fold(0u64, |a, x| (a << 8) | *x as u64) == 2,
                    0x22B59C => lang = Some(text(v)),
                    0x22B59D => bcp47 = Some(text(v)),
                    0x536E => name = text(v),
                    // RFC 9559: FlagCommentary (0x55AF) and FlagVisualImpaired (0x55AC); 0x55EE is MaxBlockAdditionID.
                    0x55AF | 0x55AC if uint(v) != 0 => commentary = true,
                    _ => {}
                }
            }
            // RFC 9559: LanguageBCP47 takes precedence when present (even if "und"); only absent
            // language elements default to English.
            let lang = bcp47.filter(|l| !l.is_empty())
                .or_else(|| lang.filter(|l| !l.is_empty()))
                .unwrap_or_else(|| "eng".into());
            audio.then(|| AudioTrack {
                lang: if lang.eq_ignore_ascii_case("und") { String::new() } else { lang },
                name,
                commentary,
            })
        }).collect());
    }
    None
}

fn text(v: &[u8]) -> String { String::from_utf8_lossy(v).trim_end_matches('\0').trim().to_owned() }

fn uint(v: &[u8]) -> u64 { v.iter().take(8).fold(0u64, |a, x| (a << 8) | *x as u64) }

fn float(v: &[u8]) -> Option<f64> {
    match v.len() { 4 => Some(f32::from_be_bytes(v.try_into().ok()?) as f64), 8 => Some(f64::from_be_bytes(v.try_into().ok()?)), _ => None }
}

/// Matroska's first video track (Tracks), with its colour and Dolby Vision mapping, and the
/// length from Info.
fn matroska_picture(head: &[u8]) -> Option<Picture> {
    const SEGMENT: u64 = 0x1853_8067;
    const INFO: u64 = 0x1549_A966;
    const TRACKS: u64 = 0x1654_AE6B;
    const CLUSTER: u64 = 0x1F43_B675;
    let (_, seg_start, seg_end, _) = elements(head).into_iter().find(|e| e.0 == SEGMENT)?;
    let segment = &head[seg_start..seg_end];
    let mut seconds = None;
    let mut found: Option<Picture> = None;
    for (id, start, end, whole) in elements(segment) {
        if id == CLUSTER { break; }
        if id == INFO && whole {
            let info = &segment[start..end];
            let (mut scale, mut duration) = (1_000_000u64, None);
            for (id, s, e, _) in elements(info) {
                match id { 0x2AD7B1 => scale = uint(&info[s..e]).max(1), 0x4489 => duration = float(&info[s..e]), _ => {} }
            }
            seconds = duration.and_then(|d| plausible(d * scale as f64 / 1e9));
            if let Some(ref mut p) = found { p.seconds = seconds; break; }
            continue;
        }
        if id != TRACKS || found.is_some() { continue; }
        if !whole { return None; }
        let tracks = &segment[start..end];
        for (_, s, e, _) in elements(tracks).filter(|e| e.0 == 0xAE && e.3) {
            let entry = &tracks[s..e];
            let mut picture = Picture { seconds, ..Picture::default() };
            let mut video = false;
            for (id, s, e, _) in elements(entry) {
                let v = &entry[s..e];
                match id {
                    0x83 => video = uint(v) == 1,
                    0xE0 => for (id, s, e, _) in elements(v) {
                        match id {
                            0xB0 => picture.width = uint(&v[s..e]) as u32,
                            0xBA => picture.height = uint(&v[s..e]) as u32,
                            0x55B0 => if let Some((_, cs, ce, _)) = elements(&v[s..e]).into_iter().find(|c| c.0 == 0x55BA) {
                                picture.transfer = Transfer::of(uint(&v[s..e][cs..ce]));
                            },
                            _ => {}
                        }
                    },
                    // BlockAdditionMapping: a Dolby Vision configuration ("dvcC", "dvvC").
                    0x41E4 => {
                        let mut kind = None;
                        let mut profile = None;
                        for (mid, ms, me, _) in elements(v) {
                            match mid {
                                0x41E7 => kind = Some(uint(&v[ms..me])),
                                0x41ED => profile = dv_profile(&v[ms..me]),
                                _ => {}
                            }
                        }
                        if matches!(kind, Some(0x6476_6343 | 0x6476_7643)) {
                            picture.dolby_vision = profile;
                        }
                    }
                    _ => {}
                }
            }
            if video {
                found = Some(picture);
                break;
            }
        }
        if found.is_some() && seconds.is_some() { break; }
    }
    found
}

// ---------------------------------------------------------------- MP4

/// The boxes in `b`: (type, payload start, payload end, whole). A box is whole when all of it is
/// in `b`; one sized "to the end of the file" (size 0) can't be known whole from a file's head.
fn boxes(b: &[u8]) -> Vec<([u8; 4], usize, usize, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= b.len() {
        let size = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as u64;
        let size_field = size;
        let kind: [u8; 4] = b[at + 4..at + 8].try_into().unwrap();
        let (header, size) = match size {
            1 if at + 16 <= b.len() => (16, u64::from_be_bytes(b[at + 8..at + 16].try_into().unwrap())),
            0 => (8, (b.len() - at) as u64),
            n => (8, n),
        };
        if size < header as u64 { break; }
        let size_usize = usize::try_from(size).unwrap_or(usize::MAX);
        let end = at.saturating_add(size_usize);
        out.push((kind, at + header, end.min(b.len()), size_field != 0 && end <= b.len()));
        if end > b.len() { break; }
        at = end;
    }
    out
}

fn mp4(head: &[u8]) -> Option<Vec<AudioTrack>> {
    let top = boxes(head);
    let (_, ms, me, whole) = *top.iter().find(|b| &b.0 == b"moov")?;
    // The index must be whole (whatever form its size is written in).
    if !whole { return None; }
    let moov = &head[ms..me];
    Some(boxes(moov).into_iter().filter(|b| &b.0 == b"trak").filter_map(|(_, ts, te, _)| {
        let trak = &moov[ts..te];
        let trak_boxes = boxes(trak);
        let (_, ds, de, _) = *trak_boxes.iter().find(|b| &b.0 == b"mdia")?;
        let mdia = &trak[ds..de];
        let parts = boxes(mdia);
        let (_, hs, he, _) = *parts.iter().find(|b| &b.0 == b"hdlr")?;
        let hdlr = &mdia[hs..he];
        if hdlr.get(8..12) != Some(b"soun") { return None; }
        let mut name = hdlr.get(24..).map(text).unwrap_or_default();
        let mut commentary = false;
        if let Some((_, us, ue, _)) = trak_boxes.iter().find(|b| &b.0 == b"udta") {
            let udta = &trak[*us..*ue];
            for (kind, s, e, _) in boxes(udta) {
                match &kind {
                    b"name" => {
                        let u = text(&udta[s..e]);
                        if !u.is_empty() {
                            if is_extra_audio_name(&u) { commentary = true; }
                            if name.is_empty() || name.eq_ignore_ascii_case("SoundHandler") { name = u; }
                        }
                    }
                    b"tagc" => {
                        if let Some(tag) = udta.get(s + 4..e).map(text) {
                            let t = tag.to_lowercase();
                            if t.contains("describes-video") || t.contains("auxiliary-content") || t.contains("commentary") || t.contains("visual-impaired") {
                                commentary = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let elng = parts.iter().find(|b| &b.0 == b"elng").and_then(|(_, s, e, _)| mdia.get(s + 4..*e)).map(text);
        let packed = parts.iter().find(|b| &b.0 == b"mdhd").and_then(|(_, s, e, _)| {
            let box_data = mdia.get(*s..*e)?;
            let offset = if box_data.first() == Some(&1) { 32 } else { 20 };
            Some(u16::from_be_bytes(box_data.get(offset..offset + 2)?.try_into().ok()?))
        });
        let lang = elng.filter(|l| !l.is_empty()).or_else(|| packed.map(|p| (0..3).rev().map(|i| (((p >> (5 * i)) & 0x1F) as u8 + 0x60) as char).collect()))
            .unwrap_or_default();
        // Allow BCP-47 numeric region subtags such as `es-419` (Latin-American Spanish).
        Some(AudioTrack {
            lang: if lang.eq_ignore_ascii_case("und") || !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { String::new() } else { lang },
            name,
            commentary,
        })
    }).collect())
}

/// MP4's first video track: its sample entry's size, colour ("colr" nclx) and Dolby Vision
/// configuration ("dvcC", "dvvC"), and the length from mvhd.
fn mp4_picture(head: &[u8]) -> Option<Picture> {
    let top = boxes(head);
    let (_, ms, me, whole) = *top.iter().find(|b| &b.0 == b"moov")?;
    if !whole { return None; }
    let moov = &head[ms..me];
    let seconds = boxes(moov).into_iter().find(|b| &b.0 == b"mvhd").and_then(|(_, s, e, _)| {
        let h = &moov[s..e];
        let (scale, duration) = if h.first() == Some(&1) { (uint(h.get(20..24)?), uint(h.get(24..32)?)) } else { (uint(h.get(12..16)?), uint(h.get(16..20)?)) };
        (scale > 0).then(|| duration as f64 / scale as f64).and_then(plausible)
    });
    let child = |b: &[u8], kind: &[u8; 4]| boxes(b).into_iter().find(|x| &x.0 == kind).map(|(_, s, e, _)| (s, e));
    boxes(moov).into_iter().filter(|b| &b.0 == b"trak").find_map(|(_, ts, te, _)| {
        let trak = &moov[ts..te];
        let (ds, de) = child(trak, b"mdia")?;
        let mdia = &trak[ds..de];
        let (hs, he) = child(mdia, b"hdlr")?;
        if mdia[hs..he].get(8..12) != Some(b"vide") { return None; }
        let (ns, ne) = child(mdia, b"minf")?;
        let minf = &mdia[ns..ne];
        let (ls, le) = child(minf, b"stbl")?;
        let stbl = &minf[ls..le];
        let (ss, se) = child(stbl, b"stsd")?;
        // stsd: version and flags, entry count, then the sample entries.
        let entries = stbl[ss..se].get(8..)?;
        let (_, es, ee, _) = *boxes(entries).first()?;
        let entry = &entries[es..ee];
        let mut picture = Picture { seconds, ..Picture::default() };
        picture.width = uint(entry.get(24..26)?) as u32;
        picture.height = uint(entry.get(26..28)?) as u32;
        // A visual sample entry's fields take 78 bytes; its own boxes follow.
        for (kind, s, e, _) in boxes(entry.get(78..)?) {
            let v = &entry[78 + s..78 + e];
            match &kind {
                b"colr" if v.get(..4) == Some(b"nclx") => picture.transfer = Transfer::of(uint(v.get(6..8).unwrap_or_default())),
                b"dvcC" | b"dvvC" => picture.dolby_vision = dv_profile(v),
                _ => {}
            }
        }
        Some(picture)
    })
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
            AudioTrack { lang: "kor".into(), name: "".into(), commentary: false },
            AudioTrack { lang: "eng".into(), name: "English commentary".into(), commentary: false },
            AudioTrack { lang: "".into(), name: "".into(), commentary: false },
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
        let t = |lang: &str, name: &str| AudioTrack { lang: lang.into(), name: name.into(), commentary: false };
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
    fn matroska_picture_size_hdr_dolby_vision_and_length() {
        let colour = el(&[0x55, 0xB0], &el(&[0x55, 0xBA], &[16]));
        let mut video = el(&[0xB0], &[0x0F, 0x00]);
        video.extend(el(&[0xBA], &[0x06, 0x48]));
        video.extend(colour);
        let mut entry = el(&[0x83], &[1]);
        entry.extend(el(&[0xE0], &video));
        let mut mapping = el(&[0x41, 0xE7], b"dvcC");
        mapping.extend(el(&[0x41, 0xED], &[1, 0, 8 << 1, 0, 0x10]));
        entry.extend(el(&[0x41, 0xE4], &mapping));
        let mut info = el(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]);
        info.extend(el(&[0x44, 0x89], &7_200_000.0f64.to_be_bytes()));
        let mut segment = el(&[0x15, 0x49, 0xA9, 0x66], &info);
        segment.extend(el(&[0x16, 0x54, 0xAE, 0x6B], &el(&[0xAE], &entry)));
        segment.extend(el(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]));
        let mut file = el(&[0x1A, 0x45, 0xDF, 0xA3], b"header");
        file.extend(el(&[0x18, 0x53, 0x80, 0x67], &segment));
        let p = picture(&file).unwrap();
        assert_eq!((p.width, p.height, p.transfer, p.dolby_vision), (3840, 1608, Transfer::Pq, Some(8)));
        assert_eq!(p.seconds, Some(7200.0));
        assert_eq!(p.hdr_tier(), Some(2), "Dolby Vision 8 over HDR10");
        assert_eq!(Picture { dolby_vision: Some(5), transfer: Transfer::Pq, ..p }.hdr_tier(), Some(1), "profile 5: Dolby Vision alone");
        assert_eq!(Picture { transfer: Transfer::Sdr, dolby_vision: None, ..p }.hdr_tier(), Some(0));
        assert_eq!(Picture { transfer: Transfer::Unknown, dolby_vision: None, ..p }.hdr_tier(), None, "the label decides");
        // The audio-only fixture has no video size: no picture.
        assert_eq!(picture(&mkv(&[(2, Some("eng"), "")])), None);
    }

    #[test]
    fn mp4_picture_from_the_sample_entry() {
        let mut entry = vec![0u8; 78];
        entry[24..26].copy_from_slice(&1920u16.to_be_bytes());
        entry[26..28].copy_from_slice(&800u16.to_be_bytes());
        let mut nclx = b"nclx".to_vec();
        nclx.extend([0, 1, 0, 1, 0, 1, 0]);
        entry.extend(bx(b"colr", &nclx));
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(bx(b"avc1", &entry));
        let stbl = bx(b"stbl", &bx(b"stsd", &stsd));
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"vide");
        hdlr.extend([0u8; 13]);
        let mut mdia = bx(b"hdlr", &hdlr);
        mdia.extend(bx(b"minf", &stbl));
        let mut mvhd = vec![0u8; 12];
        mvhd.extend(1000u32.to_be_bytes());
        mvhd.extend(5_400_000u32.to_be_bytes());
        mvhd.extend([0u8; 80]);
        let mut moov = bx(b"mvhd", &mvhd);
        moov.extend(bx(b"trak", &bx(b"mdia", &mdia)));
        let mut file = bx(b"ftyp", b"isomiso2");
        file.extend(bx(b"moov", &moov));
        let p = picture(&file).unwrap();
        assert_eq!((p.width, p.height, p.transfer, p.dolby_vision, p.seconds), (1920, 800, Transfer::Sdr, None, Some(5400.0)));
    }

    #[test]
    fn mp4_audio_tracks() {
        let mut moov = trak(b"vide", "und", "Video");
        moov.extend(trak(b"soun", "eng", "Stereo"));
        moov.extend(trak(b"soun", "ita", ""));
        let mut file = bx(b"ftyp", b"isomiso2");
        file.extend(bx(b"moov", &moov));
        file.extend(bx(b"mdat", &[0; 32]));
        assert_eq!(audio_tracks(&file).unwrap(), [AudioTrack { lang: "eng".into(), name: "Stereo".into(), commentary: false }, AudioTrack { lang: "ita".into(), name: "".into(), commentary: false }]);
        // The index at the end (not fast-start): unknown.
        let mut late = bx(b"ftyp", b"isomiso2");
        late.extend(bx(b"mdat", &[0; 32]));
        assert_eq!(audio_tracks(&late), None);
        // A 64-bit sized index: read when whole, unknown when the head cuts it off.
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend(b"moov");
        large.extend(((moov.len() + 16) as u64).to_be_bytes());
        large.extend(&moov);
        let mut wide = bx(b"ftyp", b"isomiso2");
        wide.extend(&large);
        assert_eq!(audio_tracks(&wide).map(|t| t.len()), Some(2));
        let cut = &wide[..wide.len() - 10];
        assert_eq!(audio_tracks(cut), None);
    }

    /// Against the real bundled FFmpeg test clips: MP4 container parsing reads real dimensions
    /// and audio tracks cleanly without panicking or misreporting absent `colr` boxes.
    #[test]
    fn real_bundled_mp4_clips_parse_picture_and_audio() {
        let sdr_clip = include_bytes!("../assets/selftest.mp4");
        let hdr_clip = include_bytes!("../assets/selftest-hdr.mp4");
        let p_sdr = picture(sdr_clip).expect("SDR clip header should parse");
        assert_eq!((p_sdr.width, p_sdr.height), (320, 240));
        assert_eq!(p_sdr.transfer, Transfer::Unknown);
        let a_sdr = audio_tracks(sdr_clip).expect("SDR clip audio tracks should parse");
        assert_eq!(a_sdr.len(), 1);

        let p_hdr = picture(hdr_clip).expect("HDR clip header should parse");
        assert_eq!((p_hdr.width, p_hdr.height), (320, 240));
        // selftest-hdr.mp4 is a video-only clip (no audio track).
        assert_eq!(audio_tracks(hdr_clip).unwrap().len(), 0);
        assert_eq!(video_codec(sdr_clip), Some("avc"));
    }

    #[test]
    fn mp4_tail_moov_and_extra_audio_tags() {
        let mut moov = trak(b"vide", "und", "Video");
        moov.extend(trak(b"soun", "ita", ""));
        moov.extend(trak(b"soun", "eng", "English (AD)"));
        let tail_moov = bx(b"moov", &moov);
        let head = [bx(b"ftyp", b"isomiso2"), bx(b"mdat", &[0; 64])].concat();
        assert!(mp4_moov_at_end(&head));
        let mut tail = vec![0xAAu8; 32];
        tail.extend_from_slice(&tail_moov);
        let (tracks, _pic, _codec) = parse_mp4_tail(&tail);
        let tracks = tracks.expect("tail moov parsed");
        assert_eq!(tracks.len(), 2);
        assert!(tracks[1].extra(), "(AD) audio description must be marked extra");
        assert_eq!(in_language(&tracks, "english"), Some(false));
        assert_eq!(in_language(&tracks, "italian"), Some(true));

        // Numeric BCP-47 region subtag (`es-419`) must be preserved by MP4 parser.
        let mut moov_lat = trak(b"vide", "und", "Video");
        let mut elng = Vec::new();
        elng.extend_from_slice(&[0; 4]);
        elng.extend_from_slice(b"es-419\0");
        let mut mdia = Vec::new();
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"soun");
        hdlr.extend_from_slice(&[0u8; 12]);
        mdia.extend(bx(b"hdlr", &hdlr));
        mdia.extend(bx(b"elng", &elng));
        let s_lat = bx(b"trak", &bx(b"mdia", &mdia));
        moov_lat.extend(s_lat);
        let mp4_lat = [bx(b"ftyp", b"isom"), bx(b"moov", &moov_lat)].concat();
        let lat_tracks = audio_tracks(&mp4_lat).expect("latino mp4 parsed");
        assert_eq!(lat_tracks[0].lang, "es-419");
    }

    #[test]
    fn matroska_commentary_flag_and_bcp47_und_override() {
        // Track 1: main Japanese audio with MaxBlockAdditionID=1 (0x55EE must NOT trigger commentary).
        let mut t1 = el(&[0x83], &[2]);
        t1.extend(el(&[0x22, 0xB5, 0x9C], b"jpn"));
        t1.extend(el(&[0x55, 0xEE], &[1]));
        // Track 2: English audio flagged as commentary via FlagCommentary (0x55AF = 1), no "commentary" in Name.
        let mut t2 = el(&[0x83], &[2]);
        t2.extend(el(&[0x22, 0xB5, 0x9C], b"eng"));
        t2.extend(el(&[0x55, 0xAF], &[1]));
        // Track 3: English audio-description track via FlagVisualImpaired (0x55AC = 1).
        let mut t3 = el(&[0x83], &[2]);
        t3.extend(el(&[0x22, 0xB5, 0x9C], b"eng"));
        t3.extend(el(&[0x55, 0xAC], &[1]));
        // Track 4: legacy Language=eng overridden by LanguageBCP47=und (unknown language).
        let mut t4 = el(&[0x83], &[2]);
        t4.extend(el(&[0x22, 0xB5, 0x9C], b"eng"));
        t4.extend(el(&[0x22, 0xB5, 0x9D], b"und"));
        // Track 5: absent language (defaults to "eng" per RFC 9559), flagged FlagCommentary=1.
        let mut t5 = el(&[0x83], &[2]);
        t5.extend(el(&[0x55, 0xAF], &[1]));

        let mut tracks = Vec::new();
        for t in [t1, t2, t3, t4, t5] {
            tracks.extend(el(&[0xAE], &t));
        }
        let mut segment = el(&[0x16, 0x54, 0xAE, 0x6B], &tracks);
        segment.extend(el(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]));
        let mut file = el(&[0x1A, 0x45, 0xDF, 0xA3], b"header");
        file.extend(el(&[0x18, 0x53, 0x80, 0x67], &segment));

        let parsed = audio_tracks(&file).expect("mkv parsed");
        assert_eq!(parsed.len(), 5);
        assert!(!parsed[0].commentary && !parsed[0].extra(), "MaxBlockAdditionID (0x55EE) must not mark commentary");
        assert!(parsed[1].commentary && parsed[1].extra(), "FlagCommentary (0x55AF) must mark commentary");
        assert!(parsed[2].commentary && parsed[2].extra(), "FlagVisualImpaired (0x55AC) must mark commentary/AD");
        assert_eq!(parsed[3].lang, "");
        assert!(parsed[4].commentary && parsed[4].extra());
        // Commentary/AD-only English (plus default-eng commentary) => false; adding untagged => None.
        assert_eq!(in_language(&parsed[..3], "english"), Some(false));
        assert_eq!(in_language(&[parsed[0].clone(), parsed[4].clone()], "english"), Some(false));
        assert_eq!(in_language(&parsed, "english"), None);
    }

    #[test]
    fn mp4_udta_name_and_tagc_mark_commentary_and_extra_names() {
        let mut mdia = Vec::new();
        let l = b"eng";
        let packed = l.iter().fold(0u16, |a, c| (a << 5) | (*c - 0x60) as u16);
        let mut mdhd = vec![0u8; 20];
        mdhd.extend(packed.to_be_bytes());
        mdhd.extend([0, 0]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"soun");
        hdlr.extend([0u8; 12]);
        hdlr.extend_from_slice(b"SoundHandler\0");
        mdia.extend(bx(b"mdhd", &mdhd));
        mdia.extend(bx(b"hdlr", &hdlr));

        let mut t_name_payload = bx(b"mdia", &mdia);
        t_name_payload.extend(bx(b"udta", &bx(b"name", b"Director Commentary\0")));

        let mut tagc = vec![0u8; 4];
        tagc.extend_from_slice(b"public.accessibility.describes-video");
        let mut t_tagc_payload = bx(b"mdia", &mdia);
        t_tagc_payload.extend(bx(b"udta", &bx(b"tagc", &tagc)));

        let mut moov = trak(b"soun", "jpn", "Main");
        moov.extend(bx(b"trak", &t_name_payload));
        moov.extend(bx(b"trak", &t_tagc_payload));
        let mp4_file = [bx(b"ftyp", b"isom"), bx(b"moov", &moov)].concat();
        let parsed = audio_tracks(&mp4_file).expect("mp4 parsed");
        assert_eq!(parsed.len(), 3);
        assert!(!parsed[0].extra());
        assert_eq!(parsed[1].name, "Director Commentary");
        assert!(parsed[1].commentary && parsed[1].extra());
        assert!(parsed[2].commentary && parsed[2].extra());
        assert_eq!(in_language(&parsed, "english"), Some(false));

        for extra_name in [
            "Commento del regista", "Comentarios", "Commentaire audio",
            "Audiokommentar", "Hörfilm", "Comentário", "Audio Descrizione",
            "English AD", "English (AD)", "AD 2.0", "Cast & Crew", "Isolated Score", "English DVS",
        ] {
            assert!(is_extra_audio_name(extra_name), "expected extra: {extra_name}");
        }
        for main_name in ["Director's Cut", "Theatrical", "Extended", "Original", "English 5.1", "Ad Astra"] {
            assert!(!is_extra_audio_name(main_name), "unexpected extra: {main_name}");
        }
    }

    #[test]
    fn mp4_tail_skips_false_moov_signature_in_payload() {
        let mut real_moov = trak(b"vide", "und", "Video");
        real_moov.extend(trak(b"soun", "ita", "Main"));
        let real_box = bx(b"moov", &real_moov);

        // Decoy payload containing a size-plausible `moov` header with random junk inside.
        let decoy = bx(b"moov", &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        let mut tail = vec![0xAAu8; 16];
        tail.extend_from_slice(&decoy);
        tail.extend_from_slice(&[0xBBu8; 12]);
        tail.extend_from_slice(&real_box);

        let (tracks, _pic, _codec) = parse_mp4_tail(&tail);
        let tracks = tracks.expect("real tail moov found past decoy");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].lang, "ita");
    }
}
