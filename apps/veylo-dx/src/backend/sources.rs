//! Sources: what the add-ons offer for a video, the Play order (quality, HDR, size, your audio
//! language), the probe that checks the best candidates, and the lists fetched ahead of Play.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceKind { Direct, Torrent, External }

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub addon: String,
    pub name: String,
    pub description: String,
    pub url: String,
    pub filename: String,
    /// Declared size in bytes, or the add-on's own size text; None when absent (never estimated).
    pub size: Option<String>,
    pub size_bytes: Option<u64>,
    pub declared_size: bool,
    /// The size written in the add-on's label (💾), apart from the declared one.
    pub labelled_bytes: Option<u64>,
    /// The torrent is a season pack (its name is a season, not an episode), even when the
    /// file name is the one episode that plays.
    pub pack: bool,
    pub quality: Option<&'static str>,
    pub hdr: Option<&'static str>,
    pub kind: SourceKind,
    pub headers: Vec<(String, String)>,
    /// name, title, description and filename together: what the play order reads.
    pub label: String,
    /// No line of the release names the title being played (an add-on's loose match: "Call Me by
    /// Your Name" for "Your Name"): such sources go after every one that does.
    pub off_title: bool,
    /// The picture the file's own header declares, once Play's check has read it: its size and
    /// HDR then outrank what the label claims.
    pub picture: Option<crate::tracks::Picture>,
    /// Number of main audio tracks read from the container header (used to discount multi-dub bloat).
    pub audio_tracks_count: Option<u8>,
    /// Video codec read from the container header ("av1", "hevc", "avc"), when available.
    pub video_codec: Option<&'static str>,
}

/// Full metadata and playback context passed to Autoplay so it can match localized titles,
/// pick the right language (including per-title preference & original-language fallback), reject
/// hidden trailers/extras by runtime, and fit the stream bitrate to the current connection.
#[derive(Clone, Debug, Default)]
pub struct AutoplayContext {
    /// Display title plus original/localized alternate titles (free metadata prioritized).
    pub titles: Vec<String>,
    /// Release year for movies (None for series).
    pub year: Option<i32>,
    /// Episodes in the season for season-pack size splitting.
    pub pack_episodes: Option<usize>,
    /// Expected duration in seconds (free metadata / Cinemeta / TVmaze / TMDB) for pre-probe
    /// bitrate estimation and hidden trailer/clip rejection.
    pub expected_seconds: Option<f64>,
    /// Preferred audio language key (per-title remembered track pref or global `stream_language`).
    pub preferred_language: Option<String>,
    /// Film/show's original language key, used as fallback when preferred language isn't present.
    pub original_language: Option<String>,
    /// Requested `(season, episode)` for series streams.
    pub episode_code: Option<(i64, i64)>,
    /// When true, autoplay never silently plays an off-title source (only exact title matches).
    pub strict_title: bool,
}

impl AutoplayContext {
    pub fn simple(title: &str, year: Option<i32>, pack_episodes: Option<usize>) -> Self {
        Self {
            titles: if title.trim().is_empty() { Vec::new() } else { vec![title.to_owned()] },
            year,
            pack_episodes,
            preferred_language: stream_language(),
            strict_title: !title.trim().is_empty(),
            ..Self::default()
        }
    }
}

static_re!(qual_4k, r"(?i)(?:\b|_)(?:2160p|4k|uhd)(?:\b|_)");
static_re!(qual_1440, r"(?i)(?:\b|_)1440p(?:\b|_)");
static_re!(qual_1080, r"(?i)(?:\b|_)(?:1080p|fhd)(?:\b|_)");
static_re!(qual_720, r"(?i)(?:\b|_)720p(?:\b|_)");
static_re!(qual_sd, r"(?i)(?:\b|_)(?:576p|480p|360p|240p)(?:\b|_)");

pub(super) fn find_quality(text: &str) -> Option<&'static str> {
    if qual_4k().is_match(text) { Some("4K") }
    else if qual_1440().is_match(text) { Some("1440p") }
    else if qual_1080().is_match(text) { Some("1080p") }
    else if qual_720().is_match(text) { Some("720p") }
    else if qual_sd().is_match(text) { Some("SD") }
    else { None }
}

pub(super) fn find_hdr(text: &str) -> Option<&'static str> {
    match hdr_tier(text) { 3 => Some("HDR10+"), 2 => Some("HDR"), 1 => Some("DV"), _ => None }
}

/// "4.2 GiB": a size in bytes as the player shows it.
pub fn file_size_label(bytes: u64) -> String { binary_size(bytes as f64) }

pub(super) fn binary_size(bytes: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes; let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 { v /= 1024.0; i += 1; }
    if i >= 3 { format!("{v:.1} {}", units[i]) } else { format!("{v:.0} {}", units[i]) }
}

static_re!(hdr10_plus, r"(?i)\bHDR10(?:\+|[ ._-]?PLUS\b|P\b)");
static_re!(hdr_any, r"(?i)\b(?:HDR10|HDR|HLG)\b");
static_re!(dolby_vision, r"(?i)\b(?:DV|DOVI|DOLBY[ ._-]?VISION)\b");
static_re!(remux, r"(?i)REMUX\b");
static_re!(bluray, r"(?i)\b(?:BLU[ ._-]?RAY|BDRIP|BRRIP|BDREMUX|BD(?:25|50|66|100))\b");
static_re!(web_dl, r"(?i)\bWEB(?:[ ._-]?DL)?\b");
static_re!(web_rip, r"(?i)\bWEB[ ._-]?RIP\b");
static_re!(low_source, r"(?i)\b(?:HDTV|PDTV|DVDRIP|DVDSCR|HDCAM|CAM|TS|HDTS|TELESYNC|TC|SCREENER)\b");
// Filmed or copied in a cinema, pre-release screener, or hardcoded foreign-sub leak: never Play's
// choice, whatever resolution the name claims ("1080p TS" / "1080p HDTS" is a camera copy).
// Container extensions (`.ts`, `.mkv`, `.mp4`, ...) are stripped before testing so MPEG-TS
// `.ts` filenames are never confused with Telesync (`TS`) release tags.
static_re!(video_ext_suffix, r"(?i)\.(?:mkv|mp4|ts|m2ts|mts|avi|mov|webm|wmv|flv|mpg|mpeg)(?:$|[\s\]\)])");
static_re!(cinema_copy, r"(?i)\b(?:HD[ ._-]?CAM|CAM[ ._-]?RIP|CAM|HD[ ._-]?TS|TS|TELESYNC|HD[ ._-]?TC|TELECINE|DVD[ ._-]?SCR|SCREENER|WORKPRINT|R5|KORSUB|HARD[ ._-]?SUB(?:S|BED)?|HC[ ._-]?(?:HDRIP|WEBRIP|WEB|1080P|720P|2160P|X264|X265))\b");
static_re!(extra_clip, r"(?i)\b(?:TRAILER|TEASER|FEATURETTE|DELETED[ ._-]?SCENES?|BEHIND[ ._-]?THE[ ._-]?SCENES|MAKING[ ._-]?OF|BONUS[ ._-]?FEATURES?|NCOP|NCED)\b");
static_re!(season_ep_tag, r"(?i)\bS(\d{1,2})[ ._-]?E(\d{1,3})(?:(?:[ ._-]?[-–][ ._-]?E?|[ ._-]?E)(\d{1,3}))?\b");
static_re!(anime_ep_tag, r"(?i)(?:[ _-]-[ _-]|_)(\d{2,3})(?:v\d)?(?:[ _\-\[\(]|$)");
static_re!(av1, r"(?i)\bAV1\b");
// Resolution or HDR made up after the fact: AI upscales and SDR regraded to HDR.
static_re!(upscaled, r"(?i)(?:\b|_)(?:AI[ ._-]?)?UPSCALE(?:D|S)?(?:\b|_)|\bUPSCL\b|\bAI[ ._-]?(?:ENHANCED|REMASTER(?:ED)?)\b|\bREGRADED?\b|\bSDR[ ._-]?TO[ ._-]?HDR\b|\bFAKE[ ._-]?HDR\b");
static_re!(hevc, r"(?i)\b(?:HEVC|[HX][ ._-]?265)\b");
static_re!(avc, r"(?i)\b(?:AVC|[HX][ ._-]?264)\b");
static_re!(episode_mark, r"(?i)\bS\d{1,2}[ ._-]?E\d{1,3}\b|\b\d{1,2}x\d{2,3}\b|\bEP?[ ._-]?\d{2,3}\b");
static_re!(season_pack, r"(?i)\bS\d{1,2}\b|\bSEASONS?[ ._-]?\d|\bCOMPLETE\b");
static_re!(html_tag, "<[^>]+>");
static_re!(multi_audio, r"(?i)\b(?:MULTI|DUAL)(?:[ ._-]?AUDIO)?\b|\bMULTi\b");
// Release tags used by Torrentio, AIOStreams and their upstream scrapers.
static_re!(stereo_video, r"(?i)\b(?:3[ .-]?D|(?:H(?:ALF)?|F(?:ULL)?)[ .-]?(?:SBS|OU|TAB)|SBS|TAB|SIDE[ .-]BY[ .-]SIDE|TOP[ .-](?:AND[ .-])?BOTTOM|OVER[ .-]UNDER|STEREOSCOPIC)\b");

pub(super) fn is_3d(label: &str) -> bool { stereo_video().is_match(&label.replace('_', ".")) }

static_re!(size_label, r"(?i)(?:💾\s*)?(\d[\d.,]*)\s*([KMGT]i?B)\b");

/// HDR10+ (3), HDR10/HDR/HLG (2), Dolby Vision alone (1), SDR (0); "DV HDR10" counts as HDR10.
pub(super) fn hdr_tier(text: &str) -> u8 {
    if hdr10_plus().is_match(text) { 3 } else if hdr_any().is_match(text) { 2 } else if dolby_vision().is_match(text) { 1 } else { 0 }
}
/// Remux (4), WEB-DL (3), WEBRip (2), unlabelled (1), HDTV/DVD/cam (0); a Blu-ray re-encode is None.
pub(super) fn source_tier(text: &str) -> Option<u8> {
    if remux().is_match(text) { Some(4) } else if bluray().is_match(text) { None } else if web_rip().is_match(text) { Some(2) }
    else if web_dl().is_match(text) { Some(3) } else if low_source().is_match(text) { Some(0) } else { Some(1) }
}
pub(super) fn codec_tier(text: &str) -> u8 { if av1().is_match(text) { 3 } else if hevc().is_match(text) { 2 } else if avc().is_match(text) { 1 } else { 0 } }

pub(super) fn effective_codec_tier(s: &Source) -> u8 {
    match s.video_codec {
        Some("av1") => 3,
        Some("hevc") => 2,
        Some("avc") => 1,
        _ => codec_tier(&s.label),
    }
}

pub(super) fn is_cinema_copy(label: &str) -> bool {
    let stripped = video_ext_suffix().replace_all(label, "");
    cinema_copy().is_match(&stripped)
}

/// True when `s` is a non-feature extra (trailer, featurette, making-of, anime NCOP/NCED)
/// and the requested title itself doesn't contain that word.
pub(super) fn is_extra_clip(s: &Source, titles: &[String]) -> bool {
    extra_clip().is_match(&s.label) && !titles.iter().any(|t| extra_clip().is_match(t))
}

/// True when a stream explicitly names a different episode than `(want_s, want_e)`.
/// For season packs, still validates `s.filename` when present so a mis-mapped episode file is caught.
pub(super) fn wrong_episode(s: &Source, want: (i64, i64)) -> bool {
    if s.pack && s.filename.is_empty() { return false; }
    let hay = if !s.filename.is_empty() { s.filename.as_str() } else { s.description.lines().next().unwrap_or("") };
    let mut found_any = false;
    for cap in season_ep_tag().captures_iter(hay) {
        let Ok(season) = cap[1].parse::<i64>() else { continue };
        let Ok(ep_start) = cap[2].parse::<i64>() else { continue };
        let ep_end = cap.get(3).and_then(|m| m.as_str().parse::<i64>().ok()).filter(|&e| e >= ep_start).unwrap_or(ep_start);
        found_any = true;
        if season == want.0 && (ep_start..=ep_end).contains(&want.1) { return false; }
    }
    if found_any { return true; }
    // Anime bare episode numbers like `[Group] Show - 06 (1080p).mkv` or `Show_06_[2160p].mp4`.
    if want.0 == 1 {
        let stripped = video_ext_suffix().replace_all(hay, "");
        let mut found_ep = false;
        for cap in anime_ep_tag().captures_iter(&stripped) {
            let Ok(ep) = cap[1].parse::<i64>() else { continue };
            if !(1..=500).contains(&ep) || matches!(ep, 264 | 265 | 480) { continue; }
            found_ep = true;
            if ep == want.1 { return false; }
        }
        if found_ep { return true; }
    }
    false
}

/// The size in the add-on's label ("💾 45.3 GB" preferred): text as written, and bytes.
pub(super) fn labelled_size(text: &str) -> Option<(String, u64)> {
    let all: Vec<_> = size_label().captures_iter(text).filter(|c| c[1].chars().any(|d| ('1'..='9').contains(&d))).collect();
    let c = all.iter().find(|c| c[0].starts_with('💾')).or(all.first())?;
    let raw = &c[1];
    let number: f64 = (if raw.contains('.') { raw.replace(',', "") } else { raw.replace(',', ".") }).parse().ok()?;
    let unit = c[2].to_uppercase();
    let power = "BKMGTPE".find(unit.chars().next()?).unwrap_or(0) as i32;
    let base: f64 = if unit.contains('I') { 1024.0 } else { 1000.0 };
    Some((format!("{} {}", raw, &c[2]), (number * base.powi(power)) as u64))
}

