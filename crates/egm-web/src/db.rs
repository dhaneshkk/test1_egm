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
ALTER TABLE features ADD COLUMN IF NOT EXISTS author text NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN IF NOT EXISTS email text NOT NULL DEFAULT '';
CREATE TABLE IF NOT EXISTS comments (
    id bigint PRIMARY KEY, project text NOT NULL, feature text NOT NULL, target text NOT NULL,
    author text NOT NULL, body text NOT NULL, at text NOT NULL);
CREATE TABLE IF NOT EXISTS labels (
    project text NOT NULL, feature text NOT NULL, round int NOT NULL, alarm int NOT NULL,
    label text NOT NULL, kept boolean NOT NULL, by_user text NOT NULL, event_id bigint NOT NULL,
    at text NOT NULL, PRIMARY KEY (project, feature, round, alarm));
CREATE TABLE IF NOT EXISTS reviews (
    id bigint PRIMARY KEY, project text NOT NULL, feature text NOT NULL, reviewer text NOT NULL,
    verdict text NOT NULL, note text NOT NULL, doc_hash text NOT NULL, at text NOT NULL);
CREATE TABLE IF NOT EXISTS approvals (
    id bigint PRIMARY KEY, project text NOT NULL, feature text NOT NULL, approver text NOT NULL,
    doc_hash text NOT NULL, tier text NOT NULL, at text NOT NULL);
CREATE TABLE IF NOT EXISTS activity (
    user_id text NOT NULL, project text NOT NULL, feature text NOT NULL,
    minutes double precision NOT NULL, last_seen timestamptz NOT NULL,
    PRIMARY KEY (user_id, project, feature));
CREATE TABLE IF NOT EXISTS outbox (
    event_id bigint NOT NULL, to_addr text NOT NULL, subject text NOT NULL, body text NOT NULL,
    sent_at timestamptz, error text, PRIMARY KEY (event_id, to_addr));
CREATE TABLE IF NOT EXISTS jobs (
    id bigint PRIMARY KEY, project text NOT NULL, feature text NOT NULL, command text NOT NULL,
    args jsonb NOT NULL, requester text NOT NULL, state text NOT NULL, status text NOT NULL DEFAULT '',
    detail text NOT NULL DEFAULT '', exit_code int, tokens bigint NOT NULL DEFAULT 0,
    queued_at text NOT NULL, started_at text, finished_at text);
CREATE INDEX IF NOT EXISTS jobs_open ON jobs (state, id);
CREATE TABLE IF NOT EXISTS job_log (
    job_id bigint NOT NULL, seq int NOT NULL, speaker text NOT NULL, text text NOT NULL,
    PRIMARY KEY (job_id, seq));
CREATE TABLE IF NOT EXISTS usage (
    id bigint PRIMARY KEY, project text NOT NULL, feature text NOT NULL, role text NOT NULL,
    model text NOT NULL, tokens bigint NOT NULL, cost double precision, estimated boolean NOT NULL,
    at text NOT NULL);
CREATE TABLE IF NOT EXISTS words (
    key text PRIMARY KEY, project text NOT NULL, event_id bigint NOT NULL, text text NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash bytea PRIMARY KEY, user_id text NOT NULL, expires_at timestamptz NOT NULL);
"#;

