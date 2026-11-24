//! The Picks brain: one taste model, learned from everything you do, over the local embedding
//! model's space (picks_ai) plus a few catalogue features.
//!
//! - Every title becomes a feature vector: its synopsis embedding projected onto the main
//!   directions your catalogue varies along (PCA, whitened), then genre, type, style, decade and
//!   rating.
//! - Taste is a Gaussian belief over the weights (Bayesian logistic regression, one observation
//!   at a time), so the model knows how sure it is about every direction.
//! - Two time scales. Lasting taste comes from likes, plays and finishes, and lightly from duels.
//!   Tonight starts from lasting taste with room to move; duel votes and moods steer it, and
//!   older votes fade.
//! - Pairs are chosen by Thompson sampling: draw one plausible taste, show its favourite.
//! - Everything is replayed from a local event log, so a new projection or embedding model never
//!   loses what was learned.
//!
//! Nothing here leaves the device. The log keeps titles' features, never add-on URLs.
use crate::{backend::Media, picks_ai, picks_rules as rules};
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}, sync::Mutex};

/// Projected embedding directions.
const K: usize = 24;
/// Genres the model weighs (the app's names; a pair such as "Sci-Fi & Fantasy" counts for both).
const GENRES: [&str; 21] = ["Action", "Adventure", "Comedy", "Crime", "Documentary", "Drama", "Family", "Fantasy", "History", "Horror",
    "Music", "Mystery", "Romance", "Sci-Fi", "Thriller", "War", "Western", "Sport", "Biography", "Kids", "Politics"];
/// Decade buckets; the first holds everything before 1970.
const DECADES: [i32; 7] = [1960, 1970, 1980, 1990, 2000, 2010, 2020];
const G0: usize = K;
const KIND0: usize = G0 + GENRES.len();
const ANIM: usize = KIND0 + 2;
const DEC0: usize = ANIM + 1;
const RATING: usize = DEC0 + DECADES.len();
/// Feature dimension.
pub const D: usize = RATING + 1;

/// Whitened embedding coordinates have unit variance; this sets their weight beside the catalogue
/// features (a typical title's embedding part then has a norm near 1.7).
const EMB_SCALE: f64 = 0.35;
/// A mood is a direction of fixed strength, however long the words.
const MOOD_NORM: f64 = 1.5;
/// Prior variance of every weight, and the extra room tonight gets on top of lasting taste.
const PRIOR_VAR: f64 = 1.0;
const TONIGHT_VAR: f64 = 0.6;
/// Tonight's belief widens a little before each vote, so recent votes count more (a drifting mood).
const DRIFT: f64 = 0.03;

/// How much each signal teaches lasting taste and tonight.
const KEEP: (f64, f64) = (0.3, 1.0);
const NEITHER_TONIGHT: f64 = 0.4;
const PLAY: (f64, f64) = (1.0, 1.5);
const MOOD_TONIGHT: f64 = 1.5;
const LIKED: f64 = 1.5;
const FINISHED: f64 = 1.0;
const SAVED: f64 = 0.5;
const STARTED: f64 = 0.4;

/// Mood words: what Picks can say about tonight, and chips to steer it. Each is embedded with
/// the same model from a short description, so it lives in the titles' space.
pub const MOODS: [(&str, &str); 10] = [
    ("Funny", "a funny, light-hearted comedy full of jokes and laughs"),
    ("Dark", "a dark, gritty and bleak story about crime, violence and despair"),
    ("Tense", "a tense, suspenseful thriller with danger at every turn"),
    ("Cosy", "a cosy, warm and comforting feel-good story"),
    ("Romantic", "a romantic love story about relationships and falling in love"),
    ("Mind-bending", "a mind-bending, cerebral story with twists, puzzles and strange realities"),
    ("Epic", "an epic adventure with huge battles, quests and spectacular worlds"),
    ("Scary", "a scary horror story with monsters, ghosts and terror"),
    ("Thoughtful", "a thoughtful, moving drama about real life, family and human emotions"),
    ("Action", "an action-packed ride with fights, chases and explosions"),
];

// ---------------------------------------------------------------- titles

/// A title's identity across sources: its IMDb id, else name and year.
pub fn id(m: &Media) -> String { rules::imdb(m).unwrap_or_else(|| format!("t:{}", rules::title_key(m))) }

/// Every name a title goes by: its entry, its IMDb id (or name and year) and its name and year.
/// A title already shown under any of them counts as shown, whichever source lists it again.
pub fn identities(m: &Media) -> [String; 3] { [m.key(), id(m), format!("t:{}", rules::title_key(m))] }

/// Whether any of a title's names is in `exclude`.
pub fn excluded(m: &Media, exclude: &HashSet<&str>) -> bool { identities(m).iter().any(|i| exclude.contains(i.as_str())) }

/// What the model knows about a title (or a mood): enough to rebuild its features later.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Item {
    pub name: String,
    pub kind: String,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub animated: bool,
    pub rating: Option<f64>,
    /// Shared: the brain is copied for saving (`persist`) without copying every embedding.
    pub emb: Option<std::sync::Arc<[f32]>>,
    pub mood: bool,
}

impl Item {
    pub fn of(m: &Media) -> Self {
        Item {
            name: m.name.clone(),
            kind: if m.kind == "movie" { "movie".into() } else { "series".into() },
            year: m.first_year(),
            genres: GENRES.iter().filter(|g| rules::has_genre(m, g)).map(|g| (*g).to_owned()).collect(),
            animated: rules::is_animated(m),
            rating: ["imdbRating", "tmdbRating", "tvmazeRating"].iter().find_map(|k| {
                m.raw[*k].as_str().and_then(|r| r.trim().parse::<f64>().ok()).or_else(|| m.raw[*k].as_f64())
                    .filter(|r| r.is_finite() && *r > 0.0 && *r <= 10.0)
            }),
            emb: picks_ai::embedding(m).map(Into::into),
            mood: false,
        }
    }
    fn mood(text: &str, emb: Vec<f32>) -> Self { Item { name: text.to_owned(), kind: "mood".into(), emb: Some(emb.into()), mood: true, ..Default::default() } }
    fn encode(&self) -> Value {
        json!({"n": self.name, "k": self.kind, "y": self.year, "g": self.genres, "a": self.animated, "r": self.rating,
            "e": self.emb.as_deref().map(picks_ai::pack), "m": self.mood})
    }
    fn decode(v: &Value) -> Option<Self> {
        Some(Item {
            name: v["n"].as_str()?.to_owned(),
            kind: v["k"].as_str().unwrap_or("movie").to_owned(),
            year: v["y"].as_i64().map(|y| y as i32),
            genres: v["g"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|g| GENRES.contains(g)).map(str::to_owned).collect(),
            animated: v["a"].as_bool().unwrap_or(false),
            rating: v["r"].as_f64(),
            emb: v["e"].as_str().and_then(picks_ai::unpack).map(Into::into),
            mood: v["m"].as_bool().unwrap_or(false),
        })
    }
}

/// What the library says about a title: liked or not, finished, saved, how long it was watched.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact { pub id: String, pub item: Item, pub liked: i64, pub finished: bool, pub saved: bool, pub watched_ms: i64 }