pub(super) fn source_of(addon: &str, v: &Value) -> Source {
    let hints = &v["behaviorHints"];
    let (name, description) = (s(v, "name"), [s(v, "description"), s(v, "title")].into_iter().find(|t| !t.is_empty()).unwrap_or_default());
    let filename = s(hints, "filename");
    let url = s(v, "url");
    let kind = if !s(v, "infoHash").is_empty() { SourceKind::Torrent }
        else if url.starts_with("https://") || url.starts_with("http://") { SourceKind::Direct } else { SourceKind::External };
    let label = format!("{name} {} {} {filename}", s(v, "title"), s(v, "description"));
    let headers = hints["proxyHeaders"]["request"].as_object()
        .map(|h| h.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))).collect()).unwrap_or_default();
    let declared = hints["videoSize"].as_u64().or_else(|| hints["videoSize"].as_str().and_then(|x| x.parse().ok())).filter(|b| *b > 0);
    let labelled = labelled_size(&format!("{}\n{}\n{}", s(v, "title"), s(v, "description"), s(v, "name")));
    // Add-ons write the torrent's name on its own line (the file name, if any, on another).
    let pack = format!("{name}\n{}\n{}", s(v, "title"), s(v, "description")).lines()
        .any(|line| season_pack().is_match(line) && !episode_mark().is_match(line))
        || (!episode_mark().is_match(&label) && season_pack().is_match(&label));
    Source {
        addon: addon.to_owned(), name, description, filename,
        size: declared.map(|b| binary_size(b as f64)).or_else(|| labelled.as_ref().map(|l| l.0.clone())),
        size_bytes: declared.or(labelled.as_ref().map(|l| l.1)),
        declared_size: declared.is_some(),
        labelled_bytes: labelled.as_ref().map(|l| l.1),
        pack,
        quality: find_quality(&label), hdr: find_hdr(&label), kind, headers,
        url: if kind == SourceKind::Direct { url } else { String::new() },
        label,
        off_title: false,
        picture: None,
        audio_tracks_count: None,
        video_codec: None,
    }
}

pub(super) fn quality_rank(q: Option<&str>) -> u8 { match q { Some("4K") => 0, Some("1440p") => 1, Some("1080p") => 2, Some("720p") => 3, Some("SD") => 4, _ => 5 } }

/// Every installed stream add-on's sources for one video, best declared quality first; add-on order kept within a quality.
pub async fn sources(kind: &str, video_id: &str) -> Result<(Vec<Source>, Vec<String>)> {
    sources_within(kind, video_id, None, None).await.map(|(list, failed, _)| (list, failed))
}

/// How long Play waits for the slower add-ons once one has answered with sources it can play.
pub(super) const ADDON_GRACE: Duration = Duration::from_secs(6);

/// `sources`, but with `grace` the answer comes that long after the first add-on with a playable
/// source, without waiting for an add-on that is slow or down (the core waits up to 30 s each).
/// `early` gets each add-on's sources as they come, while others are still answering.
/// The flag says whether every add-on asked answered (false when `grace` left slower ones out).
pub(super) async fn sources_within(kind: &str, video_id: &str, grace: Option<Duration>, early: Option<tokio::sync::mpsc::UnboundedSender<Vec<Source>>>) -> Result<(Vec<Source>, Vec<String>, bool)> {
    let targets = exec(json!({"op":"streams_targets","type":kind,"id":video_id})).await?;
    let targets: Vec<Value> = targets["targets"].as_array().cloned().unwrap_or_default();
    let (mut out, failed, complete) = ask_addons(kind, video_id, targets, grace, early).await?;
    out.sort_by_key(|x| (x.kind != SourceKind::Direct, quality_rank(x.quality)));
    Ok((out, failed, complete))
}

/// One round of `sources_within`: these add-ons' sources (2D only), the ones that failed, and
/// whether all of them answered.
pub(super) async fn ask_addons(kind: &str, video_id: &str, targets: Vec<Value>, grace: Option<Duration>, early: Option<tokio::sync::mpsc::UnboundedSender<Vec<Source>>>) -> Result<(Vec<Source>, Vec<String>, bool)> {
    let (kind, id) = (kind.to_owned(), video_id.to_owned());
    let (groups, complete) = on_runtime(async move {
        let mut jobs = tokio::task::JoinSet::new();
        for (i, t) in targets.into_iter().enumerate() {
            let request = json!({"op":"streams","type":kind,"id":id,"addon_url":t["addon_url"]});
            jobs.spawn(async move { (i, s(&t, "addon_name"), backend().core.execute(request).await) });
        }
        let mut groups: Vec<(usize, Value)> = Vec::new();
        let mut deadline: Option<tokio::time::Instant> = None;
        let mut complete = true;
        loop {
            let next = match deadline {
                Some(at) => match tokio::time::timeout_at(at, jobs.join_next()).await { Ok(n) => n, Err(_) => { complete = jobs.is_empty(); break } },
                None => jobs.join_next().await,
            };
            let Some(done) = next else { break };
            let Ok((i, name, answer)) = done else { continue };
            match answer {
                Ok(v) => for g in v["groups"].as_array().into_iter().flatten() {
                    let playable = g["streams"].as_array().into_iter().flatten().any(|st| s(st, "url").starts_with("http"));
                    if let (Some(grace), None, true) = (grace, deadline, playable) { deadline = Some(tokio::time::Instant::now() + grace); }
                    if let Some(tx) = early.as_ref().filter(|_| playable && !jobs.is_empty()) {
                        let addon = s(g, "addon_name");
                        let _ = tx.send(g["streams"].as_array().into_iter().flatten().map(|st| source_of(&addon, st)).filter(|s| !is_3d(&s.label)).collect());
                    }
                    groups.push((i, g.clone()));
                },
                Err(_) => groups.push((i, json!({"addon_name": name, "streams": [], "error": "unavailable"}))),
            }
        }
        // Add-ons still answering are left out (dropping the set cancels them).
        groups.sort_by_key(|(i, _)| *i);
        (groups.into_iter().map(|(_, g)| g).collect::<Vec<Value>>(), complete)
    }).await?;
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for g in &groups {
        let addon = s(g, "addon_name");
        if g.get("error").is_some() { failed.push(addon.clone()); }
        out.extend(g["streams"].as_array().into_iter().flatten().map(|st| source_of(&addon, st)).filter(|s| !is_3d(&s.label)));
    }
    Ok((out, failed, complete))
}


/// Size of the one file that plays: a season pack's size is split per episode. Some add-ons
/// (AIOStreams) declare the whole torrent's size as the file's; a declared size that matches
/// the labelled pack size (or has no label to compare with) is split too, a different one is
/// the file's own.
pub(super) fn episode_size(src: &Source, pack_episodes: Option<usize>) -> Option<u64> {
    let size = src.size_bytes?;
    let Some(n) = pack_episodes.filter(|n| src.pack && *n > 1) else { return Some(size) };
    if src.declared_size {
        if let Some(label) = src.labelled_bytes.filter(|l| *l > 0) {
            if (size as f64 - label as f64).abs() / label as f64 > 0.1 { return Some(size); }
        }
    }
    Some(size / n as u64)
}

/// Play order: 2D direct sources of 1080p or more only (unlabelled ones after every labelled one);
/// HDR before SDR, then 4K > 1440p > 1080p (a file too small for its resolution counts as a
/// 1080p, see `picture_class`), then biggest file (per episode for season packs),
/// remux > WEB-DL > WEBRip > other, HDR10+ > HDR > SDR, then AV1 > H.265 > H.264.
/// Dolby-Vision-only files (usually profile 5, wrong colours without a DV decoder) come last;
/// AI upscales and SDR regraded to HDR after every native file.
/// Torrents, external links and anything under 1080p never.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn play_order(sources: &[Source], pack_episodes: Option<usize>) -> Vec<Source> {
    let language = stream_language();
    play_order_for(sources, pack_episodes, language.as_deref())
}

pub(super) static STREAM_LANGUAGE: Mutex<Option<String>> = Mutex::new(None);
pub(super) fn set_stream_language(language: Option<String>) {
    if let Ok(mut l) = STREAM_LANGUAGE.lock() { *l = language; }
}

/// The preferred audio language (a `crate::addons::LANGUAGES` key), None for any.
pub fn stream_language() -> Option<String> { STREAM_LANGUAGE.lock().ok().and_then(|l| l.clone()) }

/// The language codes a file's tracks use for an audio-language setting, in mpv's `alang`
/// order (ISO 639-1, then -2/B and -2/T).
pub fn track_codes(key: &str) -> &'static [&'static str] {
    crate::addons::language(key).map_or(&[], |l| l.codes)
}

/// Whether a track's language tag is the setting's language ("ita", "it", "it-IT" for Italian).
/// Also for subtitle add-on codes: "pob" is Portuguese, "spl" Latin-American Spanish.
pub fn track_in_language(code: &str, key: &str) -> bool {
    let code = code.trim().to_lowercase().replace('_', "-");
    if code.is_empty() { return false; }
    let base = code.split('-').next().unwrap_or("");
    let named = crate::engine::language_name(&code);
    track_codes(key).iter().any(|c| *c == code || (!c.contains('-') && *c == base && key != "latino") || (key == "latino" && code.starts_with("es-") && code != "es-es"))
        || named == subtitle_language_of(key)
        || (key == "portuguese" && named == "Portuguese (Brazil)")
}

/// `play_order` with a preferred audio language, in three tiers, each in the usual order:
/// sources tagged with it (flag, name or release tag, multi-audio included); then untagged or
/// multi-language ones; then those tagged only with other languages (an Italian-only release
/// when you chose English). English is mostly untagged, so for English the first two are one.
/// Within a tier: Real-Debrid sources first, then the free web add-ons' (NoTorrent, Absolute
/// Cinema); a free web link marked as a trial goes last, as it may stop partway.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn play_order_for(sources: &[Source], pack_episodes: Option<usize>, language: Option<&str>) -> Vec<Source> {
    let lang = language.and_then(crate::addons::language);
    let mut out: Vec<Source> = sources.iter().filter(|s| playable(s)).cloned().collect();
    out.sort_by_cached_key(|s| order_key(s, episode_size(s, pack_episodes).unwrap_or(0), lang));
    out
}

static_re!(sample_clip, r"(?i)(?:\b|_)SAMPLE(?:\b|_)");

/// Extensions that are never directly playable video streams (archives, disc images, text/subtitles).
fn non_video_filename(name: &str) -> bool {
    let ext = name.rsplit('.').next().filter(|_| name.contains('.')).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "iso" | "rar" | "zip" | "7z" | "tar" | "gz" | "nfo" | "txt" | "srt" | "sub" | "idx" | "exe" | "jpg" | "jpeg" | "png")
}

/// What Play may choose: a direct 2D video link, not labelled under 1080p (nor an unlabelled DVD rip
/// or TV capture), never a cinema copy / telesync / screener, and never an archive, disc image, or tiny sample clip.
pub(super) fn playable(s: &Source) -> bool {
    let low = match s.quality { Some(q) => quality_rank(Some(q)) > 2, None => low_source().is_match(&s.label) };
    let bad_file = !s.filename.is_empty() && non_video_filename(&s.filename);
    let tiny_sample = sample_clip().is_match(&s.label) && s.size_bytes.is_some_and(|b| b < 200_000_000);
    s.kind == SourceKind::Direct && !is_3d(&s.label) && !low && !is_cinema_copy(&s.label) && !bad_file && !tiny_sample
}

pub(super) type OrderKey = (bool, u8, u8, bool, bool, bool, std::cmp::Reverse<(bool, u8, u64, i16, u8, u8)>);

/// Connection bitrate ceiling for autoplay: None means unlimited.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BandwidthBudget {
    pub max_encode_mbps: Option<f64>,
    pub max_remux_mbps: Option<f64>,
}

impl BandwidthBudget {
    pub(super) fn unlimited() -> Self { Self { max_encode_mbps: None, max_remux_mbps: None } }

    pub(super) fn from_settings(cap: Option<u32>, measured_mbps: Option<f64>) -> Self {
        match cap {
            Some(0) => Self::unlimited(),
            Some(c) => {
                let c = f64::from(c);
                Self { max_encode_mbps: Some(c), max_remux_mbps: Some(c) }
            }
            None => match measured_mbps {
                Some(m) if m > 0.0 => Self {
                    // Leave 15% network headroom and account for VBR scene peaks (1.45x encodes, 1.65x remux).
                    max_encode_mbps: Some((m * 0.85 / 1.45).max(4.0)),
                    max_remux_mbps: Some((m * 0.85 / 1.65).max(4.0)),
                },
                _ => Self::unlimited(),
            },
        }
    }

    pub(super) fn current() -> Self {
        Self::from_settings(max_bitrate_mbps(), connection_mbps())
    }
}

/// Estimated or verified average bitrate (Mbps) for a source.
pub(super) fn estimated_avg_mbps(s: &Source, size: u64, expected_seconds: Option<f64>) -> Option<f64> {
    if size == 0 { return None; }
    let episode = s.pack || episode_mark().is_match(&s.label);
    let secs = s.picture.and_then(|p| p.seconds)
        .or(expected_seconds)
        .unwrap_or(if episode { 2700.0 } else { 6600.0 });
    (secs > 0.0).then(|| size as f64 * 8.0 / secs / 1e6)
}

/// True when the source's average bitrate exceeds the current connection/manual budget.
pub(super) fn exceeds_budget(s: &Source, size: u64, expected_seconds: Option<f64>, budget: BandwidthBudget) -> bool {
    let cap = if source_tier(&s.label) == Some(4) { budget.max_remux_mbps } else { budget.max_encode_mbps };
    let Some(cap) = cap else { return false };
    estimated_avg_mbps(s, size, expected_seconds).is_some_and(|mbps| mbps > cap)
}

