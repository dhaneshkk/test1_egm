//! Confidentiality (SOC 2 C1): keep data only as long as the project's `retention_days`.
//!
//! Two kinds of data, two mechanisms:
//! * Postgres-only data (job output, sent mail, expired sessions) is deleted by [`purge`], which
//!   the worker runs every hour. It is not part of the record, so deleting it loses nothing an
//!   auditor relies on.
//! * People's words in the audit log (comments, review notes, ideas and answers to the elephant,
//!   defect notes) live in their own `words.recorded` events, and names and emails in
//!   `auth.signed-in` events. `egm-team redact` erases those older than the retention period from
//!   Vesdb, using a token with the `redact` scope that no routine process holds. Vesdb keeps every
//!   proof valid and records the redaction itself; the actions (who commented, reviewed or ran
//!   what, and when) stay in the record.

use egm_audit::Written;
use std::collections::HashMap;
use tokio_postgres::GenericClient;

/// Delete Postgres-only data older than `days` for `project`; returns rows deleted.
pub async fn purge(c: &impl GenericClient, project: &str, days: u32) -> anyhow::Result<u64> {
    let days = days as i32;
    let cutoff = "to_char((now() - make_interval(days => $2)) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS')";
    let mut n = c
        .execute(
            &format!("DELETE FROM job_log WHERE job_id IN (SELECT id FROM jobs WHERE project=$1 AND state='done' AND finished_at < {cutoff})"),
            &[&project, &days],
        )
        .await?;
    n += c
        .execute(
            "DELETE FROM outbox o WHERE o.sent_at < now() - make_interval(days => $2)
             AND EXISTS (SELECT 1 FROM timeline t WHERE t.id = o.event_id AND t.project = $1)",
            &[&project, &days],
        )
        .await?;
    n += c.execute("DELETE FROM sessions WHERE expires_at < now()", &[]).await?;
    Ok(n)
}

/// The events `egm-team redact` erases: words older than `cutoff` (an RFC 3339 prefix such as
/// `2026-08-01`) in the project's events, and sign-ins older than it except each person's latest
/// (which the app needs for their name and mail address). Already redacted events are skipped.
pub fn to_redact(project_events: &[Written], sign_ins: &[Written], cutoff: &str) -> Vec<u64> {
    let old = |w: &Written| !w.time.is_empty() && w.time.as_str() < cutoff && !w.data.is_null();
    let mut ids: Vec<u64> = project_events.iter().filter(|w| w.kind == "words.recorded" && old(w)).map(|w| w.id).collect();
    let mut latest: HashMap<&str, u64> = HashMap::new();
    for w in sign_ins.iter().filter(|w| w.kind == "auth.signed-in") {
        if let Some(u) = w.data["user"].as_str() {
            let e = latest.entry(u).or_insert(w.id);
            *e = (*e).max(w.id);
        }
    }
    ids.extend(sign_ins.iter().filter(|w| w.kind == "auth.signed-in" && old(w) && w.data["user"].as_str().and_then(|u| latest.get(u)) != Some(&w.id)).map(|w| w.id));
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn w(id: u64, kind: &str, time: &str, data: serde_json::Value) -> Written {
        Written { id, subject: "/egm/jot/f".into(), kind: kind.into(), actor: "github:1".into(), data, time: time.into() }
    }

    #[test]
    fn only_old_words_and_superseded_sign_ins() {
        let evs = [
            w(1, "words.recorded", "2026-01-05T00:00:00Z", json!({"text": "old"})),
            w(2, "comment.added", "2026-01-05T00:00:00Z", json!({"words": "k"})),
            w(3, "words.recorded", "2026-06-05T00:00:00Z", json!({"text": "new"})),
            w(4, "words.recorded", "2026-01-06T00:00:00Z", serde_json::Value::Null),
        ];
        let signs = [
            w(10, "auth.signed-in", "2026-01-01T00:00:00Z", json!({"user": "github:1"})),
            w(11, "auth.signed-in", "2026-01-02T00:00:00Z", json!({"user": "github:1"})),
            w(12, "auth.signed-in", "2026-01-03T00:00:00Z", json!({"user": "github:2"})),
        ];
        assert_eq!(to_redact(&evs, &signs, "2026-03-01"), [1, 10], "the action, newer words, redacted words and latest sign-ins stay");
    }
}
