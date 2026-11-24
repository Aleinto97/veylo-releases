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

/// The intro, recap and credits the online databases know for an episode: IntroDB for IMDb
/// episodes, AniSkip for anime (a Kitsu id is turned into its MyAnimeList id first). Only public
/// ids are sent. Nothing found, or a service down, is an empty list: the player just offers no skip.
pub async fn online_segments(video_id: &str, duration: f64) -> Vec<crate::engine::Segment> {
    let Some(lookup) = skip_lookup(video_id) else { return Vec::new() };
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
    match found { Ok(Ok(list)) => list, _ => Vec::new() }
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
    let mut list = v["subtitles"].as_array().cloned().unwrap_or_default();
    if list.len() < 5 && video_id.starts_with("tt") {
        if let Ok(os_list) = opensubtitles(kind, video_id).await {
            list.extend(os_list);
        } else if list.is_empty() {
            anyhow::bail!("Subtitle providers could not be reached");
        }
    }
    if list.is_empty() && v["errors"].as_array().is_some_and(|e| !e.is_empty()) {
        anyhow::bail!("Subtitle providers could not be reached");
    }
    let preferred = stream_language().map(|k| subtitle_language_of(&k));
    Ok(rank_subtitles_for(list.iter().filter_map(subtitle_of).collect(), preferred.as_deref(), filename))
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

fn release_tokens(name: &str) -> Vec<String> {
    let stem = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = stem.rsplit_once('.').map(|(a, _)| a).unwrap_or(stem);
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
        Some(s.language.as_str()) != preferred,
        s.language != "English",
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

/// Download one subtitle into the cache and return the local file for the player; a file the
/// server says not to keep (or one Veylo can't read, like ASS) is handed to the player as a link.
pub async fn fetch_subtitle(url: &str) -> String {
    // Plain http needs consent the core doesn't have for subtitle links: such a link isn't
    // cached, and goes to the player as offered.
    match exec(json!({"op":"subtitle_file","url":url})).await {
        Ok(v) => v["cache_path"].as_str().map(str::to_owned).unwrap_or_else(|| url.to_owned()),
        Err(_) => url.to_owned(),
    }
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
}
