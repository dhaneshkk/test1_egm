//! False-alarm mitigation for goldfish reviews.
//!
//! An *alarm* is anything that blocks a build: an open question, an unexplained
//! "not ready", or a blocking finding. A cheap reviewer raises too many, so
//! egm checks each model-raised alarm in two steps before it counts:
//!
//! 1. **Grounding** (no model). Every alarm must name the section where its
//!    answer should be, may quote the words it is about, and lists search
//!    terms whose absence makes it an alarm. egm checks the quote against the
//!    doc and its linked files. A quote that is not there means the alarm rests
//!    on text the doc does not contain, so it is dropped, with that reason
//!    recorded. If every search term already appears in the named section, the
//!    alarm is flagged "possibly answered" for the verifier.
//! 2. **Escalation** (one model call, only when alarms remain). A verifier
//!    session, a larger model by default, judges each remaining alarm. It can
//!    only drop one by quoting the passage that answers it (egm checks the
//!    quote) or by giving a reason it is not blocking. Anything the verifier
//!    does not clearly reject stays.
//!
//! egm's own deterministic alarms (missing sections, criteria without a
//! check) are never dropped. `proofs/EgmGate.lean` proves that the gate only
//! passes when every alarm was dropped this way, with a recorded reason.

use serde::{Deserialize, Serialize};

use crate::doc::{Doc, SECTIONS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// egm's own checks. Never dropped.
    Egm,
    Comprehension,
    Readiness,
    Critic,
    CrossCritic,
    /// Upstream EGM's zoom-out (Skeptical Architect) stage.
    ZoomOut,
    /// The builder: a decision it had to make because the doc did not (`executor.rs`).
    Builder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Question,
    Blocking,
    /// A comprehension reviewer's "unclear" point: shown and handed to the elephant,
    /// never counted against the doc (`EgmAdvice.notes_never_change_the_verdict`).
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grounding {
    /// egm's own alarm; nothing to check.
    NotApplicable,
    /// The quoted words are in the doc or a linked file.
    QuoteFound,
    /// The quoted words are nowhere in the doc or its links.
    QuoteMissing,
    /// Every search term already appears in the section named.
    PossiblyAnswered,
    /// The search terms are not all in that section: the gap looks real.
    Grounded,
    /// No section, quote or search terms were given.
    NoEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Counts against the doc; not yet verified.
    Open,
    /// The verifier agreed it is a real gap.
    Confirmed,
    /// Dropped; `dropped_by` and `reason` say why.
    Dropped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub id: usize,
    pub source: Source,
    pub kind: Kind,
    pub text: String,
    pub section: String,
    pub quote: String,
    pub search_terms: Vec<String>,
    pub grounding: Grounding,
    pub status: Status,
    /// `grounding` or `verifier`.
    pub dropped_by: Option<String>,
    pub reason: Option<String>,
    /// The feature record sent to the triage model and used for training (`triage::features`).
    #[serde(default)]
    pub features: Option<serde_json::Value>,
    /// What the triage model answered, if one was asked.
    #[serde(default)]
    pub triage: Option<crate::triage::TriageMark>,
    /// Set only by triage: the alarm stands without going to the verifier.
    #[serde(default)]
    pub skip_verify: bool,
    /// The acceptance criterion (from 1) whose check catches a wrong guess, when the
    /// verifier dropped it as `caught-by-check`. The tester is told to cover it.
    #[serde(default)]
    pub caught_by: Option<usize>,
    /// Which readiness reader (from 0) raised it, when several vote (`EgmVote`).
    #[serde(default)]
    pub vote: Option<usize>,
}

impl Alarm {
    pub fn model_raised(&self) -> bool {
        self.source != Source::Egm
    }

    /// Whether the alarm still blocks the build. Notes never do (`EgmAdvice.counts`).
    pub fn counts(&self) -> bool {
        self.status != Status::Dropped && self.kind != Kind::Note
    }

    /// Whether the verifier (and triage) should look at it: a model's open alarm that counts.
    pub fn to_verify(&self) -> bool {
        self.status == Status::Open && self.model_raised() && self.kind != Kind::Note && !self.skip_verify
    }

    /// Drop an alarm with a recorded reason. egm's own alarms cannot be dropped.
    pub fn drop_with(&mut self, by: &str, reason: String) {
        if self.model_raised() {
            self.status = Status::Dropped;
            self.dropped_by = Some(by.into());
            self.reason = Some(reason);
        }
    }
}

/// What a reviewer gave as evidence for one question or finding.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Ask {
    pub text: String,
    pub section: String,
    pub quote: String,
    pub search_terms: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AskObj {
    #[serde(alias = "question", alias = "point", alias = "issue")]
    text: String,
    section: String,
    quote: String,
    search_terms: Vec<String>,
}

/// Models sometimes answer with a bare string; accept both shapes.
#[derive(Deserialize)]
#[serde(untagged)]
enum AskIn {
    Text(String),
    Obj(AskObj),
}

impl<'de> Deserialize<'de> for Ask {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match AskIn::deserialize(d)? {
            AskIn::Text(text) => Ask { text, ..Ask::default() },
            AskIn::Obj(o) => Ask { text: o.text, section: o.section, quote: o.quote, search_terms: o.search_terms },
        })
    }
}

