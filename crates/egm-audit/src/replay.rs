//! Rebuild a feature's gate from its audit events.
//!
//! egm-core emits a domain event for every gate transition that changed the gate
//! ([`egm_core::events::gate_event`]). Folding a feature's events through the same
//! [`Gate::step`] the CLI uses gives back the gate the CLI saved, so the phase the web app shows
//! can always be rebuilt from Vesdb alone. A transition the gate would refuse cannot appear in
//! a log egm-core wrote; finding one means the log was written by something else, and replay
//! stops there. `proofs/EgmReplay.lean` proves both facts for every finite trace.

use crate::Written;
use egm_core::events::{gate_event_of, DomainEvent};
use egm_core::phase::{Gate, Refusal};

#[derive(Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// The first event of the feature is not `feature.created`.
    NotCreatedFirst { id: u64 },
    /// A gate event the gate refuses: this log was not written by egm-core.
    Refused { id: u64, refusal: Refusal },
    /// A gate event without the fields replay needs.
    Unreadable { id: u64 },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::NotCreatedFirst { id } => write!(f, "event {id}: a feature's first event must be feature.created"),
            ReplayError::Refused { id, refusal } => write!(f, "event {id}: the gate refuses this transition ({refusal})"),
            ReplayError::Unreadable { id } => write!(f, "event {id}: missing the fields replay needs"),
        }
    }
}

impl std::error::Error for ReplayError {}

/// Replay one feature's events, oldest first. `None` when the feature has no events yet.
pub fn gate(events: &[Written]) -> Result<Option<Gate>, ReplayError> {
    let mut g: Option<Gate> = None;
    for w in events {
        // Only egm-core's own record of the feature takes part: the app's events (a job queued
        // before the feature exists, comments, reviews) and usage counts never move the gate.
        if !egm_core::events::TYPES.contains(&w.kind.as_str()) || w.kind == "usage.recorded" {
            continue;
        }
        match (&mut g, w.kind.as_str()) {
            (None, "feature.created") => {
                let h = w.data.get("doc_hash").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok()).ok_or(ReplayError::Unreadable { id: w.id })?;
                g = Some(Gate::new(h));
            }
            (None, _) => return Err(ReplayError::NotCreatedFirst { id: w.id }),
            (Some(cur), kind) => {
                let Some(kind) = egm_core::events::TYPES.iter().find(|t| **t == kind) else { continue };
                let d = DomainEvent { kind, feature: String::new(), data: w.data.clone() };
                match gate_event_of(&d) {
                    Some(ev) => *cur = cur.step(ev).map_err(|refusal| ReplayError::Refused { id: w.id, refusal })?,
                    None if ["doc.revised", "goldfish.verdict", "review.completed"].contains(kind) => return Err(ReplayError::Unreadable { id: w.id }),
                    None => {}
                }
            }
        }
    }
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egm_core::phase::{Event, Phase};
    use serde_json::json;

    fn w(id: u64, kind: &str, data: serde_json::Value) -> Written {
        Written { id, subject: "/egm/jot/tags".into(), kind: kind.into(), actor: "worker".into(), data, time: String::new() }
    }

    #[test]
    fn replays_a_whole_feature() {
        let evs = [
            w(0, "feature.created", json!({"doc_hash": "1"})),
            w(1, "doc.revised", json!({"doc_hash": "2", "phase": "designing"})),
            w(2, "goldfish.verdict", json!({"ready": true, "doc_hash": "2"})),
            w(3, "criteria.frozen", json!({})),
            w(4, "build.completed", json!({"doc_hash": "2"})),
            w(5, "gate.verdict", json!({"pass": true})),
            w(6, "review.completed", json!({"clean": true, "doc_hash": "2"})),
        ];
        let g = gate(&evs).unwrap().unwrap();
        assert_eq!(g, Gate::new(1).step(Event::EditDoc(2)).unwrap().step(Event::Goldfish(true)).unwrap().step(Event::Build).unwrap().step(Event::Review(true)).unwrap());
        assert_eq!(g.phase, Phase::Hardened);
    }

    #[test]
    fn app_events_and_usage_are_not_part_of_the_trace() {
        let evs = [
            w(0, "job.requested", json!({"command": "elephant"})),
            w(1, "job.started", json!({"job": 0})),
            w(2, "feature.created", json!({"doc_hash": "1"})),
            w(3, "usage.recorded", json!({"tokens": 10})),
            w(4, "comment.added", json!({"text": "x"})),
            w(5, "job.finished", json!({"job": 0})),
        ];
        assert_eq!(gate(&evs).unwrap().unwrap(), Gate::new(1));
        assert_eq!(gate(&[w(0, "usage.recorded", json!({})), w(1, "build.completed", json!({}))]), Err(ReplayError::NotCreatedFirst { id: 1 }));
    }

    #[test]
    fn a_refused_transition_stops_replay() {
        let evs = [w(0, "feature.created", json!({"doc_hash": "1"})), w(1, "build.completed", json!({}))];
        assert_eq!(gate(&evs), Err(ReplayError::Refused { id: 1, refusal: Refusal::BuildBeforeGoldfish }));
        assert_eq!(gate(&evs[1..]), Err(ReplayError::NotCreatedFirst { id: 1 }));
        assert_eq!(gate(&[]), Ok(None));
    }

    /// Replay through the events egm-core really emits: run gate transitions with a collector
    /// sink installed, then replay what it caught.
    #[test]
    fn replay_matches_the_gate_egm_core_kept() {
        use egm_core::events::{collector, gate_event, with_sink, Event as Ev};
        let (sink, got) = collector();
        let trace = [Event::EditDoc(5), Event::Goldfish(false), Event::Goldfish(true), Event::Build, Event::Review(false), Event::EditDoc(6), Event::Goldfish(true)];
        let mut live = Gate::new(4);
        with_sink(sink, || {
            egm_core::events::emit("feature.created", "tags", json!({"doc_hash": "4"}));
            for e in trace {
                let next = live.step(e).unwrap_or(live);
                if next != live {
                    gate_event("tags", e, &next);
                }
                live = next;
            }
        });
        let evs: Vec<Written> = got
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Ev::Domain(d) => Some(w(i as u64, d.kind, d.data.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(gate(&evs).unwrap(), Some(live));
    }
}
