//! Learned alarm triage, mirrored by `proofs/EgmTriage.lean`.
//!
//! A trained model (CatBoost or a SAM-trained net from `notebooks/`) sits
//! behind an OpenAI-compatible `/chat/completions` endpoint. For each alarm
//! that survived grounding it answers `keep` (skip the verifier; the alarm
//! stands) or `verify`. That is all it can do: this module only ever sets
//! `Alarm::skip_verify`, and has no way to drop an alarm. A wrong model can
//! cost a round with the elephant; it cannot weaken the gate.
//!
//! - **Fail-safe.** No endpoint, an error, or an answer egm cannot read means
//!   every alarm goes to the verifier, as without triage.
//! - **Exploration.** A deterministic share of `keep` answers (`explore`, 10% by
//!   default) is verified anyway, so the verifier keeps producing labels for
//!   the kind of alarms the model keeps; without that, retraining would only
//!   ever see the alarms the model was unsure about.
//! - **Same features everywhere.** `features` is saved in the review file and
//!   sent verbatim to the endpoint; the notebook trains on exactly these records.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::alarm::{Alarm, Status};
use crate::doc::{Doc, SECTIONS};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriageConfig {
    /// Base URL of the OpenAI-compatible server; `/chat/completions` is appended.
    pub base_url: String,
    /// Model name the server exposes, e.g. `egm-triage-catboost` or `egm-triage-sam-mlp`.
    pub model: String,
    /// Environment variable with a bearer token; empty for none.
    #[serde(default)]
    pub api_key_env: String,
    /// Share of `keep` answers verified anyway, for unbiased labels.
    #[serde(default = "default_explore")]
    pub explore: f64,
}

fn default_explore() -> f64 {
    0.1
}

/// What triage did to one alarm, saved with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriageMark {
    pub model: String,
    /// `keep` or `verify`, as the model answered.
    pub action: String,
    pub p_real: Option<f64>,
    /// A `keep` that was verified anyway for exploration.
    pub explored: bool,
}

/// Facts about the review an alarm came from, for its feature record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReviewContext {
    pub reader_model: String,
    pub reader_effort: String,
    pub critic_model: String,
    pub mode: String,
    pub run: u32,
    pub doc_words: usize,
    pub doc_map_files: usize,
    pub doc_criteria: usize,
    pub doc_sections_missing: usize,
}

impl ReviewContext {
    pub fn with_doc(mut self, doc: &Doc) -> Self {
        self.doc_words = doc.text.split_whitespace().count();
        self.doc_map_files = doc.implementation_map().len();
        self.doc_criteria = doc.criteria().len();
        self.doc_sections_missing = doc.missing_sections().len();
        self
    }
}

fn canonical_section(name: &str) -> &'static str {
    let lc = name.trim().trim_start_matches('#').trim().to_lowercase();
    SECTIONS.iter().find(|s| !lc.is_empty() && (lc.contains(&s.to_lowercase()) || s.to_lowercase().contains(&lc))).copied().unwrap_or("other")
}

fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The feature record for one alarm. Schema version 1; the notebook reads the same keys.
pub fn features(a: &Alarm, ctx: &ReviewContext, all: &[Alarm]) -> Value {
    let model_alarms = all.iter().filter(|x| x.model_raised()).count();
    let grounding_drops = all.iter().filter(|x| x.dropped_by.as_deref() == Some("grounding")).count();
    json!({
        "schema": 1,
        "source": a.source, "kind": a.kind, "grounding": a.grounding,
        "section": canonical_section(&a.section),
        "text": a.text, "quote": a.quote, "search_terms": a.search_terms,
        "n_terms": a.search_terms.len(), "has_quote": !a.quote.trim().is_empty(),
        "text_words": words(&a.text), "quote_words": words(&a.quote),
        "reader_model": ctx.reader_model, "reader_effort": ctx.reader_effort,
        "critic_model": ctx.critic_model, "mode": ctx.mode, "run": ctx.run,
        "doc_words": ctx.doc_words, "doc_map_files": ctx.doc_map_files,
        "doc_criteria": ctx.doc_criteria, "doc_sections_missing": ctx.doc_sections_missing,
        "n_model_alarms": model_alarms, "n_grounding_drops": grounding_drops,
    })
}

