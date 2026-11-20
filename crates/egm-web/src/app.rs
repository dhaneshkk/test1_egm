//! The read-only team view (phase 2): sign-in, projects, the feature board, a feature's design
//! doc with its revisions, goldfish reviews and evidence, metrics, members and the audit trail.
//!
//! Rule for this file: every `#[page]` and `#[route]` calls `authz::` first, except the
//! sign-in pages, the OAuth callback and the health check, which `tests/policy_lint.rs` lists by
//! name. There are no shards or procedures yet; when they come, the same test covers them.
//! Text from the checkout (docs, review findings) is rendered as text, never as HTML.

use crate::authz::{self, app};
use crate::oauth::{self, Pending, Provider};
use crate::{checkout, db, Ah, SYSTEM};
use egm_audit::AuditEvent;
use egm_core::policy::Action;
use serde_json::{json, Value};
use topcoat::context::Cx;
use topcoat::cookie::{time::Duration, Cookie, CookieLayer, Cookies, SameSite, cookies};
use topcoat::router::error::{bad_request, not_found, see_other, ForbiddenError, NotFoundError, SeeOther};
use topcoat::router::{layout, page, path_param, query_params, route, Router, Slot};
use topcoat::session;
use topcoat::router::StatusCode;
use topcoat::view::{component, error_boundary, view, View};
use topcoat::Result;

pub fn router(state: crate::App) -> Router {
    Router::builder()
        .layout(root)
        .page(index)
        .page(sign_in)
        .route(auth_start)
        .route(auth_callback)
        .route(sign_out)
        .page(board)
        .page(feature_page)
        .page(revision)
        .page(metrics)
        .page(members)
        .page(audit_trail)
        .route(health)
        .with(state)
        .layer(CookieLayer::new())
        .build()
}

path_param!(project);
path_param!(feature);
path_param!(rev);
path_param!(provider);

const STYLE: &str = "body{font-family:system-ui,sans-serif;margin:0;color:#1d1d1f;background:#fafafa}\
header{display:flex;gap:16px;align-items:center;padding:12px 24px;background:#fff;border-bottom:1px solid #ddd}\
header a{color:inherit;text-decoration:none}main{padding:16px 24px;max-width:1100px}\
table{border-collapse:collapse;width:100%;background:#fff}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid #eee;vertical-align:top}\
pre{white-space:pre-wrap;background:#fff;border:1px solid #ddd;padding:12px}\
.phase{display:inline-block;padding:1px 8px;border-radius:10px;background:#eef;font-size:13px}\
td code{white-space:pre-wrap;word-break:break-all;font-size:12px}.nowrap{white-space:nowrap}\
.ok{color:#176f2c}.bad{color:#b3261e}.muted{color:#666;font-size:13px}nav.tabs a{margin-right:14px}\
pre{overflow-wrap:anywhere}\
@media (max-width:600px){main,header{padding:10px 16px}table{display:block;overflow-x:auto}.nowrap{white-space:normal}}";

#[layout("/")]
async fn root(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    let v = authz::viewer(cx).await?;
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"egm-team"</title>
                <style>(STYLE)</style>
            </head>
            <body>
                <header>
                    <a href="/"><strong>"egm-team"</strong></a>
                    <span style="flex:1"></span>
                    if let Some(v) = &v {
                        <span class="muted" id="viewer">(&v.login)</span>
                        <form method="POST" action="/sign-out" style="margin:0">
                            <button>"Sign out"</button>
                        </form>
                    }
                </header>
                <main>
                    error_boundary(
                        fallback: |error| {
                            let forbidden = if error.downcast_ref::<ForbiddenError>().is_some() {
                                true
                            } else if error.downcast_ref::<NotFoundError>().is_some() {
                                false
                            } else {
                                return Err(error);
                            };
                            Ok(view! {
                                if forbidden {
                                    (StatusCode::FORBIDDEN)
                                    <h1 id="forbidden">"No access"</h1>
                                    <p>"You have no role in this project, so egm-team cannot show it to you. The refusal has been recorded in the audit log."</p>
                                    <p>"Ask a project admin to add you."</p>
                                } else {
                                    (StatusCode::NOT_FOUND)
                                    <h1 id="not-found">"Not found"</h1>
                                    <p>"There is no such project, feature or revision."</p>
                                }
                                <p><a href="/">"Back to your projects"</a></p>
                            })
                        },
                        (slot)
                    )
                </main>
            </body>
        </html>
    })
}