/// The projected tables, in the order `rebuild` empties them.
/// Not projected: `sessions`; `activity` (measured minutes, not events); `outbox` (kept across
/// rebuilds so a rebuild never mails anyone twice); `job_log` (a job's terminal output, which
/// may quote code, so it is kept in Postgres only, never in the audit log).
const PROJECTED: &[&str] = &["timeline", "words", "jobs", "usage", "comments", "labels", "reviews", "approvals", "features", "members", "users", "egm_meta"];

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
                "INSERT INTO features (project, slug, doc, gate, phase, doc_hash, created_id, updated_id, updated_at, author)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$7,$8,$9) ON CONFLICT (project, slug) DO NOTHING",
                &[&project, &feature, &doc, &serde_json::to_value(gate)?, &phase_name(&gate), &hash.to_string(), &id, &w.time, &w.actor],
            )
            .await?;
        }
        ("words.recorded", _) => {
            // a redacted event has no data: its words are gone, and stay gone after a rebuild
            if let (Some(k), Some(t)) = (w.data["key"].as_str(), w.data["text"].as_str()) {
                c.execute("INSERT INTO words (key, project, event_id, text) VALUES ($1,$2,$3,$4) ON CONFLICT (key) DO NOTHING", &[&k, &project, &id, &t]).await?;
            }
        }
        ("comment.added", false) => {
            let body = words_of(c, &w.data, "text").await?;
            c.execute(
                "INSERT INTO comments (id, project, feature, target, author, body, at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO NOTHING",
                &[&id, &project, &feature, &w.data["target"].as_str().unwrap_or("doc"), &w.actor, &body, &w.time],
            )
            .await?;
            notify_author(c, id, project, feature, &w.actor, &format!("{} commented on {feature}", who(c, &w.actor).await?)).await?;
        }
        ("alarm.labelled", false) => {
            let round = w.data["round"].as_i64().unwrap_or(0) as i32;
            let alarm = w.data["alarm"].as_i64().unwrap_or(0) as i32;
            c.execute(
                "INSERT INTO labels (project, feature, round, alarm, label, kept, by_user, event_id, at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                 ON CONFLICT (project, feature, round, alarm) DO UPDATE SET label=$5, kept=$6, by_user=$7, event_id=$8, at=$9",
                &[&project, &feature, &round, &alarm, &w.data["label"].as_str().unwrap_or_default(), &w.data["kept"].as_bool().unwrap_or(false), &w.actor, &id, &w.time],
            )
            .await?;
        }
        ("review.submitted", false) => {
            c.execute(
                "INSERT INTO reviews (id, project, feature, reviewer, verdict, note, doc_hash, at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (id) DO NOTHING",
                &[&id, &project, &feature, &w.actor, &w.data["verdict"].as_str().unwrap_or_default(), &words_of(c, &w.data, "note").await?, &w.data["doc_hash"].as_str().unwrap_or_default(), &w.time],
            )
            .await?;
            let what = if w.data["verdict"] == "approve" { "approved" } else { "asked for changes to" };
            notify_author(c, id, project, feature, &w.actor, &format!("{} {what} {feature}", who(c, &w.actor).await?)).await?;
        }
        ("merge.approved", false) => {
            c.execute(
                "INSERT INTO approvals (id, project, feature, approver, doc_hash, tier, at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO NOTHING",
                &[&id, &project, &feature, &w.actor, &w.data["doc_hash"].as_str().unwrap_or_default(), &w.data["tier"].as_str().unwrap_or_default(), &w.time],
            )
            .await?;
            notify_author(c, id, project, feature, &w.actor, &format!("{} approved the merge of {feature}", who(c, &w.actor).await?)).await?;
        }
        ("job.requested", false) => {
            let mut args = w.data["args"].clone();
            if w.data.get("words").is_some() && args.is_object() {
                args["text"] = json!(words_of(c, &w.data, "").await?);
            }
            c.execute(
                "INSERT INTO jobs (id, project, feature, command, args, requester, state, queued_at) VALUES ($1,$2,$3,$4,$5,$6,'queued',$7)
                 ON CONFLICT (id) DO NOTHING",
                &[&id, &project, &feature, &w.data["command"].as_str().unwrap_or_default(), &args, &w.actor, &w.time],
            )
            .await?;
        }
        ("job.started", false) => {
            let job = w.data["job"].as_i64().unwrap_or(-1);
            c.execute("UPDATE jobs SET state='running', started_at=$2 WHERE id=$1 AND state='queued'", &[&job, &w.time]).await?;
        }
        ("job.finished", false) => {
            let job = w.data["job"].as_i64().unwrap_or(-1);
            let code = w.data["exit_code"].as_i64().map(|c| c as i32);
            c.execute(
                "UPDATE jobs SET state='done', status=$2, detail=$3, exit_code=$4, tokens=$5, finished_at=$6 WHERE id=$1",
                &[&job, &w.data["status"].as_str().unwrap_or_default(), &w.data["detail"].as_str().unwrap_or_default(), &code, &w.data["tokens"].as_i64().unwrap_or(0), &w.time],
            )
            .await?;
        }
        ("usage.recorded", false) => {
            c.execute(
                "INSERT INTO usage (id, project, feature, role, model, tokens, cost, estimated, at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (id) DO NOTHING",
                &[&id, &project, &feature, &w.data["role"].as_str().unwrap_or_default(), &w.data["model"].as_str().unwrap_or_default(),
                  &w.data["tokens"].as_i64().unwrap_or(0), &w.data["cost_usd"].as_f64(), &w.data["estimated"].as_bool().unwrap_or(false), &w.time],
            )
            .await?;
        }
        (kind, false) => {
            if kind == "gate.verdict" && w.data["pass"] == true {
                notify_reviewers(c, id, project, feature).await?;
            }
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
                "INSERT INTO users (id, login, name, last_sign_in, email) VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (id) DO UPDATE SET login=$2, name=$3, last_sign_in=$4, email=$5",
                &[&user, &w.data["login"].as_str().unwrap_or_default(), &w.data["name"].as_str().unwrap_or_default(), &w.time, &w.data["email"].as_str().unwrap_or_default()],
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

/// What a person wrote for an action: inline in older events (`field`), or in the
/// `words.recorded` event the action names. Redacted words read as a notice.
pub const REDACTED: &str = "(removed under the retention policy)";

async fn words_of(c: &impl GenericClient, data: &Value, field: &str) -> anyhow::Result<String> {
    if let Some(t) = data.get(field).and_then(|v| v.as_str()) {
        return Ok(t.to_string());
    }
    let Some(k) = data["words"].as_str() else { return Ok(String::new()) };
    let row = c.query_opt("SELECT text FROM words WHERE key=$1", &[&k]).await?;
    Ok(row.map(|r| r.get(0)).unwrap_or_else(|| REDACTED.to_string()))
}

/// A person's login for mail text (their id when they never signed in).
async fn who(c: &impl GenericClient, id: &str) -> anyhow::Result<String> {
    Ok(c.query_opt("SELECT login FROM users WHERE id=$1", &[&id]).await?.map(|r| r.get(0)).unwrap_or_else(|| id.to_string()))
}

/// Mail the feature's author about an action by someone else (when the author has an email).
async fn notify_author(c: &impl GenericClient, id: i64, project: &str, feature: &str, actor: &str, subject: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO outbox (event_id, to_addr, subject, body)
         SELECT $1, u.email, $4, $5 FROM features f JOIN users u ON u.id = f.author
         WHERE f.project=$2 AND f.slug=$3 AND u.email <> '' AND f.author <> $6
         ON CONFLICT (event_id, to_addr) DO NOTHING",
        &[&id, &project, &feature, &subject, &format!("{subject}.\n\n{{url}}/p/{project}/f/{feature}\n"), &actor],
    )
    .await?;
    Ok(())
}

