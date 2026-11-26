//! What Picks offers, decided the same way for every source: one genre vocabulary, formats and
//! kids' titles left out, animation as a style beside the type, series judged by the years they
//! aired, and one entry per title however many sources list it.
use crate::backend::Media;
use std::collections::{HashMap, HashSet};

/// Live action, animation, or both: the Style switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style { Live, Animation, Both }

impl Style {
    pub fn as_str(self) -> &'static str { match self { Style::Live => "live", Style::Animation => "animation", Style::Both => "both" } }
    pub fn parse(s: &str) -> Style { match s { "animation" => Style::Animation, "both" => Style::Both, _ => Style::Live } }
}

/// The app's name for a genre, whichever source spelled it. A TMDB pair ("Sci-Fi & Fantasy")
/// stays a pair: which half applies is not known, so neither is claimed.
pub fn canonical_genre(g: &str) -> String {
    let one = |g: &str| -> String {
        match g.trim().to_lowercase().as_str() {
            "science fiction" | "science-fiction" | "sci-fi" | "scifi" | "sci fi" => "Sci-Fi".into(),
            "children" | "kids" => "Kids".into(),
            "reality-tv" | "reality tv" | "reality" => "Reality".into(),
            "talk-show" | "talk show" | "talk" => "Talk".into(),
            "game-show" | "game show" => "Game Show".into(),
            "tv movie" => "TV Movie".into(),
            "musical" => "Musical".into(),
            "war & politics" => "War & Politics".into(),
            other => {
                let mut c = other.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        }
    };
    if g.contains(" & ") && !g.eq_ignore_ascii_case("war & politics") {
        g.split(" & ").map(one).collect::<Vec<_>>().join(" & ")
    } else {
        one(g)
    }
}

/// The single genres a title offers to the Genre list (pairs split for choosing, never for tagging).
fn halves(g: &str) -> Vec<&str> { if g == "War & Politics" { vec!["War", "Politics"] } else { g.split(" & ").collect() } }

/// Formats, not genres: never offered by Picks.
const FORMATS: [&str; 5] = ["Reality", "Talk", "News", "Soap", "Game Show"];
/// Style, not genre: handled by the Style switch.
const STYLES: [&str; 2] = ["Animation", "Anime"];

pub fn is_format(m: &Media) -> bool { m.genres.iter().any(|g| FORMATS.contains(&g.as_str())) }
pub fn is_kids(m: &Media) -> bool { m.genres.iter().any(|g| g == "Kids") }
/// Set once TMDB has said whether a title is animated: catalogues sometimes tag live-action
/// films "Animation" (Cinemeta lists The Last Viking, 2025, so), and TMDB's answer then wins.
const SETTLED: &str = "animated";

pub fn is_animated(m: &Media) -> bool {
    if let Some(settled) = m.raw[SETTLED].as_bool() { return settled; }
    m.kind == "anime" || m.genres.iter().any(|g| STYLES.iter().any(|s| g.eq_ignore_ascii_case(s)))
        || (m.raw["tmdb_id"].as_i64().is_some() && m.raw["genre_ids"].as_array().is_some_and(|a| a.iter().any(|g| g.as_i64() == Some(16))))
        || m.raw["show_type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("animation") || t.eq_ignore_ascii_case("anime"))
}
pub fn style_matches(m: &Media, style: Style) -> bool {
    match style { Style::Live => !is_animated(m), Style::Animation => is_animated(m), Style::Both => true }
}

/// Whether a title carries a genre; a pair carries both its halves.
pub fn has_genre(m: &Media, wanted: &str) -> bool {
    let wanted = canonical_genre(wanted);
    m.genres.iter().any(|g| g.eq_ignore_ascii_case(&wanted) || halves(g).iter().any(|h| h.eq_ignore_ascii_case(&wanted)))
}

/// Genres to choose from, for the Genre list: single names, no styles or formats.
pub fn genre_choices(m: &Media) -> Vec<String> {
    m.genres.iter().flat_map(|g| halves(g).into_iter().map(str::to_owned).collect::<Vec<_>>())
        .filter(|g| !STYLES.contains(&g.as_str()) && !FORMATS.contains(&g.as_str())).collect()
}

/// The years a title was out: a film's year; a series from its first to its last season
/// ("2016–" still running counts to this year).
pub fn year_range(m: &Media, this_year: i32) -> Option<(i32, i32)> {
    let years: Vec<i32> = m.year.split(|c: char| !c.is_ascii_digit()).filter(|p| p.len() == 4).filter_map(|p| p.parse().ok()).collect();
    let first = *years.first()?;
    if m.kind == "movie" { return Some((first, first)); }
    let open = m.year.trim_end().ends_with(['–', '-']);
    let last = if open { this_year } else { years.get(1).copied().unwrap_or(first) };
    Some((first, last.max(first)))
}

/// The same title from another source (Kitsu and Cinemeta for one anime): name and year.
pub fn title_key(m: &Media) -> String {
    let name: String = m.name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    format!("{name}|{}", m.first_year().unwrap_or(0))
}

/// Genres in the app's names, ready for Picks.
pub fn tidy(mut m: Media) -> Media {
    let mut seen = HashSet::new();
    m.genres = m.genres.iter().map(|g| canonical_genre(g)).filter(|g| !g.is_empty() && seen.insert(g.clone())).collect();
    m
}

/// A title's IMDb id, whichever source listed it.
pub fn imdb(m: &Media) -> Option<String> {
    Some(m.id.as_str()).filter(|i| i.starts_with("tt"))
        .or_else(|| m.raw["imdb_id"].as_str().filter(|i| i.starts_with("tt")))
        .map(|i| i.split(':').next().unwrap_or(i).to_owned())
}

/// Record TMDB's word on whether a title is animated, and make its genres agree.
pub fn settle_style(m: &mut Media, animated: bool) {
    let style = |g: &String| STYLES.iter().any(|s| g.eq_ignore_ascii_case(s));
    if !animated { m.genres.retain(|g| !style(g)); } else if !m.genres.iter().any(style) { m.genres.push("Animation".into()); }
    m.raw[SETTLED] = serde_json::json!(animated);
    m.raw["genres"] = serde_json::json!(m.genres);
}

/// Fill what a title is still missing from another source's listing of it: a synopsis, artwork
/// and ratings. Anything already present is kept as it is.
fn fill_missing(existing: &mut Media, later: &Media) {
    if existing.description.trim().is_empty() && !later.description.trim().is_empty() { existing.description = later.description.clone(); }
    if existing.poster.is_empty() && !later.poster.is_empty() { existing.poster = later.poster.clone(); }
    if existing.background.is_empty() && !later.background.is_empty() { existing.background = later.background.clone(); }
    for k in RATINGS {
        let present = |m: &Media| m.raw.get(k).is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.trim().is_empty()));
        if !present(existing) && present(later) { existing.raw[k] = later.raw[k].clone(); }
    }
}
/// Rating fields the sources fill in (read by the brain's features).
const RATINGS: [&str; 3] = ["imdbRating", "tmdbRating", "tvmazeRating"];