#[component]
async fn phase_badge(cx: &Cx, p: &str) -> Result<impl View> {
    Ok(view! { <span class="phase" data-phase=(p)>(p)</span> })
}

#[component]
async fn tabs(cx: &Cx, project: &str) -> Result<impl View> {
    Ok(view! {
        <nav class="tabs">
            <a href=(format!("/p/{project}"))>"Features"</a>
            <a href=(format!("/p/{project}/metrics"))>"Metrics"</a>
            <a href=(format!("/p/{project}/members"))>"Members"</a>
            <a href=(format!("/p/{project}/audit"))>"Audit trail"</a>
        </nav>
    })
}

// ------------------------------------------------------------------------------- sign-in

#[page("/sign-in")]
async fn sign_in(cx: &Cx) -> Result<impl View> {
    let a = app(cx);
    let github = a.cfg.github.is_some();
    let gitlab = a.cfg.gitlab.is_some();
    Ok(view! {
        <h1>"Sign in"</h1>
        <p>"egm-team uses your GitHub or GitLab account. It reads only who you are."</p>
        if github { <p><a id="signin-github" href="/auth/github/start">"Sign in with GitHub"</a></p> }
        if gitlab { <p><a id="signin-gitlab" href="/auth/gitlab/start">"Sign in with GitLab"</a></p> }
    })
}

fn oauth_cookie_secure(cx: &Cx) -> bool {
    app(cx).cfg.public_url.starts_with("https://")
}

#[route(GET "/auth/{provider}/start")]
async fn auth_start(cx: &Cx) -> Result<SeeOther> {
    let a = app(cx);
    let p = Provider::parse(provider(cx)).ok_or_else(not_found)?;
    let oa = match p {
        Provider::GitHub => a.cfg.github.as_ref(),
        Provider::GitLab => a.cfg.gitlab.as_ref(),
    }
    .ok_or_else(not_found)?;
    let pending = Pending::new(p).ah()?;
    cookies(cx).add(
        Cookie::build(("egm_oauth", pending.encode()))
            .path("/auth")
            .max_age(Duration::minutes(10))
            .http_only(true)
            .secure(oauth_cookie_secure(cx))
            .same_site(SameSite::Lax),
    );
    Ok(see_other(oauth::authorize_url(oa, &a.cfg.public_url, &pending)))
}

#[query_params(error = bad_request)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[route(GET "/auth/{provider}/callback")]
async fn auth_callback(cx: &Cx) -> Result<SeeOther> {
    let a = app(cx);
    let p = Provider::parse(provider(cx)).ok_or_else(not_found)?;
    let q = query_params::<Callback>(cx)?;
    let jar = cookies(cx);
    let pending = jar.get("egm_oauth").and_then(|c| Pending::decode(c.value()));
    jar.remove(Cookie::build(("egm_oauth", "")).path("/auth"));
    let fail = |reason: &str| AuditEvent {
        kind: "auth.sign-in-failed".into(),
        project: SYSTEM.into(),
        feature: String::new(),
        actor: "anonymous".into(),
        data: json!({"provider": p.name(), "reason": reason}),
    };
    let (Some(pending), Some(code), Some(state)) = (pending, q.code.clone(), q.state.clone()) else {
        let why = q.error.clone().unwrap_or_else(|| "missing code, state or sign-in cookie".into());
        authz::record(a.log.clone(), fail(&why)).await;
        return Err(bad_request(format!("sign-in failed: {why}")).into());
    };
    if pending.provider != p || !oauth::same_secret(&pending.state, &state) {
        authz::record(a.log.clone(), fail("state mismatch")).await;
        return Err(bad_request("sign-in failed: the sign-in link does not match this browser").into());
    }
    let oa = match p {
        Provider::GitHub => a.cfg.github.clone(),
        Provider::GitLab => a.cfg.gitlab.clone(),
    }
    .ok_or_else(not_found)?;
    let public = a.cfg.public_url.clone();
    let who = tokio::task::spawn_blocking(move || oauth::identify(&oa, &public, &pending, &code)).await?;
    let who = match who {
        Ok(w) => w,
        Err(e) => {
            authz::record(a.log.clone(), fail(&e.to_string())).await;
            return Err(bad_request(format!("sign-in failed: {e}")).into());
        }
    };
    // The sign-in is recorded before the session starts: no record, no session.
    let ev = AuditEvent {
        kind: "auth.signed-in".into(),
        project: SYSTEM.into(),
        feature: String::new(),
        actor: who.id.clone(),
        data: json!({"user": who.id, "login": who.login, "name": who.name, "provider": p.name()}),
    };
    let log = a.log.clone();
    let key = format!("web/{}", oauth::random_hex(16).ah()?);
    tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
    db::user_signed_in(&*a.db, &who.id, &who.login, &who.name).await.ah()?;
    let s = session::start(cx).await?;
    db::session_create(&*a.db, &s.token_hash[..], &who.id, s.expires_at).await.ah()?;
    a.refresh.notify_one();
    Ok(see_other("/"))
}

