//! Postgres holds the views the screens read. Every table but `sessions` is a projection of the
//! Vesdb log and can be dropped and rebuilt from it (`egm-team rebuild`); the log, not the
//! database, is the record an auditor reads.

use crate::SYSTEM;
use egm_audit::{AuditLog, Written};
use egm_core::events::{gate_event_of, DomainEvent, TYPES};
use egm_core::phase::Gate;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::{Client, GenericClient, NoTls};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS egm_meta (key text PRIMARY KEY, value text NOT NULL);
CREATE TABLE IF NOT EXISTS users (
    id text PRIMARY KEY, login text NOT NULL, name text NOT NULL, last_sign_in text NOT NULL);
CREATE TABLE IF NOT EXISTS members (
    project text NOT NULL, user_id text NOT NULL, roles text[] NOT NULL,
    granted_by text NOT NULL, event_id bigint NOT NULL, PRIMARY KEY (project, user_id));
CREATE TABLE IF NOT EXISTS features (
    project text NOT NULL, slug text NOT NULL, doc text NOT NULL, gate jsonb NOT NULL,
    phase text NOT NULL, doc_hash text NOT NULL, created_id bigint NOT NULL,
    updated_id bigint NOT NULL, updated_at text NOT NULL, replay_error text,
    PRIMARY KEY (project, slug));
CREATE TABLE IF NOT EXISTS timeline (
    id bigint PRIMARY KEY, project text NOT NULL, feature text NOT NULL, kind text NOT NULL,
    actor text NOT NULL, data jsonb NOT NULL, at text NOT NULL);
CREATE INDEX IF NOT EXISTS timeline_by_feature ON timeline (project, feature, id);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash bytea PRIMARY KEY, user_id text NOT NULL, expires_at timestamptz NOT NULL);
"#;

/// The projected tables, in the order `rebuild` empties them.
const PROJECTED: &[&str] = &["timeline", "features", "members", "users", "egm_meta"];

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("egm-team: database connection closed: {e}");
        }
    });
    client.batch_execute(SCHEMA).await?;
    Ok(client)
}

/// Split `/egm/<project>[/<feature>]`.
pub fn split_subject(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("/egm/")?;
    match rest.split_once('/') {
        Some((p, f)) if !f.contains('/') => Some((p, f)),
        Some(_) => None,
        None => Some((rest, "")),
    }
}