/// Codec- and multi-dub-adjusted size used to compare visual quality fairly within a resolution tier.
/// Remuxes keep a lossless-source weight; bloated multi-dub encodes discount extra audio tracks;
/// HEVC and AV1 encodes get a modest efficiency boost over H.264.
pub(super) fn effective_size(s: &Source, size: u64) -> u64 {
    if size == 0 { return 0; }
    if source_tier(&s.label) == Some(4) {
        return size.saturating_mul(115) / 100;
    }
    let extra_tracks = s.audio_tracks_count.unwrap_or(1).saturating_sub(1) as u64;
    let audio_bloat = (extra_tracks * 350_000_000).min(size / 2);
    let video_bytes = size.saturating_sub(audio_bloat);
    let factor = match effective_codec_tier(s) {
        3 => 115, // AV1
        2 => 110, // HEVC
        _ => 100, // AVC / unknown
    };
    video_bytes.saturating_mul(factor) / 100
}

/// The HDR scale Play ranks a file by: its header's where Play's check read one that says,
/// else its label's. HDR10+ lives in the frames, so a label's HDR10+ stands over a header's HDR.
pub(super) fn verified_hdr_tier(s: &Source) -> u8 {
    let label = hdr_tier(&s.label);
    match s.picture.and_then(|p| p.hdr_tier()) { Some(2) if label == 3 => 3, Some(t) => t, None => label }
}

/// The resolution Play ranks a file by: its label's, or 1080p's when the file is too small to
/// carry it (a 2 GB "4K" film is a heavy re-encode: among the 1080p files its size decides). An
/// episode (S01E02, or a season pack's share) needs less than a film. Unknown sizes keep their
/// label's class.
pub(super) fn picture_class(s: &Source, size: u64) -> u8 {
    let class = 5 - quality_rank(s.quality);
    // With the file's real length the bitrate decides (about what the size floors mean for a
    // two-hour film or a 45-minute episode).
    if let Some(seconds) = s.picture.and_then(|p| p.seconds).filter(|_| size > 0) {
        let mbps = size as f64 * 8.0 / seconds / 1e6;
        let floor = match s.quality { Some("4K") => 3.0, Some("1440p") => 2.0, _ => 0.0 };
        return if mbps < floor { class.min(5 - quality_rank(Some("1080p"))) } else { class };
    }
    let episode = s.pack || episode_mark().is_match(&s.label);
    let floor: u64 = match (s.quality, episode) {
        (Some("4K"), false) => 3_000_000_000, (Some("4K"), true) => 1_000_000_000,
        (Some("1440p"), false) => 2_000_000_000, (Some("1440p"), true) => 700_000_000,
        _ => 0,
    };
    if size > 0 && size < floor { class.min(5 - quality_rank(Some("1080p"))) } else { class }
}

/// Whether the current display natively decodes Dolby Vision (cached cheaply per call).
fn screen_supports_dv() -> bool {
    #[cfg(test)]
    { false }
    #[cfg(not(test))]
    { crate::display::screen().is_some_and(|s| s.dolby_vision) }
}

/// The Play-order sort key of one source whose file is `size` bytes: smaller plays first.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(super) fn order_key(s: &Source, size: u64, lang: Option<&crate::addons::Language>) -> OrderKey {
    order_key_for(s, size, lang, screen_supports_dv())
}

pub(super) fn order_key_for(s: &Source, size: u64, lang: Option<&crate::addons::Language>, dolby_vision_screen: bool) -> OrderKey {
    let tier = match lang {
        None => 0,
        Some(l) if l.matches(&s.label) => 0,
        Some(l) if other_language_only(&s.label, l.key) => 2,
        Some(l) => u8::from(l.key != "english"),
    };
    let provider = u8::from(s.label.to_uppercase().contains("FREE TRIAL"));
    let hdr = verified_hdr_tier(s);
    // Dolby Vision alone (typically profile 5) ranks below SDR on screens without native DV,
    // because without a DV decoder its colours are wrong; on a native DV panel it ranks with HDR.
    let dv_penalty = hdr == 1 && !dolby_vision_screen;
    let is_hdr = hdr >= 2 || (hdr == 1 && dolby_vision_screen);
    let hdr_rank = match hdr { 1 if dolby_vision_screen => 2, 1 => 0, 0 => 1, t => t };
    // An upscale or regrade goes after every native file: its 4K and HDR are made up, its size padding.
    (s.off_title, tier, provider, dv_penalty, upscaled().is_match(&s.label), s.quality.is_none(), std::cmp::Reverse((is_hdr,
        picture_class(s, size), effective_size(s, size), source_tier(&s.label).map_or(-1, i16::from), hdr_rank, effective_codec_tier(s))))
}

/// Unified quality tie-breaker: sources that fit the connection rank highest-quality first;
/// if every source exceeds the connection budget, lowest-bitrate goes first (bucketed in 5 Mbps
/// bands so a 1 Mbps difference never beats primary-language audio) so playback won't stall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum QualityOrder {
    Fit(std::cmp::Reverse<(bool, u8, u64)>, bool, bool, std::cmp::Reverse<(u64, i16, u8, u8)>),
    OverBudget(u64, bool, u64, std::cmp::Reverse<u8>),
}

/// One unified total order used for pre-probe optimism, wave early-stop, and final post-probe
/// selection. `off_title` and connection fit can never be overridden by size.
pub(super) type UnifiedKey = (bool, bool, u8, u8, bool, bool, bool, QualityOrder);

#[allow(clippy::too_many_arguments)]
pub(super) fn unified_key(
    s: &Source,
    size: u64,
    audio: AudioVerdict,
    has_original_lang: bool,
    lang: Option<&crate::addons::Language>,
    expected_seconds: Option<f64>,
    budget: BandwidthBudget,
    dolby_vision_screen: bool,
    remux_group_size: Option<u64>,
) -> UnifiedKey {
    let over = exceeds_budget(s, size, expected_seconds, budget);
    let lang_tier = match (lang, audio) {
        (None, _) => 0,
        (Some(_), Some((true, _))) => 0,
        (Some(l), None) => {
            if l.matches(&s.label) {
                0
            } else if other_language_only(&s.label, l.key) {
                2
            } else if l.key == "english" {
                0
            } else {
                1
            }
        }
        (Some(_), Some((false, _))) => {
            if has_original_lang { 2 } else { 3 }
        }
    };
    let provider = u8::from(s.label.to_uppercase().contains("FREE TRIAL"));
    let hdr = verified_hdr_tier(s);
    let dv_penalty = hdr == 1 && !dolby_vision_screen;
    let is_hdr = hdr >= 2 || (hdr == 1 && dolby_vision_screen);
    let hdr_rank = match hdr { 1 if dolby_vision_screen => 2, 1 => 0, 0 => 1, t => t };
    let eff = effective_size(s, size);
    let quality = if over {
        let mbps_bucket = estimated_avg_mbps(s, size, expected_seconds).map_or(u64::MAX, |m| (m / 5.0).floor() as u64);
        let not_primary_remux = remux_group_size.is_some() && !matches!(audio, Some((true, true)));
        QualityOrder::OverBudget(mbps_bucket, not_primary_remux, size, std::cmp::Reverse(effective_codec_tier(s)))
    } else {
        // Group near-identical encodes (within 50 MB) so a container-verified file (audio + picture read)
        // wins cleanly over an unverified twin of virtually identical size.
        let group_size = remux_group_size.unwrap_or_else(|| if eff >= 50_000_000 { (eff / 50_000_000) * 50_000_000 } else { eff });
        let not_primary_remux = remux_group_size.is_some() && !matches!(audio, Some((true, true)));
        let unverified = s.picture.is_none() && audio.is_none();
        QualityOrder::Fit(
            std::cmp::Reverse((is_hdr, picture_class(s, size), group_size)),
            not_primary_remux,
            unverified,
            std::cmp::Reverse((eff, source_tier(&s.label).map_or(-1, i16::from), hdr_rank, effective_codec_tier(s))),
        )
    };
    (s.off_title, over, lang_tier, provider, dv_penalty, upscaled().is_match(&s.label), s.quality.is_none(), quality)
}

/// Optimistic lower-bound key for an unprobed source (assuming its audio turns out to match your
/// language, while respecting its declared size, resolution, HDR, and connection budget).
pub(super) fn optimistic_key(
    s: &Source,
    size: u64,
    lang: Option<&crate::addons::Language>,
    expected_seconds: Option<f64>,
    budget: BandwidthBudget,
    dolby_vision_screen: bool,
) -> UnifiedKey {
    let optimistic_audio = lang.and_then(|l| {
        if other_language_only(&s.label, l.key) { None } else { Some((true, true)) }
    });
    unified_key(s, size, optimistic_audio, false, lang, expected_seconds, budget, dolby_vision_screen, None)
}

/// The source's label names the Audio-language setting's language (flag, name or release tag).
pub fn claims_language(s: &Source) -> bool {
    stream_language().and_then(|l| crate::addons::language(&l)).is_some_and(|l| l.matches(&s.label))
}

/// A label tagged with some language other than `key`, and not as a multi-language release.
pub(super) fn other_language_only(label: &str, key: &str) -> bool {
    !multi_audio().is_match(label) && crate::addons::LANGUAGES.iter().any(|l| l.key != key && l.matches(label))
}

/// The automatic choice: the source that plays, the others to try next ("Another source"),
/// and why nothing played when nothing did.
/// `audio_known`: the chosen file's own tracks said whether it has your language (the player
/// need not check again).
#[derive(Clone, Debug, Default)]
pub struct Pick { pub best: Option<Source>, pub rest: Vec<Source>, pub debrid: Option<String>, pub audio_known: bool }

/// Wave probing limits: smaller waves prevent debrid 429 bursts while mirror failover inside each
/// wave task recovers broken/uncached links without waiting for a second wave.
pub(super) const PROBE_FIRST: usize = 6;
pub(super) const PROBE_NEXT: usize = 4;
pub(super) const PROBE_MAX: usize = 16;
pub(super) const PROBE_BUDGET: Duration = Duration::from_millis(3000);
/// Hard wall-clock ceiling when every candidate is failing/rate-limited so Play never hangs.
pub(super) const PROBE_EMPTY_BUDGET: Duration = Duration::from_secs(7);
/// One probe's limit, and tier-aware grace windows once a working source is found.
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(7);
pub(super) const PROBE_GRACE_HIGH: Duration = Duration::from_millis(2000);
pub(super) const PROBE_GRACE_SAME: Duration = Duration::from_millis(700);
/// Maximum mirrors tried sequentially inside a single wave task when mirror #1 fails.
pub(super) const MAX_MIRRORS_PER_GROUP: usize = 3;
/// Cap for the one-time cold-start CDN throughput burst on the already-open header response.
pub(super) const SPEED_BURST_BYTES: usize = 1536 * 1024;
pub(super) const SPEED_BURST_MAX_TIME: Duration = Duration::from_millis(360);

/// Play's automatic choice for one video (simple signature kept for compatibility).
pub async fn autoplay(kind: &str, video_id: &str, pack_episodes: Option<usize>, title: &str, year: Option<i32>) -> Pick {
    autoplay_for(kind, video_id, AutoplayContext::simple(title, year, pack_episodes)).await
}

/// Play's automatic choice with full metadata context (localized titles, runtime, original
/// language, per-title audio preference, episode validation, and connection-aware bitrate fitting).
pub async fn autoplay_for(kind: &str, video_id: &str, ctx: AutoplayContext) -> Pick {
    let started = std::time::Instant::now();
    dev_log("autoplay: start");
    // While slower add-ons answer, gently pre-warm only the top 3 exact-title files (never a
    // full burst) so their probes are already cached when the final list arrives.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<Source>>();
    let early_ctx = ctx.clone();
    let early = async move {
        let mut warmed: HashSet<String> = HashSet::new();
        while let Some(mut batch) = rx.recv().await {
            classify_sources(&mut batch, &early_ctx);
            let order = play_order_with_ctx(&batch, &early_ctx);
            let top: Vec<Source> = order.into_iter()
                .filter(|s| !s.off_title && warmed.insert(s.url.clone()))
                .take(3)
                .collect();
            if !top.is_empty() {
                dev_log(&format!("autoplay: pre-warming {} top sources", top.len()));
                let mut set = tokio::task::JoinSet::new();
                for s in top { set.spawn(async move { let _ = probe_one(s, false).await; }); }
                while set.join_next().await.is_some() {}
            }
        }
    };
    let fetch = play_sources(kind, video_id, Some(tx));
    tokio::pin!(fetch, early);
    let mut list = tokio::select! { biased; res = &mut fetch => res, _ = &mut early => fetch.await };
    classify_sources(&mut list, &ctx);
    let order = play_order_with_ctx(&list, &ctx);
    let listed = started.elapsed().as_millis();
    // Direct links exist but none is 1080p or better (or all were cinema copies / extras).
    if order.is_empty() && list.iter().any(|s| s.kind == SourceKind::Direct && !is_3d(&s.label)) {
        dev_log(&format!("autoplay: {} sources, none 1080p or better", list.len()));
        return Pick { debrid: Some(BELOW_1080P.into()), ..Default::default() };
    }
    let (exact, off_title): (Vec<Source>, Vec<Source>) = if ctx.strict_title && !ctx.titles.is_empty() {
        order.into_iter().partition(|s| !s.off_title)
    } else {
        (order, Vec::new())
    };
    if exact.is_empty() && !off_title.is_empty() {
        dev_log(&format!("autoplay: {} sources, none matched title (refusing {} off-title)", list.len(), off_title.len()));
        return Pick { best: None, rest: off_title, debrid: Some(NO_MATCHING_TITLE.into()), audio_known: false };
    }
    let exact_len = exact.len();
    let mut pick = pick_source_with(exact, &ctx).await;
    // Keep off-title candidates out of automatic playback, never silently starting the wrong movie.
    let total_ms = started.elapsed().as_millis();
    let speed_note = connection_mbps().map(|m| format!(" @{m:.0}Mb/s")).unwrap_or_default();
    dev_log(&format!("autoplay: {} sources, {exact_len} matched in {listed} ms{speed_note}; chose {} after {total_ms} ms",
        list.len(), pick.best.as_ref().map(source_summary).unwrap_or_else(|| "nothing".into())));
    if total_ms >= 8000 {
        report_issue_bg("performance",
            format!("Slow autoplay resolution ({total_ms} ms)"),
            format!("kind={kind} sources={} playable={exact_len} list_ms={listed} total_ms={total_ms} picked={}",
                list.len(), pick.best.is_some()));
    }
    // Never append `off_title` to `pick.rest` when strict title matching is enabled:
    // otherwise runtime fallback / language retries could silently play the wrong film.
    if pick.best.is_some() && !ctx.strict_title {
        pick.rest.extend(off_title);
    }
    pick
}

