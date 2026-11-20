//! The SOC 2 controls egm-team supports, and the evidence pack an auditor asks for
//! (`egm-team evidence --from --to --out`). Scope: Security (the common criteria), Availability
//! and Confidentiality. `docs/soc2/CONTROLS.md` describes the same list for people; a test keeps
//! the two in step.
//!
//! Every control here is one egm-team can show evidence for from its own record. Controls that
//! are the organisation's alone (board oversight, HR screening, vendor contracts, incident
//! response drills...) are listed in `CONTROLS.md` as the organisation's, with no app evidence.

use crate::config::Config;
use egm_audit::AuditLog;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub struct Control {
    pub id: &'static str,
    pub category: &'static str,
    pub title: &'static str,
    /// How egm-team meets it.
    pub mechanism: &'static str,
    /// The key of this control's section in the evidence pack.
    pub evidence: &'static str,
}

pub const CONTROLS: &[Control] = &[
    Control { id: "CC6.1", category: "Security", title: "Logical access: identity and authentication",
        mechanism: "GitHub or GitLab OAuth sign-in (GitLab with PKCE); hashed sessions; every sign-in recorded before the session starts", evidence: "access" },
    Control { id: "CC6.2", category: "Security", title: "Access granted and removed through an authorised process",
        mechanism: "roles change only through `egm-team grant`, each change a `member.role-changed` event naming who granted it", evidence: "access" },
    Control { id: "CC6.3", category: "Security", title: "Least privilege and separation of duties",
        mechanism: "a Lean-proven role-by-action policy on every page and route; an author never reviews or approves their own feature; high risk needs the approver", evidence: "separation_of_duties" },
    Control { id: "CC5.1", category: "Security", title: "Control activities that mitigate risk (separation of duties)",
        mechanism: "the merge-approval commit status, required on the code host, turns green only after an approval by someone other than the author (EgmHost)", evidence: "separation_of_duties" },
    Control { id: "CC6.6", category: "Security", title: "Protection at the system boundary",
        mechanism: "TLS at the reverse proxy; Postgres and Vesdb on a private network; Topcoat's origin policy refuses cross-site posts; strict security headers", evidence: "boundary" },
    Control { id: "CC6.7", category: "Security", title: "Data protected in transmission",
        mechanism: "HTTPS to the app (HSTS), HTTPS to Vesdb through its TLS proxy with a pinned CA, TLS to SMTP and the code hosts", evidence: "boundary" },
    Control { id: "CC6.8", category: "Security", title: "Unauthorised or malicious software prevented",
        mechanism: "model-written code and checks run under egm-dsh's sandbox; jobs get an empty environment plus an allowlist; dependency audit in CI", evidence: "software" },
    Control { id: "CC7.2", category: "Security", title: "Security events monitored",
        mechanism: "every refusal by the policy or the gate, failed sign-in and bad webhook signature is an event in Vesdb", evidence: "monitoring" },
    Control { id: "CC7.3", category: "Security", title: "Security events evaluated",
        mechanism: "the pack lists refusals and failed sign-ins per user and action for the period, for review", evidence: "monitoring" },
    Control { id: "CC8.1", category: "Security", title: "Changes authorised, tested and approved before release",
        mechanism: "the Lean-proven evidence gate, frozen tests and seeded bugs; both commit statuses required; every merge recorded from the code host's webhook", evidence: "change_management" },
    Control { id: "CC4.1", category: "Security", title: "Monitoring of controls; logs that cannot be changed quietly",
        mechanism: "Vesdb's Merkle log, signed checkpoints and offline bundles; the pack carries a bundle of the period's change-management events", evidence: "log_integrity" },
    Control { id: "CC9.2", category: "Security", title: "Vendors and business partners",
        mechanism: "the model provider and models used, with token counts, and the code hosts, from the record", evidence: "vendors" },
    Control { id: "A1.1", category: "Availability", title: "Capacity and health monitored",
        mechanism: "`/health/ready` checks Postgres and Vesdb for the load balancer; job queue and token use per project", evidence: "availability" },
    Control { id: "A1.2", category: "Availability", title: "Recovery infrastructure and backups",
        mechanism: "Vesdb is the record (its own HA and backups); every Postgres view is rebuilt from it; job output is the only data Postgres alone holds", evidence: "availability" },
    Control { id: "A1.3", category: "Availability", title: "Recovery tested",
        mechanism: "`egm-team rebuild` is the restore drill; each run is recorded as `ops.restore-tested` with the events replayed and the time taken", evidence: "availability" },
    Control { id: "C1.1", category: "Confidentiality", title: "Confidential information identified and kept as long as needed",
        mechanism: "the log holds hashes and verdicts, not code; people's words in their own events; a retention period per project", evidence: "confidentiality" },
    Control { id: "C1.2", category: "Confidentiality", title: "Confidential information disposed of",
        mechanism: "the worker purges Postgres-only data hourly; `egm-team redact` erases old words and sign-in details from Vesdb (recorded as `ops.redacted`)", evidence: "confidentiality" },
];