impl Fact {
    pub fn of(m: &Media) -> Self {
        Fact { id: id(m), item: Item::of(m), liked: m.liked, finished: m.watched, saved: m.saved,
            watched_ms: m.raw["watched_ms"].as_i64().unwrap_or(0) }
    }
}

/// The library titles that say something about taste.
pub fn facts(library: &[Media]) -> Vec<Fact> {
    library.iter().filter(|m| m.liked != 0 || m.watched || m.saved || m.raw["watched_ms"].as_i64().unwrap_or(0) >= 600_000).map(Fact::of).collect()
}

// ---------------------------------------------------------------- the embedding space

/// The main directions titles differ along in embedding space, from the titles embedded so far.
/// Whitening gives every direction the same prior weight.
#[derive(Clone, Debug, PartialEq)]
pub struct Space { mean: Vec<f32>, basis: Vec<Vec<f32>>, scale: Vec<f64>, pub count: usize }

impl Space {
    /// Principal directions by subspace iteration on the covariance. None with too few titles.
    pub fn fit(vectors: &[Vec<f32>]) -> Option<Space> {
        let dim = picks_ai::DIM;
        let vectors: Vec<&Vec<f32>> = vectors.iter().filter(|v| v.len() == dim && v.iter().all(|x| x.is_finite())).collect();
        let n = vectors.len();
        if n < 16 { return None; }
        let k = K.min(n / 2);
        let mut mean = vec![0f64; dim];
        for v in &vectors { for (m, x) in mean.iter_mut().zip(v.iter()) { *m += f64::from(*x); } }
        for m in &mut mean { *m /= n as f64; }
        let mut cov = vec![0f64; dim * dim];
        let mut centered = vec![0f64; dim];
        for v in &vectors {
            for (c, (x, m)) in centered.iter_mut().zip(v.iter().zip(&mean)) { *c = f64::from(*x) - m; }
            for i in 0..dim {
                let ci = centered[i];
                if ci == 0.0 { continue; }
                let row = &mut cov[i * dim..i * dim + dim];
                for j in i..dim { row[j] += ci * centered[j]; }
            }
        }
        for i in 0..dim { for j in i..dim { let v = cov[i * dim + j] / n as f64; cov[i * dim + j] = v; cov[j * dim + i] = v; } }
        // A fixed start keeps the result reproducible; 40 rounds settle the leading directions.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let mut q: Vec<Vec<f64>> = (0..k).map(|_| (0..dim).map(|_| next()).collect()).collect();
        orthonormalize(&mut q);
        for _ in 0..40 {
            q = q.iter().map(|col| (0..dim).map(|i| dot(&cov[i * dim..i * dim + dim], col)).collect()).collect();
            orthonormalize(&mut q);
        }
        let mut pairs: Vec<(f64, Vec<f64>)> = q.into_iter().map(|col| {
            let cq: Vec<f64> = (0..dim).map(|i| dot(&cov[i * dim..i * dim + dim], &col)).collect();
            (dot(&col, &cq).max(0.0), col)
        }).collect();
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
        // Directions with next to no spread are numerical noise; whitening would blow them up.
        let top = pairs.first().map_or(0.0, |p| p.0);
        pairs.retain(|(l, _)| *l > 1e-8 && *l > top * 1e-4);
        if pairs.is_empty() { return None; }
        Some(Space {
            mean: mean.iter().map(|m| *m as f32).collect(),
            scale: pairs.iter().map(|(l, _)| 1.0 / l.sqrt()).collect(),
            basis: pairs.into_iter().map(|(_, c)| c.iter().map(|x| *x as f32).collect()).collect(),
            count: n,
        })
    }
    fn dims(&self) -> usize { self.basis.len() }
    /// Whitened coordinates of `e` around `center`.
    fn project(&self, e: &[f32], center: &[f32]) -> Vec<f64> {
        if e.len() != self.mean.len() || center.len() != self.mean.len() { return vec![0.0; self.dims()]; }
        let c: Vec<f64> = e.iter().zip(center).map(|(x, m)| f64::from(*x) - f64::from(*m)).collect();
        self.basis.iter().zip(&self.scale).map(|(b, s)| b.iter().zip(&c).map(|(x, y)| f64::from(*x) * y).sum::<f64>() * s * EMB_SCALE).collect()
    }
    fn encode(&self) -> Value {
        json!({"version": picks_ai::SPACE_VERSION, "count": self.count, "mean": picks_ai::pack(&self.mean),
            "scale": self.scale, "basis": self.basis.iter().map(|b| picks_ai::pack(b)).collect::<Vec<_>>()})
    }
    fn decode(v: &Value) -> Option<Space> {
        if v["version"] != picks_ai::SPACE_VERSION { return None; }
        let mean = picks_ai::unpack(v["mean"].as_str()?)?;
        let basis: Vec<Vec<f32>> = v["basis"].as_array()?.iter().map(|b| b.as_str().and_then(picks_ai::unpack)).collect::<Option<_>>()?;
        let scale: Vec<f64> = v["scale"].as_array()?.iter().map(Value::as_f64).collect::<Option<_>>()?;
        (basis.len() == scale.len() && !basis.is_empty() && basis.len() <= K && scale.iter().all(|s| s.is_finite() && *s > 0.0))
            .then_some(Space { mean, basis, scale, count: v["count"].as_u64().unwrap_or(0) as usize })
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 { a.iter().zip(b).map(|(x, y)| x * y).sum() }
fn norm(a: &[f64]) -> f64 { dot(a, a).sqrt() }
fn cosine(a: &[f64], b: &[f64]) -> f64 { let n = norm(a) * norm(b); if n > 1e-12 { dot(a, b) / n } else { 0.0 } }

fn orthonormalize(q: &mut [Vec<f64>]) {
    for i in 0..q.len() {
        for j in 0..i { let p = dot(&q[i], &q[j]); let (head, tail) = q.split_at_mut(i); for (x, y) in tail[0].iter_mut().zip(&head[j]) { *x -= p * y; } }
        let n = norm(&q[i]);
        if n > 1e-12 { for x in &mut q[i] { *x /= n; } } else { q[i].fill(0.0); }
    }
}

// ---------------------------------------------------------------- belief

/// A Gaussian over the taste weights: the best guess and how unsure it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Belief { mu: Vec<f64>, cov: Vec<f64> }

impl Belief {
    fn new(mean: Vec<f64>, var: f64) -> Self {
        let mut cov = vec![0.0; D * D];
        for i in 0..D { cov[i * D + i] = var; }
        Belief { mu: mean, cov }
    }
    fn mean(&self, x: &[f64]) -> f64 { dot(&self.mu, x) }
    fn sigma_x(&self, x: &[f64]) -> Vec<f64> { (0..D).map(|i| dot(&self.cov[i * D..i * D + D], x)).collect() }
    fn var(&self, x: &[f64]) -> f64 { dot(x, &self.sigma_x(x)).max(0.0) }
    /// One logistic observation (y: the title or difference scored above zero), with a weight.
    /// The online Laplace update: the mean moves along Σx, the uncertainty shrinks along it.
    fn observe(&mut self, x: &[f64], y: bool, weight: f64) {
        if weight <= 0.0 || norm(x) < 1e-9 { return; }
        let s = self.sigma_x(x);
        let v = dot(x, &s).max(0.0);
        let p = prob(self.mean(x), v);
        let h = weight * p * (1.0 - p);
        let g = weight * (if y { 1.0 } else { 0.0 } - p);
        let denom = 1.0 + h * v;
        let shrink = h / denom;
        for i in 0..D { let si = s[i]; if si == 0.0 { continue; } let row = &mut self.cov[i * D..i * D + D]; for j in 0..D { row[j] -= shrink * si * s[j]; } }
        for i in 0..D { self.mu[i] += g / denom * s[i]; }
    }
    fn widen(&mut self, q: f64) { for i in 0..D { self.cov[i * D + i] += q; } }
    /// One plausible taste drawn from the belief.
    fn sample(&self) -> Vec<f64> {
        let z: Vec<f64> = (0..D).map(|_| normal()).collect();
        match cholesky(&self.cov) {
            Some(l) => (0..D).map(|i| self.mu[i] + (0..=i).map(|j| l[i * D + j] * z[j]).sum::<f64>()).collect(),
            None => (0..D).map(|i| self.mu[i] + self.cov[i * D + i].max(0.0).sqrt() * z[i]).collect(),
        }
    }
}

/// The probit-moderated chance that a score with this mean and variance is above zero.
fn prob(mean: f64, var: f64) -> f64 { sigmoid(mean / (1.0 + std::f64::consts::PI * var / 8.0).sqrt()) }
fn sigmoid(x: f64) -> f64 { 1.0 / (1.0 + (-x.clamp(-40.0, 40.0)).exp()) }

fn cholesky(a: &[f64]) -> Option<Vec<f64>> {
    let mut l = vec![0.0; D * D];
    for i in 0..D {
        for j in 0..=i {
            let s: f64 = (0..j).map(|k| l[i * D + k] * l[j * D + k]).sum();
            if i == j {
                let d = a[i * D + i] + 1e-9 - s;
                if d <= 0.0 || !d.is_finite() { return None; }
                l[i * D + i] = d.sqrt();
            } else {
                l[i * D + j] = (a[i * D + j] - s) / l[j * D + j];
            }
        }
    }
    Some(l)
}

fn uniform() -> f64 { ((crate::backend::rand_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64 }
fn normal() -> f64 { (-2.0 * uniform().ln()).sqrt() * (std::f64::consts::TAU * uniform()).cos() }

// ---------------------------------------------------------------- events

/// Something you did in Picks. Likes, plays outside Picks and finishes come from the library.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// Kept `won` over `lost` in a duel.
    Keep { won: String, lost: String },
    /// Neither card: both below what tonight wants.
    Neither(String, String),
    /// Played from Picks: the strongest yes.
    Play(String),
    /// A mood turned on or off tonight (its id, "mood:<words>").
    Mood(String),
    Unmood(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event { pub at: i64, pub session: u64, pub signal: Signal }

impl Event {
    fn encode(&self) -> Value {
        let (k, a, b) = match &self.signal {
            Signal::Keep { won, lost } => ("keep", won, Some(lost)),
            Signal::Neither(a, b) => ("neither", a, Some(b)),
            Signal::Play(a) => ("play", a, None),
            Signal::Mood(a) => ("mood", a, None),
            Signal::Unmood(a) => ("unmood", a, None),
        };
        json!({"t": self.at, "s": self.session.to_string(), "k": k, "a": a, "b": b})
    }
    fn decode(v: &Value) -> Option<Self> {
        let a = v["a"].as_str()?.to_owned();
        let b = v["b"].as_str().map(str::to_owned);
        let signal = match v["k"].as_str()? {
            "keep" => Signal::Keep { won: a, lost: b? },
            "neither" => Signal::Neither(a, b?),
            "play" => Signal::Play(a),
            "mood" => Signal::Mood(a),
            "unmood" => Signal::Unmood(a),
            _ => return None,
        };
        Some(Event { at: v["t"].as_i64().unwrap_or(0), session: v["s"].as_str()?.parse().ok()?, signal })
    }
    fn ids(&self) -> Vec<&String> {
        match &self.signal { Signal::Keep { won, lost } => vec![won, lost], Signal::Neither(a, b) => vec![a, b],
            Signal::Play(a) | Signal::Mood(a) | Signal::Unmood(a) => vec![a] }
    }
}

// ---------------------------------------------------------------- the brain

/// Why a card is on screen, in words Picks can show.
#[derive(Clone, Debug, PartialEq)]
pub enum Why { Kept(String), Liked(String), Mood(String), Explore, Fits }

impl Why {
    pub fn text(&self) -> String {
        match self {
            Why::Kept(n) => format!("Close to {n}, which you kept"),
            Why::Liked(n) => format!("Close to {n}, which you liked"),
            Why::Mood(m) => format!("Fits tonight's {} mood", m.to_lowercase()),
            Why::Explore => "Something different, to test your taste".into(),
            Why::Fits => "Fits your taste so far".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Brain {
    pub space: Option<Space>,
    items: HashMap<String, Item>,
    events: Vec<Event>,
    facts: Vec<Fact>,
    prior: Vec<f64>,
    anchors: Vec<(String, Vec<f32>)>,
    session: u64,
    long: Belief,
    tonight: Belief,
    /// Items added since the last save.
    pub dirty: bool,
}

impl Default for Brain { fn default() -> Self { Brain::new(None, HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], Vec::new(), 0) } }

impl Brain {
    pub fn new(space: Option<Space>, items: HashMap<String, Item>, events: Vec<Event>, facts: Vec<Fact>, prior: Vec<f64>, anchors: Vec<(String, Vec<f32>)>, session: u64) -> Self {
        let prior = if prior.len() == D { prior } else { vec![0.0; D] };
        let mut b = Brain { space, items, events, facts, long: Belief::new(prior.clone(), PRIOR_VAR), tonight: Belief::new(prior.clone(), PRIOR_VAR + TONIGHT_VAR),
            prior, anchors, session, dirty: false };
        b.rebuild();
        b
    }

    /// The anchors' average: queries share an offset that says nothing about mood.
    fn query_center(&self) -> Option<Vec<f32>> {
        if self.anchors.is_empty() { return None; }
        let dim = self.anchors[0].1.len();
        let mut c = vec![0f32; dim];
        for (_, a) in &self.anchors { for (x, y) in c.iter_mut().zip(a) { *x += y; } }
        for x in &mut c { *x /= self.anchors.len() as f32; }
        Some(c)
    }

    fn x(&self, item: &Item) -> Vec<f64> {
        let mut x = vec![0.0; D];
        if let (Some(space), Some(e)) = (&self.space, &item.emb) {
            // Titles centre on the catalogue's mean (borrowed, not copied: this runs for every title scored).
            let query_center = if item.mood { self.query_center() } else { None };
            let mut z = space.project(e, query_center.as_deref().unwrap_or(&space.mean));
            if item.mood { let n = norm(&z); if n > 1e-9 { for v in &mut z { *v *= MOOD_NORM / n; } } }
            x[..z.len()].copy_from_slice(&z);
        }
        if item.mood { return x; }
        let n = (item.genres.len().max(1) as f64).sqrt();
        for g in &item.genres { if let Some(i) = GENRES.iter().position(|x| x == g) { x[G0 + i] = 1.0 / n; } }
        x[KIND0 + usize::from(item.kind != "movie")] = 0.5;
        if item.animated { x[ANIM] = 0.6; }
        if let Some(y) = item.year { let i = DECADES.iter().rposition(|d| y >= *d).unwrap_or(0); x[DEC0 + i] = 0.5; }
        if let Some(r) = item.rating { x[RATING] = ((r - 6.8) / 1.2).clamp(-2.0, 2.0) * 0.4; }
        x
    }
    pub fn features(&self, m: &Media) -> Vec<f64> {
        let mut item = Item::of(m);
        if item.emb.is_none() { item.emb = self.items.get(&id(m)).and_then(|i| i.emb.clone()); }
        self.x(&item)
    }
    fn x_id(&self, id: &str) -> Option<Vec<f64>> { self.items.get(id).map(|i| self.x(i)) }

    /// Replay: lasting taste from the old profile, the library and earlier nights; tonight from
    /// that with room to move, then tonight's moods and votes in order.
    fn rebuild(&mut self) {
        let mut base = Belief::new(self.prior.clone(), PRIOR_VAR);
        for f in &self.facts {
            let x = self.x(&f.item);
            if f.liked > 0 { base.observe(&x, true, LIKED); }
            if f.liked < 0 { base.observe(&x, false, LIKED); }
            if f.finished { base.observe(&x, true, FINISHED); }
            else if f.watched_ms >= 600_000 { base.observe(&x, true, STARTED); }
            if f.saved { base.observe(&x, true, SAVED); }
        }
        let (earlier, tonight): (Vec<&Event>, Vec<&Event>) = self.events.iter().partition(|e| e.session != self.session);
        for e in earlier { self.teach(&mut base, e, true); }
        let mut long = base.clone();
        let mut night = base;
        night.widen(TONIGHT_VAR);
        for mood in active_moods(&tonight) { if let Some(x) = self.x_id(&mood) { night.observe(&x, true, MOOD_TONIGHT); } }
        for e in &tonight {
            self.teach(&mut long, e, true);
            if !matches!(e.signal, Signal::Mood(_) | Signal::Unmood(_)) { night.widen(DRIFT); self.teach(&mut night, e, false); }
        }
        self.long = long;
        self.tonight = night;
    }

    /// One event's lesson for lasting taste (`lasting`) or tonight. Moods are handled by replay.
    fn teach(&self, b: &mut Belief, e: &Event, lasting: bool) {
        let pick = |w: (f64, f64)| if lasting { w.0 } else { w.1 };
        match &e.signal {
            Signal::Keep { won, lost } => if let (Some(a), Some(c)) = (self.x_id(won), self.x_id(lost)) {
                let d: Vec<f64> = a.iter().zip(&c).map(|(x, y)| x - y).collect();
                b.observe(&d, true, pick(KEEP));
            },
            Signal::Neither(a, c) => if !lasting { for id in [a, c] { if let Some(x) = self.x_id(id) { b.observe(&x, false, NEITHER_TONIGHT); } } },
            Signal::Play(a) => if let Some(x) = self.x_id(a) { b.observe(&x, true, pick(PLAY)); },
            Signal::Mood(_) | Signal::Unmood(_) => {}
        }
    }

    fn remember(&mut self, m: &Media) -> String {
        let key = id(m);
        let mut item = Item::of(m);
        if item.emb.is_none() { item.emb = self.items.get(&key).and_then(|i| i.emb.clone()); }
        if self.items.get(&key) != Some(&item) { self.items.insert(key.clone(), item); self.dirty = true; }
        key
    }

    /// Learn from a duel vote, Neither or a play, now; the event is returned for the log.
    pub fn keep(&mut self, won: &Media, lost: &Media) -> Event { let (w, l) = (self.remember(won), self.remember(lost)); self.learn(Signal::Keep { won: w, lost: l }) }
    pub fn neither(&mut self, a: &Media, b: &Media) -> Event { let (x, y) = (self.remember(a), self.remember(b)); self.learn(Signal::Neither(x, y)) }
    pub fn play(&mut self, m: &Media) -> Event { let x = self.remember(m); self.learn(Signal::Play(x)) }
    fn learn(&mut self, signal: Signal) -> Event {
        let e = Event { at: crate::backend::now_secs(), session: self.session, signal };
        let (mut long, mut night) = (self.long.clone(), self.tonight.clone());
        self.teach(&mut long, &e, true);
        night.widen(DRIFT);
        self.teach(&mut night, &e, false);
        self.long = long;
        self.tonight = night;
        self.events.push(e.clone());
        e
    }
    /// Turn a mood on (with the words' embedding) or off; tonight is replayed.
    pub fn set_mood(&mut self, words: &str, emb: Option<Vec<f32>>) -> Event {
        let key = mood_id(words);
        let signal = match emb {
            Some(e) => { let item = Item::mood(words, e); if self.items.get(&key) != Some(&item) { self.items.insert(key.clone(), item); self.dirty = true; } Signal::Mood(key) }
            None => Signal::Unmood(key),
        };
        let e = Event { at: crate::backend::now_secs(), session: self.session, signal };
        self.events.push(e.clone());
        self.rebuild();
        e
    }
    /// Moods on tonight, as their words.
    pub fn moods(&self) -> Vec<String> {
        let tonight: Vec<&Event> = self.events.iter().filter(|e| e.session == self.session).collect();
        active_moods(&tonight).into_iter().map(|k| self.items.get(&k).map(|i| i.name.clone()).unwrap_or_else(|| k.trim_start_matches("mood:").to_owned())).collect()
    }

    pub fn set_facts(&mut self, facts: Vec<Fact>) {
        // A title's embedding outlives the embedding cache in the brain's own store.
        let facts: Vec<Fact> = facts.into_iter().map(|mut f| {
            match (&f.item.emb, self.items.get(&f.id).and_then(|i| i.emb.clone())) {
                (None, Some(e)) => f.item.emb = Some(e),
                (Some(_), stored) if stored.is_none() && (f.liked != 0 || f.finished || f.saved) => { self.items.insert(f.id.clone(), f.item.clone()); self.dirty = true; }
                _ => {}
            }
            f
        }).collect();
        if facts != self.facts { self.facts = facts; self.rebuild(); }
    }
    pub fn set_space(&mut self, space: Option<Space>) { if space != self.space { self.space = space; self.rebuild(); } }
    pub fn set_anchors(&mut self, anchors: Vec<(String, Vec<f32>)>) { if anchors != self.anchors { self.anchors = anchors; self.rebuild(); } }
    pub fn set_session(&mut self, session: u64) { if session != self.session { self.session = session; self.rebuild(); } }
    /// Fill in embeddings computed since a title was remembered.
    pub fn refresh_items(&mut self, pool: &[Media]) {
        let mut changed = false;
        for m in pool {
            let key = id(m);
            if self.items.get(&key).is_some_and(|i| i.emb.is_none()) {
                if let Some(e) = picks_ai::embedding(m) { if let Some(i) = self.items.get_mut(&key) { i.emb = Some(e.into()); changed = true; } }
            }
        }
        if changed { self.dirty = true; self.rebuild(); }
    }
    pub fn votes_tonight(&self) -> usize { self.events.iter().filter(|e| e.session == self.session && matches!(e.signal, Signal::Keep { .. })).count() }

    // ------------------------------------------------------------ choosing

    /// Thompson sampling: the favourite of one plausible taste, away from `exclude` and from
    /// near-copies of `against` (a vote between near-copies teaches nothing).
    fn draw(&self, pool: &[Media], exclude: &[String], against: Option<&Media>) -> Option<Media> {
        let w = self.tonight.sample();
        let a_emb = against.and_then(picks_ai::embedding);
        let mut skip: HashSet<&str> = exclude.iter().map(String::as_str).collect();
        let a_names = against.map(identities);
        if let Some(names) = &a_names { skip.extend(names.iter().map(String::as_str)); }
        pool.iter()
            .filter(|m| !excluded(m, &skip))
            .filter(|m| match (&a_emb, picks_ai::embedding(m)) { (Some(a), Some(b)) => picks_ai::cosine(a, &b) < 0.97, _ => true })
            .map(|m| (dot(&w, &self.features(m)) + 1e-9 * uniform(), m))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, m)| m.clone())
    }
    /// Two cards, each the favourite of its own plausible taste.
    pub fn opening_pair(&self, pool: &[Media], exclude: &[String]) -> (Option<Media>, Option<Media>) {
        let first = self.draw(pool, exclude, None);
        let second = first.as_ref().and_then(|a| self.draw(pool, exclude, Some(a)));
        (first, second)
    }
    /// The strongest plausible rival for the card you kept.
    pub fn challenger(&self, pool: &[Media], exclude: &[String], kept: &Media) -> Option<Media> { self.draw(pool, exclude, Some(kept)) }

    // ------------------------------------------------------------ reading the model

    /// Average features of a list: the "typical title" a match is measured against.
    pub fn reference(&self, pool: &[Media]) -> Vec<f64> {
        let mut r = vec![0.0; D];
        let take: Vec<&Media> = pool.iter().take(120).collect();
        for m in &take { for (a, b) in r.iter_mut().zip(self.features(m)) { *a += b; } }
        if !take.is_empty() { for a in &mut r { *a /= take.len() as f64; } }
        r
    }
    /// How likely tonight you'd rather watch this than a typical title in the pool (0–1).
    pub fn match_of(&self, m: &Media, reference: &[f64]) -> f64 {
        let x: Vec<f64> = self.features(m).iter().zip(reference).map(|(a, b)| a - b).collect();
        prob(self.tonight.mean(&x), self.tonight.var(&x))
    }
    pub fn score(&self, m: &Media) -> f64 { self.tonight.mean(&self.features(m)) }
    pub fn lasting(&self, m: &Media) -> f64 { self.long.mean(&self.features(m)) }
    /// How much tonight's uncertainty has shrunk over the pool, 0 (nothing known) to 1.
    pub fn learning(&self, pool: &[Media], reference: &[f64]) -> f64 {
        let (mut now, mut before) = (0.0, 0.0);
        for m in pool.iter().take(60) {
            let x: Vec<f64> = self.features(m).iter().zip(reference).map(|(a, b)| a - b).collect();
            now += self.tonight.var(&x);
            before += dot(&x, &x) * (PRIOR_VAR + TONIGHT_VAR);
        }
        if before <= 1e-9 { 0.0 } else { (1.0 - now / before).clamp(0.0, 1.0) }
    }
    /// Titles in order of tonight's taste.
    pub fn ranked(&self, titles: &[Media]) -> Vec<Media> {
        let mut scored: Vec<(f64, &Media)> = titles.iter().map(|m| (self.score(m), m)).collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().map(|(_, m)| m.clone()).collect()
    }

    fn anchor_dirs(&self) -> Vec<(String, Vec<f64>)> {
        let (Some(space), Some(center)) = (&self.space, self.query_center()) else { return Vec::new() };
        self.anchors.iter().map(|(label, e)| (label.clone(), space.project(e, &center))).collect()
    }
    fn emb_part<'a>(&self, x: &'a [f64]) -> &'a [f64] { &x[..self.space.as_ref().map_or(0, Space::dims)] }
    /// Which moods tonight leans to, strongest first (only once the votes say something).
    pub fn leaning(&self) -> Vec<(String, f64)> {
        let taste = self.emb_part(&self.tonight.mu).to_vec();
        if norm(&taste) < 0.15 { return Vec::new(); }
        let mut out: Vec<(String, f64)> = self.anchor_dirs().into_iter().map(|(l, a)| (l, cosine(&taste, &a))).filter(|(_, c)| *c > 0.12).collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out.truncate(3);
        out
    }
    /// The moods a title itself carries, strongest first.
    pub fn feels(&self, m: &Media) -> Vec<String> {
        let x = self.features(m);
        let e = self.emb_part(&x);
        if norm(e) < 1e-6 { return Vec::new(); }
        let mut out: Vec<(String, f64)> = self.anchor_dirs().into_iter().map(|(l, a)| (l, cosine(e, &a))).filter(|(_, c)| *c > 0.2).collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out.into_iter().take(2).map(|(l, _)| l).collect()
    }
    /// The reason to show beside a card: the nearest title you kept or liked, tonight's mood,
    /// exploration, or a plain fit.
    pub fn why(&self, m: &Media, kept: &[Media]) -> Option<Why> {
        let x = self.features(m);
        let e = self.emb_part(&x).to_vec();
        if norm(&e) > 1e-6 {
            let me = rules::title_key(m);
            let near = |x: &[f64]| cosine(&e, self.emb_part(x));
            let best_kept = kept.iter().filter(|k| rules::title_key(k) != me).map(|k| (near(&self.features(k)), k.name.clone())).max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, name)) = best_kept.filter(|(c, _)| *c > 0.5) { return Some(Why::Kept(name)); }
            let best_liked = self.facts.iter().filter(|f| f.liked > 0 && f.item.name != m.name).map(|f| (near(&self.x(&f.item)), f.item.name.clone())).max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, name)) = best_liked.filter(|(c, _)| *c > 0.5) { return Some(Why::Liked(name)); }
            if let Some((mood, _)) = self.leaning().first() { if self.feels(m).contains(mood) { return Some(Why::Mood(mood.clone())); } }
        }
        let v = self.tonight.var(&x);
        let before = dot(&x, &x) * (PRIOR_VAR + TONIGHT_VAR);
        if before > 1e-9 && v / before > 0.75 && self.votes_tonight() > 0 { return Some(Why::Explore); }
        (self.score(m) > 0.3).then_some(Why::Fits)
    }
    /// A reason for Home from lasting taste: the liked or finished title it is closest to.
    pub fn home_reason(&self, m: &Media) -> Option<String> {
        let x = self.features(m);
        let e = self.emb_part(&x).to_vec();
        if norm(&e) < 1e-6 { return None; }
        self.facts.iter().filter(|f| (f.liked > 0 || f.finished) && f.item.name != m.name)
            .map(|f| (cosine(&e, self.emb_part(&self.x(&f.item))), f))
            .filter(|(c, _)| *c > 0.5).max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, f)| if f.liked > 0 { format!("Close to {}, which you liked", f.item.name) } else { format!("Close to {}, which you watched", f.item.name) })
    }
}

