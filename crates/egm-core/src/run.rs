//! Run budgets and agent sessions: the run state machine.
//!
//! A line-for-line mirror of the `EgmRun` section of `proofs/EgmGate.lean`
//! (`RState`, `REvent`, `rstep`, `rank`). `session.rs` executes this step
//! function for every activity and agent session egm starts, so what Lean
//! proves about `rstep` is what the CLI does:
//!
//! * the gate part of any run is the gate (`built_means_goldfish_passed` holds);
//! * `rank` strictly drops on every accepted event, so no run is infinite;
//! * a state that accepts nothing has no live session and no open activity;
//! * spent + left = initial for every budget, so no budget is overdrawn.

use serde::{Deserialize, Serialize};

use crate::phase::{Event, Gate};

/// One command's model work. Mirrors `Kind` in Lean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Elephant,
    Goldfish,
    Build,
    Tests,
    Review,
}

/// Which budget an activity draws on. Mirrors `Counter` / `Kind.counter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    Redraft,
    Goldfish,
    Build,
    Review,
}

impl Kind {
    pub fn counter(self) -> Counter {
        match self {
            Kind::Elephant => Counter::Redraft,
            Kind::Goldfish => Counter::Goldfish,
            Kind::Build | Kind::Tests => Counter::Build,
            Kind::Review => Counter::Review,
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Elephant => "elephant turn",
            Kind::Goldfish => "goldfish round",
            Kind::Build => "build attempt",
            Kind::Tests => "test-writing run (counts as a build attempt)",
            Kind::Review => "mean review",
        }
    }
}

/// What is LEFT of each budget. Mirrors `Budget`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub redraft: u32,
    pub goldfish: u32,
    pub build: u32,
    pub review: u32,
    pub calls: u32,
}

impl Budget {
    pub fn get(&self, c: Counter) -> u32 {
        match c {
            Counter::Redraft => self.redraft,
            Counter::Goldfish => self.goldfish,
            Counter::Build => self.build,
            Counter::Review => self.review,
        }
    }

    /// `Budget.spend`. Only called when `get(c) > 0` (Lean: `0 < get`), so it never saturates.
    fn spend(mut self, c: Counter) -> Budget {
        match c {
            Counter::Redraft => self.redraft -= 1,
            Counter::Goldfish => self.goldfish -= 1,
            Counter::Build => self.build -= 1,
            Counter::Review => self.review -= 1,
        }
        self
    }

    pub fn total(&self) -> u64 {
        self.redraft as u64 + self.goldfish as u64 + self.build as u64 + self.review as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunState {
    pub gate: Gate,
    pub budget: Budget,
    /// Ids of agent sessions that are open.
    pub live: Vec<u64>,
    pub next: u64,
    pub active: Option<Kind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEvent {
    Begin(Kind),
    OpenSession,
    Close(u64),
    Finish(Option<Event>),
    Cancel,
}

/// Why a run event was refused. A refused event leaves the state unchanged.
#[derive(Debug, PartialEq, Eq)]
pub enum RunRefusal {
    ActivityInProgress(Kind),
    BudgetSpent(Kind),
    NoActivity,
    CallsSpent,
    UnknownSession(u64),
    SessionsStillOpen(usize),
    WrongOutcome(Kind),
}

impl std::fmt::Display for RunRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunRefusal::ActivityInProgress(k) => write!(f, "a {} is still in progress", k.noun()),
            RunRefusal::BudgetSpent(k) => write!(f, "the {} budget is used up", k.noun()),
            RunRefusal::NoActivity => f.write_str("no activity is in progress"),
            RunRefusal::CallsSpent => f.write_str("the model-call budget is used up"),
            RunRefusal::UnknownSession(id) => write!(f, "session {id} is not open"),
            RunRefusal::SessionsStillOpen(n) => write!(f, "{n} agent session(s) are still open"),
            RunRefusal::WrongOutcome(k) => write!(f, "a {} cannot produce that gate event", k.noun()),
        }
    }
}

/// `fits`: which gate event an activity may produce; `None` always fits.
pub fn fits(k: Kind, o: Option<Event>) -> bool {
    matches!(
        (k, o),
        (_, None)
            | (Kind::Elephant, Some(Event::EditDoc(_)))
            | (Kind::Goldfish, Some(Event::Goldfish(_)))
            | (Kind::Build, Some(Event::Build))
            // the builder had to make a decision the doc left open: design reopens (never opens the gate)
            | (Kind::Build, Some(Event::Goldfish(false)))
            | (Kind::Review, Some(Event::Review(_)))
    )
}