/// Lowercase, drop Markdown emphasis and quote marks, collapse whitespace.
pub fn normalize(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '`' | '*' | '_' | '"' | '\'' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}' => ' ',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `quote` occurs in `corpus` (both normalized). An ellipsis splits the
/// quote; every fragment must occur, in order.
pub fn quote_found(corpus_norm: &str, quote: &str) -> bool {
    let mut from = 0;
    let fragments: Vec<String> = quote.split(['…']).flat_map(|p| p.split("...")).map(normalize).filter(|f| !f.is_empty()).collect();
    if fragments.is_empty() {
        return true;
    }
    for f in fragments {
        match corpus_norm[from..].find(&f) {
            Some(i) => from += i + f.len(),
            None => return false,
        }
    }
    true
}

/// The body of the section a reviewer named, tolerating `## `, case, and
/// a trailing word like "section".
pub fn find_section(doc: &Doc, name: &str) -> Option<String> {
    let cleaned = name.trim().trim_start_matches('#').trim();
    let cleaned = cleaned.strip_suffix(" section").unwrap_or(cleaned).trim();
    if cleaned.is_empty() {
        return None;
    }
    if let Some(body) = doc.section(cleaned) {
        return Some(body);
    }
    let lc = cleaned.to_lowercase();
    SECTIONS
        .iter()
        .find(|s| lc.contains(&s.to_lowercase()) || s.to_lowercase().contains(&lc))
        .and_then(|s| doc.section(s))
}

/// Step 1: check one alarm's evidence against the doc and linked files.
pub fn ground(alarm: &mut Alarm, doc: &Doc, corpus_norm: &str) {
    if !alarm.model_raised() {
        alarm.grounding = Grounding::NotApplicable;
        return;
    }
    let has_quote = !alarm.quote.trim().is_empty();
    let terms: Vec<String> = alarm.search_terms.iter().map(|t| normalize(t)).filter(|t| !t.is_empty()).collect();
    if has_quote && !quote_found(corpus_norm, &alarm.quote) {
        alarm.grounding = Grounding::QuoteMissing;
        let q = alarm.quote.trim().to_string();
        alarm.drop_with("grounding", format!("the quoted text \u{201c}{q}\u{201d} is not in the doc or its linked files"));
        return;
    }
    if !terms.is_empty() {
        let scope = find_section(doc, &alarm.section).unwrap_or_else(|| doc.text.clone());
        let scope = normalize(&scope);
        alarm.grounding = if terms.iter().all(|t| scope.contains(t.as_str())) { Grounding::PossiblyAnswered } else { Grounding::Grounded };
    } else if has_quote {
        alarm.grounding = Grounding::QuoteFound;
    } else {
        alarm.grounding = Grounding::NoEvidence;
    }
}

/// One verifier judgment.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Judgment {
    pub id: usize,
    /// `confirmed`, `answered`, or `not-blocking`.
    pub verdict: String,
    /// For `answered`: the exact words of the doc that answer the alarm.
    pub quote: String,
    pub reason: String,
    /// For `caught-by-check`: the number (from 1) of the acceptance criterion whose
    /// check a wrong guess would fail.
    pub criterion: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Judgments {
    pub items: Vec<Judgment>,
}