fn mood_id(words: &str) -> String { format!("mood:{}", words.trim().to_lowercase()) }

/// Moods still on at the end of these events, in the order they were turned on.
fn active_moods(events: &[&Event]) -> Vec<String> {
    let mut on: Vec<String> = Vec::new();
    for e in events {
        match &e.signal {
            Signal::Mood(k) => { if !on.contains(k) { on.push(k.clone()); } }
            Signal::Unmood(k) => on.retain(|x| x != k),
            _ => {}
        }
    }
    on
}

// ---------------------------------------------------------------- storage

const ITEMS_FILE: &str = "picks-brain-items.json";
const EVENTS_FILE: &str = "picks-brain-events.jsonl";
const SPACE_FILE: &str = "picks-brain-space.json";
/// The profile of the earlier duel model: its genre, type and decade weights seed lasting taste.
const OLD_TASTE_FILE: &str = "picks-taste.json";
const MAX_EVENTS: usize = 3000;
const MAX_ITEMS: usize = 4000;

fn load_items(dir: &Path) -> HashMap<String, Item> {
    let v: Value = std::fs::read_to_string(dir.join(ITEMS_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    // An embedding only means something in the space it was made in.
    let same_space = v["space"] == picks_ai::SPACE_VERSION;
    v["items"].as_object().into_iter().flatten().filter_map(|(k, i)| {
        let mut item = Item::decode(i)?;
        if !same_space || item.emb.as_ref().is_some_and(|e| e.len() != picks_ai::DIM) { item.emb = None; }
        Some((k.clone(), item))
    }).collect()
}

fn items_value(items: &HashMap<String, Item>, events: &[Event], facts: &[Fact]) -> Value {
    // Past the cap, keep what the log and the library still use.
    let keep: Option<HashSet<&String>> = (items.len() > MAX_ITEMS).then(|| events.iter().flat_map(Event::ids).chain(facts.iter().map(|f| &f.id)).collect());
    let map: serde_json::Map<String, Value> = items.iter().filter(|(k, _)| keep.as_ref().is_none_or(|s| s.contains(k)))
        .map(|(k, i)| (k.clone(), i.encode())).collect();
    json!({"version": 1, "space": picks_ai::SPACE_VERSION, "items": map})
}

fn load_events(dir: &Path) -> Vec<Event> {
    let path = dir.join(EVENTS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let mut events: Vec<Event> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| Event::decode(&v)).collect();
    if events.len() > MAX_EVENTS * 2 {
        events.drain(..events.len() - MAX_EVENTS);
        let body: String = events.iter().map(|e| format!("{}\n", e.encode())).collect();
        let _ = std::fs::write(&path, body);
    } else if events.len() > MAX_EVENTS {
        events.drain(..events.len() - MAX_EVENTS);
    }
    events
}

fn append_event(dir: &Path, e: &Event) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(EVENTS_FILE)) { let _ = writeln!(f, "{}", e.encode()); }
}

