//! Online subtitles (the add-ons', then OpenSubtitles) and intro/recap/credits times (IntroDB, AniSkip).
use super::*;

// ---------------------------------------------------------------- subtitles

/// OpenSubtitles' public add-on, asked when no installed add-on has subtitles for a video.
pub(super) const OPENSUBTITLES: &str = "https://opensubtitles-v3.strem.io";
pub(super) const INTRODB: &str = "https://api.introdb.app/segments";
pub(super) const ANISKIP: &str = "https://api.aniskip.com/v2/skip-times";
pub(super) const KITSU_API: &str = "https://kitsu.app/api/edge/anime";

/// Where an episode's intro times can be looked up, from its video id.
#[derive(Debug, PartialEq)]
pub enum SkipLookup { IntroDb { imdb: String, season: u32, episode: u32 }, Kitsu { id: u64, episode: u32 }, Mal { id: u64, episode: u32 } }

/// "tt0903747:1:2" (IntroDB), "kitsu:1376:5" and "mal:5114:3" (AniSkip); None for films and the rest.
pub fn skip_lookup(video_id: &str) -> Option<SkipLookup> {
    let parts: Vec<&str> = video_id.split(':').collect();
    match parts.as_slice() {
        [imdb, s, e] if imdb.starts_with("tt") && imdb[2..].chars().all(|c| c.is_ascii_digit()) && imdb.len() > 2 =>
            Some(SkipLookup::IntroDb { imdb: (*imdb).to_owned(), season: s.parse().ok()?, episode: e.parse().ok()? }),
        ["kitsu", id, e] => Some(SkipLookup::Kitsu { id: id.parse().ok()?, episode: e.parse().ok()? }),
        ["mal", id, e] => Some(SkipLookup::Mal { id: id.parse().ok()?, episode: e.parse().ok()? }),
        _ => None,
    }
}

static SEGMENTS_CACHE: OnceLock<Mutex<HashMap<String, Vec<crate::engine::Segment>>>> = OnceLock::new();

/// The intro, recap and credits the online databases know for an episode: IntroDB for IMDb
/// episodes, AniSkip for anime (a Kitsu id is turned into its MyAnimeList id first). Only public
/// ids are sent. Returns `Some(list)` on a definitive answer (or non-episode) and `None` on a
/// transient network/HTTP error so the caller can retry once.
pub async fn online_segments(video_id: &str, duration: f64) -> Option<Vec<crate::engine::Segment>> {
    let Some(lookup) = skip_lookup(video_id) else { return Some(Vec::new()) };
    let cache_key = format!("{video_id}|{}", (duration.max(0.0) / 5.0).round() as u64);
    if let Ok(map) = SEGMENTS_CACHE.get_or_init(Default::default).lock() {
        if let Some(hit) = map.get(&cache_key) { return Some(hit.clone()); }
    }
    let found = on_runtime(async move {
        let http = backend_http();
        let get = |url: url::Url| { let http = http.clone(); async move {
            let r = http.get(url).send().await?;
            // AniSkip answers 404 when it has nothing; that is an answer, not a failure.
            if r.status() == reqwest::StatusCode::NOT_FOUND { return Ok::<Value, anyhow::Error>(Value::Null); }
            Ok(r.error_for_status()?.json::<Value>().await?)
        } };
        let aniskip = |mal: u64, episode: u32| async move {
            let mut url = url::Url::parse(ANISKIP)?;
            url.path_segments_mut().map_err(|_| anyhow!("bad url"))?.extend([mal.to_string(), episode.to_string()]);
            {
                let mut q = url.query_pairs_mut();
                for t in ["op", "ed", "mixed-op", "mixed-ed", "recap"] { q.append_pair("types", t); }
                q.append_pair("episodeLength", &format!("{:.3}", duration.max(0.0)));
            }
            Ok::<_, anyhow::Error>(aniskip_segments(&get(url).await?, duration))
        };
        match lookup {
            SkipLookup::IntroDb { imdb, season, episode } => {
                let mut url = url::Url::parse(INTRODB)?;
                url.query_pairs_mut().append_pair("imdb_id", &imdb).append_pair("season", &season.to_string()).append_pair("episode", &episode.to_string());
                Ok(introdb_segments(&get(url).await?, duration))
            }
            SkipLookup::Mal { id, episode } => aniskip(id, episode).await,
            SkipLookup::Kitsu { id, episode } => {
                let mut url = url::Url::parse(KITSU_API)?;
                url.path_segments_mut().map_err(|_| anyhow!("bad url"))?.extend([id.to_string(), "mappings".into()]);
                url.query_pairs_mut().append_pair("filter[externalSite]", "myanimelist/anime");
                match mal_of(&get(url).await?) { Some(mal) => aniskip(mal, episode).await, None => Ok(Vec::new()) }
            }
        }
    }).await;
    match found {
        Ok(Ok(list)) => {
            if let Ok(mut map) = SEGMENTS_CACHE.get_or_init(Default::default).lock() {
                if map.len() >= 256 { map.clear(); }
                map.insert(cache_key, list.clone());
            }
            Some(list)
        }
        _ => None,
    }
}

