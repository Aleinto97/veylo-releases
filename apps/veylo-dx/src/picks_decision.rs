//! Conservative, pre-vote stopping evidence; never a calibrated enjoyment probability.
use crate::backend::{Media, Taste};
use serde_json::{json, Value};
use std::path::Path;

const REQUIRED_CONFIRMATIONS: u32 = 4;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evidence {
    pub incumbent: String,
    pub streak: u32,
    pub choices: u32,
    pub opponents: Vec<String>,
    pub reason: String,
    pub confirmations: u32,
    pub confirmed_streak: u32,
    pub checkpoint: u32,
    pub compared: Vec<String>,
}
impl Evidence {
    /// Only a prediction made BEFORE this vote can count as independent confirmation.
    pub fn record(&mut self, kept: &Media, dropped: &Media, before: Option<f64>) {
        self.choices = self.choices.saturating_add(1);
        let same = self.incumbent == kept.key();
        if !same {
            self.incumbent = kept.key();
            self.streak = 0;
            self.opponents.clear();
            self.confirmations = 0;
            self.confirmed_streak = 0;
            self.checkpoint = 0;
            self.compared.clear();
        }
        self.streak = self.streak.saturating_add(1);
        let opponent = crate::picks_rules::title_key(dropped);
        let novel = !self.compared.contains(&opponent) && self.compared.len() < 512;
        if novel {
            self.compared.push(opponent);
            let mut genres = dropped.genres.clone();
            genres.sort();
            let signature = format!("{}|{}|{}", dropped.kind, genres.join(","), dropped.first_year().unwrap_or(0) / 10);
            if !self.opponents.contains(&signature) { self.opponents.push(signature); }
        }
        if same && novel && before.is_some_and(|m| m >= 0.45) {
            self.confirmations = self.confirmations.saturating_add(1);
            self.confirmed_streak = self.confirmed_streak.saturating_add(1);
        } else { self.confirmed_streak = 0; }
        self.reason.clear();
    }
    pub fn assess(&mut self) -> bool {
        let stable = self.confirmed_streak >= REQUIRED_CONFIRMATIONS && self.opponents.len() >= 3
            && self.confirmations.saturating_sub(self.checkpoint) >= REQUIRED_CONFIRMATIONS;
        if stable {
            self.reason = "stable".into();
            self.checkpoint = self.confirmations;
        }
        stable
    }
    /// Continue keeps the entire duel; the checkpoint requires fresh evidence for another proposal.
    pub fn resume(&mut self) { self.reason.clear(); }
    pub fn choose(&mut self, taste: &mut Taste, kept: &Media, dropped: &Media, pool: &[Media], pool_ready: bool) -> (bool, Option<f64>) {
        let before = if pool_ready { margin(taste, kept, pool) } else { None };
        self.record(kept, dropped, before);
        taste.learn(kept, dropped);
        (pool_ready && self.assess(), before)
    }
    pub fn encode(&self) -> Value {
        json!({"incumbent":self.incumbent,"streak":self.streak,"choices":self.choices,"opponents":self.opponents,"reason":self.reason,
            "confirmations":self.confirmations,"confirmed_streak":self.confirmed_streak,"checkpoint":self.checkpoint,"compared":self.compared})
    }
    pub fn decode(v: &Value) -> Self {
        let count = |key: &str| v[key].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32;
        let strings = |key: &str, limit| v[key].as_array().into_iter().flatten().filter_map(Value::as_str).take(limit).map(str::to_owned).collect();
        Self { incumbent: v["incumbent"].as_str().unwrap_or_default().into(), streak: count("streak"), choices: count("choices"),
            opponents: strings("opponents",512), reason: v["reason"].as_str().unwrap_or_default().into(),
            confirmations: count("confirmations"), confirmed_streak: count("confirmed_streak"), checkpoint: count("checkpoint"), compared: strings("compared",512) }
    }
}
/// Compare against the strongest plausible alternative. Missing coverage cannot establish confidence.
pub fn margin(taste: &Taste, kept: &Media, pool: &[Media]) -> Option<f64> {
    crate::picks_ai::embedding(kept)?;
    let mut others: Vec<_> = pool.iter().filter(|m| m.key() != kept.key()).map(|m| (taste.fit(m),m)).collect();
    others.sort_by(|a,b| b.0.total_cmp(&a.0));
    others.truncate(24);
    let embedded = others.iter().filter(|(_,m)| crate::picks_ai::embedding(m).is_some()).count();
    if others.len() < 3 || embedded * 4 < others.len() * 3 { return None; }
    Some(taste.fit(kept) - others.first()?.0)
}