/// Alarms triage may look at: open, raised by a model, not yet judged.
pub fn candidates(alarms: &[Alarm]) -> Vec<&Alarm> {
    alarms.iter().filter(|a| a.model_raised() && a.status == Status::Open && a.kind != crate::alarm::Kind::Note).collect()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Decision {
    pub id: usize,
    pub action: String,
    pub p_real: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Decisions {
    decisions: Vec<Decision>,
}

/// Deterministic exploration draw: the same alarm in the same review always gets the same answer.
pub fn explored(salt: u64, id: usize, rate: f64) -> bool {
    if rate <= 0.0 {
        return false;
    }
    // FNV alone is not uniform over short, similar keys; finish with SplitMix64.
    let mut z = crate::doc::hash(&format!("{salt}:{id}")).wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    ((z >> 11) as f64 / (1u64 << 53) as f64) < rate
}

/// Apply the model's answers. The ONLY effect is `skip_verify = true` on a
/// `keep` that is not explored. `drop`, unknown actions, unknown ids, and
/// egm's own alarms are ignored. Mirrors `triage` in `proofs/EgmTriage.lean`.
pub fn apply(alarms: &mut [Alarm], decisions: &[Decision], model: &str, explore: f64, salt: u64) {
    for a in alarms.iter_mut().filter(|a| a.model_raised() && a.status == Status::Open && a.kind != crate::alarm::Kind::Note) {
        let Some(d) = decisions.iter().find(|d| d.id == a.id) else { continue };
        let keep = d.action.trim().eq_ignore_ascii_case("keep");
        let exp = keep && explored(salt, a.id, explore);
        a.skip_verify = keep && !exp;
        a.triage = Some(TriageMark { model: model.into(), action: d.action.trim().to_lowercase(), p_real: d.p_real, explored: exp });
    }
}

/// Ask the endpoint about the candidates. Any failure returns `Err`, and the
/// caller then verifies everything.
pub fn ask(cfg: &TriageConfig, alarms: &[Alarm]) -> anyhow::Result<Vec<Decision>> {
    let items: Vec<Value> = candidates(alarms).iter().map(|a| json!({"id": a.id, "features": a.features})).collect();
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = json!({
        "model": cfg.model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": "egm alarm triage, schema 1. Answer keep or verify for each alarm."},
            {"role": "user", "content": json!({"alarms": items}).to_string()},
        ],
    });
    let mut req = ureq::post(&url);
    let key;
    if !cfg.api_key_env.is_empty() {
        key = std::env::var(&cfg.api_key_env).map_err(|_| anyhow::anyhow!("set {} for the triage endpoint", cfg.api_key_env))?;
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    let mut resp = req.send_json(&body)?;
    let v: Value = resp.body_mut().read_json()?;
    let content = v["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow::anyhow!("no message content"))?;
    let parsed: Decisions = crate::goldfish::parse(content).ok_or_else(|| anyhow::anyhow!("content is not decisions JSON"))?;
    Ok(parsed.decisions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::{Grounding, Kind, Source};

    fn alarm(id: usize, source: Source) -> Alarm {
        Alarm {
            id,
            source,
            kind: Kind::Question,
            text: "q".into(),
            section: "Technical plan".into(),
            quote: String::new(),
            search_terms: vec![],
            grounding: Grounding::Grounded,
            status: Status::Open,
            dropped_by: None,
            reason: None,
            features: None,
            triage: None,
            skip_verify: false,
            caught_by: None,
            vote: None,
        }
    }

    #[test]
    fn triage_never_drops_whatever_the_model_says() {
        let mut alarms = vec![alarm(1, Source::Readiness), alarm(2, Source::Critic), alarm(3, Source::Egm), alarm(4, Source::Readiness)];
        let d = |id, action: &str| Decision { id, action: action.into(), p_real: Some(0.5) };
        // A hostile or buggy model: tries to drop, keeps egm's own alarm, invents ids.
        apply(&mut alarms, &[d(1, "keep"), d(2, "drop"), d(3, "keep"), d(4, "DROP"), d(99, "keep")], "m", 0.0, 7);
        assert!(alarms.iter().all(|a| a.counts() && a.status == Status::Open && a.dropped_by.is_none()));
        assert!(alarms[0].skip_verify);
        assert!(!alarms[1].skip_verify && !alarms[3].skip_verify);
        assert!(alarms[2].triage.is_none() && !alarms[2].skip_verify, "egm's own alarms are not triaged");
    }

    #[test]
    fn exploration_is_deterministic_and_near_its_rate() {
        let hits = (0..10_000).filter(|i| explored(42, *i, 0.1)).count();
        assert!((800..1200).contains(&hits), "{hits}");
        assert_eq!(explored(42, 5, 0.1), explored(42, 5, 0.1));
        assert!(!explored(42, 5, 0.0));
        let mut a = vec![alarm(1, Source::Readiness)];
        apply(&mut a, &[Decision { id: 1, action: "keep".into(), p_real: None }], "m", 1.0, 1);
        assert!(!a[0].skip_verify && a[0].triage.as_ref().unwrap().explored);
    }

    #[test]
    fn feature_record_has_the_schema_keys() {
        let all = vec![alarm(1, Source::Readiness)];
        let f = features(&all[0], &ReviewContext { reader_model: "deepseek-flash".into(), ..Default::default() }, &all);
        for k in ["schema", "source", "kind", "grounding", "section", "text", "n_terms", "reader_model", "doc_words", "n_model_alarms"] {
            assert!(f.get(k).is_some(), "{k}");
        }
        assert_eq!(f["section"], "Technical plan");
        assert_eq!(canonical_section("## technical plan section"), "Technical plan");
        assert_eq!(canonical_section("Rollout"), "other");
    }
}