/// Add titles to the pool once each: same entry, same IMDb id or same title+kind from another source.
/// Lookups go through indexes built once, so a big pool is not rescanned (and its titles not
/// re-normalised) for every title added.
pub fn merge(pool: &mut Vec<Media>, more: Vec<Media>) {
    let is_movie = |m: &Media| m.kind == "movie";
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut by_imdb: HashMap<String, usize> = HashMap::new();
    // Titles by (movie or not, name and year): every entry, in pool order.
    let mut by_title: HashMap<(bool, String), Vec<usize>> = HashMap::new();
    let mut imdbs: Vec<Option<String>> = Vec::with_capacity(pool.len() + more.len());
    for (i, old) in pool.iter().enumerate() {
        by_key.entry(old.key()).or_insert(i);
        let id = imdb(old);
        if let Some(id) = &id { by_imdb.entry(id.clone()).or_insert(i); }
        by_title.entry((is_movie(old), title_key(old))).or_default().push(i);
        imdbs.push(id);
    }
    for m in more.into_iter().map(tidy) {
        let title = (is_movie(&m), title_key(&m));
        let id = imdb(&m);
        // The first pool entry that matches, whichever way: same entry; both with an IMDb id and
        // the same one; or, unless both have one, the same kind and title.
        let mut found = by_key.get(&m.key()).copied();
        let mut consider = |i: usize| found = Some(found.map_or(i, |f| f.min(i)));
        if let Some(i) = id.as_ref().and_then(|id| by_imdb.get(id)) { consider(*i); }
        if let Some(list) = by_title.get(&title) {
            if let Some(i) = list.iter().copied().find(|i| id.is_none() || imdbs[*i].is_none()) { consider(i); }
        }
        let Some(i) = found else {
            let i = pool.len();
            by_key.entry(m.key()).or_insert(i);
            if let Some(id) = &id { by_imdb.entry(id.clone()).or_insert(i); }
            by_title.entry(title).or_default().push(i);
            imdbs.push(id);
            pool.push(m);
            continue;
        };
        let existing = &mut pool[i];
        // A later source can supply a missing style. Deduplication must not throw it away,
        // unless TMDB already settled it (then only TMDB's word counts).
        let settled = m.raw[SETTLED].as_bool().or_else(|| existing.raw[SETTLED].as_bool());
        if settled.is_none() && is_animated(&m) && !is_animated(existing) { existing.genres.push("Animation".into()); }
        fill_missing(existing, &m);
        for genre in &m.genres { if !existing.genres.contains(genre) { existing.genres.push(genre.clone()); } }
        existing.raw["genres"] = serde_json::json!(existing.genres);
        if let Some(animated) = settled { settle_style(existing, animated); }
        // An entry without an IMDb id gains the one a later source knows.
        if imdbs[i].is_none() {
            if let Some(id) = id { by_imdb.entry(id.clone()).or_insert(i); imdbs[i] = Some(id); }
        }
    }
}