/// Apply title matching, episode matching, and extra-clip filtering in place.
pub(super) fn classify_sources(list: &mut Vec<Source>, ctx: &AutoplayContext) {
    if !ctx.titles.is_empty() {
        list.retain(|s| !is_extra_clip(s, &ctx.titles));
    }
    for s in list.iter_mut() {
        let title_ok = ctx.titles.is_empty() || names_title_any(s, &ctx.titles, ctx.year);
        let ep_ok = ctx.episode_code.is_none_or(|ep| !wrong_episode(s, ep));
        s.off_title = !(title_ok && ep_ok);
    }
}

/// Classify and sort sources for the manual Source Chooser using `ctx`: on-title direct sources
/// always come first, while loose add-on matches for another film (e.g. "Judy" returned under
/// "Digger") are flagged `off_title` and pushed to the bottom.
pub(super) fn classify_manual_sources_with(list: &mut [Source], ctx: &AutoplayContext) {
    for s in list.iter_mut() {
        let title_ok = ctx.titles.is_empty() || names_title_any(s, &ctx.titles, ctx.year);
        let ep_ok = ctx.episode_code.is_none_or(|ep| !wrong_episode(s, ep));
        s.off_title = !(title_ok && ep_ok);
    }
    list.sort_by_key(|x| (x.off_title, x.kind != SourceKind::Direct, quality_rank(x.quality)));
}

/// Classify and sort sources for the manual Source Chooser.
pub fn classify_manual_sources(list: &mut [Source], media: &Media, video_id: &str) {
    let ctx = autoplay_context_for_media(media, video_id, None);
    classify_manual_sources_with(list, &ctx);
}

/// Classify and rank sources for the `--sources` developer CLI using the exact same rules as autoplay.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn play_order_for_cli(list: &mut Vec<Source>, ctx: &AutoplayContext) -> Vec<Source> {
    classify_sources(list, ctx);
    play_order_with_ctx(list, ctx)
}

/// Pre-probe ranking using the unified ranker and current connection budget.
pub(super) fn play_order_with_ctx(sources: &[Source], ctx: &AutoplayContext) -> Vec<Source> {
    let lang = ctx.preferred_language.as_deref().and_then(crate::addons::language);
    let budget = BandwidthBudget::current();
    let dv_screen = screen_supports_dv();
    let mut out: Vec<Source> = sources.iter().filter(|s| playable(s)).cloned().collect();
    out.sort_by_cached_key(|s| {
        let sz = episode_size(s, ctx.pack_episodes).unwrap_or(0);
        unified_key(s, sz, None, false, lang, ctx.expected_seconds, budget, dv_screen, None)
    });
    out
}

/// Play's source lists, fetched ahead (`prefetch_sources`) or by Play itself, kept for a while:
/// Play joins a fetch already under way instead of asking the add-ons again. The flag says
/// whether every add-on answered; a list without the slower ones is kept only briefly.
pub(super) type Ahead = std::sync::Arc<tokio::sync::OnceCell<(Vec<Source>, bool)>>;
pub(super) static AHEAD: OnceLock<Mutex<HashMap<String, (std::time::Instant, Ahead)>>> = OnceLock::new();
pub(super) const AHEAD_TTL: Duration = Duration::from_secs(10 * 60);
pub(super) const PARTIAL_TTL: Duration = Duration::from_secs(60);

/// Forget the source lists and link checks kept so far: the key, the add-ons or the language
/// changed, so they may hold links that no longer work (or miss new add-ons).
pub fn forget_sources() {
    if let Some(Ok(mut map)) = AHEAD.get().map(|m| m.lock()) { map.clear(); }
    if let Some(Ok(mut map)) = PROBED.get().map(|m| m.lock()) { map.clear(); }
}

pub(super) async fn play_sources(kind: &str, video_id: &str, early: Option<tokio::sync::mpsc::UnboundedSender<Vec<Source>>>) -> Vec<Source> {
    let cell = {
        let Ok(mut map) = AHEAD.get_or_init(Default::default).lock() else { return Vec::new() };
        // Expired lists, and fetches that found nothing (an add-on may have been down), are asked again.
        map.retain(|_, (at, cell)| {
            let ttl = if cell.get().is_some_and(|(_, complete)| !complete) { PARTIAL_TTL } else { AHEAD_TTL };
            at.elapsed() < ttl && cell.get().is_none_or(|(v, _)| !v.is_empty())
        });
        map.entry(format!("{kind}/{video_id}")).or_insert_with(|| (std::time::Instant::now(), Ahead::default())).1.clone()
    };
    let (kind, id) = (kind.to_owned(), video_id.to_owned());
    cell.get_or_init(|| async move { sources_within(&kind, &id, Some(ADDON_GRACE), early).await.map(|r| (r.0, r.2)).unwrap_or_default() }).await.0.clone()
}

/// Ask installed add-ons for a video's sources before Play is pressed, only when the viewer
/// opted into browsing prefetch in Settings (off by default to protect browsing privacy).
pub fn prefetch_sources(kind: &str, video_id: &str) {
    if !prefetch_sources_enabled() { return; }
    let (kind, id) = (kind.to_owned(), video_id.to_owned());
    if let Some(runtime) = RUNTIME.get() { runtime.spawn(async move { play_sources(&kind, &id, None).await; }); }
}

/// Developer option: Play's automatic choice for each "kind/id/title/year" line, one after the
/// other, nothing played: cold (sources asked for then), then again with the sources fetched
/// ahead as a title page does. Times and picks go to the developer log.
pub async fn dev_autoplay(list: String) {
    for pass in ["cold", "ahead"] {
        for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let parts: Vec<&str> = line.splitn(4, '/').collect();
            let [kind, id, ..] = parts[..] else { continue };
            let (name, year) = (parts.get(2).copied().unwrap_or(""), parts.get(3).and_then(|y| y.parse().ok()));
            dev_log(&format!("bench {pass} {name}"));
            let t = std::time::Instant::now();
            let pick = autoplay(kind, id, (kind == "series").then_some(10), name, year).await;
            dev_log(&format!("bench {pass} {name}: {} ms -> {}", t.elapsed().as_millis(), pick.best.as_ref().map(source_summary).unwrap_or_else(|| "nothing".into())));
        }
    }
}

/// "4K HDR 58.2 GiB REMUX (Torrentio)": what the log says about a source; never its link.
pub(crate) fn source_summary(s: &Source) -> String {
    format!("{} {} {} ({})", s.quality.unwrap_or("?"), s.hdr.unwrap_or("SDR"), s.size.as_deref().unwrap_or("size?"), s.addon)
}

/// One line in the developer log (logcat "veylo" on Android, stderr elsewhere). Never URLs or keys.
pub(crate) fn dev_log(message: &str) {
    record_diag_line(message);
    #[cfg(target_os = "android")]
    crate::android::log(message);
    #[cfg(not(target_os = "android"))]
    eprintln!("{message}");
}

/// Mirrors of the same underlying release (same normalized stem + similar size) grouped together
/// so when mirror #1 fails (e.g. uncached debrid clip), autoplay immediately fails over to mirror #2.
#[derive(Clone, Debug)]
pub(super) struct ReleaseGroup {
    pub mirrors: Vec<Source>,
}

fn normalized_release_stem(s: &Source) -> String {
    let raw = if !s.filename.is_empty() {
        s.filename.rsplit(['/', '\\']).next().unwrap_or(&s.filename)
    } else {
        s.description.lines().next().unwrap_or("")
    };
    let stripped = video_ext_suffix().replace_all(raw, "");
    stripped.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn similar_size(a: Option<u64>, b: Option<u64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) if x > 0 && y > 0 => {
            let (lo, hi) = (x.min(y) as f64, x.max(y) as f64);
            (hi - lo) / hi <= 0.08
        }
        _ => true,
    }
}

pub(super) fn group_releases(candidates: &[Source], pack_episodes: Option<usize>) -> Vec<ReleaseGroup> {
    let mut groups: Vec<(String, Option<u64>, ReleaseGroup)> = Vec::new();
    let mut seen_urls: HashSet<&str> = HashSet::new();
    for c in candidates {
        if !seen_urls.insert(c.url.as_str()) { continue; }
        let stem = normalized_release_stem(c);
        let ep_sz = episode_size(c, pack_episodes);
        if !stem.is_empty() {
            if let Some((_, _, g)) = groups.iter_mut().find(|(st, sz, _)| *st == stem && (similar_size(*sz, ep_sz) || similar_size(*sz, c.size_bytes))) {
                g.mirrors.push(c.clone());
                continue;
            }
        }
        groups.push((stem, ep_sz.or(c.size_bytes), ReleaseGroup { mirrors: vec![c.clone()] }));
    }
    groups.into_iter().map(|(_, _, g)| g).collect()
}

/// Simple wrapper kept for callers/tests without full metadata context.
pub async fn pick_source(candidates: Vec<Source>, pack_episodes: Option<usize>) -> Pick {
    let ctx = AutoplayContext { pack_episodes, preferred_language: stream_language(), ..AutoplayContext::default() };
    pick_source_with(candidates, &ctx).await
}

/// Self-test hook: exercises off-thread `display::screen()` and the Tokio autoplay ranking path
/// (same worker pool as `probe_wave`) so an unguarded Dioxus runtime call on Windows/macOS is
/// caught in CI before release.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) async fn selftest_autoplay_off_thread() -> Result<String> {
    let dv_screen = screen_supports_dv();
    let sample = source_of("SelfTest", &json!({
        "title": "SelfTest.2026.2160p.WEB-DL.HDR10.HEVC.mkv",
        "url": "https://self-test.invalid/clip.mkv",
        "behaviorHints": { "filename": "SelfTest.2026.2160p.WEB-DL.HDR10.HEVC.mkv", "videoSize": 12_000_000_000u64 }
    }));
    let lang = crate::addons::language("english");
    let ranked = on_runtime(async move {
        let _ = crate::display::screen();
        let budget = BandwidthBudget::unlimited();
        let _ = optimistic_key(&sample, 12_000_000_000, lang, Some(6600.0), budget, dv_screen);
        let working = vec![VerifiedSource { source: sample, audio: Some((true, true)), has_original_lang: true }];
        let _ = rivals_wanted_verified(&working, lang, None, dv_screen);
        rank_verified(working, lang, None, Some(6600.0), budget, dv_screen)
    }).await?;
    anyhow::ensure!(ranked.len() == 1, "expected 1 ranked self-test source");
    Ok(format!("off-thread display & autoplay ok (dv={dv_screen})"))
}

/// Verified working source ready for unified final ranking.
#[derive(Clone, Debug)]
pub(super) struct VerifiedSource {
    pub source: Source,
    pub audio: AudioVerdict,
    pub has_original_lang: bool,
}

/// Aggregated failure counts across probe waves so `chose nothing` logs why candidates failed
/// (never URLs or keys).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ProbeStats {
    probed: usize,
    timeout: usize,
    http_429: usize,
    http_4xx: usize,
    http_5xx: usize,
    debrid: usize,
    short_clip: usize,
    small: usize,
}

impl ProbeStats {
    fn add(&mut self, other: Self) {
        self.probed += other.probed;
        self.timeout += other.timeout;
        self.http_429 += other.http_429;
        self.http_4xx += other.http_4xx;
        self.http_5xx += other.http_5xx;
        self.debrid += other.debrid;
        self.short_clip += other.short_clip;
        self.small += other.small;
    }
    fn summary(&self) -> String {
        format!(
            "probed={} timeout={} 429={} 4xx={} 5xx={} debrid={} short={} small={}",
            self.probed, self.timeout, self.http_429, self.http_4xx, self.http_5xx, self.debrid, self.short_clip, self.small
        )
    }
}

