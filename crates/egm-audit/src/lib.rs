//! egm-team's audit log. Every change egm-core reports (a [`DomainEvent`]) and every action a
//! person takes in the web app becomes an [`AuditEvent`], written to Vesdb under the subject
//! `/egm/<project>/<feature>`. Vesdb keeps it in a Merkle log with signed receipts and
//! witness-cosigned checkpoints, so the record cannot be changed without detection.
//!
//! [`AuditLog`] is the seam: [`VesdbLog`] talks to a Vesdb server over its
//! EventSourcingDB-compatible HTTP API; [`MemoryLog`] is the same contract in memory, for tests.
//! [`replay`] rebuilds a feature's gate phase from its audit events.

use egm_core::events::DomainEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

pub mod replay;

/// Every event type egm-team writes, beyond egm-core's own (see [`egm_core::events::TYPES`]).
/// The app's actions: people, comments, reviews, approvals and refusals.
pub const APP_TYPES: &[&str] = &[
    "auth.signed-in",
    "auth.sign-in-failed",
    "member.role-changed",
    "project.settings-changed",
    "alarm.labelled",
    "comment.added",
    "review.submitted",
    "merge.approved",
    "policy.refused",
    "export.created",
];

/// The prefix of every egm event type in Vesdb, so `feature.created` is stored as
/// `io.egm.feature.created`.
pub const TYPE_PREFIX: &str = "io.egm.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Without the prefix, such as `gate.verdict`.
    pub kind: String,
    pub project: String,
    /// Empty for a project-wide event.
    pub feature: String,
    /// Who did it: a user id, or `worker` for egm-core running a job.
    pub actor: String,
    /// Hashes, counts and verdicts; never source code, document text or personal data beyond ids.
    pub data: Value,
}

impl AuditEvent {
    pub fn subject(&self) -> String {
        subject(&self.project, (!self.feature.is_empty()).then_some(self.feature.as_str()))
    }

    pub fn event_type(&self) -> String {
        format!("{TYPE_PREFIX}{}", self.kind)
    }

    /// Wrap a change egm-core reported while running a job for `actor`.
    pub fn from_domain(project: &str, actor: &str, d: &DomainEvent) -> Self {
        AuditEvent { kind: d.kind.to_string(), project: project.into(), feature: d.feature.clone(), actor: actor.into(), data: d.data.clone() }
    }

    fn payload(&self) -> Value {
        json!({ "actor": self.actor, "data": self.data })
    }
}

/// The Vesdb subject for a project, or one feature in it.
pub fn subject(project: &str, feature: Option<&str>) -> String {
    match feature {
        Some(f) => format!("/egm/{project}/{f}"),
        None => format!("/egm/{project}"),
    }
}

/// An event as Vesdb stored it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Written {
    /// Vesdb's event id: the event's position in the Merkle log.
    pub id: u64,
    pub subject: String,
    /// Without the `io.egm.` prefix.
    pub kind: String,
    pub actor: String,
    pub data: Value,
    pub time: String,
}

#[derive(Debug)]
pub enum AuditError {
    /// The log refused the write (bad input, a failed precondition, no permission).
    Refused(String),
    /// The log could not be reached, or stayed unavailable through every retry.
    Unavailable(String),
    /// The log answered with something this client cannot read.
    Protocol(String),
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditError::Refused(m) => write!(f, "the audit log refused the write: {m}"),
            AuditError::Unavailable(m) => write!(f, "the audit log is unavailable: {m}"),
            AuditError::Protocol(m) => write!(f, "unexpected answer from the audit log: {m}"),
        }
    }
}

impl std::error::Error for AuditError {}

pub trait AuditLog: Send + Sync {
    /// Append `events` atomically. `key` names this write: repeating a write with the same key
    /// and the same events returns the first result and records nothing new.
    fn append(&self, key: &str, events: &[AuditEvent]) -> Result<Vec<Written>, AuditError>;

    /// Every event of a project (`feature: None`, including all features) or of one feature,
    /// oldest first.
    fn history(&self, project: &str, feature: Option<&str>) -> Result<Vec<Written>, AuditError>;
}

/// An idempotency key for the `seq`-th write of job `job`: stable across retries and
/// restarts, unique across jobs.
pub fn key(job: &str, seq: u64) -> String {
    format!("egm/{job}/{seq}")
}

// ------------------------------------------------------------------------------------- memory

/// The [`AuditLog`] contract in memory: ids from 0, idempotency keys honoured.
/// Idempotency key → (request body, the answer it got).
type Answers = HashMap<String, (String, Vec<Written>)>;

#[derive(Default)]
pub struct MemoryLog {
    inner: Mutex<(Vec<Written>, Answers)>,
}