/// Narrow the pool without reordering it. Formats never pass; kids' titles only when Kids is
/// among the chosen genres; unknown years never match a bound; a series matches if it aired in
/// the range; multiple genres combine with AND (a title must carry every chosen genre).
pub fn filter_picks(items: &[Media], kind: Option<&str>, style: Style, genres: &[String], from: Option<i32>, to: Option<i32>, this_year: i32) -> Vec<Media> {
    let wants_kids = genres.iter().any(|g| canonical_genre(g) == "Kids");
    items.iter().filter(|m| {
        let kind_ok = kind.is_none_or(|k| if k == "series" { m.kind == "series" || m.kind == "anime" } else { m.kind == k });
        let style_ok = style_matches(m, style);
        let years_ok = if from.is_none() && to.is_none() { true } else {
            year_range(m, this_year).is_some_and(|(a, b)| from.is_none_or(|f| b >= f) && to.is_none_or(|t| a <= t))
        };
        let genres_ok = genres.iter().all(|g| has_genre(m, g));
        kind_ok && style_ok && years_ok && !is_format(m) && (wants_kids || !is_kids(m)) && genres_ok
    }).cloned().collect()
}

/// Whether an episode's release date allows playing it on `today` ("YYYY-MM-DD"). No date at all
/// (older episodes often carry none) counts as released; a placeholder such as "TBA" or "TBD"
/// is an episode that has not been announced yet, so it does not.
fn released_by(released: &str, today: &str) -> bool {
    let released = released.trim();
    if released.is_empty() { return true; }
    match released.get(..10) {
        Some(date) if date.as_bytes().iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() }) => date <= today,
        _ => {
            let word = released.to_lowercase();
            !(word.starts_with("tba") || word.starts_with("tbd") || word.starts_with("to be") || word.starts_with("unannounced"))
        }
    }
}