fn load_prior(dir: &Path) -> Vec<f64> {
    let mut prior = vec![0.0; D];
    let Some(v) = std::fs::read_to_string(dir.join(OLD_TASTE_FILE)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return prior };
    let weight = |w: f64| (w / 4.0).clamp(-1.0, 1.0);
    for (g, w) in v["genres"].as_object().into_iter().flatten() {
        let name = rules::canonical_genre(g);
        if let (Some(i), Some(w)) = (GENRES.iter().position(|x| x.eq_ignore_ascii_case(&name)), w.as_f64()) { prior[G0 + i] = weight(w); }
    }
    for (k, w) in v["kinds"].as_object().into_iter().flatten() {
        if let Some(w) = w.as_f64() { prior[KIND0 + usize::from(k != "movie")] += weight(w); }
    }
    for (d, w) in v["decades"].as_object().into_iter().flatten() {
        if let (Some(y), Some(w)) = (d.trim_end_matches('s').parse::<i32>().ok(), w.as_f64()) {
            prior[DEC0 + DECADES.iter().rposition(|x| y >= *x).unwrap_or(0)] = weight(w);
        }
    }
    prior
}

fn load_space(dir: &Path) -> Option<Space> {
    std::fs::read_to_string(dir.join(SPACE_FILE)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| Space::decode(&v))
}