/// A passed evidence gate: mail the project's reviewers and approvers, except the author.
async fn notify_reviewers(c: &impl GenericClient, id: i64, project: &str, feature: &str) -> anyhow::Result<()> {
    let subject = format!("{feature} is ready for review in {project}");
    c.execute(
        "INSERT INTO outbox (event_id, to_addr, subject, body)
         SELECT $1, u.email, $4, $5 FROM members m JOIN users u ON u.id = m.user_id
         WHERE m.project=$2 AND m.roles && ARRAY['reviewer','approver']::text[] AND u.email <> ''
           AND m.user_id <> COALESCE((SELECT author FROM features WHERE project=$2 AND slug=$3), '')
         ON CONFLICT (event_id, to_addr) DO NOTHING",
        &[&id, &project, &feature, &subject, &format!("The evidence gate passed for {feature}.\n\n{{url}}/p/{project}/f/{feature}\n")],
    )
    .await?;
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
pub async fn user_signed_in(c: &impl GenericClient, id: &str, login: &str, name: &str, email: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO users (id, login, name, last_sign_in, email) VALUES ($1,$2,$3,'',$4) ON CONFLICT (id) DO UPDATE SET login=$2, name=$3, email=$4",
        &[&id, &login, &name, &email],
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------- collaboration

/// What the policy needs about a feature: its author, risk tier, and the merge evidence.
#[derive(Debug, Clone, Default)]
pub struct FeatureState {
    pub author: String,
    pub tier: Option<String>,
    pub phase: String,
    pub doc_hash: String,
    pub merge: egm_core::policy::MergeState,
    pub approved: bool,
}

/// A person, not the worker: ids look like `github:123` or `gitlab:5`.
pub fn is_person(id: &str) -> bool {
    id.split_once(':').is_some_and(|(p, n)| (p == "github" || p == "gitlab") && !n.is_empty())
}

pub async fn feature_state(c: &impl GenericClient, project: &str, slug: &str) -> anyhow::Result<Option<FeatureState>> {
    let Some(f) = c.query_opt("SELECT author, phase, doc_hash FROM features WHERE project=$1 AND slug=$2", &[&project, &slug]).await? else {
        return Ok(None);
    };
    let (author, phase, doc_hash): (String, String, String) = (f.get(0), f.get(1), f.get(2));
    let gate = c
        .query_opt("SELECT data FROM timeline WHERE project=$1 AND feature=$2 AND kind='gate.verdict' ORDER BY id DESC LIMIT 1", &[&project, &slug])
        .await?
        .map(|r| r.get::<_, Value>(0));
    let tier = gate.as_ref().and_then(|g| g["tier"].as_str().map(String::from));
    let gate_passed = gate.as_ref().is_some_and(|g| g["pass"] == true && g["doc_hash"].as_str() == Some(doc_hash.as_str()));
    let reviewed = c
        .query_opt("SELECT 1 FROM reviews WHERE project=$1 AND feature=$2 AND verdict='approve' AND doc_hash=$3 AND reviewer <> $4 LIMIT 1", &[&project, &slug, &doc_hash, &author])
        .await?
        .is_some();
    let approved = c.query_opt("SELECT 1 FROM approvals WHERE project=$1 AND feature=$2 AND doc_hash=$3 LIMIT 1", &[&project, &slug, &doc_hash]).await?.is_some();
    let merge = egm_core::policy::MergeState {
        author_known: is_person(&author),
        built: phase == "built" || phase == "hardened",
        gate_passed,
        reviewed_by_other: reviewed,
    };
    Ok(Some(FeatureState { author, tier, phase, doc_hash, merge, approved }))
}

pub async fn comments(c: &impl GenericClient, project: &str, slug: &str) -> anyhow::Result<Vec<(String, String, String, String)>> {
    let rows = c
        .query("SELECT c.target, COALESCE(u.login, c.author), c.body, c.at FROM comments c LEFT JOIN users u ON u.id=c.author WHERE c.project=$1 AND c.feature=$2 ORDER BY c.id", &[&project, &slug])
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect())
}

/// `(round, alarm) -> (label, by)` for a feature.
pub async fn labels(c: &impl GenericClient, project: &str, slug: &str) -> anyhow::Result<std::collections::HashMap<(i32, i32), (String, String)>> {
    let rows = c
        .query("SELECT l.round, l.alarm, l.label, COALESCE(u.login, l.by_user) FROM labels l LEFT JOIN users u ON u.id=l.by_user WHERE l.project=$1 AND l.feature=$2", &[&project, &slug])
        .await?;
    Ok(rows.iter().map(|r| ((r.get(0), r.get(1)), (r.get(2), r.get(3)))).collect())
}

pub async fn reviews(c: &impl GenericClient, project: &str, slug: &str) -> anyhow::Result<Vec<(String, String, String, String, String)>> {
    let rows = c
        .query("SELECT COALESCE(u.login, r.reviewer), r.verdict, r.note, r.doc_hash, r.at FROM reviews r LEFT JOIN users u ON u.id=r.reviewer WHERE r.project=$1 AND r.feature=$2 ORDER BY r.id", &[&project, &slug])
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4))).collect())
}

