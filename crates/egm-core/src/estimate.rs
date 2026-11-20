//! Token estimates before a run, and the token ceiling.
//!
//! The call structure is egm-dsh's own, so the number of model sessions a step
//! can start is known exactly (an upper bound for the settings in use):
//!
//! * a goldfish round, parallel mode: 3 reader stages + critic + zoom-out, then
//!   at most one verifier session, plus one cross-family critic when asked;
//!   combined mode: 1 session + at most one verifier;
//! * with fail-fast, a doc that fails egm-dsh's own checks costs **0** sessions
//!   (computed here, exactly, from the doc as it is now); otherwise the worst case
//!   is the readiness stage and its verifier, then the full round;
//! * a `loop` round that does not pass adds one escalate and one redraft turn of
//!   the elephant.
//!
//! What a session costs in tokens is not known before it runs. The estimate uses
//! the project's own recorded usage (`.egm-dsh/usage.jsonl`) per role: the mean
//! per call, and for the elephant, whose resumed session grows every turn, a
//! least-squares trend over its turns. With no record yet it falls back to a
//! size heuristic and says so. The ceiling check refuses a step whose estimate
//! would cross the ceiling; `EgmCeiling` in proofs/EgmGate.lean proves the
//! spend can then exceed the ceiling by at most the estimate's error on the last
//! admitted step, and never when the estimate is an upper bound.

use std::collections::BTreeMap;
use std::path::Path;

use crate::doc::Doc;

/// Tokens of one model session for a role.
#[derive(Clone, Debug, PartialEq)]
pub struct PerCall {
    pub input: f64,
    pub output: f64,
    /// Recorded calls the numbers come from (0: heuristic).
    pub samples: usize,
    /// Extra input per further elephant turn (resumed history), when fitted.
    pub growth: f64,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct Row {
    feature: String,
    role: String,
    model: String,
    cache_hit: u64,
    cache_miss: u64,
    output: u64,
    estimated: bool,
}

/// Rough tokens of a text: 4 characters each.
pub fn tokens_of(chars: usize) -> f64 {
    (chars as f64 / 4.0).ceil()
}

/// Fixed prompt overhead of one dsh session (system prompt and tool list), per request.
const OVERHEAD: f64 = 2_500.0;

/// The heuristic when nothing is recorded: a session reads the doc and its links
/// (`reads`) over a few requests, each resending what came before.
pub fn heuristic(role: &str, doc_tokens: f64) -> PerCall {
    let (requests, output) = match role {
        "goldfish" | "goldfish-critic" | "cross-family" => (3.0, 700.0),
        "goldfish-verifier" => (2.0, 400.0),
        "elephant" => (4.0, 1_500.0),
        _ => (3.0, 800.0),
    };
    // request k carries the overhead plus everything read before it
    let input = requests * OVERHEAD + doc_tokens * (requests - 1.0) * requests / 2.0 + doc_tokens;
    PerCall { input, output, samples: 0, growth: 0.0 }
}

/// Per-role means from the usage ledger (`feature`: only that feature's rows, if any).
pub fn profiles(usage_jsonl: &str, feature: Option<&str>) -> BTreeMap<String, PerCall> {
    let rows: Vec<Row> = usage_jsonl.lines().filter_map(|l| serde_json::from_str(l).ok()).filter(|r: &Row| !r.estimated).collect();
    let mut by: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in &rows {
        by.entry(r.role.clone()).or_default().push(r);
    }
    let mut out = BTreeMap::new();
    for (role, rs) in by {
        let n = rs.len();
        let input: Vec<f64> = rs.iter().map(|r| (r.cache_hit + r.cache_miss) as f64).collect();
        let mean_in = input.iter().sum::<f64>() / n as f64;
        let mean_out = rs.iter().map(|r| r.output as f64).sum::<f64>() / n as f64;
        // The elephant's history grows: fit the trend over this feature's turns.
        let mut growth = 0.0;
        if role == "elephant" {
            let own: Vec<f64> = rs.iter().filter(|r| feature.is_none_or(|f| r.feature == f)).map(|r| (r.cache_hit + r.cache_miss) as f64).collect();
            growth = slope(&own).max(0.0);
        }
        out.insert(role, PerCall { input: mean_in, output: mean_out, samples: n, growth });
    }
    out
}

/// Least-squares slope of `ys` against 0, 1, 2, ...
pub fn slope(ys: &[f64]) -> f64 {
    let n = ys.len() as f64;
    if ys.len() < 2 {
        return 0.0;
    }
    let mx = (n - 1.0) / 2.0;
    let my = ys.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (i, y) in ys.iter().enumerate() {
        let dx = i as f64 - mx;
        num += dx * (y - my);
        den += dx * dx;
    }
    num / den
}

/// How many sessions of each role one step starts, at most.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub sessions: BTreeMap<&'static str, u64>,
    /// Why some of them will not run (for example, fail-fast stops at precheck).
    pub note: Option<String>,
}

