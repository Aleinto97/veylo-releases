use crate::db::Media;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

// Explainable local content ranking, never cross-namespace inference.
fn terms(meta: &Value, fields: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for field in fields {
        match &meta[*field] {
            Value::String(s) => {
                for s in s.split(',') {
                    let s = s.trim().to_lowercase();
                    if !s.is_empty() {
                        out.insert(s);
                    }
                }
            }
            Value::Array(a) => {
                for s in a {
                    if let Some(s) = s.as_str().or_else(|| s["name"].as_str()) {
                        let s = s.trim().to_lowercase();
                        if !s.is_empty() {
                            out.insert(s);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}
struct Features {
    genres: BTreeSet<String>,
    directors: BTreeSet<String>,
    cast: BTreeSet<String>,
}
impl Features {
    fn of(media: &Media) -> Self {
        Self {
            genres: terms(&media.meta, &["genres", "genre", "tags"]),
            directors: terms(&media.meta, &["director", "directors"]),
            cast: terms(&media.meta, &["cast"]),
        }
    }
}
fn affinity_score(a: &Features, b: &Features, same: bool) -> f64 {
    let score = a.genres.intersection(&b.genres).count() as f64 * 2.0
        + a.directors.intersection(&b.directors).count() as f64 * 4.0
        + a.cast.intersection(&b.cast).count() as f64 * 0.5
        + if same { 0.4 } else { 0.0 };
    if same {
        score
    } else {
        score * 0.5
    }
}
fn reason(candidate: &Media, a: &Features, seed: &Media, b: &Features, similar: bool) -> String {
    let title = seed.meta["name"]
        .as_str()
        .filter(|name| !name.trim().is_empty());
    let basis = title.map(|name| {
        if similar {
            format!("Similar to {name}")
        } else if seed.liked > 0 {
            format!("Because you liked {name}")
        } else if seed.watched || seed.watched_ms >= 60_000 {
            format!("Because you watched {name}")
        } else {
            format!("Because you saved {name}")
        }
    });
    let directors: Vec<_> = a.directors.intersection(&b.directors).cloned().collect();
    if !directors.is_empty() {
        return match basis {
            Some(basis) => format!("{basis}: same director, {}", directors.join(", ")),
            None => format!("Shared director: {}", directors.join(", ")),
        };
    }
    let genres: Vec<_> = a.genres.intersection(&b.genres).take(3).cloned().collect();
    if !genres.is_empty() {
        return basis.unwrap_or_else(|| format!("Shared genres/tags: {}", genres.join(", ")));
    }
    let cast: Vec<_> = a.cast.intersection(&b.cast).take(2).cloned().collect();
    if !cast.is_empty() {
        return match basis {
            Some(basis) => format!("{basis}: shares {} in the cast", cast.join(", ")),
            None => format!("Shared cast: {}", cast.join(", ")),
        };
    }
    if candidate.kind == seed.kind {
        format!("More {} titles from your cached catalogs", candidate.kind)
    } else {
        "From your cached catalogs".into()
    }
}
fn positive(m: &Media) -> bool {
    m.liked > 0 || m.saved || m.watched || m.watched_ms >= 60_000
}
pub(crate) fn recommend(
    all: &[Media],
    similar_key: Option<&str>,
    limit: usize,
    exclude_liked: bool,
) -> Value {
    let similar = similar_key.and_then(|key| all.iter().position(|m| m.key == key));
    if similar_key.is_some() && similar.is_none() {
        return json!({"metas":[],"needs_history":false,"missing_metadata":true});
    }
    // Decode each metadata vocabulary once, then score pairs without allocating
    // strings. Explanations are built only for each candidate's winning seed.
    let features: Vec<_> = all.iter().map(Features::of).collect();
    let positives: Vec<_> = all
        .iter()
        .enumerate()
        .filter(|(_, m)| positive(m) && m.liked >= 0)
        .map(|(i, _)| i)
        .collect();
    let negatives: Vec<_> = all
        .iter()
        .enumerate()
        .filter(|(_, m)| m.liked < 0)
        .map(|(i, _)| i)
        .collect();
    let needs_history = similar.is_none() && positives.is_empty();
    let mut ranked = Vec::new();
    for (index, c) in all.iter().enumerate() {
        if c.liked < 0
            || similar == Some(index)
            || c.meta.get("name").and_then(Value::as_str).is_none()
            || (similar.is_none()
                && (c.saved
                    || c.watched
                    || c.watched_ms >= 60_000
                    || (exclude_liked && c.liked > 0)))
        {
            continue;
        }
        let score_for = |seed: usize| {
            affinity_score(&features[index], &features[seed], c.kind == all[seed].kind)
        };
        let best = if let Some(seed) = similar {
            Some((score_for(seed), seed))
        } else {
            positives
                .iter()
                .map(|&seed| {
                    let weight = if all[seed].liked > 0 {
                        1.8
                    } else if all[seed].watched {
                        1.2
                    } else {
                        1.0
                    };
                    (score_for(seed) * weight, seed)
                })
                .max_by(|a, b| a.0.total_cmp(&b.0))
        };
        let (mut score, explanation) = best
            .map(|(score, seed)| {
                (
                    score,
                    reason(
                        c,
                        &features[index],
                        &all[seed],
                        &features[seed],
                        similar.is_some(),
                    ),
                )
            })
            .unwrap_or((
                0.0,
                "Discover from your cached catalogs; add history to personalize".into(),
            ));
        for &seed in &negatives {
            score -= score_for(seed) * 0.45;
        }
        if c.liked > 0 {
            score += 3.0;
        }
        ranked.push((score, c.key.clone(), c, explanation));
    }
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let metas = ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, m, reason)| {
            let mut v = m.value();
            v["reason"] = json!(reason);
            v
        })
        .collect::<Vec<_>>();
    json!({"metas":metas,"needs_history":needs_history,"source":"local_cached_metadata"})
}
const MAX_RULE_COLLECTIONS: usize = 24;
pub(crate) fn collections(all: &[Media]) -> Value {
    let mut collections = Vec::new();
    for (id, name, rule) in [
        ("continue-watching", "Continue watching", 0),
        ("watchlist", "Watchlist", 1),
        ("watched", "Watched", 2),
        ("liked", "Liked", 3),
    ] {
        let items = all
            .iter()
            .filter(|m| match rule {
                0 => m.position > 0 && !m.watched && !m.video_watched,
                1 => m.saved,
                2 => m.watched,
                _ => m.liked > 0,
            })
            .map(|m| m.value())
            .collect::<Vec<_>>();
        if !items.is_empty() {
            collections.push(json!({"id":id,"name":name,"kind":"history_rule","metas":items}));
        }
    }
    let mut buckets: BTreeMap<(String, String), Vec<&Media>> = BTreeMap::new();
    for m in all {
        if m.liked < 0 {
            continue;
        }
        for (label, fields) in [
            ("Genre / tag", vec!["genres", "genre", "tags"]),
            ("Director", vec!["director", "directors"]),
        ] {
            for term in terms(&m.meta, &fields) {
                buckets.entry((label.into(), term)).or_default().push(m);
            }
        }
    }
    // Only the strongest genre/director rules: one collection per term was
    // unbounded and every one crossed JNI on each library change.
    let mut rules = buckets
        .into_iter()
        .filter(|(_, items)| items.len() >= 2 && items.iter().any(|m| positive(m)))
        .collect::<Vec<_>>();
    rules.sort_by_key(|((label, term), items)| {
        (
            std::cmp::Reverse(items.iter().filter(|m| positive(m)).count()),
            std::cmp::Reverse(items.len()),
            label.clone(),
            term.clone(),
        )
    });
    for ((label, term), items) in rules.into_iter().take(MAX_RULE_COLLECTIONS) {
        collections.push(json!({"id":json!(["metadata-rule",label,term]).to_string(),"name":format!("{label}: {term}"),"kind":"metadata_rule","metas":items.iter().take(100).map(|m|m.value()).collect::<Vec<_>>()}));
    }
    // Franchises require explicit source IDs and names. Similar titles never imply relationships.
    let mut franchises: BTreeMap<(String, String, String, String), Vec<&Media>> = BTreeMap::new();
    for m in all {
        for field in ["collection", "belongsToCollection", "franchise"] {
            let v = &m.meta[field];
            if let (Some(id), Some(name)) = (v["id"].as_str(), v["name"].as_str()) {
                franchises
                    .entry((m.addon.clone(), m.kind.clone(), id.into(), name.into()))
                    .or_default()
                    .push(m);
            }
        }
    }
    for ((addon, kind, id, name), mut items) in franchises {
        items.sort_by(|a, b| a.key.cmp(&b.key));
        items.dedup_by(|a, b| a.key == b.key);
        if items.len() < 2 {
            continue;
        }
        collections.push(json!({"id":json!(["curated",addon,kind,id]).to_string(),"name":name,"kind":"addon_curated_franchise","metas":items.iter().take(100).map(|m|m.value()).collect::<Vec<_>>()}));
    }
    json!({"collections":collections})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(key: &str, name: &str, genres: &[&str], liked: i64) -> Media {
        Media {
            key: key.into(),
            kind: "movie".into(),
            id: key.into(),
            addon: "https://a.invalid/manifest.json".into(),
            meta: json!({"name": name, "genres": genres}),
            saved: false,
            watched: false,
            liked,
            watched_ms: 0,
            position: 0,
            duration: 0,
            video_id: None,
            video_watched: false,
        }
    }

    #[test]
    fn exclude_liked_keeps_liked_as_seeds_without_consuming_candidate_slots() {
        let all = vec![
            mk("liked1", "Liked SciFi", &["Sci-Fi", "Thriller"], 1),
            mk("cand1", "Unseen SciFi", &["Sci-Fi", "Thriller"], 0),
            mk("cand2", "Unseen Comedy", &["Comedy"], 0),
        ];
        let v = recommend(&all, None, 10, true);
        let metas = v["metas"].as_array().unwrap();
        assert_eq!(v["needs_history"], false);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0]["id"], "cand1");
        assert!(metas.iter().all(|m| m["id"] != "liked1"));
    }
}
