//! What Picks offers, decided the same way for every source: one genre vocabulary, formats and
//! kids' titles left out, animation as a style beside the type, series judged by the years they
//! aired, and one entry per title however many sources list it.
use crate::backend::Media;
use std::collections::HashSet;

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
            "reality-tv" | "reality" => "Reality".into(),
            "talk-show" | "talk" => "Talk".into(),
            "game-show" => "Game Show".into(),
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
pub fn is_animated(m: &Media) -> bool { m.kind == "anime" || m.genres.iter().any(|g| STYLES.contains(&g.as_str())) }

/// Whether a title carries a genre; a pair carries both its halves.
pub fn has_genre(m: &Media, wanted: &str) -> bool {
    let wanted = canonical_genre(wanted);
    m.genres.iter().any(|g| g.eq_ignore_ascii_case(&wanted) || halves(g).iter().any(|h| h.eq_ignore_ascii_case(&wanted)))
}

/// Whether a genre name (in any source's spelling) is, or contains, the wanted one.
pub fn has_genre_name(g: &str, wanted: &str) -> bool {
    let (g, wanted) = (canonical_genre(g), canonical_genre(wanted));
    g.eq_ignore_ascii_case(&wanted) || halves(&g).iter().any(|h| h.eq_ignore_ascii_case(&wanted))
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

/// Add titles to the pool once each: same entry or same title from another source.
pub fn merge(pool: &mut Vec<Media>, more: Vec<Media>) {
    let mut keys: HashSet<String> = pool.iter().map(Media::key).collect();
    let mut titles: HashSet<String> = pool.iter().map(title_key).collect();
    for m in more.into_iter().map(tidy) {
        if keys.insert(m.key()) && titles.insert(title_key(&m)) { pool.push(m); }
    }
}

/// Narrow the pool without reordering it. Formats never pass; kids' titles only when Kids is
/// the genre; unknown years never match a bound; a series matches if it aired in the range.
pub fn filter_picks(items: &[Media], kind: Option<&str>, style: Style, genre: Option<&str>, from: Option<i32>, to: Option<i32>, this_year: i32) -> Vec<Media> {
    let wants_kids = genre.is_some_and(|g| canonical_genre(g) == "Kids");
    items.iter().filter(|m| {
        let kind_ok = kind.map_or(true, |k| if k == "series" { m.kind == "series" || m.kind == "anime" } else { m.kind == k });
        let style_ok = match style { Style::Live => !is_animated(m), Style::Animation => is_animated(m), Style::Both => true };
        let years_ok = if from.is_none() && to.is_none() { true } else {
            year_range(m, this_year).is_some_and(|(a, b)| from.map_or(true, |f| b >= f) && to.map_or(true, |t| a <= t))
        };
        kind_ok && style_ok && years_ok && !is_format(m) && (wants_kids || !is_kids(m)) && genre.map_or(true, |g| has_genre(m, g))
    }).cloned().collect()
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
    fn type_and_style_combine_and_formats_and_kids_stay_out() {
        let c = [media("film", "movie", "2000", &["Drama"]), media("toon", "movie", "2010", &["Animation"]),
            media("show", "series", "2015", &["Drama"]), media("anime", "anime", "2020", &[]),
            media("reality", "series", "2018", &["Reality-TV"]), media("kids", "series", "2016", &["Children"])];
        assert_eq!(ids(filter_picks(&c, None, Style::Live, None, None, None, 2026)), ["film", "show"]);
        assert_eq!(ids(filter_picks(&c, Some("movie"), Style::Animation, None, None, None, 2026)), ["toon"]);
        assert_eq!(ids(filter_picks(&c, Some("series"), Style::Animation, None, None, None, 2026)), ["anime"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, None, None, None, 2026)), ["film", "toon", "show", "anime"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, Some("Kids"), None, None, 2026)), ["kids"]);
    }

    #[test]
    fn series_match_any_year_they_aired_and_unknown_years_never_match_a_bound() {
        let c = [media("a", "movie", "2000", &["Drama"]), media("b", "series", "2010–2017", &["Drama"]),
            media("c", "series", "2016–", &["Drama"]), media("d", "movie", "", &["Drama"])];
        assert_eq!(ids(filter_picks(&c, None, Style::Both, Some("drama"), Some(2000), Some(2010), 2026)), ["a", "b"]);
        assert_eq!(ids(filter_picks(&c, None, Style::Both, None, Some(2020), None, 2026)), ["c"]);
        assert_eq!(ids(filter_picks(&c, Some("movie"), Style::Both, None, None, None, 2026)), ["a", "d"]);
        assert!(filter_picks(&c, None, Style::Both, None, Some(2021), Some(2000), 2026).is_empty());
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
}