#[route(POST "/sign-out")]
async fn sign_out(cx: &Cx) -> Result<SeeOther> {
    if let Some(hash) = session::stop(cx).await? {
        db::session_delete(&*app(cx).db, &hash[..]).await.ah()?;
    }
    Ok(see_other("/sign-in"))
}

#[route(GET "/health")]
async fn health(cx: &Cx) -> Result<&'static str> {
    let _ = cx;
    Ok("ok")
}

// ------------------------------------------------------------------------------- pages

#[page("/")]
async fn index(cx: &Cx) -> Result<impl View> {
    let v = authz::signed_in(cx).await?;
    let projects = authz::readable_projects(cx, &v).await?;
    let a = app(cx);
    let mut rows = vec![];
    for p in &projects {
        let n = db::features(&*a.db, p).await.ah()?.len();
        let host = a.cfg.project(p).map(|c| c.host.clone()).unwrap_or_default();
        rows.push((p.clone(), n, host));
    }
    Ok(view! {
        <h1>"Projects"</h1>
        if rows.is_empty() {
            <p id="no-projects">"You are not a member of any project yet. Ask an admin to add you."</p>
        } else {
            <table id="projects">
                <tr><th>"Project"</th><th>"Features"</th><th>"Code host"</th></tr>
                for (name, n, host) in &rows {
                    <tr><td><a href=(format!("/p/{name}"))>(name)</a></td><td>(*n)</td><td>(host)</td></tr>
                }
            </table>
        }
    })
}

#[page("/p/{project}")]
async fn board(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let feats = db::features(&*app(cx).db, p).await.ah()?;
    let columns = ["designing", "ready", "built", "hardened"];
    Ok(view! {
        <h1>(p)</h1>
        tabs(project: p)
        <div id="board" style="display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;margin-top:12px">
            for col in columns {
                <section data-column=(col)>
                    <h3>(col)</h3>
                    for f in feats.iter().filter(|f| f.phase == col) {
                        <div class="card" style="background:#fff;border:1px solid #ddd;padding:8px;margin-bottom:8px">
                            <a href=(format!("/p/{p}/f/{}", f.slug))>(&f.slug)</a>
                            <div class="muted">(when(&f.updated_at))</div>
                            if let Some(e) = &f.replay_error { <div class="bad">(e)</div> }
                        </div>
                    }
                </section>
            }
        </div>
    })
}

fn verdict_class(v: &str) -> &'static str {
    if v == "ready" || v == "true" { "ok" } else { "bad" }
}