/// Apply one event to the views. Idempotent: an event already applied changes nothing.
pub async fn apply(c: &impl GenericClient, w: &Written) -> anyhow::Result<()> {
    let Some((project, feature)) = split_subject(&w.subject) else { return Ok(()) };
    let id = w.id as i64;
    let n = c
        .execute(
            "INSERT INTO timeline (id, project, feature, kind, actor, data, at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO NOTHING",
            &[&id, &project, &feature, &w.kind, &w.actor, &w.data, &w.time],
        )
        .await?;
    if n == 0 {
        return Ok(()); // seen before
    }
    match (w.kind.as_str(), feature.is_empty()) {
        ("feature.created", false) => {
            let hash = w.data["doc_hash"].as_str().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            let gate = Gate::new(hash);
            let doc = w.data["doc"].as_str().unwrap_or_default();
            c.execute(
                "INSERT INTO features (project, slug, doc, gate, phase, doc_hash, created_id, updated_id, updated_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$7,$8) ON CONFLICT (project, slug) DO NOTHING",
                &[&project, &feature, &doc, &serde_json::to_value(gate)?, &phase_name(&gate), &hash.to_string(), &id, &w.time],
            )
            .await?;
        }
        (kind, false) => {
            let Some(row) = c.query_opt("SELECT gate FROM features WHERE project=$1 AND slug=$2", &[&project, &feature]).await? else {
                return Ok(());
            };
            let gate: Gate = serde_json::from_value(row.get::<_, Value>(0))?;
            let mut error: Option<String> = None;
            let mut next = gate;
            if let Some(kind) = TYPES.iter().find(|t| **t == kind) {
                let d = DomainEvent { kind, feature: feature.into(), data: w.data.clone() };
                if let Some(ev) = gate_event_of(&d) {
                    match gate.step(ev) {
                        Ok(g) => next = g,
                        Err(r) => error = Some(format!("event {}: the gate refuses this transition ({r})", w.id)),
                    }
                }
            }
            c.execute(
                "UPDATE features SET gate=$3, phase=$4, doc_hash=$5, updated_id=$6, updated_at=$7,
                 replay_error=COALESCE(replay_error, $8) WHERE project=$1 AND slug=$2",
                &[&project, &feature, &serde_json::to_value(next)?, &phase_name(&next), &next.doc_hash.to_string(), &id, &w.time, &error],
            )
            .await?;
        }
        ("member.role-changed", true) => {
            let user = w.data["user"].as_str().unwrap_or_default();
            let roles: Vec<String> = w.data["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(String::from)).collect()).unwrap_or_default();
            if roles.is_empty() {
                c.execute("DELETE FROM members WHERE project=$1 AND user_id=$2", &[&project, &user]).await?;
            } else {
                c.execute(
                    "INSERT INTO members (project, user_id, roles, granted_by, event_id) VALUES ($1,$2,$3,$4,$5)
                     ON CONFLICT (project, user_id) DO UPDATE SET roles=$3, granted_by=$4, event_id=$5",
                    &[&project, &user, &roles, &w.actor, &id],
                )
                .await?;
            }
        }
        ("auth.signed-in", true) if project == SYSTEM => {
            let user = w.data["user"].as_str().unwrap_or_default();
            c.execute(
                "INSERT INTO users (id, login, name, last_sign_in) VALUES ($1,$2,$3,$4)
                 ON CONFLICT (id) DO UPDATE SET login=$2, name=$3, last_sign_in=$4",
                &[&user, &w.data["login"].as_str().unwrap_or_default(), &w.data["name"].as_str().unwrap_or_default(), &w.time],
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

fn phase_name(g: &Gate) -> String {
    serde_json::to_value(g.phase).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

pub async fn last_id(c: &impl GenericClient) -> anyhow::Result<Option<u64>> {
    let row = c.query_opt("SELECT value FROM egm_meta WHERE key='last_event_id'", &[]).await?;
    Ok(row.and_then(|r| r.get::<_, String>(0).parse().ok()))
}

/// Read every event after the last one applied and apply them in one transaction.
/// Returns how many were read.
pub async fn sync(c: &mut Client, log: Arc<dyn AuditLog>) -> anyhow::Result<usize> {
    let after = last_id(&*c).await?;
    let evs = tokio::task::spawn_blocking(move || log.since(after)).await??;
    if evs.is_empty() {
        return Ok(0);
    }
    let tx = c.transaction().await?;
    for w in &evs {
        apply(&tx, w).await?;
    }
    let last = evs.last().map(|w| w.id).unwrap_or_default().to_string();
    tx.execute("INSERT INTO egm_meta (key, value) VALUES ('last_event_id', $1) ON CONFLICT (key) DO UPDATE SET value=$1", &[&last]).await?;
    tx.commit().await?;
    Ok(evs.len())
}

/// Empty every projected table and replay the whole log.
pub async fn rebuild(c: &mut Client, log: Arc<dyn AuditLog>) -> anyhow::Result<usize> {
    for t in PROJECTED {
        c.execute(&format!("DELETE FROM {t}"), &[]).await?;
    }
    sync(c, log).await
}

// ------------------------------------------------------------------------------------ queries

#[derive(Debug, Clone)]
pub struct FeatureRow {
    pub slug: String,
    pub doc: String,
    pub phase: String,
    pub doc_hash: String,
    pub updated_at: String,
    pub replay_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub feature: String,
    pub kind: String,
    pub actor: String,
    pub data: Value,
    pub at: String,
}

pub async fn roles(c: &impl GenericClient, project: &str, user: &str) -> anyhow::Result<Vec<String>> {
    let row = c.query_opt("SELECT roles FROM members WHERE project=$1 AND user_id=$2", &[&project, &user]).await?;
    Ok(row.map(|r| r.get(0)).unwrap_or_default())
}

pub async fn features(c: &impl GenericClient, project: &str) -> anyhow::Result<Vec<FeatureRow>> {
    let rows = c.query("SELECT slug, doc, phase, doc_hash, updated_at, replay_error FROM features WHERE project=$1 ORDER BY updated_id DESC", &[&project]).await?;
    Ok(rows.iter().map(feature_row).collect())
}

pub async fn feature(c: &impl GenericClient, project: &str, slug: &str) -> anyhow::Result<Option<FeatureRow>> {
    let row = c.query_opt("SELECT slug, doc, phase, doc_hash, updated_at, replay_error FROM features WHERE project=$1 AND slug=$2", &[&project, &slug]).await?;
    Ok(row.as_ref().map(feature_row))
}

fn feature_row(r: &tokio_postgres::Row) -> FeatureRow {
    FeatureRow { slug: r.get(0), doc: r.get(1), phase: r.get(2), doc_hash: r.get(3), updated_at: r.get(4), replay_error: r.get(5) }
}

/// A project's events, newest first; one feature's when `feature` is given.
pub async fn timeline(c: &impl GenericClient, project: &str, feature: Option<&str>, limit: i64) -> anyhow::Result<Vec<EventRow>> {
    let rows = match feature {
        Some(f) => c.query("SELECT id, feature, kind, actor, data, at FROM timeline WHERE project=$1 AND feature=$2 ORDER BY id DESC LIMIT $3", &[&project, &f, &limit]).await?,
        None => c.query("SELECT id, feature, kind, actor, data, at FROM timeline WHERE project=$1 ORDER BY id DESC LIMIT $2", &[&project, &limit]).await?,
    };
    Ok(rows.iter().map(|r| EventRow { id: r.get(0), feature: r.get(1), kind: r.get(2), actor: r.get(3), data: r.get(4), at: r.get(5) }).collect())
}

pub async fn members(c: &impl GenericClient, project: &str) -> anyhow::Result<Vec<(String, String, Vec<String>, String)>> {
    let rows = c
        .query(
            "SELECT m.user_id, COALESCE(u.login, m.user_id), m.roles, m.granted_by FROM members m LEFT JOIN users u ON u.id = m.user_id
             WHERE m.project=$1 ORDER BY 2",
            &[&project],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect())
}

/// Counts for the metrics page.
pub async fn counts(c: &impl GenericClient, project: &str) -> anyhow::Result<Value> {
    let phases = c.query("SELECT phase, count(*) FROM features WHERE project=$1 GROUP BY phase ORDER BY phase", &[&project]).await?;
    let kinds = c.query("SELECT kind, count(*) FROM timeline WHERE project=$1 GROUP BY kind ORDER BY kind", &[&project]).await?;
    let gold = c
        .query(
            "SELECT (data->>'ready')::boolean, count(*) FROM timeline WHERE project=$1 AND kind='goldfish.verdict' GROUP BY 1",
            &[&project],
        )
        .await?;
    let gates = c
        .query("SELECT (data->>'pass')::boolean, count(*) FROM timeline WHERE project=$1 AND kind='gate.verdict' GROUP BY 1", &[&project])
        .await?;
    let pairs = |rows: Vec<tokio_postgres::Row>| -> Value {
        Value::Object(rows.iter().map(|r| (r.get::<_, Option<String>>(0).unwrap_or_default(), json!(r.get::<_, i64>(1)))).collect())
    };
    let bools = |rows: Vec<tokio_postgres::Row>| -> Value {
        Value::Object(rows.iter().map(|r| (r.get::<_, Option<bool>>(0).map(|b| b.to_string()).unwrap_or_default(), json!(r.get::<_, i64>(1)))).collect())
    };
    Ok(json!({"phases": pairs(phases), "events": pairs(kinds), "goldfish": bools(gold), "gates": bools(gates)}))
}

pub async fn user_login(c: &impl GenericClient, id: &str) -> anyhow::Result<Option<(String, String)>> {
    let row = c.query_opt("SELECT login, name FROM users WHERE id=$1", &[&id]).await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

/// Write the users row a sign-in's `auth.signed-in` event projects to, at once, so the first
/// page after sign-in shows the name without waiting for the projector. The event is already in
/// Vesdb, so a rebuild writes the same row.
pub async fn user_signed_in(c: &impl GenericClient, id: &str, login: &str, name: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO users (id, login, name, last_sign_in) VALUES ($1,$2,$3,'') ON CONFLICT (id) DO UPDATE SET login=$2, name=$3",
        &[&id, &login, &name],
    )
    .await?;
    Ok(())
}

// ----------------------------------------------------------------------------------- sessions

pub async fn session_create(c: &impl GenericClient, hash: &[u8], user: &str, expires: std::time::SystemTime) -> anyhow::Result<()> {
    c.execute("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1,$2,$3)", &[&hash, &user, &expires]).await?;
    Ok(())
}

pub async fn session_user(c: &impl GenericClient, hash: &[u8]) -> anyhow::Result<Option<String>> {
    let row = c.query_opt("SELECT user_id FROM sessions WHERE token_hash=$1 AND expires_at > now()", &[&hash]).await?;
    Ok(row.map(|r| r.get(0)))
}

pub async fn session_delete(c: &impl GenericClient, hash: &[u8]) -> anyhow::Result<()> {
    c.execute("DELETE FROM sessions WHERE token_hash=$1", &[&hash]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects() {
        assert_eq!(split_subject("/egm/jot/gap-ids"), Some(("jot", "gap-ids")));
        assert_eq!(split_subject("/egm/jot"), Some(("jot", "")));
        assert_eq!(split_subject("/egm/jot/a/b"), None);
        assert_eq!(split_subject("/other/jot"), None);
    }
}