/// Normal released episodes only; specials must not become a new viewer's first episode.
pub fn regular_episodes(list: &[crate::backend::Episode], now: i64) -> Vec<&crate::backend::Episode> {
    let (y,m,d,_) = crate::backend::civil(now);
    let today = format!("{y:04}-{m:02}-{d:02}");
    let mut out: Vec<_> = list.iter().filter(|e| e.season > 0 && e.number > 0 && released_by(&e.released, &today)).collect();
    out.sort_by_key(|e| (e.season,e.number));
    out
}
pub fn series_complete(list: &[crate::backend::Episode], seen: &HashSet<String>, now: i64) -> bool {
    let available = regular_episodes(list,now);
    !available.is_empty() && available.iter().all(|e| seen.contains(&e.id))
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardPlayback { pub video: String, pub label: String }
/// Resolve from stored progress, not a catalogue card's usually empty progress fields.
pub fn card_playback(media: &Media, list: &[crate::backend::Episode], seen: &HashSet<String>, now: i64) -> Option<CardPlayback> {
    if media.kind == "movie" {
        return Some(CardPlayback { video: media.id.clone(), label: if crate::backend::in_progress(media) { "Resume" } else { "Play" }.into() });
    }
    let episodes = regular_episodes(list,now);
    if let Some(video) = media.last_video_id.as_ref().filter(|v| crate::backend::in_progress(media) && !seen.contains(*v)) {
        let label = episodes.iter().find(|e| &e.id == video).map(|e| e.label())
            .or_else(|| crate::backend::episode_label(video));
        return Some(CardPlayback { video: video.clone(), label: label.map(|l| format!("Resume {l}")).unwrap_or_else(|| "Resume".into()) });
    }
    // If S3E4 was finished or marked watched, continue with S3E5, rather than replaying S3E4.
    let furthest = episodes.iter().rposition(|e| seen.contains(&e.id));
    let next = furthest.and_then(|i| episodes.iter().skip(i+1).find(|e| !seen.contains(&e.id))).copied()
        .or_else(|| episodes.iter().find(|e| !seen.contains(&e.id)).copied()).or_else(|| episodes.first().copied())?;
    Some(CardPlayback { video: next.id.clone(), label: format!("{} {}", if furthest.is_some() && !seen.contains(&next.id) { "Resume" } else { "Play" }, next.label()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media(id: &str, kind: &str, year: &str, genres: &[&str]) -> Media {
        tidy(Media::from_json(&json!({"id": id, "type": kind, "name": id, "releaseInfo": year, "genres": genres}), "a").unwrap())
    }
    fn ids(v: Vec<Media>) -> Vec<String> { v.into_iter().map(|m| m.id).collect() }

    #[test]
    fn tmdb_settles_a_catalogue_s_wrong_animation_tag() {
        // Cinemeta lists this live-action film as Animation, Comedy, Crime.
        let mut viking = media("tt27817389", "movie", "2025", &["Animation", "Comedy", "Crime"]);
        assert!(is_animated(&viking));
        settle_style(&mut viking, false);
        assert!(!is_animated(&viking) && viking.genres == ["Comedy", "Crime"]);
        assert!(filter_picks(std::slice::from_ref(&viking), Some("movie"), Style::Animation, &[], None, None, 2026).is_empty());
        // The same title from the catalogue again cannot bring the style back.
        let mut pool = vec![viking.clone()];
        merge(&mut pool, vec![media("tt27817389", "movie", "2025", &["Animation", "Comedy"])]);
        assert!(!is_animated(&pool[0]) && !pool[0].genres.iter().any(|g| g == "Animation"));
        // A settled entry arriving later corrects the one already in the pool.
        let mut pool = vec![media("tt27817389", "movie", "2025", &["Animation", "Comedy"])];
        merge(&mut pool, vec![viking]);
        assert!(!is_animated(&pool[0]));
        // TMDB can also confirm animation a catalogue left out.
        let mut film = media("tt0000001", "movie", "2020", &["Comedy"]);
        settle_style(&mut film, true);
        assert!(is_animated(&film) && style_matches(&film, Style::Animation));
    }

    fn episode(season: i64, number: i64, released: &str) -> crate::backend::Episode {
        crate::backend::Episode { id: format!("tt-fixture:{season}:{number}"), season, number, title: String::new(), thumbnail: String::new(), overview: String::new(), released: released.into() }
    }
    #[test]
    fn cards_start_at_first_regular_episode_resume_mid_series_and_skip_finished_episode() {
        let mut media = media("tt-fixture","series","2020",&["Drama"]);
        let episodes = vec![episode(0,1,"2020-01-01"),episode(3,5,"2020-01-01"),episode(1,1,"2020-01-01"),episode(3,4,"2020-01-01"),episode(4,1,"2099-01-01")];
        let now = 1_800_000_000;
        let mut seen = HashSet::new();
        assert_eq!(card_playback(&media,&episodes,&seen,now).unwrap().label,"Play S1 E1");
        media.last_video_id = Some("tt-fixture:3:4".into());media.position_ms=120_000;
        assert_eq!(card_playback(&media,&episodes,&seen,now).unwrap().label,"Resume S3 E4");
        media.watched=true; // A prior season was complete; the current episode is unfinished.
        assert_eq!(card_playback(&media,&episodes,&seen,now).unwrap().label,"Resume S3 E4");
        assert!(!series_complete(&episodes,&seen,now));
        seen.insert("tt-fixture:3:4".into());media.video_watched=true;
        assert_eq!(card_playback(&media,&episodes,&seen,now).unwrap().video,"tt-fixture:3:5");
        assert_eq!(card_playback(&media,&episodes,&seen,now).unwrap().label,"Resume S3 E5");
        seen.extend(["tt-fixture:1:1".into(),"tt-fixture:3:5".into()]);
        assert!(series_complete(&episodes,&seen,now));
        assert!(!series_complete(&[],&seen,now));
        assert_eq!(card_playback(&media,&episodes,&seen,now).unwrap().label,"Play S1 E1");
    }
    #[test]
    fn movie_cards_resume_only_unfinished_playback() {
        let mut m=media("film","movie","2020",&[]);
        m.position_ms=40_000;
        assert_eq!(card_playback(&m,&[],&HashSet::new(),1_800_000_000).unwrap().label,"Resume");
        m.watched=true;
        assert_eq!(card_playback(&m,&[],&HashSet::new(),1_800_000_000).unwrap().label,"Play");
    }

    #[test]
    fn genres_share_one_vocabulary_and_pairs_are_not_split_into_claims() {
        assert_eq!(canonical_genre("Science Fiction"), "Sci-Fi");
        assert_eq!(canonical_genre("Science-Fiction"), "Sci-Fi");
        assert_eq!(canonical_genre("Sci-Fi & Fantasy"), "Sci-Fi & Fantasy");
        assert_eq!(canonical_genre("Children"), "Kids");
        let m = media("x", "series", "2019", &["Sci-Fi & Fantasy"]);
        assert_eq!(m.genres, ["Sci-Fi & Fantasy"]);
        assert!(has_genre(&m, "Science Fiction") && has_genre(&m, "Fantasy"));
        assert_eq!(genre_choices(&media("y", "movie", "2020", &["Animation", "Comedy"])), ["Comedy"]);
    }

    #[test]
    fn duplicate_metadata_preserves_animation_and_live_action_excludes_it() {
        let sparse=media("simpsons","series","1989",&["Comedy","Family"]);
        let mut detailed=sparse.clone();detailed.genres.push("Animation".into());
        let mut pool=vec![sparse];merge(&mut pool,vec![detailed]);
        assert_eq!(pool.len(),1);assert!(is_animated(&pool[0]));
        assert!(filter_picks(&pool, None, Style::Live, &[], None, None, 2026).is_empty());
        assert_eq!(filter_picks(&pool, None, Style::Animation, &[], None, None, 2026).len(), 1);
        let mut raw=media("raw","series","2020",&["Comedy"]);
        raw.raw["tmdb_id"]=serde_json::json!(1);raw.raw["genre_ids"]=serde_json::json!([16,35]);assert!(is_animated(&raw));
        raw.raw["genre_ids"]=serde_json::Value::Null;raw.raw["show_type"]=serde_json::json!("Animation");assert!(is_animated(&raw));
    }
    #[test]
    fn type_and_style_combine_and_formats_and_kids_stay_out() {
        let c = [media("film", "movie", "2000", &["Drama"]), media("toon", "movie", "2010", &["Animation"]),
            media("show", "series", "2015", &["Drama"]), media("anime", "anime", "2020", &[]),
            media("reality", "series", "2018", &["Reality-TV"]), media("kids", "series", "2016", &["Children"])];
        assert_eq!(ids(filter_picks(&c, None, Style::Live, &[], None, None, 2026)), ["film", "show"]);
        assert_eq!(ids(filter_picks(&c, Some("movie"), Style::Animation, &[], None, None, 2026)), ["toon"]);
        assert_eq!(ids(filter_picks(&c, Some("series"), Style::Animation, &[], None, None, 2026)), ["anime"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, &[], None, None, 2026)), ["film", "toon", "show", "anime"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, &["Kids".into()], None, None, 2026)), ["kids"]);
    }

    #[test]
    fn multiple_genres_combine_with_and() {
        let c = [
            media("both", "movie", "2020", &["Sci-Fi", "Thriller"]),
            media("only-scifi", "movie", "2020", &["Sci-Fi", "Drama"]),
            media("pair", "series", "2021", &["Sci-Fi & Fantasy", "Thriller"]),
            media("kids-comedy", "series", "2022", &["Kids", "Comedy"]),
        ];
        assert_eq!(ids(filter_picks(&c, None, Style::Both, &["Sci-Fi".into(), "Thriller".into()], None, None, 2026)), ["both", "pair"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, &["Kids".into(), "Comedy".into()], None, None, 2026)), ["kids-comedy"]);
    }

    #[test]
    fn series_match_any_year_they_aired_and_unknown_years_never_match_a_bound() {
        let c = [media("a", "movie", "2000", &["Drama"]), media("b", "series", "2010–2017", &["Drama"]),
            media("c", "series", "2016–", &["Drama"]), media("d", "movie", "", &["Drama"])];
        assert_eq!(ids(filter_picks(&c, None, Style::Both, &["drama".into()], Some(2000), Some(2010), 2026)), ["a", "b"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, &[], Some(2020), None, 2026)), ["c"]);
        assert_eq!(ids(filter_picks(&c, Some("movie"), Style::Both, &[], None, None, 2026)), ["a", "d"]);
        assert!(filter_picks(&c, None, Style::Both, &[], Some(2021), Some(2000), 2026).is_empty());
    }

    #[test]
    fn merge_keeps_conflicting_imdb_ids_and_different_kinds_separate() {
        let m1 = Media::from_json(&json!({"id": "tt0000001", "type": "movie", "name": "Crash", "releaseInfo": "2004"}), "a").unwrap();
        let m2 = Media::from_json(&json!({"id": "tt0000002", "type": "movie", "name": "Crash", "releaseInfo": "2004"}), "b").unwrap();
        let mut pool = vec![m1];
        merge(&mut pool, vec![m2]);
        assert_eq!(pool.len(), 2, "different IMDb ids must never merge even with equal title/year");

        let film = Media::from_json(&json!({"id": "tmdb:100", "type": "movie", "name": "Fargo", "releaseInfo": "2014"}), "a").unwrap();
        let show = Media::from_json(&json!({"id": "tvmaze:100", "type": "series", "name": "Fargo", "releaseInfo": "2014"}), "b").unwrap();
        let mut pool2 = vec![film];
        merge(&mut pool2, vec![show]);
        assert_eq!(pool2.len(), 2, "movie and series must not merge by title_key alone");
    }

    #[test]
    fn the_same_title_from_two_sources_is_kept_once() {
        let mut pool = vec![media("kitsu:1", "anime", "2023", &[])];
        merge(&mut pool, vec![media("tt1", "series", "2023", &["Animation"]), media("tt2", "series", "2023", &[])]);
        // Same names would merge; these differ by name ("kitsu:1" vs "tt1").
        assert_eq!(pool.len(), 3);
        let a = Media::from_json(&json!({"id": "kitsu:9", "type": "anime", "name": "Frieren: Beyond Journey's End", "releaseInfo": "2023"}), "k").unwrap();
        let b = Media::from_json(&json!({"id": "tt22248376", "type": "series", "name": "Frieren: Beyond Journey’s End", "releaseInfo": "2023–"}), "c").unwrap();
        let mut pool = vec![a];
        merge(&mut pool, vec![b]);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn reality_talk_and_game_shows_keep_one_name_whichever_way_they_are_spelled() {
        for g in ["Reality TV", "reality tv", "Reality-TV", "Reality"] { assert_eq!(canonical_genre(g), "Reality", "{g}"); }
        for g in ["Talk Show", "talk show", "Talk-Show", "Talk"] { assert_eq!(canonical_genre(g), "Talk", "{g}"); }
        for g in ["Game Show", "game show", "Game-Show"] { assert_eq!(canonical_genre(g), "Game Show", "{g}"); }
        assert!(is_format(&media("tv", "series", "2020", &["Reality TV"])) && is_format(&media("tv2", "series", "2020", &["Talk Show"])));
    }

    #[test]
    fn a_later_source_fills_in_a_missing_synopsis_and_rating_but_never_replaces_them() {
        let mut sparse = media("tt0000010", "movie", "2020", &["Drama"]);
        sparse.raw["imdbRating"] = json!("7.4");
        let mut detailed = sparse.clone();
        detailed.description = "A full synopsis.".into();
        detailed.poster = "poster.jpg".into();
        detailed.raw["imdbRating"] = json!("1.0");
        detailed.raw["tmdbRating"] = json!(8.1);
        let mut pool = vec![sparse];
        merge(&mut pool, vec![detailed.clone()]);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].description, "A full synopsis.");
        assert_eq!(pool[0].poster, "poster.jpg");
        assert_eq!(pool[0].raw["imdbRating"], json!("7.4"), "a rating already there stays");
        assert_eq!(pool[0].raw["tmdbRating"], json!(8.1));
        // Once present, a different later synopsis does not replace it.
        let mut other = detailed;
        other.description = "Another synopsis.".into();
        merge(&mut pool, vec![other]);
        assert_eq!(pool[0].description, "A full synopsis.");
    }

    #[test]
    fn merging_many_titles_matches_one_at_a_time_and_prefers_the_first_match() {
        let a = media("tt1", "movie", "2001", &["Drama"]);
        let b = media("tt2", "movie", "2002", &["Drama"]);
        // Same title and year as `a`, but another source without an IMDb id.
        let mut a_other = Media::from_json(&json!({"id": "tmdb:5", "type": "movie", "name": "tt1", "releaseInfo": "2001", "genres": ["Comedy"]}), "b").unwrap();
        a_other = tidy(a_other);
        let mut pool = vec![a.clone(), b.clone()];
        merge(&mut pool, vec![a_other.clone(), b.clone(), a.clone(), media("tt3", "series", "2003", &[])]);
        assert_eq!(ids(pool.clone()), ["tt1", "tt2", "tt3"]);
        assert!(pool[0].genres.iter().any(|g| g == "Comedy"));
        // The same batch merged one title at a time gives the same pool.
        let mut single = vec![a.clone(), b.clone()];
        for m in [a_other, b, a, media("tt3", "series", "2003", &[])] { merge(&mut single, vec![m]); }
        assert_eq!(ids(single), ["tt1", "tt2", "tt3"]);
    }

    #[test]
    fn episodes_without_a_date_count_as_out_and_placeholders_do_not() {
        let now = 1_800_000_000;
        let list = vec![episode(1, 1, ""), episode(1, 2, "TBA"), episode(1, 3, "2020-05-01T00:00:00.000Z"),
            episode(1, 4, "tbd"), episode(1, 5, "2099-01-01"), episode(1, 6, "  "), episode(1, 7, "To be announced")];
        let numbers: Vec<i64> = regular_episodes(&list, now).iter().map(|e| e.number).collect();
        assert_eq!(numbers, [1, 3, 6]);
        assert!(series_complete(&list, &["tt-fixture:1:1".to_owned(), "tt-fixture:1:3".to_owned(), "tt-fixture:1:6".to_owned()].into_iter().collect(), now));
    }
}