pub async fn approvals(c: &impl GenericClient, project: &str, slug: &str) -> anyhow::Result<Vec<(String, String, String, String)>> {
    let rows = c
        .query("SELECT COALESCE(u.login, a.approver), a.doc_hash, a.tier, a.at FROM approvals a LEFT JOIN users u ON u.id=a.approver WHERE a.project=$1 AND a.feature=$2 ORDER BY a.id", &[&project, &slug])
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect())
}

/// Goldfish confusion over the latest label of each alarm: (tp, fp, fn, tn). Positive class: the
/// alarm was real (labelled valid); predicted positive: the goldfish kept it. `EgmLabels.lean`.
pub async fn confusion(c: &impl GenericClient, project: &str) -> anyhow::Result<(i64, i64, i64, i64)> {
    let r = c
        .query_one(
            "SELECT count(*) FILTER (WHERE kept AND label='valid'), count(*) FILTER (WHERE kept AND label='noise'),
                    count(*) FILTER (WHERE NOT kept AND label='valid'), count(*) FILTER (WHERE NOT kept AND label='noise')
             FROM labels WHERE project=$1",
            &[&project],
        )
        .await?;
    Ok((r.get(0), r.get(1), r.get(2), r.get(3)))
}

/// Measured minutes: each authenticated view or action of a feature adds the gap since the
/// person's previous one, capped at `GAP_CAP_MINUTES` (as egm-dsh counts human minutes: a longer
/// gap is time away and counts as the cap).
pub async fn touch(c: &impl GenericClient, user: &str, project: &str, slug: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO activity (user_id, project, feature, minutes, last_seen) VALUES ($1,$2,$3,0,now())
         ON CONFLICT (user_id, project, feature) DO UPDATE SET
           minutes = activity.minutes + LEAST(EXTRACT(EPOCH FROM now() - activity.last_seen)::float8 / 60.0, $4),
           last_seen = now()",
        &[&user, &project, &slug, &egm_core::evidence::GAP_CAP_MINUTES],
    )
    .await?;
    Ok(())
}