/// The MyAnimeList id in a Kitsu mappings answer.
pub fn mal_of(v: &Value) -> Option<u64> {
    v["data"].as_array()?.iter().find(|m| m["attributes"]["externalSite"] == "myanimelist/anime")
        .and_then(|m| m["attributes"]["externalId"].as_str()?.parse().ok())
}

/// A segment from a database, kept only when it fits inside this video (an entry for another cut
/// of the episode can run past its end).
pub(super) fn online_segment(kind: crate::engine::SegmentKind, start: f64, end: f64, duration: f64) -> Option<crate::engine::Segment> {
    let end = if duration > 0.0 && end > duration { if end - duration <= 5.0 { duration } else { return None } } else { end };
    (start >= 0.0 && end - start >= 3.0).then_some(crate::engine::Segment { kind, start, end })
}

pub fn introdb_segments(v: &Value, duration: f64) -> Vec<crate::engine::Segment> {
    use crate::engine::SegmentKind::*;
    [("recap", Recap), ("intro", Intro), ("outro", Credits)].into_iter().filter_map(|(key, kind)| {
        let g = &v[key];
        let start = g["start_ms"].as_f64().map(|ms| ms / 1000.0).or_else(|| g["start_sec"].as_f64())?;
        // Credits to the end can be stored without an end.
        let end = g["end_ms"].as_f64().map(|ms| ms / 1000.0).or_else(|| g["end_sec"].as_f64())
            .or((kind == Credits && duration > 0.0).then_some(duration))?;
        online_segment(kind, start, end, duration)
    }).collect()
}

