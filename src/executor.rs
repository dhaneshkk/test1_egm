//! The builder as the last goldfish. Every decision the builder had to make because
//! the doc did not make it is a blind spot the review missed. The builder lists them;
//! a blocking one rejects the build and reopens the design as a not-ready review
//! (`EgmExecutor` in `proofs/EgmGate.lean`), which `egm-dsh escalate` then turns into
//! repository answers or decisions like any goldfish review.

use serde::{Deserialize, Serialize};

use crate::alarm::{self, Alarm, Grounding, Kind, Source, Status};
use crate::doc::Doc;

/// What the builder is asked to add at the end of its reply.
pub const ASK: &str = "\n\nFinally, after your summary, list every decision the design document did not make that you had to make yourself, as one JSON object on its own:\n{\"assumptions\": [{\"decision\": \"what you decided\", \"section\": \"the doc section it belongs in\", \"quote\": \"exact words of the doc it concerns, or empty if the doc says nothing\", \"blocking\": true}]}\nMark an assumption blocking when another reasonable reading of the document would have led to behaviour a user or caller could notice. An empty list is a valid answer; do not invent assumptions.";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Assumption {
    #[serde(alias = "assumption", alias = "text")]
    pub decision: String,
    pub section: String,
    pub quote: String,
    pub blocking: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Listing {
    assumptions: Vec<Assumption>,
}

/// The builder's assumptions, from the last `{"assumptions": [...]}` object in its reply.
/// `None` when the reply has no such object (the builder did not answer the question).
pub fn parse(reply: &str) -> Option<Vec<Assumption>> {
    let key = reply.rfind("\"assumptions\"")?;
    // walk back to each `{` before the key and take the first that parses as the listing
    let starts: Vec<usize> = reply[..key].match_indices('{').map(|(i, _)| i).collect();
    for &start in starts.iter().rev() {
        let mut stream = serde_json::Deserializer::from_str(&reply[start..]).into_iter::<Listing>();
        if let Some(Ok(l)) = stream.next() {
            return Some(l.assumptions.into_iter().filter(|a| !a.decision.trim().is_empty()).collect());
        }
    }
    None
}

/// Blocking assumptions as alarms, grounded like a goldfish's: a quote that is not in
/// the doc (or its links) is dropped with the reason recorded. Ids start at `first_id`.
pub fn alarms(assumptions: &[Assumption], doc: &Doc, corpus_norm: &str, first_id: usize) -> Vec<Alarm> {
    let mut out: Vec<Alarm> = Vec::new();
    for a in assumptions.iter().filter(|a| a.blocking) {
        let mut al = Alarm {
            id: first_id + out.len(),
            source: Source::Builder,
            kind: Kind::Blocking,
            text: format!("The builder had to decide: {}", a.decision.trim()),
            section: a.section.trim().to_string(),
            quote: a.quote.trim().to_string(),
            search_terms: vec![],
            grounding: Grounding::NoEvidence,
            status: Status::Open,
            dropped_by: None,
            reason: None,
            features: None,
            triage: None,
            skip_verify: true,
            caught_by: None,
            vote: None,
        };
        alarm::ground(&mut al, doc, corpus_norm);
        out.push(al);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn finds_the_last_listing_even_after_code_with_braces() {
        let reply = "Changed `jot/cli.py`: `def f(): return {\"a\": 1}`.\n\n{\"assumptions\": [{\"decision\": \"tags are trimmed\", \"section\": \"Technical plan\", \"quote\": \"\", \"blocking\": true}, {\"decision\": \"\", \"blocking\": true}]}\n";
        let a = parse(reply).unwrap();
        assert_eq!(a.len(), 1, "an empty decision is ignored");
        assert!(a[0].blocking && a[0].decision == "tags are trimmed");
        assert_eq!(parse("Done. ```json\n{\"assumptions\": []}\n```"), Some(vec![]));
        assert_eq!(parse("Done, nothing to report."), None, "no listing is reported as such, not as an empty one");
    }

    #[test]
    fn only_blocking_assumptions_become_alarms_and_a_fabricated_quote_is_dropped() {
        let doc = Doc { path: PathBuf::from("d.md"), text: "## Technical plan\n\nTags are stored on each note.\n".into() };
        let corpus = alarm::normalize(&doc.text);
        let list = vec![
            Assumption { decision: "tags are lowercased".into(), section: "Technical plan".into(), quote: "".into(), blocking: true },
            Assumption { decision: "a helper name".into(), blocking: false, ..Default::default() },
            Assumption { decision: "tags are sorted".into(), quote: "Tags are sorted alphabetically".into(), blocking: true, ..Default::default() },
        ];
        let al = alarms(&list, &doc, &corpus, 1);
        assert_eq!(al.len(), 2, "the non-blocking assumption is recorded, not raised");
        assert!(al[0].counts() && al[0].source == Source::Builder && al[0].kind == Kind::Blocking);
        assert!(!al[1].counts() && al[1].dropped_by.as_deref() == Some("grounding"), "its quote is not in the doc");
    }
}