impl MemoryLog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AuditLog for MemoryLog {
    fn append(&self, key: &str, events: &[AuditEvent]) -> Result<Vec<Written>, AuditError> {
        let body = serde_json::to_string(events).map_err(|e| AuditError::Protocol(e.to_string()))?;
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((b, out)) = g.1.get(key) {
            if *b != body {
                return Err(AuditError::Refused(format!("idempotency key {key} reused with a different request")));
            }
            return Ok(out.clone());
        }
        let mut out = Vec::new();
        for e in events {
            let w = Written { id: g.0.len() as u64, subject: e.subject(), kind: e.kind.clone(), actor: e.actor.clone(), data: e.data.clone(), time: String::new() };
            g.0.push(w.clone());
            out.push(w);
        }
        g.1.insert(key.to_string(), (body, out.clone()));
        Ok(out)
    }

    fn history(&self, project: &str, feature: Option<&str>) -> Result<Vec<Written>, AuditError> {
        let s = subject(project, feature);
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(g.0.iter().filter(|w| w.subject == s || (feature.is_none() && w.subject.starts_with(&format!("{s}/")))).cloned().collect())
    }
}

// -------------------------------------------------------------------------------------- vesdb

/// A Vesdb server, through its EventSourcingDB-compatible HTTP API. Reads use `POST`, which
/// Vesdb accepts wherever it accepts `QUERY` (HTTP clients refuse the `QUERY` method on HTTP/1.1).
///
/// Writes carry an `Idempotency-Key`. A `503` (a standby, or a failover in progress) and a
/// connection error are retried with the same key and the same body, waiting `Retry-After`
/// (default 1 s), for up to `retry_for`; so a failover neither loses nor duplicates an action.
///
/// `base` is one URL (normally the load balancer) or several separated by commas (the Vesdb
/// servers themselves, for a small install without a load balancer): a `503` or a connection
/// error moves on to the next one.
pub struct VesdbLog {
    bases: Vec<String>,
    current: std::sync::atomic::AtomicUsize,
    token: String,
    /// CloudEvents `source` for every event, such as `https://egm-team.example`.
    source: String,
    agent: ureq::Agent,
    pub retry_for: Duration,
}

impl VesdbLog {
    pub fn new(base: &str, token: &str, source: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let bases = base.split(',').map(|b| b.trim().trim_end_matches('/').to_string()).filter(|b| !b.is_empty()).collect();
        VesdbLog { bases, current: Default::default(), token: token.into(), source: source.into(), agent, retry_for: Duration::from_secs(60) }
    }

    fn call(&self, method: &str, path: &str, body: &str, idem: Option<&str>) -> Result<(u16, String), AuditError> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.bases.is_empty() {
            return Err(AuditError::Unavailable("no Vesdb URL configured".into()));
        }
        let deadline = std::time::Instant::now() + self.retry_for;
        loop {
            let i = self.current.load(Relaxed) % self.bases.len();
            let mut b = ureq::http::Request::builder()
                .method(method)
                .uri(format!("{}{path}", self.bases[i]))
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Content-Type", "application/json");
            if let Some(k) = idem {
                b = b.header("Idempotency-Key", k);
            }
            let req = b.body(body.to_string()).map_err(|e| AuditError::Protocol(e.to_string()))?;
            let wait = match self.agent.run(req) {
                Ok(mut resp) => {
                    let status = resp.status().as_u16();
                    let after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
                    let text = resp.body_mut().read_to_string().map_err(|e| AuditError::Protocol(e.to_string()))?;
                    if status != 503 {
                        return Ok((status, text));
                    }
                    Duration::from_secs(after.unwrap_or(1).clamp(1, 10))
                }
                // connection refused or reset: the writer may be dying; the load balancer moves on
                Err(e) if is_transport(&e) => Duration::from_millis(500),
                Err(e) => return Err(AuditError::Protocol(e.to_string())),
            };
            self.current.store(i + 1, Relaxed); // try the next server
            if std::time::Instant::now() + wait > deadline {
                return Err(AuditError::Unavailable(format!("{method} {path}: still unavailable after {:?}", self.retry_for)));
            }
            std::thread::sleep(wait);
        }
    }

    /// The signed receipt and inclusion proof for one event (needs the `audit` scope).
    pub fn receipt(&self, id: u64) -> Result<Value, AuditError> {
        let (s, t) = self.call("POST", "/api/v2/tlog/receipt", &json!({"id": id.to_string()}).to_string(), None)?;
        ok_json(s, &t)
    }

    /// An offline evidence bundle for these events, against the latest cosigned checkpoint
    /// (needs the `audit` scope). An auditor checks it with `vesdb-verify`, trusting no server.
    pub fn bundle(&self, ids: &[u64]) -> Result<Value, AuditError> {
        let body = json!({"ids": ids.iter().map(u64::to_string).collect::<Vec<_>>()}).to_string();
        let (s, t) = self.call("POST", "/api/v2/tlog/bundle", &body, None)?;
        ok_json(s, &t)
    }
}