#[page("/p/{project}/f/{feature}")]
async fn feature_page(cx: &Cx) -> Result<impl View> {
    let (p, slug) = (project(cx), feature(cx));
    authz::require(cx, p, Action::Read).await?;
    let a = app(cx);
    let f = db::feature(&*a.db, p, slug).await.ah()?.ok_or_else(not_found)?;
    let co = a.cfg.project(p).map(|c| c.checkout.clone()).ok_or_else(not_found)?;
    let doc = checkout::read_doc(&co, &f.doc);
    let revs = checkout::revisions(&co, &f.doc);
    let reviews = checkout::reviews(&co, slug);
    let evidence = checkout::evidence(&co, slug);
    let events = db::timeline(&*a.db, p, Some(slug), 200).await.ah()?;
    let review_rows: Vec<(u32, String, usize, u64)> = reviews
        .iter()
        .map(|(n, r)| {
            let alarms = r["alarms"].as_array().map(|a| a.iter().filter(|x| x["status"] == "kept").count()).unwrap_or(0);
            (*n, r["verdict"].as_str().unwrap_or("?").to_string(), alarms, r["tokens"].as_u64().unwrap_or(0))
        })
        .collect();
    let checks: Vec<(String, bool, String)> = evidence
        .as_ref()
        .and_then(|e| e["checks"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|c| (c["name"].as_str().unwrap_or("").to_string(), c["ok"].as_bool().unwrap_or(false), c["detail"].as_str().unwrap_or("").to_string()))
        .collect();
    let passed = evidence.as_ref().and_then(|e| e["passed"].as_bool());
    let tier = evidence.as_ref().and_then(|e| e["tier"].as_str().map(String::from)).unwrap_or_else(|| "not assessed".into());
    let mutation = evidence.as_ref().and_then(|e| {
        let m = &e["mutation"];
        Some(format!("{} of {} seeded bugs caught", m["killed"].as_u64()?, m["tried"].as_u64()?))
    });
    Ok(view! {
        <p><a href=(format!("/p/{p}"))>(p)</a>" / "(slug)</p>
        <h1>(slug)</h1>
        phase_badge(p: &f.phase)
        if let Some(e) = &f.replay_error { <p class="bad" id="replay-error">(e)</p> }
        <p class="muted">"Doc: "(&f.doc)" · doc hash "(&f.doc_hash)</p>

        <h2>"Design doc"</h2>
        match &doc {
            Some(d) => <pre id="doc">(d)</pre>,
            None => <p class="muted" id="doc">"The doc is not in the checkout."</p>,
        }

        <h2>"Revisions"</h2>
        if revs.is_empty() { <p class="muted">"No commits of this doc yet."</p> } else {
            <table id="revisions">
                <tr><th>"Commit"</th><th>"When"</th><th>"Who"</th><th>"Message"</th></tr>
                for r in &revs {
                    <tr>
                        <td><a href=(format!("/p/{p}/f/{slug}/rev/{}", r.commit))>(&r.commit[..10])</a></td>
                        <td>(when(&r.date))</td><td>(&r.author)</td><td>(&r.subject)</td>
                    </tr>
                }
            </table>
        }

        <h2>"Goldfish reviews"</h2>
        if review_rows.is_empty() { <p class="muted">"No reviews yet."</p> } else {
            <table id="reviews">
                <tr><th>"Round"</th><th>"Verdict"</th><th>"Alarms kept"</th><th>"Tokens"</th></tr>
                for (n, v, al, t) in &review_rows {
                    <tr><td>(*n)</td><td class=(verdict_class(v))>(v)</td><td>(*al)</td><td>(*t)</td></tr>
                }
            </table>
        }

        <h2>"Evidence"</h2>
        match passed {
            None => <p class="muted" id="evidence">"No evidence gate run yet."</p>,
            Some(ok) => <div id="evidence">
                <p class=(if ok { "ok" } else { "bad" })>(if ok { "Gate passed" } else { "Gate failed" })" · risk "(&tier)
                    if let Some(m) = &mutation { " · "(m) }
                </p>
                <table>
                    <tr><th>"Check"</th><th></th><th>"Detail"</th></tr>
                    for (name, ok, detail) in &checks {
                        <tr><td>(name)</td><td class=(if *ok { "ok" } else { "bad" })>(if *ok { "✔" } else { "✖" })</td><td>(detail)</td></tr>
                    }
                </table>
            </div>,
        }

        <h2>"History"</h2>
        <table id="history">
            <tr><th>"#"</th><th>"Event"</th><th>"By"</th><th>"When"</th></tr>
            for e in &events {
                <tr><td>(e.id)</td><td>(&e.kind)</td><td>(&e.actor)</td><td class="nowrap">(when(&e.at))</td></tr>
            }
        </table>
    })
}

#[page("/p/{project}/f/{feature}/rev/{rev}")]
async fn revision(cx: &Cx) -> Result<impl View> {
    let (p, slug, r) = (project(cx), feature(cx), rev(cx));
    authz::require(cx, p, Action::Read).await?;
    let a = app(cx);
    let f = db::feature(&*a.db, p, slug).await.ah()?.ok_or_else(not_found)?;
    let co = a.cfg.project(p).map(|c| c.checkout.clone()).ok_or_else(not_found)?;
    let text = checkout::doc_at(&co, &f.doc, r).ok_or_else(not_found)?;
    Ok(view! {
        <p><a href=(format!("/p/{p}/f/{slug}"))>(slug)</a>" / "(r)</p>
        <h1>(&f.doc)" at "(&r[..r.len().min(10)])</h1>
        <pre id="doc-at">(text)</pre>
    })
}

/// `2026-11-20T07:42:52.790765627Z` or `...+05:30` → `2026-11-20 07:42`, as recorded (UTC for log times).
pub fn when(t: &str) -> String {
    if t.len() >= 16 && t.as_bytes()[10] == b'T' {
        let zone = if t.ends_with('Z') { " UTC".to_string() } else { t.get(19..).map(|z| format!(" {}", z.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit()))).unwrap_or_default() };
        format!("{} {}{}", &t[..10], &t[11..16], zone)
    } else {
        t.to_string()
    }
}