pub fn aniskip_segments(v: &Value, duration: f64) -> Vec<crate::engine::Segment> {
    use crate::engine::SegmentKind::*;
    let mut out: Vec<crate::engine::Segment> = v["results"].as_array().into_iter().flatten().filter_map(|r| {
        let kind = match r["skipType"].as_str()? { "op" | "mixed-op" => Intro, "ed" | "mixed-ed" => Credits, "recap" => Recap, _ => return None };
        online_segment(kind, r["interval"]["startTime"].as_f64()?, r["interval"]["endTime"].as_f64()?, duration)
    }).collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// A subtitle file an add-on offers for the playing video.
#[derive(Clone, Debug, PartialEq)]
pub struct OnlineSubtitle {
    pub id: String,
    pub url: String,
    /// The add-on's language code, verbatim ("ita", "pob").
    pub code: String,
    /// The language in English ("Italian"), or the code when it is not known.
    pub language: String,
    /// The file or release it was made for, as the add-on names it (helps pick one in sync).
    pub release: Option<String>,
}

/// Every subtitle the add-ons offer for one video: the preferred language first, then English,
/// then the rest by name; within a language, releases matching the playing file rank first.
pub async fn online_subtitles(kind: &str, video_id: &str, filename: Option<&str>, file_size: Option<u64>) -> Result<Vec<OnlineSubtitle>> {
    let mut extra = serde_json::Map::new();
    if let Some(f) = filename.map(str::trim).filter(|f| !f.is_empty()) {
        extra.insert("filename".into(), json!(f));
    }
    if let Some(sz) = file_size.filter(|&s| s > 0) {
        extra.insert("videoSize".into(), json!(sz.to_string()));
    }
    let req = if extra.is_empty() {
        json!({"op":"subtitles","type":kind,"id":video_id})
    } else {
        json!({"op":"subtitles","type":kind,"id":video_id,"extra":extra})
    };
    let v = exec(req).await?;
    let raw_list = v["subtitles"].as_array().cloned().unwrap_or_default();
    let preferred = preferred_subtitle_language(kind, video_id);
    let mut parsed = rank_subtitles_for(raw_list.iter().filter_map(subtitle_of).collect(), preferred.as_deref(), filename);
    let want_lang = preferred.as_deref().unwrap_or("English");
    let usable_in_lang = parsed.iter().filter(|s| same_language(&s.language, want_lang)).count();
    if usable_in_lang < 3 && valid_imdb(video_id.split(':').next().unwrap_or("")) {
        if let Ok(os_list) = opensubtitles(kind, video_id).await {
            parsed.extend(os_list.iter().filter_map(subtitle_of));
            parsed = rank_subtitles_for(parsed, preferred.as_deref(), filename);
        } else if parsed.is_empty() {
            anyhow::bail!("Subtitle providers could not be reached");
        }
    }
    if parsed.is_empty() && v["errors"].as_array().is_some_and(|e| !e.is_empty()) {
        anyhow::bail!("Subtitle providers could not be reached");
    }
    Ok(parsed)
}

/// The subtitle language to rank first for a video: the one remembered for this title (what the
/// viewer last picked in the player), else the Audio-language setting's.
fn preferred_subtitle_language(kind: &str, video_id: &str) -> Option<String> {
    // A series' video id is "<title id>:<season>:<episode>" ("kitsu:<id>:<episode>" for anime).
    let parts: Vec<&str> = video_id.split(':').collect();
    let title_id = match parts.first() {
        Some(&("kitsu" | "mal" | "anilist" | "anidb")) if parts.len() >= 2 => parts[..2].join(":"),
        _ => parts.first().map(|p| (*p).to_owned()).unwrap_or_default(),
    };
    let remembered = load_track_pref_for_title(kind, &title_id)
        .filter(|p| !p.sub_off)
        .and_then(|p| p.sub_lang)
        .map(|code| crate::engine::language_name(&code))
        .filter(|l| !l.is_empty());
    remembered.or_else(|| stream_language().map(|k| subtitle_language_of(&k)))
}

/// "Portuguese (Brazil)" is "Portuguese": the base language name, lower case, without a region.
fn language_base(label: &str) -> String {
    label.split('(').next().unwrap_or(label).trim().to_lowercase()
}

/// Whether a subtitle's language label is the wanted one. A wanted language without a region
/// ("Portuguese") takes its regional variants ("Portuguese (Brazil)"); one with a region
/// ("Spanish (Latin America)") takes only that.
fn same_language(label: &str, wanted: &str) -> bool {
    if label.trim().eq_ignore_ascii_case(wanted.trim()) { return true; }
    !wanted.contains('(') && language_base(label) == language_base(wanted)
}

pub(super) async fn opensubtitles(kind: &str, video_id: &str) -> Result<Vec<Value>> {
    let mut url = url::Url::parse(OPENSUBTITLES)?;
    url.path_segments_mut().map_err(|_| anyhow!("Invalid subtitle add-on"))?.extend(["subtitles", kind, &format!("{video_id}.json")]);
    let body: Value = on_runtime(async move {
        backend_http().get(url).send().await?.error_for_status()?.json::<Value>().await
    }).await?.context("Subtitle add-on unavailable")?;
    Ok(body["subtitles"].as_array().cloned().unwrap_or_default())
}

/// The subtitle-list name of an audio-language setting (`crate::addons::LANGUAGES` key): the two
/// lists name a few languages differently.
pub fn subtitle_language_of(key: &str) -> String {
    match key {
        "latino" => "Spanish (Latin America)".into(),
        other => crate::addons::language(other).map(|l| l.label.to_owned()).unwrap_or_default(),
    }
}

pub(super) fn subtitle_of(v: &Value) -> Option<OnlineSubtitle> {
    let url = v["url"].as_str()?.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) { return None; }
    let code = v["lang"].as_str().unwrap_or("").trim().to_owned();
    let language = Some(crate::engine::language_name(&code)).filter(|l| !l.is_empty()).unwrap_or_else(|| "Unknown".into());
    let id = v["id"].as_str().map(str::to_owned).or_else(|| v["id"].as_i64().map(|n| n.to_string())).unwrap_or_else(|| url.to_owned());
    let release = ["subtitleFileName", "movieReleaseName"].iter().find_map(|k| v[*k].as_str().map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned));
    Some(OnlineSubtitle { id, url: url.to_owned(), code, language, release })
}

