//! GitHub and GitLab (phase 5). For a project with a `repo`, egm-team
//!
//! * pushes each feature's `egm-dsh/<feature>` branch from the checkout (egm-dsh commits accepted
//!   builds there) and opens a pull request (GitHub) or merge request (GitLab) for it, whose
//!   description carries the evidence and a link to the audit bundle;
//! * sets two commit statuses on the branch head: `egm/evidence-gate` and `egm/merge-approval`.
//!   Made required in branch protection, they let the code host enforce the gate and separation
//!   of duties at merge time. egm-team never merges;
//! * records merges and reverts from signed webhooks (`merge.completed`, `change.reverted`).
//!
//! Which state each status has is [`statuses`], mirrored by `proofs/EgmHost.lean`. Every status
//! egm-team sets and every request it opens is recorded in Vesdb (`host.status-set`,
//! `host.pr-opened`), so the record shows what the code host was told and when.

use crate::authz::{self, app};
use crate::config::{Config, ProjectConfig};
use crate::{db, Ah};
use egm_audit::{AuditEvent, AuditLog};
use egm_core::policy::Action;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_postgres::GenericClient;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::{bad_request, not_found, see_other, SeeOther};
use topcoat::router::request::headers;
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{path_param, route, Body, StatusCode};
use topcoat::view::{component, view, View};
use topcoat::Result;

path_param!(project);
path_param!(feature);

pub const GATE_CONTEXT: &str = "egm/evidence-gate";
pub const MERGE_CONTEXT: &str = "egm/merge-approval";

// --------------------------------------------------------------------------- the status rule

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Success,
    Failure,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Success => "success",
            State::Failure => "failure",
        }
    }
}

/// What the statuses are computed from, all read from the audit log (event ids order them).
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// The latest `build.completed`.
    pub last_build: Option<i64>,
    /// The latest `gate.verdict`: (id, passed, the doc hash it judged).
    pub last_gate: Option<(i64, bool, String)>,
    /// The feature's current doc hash.
    pub doc_hash: String,
    pub author: String,
    /// `merge.approved` events: (id, approver, doc hash).
    pub approvals: Vec<(i64, String, String)>,
}

/// `gateStatus` in Lean: the gate's verdict counts only for the latest build and the current doc.
pub fn gate_status(f: &Facts) -> (State, String) {
    let Some(build) = f.last_build else { return (State::Pending, "not built yet".into()) };
    match &f.last_gate {
        Some((g, pass, doc)) if *g > build && *doc == f.doc_hash => {
            if *pass {
                (State::Success, "the evidence gate passed for this build and doc".into())
            } else {
                (State::Failure, "the evidence gate failed; see the evidence in egm-team".into())
            }
        }
        Some((g, _, _)) if *g > build => (State::Pending, "the design doc changed since the gate ran".into()),
        _ => (State::Pending, "the evidence gate has not run on this build".into()),
    }
}

/// `mergeStatus` in Lean: success only after the gate succeeded, with an approval recorded after
/// that gate verdict, for the current doc, by someone other than the author.
pub fn merge_status(f: &Facts) -> (State, String) {
    if gate_status(f).0 != State::Success {
        return (State::Pending, "waits for the evidence gate".into());
    }
    let gate = f.last_gate.as_ref().map(|g| g.0).unwrap_or(i64::MAX);
    match f.approvals.iter().find(|(id, by, doc)| *id > gate && *doc == f.doc_hash && *by != f.author && !f.author.is_empty()) {
        Some((_, by, _)) => (State::Success, format!("approved in egm-team by {by}")),
        None => (State::Pending, "waits for approval in egm-team by someone other than the author".into()),
    }
}