/// Wave-probing source picker with in-wave mirror failover, live CDN bandwidth measurement,
/// MP4 tail-moov parsing, duration sanity checks, and unified post-probe ranking.
pub async fn pick_source_with(candidates: Vec<Source>, ctx: &AutoplayContext) -> Pick {
    let lang = ctx.preferred_language.as_deref().and_then(crate::addons::language);
    let orig_lang = ctx.original_language.as_deref();
    let started = std::time::Instant::now();
    let mut groups = group_releases(&candidates, ctx.pack_episodes);
    let mut probed_urls: HashSet<String> = HashSet::new();
    let mut working: Vec<VerifiedSource> = Vec::new();
    let mut debrid = None;
    let mut cursor = 0usize;
    let mut probed_groups = 0usize;
    let mut stats = ProbeStats::default();
    let dv_screen = screen_supports_dv();
    let mut last_budget = BandwidthBudget::current();
    // Allow at most ONE cold-start speed burst per autoplay when Auto bandwidth has no fresh sample.
    let speed_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
        max_bitrate_mbps().is_none() && connection_mbps().is_none(),
    ));
    loop {
        let take = if probed_groups == 0 { PROBE_FIRST } else { PROBE_NEXT }.min(PROBE_MAX.saturating_sub(probed_groups));
        if take == 0 || cursor >= groups.len() { break; }
        let end = (cursor + take).min(groups.len());
        let wave: Vec<ReleaseGroup> = groups[cursor..end].to_vec();
        cursor = end;
        probed_groups += wave.len();
        for g in &wave {
            for m in g.mirrors.iter().take(MAX_MIRRORS_PER_GROUP) { probed_urls.insert(m.url.clone()); }
        }
        let cap = if !working.is_empty() {
            Some(tokio::time::Instant::from_std((started + PROBE_BUDGET).min(std::time::Instant::now() + PROBE_GRACE_HIGH)))
        } else {
            Some(tokio::time::Instant::from_std(started + PROBE_EMPTY_BUDGET))
        };
        let (found, status, wave_stats) = probe_wave(wave, lang, orig_lang, ctx, cap, speed_token.clone(), dv_screen).await;
        stats.add(wave_stats);
        if status.is_some() { debrid = status; }
        working.extend(found);
        // If Wave 1's cold-start burst measured a real connection speed, re-rank remaining unprobed
        // groups under the new BandwidthBudget so fitting encodes move ahead of OverBudget remuxes.
        let budget = BandwidthBudget::current();
        if budget != last_budget && cursor < groups.len() {
            last_budget = budget;
            groups[cursor..].sort_by_cached_key(|g| {
                let rep = &g.mirrors[0];
                let sz = episode_size(rep, ctx.pack_episodes).unwrap_or(0);
                unified_key(rep, sz, None, false, lang, ctx.expected_seconds, budget, dv_screen, None)
            });
        }
        if working.is_empty() {
            if started.elapsed() >= PROBE_EMPTY_BUDGET { break; }
            continue;
        }
        let best_key = working.iter()
            .map(|w| unified_key(&w.source, episode_size(&w.source, ctx.pack_episodes).unwrap_or(0), w.audio, w.has_original_lang, lang, ctx.expected_seconds, budget, dv_screen, None))
            .min()
            .expect("non-empty");
        // Stop immediately if no remaining unprobed group can optimistically beat our best working
        // file (and no remux rival is still wanted).
        let could_win = groups[cursor..].iter().take(PROBE_MAX).any(|g| {
            let c = &g.mirrors[0];
            let sz = episode_size(c, ctx.pack_episodes).unwrap_or(0);
            optimistic_key(c, sz, lang, ctx.expected_seconds, budget, dv_screen) < best_key
        }) || (started.elapsed() < PROBE_GRACE_HIGH
            && rivals_wanted_verified(&working, lang, ctx.pack_episodes, dv_screen).is_some_and(|class| {
                groups[cursor..].iter().take(PROBE_NEXT * 2)
                    .any(|g| g.mirrors.first().is_some_and(|c| remux_class(c) == Some(class)))
            }));
        if !could_win || probed_groups >= PROBE_MAX || started.elapsed() >= PROBE_BUDGET { break; }
    }
    if working.is_empty() {
        dev_log(&format!("probe: 0 working ({}, debrid={:?}) in {} ms", stats.summary(), debrid, started.elapsed().as_millis()));
        return Pick { best: None, rest: Vec::new(), debrid, audio_known: false };
    }
    let budget = BandwidthBudget::current();
    let ranked = rank_verified(working, lang, ctx.pack_episodes, ctx.expected_seconds, budget, dv_screen);
    dev_log(&format!("checked {} working files in {} ms, audio: {}; pictures: {}", ranked.len(), started.elapsed().as_millis(),
        ranked.iter().map(|w| match w.audio { Some((true, true)) => "main", Some((true, false)) => "yes", Some((false, _)) => "no", None => "?" }).collect::<Vec<_>>().join(" "),
        ranked.iter().map(|w| picture_summary(&w.source)).collect::<Vec<_>>().join(" | ")));
    let audio_known = ranked[0].audio.is_some();
    let mut ordered: Vec<Source> = ranked.into_iter().map(|w| w.source).collect();
    let mut rest = ordered.split_off(1);
    rest.extend(candidates.into_iter().filter(|c| !probed_urls.contains(&c.url)));
    Pick { best: ordered.pop(), rest, debrid: None, audio_known }
}

/// What a file's tracks say about the Audio-language setting: (it has that language, its main
/// track is in it); None when unread or unclear.
pub(super) type AudioVerdict = Option<(bool, bool)>;

/// Single unified post-probe ranker: preserves `off_title`, connection `BandwidthBudget`,
/// verified audio language (keeping untagged English in tier 0, with original-language fallback),
/// verified HDR/resolution/bitrate, and remux primary-language tiebreaking.
pub(super) fn rank_verified(
    mut working: Vec<VerifiedSource>,
    lang: Option<&crate::addons::Language>,
    pack_episodes: Option<usize>,
    expected_seconds: Option<f64>,
    budget: BandwidthBudget,
    dolby_vision_screen: bool,
) -> Vec<VerifiedSource> {
    // Compute max effective size per same-class remux group (among fitting sources with same
    // off_title, over-budget state & language tier) so an original-language remux beats a larger multi-dub remux.
    let lang_tier_of = |w: &VerifiedSource| match (lang, w.audio) {
        (None, _) => 0u8,
        (Some(_), Some((true, _))) => 0,
        (Some(l), None) => {
            if l.matches(&w.source.label) || l.key == "english" { 0 }
            else if other_language_only(&w.source.label, l.key) { 2 }
            else { 1 }
        }
        (Some(_), Some((false, _))) => if w.has_original_lang { 2 } else { 3 },
    };
    let mut remux_max: HashMap<(bool, bool, u8, RemuxClass), u64> = HashMap::new();
    for w in &working {
        if let Some(class) = remux_class(&w.source) {
            let sz = episode_size(&w.source, pack_episodes).unwrap_or(0);
            let over = exceeds_budget(&w.source, sz, expected_seconds, budget);
            let eff = effective_size(&w.source, sz);
            let entry = remux_max.entry((w.source.off_title, over, lang_tier_of(w), class)).or_insert(0);
            if eff > *entry { *entry = eff; }
        }
    }
    working.sort_by_cached_key(|w| {
        let sz = episode_size(&w.source, pack_episodes).unwrap_or(0);
        let over = exceeds_budget(&w.source, sz, expected_seconds, budget);
        let group_sz = remux_class(&w.source).and_then(|c| remux_max.get(&(w.source.off_title, over, lang_tier_of(w), c)).copied());
        unified_key(&w.source, sz, w.audio, w.has_original_lang, lang, expected_seconds, budget, dolby_vision_screen, group_sz)
    });
    working
}

/// Backwards-compatible wrapper used by existing unit tests.
#[cfg(test)]
pub(super) fn rank_by_audio(working: Vec<Source>, verdicts: Vec<AudioVerdict>) -> Vec<(AudioVerdict, Source)> {
    let items: Vec<VerifiedSource> = working.into_iter().zip(verdicts)
        .map(|(source, audio)| VerifiedSource { source, audio, has_original_lang: false })
        .collect();
    let lang = crate::addons::language("english");
    rank_verified(items, lang, None, None, BandwidthBudget::unlimited(), false)
        .into_iter()
        .map(|w| (w.audio, w.source))
        .collect()
}

/// A remux's picture class (resolution, HDR, Dolby Vision only, upscaled): remuxes of one class
/// carry the same picture. None for encodes, whose size is their bitrate.
pub(super) type RemuxClass = (Option<&'static str>, bool, bool, bool);
pub(super) fn remux_class(s: &Source) -> Option<RemuxClass> {
    let hdr = verified_hdr_tier(s);
    (source_tier(&s.label) == Some(4)).then(|| (s.quality, hdr >= 2, hdr == 1, upscaled().is_match(&s.label)))
}

/// Working remuxes of the best one's class wanted before probing stops, so their audio can decide.
pub(super) const REMUX_RIVALS: usize = 3;

/// The best working source's remux class while fewer than `REMUX_RIVALS` of it work.
#[cfg(test)]
pub(super) fn rivals_wanted(working: &[Source], lang: Option<&crate::addons::Language>, pack_episodes: Option<usize>) -> Option<RemuxClass> {
    rivals_wanted_for(working, lang, pack_episodes, screen_supports_dv())
}

pub(super) fn rivals_wanted_for(working: &[Source], lang: Option<&crate::addons::Language>, pack_episodes: Option<usize>, dolby_vision_screen: bool) -> Option<RemuxClass> {
    let best = working.iter().min_by_key(|s| order_key_for(s, episode_size(s, pack_episodes).unwrap_or(0), lang, dolby_vision_screen))?;
    let class = remux_class(best)?;
    (working.iter().filter(|s| remux_class(s) == Some(class)).count() < REMUX_RIVALS).then_some(class)
}

fn rivals_wanted_verified(working: &[VerifiedSource], lang: Option<&crate::addons::Language>, pack_episodes: Option<usize>, dolby_vision_screen: bool) -> Option<RemuxClass> {
    let sources: Vec<Source> = working.iter().map(|w| w.source.clone()).collect();
    rivals_wanted_for(&sources, lang, pack_episodes, dolby_vision_screen)
}

/// How long a working file's header may take to read.
pub(super) const AUDIO_TIMEOUT: Duration = Duration::from_millis(1400);

/// What checking one link found: it works (and its real size, debrid status clip, audio tracks,
/// picture, video codec, and final resolved CDN URL).
#[derive(Clone, Debug)]
pub(super) struct ProbeOutcome {
    ok: bool,
    status: u64,
    bytes: Option<u64>,
    debrid: Option<String>,
    tracks: Option<Vec<crate::tracks::AudioTrack>>,
    picture: Option<crate::tracks::Picture>,
    video_codec: Option<&'static str>,
    resolved: Option<String>,
}

/// Checks by link, for a while: working links stay cached for 5 min; hard failures (debrid status
/// clips, 404/410, non-video files) stay cached for 45 s so waves never re-hit dead clips;
/// transient timeouts and 429s stay retryable.
pub(super) type Probed = std::sync::Arc<tokio::sync::OnceCell<ProbeOutcome>>;
pub(super) static PROBED: OnceLock<Mutex<HashMap<String, (std::time::Instant, Probed)>>> = OnceLock::new();
pub(super) const PROBED_TTL: Duration = Duration::from_secs(5 * 60);
pub(super) const NEG_PROBED_TTL: Duration = Duration::from_secs(45);
/// Checks under way at once for one add-on's links (bounded to prevent debrid 429 storms).
pub(super) const PROBES_PER_ADDON: usize = 6;
/// The pause before checking a link again that answered "too many requests".
pub(super) const RETRY_AFTER_429: Duration = Duration::from_millis(600);
pub(super) static PROBE_GATES: OnceLock<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Semaphore>>>> = OnceLock::new();

/// Dedicated client for container header reads: never follows redirects automatically so
/// origin-scoped auth headers cannot leak across origins and HTTPS cannot downgrade to HTTP.
fn header_http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(4))
            .timeout(Duration::from_secs(8))
            .build()
            .expect("header http client")
    })
}

/// The relay's one-byte check of a link, then (when it works) its header for audio tracks,
/// picture, video codec, and optional zero-extra-request CDN speed burst.
pub(super) async fn probe_one(src: Source, allow_speed_burst: bool) -> Option<ProbeOutcome> {
    let cell = {
        let mut map = PROBED.get_or_init(Default::default).lock().ok()?;
        map.retain(|_, (at, cell)| {
            let ttl = match cell.get() {
                Some(o) if !o.ok => NEG_PROBED_TTL,
                _ => PROBED_TTL,
            };
            at.elapsed() < ttl
        });
        if map.len() >= 800 && !map.contains_key(&src.url) {
            // Evict oldest half rather than clearing all cached probes.
            let mut ages: Vec<(std::time::Instant, String)> = map.iter().map(|(k, (at, _))| (*at, k.clone())).collect();
            ages.sort_by_key(|(at, _)| *at);
            for (_, k) in ages.into_iter().take(400) { map.remove(&k); }
        }
        map.entry(src.url.clone()).or_insert_with(|| (std::time::Instant::now(), Probed::default())).1.clone()
    };
    match cell.get_or_try_init(|| async move {
        let gate = {
            let mut gates = PROBE_GATES.get_or_init(Default::default).lock().map_err(|_| None)?;
            gates.entry(src.addon.clone()).or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(PROBES_PER_ADDON))).clone()
        };
        let headers = Value::Object(src.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
        let check = || tokio::time::timeout(PROBE_TIMEOUT, backend().proxy.probe(&src.url, &headers, false));
        let mut probe = {
            let _turn = gate.clone().acquire_owned().await.map_err(|_| None)?;
            check().await.map_err(|_| None)?.map_err(|_| None)?
        };
        if probe["status"] == 429 {
            // Release semaphore during backoff and honour Retry-After (clamped 500..=3000 ms) with jitter.
            let base_ms = probe["retry_after_ms"].as_u64()
                .map(|ms| ms.clamp(500, 3000))
                .unwrap_or(RETRY_AFTER_429.as_millis() as u64);
            let jitter = rand_u64() % 250;
            tokio::time::sleep(Duration::from_millis(base_ms + jitter)).await;
            let _turn = gate.clone().acquire_owned().await.map_err(|_| None)?;
            probe = check().await.map_err(|_| None)?.map_err(|_| None)?;
        }
        let ok = probe["ok"] == true;
        let status = probe["status"].as_u64().unwrap_or(0);
        let bytes = probe["bytes"].as_u64();
        let debrid = probe["debrid_status"].as_str().map(str::to_owned);
        let playlist = probe["playlist"] == true;
        let probe_resolved = probe["resolved_url"].as_str().filter(|u| u.starts_with("https://")).map(str::to_owned);
        // Header reads hit the CDN, not the add-on: run outside the per-addon semaphore gate.
        let (tracks, picture, video_codec, header_resolved) = if ok && !playlist {
            tokio::time::timeout(AUDIO_TIMEOUT, file_header(src, probe_resolved.clone(), bytes, allow_speed_burst)).await.unwrap_or_default()
        } else {
            (None, None, None, None)
        };
        let resolved = header_resolved.or(probe_resolved);
        let hard_fail = !ok && (debrid.is_some() || matches!(status, 404 | 410 | 403 | 200 | 206));
        let outcome = ProbeOutcome { ok, status, bytes, debrid, tracks, picture, video_codec, resolved };
        if ok || hard_fail { Ok(outcome) } else { Err(Some(outcome)) }
    }).await {
        Ok(found) => Some(found.clone()),
        Err(answer) => answer,
    }
}