/// Everything the brain needs, from a folder (the app's data folder, or a test's own).
pub fn load_from(dir: &Path, session: u64) -> Brain {
    let anchors = MOODS.iter().filter_map(|(label, phrase)| picks_ai::query_embedding(phrase).map(|e| ((*label).to_owned(), e))).collect();
    Brain::new(load_space(dir), load_items(dir), load_events(dir), Vec::new(), load_prior(dir), anchors, session)
}

fn dir() -> PathBuf { crate::backend::backend_data_dir() }

/// The brain last read from disk, and its files' sizes and times then: Home and Picks load it on
/// every visit, and reading thousands of titles again is only needed once the files changed.
type FilesPrint = Vec<Option<(u64, std::time::SystemTime)>>;
static LOADED: Mutex<Option<(FilesPrint, Brain)>> = Mutex::new(None);

fn files_print(dir: &Path) -> FilesPrint {
    [ITEMS_FILE, EVENTS_FILE, SPACE_FILE, OLD_TASTE_FILE].iter()
        .map(|f| std::fs::metadata(dir.join(f)).ok().and_then(|m| Some((m.len(), m.modified().ok()?)))).collect()
}

pub fn load(session: u64) -> Brain {
    let dir = dir();
    let print = files_print(&dir);
    let cached = LOADED.lock().ok().and_then(|l| l.as_ref().filter(|(p, _)| *p == print).map(|(_, b)| b.clone()));
    let Some(mut b) = cached else {
        let b = load_from(&dir, session);
        if let Ok(mut l) = LOADED.lock() { *l = Some((print, b.clone())); }
        return b;
    };
    // The mood words may have been embedded since, and the night may differ: one replay for both.
    let anchors: Vec<(String, Vec<f32>)> = MOODS.iter().filter_map(|(label, phrase)| picks_ai::query_embedding(phrase).map(|e| ((*label).to_owned(), e))).collect();
    if anchors != b.anchors || session != b.session { b.anchors = anchors; b.session = session; b.rebuild(); }
    b
}