pub fn statuses(f: &Facts) -> [(&'static str, State, String); 2] {
    let (gs, gd) = gate_status(f);
    let (ms, md) = merge_status(f);
    [(GATE_CONTEXT, gs, gd), (MERGE_CONTEXT, ms, md)]
}

/// The facts for one feature, from the projected audit log.
pub async fn facts(c: &impl GenericClient, project: &str, feature: &str) -> anyhow::Result<Facts> {
    let st = db::feature_state(c, project, feature).await?;
    let mut f = Facts { doc_hash: st.as_ref().map(|s| s.doc_hash.clone()).unwrap_or_default(), author: st.map(|s| s.author).unwrap_or_default(), ..Facts::default() };
    let rows = c
        .query(
            "SELECT id, kind, actor, data FROM timeline WHERE project=$1 AND feature=$2 AND kind IN ('build.completed','gate.verdict','merge.approved') ORDER BY id",
            &[&project, &feature],
        )
        .await?;
    for r in rows {
        let (id, kind, actor, data): (i64, String, String, Value) = (r.get(0), r.get(1), r.get(2), r.get(3));
        match kind.as_str() {
            "build.completed" => f.last_build = Some(id),
            "gate.verdict" => f.last_gate = Some((id, data["pass"] == true, data["doc_hash"].as_str().unwrap_or_default().to_string())),
            _ => f.approvals.push((id, actor, data["doc_hash"].as_str().unwrap_or_default().to_string())),
        }
    }
    Ok(f)
}

// ------------------------------------------------------------------------------- the client

/// One code host's REST API for one repository.
pub struct Host {
    pub github: bool,
    pub api: String,
    pub repo: String,
    token: String,
    agent: ureq::Agent,
}

impl Host {
    pub fn for_project(cfg: &Config, p: &ProjectConfig) -> Option<Host> {
        let repo = p.repo.clone()?;
        let github = p.host == "github";
        let api = if github {
            cfg.github.as_ref().and_then(|g| g.api_url.clone()).unwrap_or_else(|| "https://api.github.com".into())
        } else {
            format!("{}/api/v4", cfg.gitlab.as_ref().and_then(|g| g.base_url.clone()).unwrap_or_else(|| "https://gitlab.com".into()).trim_end_matches('/'))
        };
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
        Some(Host { github, api: api.trim_end_matches('/').to_string(), repo, token: p.host_token.clone(), agent })
    }

    fn project_path(&self) -> String {
        if self.github { format!("/repos/{}", self.repo) } else { format!("/projects/{}", self.repo.replace('/', "%2F")) }
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let mut b = ureq::http::Request::builder().method(method).uri(format!("{}{}{path}", self.api, self.project_path())).header("User-Agent", "egm-team");
        b = if self.github {
            b.header("Authorization", format!("Bearer {}", self.token)).header("Accept", "application/vnd.github+json")
        } else {
            b.header("PRIVATE-TOKEN", &self.token)
        };
        let req = match body {
            Some(v) => b.header("Content-Type", "application/json").body(v.to_string())?,
            None => b.body(String::new())?,
        };
        let mut resp = self.agent.run(req)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        anyhow::ensure!((200..300).contains(&status), "{method} {path}: HTTP {status}: {}", text.chars().take(300).collect::<String>());
        Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text)? })
    }

    /// The open request for `branch`: (number, URL).
    pub fn find_request(&self, branch: &str) -> anyhow::Result<Option<(i64, String)>> {
        let v = if self.github {
            let owner = self.repo.split('/').next().unwrap_or_default();
            self.call("GET", &format!("/pulls?state=open&head={owner}:{branch}"), None)?
        } else {
            self.call("GET", &format!("/merge_requests?state=opened&source_branch={branch}"), None)?
        };
        Ok(v.as_array().and_then(|a| a.first()).map(|r| self.request_of(r)))
    }

    fn request_of(&self, r: &Value) -> (i64, String) {
        if self.github {
            (r["number"].as_i64().unwrap_or(0), r["html_url"].as_str().unwrap_or_default().to_string())
        } else {
            (r["iid"].as_i64().unwrap_or(0), r["web_url"].as_str().unwrap_or_default().to_string())
        }
    }

    pub fn open_request(&self, branch: &str, base: &str, title: &str, body: &str) -> anyhow::Result<(i64, String)> {
        let v = if self.github {
            self.call("POST", "/pulls", Some(json!({"title": title, "head": branch, "base": base, "body": body})))?
        } else {
            self.call("POST", "/merge_requests", Some(json!({"title": title, "source_branch": branch, "target_branch": base, "description": body})))?
        };
        Ok(self.request_of(&v))
    }

    pub fn update_request(&self, number: i64, body: &str) -> anyhow::Result<()> {
        if self.github {
            self.call("PATCH", &format!("/pulls/{number}"), Some(json!({"body": body})))?;
        } else {
            self.call("PUT", &format!("/merge_requests/{number}"), Some(json!({"description": body})))?;
        }
        Ok(())
    }

    pub fn set_status(&self, sha: &str, context: &str, state: State, description: &str, url: &str) -> anyhow::Result<()> {
        let desc: String = description.chars().take(140).collect();
        if self.github {
            self.call("POST", &format!("/statuses/{sha}"), Some(json!({"state": state.name(), "context": context, "description": desc, "target_url": url})))?;
        } else {
            let s = match state {
                State::Pending => "pending",
                State::Success => "success",
                State::Failure => "failed",
            };
            self.call("POST", &format!("/statuses/{sha}"), Some(json!({"state": s, "name": context, "description": desc, "target_url": url})))?;
        }
        Ok(())
    }
}