async fn events(c: &impl GenericClient, kinds: &[&str], from: &str, to: &str) -> anyhow::Result<Vec<(i64, String, String, String, String, Value, String)>> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    let rows = c
        .query(
            "SELECT id, project, feature, kind, actor, data, at FROM timeline WHERE kind = ANY($1) AND at >= $2 AND at < $3 ORDER BY id",
            &[&kinds, &from, &to],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5), r.get(6))).collect())
}

/// Every merged change in the period, with whether its approval and gate held at merge time.
async fn changes(c: &impl GenericClient, from: &str, to: &str) -> anyhow::Result<Vec<Value>> {
    let merges = events(c, &["merge.completed"], from, to).await?;
    let mut out = vec![];
    for (id, project, feature, _, actor, data, at) in merges {
        let f = crate::host::facts(c, &project, &feature).await?;
        // the facts as they stood just before the merge event
        let before = |x: i64| x < id;
        let at_merge = crate::host::Facts {
            last_build: f.last_build.filter(|b| before(*b)),
            last_gate: f.last_gate.clone().filter(|g| before(g.0)),
            approvals: f.approvals.iter().filter(|a| before(a.0)).cloned().collect(),
            ..f.clone()
        };
        let (gate, _) = crate::host::gate_status(&at_merge);
        let (merge, why) = crate::host::merge_status(&at_merge);
        out.push(json!({
            "project": project, "feature": feature, "merge_event": id, "merged_at": at, "merged_by": actor,
            "merge_sha": data["merge_sha"], "author": f.author,
            "gate_passed_at_merge": gate == crate::host::State::Success,
            "approved_by_other_at_merge": merge == crate::host::State::Success,
            "approval": why,
            "exception": merge != crate::host::State::Success,
        }));
    }
    Ok(out)
}

