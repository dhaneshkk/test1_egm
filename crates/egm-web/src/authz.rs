//! The one policy check every page and route calls before it reads or does anything.
//!
//! Roles come from the members view (itself rebuilt from `member.role-changed` events in
//! Vesdb); configured admins hold the admin role in every project. The decision is
//! [`egm_core::policy::allowed`], proved in `proofs/EgmPolicy.lean`. A refusal is written to the
//! audit log as `policy.refused` before the 403 goes out.
//!
//! `tests/policy_lint.rs` reads every handler in `src/` and fails the build if one does not
//! start with a call into this module.

use crate::{db, Ah, App, SYSTEM};
use egm_audit::{AuditEvent, AuditLog};
use egm_core::evidence::Tier;
use egm_core::policy::{allowed, Action, Role};
use serde_json::json;
use std::sync::Arc;
use topcoat::context::{context, Cx};
use topcoat::router::error::{forbidden, not_found, see_other};
use topcoat::session;

#[derive(Debug, Clone)]
pub struct Viewer {
    pub id: String,
    pub login: String,
    pub name: String,
    pub admin: bool,
}

pub fn app(cx: &Cx) -> &App {
    context(cx)
}

/// Who is signed in, if anyone.
pub async fn viewer(cx: &Cx) -> topcoat::Result<Option<Viewer>> {
    let Some(hash) = session::token_hash(cx).await? else { return Ok(None) };
    let a = app(cx);
    let Some(id) = db::session_user(&*a.db, &hash[..]).await.ah()? else { return Ok(None) };
    let (login, name) = db::user_login(&*a.db, &id).await.ah()?.unwrap_or_else(|| (id.clone(), id.clone()));
    let admin = a.cfg.admins.contains(&id);
    Ok(Some(Viewer { id, login, name, admin }))
}

/// Signed in, or off to the sign-in page.
pub async fn signed_in(cx: &Cx) -> topcoat::Result<Viewer> {
    match viewer(cx).await? {
        Some(v) => Ok(v),
        None => Err(see_other("/sign-in").into()),
    }
}

/// The viewer's roles in `project` (admins also hold `admin`).
pub async fn roles_of(cx: &Cx, v: &Viewer, project: &str) -> topcoat::Result<Vec<Role>> {
    let names = db::roles(&*app(cx).db, project, &v.id).await.ah()?;
    let mut roles: Vec<Role> = names.iter().filter_map(|n| serde_json::from_value(json!(n)).ok()).collect();
    if v.admin && !roles.contains(&Role::Admin) {
        roles.push(Role::Admin);
    }
    Ok(roles)
}

/// The check: signed in, the project exists, and the viewer's roles allow `action` there.
pub async fn require(cx: &Cx, project: &str, action: Action) -> topcoat::Result<Viewer> {
    let v = signed_in(cx).await?;
    if app(cx).cfg.project(project).is_none() {
        return Err(not_found().into());
    }
    let roles = roles_of(cx, &v, project).await?;
    if let Err(denial) = allowed(&roles, false, Tier::Low, action) {
        refuse(app(cx).log.clone(), project, &v.id, action, &denial.to_string()).await;
        return Err(forbidden().into());
    }
    Ok(v)
}

/// The check for an action on one feature: the role rules with the feature's author and risk
/// tier (from the audit log). Returns the viewer and the feature's state.
pub async fn require_feature(cx: &Cx, project: &str, slug: &str, action: Action) -> topcoat::Result<(Viewer, db::FeatureState)> {
    let v = signed_in(cx).await?;
    if app(cx).cfg.project(project).is_none() {
        return Err(not_found().into());
    }
    let st = db::feature_state(&*app(cx).db, project, slug).await.ah()?.ok_or_else(not_found)?;
    let roles = roles_of(cx, &v, project).await?;
    let author = st.author == v.id;
    if let Err(denial) = allowed(&roles, author, tier_of(&st), action) {
        refuse(app(cx).log.clone(), project, &v.id, action, &denial.to_string()).await;
        return Err(forbidden().into());
    }
    Ok((v, st))
}

/// The feature's risk tier; unknown counts as high, so an unassessed change needs an approver.
pub fn tier_of(st: &db::FeatureState) -> Tier {
    match st.tier.as_deref() {
        Some("low") => Tier::Low,
        Some("medium") => Tier::Medium,
        _ => Tier::High,
    }
}

/// Write one app event to the audit log and apply it to the views at once. Nothing happens
/// unless the log accepted it: no record, no action.
pub async fn record_now(cx: &Cx, ev: AuditEvent) -> topcoat::Result<()> {
    let a = app(cx);
    let log = a.log.clone();
    let key = format!("web/{}", crate::oauth::random_hex(16).ah()?);
    let written = tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
    for w in &written {
        db::apply(&*a.db, w).await.ah()?;
    }
    a.refresh.notify_one();
    Ok(())
}

/// The projects the viewer may read.
pub async fn readable_projects(cx: &Cx, v: &Viewer) -> topcoat::Result<Vec<String>> {
    let mut out = vec![];
    for p in &app(cx).cfg.projects {
        if allowed(&roles_of(cx, v, &p.name).await?, false, Tier::Low, Action::Read).is_ok() {
            out.push(p.name.clone());
        }
    }
    Ok(out)
}

/// Record a refusal; a log outage must not turn a 403 into a 500, so failures are reported only.
pub async fn refuse(log: Arc<dyn AuditLog>, project: &str, user: &str, action: Action, why: &str) {
    let ev = AuditEvent {
        kind: "policy.refused".into(),
        project: if project.is_empty() { SYSTEM.into() } else { project.into() },
        feature: String::new(),
        actor: user.into(),
        data: json!({"action": action, "reason": why}),
    };
    record(log, ev).await;
}

/// Write one app event to the audit log under a fresh idempotency key.
pub async fn record(log: Arc<dyn AuditLog>, ev: AuditEvent) {
    let key = format!("web/{}", crate::oauth::random_hex(16).unwrap_or_default());
    match tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("egm-team: could not record an audit event: {e}"),
        Err(e) => eprintln!("egm-team: could not record an audit event: {e}"),
    }
}
