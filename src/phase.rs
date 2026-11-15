//! The feature lifecycle: the gate that keeps code from being written before a
//! memoryless reviewer has understood the design.
//!
//! This is a line-for-line mirror of `proofs/EgmGate.lean`. The Lean file proves
//! the invariant for every finite event trace; the test at the bottom of this
//! file re-checks the same invariant over every trace up to length 6 so the two
//! models cannot silently diverge.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// The elephant is still shaping the design doc.
    Designing,
    /// A goldfish passed the current revision of the doc.
    Ready,
    /// Code was written from a goldfish-approved doc and its checks passed.
    Built,
    /// The "mean" review found nothing blocking or major.
    Hardened,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The design doc now hashes to this value.
    EditDoc(u64),
    /// A goldfish review finished; `true` means it judged the doc ready.
    Goldfish(bool),
    /// The coding agent finished and the acceptance checks passed.
    Build,
    /// A mean review finished; `true` means no blocking or major findings.
    Review(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub phase: Phase,
    pub doc_hash: u64,
    /// The doc revision the last passing goldfish review saw.
    pub passed_hash: Option<u64>,
}

/// Why an event was refused. A refused event leaves the gate unchanged.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    GoldfishAfterBuild,
    BuildBeforeGoldfish,
    AlreadyBuilt,
    ReviewBeforeBuild,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::GoldfishAfterBuild => "the feature is already built; edit the design doc to reopen it",
            Refusal::BuildBeforeGoldfish => "no goldfish has passed the current design doc; run `egm-dsh goldfish` first",
            Refusal::AlreadyBuilt => "the feature is already built from this revision of the doc",
            Refusal::ReviewBeforeBuild => "there is no code to review yet; run `egm-dsh build` first",
        })
    }
}

impl Gate {
    pub fn new(doc_hash: u64) -> Self {
        Gate { phase: Phase::Designing, doc_hash, passed_hash: None }
    }

    /// Apply one event. Mirrors `step` in `proofs/EgmGate.lean`.
    pub fn step(self, event: Event) -> Result<Gate, Refusal> {
        use Phase::*;
        match event {
            Event::EditDoc(h) if h == self.doc_hash => Ok(self),
            Event::EditDoc(h) => Ok(Gate { phase: Designing, doc_hash: h, passed_hash: None }),
            Event::Goldfish(ok) => match self.phase {
                Designing | Ready => Ok(if ok {
                    Gate { phase: Ready, doc_hash: self.doc_hash, passed_hash: Some(self.doc_hash) }
                } else {
                    Gate { phase: Designing, doc_hash: self.doc_hash, passed_hash: None }
                }),
                Built | Hardened => Err(Refusal::GoldfishAfterBuild),
            },
            Event::Build => match self.phase {
                Ready => Ok(Gate { phase: Built, ..self }),
                Designing => Err(Refusal::BuildBeforeGoldfish),
                Built | Hardened => Err(Refusal::AlreadyBuilt),
            },
            Event::Review(clean) => match self.phase {
                Built | Hardened => Ok(Gate { phase: if clean { Hardened } else { self.phase }, ..self }),
                Designing | Ready => Err(Refusal::ReviewBeforeBuild),
            },
        }
    }

    /// The safety property: past `Designing`, the last passing goldfish review
    /// saw exactly the doc revision in force. Mirrors `Inv` in the Lean file.
    pub fn invariant(&self) -> bool {
        self.phase == Phase::Designing || self.passed_hash == Some(self.doc_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALPHABET: [Event; 7] = [
        Event::EditDoc(1),
        Event::EditDoc(2),
        Event::Goldfish(true),
        Event::Goldfish(false),
        Event::Build,
        Event::Review(true),
        Event::Review(false),
    ];

    /// Bounded model check: every trace of length <= 6 over the alphabet
    /// (7^6 = 117,649 traces) keeps the invariant at every prefix, and `Built`
    /// is only ever entered from `Ready`.
    #[test]
    fn invariant_holds_on_all_short_traces() {
        fn walk(g: Gate, depth: usize, checked: &mut u64) {
            assert!(g.invariant(), "invariant broken at {g:?}");
            *checked += 1;
            if depth == 0 {
                return;
            }
            for e in ALPHABET {
                let next = g.step(e).unwrap_or(g);
                if next.phase == Phase::Built && g.phase != Phase::Built {
                    assert_eq!(g.phase, Phase::Ready, "entered Built from {g:?}");
                }
                walk(next, depth - 1, checked);
            }
        }
        let mut checked = 0;
        walk(Gate::new(1), 6, &mut checked);
        assert!(checked > 100_000);
    }

    #[test]
    fn editing_the_doc_reopens_design() {
        let g = Gate::new(1).step(Event::Goldfish(true)).unwrap().step(Event::Build).unwrap();
        let g = g.step(Event::EditDoc(9)).unwrap();
        assert_eq!(g.phase, Phase::Designing);
        assert_eq!(g.step(Event::Build), Err(Refusal::BuildBeforeGoldfish));
    }

    #[test]
    fn failed_goldfish_revokes_a_pass() {
        let g = Gate::new(1).step(Event::Goldfish(true)).unwrap();
        let g = g.step(Event::Goldfish(false)).unwrap();
        assert_eq!(g.step(Event::Build), Err(Refusal::BuildBeforeGoldfish));
    }
}