/// Minutes per feature, summed over people, with how many people.
pub async fn minutes(c: &impl GenericClient, project: &str) -> anyhow::Result<Vec<(String, f64, i64)>> {
    let rows = c.query("SELECT feature, sum(minutes), count(*) FROM activity WHERE project=$1 GROUP BY feature ORDER BY feature", &[&project]).await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
}

/// Features waiting for this person's review across `projects`: built, gate passed for the
/// current doc, not approved yet, not written by them. Highest risk first.
pub async fn inbox(c: &impl GenericClient, projects: &[String], user: &str) -> anyhow::Result<Vec<(String, String, String, String)>> {
    let mut out = vec![];
    for p in projects {
        for f in features(c, p).await? {
            let Some(st) = feature_state(c, p, &f.slug).await? else { continue };
            if st.merge.built && st.merge.gate_passed && !st.approved && st.author != user {
                out.push((p.clone(), f.slug.clone(), st.tier.clone().unwrap_or_else(|| "unknown".into()), f.updated_at.clone()));
            }
        }
    }
    let rank = |t: &str| match t {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    };
    out.sort_by(|a, b| rank(&a.2).cmp(&rank(&b.2)).then(a.3.cmp(&b.3)));
    Ok(out)
}

pub async fn outbox_pending(c: &impl GenericClient) -> anyhow::Result<Vec<(i64, String, String, String)>> {
    let rows = c.query("SELECT event_id, to_addr, subject, body FROM outbox WHERE sent_at IS NULL AND error IS NULL ORDER BY event_id LIMIT 50", &[]).await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect())
}