/// The title store waiting to be written: the titles (their embeddings shared, not copied) and,
/// past the cap, the log and library facts that decide which titles are kept.
type PendingItems = (HashMap<String, Item>, Vec<Event>, Vec<Fact>);
static PENDING_ITEMS: Mutex<Option<PendingItems>> = Mutex::new(None);

/// Log an event at once; the title store is written a few seconds later, one write for a burst,
/// and encoded there too (thousands of embeddings): a vote never waits for it.
pub fn persist(brain: &mut Brain, event: Option<&Event>) {
    let dir = dir();
    if let Some(e) = event { append_event(&dir, e); }
    if !brain.dirty { return; }
    brain.dirty = false;
    // Only past the cap does the encoding need the log and the library (what to keep).
    let over = brain.items.len() > MAX_ITEMS;
    let snapshot = (brain.items.clone(), if over { brain.events.clone() } else { Vec::new() }, if over { brain.facts.clone() } else { Vec::new() });
    let first = PENDING_ITEMS.lock().map(|mut p| p.replace(snapshot).is_none()).unwrap_or(false);
    if first {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if let Some((items, events, facts)) = PENDING_ITEMS.lock().ok().and_then(|mut p| p.take()) {
                let _ = std::fs::write(dir.join(ITEMS_FILE), items_value(&items, &events, &facts).to_string());
            }
        });
    }
}