/// Build the pack for `[from, to)` (RFC 3339 dates such as `2026-11-01`).
pub async fn pack(cfg: &Config, c: &impl GenericClient, log: Arc<dyn AuditLog>, from: &str, to: &str) -> anyhow::Result<(Value, Option<Value>)> {
    let mut members = BTreeMap::new();
    for p in &cfg.projects {
        members.insert(p.name.clone(), json!(crate::db::members(c, &p.name).await?.into_iter()
            .map(|(id, login, roles, by)| json!({"user": id, "login": login, "roles": roles, "granted_by": by})).collect::<Vec<_>>()));
    }
    let role_changes = events(c, &["member.role-changed"], from, to).await?;
    let sign_ins = events(c, &["auth.signed-in"], from, to).await?;
    let failed = events(c, &["auth.sign-in-failed"], from, to).await?;
    let refused = events(c, &["policy.refused"], from, to).await?;
    let mut refused_by: BTreeMap<String, i64> = BTreeMap::new();
    for r in &refused {
        *refused_by.entry(format!("{} · {} · {}", r.1, r.4, r.5["action"].as_str().unwrap_or("?"))).or_default() += 1;
    }
    let self_refusals = refused.iter().filter(|r| ["submit-review", "approve-merge"].contains(&r.5["action"].as_str().unwrap_or(""))).count();
    let changes = changes(c, from, to).await?;
    let exceptions: Vec<&Value> = changes.iter().filter(|c| c["exception"] == true).collect();
    let statuses = events(c, &["host.status-set"], from, to).await?;
    let usage = c.query(
        "SELECT model, COUNT(*), SUM(tokens)::bigint, bool_or(estimated) FROM usage WHERE at >= $1 AND at < $2 GROUP BY model ORDER BY model", &[&from, &to]).await?;
    let restores = events(c, &["ops.restore-tested"], from, to).await?;
    let redactions = events(c, &["ops.redacted"], from, to).await?;
    let exports = events(c, &["export.created"], from, to).await?;
    let total: i64 = c.query_one("SELECT COUNT(*) FROM timeline WHERE at >= $1 AND at < $2", &[&from, &to]).await?.get(0);
    let https = cfg.public_url.starts_with("https://");
    let vesdb_tls = cfg.vesdb_url.split(',').all(|u| u.trim().starts_with("https://"));

    // the period's change-management events, as an offline-verifiable bundle
    let ids: BTreeSet<u64> = c.query(
        "SELECT t.id FROM timeline t WHERE t.at >= $1 AND t.at < $2 AND t.kind IN
         ('merge.completed','merge.approved','review.submitted','gate.verdict','build.completed','host.status-set','policy.refused','member.role-changed')",
        &[&from, &to]).await?.iter().map(|r| r.get::<_, i64>(0) as u64).collect();
    let ids: Vec<u64> = ids.into_iter().collect();
    let bundle = if ids.is_empty() {
        None
    } else {
        let log2 = log.clone();
        let ids2 = ids.clone();
        Some(tokio::task::spawn_blocking(move || log2.bundle(&ids2)).await??)
    };

    let providers: Vec<&str> = [cfg.github.as_ref().map(|_| "github"), cfg.gitlab.as_ref().map(|_| "gitlab")].into_iter().flatten().collect();
    let sections = json!({
        "access": {
            "members_now": members, "admins": cfg.admins,
            "role_changes": role_changes.iter().map(|e| json!({"event": e.0, "project": e.1, "by": e.4, "user": e.5["user"], "roles": e.5["roles"], "at": e.6})).collect::<Vec<_>>(),
            "sign_ins": sign_ins.len(), "failed_sign_ins": failed.len(),
        },
        "separation_of_duties": {
            "merged_changes": changes.len(), "exceptions": exceptions,
            "self_review_or_approval_refused": self_refusals,
            "rule": "egm/merge-approval is green only with an approval after the passing gate, for the current doc, by someone other than the author (proofs/EgmHost.lean)",
        },
        "boundary": {
            "app_https": https, "hsts": https, "vesdb_https": vesdb_tls, "vesdb_ca_pinned": cfg.vesdb_ca.is_some(),
            "content_security_policy": crate::security::CSP, "cross_site_posts": "refused by Topcoat's origin policy",
            "smtp_tls": cfg.mail.as_ref().map(|m| m.tls.clone()),
        },
        "software": {
            "job_environment": "cleared, plus PATH, HOME, LANG, LC_ALL, TMPDIR, USER and pass_env",
            "pass_env": cfg.worker.as_ref().map(|w| w.pass_env.clone()).unwrap_or_default(),
            "dependency_audit": "cargo deny check advisories, in CI on every push",
        },
        "monitoring": {
            "refusals": refused.len(), "refusals_by_project_user_action": refused_by, "failed_sign_ins": failed.len(),
            "webhook_signature_failures": refused.iter().filter(|r| r.4.ends_with("-webhook")).count(),
        },
        "change_management": {
            "merged_changes": changes, "statuses_set_on_code_host": statuses.len(),
        },
        "log_integrity": {
            "events_in_period": total, "bundle_events": ids.len(), "bundle_file": if bundle.is_some() { Some("bundle.json") } else { None },
            "verify": "vesdb-verify bundle bundle.json --log-key <Vesdb's public key>",
            "exports_in_period": exports.len(),
        },
        "vendors": {
            "models": usage.iter().map(|r| json!({"model": r.get::<_, String>(0), "calls": r.get::<_, i64>(1), "tokens": r.get::<_, i64>(2), "estimated": r.get::<_, bool>(3)})).collect::<Vec<_>>(),
            "code_hosts": cfg.projects.iter().filter_map(|p| p.repo.as_ref().map(|r| json!({"project": p.name, "host": p.host, "repo": r}))).collect::<Vec<_>>(),
            "sign_in_providers": providers,
        },
        "availability": {
            "readiness_endpoint": "/health/ready (Postgres and Vesdb)",
            "restore_drills": restores.iter().map(|e| json!({"at": e.6, "by": e.4, "events": e.5["events"], "seconds": e.5["seconds"]})).collect::<Vec<_>>(),
            "vesdb_urls": cfg.vesdb_url.split(',').count(),
        },
        "confidentiality": {
            "retention_days": cfg.projects.iter().map(|p| (p.name.clone(), json!(p.retention_days))).collect::<BTreeMap<_, _>>(),
            "redactions": redactions.iter().map(|e| json!({"at": e.6, "by": e.4, "project": e.1, "events": e.5["events"], "before": e.5["before"]})).collect::<Vec<_>>(),
            "log_payloads": "hashes, counts and verdicts; people's words only in words.recorded events",
        },
    });
    let controls: Vec<Value> = CONTROLS.iter().map(|k| json!({
        "id": k.id, "category": k.category, "title": k.title, "mechanism": k.mechanism, "evidence": k.evidence,
    })).collect();
    Ok((json!({"period": {"from": from, "to": to}, "generated_by": "egm-team evidence", "controls": controls, "evidence": sections}), bundle))
}