fn same_origin(a: &str, b: &str) -> bool {
    let (Ok(ua), Ok(ub)) = (url::Url::parse(a), url::Url::parse(b)) else { return false };
    ua.scheme() == ub.scheme() && ua.host_str() == ub.host_str() && ua.port_or_known_default() == ub.port_or_known_default()
}

/// Read container header (MKV/MP4 head, plus fast MP4 tail-moov fallback for non-fast-start files),
/// optionally continuing on the exact same open HTTP stream for up to 360 ms / 1.5 MiB to seed
/// the live CDN bandwidth estimator with zero extra round-trips.
pub(super) async fn file_header(
    src: Source,
    probe_resolved: Option<String>,
    total_bytes: Option<u64>,
    measure_speed: bool,
) -> (Option<Vec<crate::tracks::AudioTrack>>, Option<crate::tracks::Picture>, Option<&'static str>, Option<String>) {
    let (tracks, pic, codec, resolved, speed_sample) =
        file_header_with_client_full(header_http(), src, probe_resolved, total_bytes, measure_speed).await;
    if let Some((bytes, elapsed)) = speed_sample {
        backend().proxy.record_bandwidth_sample(bytes, elapsed);
    }
    (tracks, pic, codec, resolved)
}

#[cfg(test)]
pub(super) async fn file_header_with_client(
    http: &reqwest::Client,
    src: Source,
    probe_resolved: Option<String>,
) -> (Option<Vec<crate::tracks::AudioTrack>>, Option<crate::tracks::Picture>, Option<String>) {
    let (tracks, pic, _, resolved, _) = file_header_with_client_full(http, src, probe_resolved, None, false).await;
    (tracks, pic, resolved)
}

pub(super) async fn file_header_with_client_full(
    http: &reqwest::Client,
    src: Source,
    probe_resolved: Option<String>,
    total_bytes: Option<u64>,
    measure_speed: bool,
) -> (
    Option<Vec<crate::tracks::AudioTrack>>,
    Option<crate::tracks::Picture>,
    Option<&'static str>,
    Option<String>,
    Option<(u64, Duration)>,
) {
    let target = probe_resolved.as_deref().unwrap_or(&src.url);
    let range_end = if measure_speed { SPEED_BURST_BYTES - 1 } else { crate::tracks::HEAD_BYTES - 1 };
    let mut req = http.get(target).header("range", format!("bytes=0-{range_end}"));
    // Forward origin-scoped headers only when fetching the original origin (never leak to redirected CDNs).
    if same_origin(&src.url, target) {
        for (k, v) in &src.headers { req = req.header(k.as_str(), v.as_str()); }
    }
    let Ok(mut response) = req.send().await else { return (None, None, None, probe_resolved, None) };
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT { return (None, None, None, probe_resolved, None); }
    let resolved = Some(response.url().to_string()).filter(|u| u.starts_with("https://") && *u != src.url).or(probe_resolved);
    let read_started = std::time::Instant::now();
    // Skip the first 128 KiB (TCP slow-start ramp) when timing throughput so fast connections
    // aren't underestimated by 2-3x on a fresh TLS socket.
    let mut post_warmup_at: Option<std::time::Instant> = None;
    let mut post_warmup_bytes = 0u64;
    let mut head = Vec::with_capacity(65536);
    let mut total_read = 0u64;
    let mut next_parse = 16 * 1024;
    let mut parsed: Option<(Vec<crate::tracks::AudioTrack>, Option<crate::tracks::Picture>, Option<&'static str>)> = None;

    fn account_chunk(n: u64, total_read: &mut u64, post_warmup_at: &mut Option<std::time::Instant>, post_warmup_bytes: &mut u64) {
        *total_read += n;
        if post_warmup_at.is_some() {
            *post_warmup_bytes += n;
        } else if *total_read >= 128 * 1024 {
            *post_warmup_at = Some(std::time::Instant::now());
        }
    }

    while head.len() < crate::tracks::HEAD_BYTES {
        let Ok(Some(chunk)) = response.chunk().await else { break };
        account_chunk(chunk.len() as u64, &mut total_read, &mut post_warmup_at, &mut post_warmup_bytes);
        head.extend_from_slice(&chunk);
        if head.len() >= next_parse {
            if let Some(tracks) = crate::tracks::audio_tracks(&head) {
                parsed = Some((tracks, crate::tracks::picture(&head), crate::tracks::video_codec(&head)));
                break;
            }
            // Non-fast-start MP4 (`ftyp` + `mdat` with no `moov` in head): don't waste bandwidth reading 1 MiB of video data!
            if crate::tracks::mp4_moov_at_end(&head) { break; }
            next_parse = (next_parse * 2).min(crate::tracks::HEAD_BYTES);
        }
    }

    // Zero-extra-request CDN speed burst: if requested (cold start) and we haven't read enough yet,
    // keep draining the same open response up to 1.5 MiB or 360 ms max (enough to measure slower
    // 5-15 Mbps links accurately so Auto bitrate fitting caps huge 4K remuxes).
    let mut speed_sample = None;
    if measure_speed {
        while (total_read as usize) < SPEED_BURST_BYTES && read_started.elapsed() < SPEED_BURST_MAX_TIME {
            let remain = SPEED_BURST_MAX_TIME.saturating_sub(read_started.elapsed());
            match tokio::time::timeout(remain, response.chunk()).await {
                Ok(Ok(Some(chunk))) => account_chunk(chunk.len() as u64, &mut total_read, &mut post_warmup_at, &mut post_warmup_bytes),
                _ => break,
            }
        }
    }
    if let Some(since) = post_warmup_at {
        let elapsed = since.elapsed();
        if (post_warmup_bytes >= 256 * 1024 && elapsed >= Duration::from_millis(5))
            || (measure_speed && post_warmup_bytes >= 64 * 1024 && elapsed >= Duration::from_millis(180))
        {
            speed_sample = Some((post_warmup_bytes, elapsed));
        }
    } else if measure_speed && total_read >= 64 * 1024 && read_started.elapsed() >= Duration::from_millis(180) {
        speed_sample = Some((total_read, read_started.elapsed()));
    }
    drop(response);

    if let Some((tracks, pic, codec)) = parsed {
        return (Some(tracks), pic, codec, resolved, speed_sample);
    }
    let tracks = crate::tracks::audio_tracks(&head);
    let pic = crate::tracks::picture(&head);
    let codec = crate::tracks::video_codec(&head);
    if tracks.is_some() || pic.is_some() {
        return (tracks, pic, codec, resolved, speed_sample);
    }

    // Non-fast-start MP4 fallback: read the last 1 MiB from the resolved URL to parse tail `moov`.
    if crate::tracks::mp4_moov_at_end(&head) && total_bytes.is_some_and(|b| b > 1_048_576) {
        let tail_target = resolved.as_deref().unwrap_or(&src.url);
        let mut treq = http.get(tail_target).header("range", "bytes=-1048576");
        if same_origin(&src.url, tail_target) {
            for (k, v) in &src.headers { treq = treq.header(k.as_str(), v.as_str()); }
        }
        if let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_millis(600), treq.send()).await {
            if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                if let Ok(Ok(tail)) = tokio::time::timeout(Duration::from_millis(600), resp.bytes()).await {
                    let (t_tracks, t_pic, t_codec) = crate::tracks::parse_mp4_tail(&tail);
                    return (t_tracks, t_pic, t_codec.or(codec), resolved, speed_sample);
                }
            }
        }
    }

    (None, None, codec, resolved, speed_sample)
}

/// A working file as its header declares it: its resolution and HDR replace the label's (the
/// label stays where the header can't tell).
pub(super) fn with_picture(mut src: Source, picture: crate::tracks::Picture) -> Source {
    let class = crate::player::resolution_class(picture.width as i64, picture.height as i64);
    if !class.is_empty() { src.quality = Some(class); }
    src.picture = Some(picture);
    src.hdr = match verified_hdr_tier(&src) { 3 => Some("HDR10+"), 2 => Some("HDR"), 1 => Some("DV"), _ => None };
    src
}

/// "3840x1608 PQ DV8 24 Mb/s": what the log says about a file's own picture.
pub(super) fn picture_summary(s: &Source) -> String {
    let Some(p) = s.picture else { return "?".into() };
    let transfer = match p.transfer { crate::tracks::Transfer::Pq => " PQ", crate::tracks::Transfer::Hlg => " HLG", crate::tracks::Transfer::Sdr => " SDR", _ => "" };
    let dv = p.dolby_vision.map(|d| format!(" DV{d}")).unwrap_or_default();
    let rate = p.seconds.zip(s.size_bytes).map(|(sec, b)| format!(" {:.0} Mb/s", b as f64 * 8.0 / sec / 1e6)).unwrap_or_default();
    format!("{}x{}{transfer}{dv}{rate}", p.width, p.height)
}

/// Reject hidden trailers, bumpers, or broken short clips when the container reports a real duration.
pub(super) fn duration_plausible(pic: &crate::tracks::Picture, expected_seconds: Option<f64>, is_episode: bool) -> bool {
    let Some(secs) = pic.seconds else { return true };
    if secs < 120.0 { return false; }
    if let Some(exp) = expected_seconds.filter(|e| *e >= 1200.0) {
        let min_secs = if is_episode { (exp * 0.35).max(120.0) } else { (exp * 0.45).max(300.0) };
        if secs < min_secs { return false; }
    }
    true
}