/// MCC from a confusion matrix; `None` when a margin is empty.
pub fn mcc(tp: f64, fp: f64, tn: f64, fn_: f64) -> Option<f64> {
    let d = ((tp + fp) * (tp + fn_) * (tn + fp) * (tn + fn_)).sqrt();
    (d > 0.0).then(|| (tp * tn - fp * fn_) / d)
}

#[page("/p/{project}/metrics")]
async fn metrics(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let c = db::counts(&*app(cx).db, p).await.ah()?;
    let entries = |v: &Value| -> Vec<(String, i64)> { v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0))).collect()).unwrap_or_default() };
    let phases = entries(&c["phases"]);
    let events = entries(&c["events"]);
    let n = |k: &str, s: &str| c[k][s].as_i64().unwrap_or(0);
    let (gy, gn, py, pn) = (n("goldfish", "true"), n("goldfish", "false"), n("gates", "true"), n("gates", "false"));
    Ok(view! {
        <h1>(p)" · metrics"</h1>
        tabs(project: p)
        <h2>"Features by phase"</h2>
        <table id="phases">
            for (k, n) in &phases { <tr><td>(k)</td><td>(*n)</td></tr> }
        </table>
        <h2>"Verdicts"</h2>
        <table id="verdicts">
            <tr><th></th><th>"Pass"</th><th>"Fail"</th></tr>
            <tr><td>"Goldfish review (ready)"</td><td>(gy)</td><td>(gn)</td></tr>
            <tr><td>"Evidence gate"</td><td>(py)</td><td>(pn)</td></tr>
        </table>
        <h2>"MCC"</h2>
        <p id="mcc">"Goldfish MCC needs reviewers' valid-or-noise labels on alarms, and gate MCC needs defects found after merge. Both arrive with collaboration (phase 3); until then there is nothing honest to score. MCC is shown with a warning below 29 cases per class."</p>
        <h2>"Events"</h2>
        <table id="events">
            for (k, n) in &events { <tr><td>(k)</td><td>(*n)</td></tr> }
        </table>
    })
}

#[page("/p/{project}/members")]
async fn members(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let rows = db::members(&*app(cx).db, p).await.ah()?;
    let admins = app(cx).cfg.admins.clone();
    Ok(view! {
        <h1>(p)" · members"</h1>
        tabs(project: p)
        <table id="members">
            <tr><th>"Who"</th><th>"Roles"</th><th>"Granted by"</th></tr>
            for (id, login, roles, by) in &rows {
                <tr data-user=(id)><td>(login)</td><td>(roles.join(", "))</td><td>(by)</td></tr>
            }
        </table>
        <p class="muted">"Admins in every project: "(admins.join(", "))". Roles change through the audit log (member.role-changed); editing them in the app comes with phase 3."</p>
    })
}

#[page("/p/{project}/audit")]
async fn audit_trail(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let events = db::timeline(&*app(cx).db, p, None, 500).await.ah()?;
    Ok(view! {
        <h1>(p)" · audit trail"</h1>
        tabs(project: p)
        <p class="muted">"Every event below is in Vesdb's Merkle log with a signed receipt; an auditor can verify any of them offline with vesdb-verify."</p>
        <table id="audit">
            <tr><th>"#"</th><th>"Feature"</th><th>"Event"</th><th>"By"</th><th>"Data"</th><th>"When"</th></tr>
            for e in &events {
                <tr><td>(e.id)</td><td>(&e.feature)</td><td>(&e.kind)</td><td>(&e.actor)</td><td><code>(e.data.to_string())</code></td><td class="nowrap">(when(&e.at))</td></tr>
            }
        </table>
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn times_are_short() {
        assert_eq!(super::when("2026-11-20T07:42:52.790765627Z"), "2026-11-20 07:42 UTC");
        assert_eq!(super::when("2026-11-20T13:12:40+05:30"), "2026-11-20 13:12 +05:30");
        assert_eq!(super::when(""), "");
    }

    #[test]
    fn mcc_matches_hand_values() {
        assert_eq!(super::mcc(5.0, 0.0, 5.0, 0.0), Some(1.0));
        assert_eq!(super::mcc(0.0, 5.0, 0.0, 5.0), Some(-1.0));
        assert_eq!(super::mcc(3.0, 0.0, 0.0, 0.0), None);
    }
}