/// Whether a `caught-by-check` may drop this alarm: a model's question (never a
/// blocking finding, never egm-dsh's own) and criterion `n` (from 1) exists and names
/// a check. Mirrors `EgmAdvice.caughtOk`.
pub fn caught_ok(a: &Alarm, n: usize, criteria: &[crate::doc::Criterion]) -> bool {
    a.model_raised() && a.kind == Kind::Question && n >= 1 && criteria.get(n - 1).is_some_and(|c| c.check.is_some())
}

/// Step 2: apply the verifier's judgments. Conservative: an alarm is dropped
/// only by an `answered` whose quote egm finds, a `not-blocking` with a reason, or a
/// `caught-by-check` naming a criterion that has a check (`caught_ok`).
/// Everything else (missing, unclear, unverifiable) keeps the alarm.
pub fn apply_judgments(alarms: &mut [Alarm], judgments: &[Judgment], corpus_norm: &str, criteria: &[crate::doc::Criterion]) {
    for a in alarms.iter_mut().filter(|a| a.to_verify()) {
        let Some(j) = judgments.iter().find(|j| j.id == a.id) else { continue };
        match j.verdict.trim().to_lowercase().as_str() {
            "answered" if !j.quote.trim().is_empty() && quote_found(corpus_norm, &j.quote) => {
                let q = j.quote.trim().to_string();
                a.drop_with("verifier", format!("answered by the doc: \u{201c}{q}\u{201d}"));
            }
            "answered" => {
                a.status = Status::Confirmed;
                a.reason = Some("the verifier said it is answered, but its quote is not in the doc; kept".into());
            }
            "not-blocking" | "not_blocking" | "not blocking" if !j.reason.trim().is_empty() => {
                a.drop_with("verifier", format!("not blocking: {}", j.reason.trim()));
            }
            "caught-by-check" | "caught_by_check" | "caught by check" if caught_ok(a, j.criterion, criteria) => {
                let c = &criteria[j.criterion - 1];
                let why = if j.reason.trim().is_empty() { String::new() } else { format!(" — {}", j.reason.trim()) };
                a.drop_with("verifier", format!("caught by acceptance criterion {}: \u{201c}{}\u{201d} (check: {}){why}", j.criterion, c.text, c.check.as_deref().unwrap_or("")));
                a.caught_by = Some(j.criterion);
            }
            "caught-by-check" | "caught_by_check" | "caught by check" => {
                a.status = Status::Confirmed;
                a.reason = Some(format!("the verifier said a check catches it, but criterion {} has no check (or is not a question); kept", j.criterion));
            }
            "confirmed" => a.status = Status::Confirmed,
            _ => {}
        }
    }
}

/// The verifier's prompt. Items go in as JSON after `ITEMS_JSON:` so any model
/// (and the mock agent) can read them unambiguously.
pub fn verification_prompt(brief: &str, alarms: &[Alarm], criteria: &[crate::doc::Criterion]) -> String {
    let numbered: Vec<String> = criteria
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}{}", i + 1, c.text, c.check.as_deref().map(|k| format!(" (check: {k})")).unwrap_or_else(|| " (no check)".into())))
        .collect();
    let items: Vec<serde_json::Value> = alarms
        .iter()
        .filter(|a| a.to_verify())
        .map(|a| {
            let mut v = serde_json::json!({"id": a.id, "kind": a.kind, "raised_by": a.source, "text": a.text, "section": a.section, "search_terms": a.search_terms});
            if !a.quote.is_empty() {
                v["quote"] = a.quote.clone().into();
            }
            if a.grounding == Grounding::PossiblyAnswered {
                v["hint"] = format!("all of {:?} already appear in that section", a.search_terms).into();
            }
            v
        })
        .collect();
    format!(
        "{brief}\n\nYou are verifying alarms that a less experienced reviewer raised as reasons this document is not ready to implement. For each item decide one of:\n- \"confirmed\": a real gap. An implementer would have to guess, and a wrong guess would ship a defect.\n- \"answered\": the files already answer it. Put the exact words that answer it in \"quote\", copied character for character.\n- \"not-blocking\": a fair point, but any reasonable choice would be correct. Say why in \"reason\". How the tests are written, which test framework they use, and test files that do not exist yet are always not-blocking: the build writes the tests from the acceptance criteria.\n- \"caught-by-check\" (questions only, never a finding that the design itself is wrong): an implementer might guess wrong, but every wrong guess would make one of the acceptance criteria below false, so its check fails before anything ships. Put that criterion's number in \"criterion\" and, in \"reason\", the wrong guess and how the criterion catches it. A criterion marked (no check) catches nothing.\nIf you are unsure, choose \"confirmed\". Judge only from the files.\n\nACCEPTANCE CRITERIA:\n{}\n\nITEMS_JSON: {}\n\nReply with only one JSON object of this shape, and no other text:\n{{\"items\": [{{\"id\": 1, \"verdict\": \"confirmed | answered | not-blocking | caught-by-check\", \"quote\": \"...\", \"reason\": \"...\", \"criterion\": 0}}]}}",
        if numbered.is_empty() { "(none)".to_string() } else { numbered.join("\n") },
        serde_json::Value::Array(items)
    )
}