/// Probe a wave of `ReleaseGroup`s concurrently. Within each group task, if mirror #1 fails
/// (e.g. uncached debrid status clip), immediately fail over to mirror #2/#3. Stops as soon as no
/// pending group's optimistic key can beat the best verified working candidate, or on a tier-aware
/// grace deadline.
pub(super) async fn probe_wave(
    wave: Vec<ReleaseGroup>,
    lang: Option<&'static crate::addons::Language>,
    orig_lang: Option<&str>,
    ctx: &AutoplayContext,
    cap: Option<tokio::time::Instant>,
    speed_token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dv_screen: bool,
) -> (Vec<VerifiedSource>, Option<String>, ProbeStats) {
    let pack_episodes = ctx.pack_episodes;
    let expected_seconds = ctx.expected_seconds;
    let is_episode = ctx.episode_code.is_some() || pack_episodes.is_some();
    let orig_key = orig_lang.map(str::to_owned);
    on_runtime(async move {
        let mut jobs = tokio::task::JoinSet::new();
        for (i, group) in wave.iter().enumerate() {
            let mirrors: Vec<Source> = group.mirrors.iter().take(MAX_MIRRORS_PER_GROUP).cloned().collect();
            let orig_key = orig_key.clone();
            let speed_token = speed_token.clone();
            jobs.spawn(async move {
                let mut last_status: Option<String> = None;
                let mut last_http_429 = false;
                let mut st = ProbeStats::default();
                for mirror in mirrors {
                    st.probed += 1;
                    let wants_burst = mirror.size_bytes.is_none_or(|b| b >= 3_000_000_000)
                        && speed_token.swap(false, std::sync::atomic::Ordering::Relaxed);
                    let outcome = probe_one(mirror.clone(), wants_burst).await;
                    // If a pre-warmed PROBED cache hit consumed the token without measuring speed,
                    // restore it so the next uncached mirror can still seed connection_mbps().
                    if wants_burst && connection_mbps().is_none() {
                        speed_token.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    let Some(result) = outcome else {
                        st.timeout += 1;
                        continue;
                    };
                    if !result.ok {
                        if result.debrid.is_some() { st.debrid += 1; last_status = result.debrid; }
                        else if result.status == 429 { st.http_429 += 1; last_http_429 = true; }
                        else if (400..500).contains(&result.status) { st.http_4xx += 1; }
                        else if result.status >= 500 { st.http_5xx += 1; }
                        else if matches!(result.status, 200 | 206) { st.small += 1; }
                        else { st.timeout += 1; }
                        continue;
                    }
                    if let Some(pic) = &result.picture {
                        if !duration_plausible(pic, expected_seconds, is_episode) {
                            st.short_clip += 1;
                            continue;
                        }
                    }
                    let audio = lang.zip(result.tracks.as_ref())
                        .and_then(|(l, t)| crate::tracks::in_language(t, l.key).map(|has| (has, crate::tracks::primary_in_language(t, l.key))));
                    let has_original_lang = orig_key.as_deref().zip(result.tracks.as_ref())
                        .is_some_and(|(o, t)| crate::tracks::in_language(t, o) == Some(true));
                    let mut src = mirror;
                    if let Some(picture) = result.picture { src = with_picture(src, picture); }
                    if let Some(tracks) = &result.tracks {
                        let main_count = tracks.iter().filter(|t| !t.extra()).count().min(u8::MAX as usize) as u8;
                        if main_count > 0 { src.audio_tracks_count = Some(main_count); }
                    }
                    if result.video_codec.is_some() { src.video_codec = result.video_codec; }
                    if let Some(url) = result.resolved {
                        // Never carry origin-scoped auth headers onto a redirected third-party CDN URL.
                        if !same_origin(&src.url, &url) {
                            src.headers.clear();
                        }
                        src.url = url;
                    }
                    if let Some(bytes) = result.bytes.filter(|b| *b > 0) {
                        src.size_bytes = Some(bytes);
                        src.size = Some(binary_size(bytes as f64));
                        src.declared_size = true;
                        src.labelled_bytes = None;
                        src.pack = false;
                    }
                    return (i, Some(VerifiedSource { source: src, audio, has_original_lang }), None, false, st);
                }
                (i, None, last_status, last_http_429, st)
            });
        }
        let mut pending: HashSet<usize> = (0..wave.len()).collect();
        let mut working: Vec<VerifiedSource> = Vec::new();
        let mut debrid = None;
        let mut wave_stats = ProbeStats::default();
        let mut deadline: Option<tokio::time::Instant> = cap;
        let mut first_working_at: Option<tokio::time::Instant> = None;
        let started = std::time::Instant::now();
        let trace = std::env::var_os("VEYLO_PROBE_TRACE").is_some();
        while !pending.is_empty() {
            let next = match deadline {
                Some(at) => match tokio::time::timeout_at(at, jobs.join_next()).await { Ok(n) => n, Err(_) => break },
                None => jobs.join_next().await,
            };
            let Some(Ok((i, found, status, hit_429, st))) = next else { if next.is_none() { break } else { continue } };
            wave_stats.add(st);
            pending.remove(&i);
            if trace {
                let rep = &wave[i].mirrors[0];
                let what = match &found {
                    Some(v) => format!("OK bytes={:?} {}", v.source.size_bytes, picture_summary(&v.source)),
                    None => format!("FAIL status={status:?} 429={hit_429}"),
                };
                // Never log raw filenames into DIAG_RING: log only technical summary.
                dev_log(&format!("  wave #{i} ({} mirrors) {} -> {what} at {} ms",
                    wave[i].mirrors.len(), source_summary(rep), started.elapsed().as_millis()));
            }
            match found {
                Some(verified) => working.push(verified),
                None => {
                    if status.is_some() { debrid = status; }
                    else if hit_429 && debrid.is_none() { debrid = Some("busy".into()); }
                    continue;
                }
            }
            let now = tokio::time::Instant::now();
            let first = *first_working_at.get_or_insert(now);
            let budget = BandwidthBudget::current();
            let best_key = working.iter()
                .map(|w| unified_key(&w.source, episode_size(&w.source, pack_episodes).unwrap_or(0), w.audio, w.has_original_lang, lang, expected_seconds, budget, dv_screen, None))
                .min()
                .expect("one works");
            let wanted = rivals_wanted_verified(&working, lang, pack_episodes, dv_screen);
            // Check if any still-pending group can optimistically beat our best verified source.
            let mut better_tier_pending = false;
            let mut any_can_beat = false;
            for &p in &pending {
                let rep = &wave[p].mirrors[0];
                let sz = episode_size(rep, pack_episodes).unwrap_or(0);
                let opt = optimistic_key(rep, sz, lang, expected_seconds, budget, dv_screen);
                if opt < best_key || (wanted.is_some() && remux_class(rep) == wanted) {
                    any_can_beat = true;
                    // Strictly higher tier (better budget fit, better language, or better HDR/resolution class).
                    let higher_quality = match (opt.7, best_key.7) {
                        (QualityOrder::Fit(a, _, _, _), QualityOrder::Fit(b, _, _, _)) => (a.0 .0, a.0 .1) > (b.0 .0, b.0 .1),
                        (QualityOrder::Fit(..), QualityOrder::OverBudget(..)) => true,
                        _ => false,
                    };
                    if (opt.0, opt.1, opt.2) < (best_key.0, best_key.1, best_key.2) || higher_quality {
                        better_tier_pending = true;
                    }
                }
            }
            if !any_can_beat { break; }
            let grace = if better_tier_pending { PROBE_GRACE_HIGH } else { PROBE_GRACE_SAME };
            let target = first + grace;
            deadline = Some(cap.map_or(target, |c| c.min(target)));
        }
        (working, debrid, wave_stats)
    }).await.unwrap_or_default()
}

/// `Pick::debrid` when the only direct links are under 1080p (Play never chooses those).
pub(super) const BELOW_1080P: &str = "below-1080p";
/// `Pick::debrid` when no playable source matched the requested title (strict title safety).
pub(super) const NO_MATCHING_TITLE: &str = "no-matching-title";

/// What to say when no checked source worked (the Android app's wording).
pub fn no_source_message(debrid: Option<&str>) -> &'static str {
    match debrid {
        Some("downloading") => "Your debrid service is still downloading this file. Try again later or choose another source.",
        Some("failed") => "Your debrid service could not provide this file. Choose another source.",
        Some("busy") => "Your sources are refusing requests for a moment (too many at once). Try again in a minute.",
        Some(BELOW_1080P) => "No source is 1080p or better, so Play didn't choose one. Pick one from the list.",
        Some(NO_MATCHING_TITLE) => "No source matched this title, so Play didn't start the wrong one. Pick one from the list.",
        _ => "No source responded. Try again or choose another source.",
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn playable_rejects_non_video_extensions_and_tiny_samples() {
        let mk = |title: &str, filename: &str, bytes: u64| source_of("A", &json!({
            "title": title, "url": "https://x.invalid/v",
            "behaviorHints": {"filename": filename, "videoSize": bytes}
        }));
        assert!(!playable(&mk("Film 2160p HDR", "Film.2160p.iso", 50_000_000_000)));
        assert!(!playable(&mk("Film 2160p HDR", "Film.2160p.rar", 50_000_000_000)));
        assert!(!playable(&mk("Film 2160p HDR", "Film.2160p.srt", 100_000)));
        assert!(!playable(&mk("Film.2025.2160p.SAMPLE.mkv", "Film.2025.2160p.sample.mkv", 50_000_000)));
        assert!(playable(&mk("Film.2025.2160p.HDR.mkv", "Film.2025.2160p.HDR.mkv", 25_000_000_000)));
        // A full film whose title happens to contain "Sample" is not rejected.
        assert!(playable(&mk("The Sample 2025 1080p WEB-DL", "The.Sample.2025.1080p.mkv", 5_000_000_000)));
    }

    #[test]
    fn dolby_vision_only_ranks_with_hdr_on_dv_screens_only() {
        let src = |title: &str| source_of("A", &json!({"title": title, "url": "https://x.invalid/a"}));
        let dv = src("Film 2025 2160p DV WEB-DL 💾 25 GB");
        let sdr = src("Film 2025 2160p WEB-DL 💾 30 GB");
        // Non-DV screen: SDR beats DV-only.
        assert!(order_key_for(&sdr, 30_000_000_000, None, false) < order_key_for(&dv, 25_000_000_000, None, false));
        // Native DV screen: DV-only beats SDR.
        assert!(order_key_for(&dv, 25_000_000_000, None, true) < order_key_for(&sdr, 30_000_000_000, None, true));
    }

    #[test]
    fn remux_class_uses_verified_header_hdr() {
        use crate::tracks::{Picture, Transfer};
        let mut unlabeled_hdr = source_of("A", &json!({"title": "Film 2160p REMUX 60 GB", "url": "https://x.invalid/1"}));
        unlabeled_hdr = with_picture(unlabeled_hdr, Picture { width: 3840, height: 2160, transfer: Transfer::Pq, dolby_vision: None, seconds: Some(7200.0) });
        let labeled_hdr = source_of("A", &json!({"title": "Film 2160p HDR REMUX 65 GB", "url": "https://x.invalid/2"}));
        assert_eq!(remux_class(&unlabeled_hdr), remux_class(&labeled_hdr));
    }

    #[test]
    fn same_origin_guards_redirected_header_forwarding() {
        assert!(same_origin("https://addon.example/a", "https://addon.example/b"));
        assert!(!same_origin("https://addon.example/a", "https://cdn.other.example/file.mkv"));
        assert!(!same_origin("https://addon.example:8443/a", "https://addon.example/a"));
    }

    #[test]
    fn autoplay_ranking_matrix_language_trial_and_rivals() {
        let mk = |addon: &str, title: &str, url: &str| source_of(addon, &json!({"title": title, "url": url}));
        let ita_4k = mk("Addon A", "Film 2025 2160p HDR ITA 💾 20 GB", "https://x.invalid/ita-4k");
        let ita_trial = mk("Addon A", "Film 2025 2160p HDR ITA FREE TRIAL 💾 30 GB", "https://x.invalid/ita-trial");
        let ita_1080 = mk("Addon B", "Film 2025 1080p ITA 💾 12 GB", "https://x.invalid/ita-1080");
        let multi = mk("Addon B", "Film 2025 2160p HDR MULTI 💾 40 GB", "https://x.invalid/multi");
        let eng_only = mk("Addon B", "Film 2025 2160p HDR ENG 💾 50 GB", "https://x.invalid/eng");
        let order = play_order_for(&[eng_only.clone(), ita_trial.clone(), multi.clone(), ita_4k.clone(), ita_1080.clone()], None, Some("italian"));
        let urls: Vec<&str> = order.iter().map(|s| s.url.as_str()).collect();
        // Tier 0 (Italian claimed): full 4K > full 1080p > trial; Tier 1 (untagged/MULTI); Tier 2 (other language only).
        assert_eq!(urls, [ita_4k.url.as_str(), ita_1080.url.as_str(), ita_trial.url.as_str(), multi.url.as_str(), eng_only.url.as_str()]);

        // Rivals wanted asks for up to REMUX_RIVALS working remuxes of the best class.
        let r1 = mk("Addon B", "Film 2025 2160p HDR REMUX 💾 65 GB", "https://x.invalid/r1");
        let r2 = mk("Addon B", "Film 2025 2160p HDR REMUX 💾 60 GB", "https://x.invalid/r2");
        let r3 = mk("Addon B", "Film 2025 2160p HDR REMUX 💾 58 GB", "https://x.invalid/r3");
        assert!(rivals_wanted(&[r1.clone(), r2.clone()], None, None).is_some());
        assert!(rivals_wanted(&[r1, r2, r3], None, None).is_none());
    }

    #[test]
    fn file_header_reads_chunked_206_and_never_leaks_headers_cross_origin() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let clip = include_bytes!("../../assets/selftest.mp4");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen_headers = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
            let seen_srv = seen_headers.clone();
            let server = tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut req_buf = [0u8; 2048];
                    let n = sock.read(&mut req_buf).await.unwrap_or(0);
                    seen_srv.lock().unwrap().push(String::from_utf8_lossy(&req_buf[..n]).to_lowercase());
                    let resp = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        clip.len() - 1, clip.len(), clip.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(clip).await;
                }
            });
            let client = reqwest::Client::new();
            let local_url = format!("http://127.0.0.1:{port}/video.mp4");
            // Same-origin fetch: Authorization header is sent; picture and audio track parse.
            let mut src = source_of("A", &json!({"title": "Film 2160p", "url": local_url}));
            src.headers.push(("Authorization".into(), "Bearer secret-token".into()));
            let (tracks, pic, _) = file_header_with_client(&client, src.clone(), None).await;
            assert_eq!(tracks.as_ref().map(Vec::len), Some(1));
            assert_eq!(pic.map(|p| (p.width, p.height)), Some((320, 240)));

            // Cross-origin resolved URL: Authorization header MUST NOT be forwarded.
            src.url = "https://addon.origin.invalid/resolve/123".into();
            let (tracks2, pic2, _) = file_header_with_client(&client, src, Some(local_url)).await;
            assert_eq!(tracks2.as_ref().map(Vec::len), Some(1));
            assert_eq!(pic2.map(|p| (p.width, p.height)), Some((320, 240)));

            server.await.unwrap();
            let reqs = seen_headers.lock().unwrap();
            assert!(reqs[0].contains("authorization: bearer secret-token"), "same-origin must send header");
            assert!(!reqs[1].contains("secret-token"), "cross-origin resolved URL must never receive origin secret headers");
        });
    }

    #[test]
    fn digger_rejects_telesync_and_off_title_never_beats_on_title() {
        let mk = |name: &str, bytes: u64| source_of("AIOStreams", &json!({
            "title": name, "url": format!("https://x.invalid/{}", name.len() + (bytes as usize % 997)),
            "behaviorHints": {"filename": name, "videoSize": bytes}
        }));
        // Bare TS / D.TS.1O8Op camrips are rejected, while MPEG-TS `.ts` container stays playable.
        assert!(!playable(&mk("Digger.2026.TS.1080p.MULTi.mkv", 4_600_000_000)));
        assert!(!playable(&mk("Digger.2026.D.TS.1O8Op.mkv", 4_400_000_000)));
        assert!(!playable(&mk("Digger.2026.TS.1080p.Dragon.Money.Studio", 4_400_000_000)));
        assert!(playable(&mk("Digger.2026.1080p.WEB.H264-POKE.mkv", 1_700_000_000)));
        assert!(playable(&mk("Film.2025.2160p.UHD.BluRay.REMUX.HEVC.ts", 55_000_000_000)));

        // Off-title Judy (with English audio) MUST NEVER beat on-title Digger even when Digger audio is unread/other.
        let mut digger = mk("Digger.2026.1080p.WEB.H264-POKE.mkv", 1_700_000_000);
        let mut judy = mk("Judy.BDRemux.1080p.pk.mkv", 18_400_000_000);
        digger.off_title = !names_title(&digger, "Digger", Some(2026));
        judy.off_title = !names_title(&judy, "Digger", Some(2026));
        assert!(!digger.off_title && judy.off_title);

        let ranked = rank_verified(
            vec![
                VerifiedSource { source: judy.clone(), audio: Some((true, true)), has_original_lang: true },
                VerifiedSource { source: digger.clone(), audio: Some((false, false)), has_original_lang: false },
            ],
            crate::addons::language("english"),
            None,
            Some(6600.0),
            BandwidthBudget::unlimited(),
            false,
        );
        assert_eq!(ranked[0].source.filename, digger.filename);

        // Manual Source Chooser also flags Judy as off-title and sorts Digger ahead of it.
        let ctx = AutoplayContext::simple("Digger", Some(2026), None);
        let mut manual = vec![judy.clone(), digger.clone()];
        classify_manual_sources_with(&mut manual, &ctx);
        assert_eq!(manual[0].filename, digger.filename);
        assert!(!manual[0].off_title);
        assert!(manual[1].off_title);
    }

    #[test]
    fn untagged_english_4k_beats_tagged_1080p_while_italian_prefers_italian() {
        let mk = |name: &str, url: &str, bytes: u64| source_of("AIOStreams", &json!({
            "title": name, "url": url,
            "behaviorHints": {"filename": name, "videoSize": bytes}
        }));
        let yts_4k = mk("Clashing.Through.The.Snow.2026.2160p.4K.WEB.x265.10bit.AAC5.1-YTS.mkv", "https://x.invalid/yts", 4_900_000_000);
        let mircrew = mk("Clashing Through the Snow (2026) 1080p H264 iTA EnG AC3-MIRCrew.mkv", "https://x.invalid/mir", 2_500_000_000);
        let amen_vf = mk("Clashing.Through.The.Snow.2026.MULTI.VF2.2160p.WEB.DV.HDR10+.H265-Amen.mkv", "https://x.invalid/amen", 10_200_000_000);

        // English user: untagged 4K YTS (audio: None) beats tagged 1080p MIRCrew (audio: Some((true, false))).
        let eng = rank_verified(
            vec![
                VerifiedSource { source: mircrew.clone(), audio: Some((true, false)), has_original_lang: true },
                VerifiedSource { source: yts_4k.clone(), audio: None, has_original_lang: false },
            ],
            crate::addons::language("english"),
            None,
            Some(6600.0),
            BandwidthBudget::unlimited(),
            false,
        );
        assert_eq!(eng[0].source.url, yts_4k.url);

        // Italian user: MIRCrew (verified Italian) beats Amen (French+English only) and untagged YTS.
        let ita = rank_verified(
            vec![
                VerifiedSource { source: amen_vf, audio: Some((false, false)), has_original_lang: true },
                VerifiedSource { source: yts_4k, audio: None, has_original_lang: false },
                VerifiedSource { source: mircrew.clone(), audio: Some((true, true)), has_original_lang: true },
            ],
            crate::addons::language("italian"),
            None,
            Some(6600.0),
            BandwidthBudget::unlimited(),
            false,
        );
        assert_eq!(ita[0].source.url, mircrew.url);
    }

    #[test]
    fn release_groups_preserve_mirrors_for_failover_and_dedup_across_scrapers() {
        let mk = |addon: &str, name: &str, url: &str, bytes: u64| source_of(addon, &json!({
            "title": name, "url": url,
            "behaviorHints": {"filename": name, "videoSize": bytes}
        }));
        let amen1 = mk("AIOStreams", "Clashing.Through.The.Snow.2026.2160p.HDR10+-Amen.mkv", "https://x.invalid/a1", 10_950_000_000);
        let amen2 = mk("AIOStreams", "Clashing.Through.The.Snow.2026.2160p.HDR10+-Amen.mkv", "https://x.invalid/a2", 10_950_000_000);
        let yts1 = mk("AIOStreams", "Clashing.Through.The.Snow.2026.2160p.WEB.x265-YTS.mkv", "https://x.invalid/y1", 5_260_000_000);
        let yts2 = mk("Torrentio RD", "Clashing.Through.The.Snow.2026.2160p.WEB.x265-YTS.mkv", "https://x.invalid/y2", 4_880_000_000);
        let groups = group_releases(&[amen1, amen2, yts1, yts2], None);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].mirrors.len(), 2, "both Amen mirrors kept for failover");
        assert_eq!(groups[1].mirrors.len(), 2, "AIOStreams & Torrentio YTS merged into one group");
    }

    #[test]
    fn bandwidth_budget_fits_connection_and_degrades_gracefully() {
        let mk = |title: &str, url: &str, bytes: u64| source_of("Torrentio", &json!({
            "title": title, "url": url, "behaviorHints": {"videoSize": bytes}
        }));
        // 2-hour movie (7200 s):
        // remux = 54 GB ≈ 60 Mbps; web4k = 11.7 GB ≈ 13 Mbps; web1080 = 3.6 GB ≈ 4 Mbps.
        let remux = mk("Film 2026 2160p HDR REMUX", "https://x.invalid/remux", 54_000_000_000);
        let web4k = mk("Film 2026 2160p HDR10+ WEB-DL H265", "https://x.invalid/web4k", 11_700_000_000);
        let web1080 = mk("Film 2026 1080p WEB-DL H265", "https://x.invalid/web1080", 3_600_000_000);
        let all = || vec![
            VerifiedSource { source: remux.clone(), audio: Some((true, true)), has_original_lang: true },
            VerifiedSource { source: web4k.clone(), audio: Some((true, true)), has_original_lang: true },
            VerifiedSource { source: web1080.clone(), audio: Some((true, true)), has_original_lang: true },
        ];
        let eng = crate::addons::language("english");

        // Fast 150 Mbps fiber (Auto): 60 Mbps 4K Remux fits and wins.
        let fast = rank_verified(all(), eng, None, Some(7200.0), BandwidthBudget::from_settings(None, Some(150.0)), false);
        assert_eq!(fast[0].source.url, remux.url);

        // 30 Mbps tablet Wi-Fi (Auto -> ~17.6 Mbps encode / ~15.5 Mbps remux safe cap): 13 Mbps 4K HDR10+ wins!
        let wifi = rank_verified(all(), eng, None, Some(7200.0), BandwidthBudget::from_settings(None, Some(30.0)), false);
        assert_eq!(wifi[0].source.url, web4k.url);

        // Slow 5 Mbps hotel Wi-Fi (Auto -> 4.0 Mbps floor; only 4 Mbps 1080p fits, or if all over, smallest wins).
        let slow = rank_verified(all(), eng, None, Some(7200.0), BandwidthBudget::from_settings(None, Some(5.0)), false);
        assert_eq!(slow[0].source.url, web1080.url);

        // Manual Unlimited (Some(0)): Remux wins even on 15 Mbps measured speed.
        let unlim = rank_verified(all(), eng, None, Some(7200.0), BandwidthBudget::from_settings(Some(0), Some(15.0)), false);
        assert_eq!(unlim[0].source.url, remux.url);
    }

    #[test]
    fn original_language_fallback_duration_extras_and_wrong_episode() {
        use crate::tracks::{Picture, Transfer};
        let mk = |title: &str, url: &str, bytes: u64| source_of("A", &json!({
            "title": title, "url": url, "behaviorHints": {"filename": title, "videoSize": bytes}
        }));
        // Foreign-original Korean film when no English dub exists: original Korean beats French dub.
        let kor = mk("Parasite.2019.2160p.HDR.WEB-DL.KOR.mkv", "https://x.invalid/kor", 12_000_000_000);
        let fre = mk("Parasite.2019.2160p.HDR.REMUX.FRE.mkv", "https://x.invalid/fre", 55_000_000_000);
        let ranked = rank_verified(
            vec![
                VerifiedSource { source: fre, audio: Some((false, false)), has_original_lang: false },
                VerifiedSource { source: kor.clone(), audio: Some((false, false)), has_original_lang: true },
            ],
            crate::addons::language("english"),
            None,
            Some(7920.0),
            BandwidthBudget::unlimited(),
            false,
        );
        assert_eq!(ranked[0].source.url, kor.url);

        // Localized title matching via `names_title_any`.
        let ita_release = mk("Scontrarsi a Natale (2026) iTA-ENG.WEBDL.1080p.x264.mkv", "https://x.invalid/it", 2_700_000_000);
        assert!(names_title_any(&ita_release, &["Clashing Through the Snow".into(), "Scontrarsi a Natale".into()], Some(2026)));

        // Duration sanity rejects trailers/clips while accepting real cuts.
        let trailer_pic = Picture { width: 1920, height: 1080, transfer: Transfer::Sdr, dolby_vision: None, seconds: Some(150.0) };
        let feature_pic = Picture { width: 3840, height: 2160, transfer: Transfer::Pq, dolby_vision: None, seconds: Some(6200.0) };
        assert!(!duration_plausible(&trailer_pic, Some(6600.0), false));
        assert!(duration_plausible(&feature_pic, Some(6600.0), false));

        // Extra clips rejected unless the title itself contains the word.
        let making = mk("Digger.2026.Making.Of.1080p.WEB.mkv", "https://x.invalid/m", 800_000_000);
        assert!(is_extra_clip(&making, &["Digger".into()]));
        let tpb = mk("Trailer.Park.Boys.S01E01.1080p.WEB.mkv", "https://x.invalid/tpb", 900_000_000);
        assert!(!is_extra_clip(&tpb, &["Trailer Park Boys".into()]));

        // Wrong episode rejected (both SxxEyy and anime bare numbers); matching & multi-episode allowed.
        let ep2 = mk("Show.S01E02.1080p.WEB.mkv", "https://x.invalid/e2", 1_500_000_000);
        let ep12 = mk("Show.S01E01-E02.1080p.WEB.mkv", "https://x.invalid/e12", 3_000_000_000);
        let anime_ep6 = mk("[SickeClipse]_Solo_Leveling_06_[2160p][x265].mp4", "https://x.invalid/a6", 1_400_000_000);
        let anime_ep1 = mk("[Salieri] Solo Leveling - 01 (1080p) (HDR) [Dual Audio].mkv", "https://x.invalid/a1", 1_400_000_000);
        assert!(wrong_episode(&ep2, (1, 1)));
        assert!(!wrong_episode(&ep12, (1, 1)));
        assert!(!wrong_episode(&ep12, (1, 2)));
        assert!(wrong_episode(&anime_ep6, (1, 1)));
        assert!(!wrong_episode(&anime_ep1, (1, 1)));

        // Regression tests: `384kbps` must not match 4K, `S01E01.5.1` is ep 1 (not 1..=5),
        // anime ep 10 is validated, and "Subs: ITA" must not match as Italian audio.
        assert_eq!(find_quality("Movie.2024.1080p.DDP5.1.384kbps.mkv"), Some("1080p"));
        assert_eq!(find_quality("Movie.2024.384kbps.AC3.mkv"), None);
        assert_eq!(find_quality("Movie.2024.360p.WEB.mp4"), Some("SD"));
        let ep1_51 = mk("Show.S01E01.5.1.1080p.WEB.mkv", "https://x.invalid/e151", 1_500_000_000);
        assert!(!wrong_episode(&ep1_51, (1, 1)));
        assert!(wrong_episode(&ep1_51, (1, 3)));
        let anime_ep10 = mk("[SubGroup] Anime - 10 (1080p).mkv", "https://x.invalid/a10", 1_200_000_000);
        assert!(wrong_episode(&anime_ep10, (1, 1)));
        assert!(!wrong_episode(&anime_ep10, (1, 10)));
        assert!(track_in_language("es_419", "latino"));
        let ita = crate::addons::language("italian").unwrap();
        assert!(!ita.matches("Movie.2024.1080p.ENG | Subs: ITA FRE SPA"));
        assert!(ita.matches("Movie.2024.1080p.ITA.ENG | Subs: ENG"));
    }
}