/// A projection fitted to the titles embedded so far, when there is none yet or the catalogue
/// has grown well past the one in use. Runs off the UI thread.
pub async fn refresh_space(current: Option<Space>, items: Vec<Vec<f32>>) -> Option<Space> {
    let dir = dir();
    crate::backend::on_runtime(async move {
        tokio::task::spawn_blocking(move || {
            let mut vectors = picks_ai::all_embeddings();
            vectors.extend(items);
            let n = vectors.len();
            let stale = match &current { None => n >= 16, Some(s) => n >= s.count + 40 && n * 2 >= s.count * 3 };
            if !stale { return None; }
            let space = Space::fit(&vectors)?;
            let _ = std::fs::write(dir.join(SPACE_FILE), space.encode().to_string());
            Some(space)
        }).await.ok().flatten()
    }).await.ok().flatten()
}

impl Brain {
    /// Embeddings the brain keeps for titles, for fitting the projection.
    pub fn item_embeddings(&self) -> Vec<Vec<f32>> { self.items.values().filter(|i| !i.mood).filter_map(|i| i.emb.as_deref().map(<[f32]>::to_vec)).collect() }
}

/// Mood words embedded by the local model (once; cached with the titles' embeddings).
pub async fn anchors() -> Vec<(String, Vec<f32>)> {
    let mut out = Vec::new();
    for (label, phrase) in MOODS {
        if let Ok(e) = picks_ai::embed_query(phrase).await { out.push((label.to_owned(), e)); }
    }
    out
}

