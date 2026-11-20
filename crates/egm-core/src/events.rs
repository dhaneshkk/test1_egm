//! Everything egm-core says goes through one sink: terminal lines (who said what) and domain
//! events (what changed: a feature created, a doc revised, a goldfish verdict, a build, a gate
//! verdict). The CLI's default sink prints the lines exactly as before and ignores domain events;
//! the web app's worker installs a sink that streams both to the browser and writes the domain
//! events to the audit log.
//!
//! The sink is process-wide, so lines from the goldfish threads reach it too. The worker runs one
//! job per process, so one process never mixes two jobs' events.

use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::sync::{Arc, RwLock};

/// Who a terminal line comes from (the column the CLI prints it under).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Elephant,
    Goldfish,
    Egm,
    Ok,
    Warn,
    Tool,
    Block,
    Banner,
    /// Plain output: tables, JSON, a rendered document.
    Out,
}

/// A change to a feature that the team app records and the audit log keeps.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DomainEvent {
    /// Event type, such as `feature.created`; [`TYPES`] lists every one.
    pub kind: &'static str,
    /// The feature's slug, or empty for a project-wide event.
    pub feature: String,
    /// Hashes, counts and verdicts; never source code or document text.
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Line { speaker: Speaker, text: String },
    Domain(DomainEvent),
}

/// Every domain event type egm-core emits, in workflow order.
pub const TYPES: &[&str] = &[
    "feature.created",
    "doc.revised",
    "goldfish.verdict",
    "decision.recorded",
    "criteria.frozen",
    "build.completed",
    "review.completed",
    "gate.verdict",
];

/// The domain event for a gate transition, with the phase it led to. Emitted only when the
/// transition changed the gate, so replaying these events through [`crate::phase::Gate::step`]
/// rebuilds the phase the CLI shows (`EgmReplay` in `proofs/EgmReplay.lean`).
pub fn gate_event(feature: &str, ev: crate::phase::Event, after: &crate::phase::Gate) {
    use crate::phase::Event as G;
    let phase = serde_json::to_value(after.phase).unwrap_or(Value::Null);
    let (kind, data) = match ev {
        G::EditDoc(h) => ("doc.revised", serde_json::json!({"doc_hash": h.to_string(), "phase": phase})),
        G::Goldfish(ready) => ("goldfish.verdict", serde_json::json!({"ready": ready, "doc_hash": after.doc_hash.to_string(), "phase": phase})),
        G::Build => ("build.completed", serde_json::json!({"doc_hash": after.doc_hash.to_string(), "phase": phase})),
        G::Review(clean) => ("review.completed", serde_json::json!({"clean": clean, "doc_hash": after.doc_hash.to_string(), "phase": phase})),
    };
    emit(kind, feature, data);
}

/// Read a gate event back from a domain event (the inverse of [`gate_event`]); `None` for
/// events that are not gate transitions.
pub fn gate_event_of(d: &DomainEvent) -> Option<crate::phase::Event> {
    use crate::phase::Event as G;
    let hash = || d.data.get("doc_hash")?.as_str()?.parse::<u64>().ok();
    match d.kind {
        "doc.revised" => Some(G::EditDoc(hash()?)),
        "goldfish.verdict" => Some(G::Goldfish(d.data.get("ready")?.as_bool()?)),
        "build.completed" => Some(G::Build),
        "review.completed" => Some(G::Review(d.data.get("clean")?.as_bool()?)),
        _ => None,
    }
}

pub type Sink = Arc<dyn Fn(&Event) + Send + Sync>;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

/// Install a sink for the whole process; `None` restores the terminal printer.
pub fn set_sink(sink: Option<Sink>) {
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = sink;
}

/// Run `f` with `sink` installed, then restore whatever was installed before.
pub fn with_sink<T>(sink: Sink, f: impl FnOnce() -> T) -> T {
    let before = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    set_sink(Some(sink));
    let out = f();
    set_sink(before);
    out
}

pub(crate) fn send(ev: Event) {
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    match sink {
        Some(s) => s(&ev),
        None => print_terminal(&ev),
    }
}

/// Emit a domain event. `kind` must be one of [`TYPES`].
pub fn emit(kind: &'static str, feature: &str, data: Value) {
    debug_assert!(TYPES.contains(&kind), "unknown event type {kind}");
    send(Event::Domain(DomainEvent { kind, feature: feature.to_string(), data }));
}

/// A sink that appends every event as one JSON line to `path` and still prints the terminal
/// lines (the CLI's `--events` flag).
pub fn jsonl_sink(path: &std::path::Path) -> std::io::Result<Sink> {
    let f = std::sync::Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(path)?);
    Ok(Arc::new(move |ev: &Event| {
        if let Ok(mut f) = f.lock()
            && let Ok(line) = serde_json::to_string(ev)
        {
            let _ = writeln!(f, "{line}");
        }
        if matches!(ev, Event::Line { .. }) {
            print_terminal(ev);
        }
    }))
}

/// A sink that keeps every event in memory (tests, and the worker's per-job buffer).
pub fn collector() -> (Sink, Arc<std::sync::Mutex<Vec<Event>>>) {
    let store = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = store.clone();
    (Arc::new(move |ev: &Event| s2.lock().unwrap_or_else(|e| e.into_inner()).push(ev.clone())), store)
}

/// The terminal printer: byte-for-byte what egm-dsh printed before the library split.
pub fn print_terminal(ev: &Event) {
    use crate::ui::paint;
    let Event::Line { speaker, text } = ev else { return };
    let msg = text.as_str();
    match speaker {
        Speaker::Elephant => println!("{} {msg}", paint("1;35", "🐘 elephant │")),
        Speaker::Goldfish => println!("{} {msg}", paint("1;33", "🐟 goldfish │")),
        Speaker::Egm => println!("{} {msg}", paint("1;36", "   egm-dsh  │")),
        Speaker::Ok => println!("{} {msg}", paint("1;32", "   ✔        │")),
        Speaker::Warn => eprintln!("{} {msg}", paint("1;31", "   ✖        │")),
        Speaker::Tool => eprintln!("{}", paint("2", msg)),
        Speaker::Block => {
            for line in msg.trim_matches('\n').trim_end().lines() {
                println!("             {line}");
            }
        }
        Speaker::Banner => println!("\n{}", paint("1;7", &format!(" {msg} "))),
        Speaker::Out => println!("{msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_sees_lines_and_domain_events_in_order() {
        let (sink, got) = collector();
        with_sink(sink, || {
            crate::ui::egm("hello");
            emit("feature.created", "tags", serde_json::json!({"idea_sha256": "ab"}));
            crate::ui::out("a table row");
        });
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], Event::Line { speaker: Speaker::Egm, text: "hello".into() });
        assert!(matches!(&got[1], Event::Domain(d) if d.kind == "feature.created" && d.feature == "tags"));
        assert_eq!(got[2], Event::Line { speaker: Speaker::Out, text: "a table row".into() });
    }

    #[test]
    fn events_serialize_with_a_type_tag() {
        let e = Event::Domain(DomainEvent { kind: "gate.verdict", feature: "f".into(), data: serde_json::json!({"pass": true}) });
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""type":"domain""#) && s.contains(r#""kind":"gate.verdict""#), "{s}");
    }
}