/// The URL the worker pushes to: `push_url`, else the host's HTTPS clone URL.
pub fn push_url(cfg: &Config, p: &ProjectConfig) -> Option<String> {
    if let Some(u) = &p.push_url {
        return Some(u.clone());
    }
    let repo = p.repo.as_ref()?;
    Some(if p.host == "github" {
        format!("https://github.com/{repo}.git")
    } else {
        format!("{}/{repo}.git", cfg.gitlab.as_ref().and_then(|g| g.base_url.clone()).unwrap_or_else(|| "https://gitlab.com".into()).trim_end_matches('/'))
    })
}

/// Push `egm-dsh/<feature>` from the checkout; returns the pushed head. The token travels in a
/// git config entry passed through the environment, never on the command line.
pub fn push_branch(checkout: &Path, url: &str, github: bool, token: &str, feature: &str) -> anyhow::Result<Option<String>> {
    let branch = format!("egm-dsh/{feature}");
    let head = std::process::Command::new("git").arg("-C").arg(checkout).args(["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).output()?;
    if !head.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let user = if github { "x-access-token" } else { "oauth2" };
    use base64::Engine;
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
    // egm-team owns egm-dsh/* branches: a rejected build is reverted locally, so the push may rewind
    let out = std::process::Command::new("git")
        .arg("-C").arg(checkout)
        .args(["push", "--force", "--quiet", url, &format!("refs/heads/{branch}:refs/heads/{branch}")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {basic}"))
        .output()?;
    anyhow::ensure!(out.status.success(), "git push {branch}: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(Some(sha))
}

// ------------------------------------------------------------------------------- the sync

/// The pull or merge request description: the evidence, and where the record is.
#[allow(clippy::too_many_arguments)]
pub fn request_body(cfg: &Config, project: &str, feature: &str, doc: &str, doc_hash: &str, evidence: Option<&Value>, events: usize, last_event: i64) -> String {
    let url = format!("{}/p/{project}/f/{feature}", cfg.public_url);
    let mut s = format!("egm-dsh feature `{feature}`, built from its design doc `{doc}` (doc hash `{doc_hash}`).\n\n");
    match evidence {
        Some(e) => {
            let passed = e["passed"].as_bool().unwrap_or(false);
            s += &format!("**Evidence gate: {}** · risk {}", if passed { "passed" } else { "failed" }, e["tier"].as_str().unwrap_or("not assessed"));
            if let (Some(k), Some(t)) = (e["mutation"]["killed"].as_u64(), e["mutation"]["tried"].as_u64()) {
                s += &format!(" · seeded bugs caught: {k} of {t}");
            }
            s += "\n\n";
            for c in e["checks"].as_array().into_iter().flatten() {
                let ok = c["ok"].as_bool().unwrap_or(false);
                s += &format!("- {} {}: {}\n", if ok { "✔" } else { "✖" }, c["name"].as_str().unwrap_or(""), c["detail"].as_str().unwrap_or(""));
            }
        }
        None => s += "**Evidence gate:** not run yet.\n",
    }
    s += &format!(
        "\n**Merging.** `{GATE_CONTEXT}` turns green when the gate passed for the current build and doc; `{MERGE_CONTEXT}` when someone other than the author approved it in egm-team after that. Make both required checks.\n\n\
         **Audit record.** {events} event(s) for this feature in Vesdb, the latest #{last_event}. An auditor downloads the offline bundle with **Export** on [the feature in egm-team]({url}) and checks it with `vesdb-verify bundle`.\n\n\
         <sub>Written by egm-team. Edits here are replaced at the next update.</sub>\n"
    );
    s
}

/// Keep every host project's requests and statuses current. Runs beside the worker.
pub async fn run(cfg: Config, log: Arc<dyn AuditLog>) {
    if !cfg.projects.iter().any(|p| p.repo.is_some()) {
        return;
    }
    let client = loop {
        match db::connect(&cfg.database_url).await {
            Ok(c) => break c,
            Err(e) => {
                eprintln!("egm-team host: database: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    // per feature: the last event id and head synced, and the last body sent
    let mut seen: HashMap<(String, String), (i64, String, String)> = HashMap::new();
    loop {
        for p in cfg.projects.iter().filter(|p| p.repo.is_some()) {
            if let Err(e) = sync_project(&cfg, p, &client, &log, &mut seen).await {
                eprintln!("egm-team host: {}: {e}", p.name);
            }
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
}

async fn sync_project(cfg: &Config, p: &ProjectConfig, c: &tokio_postgres::Client, log: &Arc<dyn AuditLog>,
                      seen: &mut HashMap<(String, String), (i64, String, String)>) -> anyhow::Result<()> {
    let rows = c.query("SELECT f.slug, f.doc, f.doc_hash, COALESCE(MAX(t.id), 0), COUNT(t.id) FROM features f LEFT JOIN timeline t ON t.project=f.project AND t.feature=f.slug
                        WHERE f.project=$1 GROUP BY f.slug, f.doc, f.doc_hash", &[&p.name]).await?;
    for r in rows {
        let (slug, doc, doc_hash, last, n): (String, String, String, i64, i64) = (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
        let k = (p.name.clone(), slug.clone());
        let (pcfg, url) = (p.clone(), push_url(cfg, p).unwrap_or_default());
        let checkout = p.checkout.clone();
        let github = p.host == "github";
        let s2 = slug.clone();
        let Some(sha) = tokio::task::spawn_blocking(move || push_branch(&checkout, &url, github, &pcfg.host_token, &s2)).await?? else { continue };
        if seen.get(&k).is_some_and(|(id, head, _)| *id == last && *head == sha) {
            continue;
        }
        let host = Host::for_project(cfg, p).ok_or_else(|| anyhow::anyhow!("no host"))?;
        let host = Arc::new(host);
        let evidence = crate::checkout::evidence(&p.checkout, &slug);
        let body = request_body(cfg, &p.name, &slug, &doc, &doc_hash, evidence.as_ref(), n as usize, last);
        let branch = format!("egm-dsh/{slug}");
        // the request
        let known = c.query_opt("SELECT data FROM timeline WHERE project=$1 AND feature=$2 AND kind='host.pr-opened' ORDER BY id DESC LIMIT 1", &[&p.name, &slug]).await?;
        let (h, b2, br, base) = (host.clone(), body.clone(), branch.clone(), p.base_branch.clone());
        let title = format!("egm-dsh: {slug}");
        let found = tokio::task::spawn_blocking(move || -> anyhow::Result<(i64, String, bool)> {
            if let Some((n, u)) = h.find_request(&br)? {
                return Ok((n, u, false));
            }
            let (n, u) = h.open_request(&br, &base, &title, &b2)?;
            Ok((n, u, true))
        }).await??;
        if found.2 || known.is_none() {
            record(c, log, &p.name, &slug, "host.pr-opened", json!({"number": found.0, "url": found.1, "branch": branch, "host": p.host}), &format!("host/{}/{slug}/pr/{}", p.name, found.0)).await?;
        }
        if !found.2 && seen.get(&k).is_none_or(|s| s.2 != body) {
            let (h, b2, n) = (host.clone(), body.clone(), found.0);
            tokio::task::spawn_blocking(move || h.update_request(n, &b2)).await??;
        }
        // the statuses, set when they change for this head
        let f = facts(c, &p.name, &slug).await?;
        let target = format!("{}/p/{}/f/{slug}", cfg.public_url, p.name);
        for (context, state, desc) in statuses(&f) {
            let prev = c.query_opt(
                "SELECT data FROM timeline WHERE project=$1 AND feature=$2 AND kind='host.status-set' AND data->>'sha'=$3 AND data->>'context'=$4 ORDER BY id DESC LIMIT 1",
                &[&p.name, &slug, &sha, &context]).await?;
            if prev.is_some_and(|r| { let d: Value = r.get(0); d["state"] == state.name() && d["description"] == desc.as_str() }) {
                continue;
            }
            let (h, s, d, t) = (host.clone(), sha.clone(), desc.clone(), target.clone());
            tokio::task::spawn_blocking(move || h.set_status(&s, context, state, &d, &t)).await??;
            let key = format!("host/{}/{slug}/{sha}/{context}/{}/{}", p.name, state.name(), &hex(&Sha256::digest(desc.as_bytes()))[..12]);
            record(c, log, &p.name, &slug, "host.status-set", json!({"sha": sha, "context": context, "state": state.name(), "description": desc}), &key).await?;
        }
        let last_now: i64 = c.query_one("SELECT COALESCE(MAX(id), 0) FROM timeline WHERE project=$1 AND feature=$2", &[&p.name, &slug]).await?.get(0);
        seen.insert(k, (last_now, sha, body));
    }
    Ok(())
}

async fn record(c: &impl GenericClient, log: &Arc<dyn AuditLog>, project: &str, feature: &str, kind: &str, data: Value, key: &str) -> anyhow::Result<()> {
    let ev = AuditEvent { kind: kind.into(), project: project.into(), feature: feature.into(), actor: "worker:host".into(), data };
    let (log, key) = (log.clone(), key.to_string());
    let written = tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
    for w in &written {
        db::apply(c, w).await?;
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------------------------- webhooks

/// `X-Hub-Signature-256: sha256=<hex>` over the exact body.
pub fn github_signature_ok(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(sig) = header.strip_prefix("sha256=") else { return false };
    let Ok(bytes) = (0..sig.len()).step_by(2).map(|i| u8::from_str_radix(sig.get(i..i + 2).unwrap_or("x"), 16)).collect::<std::result::Result<Vec<u8>, _>>() else { return false };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else { return false };
    mac.update(body);
    mac.verify_slice(&bytes).is_ok()
}

/// `This reverts commit <sha>` lines in a commit message.
pub fn reverted_shas(message: &str) -> Vec<String> {
    message
        .lines()
        .filter_map(|l| l.trim().strip_prefix("This reverts commit "))
        .map(|r| r.trim_end_matches('.').trim().to_string())
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect()
}

/// What a webhook delivery means: events to record (feature, kind, actor, data).
pub fn interpret(github: bool, event: &str, payload: &Value, base_branch: &str, merged: &HashMap<String, String>) -> Vec<(String, &'static str, String, Value)> {
    let mut out = vec![];
    let host = if github { "github" } else { "gitlab" };
    let feature_of = |branch: &str| branch.strip_prefix("egm-dsh/").filter(|f| crate::config::valid_name(f)).map(String::from);
    if github && event == "pull_request" && payload["action"] == "closed" && payload["pull_request"]["merged"] == true {
        let pr = &payload["pull_request"];
        if let Some(f) = feature_of(pr["head"]["ref"].as_str().unwrap_or_default()) {
            out.push((f, "merge.completed", format!("github:{}", pr["merged_by"]["id"].as_i64().unwrap_or(0)), json!({
                "number": pr["number"], "merge_sha": pr["merge_commit_sha"], "base": pr["base"]["ref"], "merged_by": pr["merged_by"]["login"], "host": host,
            })));
        }
    }
    if !github && event == "Merge Request Hook" && payload["object_attributes"]["action"] == "merge" {
        let mr = &payload["object_attributes"];
        if let Some(f) = feature_of(mr["source_branch"].as_str().unwrap_or_default()) {
            out.push((f, "merge.completed", format!("gitlab:{}", payload["user"]["id"].as_i64().unwrap_or(0)), json!({
                "number": mr["iid"], "merge_sha": mr["merge_commit_sha"], "base": mr["target_branch"], "merged_by": payload["user"]["username"], "host": host,
            })));
        }
    }
    let push = (github && event == "push") || (!github && event == "Push Hook");
    if push && payload["ref"].as_str() == Some(&format!("refs/heads/{base_branch}")) {
        let actor = if github { format!("github:{}", payload["sender"]["id"].as_i64().unwrap_or(0)) } else { format!("gitlab:{}", payload["user_id"].as_i64().unwrap_or(0)) };
        for commit in payload["commits"].as_array().into_iter().flatten() {
            for sha in reverted_shas(commit["message"].as_str().unwrap_or_default()) {
                if let Some(f) = merged.get(&sha) {
                    out.push((f.clone(), "change.reverted", actor.clone(), json!({"merge_sha": sha, "revert_sha": commit["id"], "host": host})));
                }
            }
        }
    }
    out
}

struct Plain(StatusCode, &'static str);

impl IntoResponse for Plain {
    fn into_response(self, _cx: &Cx) -> Result<Response> {
        Ok(Response::builder().status(self.0).header("Content-Type", "text/plain; charset=utf-8").body(Body::from(self.1))?)
    }
}

async fn hook(cx: &Cx, github: bool, body: String) -> Result<Plain> {
    let a = app(cx);
    let p = project(cx);
    let Some(pc) = a.cfg.project(p).filter(|pc| pc.repo.is_some() && (pc.host == "github") == github) else { return Ok(Plain(StatusCode::NOT_FOUND, "no such project")) };
    let h = headers(cx);
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let ok = if github {
        github_signature_ok(&pc.webhook_secret, body.as_bytes(), &get("x-hub-signature-256"))
    } else {
        crate::oauth::same_secret(&get("x-gitlab-token"), &pc.webhook_secret)
    };
    if !ok {
        authz::refuse(a.log.clone(), p, &format!("{}-webhook", pc.host), Action::RecordJob, "webhook signature does not match").await;
        return Ok(Plain(StatusCode::UNAUTHORIZED, "bad signature"));
    }
    let event = if github { get("x-github-event") } else { get("x-gitlab-event") };
    let delivery = if github { get("x-github-delivery") } else { get("x-gitlab-event-uuid") };
    let delivery = if delivery.is_empty() { hex(&Sha256::digest(body.as_bytes())) } else { delivery };
    let payload: Value = serde_json::from_str(&body).map_err(|_| bad_request("the body is not JSON"))?;
    let rows = a.db.query("SELECT feature, data->>'merge_sha' FROM timeline WHERE project=$1 AND kind='merge.completed'", &[&p]).await.map_err(|e| topcoat::Error::from_anyhow(e.into()))?;
    let merged: HashMap<String, String> = rows.iter().filter_map(|r| Some((r.get::<_, Option<String>>(1)?, r.get::<_, String>(0)))).collect();
    let events = interpret(github, &event, &payload, &pc.base_branch, &merged);
    for (i, (feature, kind, actor, data)) in events.into_iter().enumerate() {
        let ev = AuditEvent { kind: kind.into(), project: p.into(), feature, actor, data };
        let log = a.log.clone();
        let key = format!("hook/{}/{delivery}/{i}", pc.host);
        let written = tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
        for w in &written {
            db::apply(&*a.db, w).await.ah()?;
        }
    }
    Ok(Plain(StatusCode::OK, "ok"))
}

/// GitHub webhook (pull requests and pushes); authenticated by its HMAC signature.
#[route(POST "/hooks/github/{project}")]
pub async fn github_hook(cx: &Cx, body: String) -> Result<Plain> {
    hook(cx, true, body).await
}

/// GitLab webhook (merge requests and pushes); authenticated by its secret token.
#[route(POST "/hooks/gitlab/{project}")]
pub async fn gitlab_hook(cx: &Cx, body: String) -> Result<Plain> {
    hook(cx, false, body).await
}

// ------------------------------------------------------------------- export and defects

struct Download {
    name: String,
    body: Vec<u8>,
}

impl IntoResponse for Download {
    fn into_response(self, _cx: &Cx) -> Result<Response> {
        Ok(Response::builder()
            .header("Content-Type", "application/json")
            .header("Content-Disposition", format!("attachment; filename=\"{}\"", self.name))
            .body(Body::from(self.body))?)
    }
}

/// The feature's offline evidence bundle from Vesdb: every event of the feature, with inclusion
/// proofs against a signed checkpoint (`VesdbLink.export_exact`). Recorded as `export.created`.
#[route(GET "/p/{project}/f/{feature}/export")]
pub async fn export(cx: &Cx) -> Result<Download> {
    let (p, slug) = (project(cx), feature(cx));
    let v = authz::require(cx, p, Action::Export).await?;
    let a = app(cx);
    db::feature(&*a.db, p, slug).await.ah()?.ok_or_else(not_found)?;
    let ids: Vec<u64> = a.db.query("SELECT id FROM timeline WHERE project=$1 AND feature=$2 ORDER BY id", &[&p, &slug]).await
        .map_err(|e| topcoat::Error::from_anyhow(e.into()))?.iter().map(|r| r.get::<_, i64>(0) as u64).collect();
    let log = a.log.clone();
    let ids2 = ids.clone();
    let bundle = tokio::task::spawn_blocking(move || log.bundle(&ids2)).await??;
    let body = serde_json::to_vec_pretty(&bundle).map_err(|e| topcoat::Error::from_anyhow(e.into()))?;
    let digest = hex(&Sha256::digest(&body));
    let ev = AuditEvent { kind: "export.created".into(), project: p.into(), feature: slug.into(), actor: v.id.clone(), data: json!({"events": ids.len(), "sha256": digest}) };
    authz::record_now(cx, ev).await?;
    Ok(Download { name: format!("egm-{p}-{slug}-bundle.json"), body })
}

#[derive(Deserialize)]
struct DefectForm {
    note: String,
}

/// Someone found a defect in a merged change: it counts against the gate's verdict (gate MCC).
#[route(POST "/p/{project}/f/{feature}/defect")]
pub async fn defect(cx: &Cx, Form(form): Form<DefectForm>) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, _) = authz::require_feature(cx, p, slug, Action::Comment).await?;
    let note = form.note.trim();
    if note.is_empty() || note.len() > 4000 {
        return Err(bad_request("say what the defect is, in 1 to 4000 characters").into());
    }
    let merged = app(cx).db.query_opt("SELECT 1 FROM timeline WHERE project=$1 AND feature=$2 AND kind='merge.completed'", &[&p, &slug]).await
        .map_err(|e| topcoat::Error::from_anyhow(e.into()))?;
    if merged.is_none() {
        return Err(bad_request("only a merged change can have a defect reported; comment instead").into());
    }
    let ev = AuditEvent { kind: "defect.reported".into(), project: p.into(), feature: slug.into(), actor: v.id.clone(), data: json!({}) };
    authz::record_with_words(cx, ev, "note", note).await?;
    Ok(see_other(format!("/p/{p}/f/{slug}#host")))
}

/// The code-host section of the feature page.
#[component]
pub async fn host_panel(cx: &Cx, p: &str, slug: &str) -> Result<impl View> {
    let a = app(cx);
    let v = authz::signed_in(cx).await?;
    let roles = authz::roles_of(cx, &v, p).await?;
    let can_export = egm_core::policy::allowed(&roles, false, egm_core::evidence::Tier::Low, Action::Export).is_ok();
    let can_report = egm_core::policy::allowed(&roles, false, egm_core::evidence::Tier::Low, Action::Comment).is_ok();
    let pc = a.cfg.project(p).cloned().ok_or_else(not_found)?;
    let ev = |kind: &'static str| {
        let db = a.db.clone();
        let (p, slug) = (p.to_string(), slug.to_string());
        async move {
            db.query("SELECT id, actor, data, at FROM timeline WHERE project=$1 AND feature=$2 AND kind=$3 ORDER BY id DESC", &[&p, &slug, &kind]).await
                .map(|rows| rows.iter().map(|r| (r.get::<_, i64>(0), r.get::<_, String>(1), r.get::<_, Value>(2), r.get::<_, String>(3))).collect::<Vec<_>>())
                .map_err(|e| topcoat::Error::from_anyhow(e.into()))
        }
    };
    let prs = ev("host.pr-opened").await?;
    let sts = ev("host.status-set").await?;
    let merges = ev("merge.completed").await?;
    let reverts = ev("change.reverted").await?;
    let defects = db::defects(&*a.db, p, slug).await.ah()?;
    let f = facts(&*a.db, p, slug).await.ah()?;
    let now = statuses(&f);
    let latest_sha = sts.first().map(|s| s.2["sha"].as_str().unwrap_or_default().to_string());
    let posted: Vec<(String, String, String)> = sts.iter()
        .filter(|s| Some(s.2["sha"].as_str().unwrap_or_default().to_string()) == latest_sha)
        .map(|s| (s.2["context"].as_str().unwrap_or_default().to_string(), s.2["state"].as_str().unwrap_or_default().to_string(), s.2["description"].as_str().unwrap_or_default().to_string()))
        .fold(vec![], |mut acc: Vec<(String, String, String)>, x| { if !acc.iter().any(|y| y.0 == x.0) { acc.push(x) } acc });
    let has_repo = pc.repo.is_some();
    Ok(view! {
        <h2 id="host">"Code host"</h2>
        if !has_repo {
            <p class="muted">"This project has no repository configured, so egm-team sets no statuses and opens no pull requests. The statuses below are what it would set."</p>
        }
        if let Some((_, _, d, _)) = prs.first() {
            <p id="host-request">(if pc.host == "github" { "Pull request " } else { "Merge request " })
                <a href=(d["url"].as_str().unwrap_or("#").to_string())>(format!("#{}", d["number"]))</a>
                " for branch "<code>(d["branch"].as_str().unwrap_or_default().to_string())</code>
            </p>
        }
        <table id="host-statuses">
            <tr><th>"Status"</th><th>"Now"</th><th>"Why"</th><th>"Last set on the host"</th></tr>
            for (context, state, desc) in &now {
                <tr data-context=(*context)>
                    <td><code>(*context)</code></td>
                    <td class=(match state { State::Success => "ok", State::Failure => "bad", State::Pending => "muted" }) data-state=(state.name())>(state.name())</td>
                    <td>(desc.as_str())</td>
                    <td class="muted">(posted.iter().find(|x| x.0 == *context).map(|x| x.1.clone()).unwrap_or_else(|| "not yet".into()))</td>
                </tr>
            }
        </table>
        if let Some(sha) = &latest_sha { <p class="muted">"Branch head: "<code>(sha.as_str())</code></p> }
        for (_, actor, d, at) in &merges {
            <p class="ok" id="merged">(format!("Merged as {} by {} ({actor})", d["merge_sha"].as_str().unwrap_or("?").chars().take(10).collect::<String>(), d["merged_by"].as_str().unwrap_or("?")))" · "(crate::app::when(at))</p>
        }
        for (_, _, d, at) in &reverts {
            <p class="bad" id="reverted">(format!("Reverted by {}", d["revert_sha"].as_str().unwrap_or("?").chars().take(10).collect::<String>()))" · "(crate::app::when(at))</p>
        }
        if !defects.is_empty() {
            <ul id="defects">
                for (who, note, at) in &defects { <li>(who)": "(note)" · "(crate::app::when(at))</li> }
            </ul>
        }
        if !merges.is_empty() && can_report {
            <form method="POST" action=(format!("/p/{p}/f/{slug}/defect")) id="defect-form">
                <textarea name="note" rows="2" cols="70" required="required" maxlength="4000" placeholder="A defect found in this change after merge"></textarea>
                <br><button>"Report a defect"</button>
            </form>
        }
        if can_export {
            <p><a id="export" href=(format!("/p/{p}/f/{slug}/export"))>"Export the audit bundle"</a>
                <span class="muted">" (offline evidence: every event of this feature with Vesdb's proofs; check with vesdb-verify bundle)"</span></p>
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(build: Option<i64>, gate: Option<(i64, bool, &str)>, approvals: &[(i64, &str, &str)]) -> Facts {
        Facts {
            last_build: build,
            last_gate: gate.map(|(i, p, d)| (i, p, d.to_string())),
            doc_hash: "d1".into(),
            author: "github:1".into(),
            approvals: approvals.iter().map(|(i, b, d)| (*i, b.to_string(), d.to_string())).collect(),
        }
    }

    #[test]
    fn the_status_rule() {
        assert_eq!(gate_status(&facts(None, None, &[])).0, State::Pending);
        assert_eq!(gate_status(&facts(Some(5), Some((3, true, "d1")), &[])).0, State::Pending, "a gate before the latest build does not count");
        assert_eq!(gate_status(&facts(Some(5), Some((6, true, "d0")), &[])).0, State::Pending, "a gate on another doc does not count");
        assert_eq!(gate_status(&facts(Some(5), Some((6, false, "d1")), &[])).0, State::Failure);
        assert_eq!(gate_status(&facts(Some(5), Some((6, true, "d1")), &[])).0, State::Success);
        assert_eq!(merge_status(&facts(Some(5), Some((6, true, "d1")), &[])).0, State::Pending);
        assert_eq!(merge_status(&facts(Some(5), Some((6, true, "d1")), &[(7, "github:1", "d1")])).0, State::Pending, "the author's own approval never counts");
        assert_eq!(merge_status(&facts(Some(5), Some((6, true, "d1")), &[(4, "github:2", "d1")])).0, State::Pending, "an approval before the gate does not count");
        assert_eq!(merge_status(&facts(Some(5), Some((6, true, "d1")), &[(7, "github:2", "d0")])).0, State::Pending, "an approval of another doc does not count");
        assert_eq!(merge_status(&facts(Some(5), Some((6, true, "d1")), &[(7, "github:2", "d1")])).0, State::Success);
        assert_eq!(merge_status(&facts(Some(8), Some((6, true, "d1")), &[(7, "github:2", "d1")])).0, State::Pending, "a new build needs a new gate and approval");
        assert_eq!(merge_status(&facts(Some(5), Some((6, false, "d1")), &[(7, "github:2", "d1")])).0, State::Pending);
    }

    #[test]
    fn github_signatures() {
        let body = br#"{"zen":"x"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"a-long-enough-secret!!").unwrap();
        mac.update(body);
        let sig = format!("sha256={}", hex(&mac.finalize().into_bytes()));
        assert!(github_signature_ok("a-long-enough-secret!!", body, &sig));
        assert!(!github_signature_ok("another-secret-entirely", body, &sig));
        assert!(!github_signature_ok("a-long-enough-secret!!", b"{}", &sig));
        assert!(!github_signature_ok("a-long-enough-secret!!", body, "sha1=00"));
        assert!(!github_signature_ok("a-long-enough-secret!!", body, "sha256=zz"));
    }

    #[test]
    fn webhooks_are_read() {
        let pr = json!({"action": "closed", "pull_request": {"merged": true, "number": 4, "merge_commit_sha": "a".repeat(40),
            "head": {"ref": "egm-dsh/tags"}, "base": {"ref": "main"}, "merged_by": {"id": 7, "login": "ada"}}});
        let out = interpret(true, "pull_request", &pr, "main", &HashMap::new());
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].0.as_str(), out[0].1, out[0].2.as_str()), ("tags", "merge.completed", "github:7"));
        let other = json!({"action": "closed", "pull_request": {"merged": true, "head": {"ref": "feature/x"}}});
        assert!(interpret(true, "pull_request", &other, "main", &HashMap::new()).is_empty(), "only egm-dsh branches");
        let merged: HashMap<String, String> = [("a".repeat(40), "tags".to_string())].into();
        let push = json!({"ref": "refs/heads/main", "sender": {"id": 9}, "commits": [
            {"id": "b".repeat(40), "message": format!("Revert \"egm-dsh: tags\"\n\nThis reverts commit {}.", "a".repeat(40))}]});
        let out = interpret(true, "push", &push, "main", &merged);
        assert_eq!((out[0].0.as_str(), out[0].1), ("tags", "change.reverted"));
        assert!(interpret(true, "push", &json!({"ref": "refs/heads/dev", "commits": push["commits"]}), "main", &merged).is_empty());
        let mr = json!({"object_attributes": {"action": "merge", "iid": 3, "source_branch": "egm-dsh/tags", "target_branch": "main", "merge_commit_sha": "c".repeat(40)},
            "user": {"id": 5, "username": "rex"}});
        assert_eq!(interpret(false, "Merge Request Hook", &mr, "main", &HashMap::new())[0].2, "gitlab:5");
    }
}