pub struct Settings {
    pub parallel: bool,
    pub verify: bool,
    pub cross: bool,
    pub fail_fast: bool,
    /// egm-dsh's own alarms on the doc as it is now (fail-fast stops at 0 sessions).
    pub precheck_alarms: usize,
    /// Readiness readers that vote (parallel mode; 1 = one reader).
    pub votes: u64,
    /// Whether the display-only invariants stage runs.
    pub invariants: bool,
}

/// One goldfish round.
pub fn goldfish_round(s: &Settings) -> Plan {
    let mut m = BTreeMap::new();
    if s.fail_fast && s.precheck_alarms > 0 {
        return Plan { sessions: m, note: Some(format!("fail-fast: egm-dsh's own checks already find {} gap(s), so this round calls no model", s.precheck_alarms)) };
    }
    if s.parallel {
        // comprehension, each readiness reader, and invariants when it is on
        m.insert("goldfish", 1 + s.votes.max(1) + u64::from(s.invariants));
        m.insert("goldfish-critic", 2);
    } else {
        m.insert("goldfish", 1);
    }
    let mut verifiers = u64::from(s.verify);
    // fail-fast's readiness step can run the verifier once more before the rest
    if s.fail_fast && s.parallel && s.verify {
        verifiers += 1;
    }
    if verifiers > 0 {
        m.insert("goldfish-verifier", verifiers);
    }
    if s.cross {
        m.insert("cross-family", 1);
    }
    let v = s.votes.max(1);
    let note = (s.fail_fast && s.parallel).then(|| format!("fail-fast: the worst case is shown; a NOT READY from readiness alone costs {v}–{} sessions", v + 1));
    Plan { sessions: m, note }
}

/// Tokens for `plan`, using `prof` where recorded and the heuristic elsewhere.
/// `elephant_turn` is the index of the next elephant turn (for the history trend).
pub fn tokens(plan: &Plan, prof: &BTreeMap<String, PerCall>, doc_tokens: f64, elephant_turn: usize) -> Vec<(String, u64, PerCall, f64)> {
    let mut out = Vec::new();
    for (role, n) in &plan.sessions {
        let pc = prof.get(*role).cloned().unwrap_or_else(|| heuristic(role, doc_tokens));
        let mut total = 0.0;
        for k in 0..*n {
            let grow = if *role == "elephant" { pc.growth * (elephant_turn as f64 + k as f64) } else { 0.0 };
            total += pc.input + grow + pc.output;
        }
        out.push((role.to_string(), *n, pc, total));
    }
    out
}

/// The doc and its linked files, in rough tokens.
pub fn doc_tokens(doc: &Doc, root: &Path) -> f64 {
    let mut chars = doc.text.len();
    for p in doc.context_files(root) {
        chars += std::fs::metadata(&p).map(|m| m.len() as usize).unwrap_or(0);
    }
    tokens_of(chars)
}

/// Tokens a feature has used so far (reported counts only).
pub fn spent(usage_jsonl: &str, feature: &str) -> u64 {
    usage_jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<Row>(l).ok())
        .filter(|r| r.feature == feature)
        .map(|r| r.cache_hit + r.cache_miss + r.output)
        .sum()
}

/// `EgmCeiling.admit`: run a step only when what is spent plus its estimate fits.
pub fn admit(spent: u64, estimate: u64, ceiling: Option<u64>) -> bool {
    ceiling.is_none_or(|c| spent.saturating_add(estimate) <= c)
}

