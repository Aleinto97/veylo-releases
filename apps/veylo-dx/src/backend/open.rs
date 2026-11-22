//! Keyless sources: everything here works without a TMDB key. TVmaze gives a series' cast and
//! crew, season posters, episode stills, next episode and Home's new episodes; Wikidata a film's
//! credits with photos, studios, certificate, critics' scores and Picks keywords; Cinemeta's
//! catalogues Picks candidates and related titles. Answers are kept for the session, one request
//! per title however many screens ask at once; a failed answer isn't kept, so the next visit
//! asks again.
use super::*;
use std::sync::Arc;

/// GET JSON from a fixed public endpoint. Wikidata asks every client to name itself; the name
/// is the app's, nothing about the person using it.
async fn get_json(url: String, query: Vec<(&'static str, String)>) -> Option<Value> {
    on_runtime(async move {
        let response = backend().http.get(url).query(&query)
            .header(reqwest::header::USER_AGENT, concat!("Veylo/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(12)).send().await.ok()?.error_for_status().ok()?;
        response.json::<Value>().await.ok()
    }).await.ok().flatten()
}

type Shared = HashMap<String, Arc<tokio::sync::OnceCell<Value>>>;

async fn once(cache: &'static OnceLock<Mutex<Shared>>, key: String, fetch: impl Future<Output = Option<Value>>) -> Option<Value> {
    let cell = {
        let mut c = cache.get_or_init(Default::default).lock().ok()?;
        match c.get(&key) {
            Some(cell) => cell.clone(),
            None => { let cell = Arc::new(tokio::sync::OnceCell::new()); cache_insert(&mut c, 120, key, cell.clone()); cell }
        }
    };
    cell.get_or_try_init(|| async move { fetch.await.ok_or(()) }).await.ok().cloned()
}

/// An IMDb id safe to put in a query: "tt" and digits only.
fn checked_imdb(m: &Media) -> Option<String> {
    imdb_of(m).filter(|id| id.len() > 2 && id[2..].bytes().all(|b| b.is_ascii_digit()))
}

// ---------------------------------------------------------------- TVmaze

static SHOWS: OnceLock<Mutex<Shared>> = OnceLock::new();

/// TVmaze's show for a series, with its cast, crew, seasons, episodes and next episode in one answer.
async fn tvmaze_show(m: &Media) -> Option<Value> {
    if m.kind == "movie" { return None; }
    let imdb = checked_imdb(m)?;
    once(&SHOWS, imdb.clone(), async move {
        let found = get_json("https://api.tvmaze.com/lookup/shows".into(), vec![("imdb", imdb)]).await?;
        let id = found["id"].as_i64()?;
        let embed = ["cast", "crew", "seasons", "episodes", "nextepisode"].map(|e| ("embed[]", e.to_owned())).to_vec();
        get_json(format!("https://api.tvmaze.com/shows/{id}"), embed).await
    }).await
}

fn tvmaze_image(v: &Value) -> Option<String> {
    v["image"]["medium"].as_str().or(v["image"]["original"].as_str()).filter(|u| u.starts_with("https://")).map(str::to_owned)
}

/// A series' cast (in billing order, with photos and parts), crew, season posters and language.
pub(super) fn tvmaze_production(show: &Value) -> Artwork {
    let e = &show["_embedded"];
    let mut cast: Vec<Person> = Vec::new();
    for c in e["cast"].as_array().into_iter().flatten() {
        let name = s(&c["person"], "name");
        if name.is_empty() || cast.iter().any(|p| p.name == name) { continue; }
        cast.push(Person { name, role: s(&c["character"], "name"), photo: tvmaze_image(&c["person"]).or_else(|| tvmaze_image(&c["character"])) });
        if cast.len() == 15 { break; }
    }
    let mut crew: Vec<(String, Vec<Person>)> = Vec::new();
    for (kind, label) in [("Creator", "Created by"), ("Music", "Music by"), ("Director Of Photography", "Cinematography by")] {
        let people: Vec<Person> = e["crew"].as_array().into_iter().flatten().filter(|c| c["type"] == kind)
            .map(|c| Person { name: s(&c["person"], "name"), role: String::new(), photo: tvmaze_image(&c["person"]) })
            .filter(|p| !p.name.is_empty()).take(3).collect();
        if !people.is_empty() { crew.push((label.into(), people)); }
    }
    let season_posters = e["seasons"].as_array().into_iter().flatten()
        .filter_map(|x| Some((x["number"].as_i64()?, tvmaze_image(x)?))).collect();
    Artwork { cast, crew, season_posters, language: Some(s(show, "language")).filter(|l| !l.is_empty()), ..Artwork::default() }
}

/// TVmaze's episode stills for one season: episode number -> image.
pub(super) async fn tvmaze_stills(m: &Media, season: i64) -> HashMap<i64, String> {
    let Some(show) = tvmaze_show(m).await else { return HashMap::new() };
    show["_embedded"]["episodes"].as_array().into_iter().flatten().filter(|e| e["season"].as_i64() == Some(season))
        .filter_map(|e| Some((e["number"].as_i64()?, tvmaze_image(e)?))).collect()
}

/// Where a series stands: its next episode and when it airs, or that it has ended.
pub async fn series_status(m: &Media) -> Option<String> {
    let show = tvmaze_show(m).await?;
    let next = &show["_embedded"]["nextepisode"];
    if let (Some(season), Some(number), Some(date)) = (next["season"].as_i64(), next["number"].as_i64(), next["airdate"].as_str().filter(|d| !d.is_empty())) {
        // "27 Nov" this year, "27 Nov 2027" beyond it: the fact shares one row with the others.
        let this_year = day_label(now_secs().div_euclid(86_400))[..4].to_owned();
        let when = date_label(date);
        let when = when.strip_suffix(&format!(" {this_year}")).unwrap_or(&when);
        return Some(format!("Next: {}, {when}", episode_tag(season, number)));
    }
    (show["status"] == "Ended").then(|| "Ended".to_owned())
}

/// A TVmaze show as a Cinemeta title (by its IMDb id), so it opens and plays like any other.
pub(super) fn tvmaze_media(s: &Value) -> Option<Media> {
    let imdb = s["externals"]["imdb"].as_str().filter(|i| i.starts_with("tt"))?;
    let year: i32 = s["premiered"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0);
    let ended: Option<i32> = s["ended"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok());
    let summary = html_tag().replace_all(s["summary"].as_str().unwrap_or_default(), "").to_string();
    let meta = json!({
        "id": imdb, "type": "series", "name": s["name"], "description": summary,
        "releaseInfo": match (year, ended) { (0, _) => String::new(), (y, Some(e)) if e != y => format!("{y}–{e}"), (y, Some(_)) => y.to_string(), (y, None) => format!("{y}–") },
        "poster": s["image"]["original"].as_str().or(s["image"]["medium"].as_str()).unwrap_or_default(),
        "genres": tvmaze_genres(s), "imdbRating": s["rating"]["average"].as_f64().map(|v| format!("{v:.1}")).unwrap_or_default(),
    });
    Media::from_json(&meta, CINEMETA).map(crate::picks_rules::tidy)
}

/// "YYYY-MM-DD" for a day counted from 1970-01-01 (UTC).
fn day_label(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

static SCHEDULES: OnceLock<Mutex<Shared>> = OnceLock::new();

/// Home's "New episodes": the most followed series with an episode out in the last three days,
/// on US television or streaming (TVmaze's schedules). Formats (talk, news, game shows, reality,
/// sport, awards) are left out.
pub async fn new_episodes() -> Vec<Media> {
    let today = now_secs().div_euclid(86_400);
    let mut shows: Vec<Value> = Vec::new();
    for day in [today, today - 1, today - 2].map(day_label) {
        for (url, country) in [("https://api.tvmaze.com/schedule/web", None), ("https://api.tvmaze.com/schedule", Some("US"))] {
            let mut query = vec![("date", day.clone())];
            if let Some(c) = country { query.push(("country", c.to_owned())); }
            let key = format!("{url}|{day}");
            let Some(list) = once(&SCHEDULES, key, get_json(url.into(), query)).await else { continue };
            shows.extend(list.as_array().into_iter().flatten().map(|e| if e["show"].is_object() { e["show"].clone() } else { e["_embedded"]["show"].clone() }));
        }
    }
    let formats = ["Talk Show", "News", "Game Show", "Reality", "Sports", "Award Show", "Variety"];
    shows.retain(|s| s["weight"].as_i64().unwrap_or(0) >= 85 && !formats.contains(&s["type"].as_str().unwrap_or_default()));
    shows.sort_by_key(|s| std::cmp::Reverse(s["weight"].as_i64().unwrap_or(0)));
    let mut seen = HashSet::new();
    shows.iter().filter(|s| seen.insert(s["id"].as_i64())).filter_map(tvmaze_media).take(20).collect()
}

// ---------------------------------------------------------------- Wikidata

const SPARQL: &str = "https://query.wikidata.org/sparql";
static TITLES: OnceLock<Mutex<Shared>> = OnceLock::new();

async fn sparql(query: String) -> Option<Value> {
    let v = get_json(SPARQL.into(), vec![("query", query), ("format", "json".into())]).await?;
    Some(v["results"]["bindings"].clone()).filter(Value::is_array)
}

/// What the title page takes from Wikidata, in one query: credits (cast with photos and parts),
/// studios and networks with their logos, languages, the US certificate and review scores.
async fn wikidata_title(m: &Media) -> Option<Value> {
    let imdb = checked_imdb(m)?;
    let query = format!(r#"SELECT ?k ?v ?vLabel ?role ?chLabel ?img ?method ?when WHERE {{
  ?f wdt:P345 "{imdb}".
  {{ ?f p:P161 ?st. ?st ps:P161 ?v. BIND("cast" AS ?k) OPTIONAL {{ ?st pq:P4633 ?role }} OPTIONAL {{ ?st pq:P453 ?ch }} OPTIONAL {{ ?v wdt:P18 ?img }} }}
  UNION {{ ?f wdt:P170 ?v. BIND("Created by" AS ?k) OPTIONAL {{ ?v wdt:P18 ?img }} }}
  UNION {{ ?f wdt:P57 ?v. BIND("Directed by" AS ?k) OPTIONAL {{ ?v wdt:P18 ?img }} }}
  UNION {{ ?f wdt:P58 ?v. BIND("Written by" AS ?k) OPTIONAL {{ ?v wdt:P18 ?img }} }}
  UNION {{ ?f wdt:P86 ?v. BIND("Music by" AS ?k) OPTIONAL {{ ?v wdt:P18 ?img }} }}
  UNION {{ ?f wdt:P344 ?v. BIND("Cinematography by" AS ?k) OPTIONAL {{ ?v wdt:P18 ?img }} }}
  UNION {{ ?f wdt:P272|wdt:P449 ?v. BIND("studio" AS ?k) OPTIONAL {{ ?v wdt:P154 ?img }} }}
  UNION {{ ?f wdt:P364 ?v. BIND("language" AS ?k) }}
  UNION {{ ?f wdt:P1657 ?v. BIND("certificate" AS ?k) }}
  UNION {{ ?f p:P444 ?st. ?st ps:P444 ?role. ?st pq:P447 ?v. BIND("score" AS ?k) OPTIONAL {{ ?st pq:P459 ?method }} OPTIONAL {{ ?st pq:P585 ?when }} }}
  SERVICE wikibase:label {{ bd:serviceParam wikibase:language "en,mul". }}
}} LIMIT 800"#);
    once(&TITLES, imdb, sparql(query)).await
}

fn field<'a>(row: &'a Value, key: &str) -> &'a str { row[key]["value"].as_str().unwrap_or_default() }

/// A label (English, else the language-neutral one many names now have), unless Wikidata has
/// neither (it then answers the item's id, "Q12345").
fn label(row: &Value, key: &str) -> Option<String> {
    let l = field(row, key);
    let qid = l.len() > 1 && l.starts_with('Q') && l[1..].bytes().all(|b| b.is_ascii_digit());
    (!l.is_empty() && !qid).then(|| l.to_owned())
}

/// A Commons file at a small width (an SVG logo comes back as a PNG).
fn commons(url: &str, width: u32) -> Option<String> {
    let file = url.split("/Special:FilePath/").nth(1).filter(|f| !f.is_empty())?;
    Some(format!("https://commons.wikimedia.org/wiki/Special:FilePath/{file}?width={width}"))
}

/// Credits, studios, language and certificate from wikidata_title's rows.
pub(super) fn wikidata_production(rows: &Value) -> Artwork {
    let rows: Vec<&Value> = rows.as_array().into_iter().flatten().collect();
    fn of<'a>(rows: &'a [&'a Value], k: &'a str) -> impl Iterator<Item = &'a Value> + 'a { rows.iter().copied().filter(move |r| field(r, "k") == k) }
    // Wikidata keeps no billing order: the named parts first, then those with a photo (the
    // title page puts the add-on's top-billed names before both).
    let mut cast: Vec<(bool, bool, Person)> = Vec::new();
    for r in of(&rows, "cast") {
        let Some(name) = label(r, "vLabel") else { continue };
        let role = Some(field(r, "role").to_owned()).filter(|x| !x.is_empty()).or_else(|| label(r, "chLabel")).unwrap_or_default();
        let photo = commons(field(r, "img"), 185);
        match cast.iter_mut().find(|(_, _, p)| p.name == name) {
            Some((has_role, has_photo, p)) => {
                if p.role.is_empty() && !role.is_empty() { p.role = role; *has_role = true; }
                if p.photo.is_none() && photo.is_some() { p.photo = photo; *has_photo = true; }
            }
            None => cast.push((!role.is_empty(), photo.is_some(), Person { name, role, photo })),
        }
    }
    cast.sort_by_key(|(role, photo, _)| std::cmp::Reverse((*role, *photo)));
    let mut crew: Vec<(String, Vec<Person>)> = Vec::new();
    for job in ["Created by", "Directed by", "Written by", "Music by", "Cinematography by"] {
        let mut people: Vec<Person> = Vec::new();
        for r in of(&rows, job) {
            let Some(name) = label(r, "vLabel") else { continue };
            let photo = commons(field(r, "img"), 185);
            match people.iter_mut().find(|p| p.name == name) {
                Some(p) => if p.photo.is_none() { p.photo = photo },
                None => if people.len() < 3 { people.push(Person { name, role: String::new(), photo }) },
            }
        }
        if !people.is_empty() { crew.push((job.into(), people)); }
    }
    let mut studios: Vec<Studio> = Vec::new();
    for r in of(&rows, "studio") {
        let (Some(name), Some(logo)) = (label(r, "vLabel"), commons(field(r, "img"), 185)) else { continue };
        if studios.len() < 4 && !studios.iter().any(|x| x.name == name) { studios.push(Studio { name, logo }); }
    }
    // Only a title with one original language says which; several can't be told apart here.
    let mut languages: Vec<String> = of(&rows, "language").filter_map(|r| label(r, "vLabel")).collect();
    languages.dedup();
    let certificate = of(&rows, "certificate").filter_map(|r| label(r, "vLabel")).find(|c| c.len() <= 6);
    Artwork {
        cast: cast.into_iter().map(|(_, _, p)| p).take(30).collect(), crew, studios, certificate,
        language: (languages.len() == 1).then(|| languages.remove(0)),
        ..Artwork::default()
    }
}

/// Critics' scores: Rotten Tomatoes' Tomatometer (percent) and Metacritic's Metascore (0-100).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Scores { pub tomatometer: Option<u8>, pub metascore: Option<u8> }

const ROTTEN_TOMATOES: &str = "http://www.wikidata.org/entity/Q105584";
const TOMATOMETER: &str = "http://www.wikidata.org/entity/Q108403393";
const METACRITIC: &str = "http://www.wikidata.org/entity/Q150248";

/// The latest of each score (scores are dated as they change).
pub(super) fn scores_from(rows: &Value) -> Scores {
    let latest = |by: &str, parse: &dyn Fn(&str) -> Option<u8>, method: Option<&str>| {
        rows.as_array().into_iter().flatten()
            .filter(|r| field(r, "k") == "score" && field(r, "v") == by && method.is_none_or(|m| field(r, "method") == m))
            .filter_map(|r| Some((field(r, "when").to_owned(), parse(field(r, "role"))?)))
            .max_by(|a, b| a.0.cmp(&b.0)).map(|(_, v)| v)
    };
    let percent = |t: &str| t.trim().strip_suffix('%')?.trim().parse::<u8>().ok().filter(|v| *v <= 100);
    let of_hundred = |t: &str| t.trim().strip_suffix("/100")?.trim().parse::<u8>().ok().filter(|v| *v <= 100);
    Scores { tomatometer: latest(ROTTEN_TOMATOES, &percent, Some(TOMATOMETER)), metascore: latest(METACRITIC, &of_hundred, None) }
}

pub async fn review_scores(m: &Media) -> Scores {
    wikidata_title(m).await.map(|rows| scores_from(&rows)).unwrap_or_default()
}

/// Picks keywords without a TMDB key: Wikidata's subjects, finer genres and settings
/// ("dream world", "heist", "Paris"). None when Wikidata can't be reached (asked again later).
pub(crate) async fn wikidata_keywords(m: &Media) -> Option<Vec<String>> {
    let Some(imdb) = checked_imdb(m) else { return Some(Vec::new()) };
    let rows = sparql(format!(r#"SELECT DISTINCT ?vLabel WHERE {{ ?f wdt:P345 "{imdb}". ?f wdt:P921|wdt:P136|wdt:P840 ?v.
  SERVICE wikibase:label {{ bd:serviceParam wikibase:language "en,mul". }} }} LIMIT 40"#)).await?;
    let mut out: Vec<String> = Vec::new();
    for r in rows.as_array().into_iter().flatten() {
        let Some(l) = label(r, "vLabel") else { continue };
        let l = l.to_lowercase();
        let k = [" television series", " tv series", " film", " series"].iter().find_map(|x| l.strip_suffix(x)).unwrap_or(&l).trim();
        let k = crate::picks_metadata::clean(k, 80);
        if !k.is_empty() && !out.contains(&k) { out.push(k); }
    }
    out.truncate(24);
    Some(out)
}

/// The title page's material without a TMDB key: TVmaze for a series, Wikidata for the rest
/// and for studios and certificates; TVmaze's answer wins where both have one.
pub async fn open_production(m: &Media) -> Option<Artwork> {
    if tmdb_ready() { return None; }
    let (show, rows) = tokio::join!(tvmaze_show(m), wikidata_title(m));
    let wiki = rows.map(|r| wikidata_production(&r)).unwrap_or_default();
    let Some(show) = show else { return Some(wiki) };
    Some(with_extras(tvmaze_production(&show), wiki))
}

/// `art` with `extra` filling what it lacks.
pub fn with_extras(mut art: Artwork, extra: Artwork) -> Artwork {
    if art.cast.is_empty() { art.cast = extra.cast; }
    if art.crew.is_empty() { art.crew = extra.crew; }
    if art.studios.is_empty() { art.studios = extra.studios; }
    if art.season_posters.is_empty() { art.season_posters = extra.season_posters; }
    art.certificate = art.certificate.or(extra.certificate);
    art.language = art.language.or(extra.language);
    art.tagline = art.tagline.or(extra.tagline);
    art
}

// ---------------------------------------------------------------- Cinemeta

/// Cinemeta's genre options.
const CINEMETA_GENRES: [&str; 19] = ["Action", "Adventure", "Animation", "Biography", "Comedy", "Crime", "Documentary", "Drama", "Family",
    "Fantasy", "History", "Horror", "Mystery", "Romance", "Sci-Fi", "Sport", "Thriller", "War", "Western"];

/// Picks' wider catalogue without a TMDB key: Cinemeta's popular and top-rated lists (for the
/// genre, when Cinemeta has it), at the pages' depths, narrowed to the style and years here.
/// Genres only other add-ons know are theirs to answer (genre_candidates).
pub(super) async fn cinemeta_candidates(genre: Option<String>, kind: Option<String>, style: Style, from: Option<i32>, to: Option<i32>, pages: Vec<u32>) -> Vec<Media> {
    let wanted = match &genre {
        Some(g) => {
            let g = match crate::picks_rules::canonical_genre(g).as_str() { "Kids" => "Family".to_owned(), other => other.to_owned() };
            match CINEMETA_GENRES.iter().find(|c| crate::picks_rules::canonical_genre(c) == g) { Some(c) => Some(*c), None => return Vec::new() }
        }
        None => (style == Style::Animation).then_some("Animation"),
    };
    let kinds: &[&str] = match kind.as_deref() { Some("movie") => &["movie"], Some("series") => &["series"], _ => &["movie", "series"] };
    let mut requests = Vec::new();
    for k in kinds {
        for (i, page) in pages.iter().enumerate() {
            let mut extra = json!({ "skip": (page.saturating_sub(1) % 20 * 50).to_string() });
            if let Some(g) = wanted { extra["genre"] = json!(g); }
            requests.push(json!({"op":"catalog","addon_url":CINEMETA,"type":k,"id":if i % 2 == 0 { "top" } else { "imdbRating" },"extra":extra}));
        }
    }
    let answers = on_runtime(async move {
        let mut set = tokio::task::JoinSet::new();
        for req in requests { set.spawn(exec(req)); }
        let mut out = Vec::new();
        while let Some(done) = set.join_next().await { if let Ok(Ok(v)) = done { out.push(v); } }
        out
    }).await.unwrap_or_default();
    let formats = ["Reality", "Talk", "Game Show", "News"];
    answers.iter().flat_map(|v| metas(v, CINEMETA)).map(crate::picks_rules::tidy).filter(|m| {
        let animated = m.genres.iter().any(|g| g == "Animation" || g == "Anime");
        let style_ok = match style { Style::Animation => animated, Style::Live => !animated, Style::Both => true };
        // A film by its release; a series by the years it aired.
        let first = m.first_year().unwrap_or(0);
        let last = if m.kind == "movie" { first } else { m.year.split('–').nth(1).and_then(|y| y.trim().parse().ok()).unwrap_or(9999) };
        let years_ok = !(from.is_some_and(|f| first == 0 || last < f) || to.is_some_and(|t| first == 0 || first > t));
        style_ok && years_ok && !m.genres.iter().any(|g| formats.contains(&g.as_str()))
    }).collect()
}

/// More like a title without a TMDB key: Cinemeta's lists for its own first two genres, a little
/// way down; Picks' local model then ranks them by story.
pub(super) async fn cinemeta_related(m: &Media) -> Vec<Media> {
    let kind = if m.kind == "movie" { "movie" } else { "series" };
    let animated = m.genres.iter().any(|g| g == "Animation" || g == "Anime");
    let style = if animated { Style::Animation } else { Style::Live };
    let mut out = Vec::new();
    for g in m.genres.iter().filter(|g| !(animated && *g == "Animation")).take(2) {
        let page = 1 + (rand_u64() % 6) as u32;
        out.extend(cinemeta_candidates(Some(g.clone()), Some(kind.into()), style, None, None, vec![page]).await.into_iter().take(12));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_become_dates() {
        assert_eq!(day_label(0), "1970-01-01");
        assert_eq!(day_label(19_782), "2024-02-29");
        assert_eq!(day_label(20_779), "2026-11-22");
    }

    fn row(k: &str, v: &str, role: &str, method: &str, when: &str) -> Value {
        json!({"k":{"value":k},"v":{"value":v},"role":{"value":role},"method":{"value":method},"when":{"value":when}})
    }

    #[test]
    fn the_latest_tomatometer_and_metascore() {
        let rows = json!([
            row("score", ROTTEN_TOMATOES, "8.1/10", "http://www.wikidata.org/entity/Q108403540", "2021-09-04T00:00:00Z"),
            row("score", ROTTEN_TOMATOES, "86%", TOMATOMETER, "2020-01-01T00:00:00Z"),
            row("score", ROTTEN_TOMATOES, "87%", TOMATOMETER, "2022-11-29T00:00:00Z"),
            row("score", METACRITIC, "74/100", "", "2025-06-16T00:00:00Z"),
            row("score", "http://www.wikidata.org/entity/Q37312", "8.8/10", "", ""),
        ]);
        assert_eq!(scores_from(&rows), Scores { tomatometer: Some(87), metascore: Some(74) });
        assert_eq!(scores_from(&json!([])), Scores::default());
    }

    #[test]
    fn wikidata_credits_skip_unlabelled_items_and_keep_one_language() {
        let rows = json!([
            {"k":{"value":"cast"},"vLabel":{"value":"Q123"}},
            {"k":{"value":"cast"},"vLabel":{"value":"Extra Person"}},
            {"k":{"value":"cast"},"vLabel":{"value":"Lead Actor"},"chLabel":{"value":"Cobb"},"img":{"value":"http://commons.wikimedia.org/wiki/Special:FilePath/Lead%20Actor.jpg"}},
            {"k":{"value":"Directed by"},"vLabel":{"value":"A Director"},"img":{"value":"http://commons.wikimedia.org/wiki/Special:FilePath/D.jpg"}},
            {"k":{"value":"studio"},"vLabel":{"value":"No Logo Pictures"}},
            {"k":{"value":"studio"},"vLabel":{"value":"Logo Pictures"},"img":{"value":"http://commons.wikimedia.org/wiki/Special:FilePath/Logo.svg"}},
            {"k":{"value":"language"},"vLabel":{"value":"English"}},
            {"k":{"value":"certificate"},"vLabel":{"value":"PG-13"}},
        ]);
        let art = wikidata_production(&rows);
        assert_eq!(art.cast.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Lead Actor", "Extra Person"]);
        assert_eq!(art.cast[0].role, "Cobb");
        assert_eq!(art.cast[0].photo.as_deref(), Some("https://commons.wikimedia.org/wiki/Special:FilePath/Lead%20Actor.jpg?width=185"));
        assert_eq!(art.crew.len(), 1);
        assert_eq!((art.crew[0].0.as_str(), art.crew[0].1[0].name.as_str()), ("Directed by", "A Director"));
        assert_eq!(art.crew[0].1[0].photo.as_deref(), Some("https://commons.wikimedia.org/wiki/Special:FilePath/D.jpg?width=185"));
        assert_eq!(art.studios.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Logo Pictures"]);
        assert_eq!((art.language.as_deref(), art.certificate.as_deref()), (Some("English"), Some("PG-13")));
    }

    #[test]
    fn tvmaze_credits_in_billing_order() {
        let show = json!({"language":"English","_embedded":{
            "cast":[{"person":{"name":"Bryan Cranston","image":{"medium":"https://static.tvmaze.com/p.jpg"}},"character":{"name":"Walter White"}}],
            "crew":[{"type":"Creator","person":{"name":"Vince Gilligan","image":{"medium":"https://static.tvmaze.com/v.jpg"}}}],
            "seasons":[{"number":1,"image":{"medium":"https://static.tvmaze.com/s1.jpg"}},{"number":2,"image":null}]}});
        let art = tvmaze_production(&show);
        assert_eq!(art.cast[0].role, "Walter White");
        assert_eq!(art.cast[0].photo.as_deref(), Some("https://static.tvmaze.com/p.jpg"));
        assert_eq!((art.crew[0].0.as_str(), art.crew[0].1[0].name.as_str()), ("Created by", "Vince Gilligan"));
        assert_eq!(art.crew[0].1[0].photo.as_deref(), Some("https://static.tvmaze.com/v.jpg"));
        assert_eq!(art.season_posters.len(), 1);
    }

    /// Asks the live services (VEYLO_DX_DATA: an empty folder, so no TMDB key) and prints what
    /// each keyless source gives. `cargo test live_keyless -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_keyless_sources() {
        assert!(std::env::var_os("VEYLO_DX_DATA").is_some(), "point VEYLO_DX_DATA at an empty folder");
        init().unwrap();
        let film = Media::from_json(&json!({"id":"tt1375666","type":"movie","name":"Inception","releaseInfo":"2010","genres":["Sci-Fi","Action"]}), CINEMETA).unwrap();
        let series = Media::from_json(&json!({"id":"tt0903747","type":"series","name":"Breaking Bad","releaseInfo":"2008–2013","genres":["Drama","Crime"]}), CINEMETA).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            assert!(!tmdb_ready());
            let t = std::time::Instant::now();
            let f = open_production(&film).await.unwrap();
            println!("film ({:?}): cast {} ({} photos), first {:?}", t.elapsed(), f.cast.len(), f.cast.iter().filter(|p| p.photo.is_some()).count(), f.cast.first());
            println!("  crew {:?}\n  studios {:?}\n  cert {:?} lang {:?}", f.crew, f.studios.iter().map(|s| &s.name).collect::<Vec<_>>(), f.certificate, f.language);
            println!("  scores {:?}", review_scores(&film).await);
            println!("  keywords {:?}", wikidata_keywords(&film).await);
            let t = std::time::Instant::now();
            let s = open_production(&series).await.unwrap();
            println!("series ({:?}): cast {:?}\n  crew {:?} posters {} lang {:?} studios {:?}", t.elapsed(), s.cast.iter().take(3).map(|p| (&p.name, &p.role)).collect::<Vec<_>>(),
                s.crew, s.season_posters.len(), s.language, s.studios.iter().map(|s| &s.name).collect::<Vec<_>>());
            println!("  stills s1 {} status {:?} scores {:?}", tvmaze_stills(&series, 1).await.len(), series_status(&series).await, review_scores(&series).await);
            let t = std::time::Instant::now();
            let n = new_episodes().await;
            println!("new episodes ({:?}): {:?}", t.elapsed(), n.iter().map(|m| &m.name).collect::<Vec<_>>());
            let c = discover_candidates(Some("Sci-Fi".into()), None, Style::Live, Some(2000), Some(2015), vec![1, 3]).await;
            println!("candidates sci-fi 2000-2015 live: {} {:?}", c.len(), c.iter().take(5).map(|m| (&m.name, &m.year)).collect::<Vec<_>>());
            let a = discover_candidates(None, Some("series".into()), Style::Animation, None, None, vec![2]).await;
            println!("candidates animated series: {} {:?}", a.len(), a.iter().take(4).map(|m| &m.name).collect::<Vec<_>>());
            let r = related(&film).await;
            println!("related: {} {:?}", r.len(), r.iter().take(5).map(|m| &m.name).collect::<Vec<_>>());
            let rows = home_rows().await.unwrap();
            println!("home rows: {:?}", rows.iter().map(|r| (&r.title, r.ranked)).collect::<Vec<_>>());
            for row in rows.into_iter().filter(|r| r.ranked || r.key == NEW_EPISODES_ROW) {
                let items = catalog(row.clone()).await.unwrap();
                println!("  {}: {} {:?}", row.title, items.len(), items.iter().take(3).map(|m| &m.name).collect::<Vec<_>>());
            }
        });
    }
}