#[cfg(test)]
mod audit {
    use super::*;

    /// Developer audit, nothing played or resolved: Play's order on saved source lists
    /// (VEYLO_AUDIT: {"movie/tt…": {"name", "year", "kind", "streams": [add-on streams]}}),
    /// printing each title's first three. `cargo test ranking_audit -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ranking_audit() {
        let path = std::env::var("VEYLO_AUDIT").expect("VEYLO_AUDIT: a saved list of titles and their streams");
        let all: serde_json::Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for (key, t) in &all {
            let (name, year, series) = (s(t, "name"), t["year"].as_i64().map(|y| y as i32), t["kind"] == "series");
            let mut list: Vec<Source> = t["streams"].as_array().into_iter().flatten().map(|v| source_of("Torrentio", v)).collect();
            for src in &mut list { src.off_title = !names_title(src, &name, if series { None } else { year }); }
            let order = play_order_for(&list, series.then_some(10), Some("english"));
            println!("== {name} ({key}): {} sources, {} playable", list.len(), order.len());
            for src in order.iter().take(3) {
                let release = src.description.lines().next().unwrap_or(&src.filename).chars().take(78).collect::<String>();
                let ranked = episode_size(src, series.then_some(10)).unwrap_or(0);
                println!("   {:5} {:6} {:>10} (ranked as {:.2} GB, class {}) {}{}", src.quality.unwrap_or("?"), src.hdr.unwrap_or("SDR"), src.size.as_deref().unwrap_or("?"),
                    ranked as f64 / 1e9, picture_class(src, ranked), if src.off_title { "[off-title] " } else { "" }, release);
            }
        }
    }
}