/// The model each role used most recently, for prices.
pub fn models(usage_jsonl: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for r in usage_jsonl.lines().filter_map(|l| serde_json::from_str::<Row>(l).ok()) {
        m.insert(r.role, r.model);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(parallel: bool, verify: bool, cross: bool, fail_fast: bool, pre: usize) -> Settings {
        Settings { parallel, verify, cross, fail_fast, precheck_alarms: pre, votes: 1, invariants: true }
    }

    #[test]
    fn session_counts_follow_the_call_structure() {
        let total = |p: Plan| p.sessions.values().sum::<u64>();
        assert_eq!(total(goldfish_round(&s(true, true, false, false, 0))), 6);
        assert_eq!(total(goldfish_round(&s(true, true, true, false, 0))), 7);
        assert_eq!(total(goldfish_round(&s(false, true, false, false, 0))), 2);
        assert_eq!(total(goldfish_round(&s(true, false, false, false, 0))), 5);
        // fail-fast: a precheck gap costs nothing; otherwise worst case is one verifier more
        assert_eq!(total(goldfish_round(&s(true, true, false, true, 3))), 0);
        assert_eq!(total(goldfish_round(&s(true, true, false, true, 0))), 7);
        // the defaults: 3 readiness readers, invariants off: 1 + 3 readers, critic, zoom-out, verifier
        let d = Settings { votes: 3, invariants: false, ..s(true, true, false, false, 0) };
        assert_eq!(goldfish_round(&d).sessions["goldfish"], 4);
        assert_eq!(total(goldfish_round(&d)), 7);
        // combined mode keeps one session whatever the vote setting
        assert_eq!(total(goldfish_round(&Settings { votes: 3, ..s(false, false, false, false, 0) })), 1);
    }

    #[test]
    fn profiles_use_reported_counts_and_fit_the_elephant_trend() {
        let ledger = [
            r#"{"feature":"a","role":"goldfish","model":"m","cache_hit":100,"cache_miss":900,"output":200}"#,
            r#"{"feature":"a","role":"goldfish","model":"m","cache_hit":0,"cache_miss":3000,"output":400}"#,
            r#"{"feature":"a","role":"goldfish","model":"m","cache_hit":0,"cache_miss":99999,"output":0,"estimated":true}"#,
            r#"{"feature":"a","role":"elephant","model":"m","cache_hit":0,"cache_miss":1000,"output":10}"#,
            r#"{"feature":"a","role":"elephant","model":"m","cache_hit":0,"cache_miss":2000,"output":10}"#,
            r#"{"feature":"a","role":"elephant","model":"m","cache_hit":0,"cache_miss":3000,"output":10}"#,
        ]
        .join("\n");
        let p = profiles(&ledger, Some("a"));
        assert_eq!(p["goldfish"], PerCall { input: 2000.0, output: 300.0, samples: 2, growth: 0.0 });
        assert_eq!(p["elephant"].growth, 1000.0);
        assert_eq!(spent(&ledger, "a"), 100 + 900 + 200 + 3000 + 400 + 99999 + 1000 + 10 + 2000 + 10 + 3000 + 10);
        let plan = Plan { sessions: BTreeMap::from([("elephant", 2)]), note: None };
        // turns 3 and 4: mean 2000 + 1000·3 and + 1000·4, plus 10 output each
        assert_eq!(tokens(&plan, &p, 0.0, 3)[0].3, 5010.0 + 6010.0);
    }

    #[test]
    fn the_ceiling_admits_exactly_what_fits() {
        assert!(admit(10, 5, None));
        assert!(admit(10, 5, Some(15)));
        assert!(!admit(10, 6, Some(15)));
        assert!(!admit(u64::MAX, 1, Some(u64::MAX - 1)));
        assert!(admit(0, 0, Some(0)), "a step that calls no model always fits");
    }

    #[test]
    fn slope_and_heuristic() {
        assert_eq!(slope(&[1.0]), 0.0);
        assert_eq!(slope(&[0.0, 2.0, 4.0]), 2.0);
        let h = heuristic("goldfish", 1000.0);
        assert!(h.input > 1000.0 && h.samples == 0);
    }
}