pub async fn outbox_done(c: &impl GenericClient, event_id: i64, to: &str, error: Option<String>) -> anyhow::Result<()> {
    c.execute("UPDATE outbox SET sent_at = CASE WHEN $3::text IS NULL THEN now() END, error = $3 WHERE event_id=$1 AND to_addr=$2", &[&event_id, &to, &error]).await?;
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

// ------------------------------------------------------------------------------------- jobs

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub project: String,
    pub feature: String,
    pub command: String,
    pub args: Value,
    pub requester: String,
    pub state: String,
    pub status: String,
    pub detail: String,
    pub exit_code: Option<i32>,
    pub tokens: i64,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

const JOB_COLS: &str = "id, project, feature, command, args, requester, state, status, detail, exit_code, tokens, queued_at, started_at, finished_at";

fn job_row(r: &tokio_postgres::Row) -> JobRow {
    JobRow {
        id: r.get(0),
        project: r.get(1),
        feature: r.get(2),
        command: r.get(3),
        args: r.get(4),
        requester: r.get(5),
        state: r.get(6),
        status: r.get(7),
        detail: r.get(8),
        exit_code: r.get(9),
        tokens: r.get(10),
        queued_at: r.get(11),
        started_at: r.get(12),
        finished_at: r.get(13),
    }
}

/// Queued and running jobs of every project, oldest first: what the worker picks from.
pub async fn open_jobs(c: &impl GenericClient) -> anyhow::Result<Vec<JobRow>> {
    let rows = c.query(&format!("SELECT {JOB_COLS} FROM jobs WHERE state IN ('queued','running') ORDER BY id"), &[]).await?;
    Ok(rows.iter().map(job_row).collect())
}

pub async fn job(c: &impl GenericClient, id: i64) -> anyhow::Result<Option<JobRow>> {
    Ok(c.query_opt(&format!("SELECT {JOB_COLS} FROM jobs WHERE id=$1"), &[&id]).await?.as_ref().map(job_row))
}

/// A project's jobs (or one feature's), newest first.
pub async fn jobs(c: &impl GenericClient, project: &str, feature: Option<&str>, limit: i64) -> anyhow::Result<Vec<JobRow>> {
    let rows = match feature {
        Some(f) => c.query(&format!("SELECT {JOB_COLS} FROM jobs WHERE project=$1 AND feature=$2 ORDER BY id DESC LIMIT $3"), &[&project, &f, &limit]).await?,
        None => c.query(&format!("SELECT {JOB_COLS} FROM jobs WHERE project=$1 ORDER BY id DESC LIMIT $2"), &[&project, &limit]).await?,
    };
    Ok(rows.iter().map(job_row).collect())
}

/// Lines of a job's output after `after`.
pub async fn job_log(c: &impl GenericClient, job: i64, after: i32) -> anyhow::Result<Vec<(i32, String, String)>> {
    let rows = c.query("SELECT seq, speaker, text FROM job_log WHERE job_id=$1 AND seq>$2 ORDER BY seq", &[&job, &after]).await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
}

pub async fn job_log_append(c: &impl GenericClient, job: i64, seq: i32, speaker: &str, text: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO job_log (job_id, seq, speaker, text) VALUES ($1,$2,$3,$4) ON CONFLICT (job_id, seq) DO NOTHING",
        &[&job, &seq, &speaker, &text],
    )
    .await?;
    Ok(())
}

/// Tokens a project used in the current calendar month (UTC), from `usage.recorded` events.
pub async fn month_tokens(c: &impl GenericClient, project: &str) -> anyhow::Result<i64> {
    let row = c
        .query_one(
            "SELECT COALESCE(SUM(tokens), 0)::bigint FROM usage WHERE project=$1 AND substr(at, 1, 7) = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM')",
            &[&project],
        )
        .await?;
    Ok(row.get(0))
}

/// One feature's tokens and cost by role and model: (role, model, calls, tokens, cost, any estimated).
pub async fn feature_usage(c: &impl GenericClient, project: &str, feature: &str) -> anyhow::Result<Vec<(String, String, i64, i64, Option<f64>, bool)>> {
    let rows = c
        .query(
            "SELECT role, model, COUNT(*), SUM(tokens)::bigint, SUM(cost), bool_or(estimated) FROM usage WHERE project=$1 AND feature=$2
             GROUP BY role, model ORDER BY role, model",
            &[&project, &feature],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5))).collect())
}

/// Every feature's tokens and cost in a project: (feature, tokens, cost, any estimated).
pub async fn project_usage(c: &impl GenericClient, project: &str) -> anyhow::Result<Vec<(String, i64, Option<f64>, bool)>> {
    let rows = c
        .query(
            "SELECT feature, SUM(tokens)::bigint, SUM(cost), bool_or(estimated) FROM usage WHERE project=$1 GROUP BY feature ORDER BY feature",
            &[&project],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect())
}

/// Defects reported against a merged change: (who, note, when).
pub async fn defects(c: &impl GenericClient, project: &str, feature: &str) -> anyhow::Result<Vec<(String, String, String)>> {
    let rows = c
        .query(
            "SELECT COALESCE(u.login, t.actor), t.data, t.at FROM timeline t LEFT JOIN users u ON u.id = t.actor
             WHERE t.project=$1 AND t.feature=$2 AND t.kind='defect.reported' ORDER BY t.id",
            &[&project, &feature],
        )
        .await?;
    let mut out = vec![];
    for r in rows {
        let data: Value = r.get(1);
        out.push((r.get(0), words_of(c, &data, "note").await?, r.get(2)));
    }
    Ok(out)
}