/// Local bounded diagnostics contain titles and decision numbers, never media/provider keys,
/// URLs, credentials, raw metadata, or the contents of the owner's library database.
pub fn audit(action: &str, kept: Option<&Media>, passed: Option<&Media>, evidence: &Evidence, before: Option<f64>, candidates: usize) {
    let name = |m: &Media| m.name.chars().take(256).collect::<String>();
    let event = json!({"time":crate::backend::now_secs(),"action":action,"kept":kept.map(name),"passed":passed.map(name),
        "choices":evidence.choices,"streak":evidence.streak,"confirmations":evidence.confirmations,"confirmed_streak":evidence.confirmed_streak,
        "checkpoint":evidence.checkpoint,"margin_before":before,"reason":evidence.reason,"candidates":candidates});
    let _ = append_event(&crate::backend::backend_data_dir().join("picks-decisions.json"),event);
}
fn append_event(path: &Path, event: Value) -> std::io::Result<()> {
    let mut events: Vec<Value> = std::fs::metadata(path).ok().filter(|m| m.len() <= 256 * 1024)
        .and_then(|_| std::fs::read(path).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["events"].as_array().cloned()).unwrap_or_default();
    events.push(event);
    if events.len() > 128 { events.drain(..events.len()-128); }
    std::fs::write(path,json!({"version":2,"events":events}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn title(id: &str, genre: &str) -> Media {
        Media::from_json(&json!({"id":id,"type":"movie","name":id,"genres":[genre],"releaseInfo":"2020","description":"A sufficiently detailed isolated synopsis for the confidence test."}), "fixture").unwrap()
    }
    fn opponents() -> Vec<Media> { (0..12).map(|i| title(&format!("opponent {i}"),["Comedy","Horror","Action"][i%3])).collect() }
    fn win(e: &mut Evidence, a: &Media, b: &Media, before: Option<f64>) -> bool { e.record(a,b,before);e.assess() }
    #[test]
    fn three_wins_cannot_stop_and_only_pre_vote_predictions_confirm() {
        let a=title("a","Drama"); let rivals=opponents(); let mut e=Evidence::default();
        for rival in &rivals[..3] { assert!(!win(&mut e,&a,rival,Some(2.0))); }
        assert!(!win(&mut e,&a,&rivals[3],Some(2.0)));
        assert!(win(&mut e,&a,&rivals[4],Some(2.0)));
        assert_eq!(e.reason,"stable");assert_eq!(e.confirmations,4);
        assert_eq!(Evidence::decode(&e.encode()),e);
    }
    #[test]
    fn continue_preserves_history_and_requires_fresh_confirmations() {
        let a=title("a","Drama");let rivals=opponents();let mut e=Evidence::default();
        for rival in &rivals[..5] { win(&mut e,&a,rival,Some(2.0)); }
        let saved=e.clone();e.resume();
        assert_eq!(e.choices,saved.choices);assert_eq!(e.streak,saved.streak);assert_eq!(e.compared,saved.compared);
        for rival in &rivals[5..8] { assert!(!win(&mut e,&a,rival,Some(2.0))); }
        assert!(win(&mut e,&a,&rivals[8],Some(2.0)));assert_eq!(e.choices,9);
    }
    #[test]
    fn missing_ai_repeated_opponents_and_uncertain_predictions_do_not_confirm() {
        let a=title("a","Drama");let rivals=opponents();let mut e=Evidence::default();
        for rival in &rivals { assert!(!win(&mut e,&a,rival,None)); }
        assert_eq!(e.choices,12); // No fixed ten-choice result or fake confidence.
        for _ in 0..20 { assert!(!win(&mut e,&a,&rivals[0],Some(2.0))); }
        e=Evidence::default();
        for rival in &rivals[..4] { win(&mut e,&a,rival,Some(2.0)); }
        assert!(!win(&mut e,&a,&rivals[4],Some(0.1)));assert_eq!(e.confirmed_streak,0);
        assert!(!win(&mut e,&rivals[0],&a,Some(2.0)));assert_eq!(e.confirmations,0);
    }
    #[test]
    fn self_training_on_the_current_choice_is_not_confirmation() {
        let kept=title("holdout-kept","Drama");
        let pool:Vec<_>=(0..3).map(|i|title(&format!("holdout-rival-{i}"),"Comedy")).collect();
        let axis=|i| {let mut v=vec![0.0;crate::picks_ai::DIM];v[i]=1.0;v};
        crate::picks_ai::cache_test_embedding(&kept,axis(0));
        for m in &pool {crate::picks_ai::cache_test_embedding(m,axis(1));}
        let mut taste=Taste::default();let mut e=Evidence::default();
        let (suggested,before)=e.choose(&mut taste,&kept,&pool[0],&pool,true);
        assert!(!suggested);assert_eq!(before,Some(0.0));assert_eq!(e.confirmations,0);
        assert!(margin(&taste,&kept,&pool).unwrap()>0.45);
        let (_,before)=e.choose(&mut taste,&kept,&pool[1],&pool,false);
        assert_eq!(before,None);assert_eq!(e.confirmations,0);
    }
    #[test]
    fn confidence_checks_the_best_rival_and_requires_embedding_coverage() {
        let kept=title("confidence-kept","Drama");
        let pool=vec![title("confidence-b","Drama"),title("confidence-c","Drama"),title("confidence-d","Drama")];
        let axis=|i| {let mut v=vec![0.0;crate::picks_ai::DIM];v[i]=1.0;v};
        crate::picks_ai::cache_test_embedding(&kept,axis(0));let taste=Taste {vibe:axis(0),..Default::default()};
        crate::picks_ai::cache_test_embedding(&pool[0],axis(1));assert_eq!(margin(&taste,&kept,&pool),None);
        for m in &pool[1..] {crate::picks_ai::cache_test_embedding(m,axis(1));}
        assert_eq!(margin(&taste,&kept,&pool),Some(1.0));
        crate::picks_ai::cache_test_embedding(&pool[2],axis(0));assert_eq!(margin(&taste,&kept,&pool),Some(0.0));
        assert_eq!(Evidence::decode(&Value::Null),Evidence::default());
    }
    #[test]
    fn audit_is_bounded_in_an_isolated_store() {
        let path=std::env::temp_dir().join(format!("veylo-decisions-{}-{}.json",std::process::id(),crate::backend::rand_u64()));
        for i in 0..140 {append_event(&path,json!({"choice":i})).unwrap();}
        let v:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["events"].as_array().unwrap().len(),128);assert_eq!(v["events"][0]["choice"],12);
        std::fs::remove_file(path).unwrap();
    }
}