/// The pack as Markdown, for people.
pub fn markdown(pack: &Value) -> String {
    let mut s = format!("# SOC 2 evidence pack\n\nPeriod: {} to {} (end exclusive). Generated by `egm-team evidence` from the audit log.\n\n",
                        pack["period"]["from"].as_str().unwrap_or(""), pack["period"]["to"].as_str().unwrap_or(""));
    s += "| Control | Category | How egm-team meets it | Evidence section |\n|---|---|---|---|\n";
    for c in pack["controls"].as_array().into_iter().flatten() {
        s += &format!("| {} {} | {} | {} | `{}` |\n", c["id"].as_str().unwrap_or(""), c["title"].as_str().unwrap_or(""), c["category"].as_str().unwrap_or(""),
                      c["mechanism"].as_str().unwrap_or(""), c["evidence"].as_str().unwrap_or(""));
    }
    let ex = &pack["evidence"]["separation_of_duties"]["exceptions"];
    s += &format!("\n## Separation of duties\n\n{} merged change(s); {} exception(s) (merged without a valid approval by someone other than the author).\n",
                  pack["evidence"]["separation_of_duties"]["merged_changes"], ex.as_array().map(|a| a.len()).unwrap_or(0));
    s += &format!("\n## Monitoring\n\n{} refusal(s) and {} failed sign-in(s) in the period.\n", pack["evidence"]["monitoring"]["refusals"], pack["evidence"]["monitoring"]["failed_sign_ins"]);
    s += "\n## Every section\n\n```json\n";
    s += &serde_json::to_string_pretty(&pack["evidence"]).unwrap_or_default();
    s += "\n```\n";
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_controls_document_lists_every_control() {
        let doc = include_str!("../../../docs/soc2/CONTROLS.md");
        for c in CONTROLS {
            assert!(doc.contains(&format!("| {} ", c.id)), "docs/soc2/CONTROLS.md does not list {}", c.id);
        }
        for cat in ["Security", "Availability", "Confidentiality"] {
            assert!(CONTROLS.iter().any(|c| c.category == cat), "no {cat} control");
        }
    }
}