impl RunState {
    pub fn new(gate: Gate, budget: Budget) -> Self {
        RunState { gate, budget, live: vec![], next: 0, active: None }
    }

    /// `rstep`. Line for line with the Lean definition.
    pub fn step(&self, e: RunEvent) -> Result<RunState, RunRefusal> {
        let mut s = self.clone();
        match e {
            RunEvent::Begin(k) => {
                if let Some(a) = s.active {
                    return Err(RunRefusal::ActivityInProgress(a));
                }
                if s.budget.get(k.counter()) == 0 {
                    return Err(RunRefusal::BudgetSpent(k));
                }
                s.budget = s.budget.spend(k.counter());
                s.active = Some(k);
            }
            RunEvent::OpenSession => {
                if s.active.is_none() {
                    return Err(RunRefusal::NoActivity);
                }
                if s.budget.calls == 0 {
                    return Err(RunRefusal::CallsSpent);
                }
                s.budget.calls -= 1;
                s.live.insert(0, s.next); // `s.next :: s.live`
                s.next += 1;
            }
            RunEvent::Close(id) => {
                // `List.erase`: removes the first occurrence.
                let Some(i) = s.live.iter().position(|x| *x == id) else { return Err(RunRefusal::UnknownSession(id)) };
                s.live.remove(i);
            }
            RunEvent::Finish(o) => {
                let Some(k) = s.active else { return Err(RunRefusal::NoActivity) };
                if !s.live.is_empty() {
                    return Err(RunRefusal::SessionsStillOpen(s.live.len()));
                }
                if !fits(k, o) {
                    return Err(RunRefusal::WrongOutcome(k));
                }
                s.active = None;
                if let Some(ev) = o {
                    // `applyO`: a refused gate event leaves the gate as is, like `step` in Lean.
                    s.gate = s.gate.step(ev).unwrap_or(s.gate);
                }
            }
            RunEvent::Cancel => {
                if s.active.is_none() {
                    return Err(RunRefusal::NoActivity);
                }
                if !s.live.is_empty() {
                    return Err(RunRefusal::SessionsStillOpen(s.live.len()));
                }
                s.active = None;
            }
        }
        Ok(s)
    }

    /// `rank`: strictly drops on every accepted event.
    pub fn rank(&self) -> u64 {
        4 * self.budget.total() + 2 * self.budget.calls as u64 + self.live.len() as u64 + u64::from(self.active.is_some())
    }

    /// The gate invariant, carried by every run state.
    pub fn invariant(&self) -> bool {
        self.gate.invariant()
    }

    /// `Stuck`: no event is accepted. Checked against the finite event set that can
    /// matter: every begin, open, closing each live id, each outcome, cancel.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stuck(&self) -> bool {
        let kinds = [Kind::Elephant, Kind::Goldfish, Kind::Build, Kind::Tests, Kind::Review];
        kinds.iter().all(|k| self.step(RunEvent::Begin(*k)).is_err())
            && self.step(RunEvent::OpenSession).is_err()
            && self.live.iter().all(|id| self.step(RunEvent::Close(*id)).is_err())
            && OUTCOMES.iter().all(|o| self.step(RunEvent::Finish(*o)).is_err())
            && self.step(RunEvent::Cancel).is_err()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Every gate outcome an activity can report (two doc hashes are enough to tell edits apart).
pub const OUTCOMES: [Option<Event>; 8] = [
    None,
    Some(Event::EditDoc(1)),
    Some(Event::EditDoc(2)),
    Some(Event::Goldfish(true)),
    Some(Event::Goldfish(false)),
    Some(Event::Build),
    Some(Event::Review(true)),
    Some(Event::Review(false)),
];

/// Per-feature limits, set in `.egm-dsh/config.json` (`budgets`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub goldfish_rounds: u32,
    /// Elephant conversation turns and draft runs.
    pub redraft_turns: u32,
    /// Build attempts: the first build plus retries. Test-writing runs count here too.
    pub build_attempts: u32,
    pub reviews: u32,
    /// Agent sessions; each dsh run or chat completion egm starts is one call.
    pub model_calls: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { goldfish_rounds: 10, redraft_turns: 40, build_attempts: 6, reviews: 6, model_calls: 300 }
    }
}