#[cfg(test)]
pub fn rank_subtitles(list: Vec<OnlineSubtitle>, preferred: Option<&str>) -> Vec<OnlineSubtitle> {
    rank_subtitles_for(list, preferred, None)
}

/// File extensions a release name may end with (never part of the tags).
const RELEASE_EXTENSIONS: [&str; 16] = ["mkv", "mp4", "m4v", "avi", "mov", "wmv", "webm", "ts", "m2ts", "mpg", "mpeg", "flv", "srt", "ass", "vtt", "sub"];

fn release_tokens(name: &str) -> Vec<String> {
    static_re!(web_dl_re, r"(?i)\bweb[ ._-]?dl\b");
    static_re!(web_rip_re, r"(?i)\bweb[ ._-]?rip\b");
    static_re!(blu_ray_re, r"(?i)\bblu[ ._-]?ray\b");
    static_re!(bd_rip_re, r"(?i)\bbd[ ._-]?rip\b");
    let stem = name.rsplit(['/', '\\']).next().unwrap_or(name);
    // Only a known video or subtitle extension is cut: "Show.S01E01.DDP5.1-NTb" has none, and
    // its ".1-NTb" is the audio layout and the release group.
    let stem = match stem.rsplit_once('.') {
        Some((a, ext)) if RELEASE_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)) => a,
        _ => stem,
    };
    // "WEB-DL" is one tag, not "web" and "dl" (and likewise WEBRip, Blu-ray, BDRip).
    let stem = web_dl_re().replace_all(stem, "webdl");
    let stem = web_rip_re().replace_all(&stem, "webrip");
    let stem = blu_ray_re().replace_all(&stem, "bluray");
    let stem = bd_rip_re().replace_all(&stem, "bdrip");
    stem.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_owned)
        .collect()
}

/// Higher score = stronger release-tag overlap between the playing video filename and subtitle release.
pub fn release_match_score(sub_release: Option<&str>, playing_filename: Option<&str>) -> u32 {
    let (Some(sub), Some(play)) = (sub_release, playing_filename) else { return 0 };
    let pt = release_tokens(play);
    let st = release_tokens(sub);
    if pt.is_empty() || st.is_empty() { return 0; }
    let mut score = 0u32;
    // Exact release group match (last token) is the strongest indicator of frame-accurate sync.
    if pt.last() == st.last() { score += 5; }
    for tok in &st {
        if pt.contains(tok) {
            score += match tok.as_str() {
                "bluray" | "bdrip" | "remux" | "webdl" | "webrip" | "web" | "hdtv" | "imax" | "extended" | "unrated" => 3,
                _ => 1,
            };
        }
    }
    score
}

pub fn rank_subtitles_for(mut list: Vec<OnlineSubtitle>, preferred: Option<&str>, playing_filename: Option<&str>) -> Vec<OnlineSubtitle> {
    let mut seen = HashSet::new();
    list.retain(|s| seen.insert(s.url.clone()));
    // A stable sort keeps the add-on's order for equal language and release-match scores.
    list.sort_by_key(|s| (
        !preferred.is_some_and(|p| same_language(&s.language, p)),
        !same_language(&s.language, "English"),
        s.language == "Unknown",
        s.language.clone(),
        std::cmp::Reverse(release_match_score(s.release.as_deref(), playing_filename)),
    ));
    list
}