fn is_transport(e: &ureq::Error) -> bool {
    matches!(e, ureq::Error::Io(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound | ureq::Error::Timeout(_))
}

fn ok_json(status: u16, text: &str) -> Result<Value, AuditError> {
    if status != 200 {
        return Err(AuditError::Refused(format!("HTTP {status}: {}", text.trim())));
    }
    serde_json::from_str(text).map_err(|e| AuditError::Protocol(e.to_string()))
}

/// Read Vesdb's NDJSON answer: `event` lines, then `end`; or an `error` line.
fn parse_events(status: u16, text: &str) -> Result<Vec<Written>, AuditError> {
    if status != 200 {
        return Err(AuditError::Refused(format!("HTTP {status}: {}", text.trim())));
    }
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| AuditError::Protocol(format!("{e}: {line}")))?;
        match v["type"].as_str() {
            Some("event") => out.push(written_of(&v["data"])?),
            Some("error") => return Err(AuditError::Refused(v["data"]["description"].as_str().unwrap_or(line).to_string())),
            Some("end") | Some("heartbeat") => {}
            _ => return Err(AuditError::Protocol(format!("unknown line: {line}"))),
        }
    }
    Ok(out)
}

fn written_of(ev: &Value) -> Result<Written, AuditError> {
    let bad = || AuditError::Protocol(format!("not an egm event: {ev}"));
    let id = ev["id"].as_str().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    let ty = ev["type"].as_str().ok_or_else(bad)?;
    Ok(Written {
        id,
        subject: ev["subject"].as_str().ok_or_else(bad)?.to_string(),
        kind: ty.strip_prefix(TYPE_PREFIX).unwrap_or(ty).to_string(),
        actor: ev["data"]["actor"].as_str().unwrap_or_default().to_string(),
        data: ev["data"]["data"].clone(),
        time: ev["time"].as_str().unwrap_or_default().to_string(),
    })
}

impl AuditLog for VesdbLog {
    fn append(&self, key: &str, events: &[AuditEvent]) -> Result<Vec<Written>, AuditError> {
        let body = json!({
            "events": events.iter().map(|e| json!({
                "source": self.source,
                "subject": e.subject(),
                "type": e.event_type(),
                "data": e.payload(),
            })).collect::<Vec<_>>()
        })
        .to_string();
        let (s, t) = self.call("POST", "/api/v2/data/write-events", &body, Some(key))?;
        parse_events(s, &t)
    }

    fn history(&self, project: &str, feature: Option<&str>) -> Result<Vec<Written>, AuditError> {
        let body = json!({"subject": subject(project, feature), "options": {"recursive": feature.is_none()}}).to_string();
        let (s, t) = self.call("POST", "/api/v2/data/read-events", &body, None)?;
        parse_events(s, &t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, feature: &str) -> AuditEvent {
        AuditEvent { kind: kind.into(), project: "jot".into(), feature: feature.into(), actor: "u1".into(), data: json!({"n": 1}) }
    }

    #[test]
    fn subjects_and_types() {
        let e = ev("gate.verdict", "tags");
        assert_eq!(e.subject(), "/egm/jot/tags");
        assert_eq!(e.event_type(), "io.egm.gate.verdict");
        assert_eq!(ev("member.role-changed", "").subject(), "/egm/jot");
    }

    #[test]
    fn memory_log_honours_idempotency_keys() {
        let log = MemoryLog::new();
        let a = log.append(&key("job1", 0), &[ev("feature.created", "tags")]).unwrap();
        let again = log.append(&key("job1", 0), &[ev("feature.created", "tags")]).unwrap();
        assert_eq!(a, again);
        assert_eq!(log.history("jot", None).unwrap().len(), 1, "a retried write records nothing new");
        assert!(matches!(log.append(&key("job1", 0), &[ev("doc.revised", "tags")]), Err(AuditError::Refused(_))));
        log.append(&key("job1", 1), &[ev("member.role-changed", "")]).unwrap();
        assert_eq!(log.history("jot", Some("tags")).unwrap().len(), 1);
        assert_eq!(log.history("jot", None).unwrap().len(), 2);
    }

    #[test]
    fn parses_vesdb_ndjson() {
        let text = concat!(
            r#"{"type":"event","data":{"specversion":"1.0","id":"7","source":"https://x","subject":"/egm/jot/tags","type":"io.egm.gate.verdict","time":"2026-11-15T00:00:00Z","data":{"actor":"worker","data":{"pass":true}}}}"#,
            "\n",
            r#"{"type":"end","data":{}}"#,
            "\n"
        );
        let w = parse_events(200, text).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].id, w[0].kind.as_str(), w[0].actor.as_str()), (7, "gate.verdict", "worker"));
        assert_eq!(w[0].data, json!({"pass": true}));
        let err = r#"{"type":"error","data":{"code":"x","description":"precondition failed","details":{}}}"#;
        assert!(matches!(parse_events(200, err), Err(AuditError::Refused(m)) if m.contains("precondition")));
    }
}