/// Forget everything Picks learned: the log, the titles and the earlier profile.
pub fn reset() {
    let dir = dir();
    if let Ok(mut p) = PENDING_ITEMS.lock() { p.take(); }
    for f in [ITEMS_FILE, EVENTS_FILE, OLD_TASTE_FILE, "picks-duel.json"] { let _ = std::fs::remove_file(dir.join(f)); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(name: &str, kind: &str, year: &str, genres: &[&str]) -> Media {
        rules::tidy(Media::from_json(&json!({"id": format!("brain-test:{name}"), "type": kind, "name": name, "releaseInfo": year, "genres": genres,
            "description": format!("An isolated brain test synopsis with enough detail for {name}.")}), "fixture").unwrap())
    }
    /// A unit vector mostly along `main`, with a little of `side`.
    fn axis(main: usize, side: usize, amount: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; picks_ai::DIM];
        v[main] = 1.0;
        v[side] += amount;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }
    /// A space where directions 0..8 vary across titles.
    fn space() -> Space {
        let vectors: Vec<Vec<f32>> = (0..64).map(|i| axis(i % 8, 8 + (i % 5), 0.3 + (i % 3) as f32 * 0.1)).collect();
        Space::fit(&vectors).unwrap()
    }
    fn embedded(name: &str, main: usize, side: usize) -> Media {
        let m = media(name, "series", "2020", &["Drama"]);
        picks_ai::cache_test_embedding(&m, axis(main, side, 0.2));
        m
    }

    #[test]
    fn space_finds_the_directions_titles_vary_along() {
        let s = space();
        assert!(s.dims() >= 8);
        let a = s.project(&axis(0, 8, 0.2), &s.mean);
        let b = s.project(&axis(1, 8, 0.2), &s.mean);
        assert!(cosine(&a, &b) < 0.5, "different titles stay apart: {}", cosine(&a, &b));
        assert_eq!(Space::decode(&s.encode()).map(|d| d.dims()), Some(s.dims()));
        assert!(Space::fit(&[axis(0, 1, 0.1)]).is_none());
    }

    #[test]
    fn belief_learns_a_preference_and_grows_surer() {
        let mut b = Belief::new(vec![0.0; D], PRIOR_VAR);
        let mut x = vec![0.0; D];
        x[3] = 1.0;
        let before = b.var(&x);
        for _ in 0..20 { b.observe(&x, true, 1.0); }
        assert!(b.mean(&x) > 1.0 && b.var(&x) < before / 3.0);
        assert!(b.mu.iter().chain(&b.cov).all(|v| v.is_finite()));
        // Thousands of identical votes stay bounded.
        for _ in 0..5000 { b.observe(&x, true, 1.0); }
        assert!(b.mean(&x).is_finite() && b.mean(&x) < 40.0);
        assert!(cholesky(&b.cov).is_some());
    }

    #[test]
    fn duels_steer_tonight_toward_what_is_kept() {
        let mut brain = Brain::new(Some(space()), HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], Vec::new(), 7);
        let dark: Vec<Media> = (0..6).map(|i| embedded(&format!("steer dark {i}"), 0, 8 + i % 4)).collect();
        let light: Vec<Media> = (0..6).map(|i| embedded(&format!("steer light {i}"), 1, 8 + i % 4)).collect();
        for (d, l) in dark.iter().zip(&light) { brain.keep(d, l); }
        let (fresh_dark, fresh_light) = (embedded("steer dark fresh", 0, 12), embedded("steer light fresh", 1, 12));
        assert!(brain.score(&fresh_dark) > brain.score(&fresh_light));
        let pool = vec![fresh_dark.clone(), fresh_light.clone()];
        let reference = brain.reference(&pool);
        assert!(brain.match_of(&fresh_dark, &reference) > 0.6);
        // Thompson sampling mostly follows the votes, yet still explores.
        let kept = embedded("steer kept", 0, 9);
        let wins = (0..200).filter(|_| brain.challenger(&pool, &[], &kept).map(|m| m.id) == Some(fresh_dark.id.clone())).count();
        assert!(wins > 140, "dark challenger {wins}/200");
        assert!(brain.learning(&pool, &reference) > 0.05);
        // Duels count lightly for lasting taste, fully for tonight.
        assert!(brain.score(&fresh_dark) - brain.score(&fresh_light) > brain.lasting(&fresh_dark) - brain.lasting(&fresh_light));
    }

    #[test]
    fn replay_rebuilds_the_same_taste_and_a_new_night_starts_fresh() {
        let mut brain = Brain::new(Some(space()), HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], Vec::new(), 1);
        let (a, b) = (embedded("replay a", 2, 9), embedded("replay b", 3, 9));
        for _ in 0..4 { brain.keep(&a, &b); }
        brain.play(&a);
        let replayed = Brain::new(brain.space.clone(), brain.items.clone(), brain.events.clone(), Vec::new(), vec![0.0; D], Vec::new(), 1);
        assert!((replayed.score(&a) - brain.score(&a)).abs() < 1e-6);
        let mut next = replayed.clone();
        next.set_session(2);
        assert!(next.score(&a) - next.score(&b) < brain.score(&a) - brain.score(&b), "a new night keeps lasting taste, not tonight's");
        assert!(next.lasting(&a) > next.lasting(&b));
        assert_eq!(next.votes_tonight(), 0);
    }

    #[test]
    fn events_and_items_round_trip_in_an_isolated_store() {
        let dir = std::env::temp_dir().join(format!("veylo-brain-{}-{}", std::process::id(), crate::backend::rand_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut brain = Brain::new(None, HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], Vec::new(), 42);
        let (a, b) = (media("store a", "movie", "1994", &["Crime"]), media("store b", "series", "2019", &["Comedy"]));
        let events = vec![brain.keep(&a, &b), brain.neither(&a, &b), brain.play(&a)];
        for e in &events { append_event(&dir, e); }
        std::fs::write(dir.join(ITEMS_FILE), items_value(&brain.items, &brain.events, &[]).to_string()).unwrap();
        let loaded = Brain::new(None, load_items(&dir), load_events(&dir), Vec::new(), load_prior(&dir), Vec::new(), 42);
        assert_eq!(loaded.events, brain.events);
        assert!((loaded.score(&a) - brain.score(&a)).abs() < 1e-9);
        // Nothing that names a provider is stored.
        let text = std::fs::read_to_string(dir.join(ITEMS_FILE)).unwrap() + &std::fs::read_to_string(dir.join(EVENTS_FILE)).unwrap();
        assert!(!text.contains("fixture") && !text.contains("http"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_old_profile_seeds_lasting_taste() {
        let dir = std::env::temp_dir().join(format!("veylo-brain-old-{}-{}", std::process::id(), crate::backend::rand_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OLD_TASTE_FILE), json!({"genres": {"horror": 3.0, "comedy": -2.0}, "kinds": {"movie": 1.0}, "decades": {"1980s": 2.0}}).to_string()).unwrap();
        let brain = Brain::new(None, HashMap::new(), Vec::new(), Vec::new(), load_prior(&dir), Vec::new(), 1);
        assert!(brain.lasting(&media("old horror", "movie", "1984", &["Horror"])) > brain.lasting(&media("old comedy", "movie", "1984", &["Comedy"])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn library_likes_and_dislikes_shape_lasting_taste() {
        let mut brain = Brain::new(Some(space()), HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], Vec::new(), 1);
        let liked = embedded("library liked", 4, 10);
        let disliked = embedded("library disliked", 5, 10);
        let mut a = Fact::of(&liked); a.liked = 1;
        let mut b = Fact::of(&disliked); b.liked = -1;
        brain.set_facts(vec![a, b]);
        let (near_liked, near_disliked) = (embedded("library near liked", 4, 11), embedded("library near disliked", 5, 11));
        assert!(brain.lasting(&near_liked) > brain.lasting(&near_disliked));
        assert!(brain.score(&near_liked) > brain.score(&near_disliked), "tonight starts from lasting taste");
    }

    #[test]
    fn moods_steer_tonight_and_can_be_turned_off() {
        let s = space();
        let anchors = vec![("Dark".to_owned(), axis(0, 9, 0.1)), ("Funny".to_owned(), axis(1, 9, 0.1)), ("Cosy".to_owned(), axis(2, 9, 0.1))];
        let mut brain = Brain::new(Some(s), HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], anchors, 3);
        let (dark, funny) = (embedded("mood dark", 0, 12), embedded("mood funny", 1, 12));
        let before = brain.score(&dark) - brain.score(&funny);
        brain.set_mood("Dark", Some(axis(0, 9, 0.1)));
        assert_eq!(brain.moods(), ["Dark"]);
        assert!(brain.score(&dark) - brain.score(&funny) > before + 0.2);
        assert_eq!(brain.leaning().first().map(|l| l.0.as_str()), Some("Dark"));
        assert!(brain.feels(&dark).contains(&"Dark".to_owned()));
        brain.set_mood("Dark", None);
        assert!(brain.moods().is_empty());
        assert!((brain.score(&dark) - brain.score(&funny) - before).abs() < 1e-6);
    }

    #[test]
    fn reasons_name_the_nearest_kept_title() {
        let mut brain = Brain::new(Some(space()), HashMap::new(), Vec::new(), Vec::new(), vec![0.0; D], Vec::new(), 1);
        let kept = embedded("why kept", 6, 9);
        let near = embedded("why near", 6, 10);
        let far = embedded("why far", 7, 10);
        brain.keep(&kept, &far);
        assert_eq!(brain.why(&near, std::slice::from_ref(&kept)), Some(Why::Kept("why kept".into())));
        assert_ne!(brain.why(&far, std::slice::from_ref(&kept)), Some(Why::Kept("why kept".into())));
    }

    #[test]
    fn a_title_shown_from_one_source_is_not_drawn_again_from_another() {
        let brain = Brain::default();
        let from = |addon: &str, year: &str| Media::from_json(&json!({"id": "tt0111161", "type": "movie", "name": "Same Film", "releaseInfo": year}), addon).unwrap();
        let (shown, other_source, other_year) = (from("https://a.invalid/manifest.json", "1994"), from("https://b.invalid/manifest.json", "1994"), from("https://c.invalid/manifest.json", "1995"));
        let fresh = media("fresh film", "movie", "2001", &["Drama"]);
        let exclude: Vec<String> = identities(&shown).to_vec();
        for _ in 0..20 {
            let (a, b) = brain.opening_pair(&[other_source.clone(), other_year.clone(), fresh.clone()], &exclude);
            assert_eq!(a.map(|m| m.id), Some(fresh.id.clone()));
            assert!(b.is_none(), "the same IMDb title under another key or year counts as shown");
        }
        let mut pool = vec![shown.clone()];
        rules::merge(&mut pool, vec![other_source, other_year]);
        assert_eq!(pool.len(), 1, "one entry per IMDb title");
    }

    #[test]
    fn opening_pair_avoids_copies_and_exclusions() {
        let brain = Brain::default();
        let a = embedded("pair a", 0, 9);
        let copy = media("pair a", "series", "2020", &["Drama"]);
        let b = embedded("pair b", 1, 9);
        for _ in 0..30 {
            let (x, y) = brain.opening_pair(&[a.clone(), copy.clone(), b.clone()], &[]);
            let (x, y) = (x.unwrap(), y.unwrap());
            assert_ne!(rules::title_key(&x), rules::title_key(&y));
        }
        assert_eq!(brain.opening_pair(&[a.clone(), b.clone()], &[a.key()]).0.map(|m| m.id), Some(b.id.clone()));
        assert!(brain.opening_pair(std::slice::from_ref(&a), &[]).1.is_none());
    }
}