/// Subtitles whose language name or code starts a word with the query ("ita", "port", "pob").
pub fn subtitle_matches(s: &OnlineSubtitle, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() { return true; }
    s.code.to_lowercase() == q || s.language.to_lowercase().split([' ', '(', ')', ',']).any(|w| w.starts_with(&q)) || s.language.to_lowercase().starts_with(&q)
}

fn format_vtt_time(ms: i64) -> String {
    let ms = ms.max(0);
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let frac = ms % 1000;
    format!("{h:02}:{m:02}:{s:02}.{frac:03}")
}

static STAGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// A staged (`no-store`) subtitle file is only needed while the player has it open.
const STAGED_SUBTITLE_TTL: Duration = Duration::from_secs(6 * 3600);

/// A private staging path unique to this request (the subtitle's URL and a counter): two videos,
/// or two requests for one, never overwrite the file the player is reading. Staged files from
/// earlier requests (and the old fixed `sub-ephemeral.*` names) are swept first.
fn staged_subtitle_path(url: &str, ext: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let dir = backend_data_dir();
    sweep_staged_subtitles(&dir, STAGED_SUBTITLE_TTL);
    let tag: String = Sha256::digest(url.as_bytes()).iter().take(5).map(|b| format!("{b:02x}")).collect();
    let seq = STAGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.join(format!("sub-ephemeral-{tag}-{:x}-{seq:x}.{ext}", std::process::id()))
}

fn sweep_staged_subtitles(dir: &std::path::Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("sub-ephemeral") || !(name.ends_with(".ass") || name.ends_with(".vtt")) { continue; }
        let stale = entry.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).is_none_or(|age| age > max_age);
        // The legacy fixed names are never in use by a player of this version.
        if stale || name == "sub-ephemeral.ass" || name == "sub-ephemeral.vtt" { let _ = std::fs::remove_file(entry.path()); }
    }
}