/// What a feature has used so far, persisted in `.egm-dsh/spent/<feature>.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spent {
    pub goldfish_rounds: u32,
    pub redraft_turns: u32,
    pub build_attempts: u32,
    pub reviews: u32,
    pub model_calls: u32,
}

impl Spent {
    /// What is left. Lowering a limit below what is spent leaves zero, never a negative.
    pub fn left(&self, l: &Limits) -> Budget {
        Budget {
            redraft: l.redraft_turns.saturating_sub(self.redraft_turns),
            goldfish: l.goldfish_rounds.saturating_sub(self.goldfish_rounds),
            build: l.build_attempts.saturating_sub(self.build_attempts),
            review: l.reviews.saturating_sub(self.reviews),
            calls: l.model_calls.saturating_sub(self.model_calls),
        }
    }

    pub fn charge(&mut self, c: Counter) {
        match c {
            Counter::Redraft => self.redraft_turns += 1,
            Counter::Goldfish => self.goldfish_rounds += 1,
            Counter::Build => self.build_attempts += 1,
            Counter::Review => self.reviews += 1,
        }
    }
}

/// The flag that raises the limit a refusal hit.
pub fn raise_hint(k: Option<Kind>) -> &'static str {
    match k.map(Kind::counter) {
        Some(Counter::Goldfish) => "--goldfish-rounds",
        Some(Counter::Redraft) => "--redraft-turns",
        Some(Counter::Build) => "--build-attempts",
        Some(Counter::Review) => "--reviews",
        None => "--model-calls",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::Phase;
    use std::collections::{HashMap, HashSet};

    const KINDS: [Kind; 5] = [Kind::Elephant, Kind::Goldfish, Kind::Build, Kind::Tests, Kind::Review];

    /// Every event that can be accepted somewhere, given the live ids of a state.
    fn events(s: &RunState) -> Vec<RunEvent> {
        let mut v: Vec<RunEvent> = KINDS.iter().map(|k| RunEvent::Begin(*k)).collect();
        v.push(RunEvent::OpenSession);
        v.extend(s.live.iter().map(|id| RunEvent::Close(*id)));
        v.push(RunEvent::Close(999)); // never open: always refused
        v.extend(OUTCOMES.iter().map(|o| RunEvent::Finish(*o)));
        v.push(RunEvent::Cancel);
        v
    }

    /// Check one accepted or refused step against everything Lean proves about `rstep`.
    fn check_step(s: &RunState, e: RunEvent, n: &mut u64) -> Option<RunState> {
        *n += 1;
        match s.step(e) {
            Ok(t) => {
                assert!(t.invariant(), "(a) gate invariant broken by {e:?} from {s:?}");
                assert!(t.rank() < s.rank(), "(b) rank did not drop on {e:?}: {} -> {}", s.rank(), t.rank());
                // (d) exact accounting for this step
                for c in [Counter::Redraft, Counter::Goldfish, Counter::Build, Counter::Review] {
                    let charged = matches!(e, RunEvent::Begin(k) if k.counter() == c) as u32;
                    assert_eq!(charged + t.budget.get(c), s.budget.get(c), "(d) {c:?} on {e:?}");
                }
                assert_eq!((e == RunEvent::OpenSession) as u32 + t.budget.calls, s.budget.calls, "(d) calls on {e:?}");
                // the gate part moves only by the gate's own step function
                match e {
                    RunEvent::Finish(Some(ev)) => assert_eq!(t.gate, s.gate.step(ev).unwrap_or(s.gate)),
                    _ => assert_eq!(t.gate, s.gate),
                }
                Some(t)
            }
            Err(_) => None,
        }
    }

    fn start(b: Budget) -> RunState {
        RunState::new(Gate::new(1), b)
    }

    /// Bounded model check, the run-level counterpart of the gate's 117,649-trace
    /// check: EVERY event sequence up to length 6, from a budget of one of each
    /// activity and three calls, with every property checked at every step.
    #[test]
    fn all_short_run_traces_keep_every_property() {
        fn walk(s: &RunState, depth: usize, steps: &mut u64, traces: &mut u64) {
            if s.stuck() {
                assert!(s.live.is_empty() && s.active.is_none(), "(c) stuck with live sessions: {s:?}");
            }
            if depth == 0 {
                *traces += 1;
                return;
            }
            for e in events(s) {
                match check_step(s, e, steps) {
                    Some(t) => walk(&t, depth - 1, steps, traces),
                    None => walk(s, depth - 1, steps, traces), // refused: state unchanged
                }
            }
        }
        let (mut steps, mut traces) = (0, 0);
        walk(&start(Budget { redraft: 1, goldfish: 1, build: 1, review: 1, calls: 3 }), 6, &mut steps, &mut traces);
        eprintln!("run model check: {traces} traces of length 6, {steps} steps checked");
        assert!(traces > 10_000_000, "only {traces} traces");
    }

    /// Exhaustive reachability: every state reachable from these budgets, every
    /// event from each. Every stuck state is clean, a build is reached only
    /// through a passing goldfish, and the longest run from the start (computed
    /// over the whole finite graph) is at most the initial rank: no run is infinite.
    #[test]
    fn every_reachable_state_terminates_cleanly() {
        fn longest(s: &RunState, memo: &mut HashMap<String, u64>, seen: &mut HashSet<String>, steps: &mut u64, built: &mut u64) -> u64 {
            let k = format!("{s:?}");
            if let Some(v) = memo.get(&k) {
                return *v;
            }
            if seen.insert(k.clone()) {
                if s.stuck() {
                    assert!(s.live.is_empty() && s.active.is_none(), "(c) {s:?}");
                }
                if matches!(s.gate.phase, Phase::Built | Phase::Hardened) {
                    *built += 1;
                    assert_eq!(s.gate.passed_hash, Some(s.gate.doc_hash), "(a) built without a passing goldfish");
                }
            }
            let mut best = 0;
            for e in events(s) {
                if let Some(t) = check_step(s, e, steps) {
                    best = best.max(1 + longest(&t, memo, seen, steps, built));
                }
            }
            memo.insert(k, best);
            best
        }
        for b in [
            Budget { redraft: 2, goldfish: 2, build: 1, review: 1, calls: 3 },
            Budget { redraft: 1, goldfish: 1, build: 2, review: 0, calls: 2 },
            Budget { redraft: 0, goldfish: 0, build: 0, review: 0, calls: 5 },
        ] {
            let s0 = start(b);
            let (mut memo, mut seen, mut steps, mut built) = (HashMap::new(), HashSet::new(), 0, 0);
            let l = longest(&s0, &mut memo, &mut seen, &mut steps, &mut built);
            assert!(l <= s0.rank(), "longest run {l} exceeds the initial rank {}", s0.rank());
            assert!(!seen.is_empty());
            eprintln!("reachability from {b:?}: {} states, {steps} steps checked, longest run {l} <= rank {}, {built} built states", seen.len(), s0.rank());
            if b.goldfish > 0 && b.build > 0 && b.calls >= 2 {
                assert!(built > 0, "a build should be reachable from {b:?}");
            }
        }
    }

    #[test]
    fn refusals_name_the_budget_and_leave_state_unchanged() {
        let s = start(Budget { redraft: 0, goldfish: 1, build: 0, review: 0, calls: 1 });
        assert_eq!(s.step(RunEvent::Begin(Kind::Build)), Err(RunRefusal::BudgetSpent(Kind::Build)));
        let s = s.step(RunEvent::Begin(Kind::Goldfish)).unwrap();
        assert_eq!(s.step(RunEvent::Begin(Kind::Review)), Err(RunRefusal::ActivityInProgress(Kind::Goldfish)));
        let s = s.step(RunEvent::OpenSession).unwrap();
        assert_eq!(s.step(RunEvent::OpenSession), Err(RunRefusal::CallsSpent));
        assert_eq!(s.step(RunEvent::Finish(Some(Event::Goldfish(true)))), Err(RunRefusal::SessionsStillOpen(1)));
        assert_eq!(s.step(RunEvent::Finish(Some(Event::Build))), Err(RunRefusal::SessionsStillOpen(1)));
        let s = s.step(RunEvent::Close(s.live[0])).unwrap();
        assert_eq!(s.step(RunEvent::Finish(Some(Event::Build))), Err(RunRefusal::WrongOutcome(Kind::Goldfish)));
        let s = s.step(RunEvent::Finish(Some(Event::Goldfish(true)))).unwrap();
        assert_eq!(s.gate.phase, Phase::Ready);
        assert!(s.stuck(), "everything is spent");
    }

    #[test]
    fn lowering_a_limit_below_spent_leaves_zero() {
        let spent = Spent { goldfish_rounds: 7, ..Spent::default() };
        let l = Limits { goldfish_rounds: 5, ..Limits::default() };
        assert_eq!(spent.left(&l).goldfish, 0);
    }
}