/// Parse the items back out of a verification prompt (used by the mock agent).
pub fn items_from_prompt(prompt: &str) -> Vec<serde_json::Value> {
    let Some(start) = prompt.find("ITEMS_JSON: ") else { return vec![] };
    let rest = &prompt[start + 12..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    serde_json::from_str(&rest[..end]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Doc {
        Doc {
            path: "docs/d.md".into(),
            text: "# D\n\n## Technical plan\n\nEach note gains a `tags` list, stored in `notes.json`. Tag matching is exact and case-sensitive.\n\n## Acceptance criteria\n\n- x. Check: `t.py`\n".into(),
        }
    }

    fn alarm(source: Source, quote: &str, section: &str, terms: &[&str]) -> Alarm {
        Alarm {
            id: 1,
            source,
            kind: Kind::Question,
            text: "q".into(),
            section: section.into(),
            quote: quote.into(),
            search_terms: terms.iter().map(|s| s.to_string()).collect(),
            grounding: Grounding::NoEvidence,
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
    fn quotes_survive_markdown_and_ellipses() {
        let c = normalize(&doc().text);
        assert!(quote_found(&c, "stored in notes.json"));
        assert!(quote_found(&c, "Each note gains a \u{201c}tags\u{201d} list … case-sensitive"));
        assert!(!quote_found(&c, "tags are lowercased before saving"));
        assert!(!quote_found(&c, "case-sensitive ... Each note")); // fragments out of order
    }

    #[test]
    fn grounding_drops_fabricated_quotes_only_from_models() {
        let d = doc();
        let c = normalize(&d.text);
        let mut fake = alarm(Source::Critic, "tags are lowercased before saving", "Technical plan", &[]);
        ground(&mut fake, &d, &c);
        assert_eq!((fake.grounding, fake.status), (Grounding::QuoteMissing, Status::Dropped));
        let mut own = alarm(Source::Egm, "tags are lowercased before saving", "", &[]);
        ground(&mut own, &d, &c);
        assert_eq!(own.status, Status::Open);
    }

    #[test]
    fn grounding_flags_possibly_answered_and_real_gaps() {
        let d = doc();
        let c = normalize(&d.text);
        let mut answered = alarm(Source::Readiness, "", "## Technical plan section", &["case-sensitive"]);
        ground(&mut answered, &d, &c);
        assert_eq!((answered.grounding, answered.status), (Grounding::PossiblyAnswered, Status::Open));
        let mut gap = alarm(Source::Readiness, "", "Technical plan", &["existing notes"]);
        ground(&mut gap, &d, &c);
        assert_eq!(gap.grounding, Grounding::Grounded);
    }

    #[test]
    fn verifier_needs_evidence_to_drop() {
        let c = normalize(&doc().text);
        let mut alarms = vec![
            Alarm { id: 1, ..alarm(Source::Readiness, "", "", &[]) },
            Alarm { id: 2, ..alarm(Source::Readiness, "", "", &[]) },
            Alarm { id: 3, ..alarm(Source::Critic, "", "", &[]) },
            Alarm { id: 4, ..alarm(Source::Critic, "", "", &[]) },
            Alarm { id: 5, ..alarm(Source::Egm, "", "", &[]) },
            Alarm { id: 6, ..alarm(Source::Critic, "", "", &[]) },
        ];
        let j = |id, verdict: &str, quote: &str, reason: &str| Judgment { id, verdict: verdict.into(), quote: quote.into(), reason: reason.into(), criterion: 0 };
        apply_judgments(&mut alarms, &[
            j(1, "answered", "Tag matching is exact and case-sensitive", ""),
            j(2, "answered", "matching ignores case", ""),
            j(3, "not-blocking", "", ""),
            j(4, "not-blocking", "", "either order is fine"),
            j(5, "answered", "stored in notes.json", ""),
        ], &c, &[]);
        let st: Vec<Status> = alarms.iter().map(|a| a.status).collect();
        assert_eq!(st, vec![Status::Dropped, Status::Confirmed, Status::Open, Status::Dropped, Status::Open, Status::Open]);
        assert_eq!(alarms[0].dropped_by.as_deref(), Some("verifier"));
    }

    #[test]
    fn asks_accept_strings_and_objects() {
        let v: Vec<Ask> = serde_json::from_str(r#"["plain", {"question": "q", "section": "S", "search_terms": ["a"]}]"#).unwrap();
        assert_eq!(v[0].text, "plain");
        assert_eq!((v[1].text.as_str(), v[1].section.as_str(), v[1].search_terms.len()), ("q", "S", 1));
    }

    #[test]
    fn verification_prompt_round_trips_items() {
        let mut a = alarm(Source::Readiness, "", "Technical plan", &["case-sensitive"]);
        a.grounding = Grounding::PossiblyAnswered;
        let p = verification_prompt("BRIEF", &[a], &[]);
        let items = items_from_prompt(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], 1);
        assert!(items[0]["hint"].as_str().unwrap().contains("case-sensitive"));
    }

    fn crit(text: &str, check: Option<&str>) -> crate::doc::Criterion {
        crate::doc::Criterion { text: text.into(), check: check.map(String::from) }
    }

    #[test]
    fn caught_by_check_needs_a_question_and_a_checked_criterion() {
        // EgmAdvice: caught_needs_a_checked_criterion, blocker_survives_caught, egm_alarm_survives_caught
        let c = normalize(&doc().text);
        let criteria = [crit("`jot delete 2` removes note 2", Some("tests/test_delete.py")), crit("deletes are fast", None)];
        let mut alarms = vec![
            Alarm { id: 1, ..alarm(Source::Readiness, "", "", &[]) },             // question, criterion 1 has a check
            Alarm { id: 2, ..alarm(Source::Readiness, "", "", &[]) },             // criterion 2 has no check
            Alarm { id: 3, ..alarm(Source::Readiness, "", "", &[]) },             // criterion 9 does not exist
            Alarm { id: 4, kind: Kind::Blocking, ..alarm(Source::Critic, "", "", &[]) }, // a design defect
            Alarm { id: 5, ..alarm(Source::Egm, "", "", &[]) },                   // egm-dsh's own
            Alarm { id: 6, ..alarm(Source::Readiness, "", "", &[]) },             // criterion 0 is not a number from 1
        ];
        let j = |id, n| Judgment { id, verdict: "caught-by-check".into(), criterion: n, reason: "a string id fails it".into(), ..Judgment::default() };
        apply_judgments(&mut alarms, &[j(1, 1), j(2, 2), j(3, 9), j(4, 1), j(5, 1), j(6, 0)], &c, &criteria);
        let st: Vec<Status> = alarms.iter().map(|a| a.status).collect();
        assert_eq!(st, vec![Status::Dropped, Status::Confirmed, Status::Confirmed, Status::Confirmed, Status::Open, Status::Confirmed]);
        assert_eq!(alarms[0].caught_by, Some(1));
        assert!(alarms[0].reason.as_deref().unwrap().contains("tests/test_delete.py"));
        assert!(alarms[1..].iter().all(|a| a.caught_by.is_none()));
    }

    #[test]
    fn notes_never_count_and_skip_the_verifier() {
        // EgmAdvice.note_never_counts
        let n = Alarm { kind: Kind::Note, ..alarm(Source::Comprehension, "", "", &[]) };
        assert!(!n.counts() && !n.to_verify());
        let p = verification_prompt("BRIEF", &[n, Alarm { id: 2, ..alarm(Source::Readiness, "", "", &[]) }], &[crit("x", Some("t.py")), crit("y", None)]);
        let items = items_from_prompt(&p);
        assert_eq!(items.len(), 1, "only the question goes to the verifier");
        assert!(p.contains("1. x (check: t.py)") && p.contains("2. y (no check)") && p.contains("caught-by-check"));
    }
}