/// One cue's text made safe inside a WebVTT block: a blank line would end the cue early and
/// `-->` would read as a timing line.
pub(super) fn sanitize_cue_text(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<String> = unified.lines()
        .map(|l| l.replace("-->", "->").trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    lines.join("\n")
}

/// Download and validate one subtitle through core and return its local file path for the player.
/// Never falls back to handing an unvalidated remote URL directly to libmpv.
pub async fn fetch_subtitle(url: &str) -> Result<String> {
    let v = exec(json!({"op":"subtitle_file","url":url})).await?;
    if let Some(path) = v["cache_path"].as_str().filter(|p| !p.is_empty()) {
        return Ok(path.to_owned());
    }
    // Valid `Cache-Control: no-store` ASS subtitle: stage the core-validated ASS bytes ephemerally.
    if let Some(ass) = v["raw_ass"].as_str().filter(|s| !s.is_empty()) {
        let path = staged_subtitle_path(url, "ass");
        write_private_file(&path, ass.as_bytes()).context("Couldn't stage subtitle")?;
        return Ok(path.to_string_lossy().into_owned());
    }
    // Valid `Cache-Control: no-store` SRT/VTT subtitle: render the core-validated cues into an ephemeral
    // private VTT file rather than bypassing core validation with the raw remote URL.
    let cues = v["cues"].as_array().filter(|c| !c.is_empty())
        .context("Subtitle could not be cached or read")?;
    let vtt = cues_to_vtt(cues);
    anyhow::ensure!(vtt.len() > "WEBVTT\n\n".len(), "Subtitle could not be cached or read");
    let path = staged_subtitle_path(url, "vtt");
    write_private_file(&path, vtt.as_bytes()).context("Couldn't stage subtitle")?;
    Ok(path.to_string_lossy().into_owned())
}

fn cues_to_vtt(cues: &[Value]) -> String {
    let mut vtt = String::from("WEBVTT\n\n");
    for cue in cues {
        let (Some(start), Some(end), Some(text)) = (cue["start_ms"].as_i64(), cue["end_ms"].as_i64(), cue["text"].as_str()) else { continue };
        let text = sanitize_cue_text(text);
        if text.is_empty() || end <= start { continue; }
        vtt.push_str(&format!("{} --> {}\n{}\n\n", format_vtt_time(start), format_vtt_time(end), text));
    }
    vtt
}

#[cfg(test)]
mod release_sync_tests {
    use super::*;
    #[test]
    fn release_matching_subtitles_rank_first_within_language() {
        let subs = vec![
            OnlineSubtitle { id: "1".into(), url: "https://a/1.srt".into(), code: "eng".into(), language: "English".into(), release: Some("Movie.2024.HDTV.x264-OTHER".into()) },
            OnlineSubtitle { id: "2".into(), url: "https://a/2.srt".into(), code: "eng".into(), language: "English".into(), release: Some("Movie.2024.2160p.WEB-DL.DDP5.1-NTb".into()) },
        ];
        let ranked = rank_subtitles_for(subs, Some("English"), Some("Movie.2024.2160p.WEB-DL.H265-NTb.mkv"));
        assert_eq!(ranked[0].id, "2");
    }

    #[test]
    fn release_tokens_keep_group_and_join_web_dl() {
        assert_eq!(release_tokens("Show.S01E01.DDP5.1-NTb").last().map(String::as_str), Some("ntb"));
        assert_eq!(release_tokens("Show.S01E01.1080p.WEB-DL.H264-NTb.mkv").last().map(String::as_str), Some("ntb"));
        assert!(release_tokens("Movie.2024.WEB-DL.x264.mkv").contains(&"webdl".to_owned()));
        assert!(release_tokens("Movie 2024 Blu-Ray x264.mkv").contains(&"bluray".to_owned()));
        // WEB-DL in the subtitle's release matches WEBDL in the file name.
        assert!(release_match_score(Some("Movie.2024.WEB-DL-GRP"), Some("Movie.2024.WEBDL-GRP.mkv")) >= 8);
    }

    #[test]
    fn regional_variants_match_the_base_language() {
        let sub = |id: &str, lang: &str| OnlineSubtitle { id: id.into(), url: format!("https://a/{id}.srt"), code: "x".into(), language: lang.into(), release: None };
        assert!(same_language("Portuguese (Brazil)", "Portuguese"));
        assert!(same_language("Portuguese", "Portuguese"));
        assert!(!same_language("Spanish", "Spanish (Latin America)"));
        assert!(!same_language("Portuguese", "Spanish"));
        let ranked = rank_subtitles_for(vec![sub("1", "English"), sub("2", "Portuguese (Brazil)"), sub("3", "French")], Some("Portuguese"), None);
        assert_eq!(ranked[0].id, "2");
        assert_eq!(ranked[1].id, "1");
    }

    #[test]
    fn cue_text_cannot_break_out_of_its_vtt_block() {
        assert_eq!(sanitize_cue_text("one\n\ntwo --> three\r\n  "), "one\ntwo -> three");
        let vtt = cues_to_vtt(&[
            json!({"start_ms": 1000, "end_ms": 2000, "text": "a\n\nb"}),
            json!({"start_ms": 3000, "end_ms": 3500, "text": "\n\n"}),
            json!({"start_ms": 4000, "end_ms": 3000, "text": "backwards"}),
        ]);
        assert_eq!(vtt, "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\na\nb\n\n");
    }

    #[test]
    fn staging_sweeps_only_old_staged_subtitles() {
        let dir = std::env::temp_dir().join(format!("veylo-sub-stage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["sub-ephemeral.ass", "sub-ephemeral-ab-1-1.vtt", "settings.json"] { std::fs::write(dir.join(name), b"x").unwrap(); }
        sweep_staged_subtitles(&dir, Duration::from_secs(3600));
        // The legacy fixed name goes at once; a fresh unique one stays; other files are never touched.
        assert!(!dir.join("sub-ephemeral.ass").exists());
        assert!(dir.join("sub-ephemeral-ab-1-1.vtt").exists());
        assert!(dir.join("settings.json").exists());
        std::thread::sleep(Duration::from_millis(20));
        sweep_staged_subtitles(&dir, Duration::from_millis(1));
        assert!(!dir.join("sub-ephemeral-ab-1-1.vtt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
