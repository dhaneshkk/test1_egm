//! Running egm-dsh from the browser (phase 4): start a feature, talk to the elephant, run the
//! goldfish, build, review and gate. Every button queues a job (`job.requested` in Vesdb, by the
//! person who pressed it); the worker ([`crate::worker`]) runs it. egm-dsh itself still decides
//! whether a step may run: a build of a doc no goldfish passed fails in the job, as on the CLI.
//!
//! Every route starts with an `authz::` check (`tests/policy_lint.rs`). Job output is shown as
//! text, never as HTML.

use crate::authz::{self, app};
use crate::worker::{action_for, argv, COMMANDS};
use crate::app::tabs;
use crate::{db, Ah};
use egm_audit::AuditEvent;
use egm_core::policy::{allowed, Action};
use serde::Deserialize;
use serde_json::{json, Value};
use topcoat::context::Cx;
use topcoat::router::content::{Form, Js, Json};
use topcoat::router::error::{bad_request, not_found, see_other, SeeOther};
use topcoat::router::{page, path_param, query_params, route};
use topcoat::view::{component, view, View};
use topcoat::Result;

path_param!(project);
path_param!(feature);
path_param!(job);

/// Write `job.requested`, apply it, wake the worker; returns the job id (the event's id).
async fn queue(cx: &Cx, p: &str, slug: &str, command: &str, args: Value, actor: &str) -> Result<i64> {
    let a = app(cx);
    let ev = AuditEvent {
        kind: "job.requested".into(),
        project: p.into(),
        feature: slug.into(),
        actor: actor.into(),
        data: json!({"command": command, "args": args}),
    };
    let log = a.log.clone();
    let key = format!("web/{}", crate::oauth::random_hex(16).ah()?);
    let written = tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
    let mut id = None;
    for w in &written {
        db::apply(&*a.db, w).await.ah()?;
        id = Some(w.id as i64);
    }
    a.work.notify_one();
    a.refresh.notify_one();
    id.ok_or_else(|| topcoat::Error::from_anyhow(anyhow::anyhow!("Vesdb wrote nothing")))
}

#[derive(Deserialize)]
struct NewFeature {
    slug: String,
    idea: String,
}

#[route(POST "/p/{project}/new")]
pub async fn new_feature(cx: &Cx, Form(form): Form<NewFeature>) -> Result<SeeOther> {
    let p = project(cx);
    let v = authz::require(cx, p, Action::CreateFeature).await?;
    let slug = form.slug.trim().to_lowercase();
    let args = json!({"text": form.idea});
    if let Err(e) = argv("elephant", &slug, &args) {
        return Err(bad_request(e).into());
    }
    let a = app(cx);
    let pending = db::jobs(&*a.db, p, Some(&slug), 50).await.ah()?.iter().any(|j| j.command == "elephant" && j.state != "done");
    if db::feature(&*a.db, p, &slug).await.ah()?.is_some() || pending {
        return Err(bad_request(format!("a feature named `{slug}` already exists; open it and talk to the elephant there")).into());
    }
    let id = queue(cx, p, &slug, "elephant", args, &v.id).await?;
    Ok(see_other(format!("/p/{p}/jobs/{id}")))
}

#[derive(Deserialize)]
struct RunForm {
    command: String,
    #[serde(default)]
    text: String,
    s0: Option<String>,
    s1: Option<String>,
    s2: Option<String>,
    s3: Option<String>,
    s4: Option<String>,
    s5: Option<String>,
    s6: Option<String>,
}

#[route(POST "/p/{project}/f/{feature}/run")]
pub async fn run(cx: &Cx, Form(form): Form<RunForm>) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, _) = authz::require_feature(cx, p, slug, Action::RunJob).await?;
    if form.command == "elephant" {
        return Err(bad_request("the feature exists already: answer the elephant instead").into());
    }
    let picked = [&form.s0, &form.s1, &form.s2, &form.s3, &form.s4, &form.s5, &form.s6];
    let sections: Vec<&str> = egm_core::doc::SECTIONS.iter().zip(picked).filter(|(_, on)| on.is_some()).map(|(s, _)| *s).collect();
    let args = match form.command.as_str() {
        "say" => json!({"text": form.text}),
        "draft" => json!({"sections": sections}),
        _ => json!({}),
    };
    if let Err(e) = argv(&form.command, slug, &args) {
        return Err(bad_request(e).into());
    }
    let id = queue(cx, p, slug, &form.command, args, &v.id).await?;
    Ok(see_other(format!("/p/{p}/jobs/{id}")))
}

/// What a job's state reads as.
pub fn label(j: &db::JobRow) -> String {
    match (j.state.as_str(), j.status.as_str()) {
        ("queued", _) => "queued".into(),
        ("running", _) => "running".into(),
        (_, "ok") => "done".into(),
        (_, "failed") => format!("failed (exit {})", j.exit_code.unwrap_or(-1)),
        (_, s) => s.to_string(),
    }
}

fn label_class(j: &db::JobRow) -> &'static str {
    match (j.state.as_str(), j.status.as_str()) {
        ("done", "ok") => "ok",
        ("done", _) => "bad",
        _ => "muted",
    }
}

/// The words a person gave the job (idea, answer, sections), for lists and the chat.
pub fn summary(j: &db::JobRow) -> String {
    if let Some(t) = j.args["text"].as_str() {
        return t.chars().take(120).collect();
    }
    j.args["sections"].as_array().filter(|a| !a.is_empty()).map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

fn job_id(cx: &Cx) -> Result<i64> {
    job(cx).parse().map_err(|_| not_found().into())
}

#[page("/p/{project}/jobs/{job}")]
pub async fn job_page(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let a = app(cx);
    let j = db::job(&*a.db, job_id(cx)?).await.ah()?.filter(|j| j.project == p).ok_or_else(not_found)?;
    let lines = db::job_log(&*a.db, j.id, 0).await.ah()?;
    let after = lines.last().map(|l| l.0).unwrap_or(0);
    let who = db::user_login(&*a.db, &j.requester).await.ah()?.map(|(l, _)| l).unwrap_or_else(|| j.requester.clone());
    let feature_exists = db::feature(&*a.db, p, &j.feature).await.ah()?.is_some();
    let position = if j.state == "queued" {
        db::open_jobs(&*a.db).await.ah()?.iter().filter(|o| o.project == j.project && o.id < j.id).count()
    } else {
        0
    };
    let lab = label(&j);
    Ok(view! {
        <p><a href=(format!("/p/{p}"))>(p)</a>" / "
            if feature_exists { <a href=(format!("/p/{p}/f/{}", j.feature))>(&j.feature)</a> } else { (&j.feature) }
            " / "<a href=(format!("/p/{p}/jobs"))>"jobs"</a>
        </p>
        <h1>"Job "(j.id)": "(&j.command)" "(&j.feature)</h1>
        <p>
            <span id="job-state" class=(label_class(&j)) data-state=(&j.state)>(&lab)</span>
            " · queued by "(who)" "(crate::app::when(&j.queued_at))
            if j.tokens > 0 { " · "(j.tokens)" tokens" }
        </p>
        if !summary(&j).is_empty() { <p class="muted" id="job-args">(summary(&j))</p> }
        if j.state == "queued" && position > 0 {
            <p class="muted" id="job-position">(format!("{position} job(s) of this project run first; one job runs at a time per project."))</p>
        }
        if !j.detail.is_empty() { <p class="bad" id="job-detail">(&j.detail)</p> }
        if !(lines.is_empty() && j.state == "done") {
            <div id="log" data-job=(j.id.to_string()) data-project=(p) data-after=(after.to_string()) data-state=(&j.state)>
                for (_, speaker, text) in &lines {
                    <div class=(format!("line sp-{speaker}"))><span class="who">(speaker.as_str())</span><span class="text">(text.as_str())</span></div>
                }
            </div>
        }
        if j.state != "done" { <p class="muted">"This page follows the job as it runs."</p> }
        <script src="/static/job.js" defer="defer"></script>
    })
}

#[query_params(error = bad_request)]
struct After {
    after: Option<i32>,
}

/// New lines of a job's output, for the job page's script.
#[route(GET "/p/{project}/jobs/{job}/log")]
pub async fn job_log(cx: &Cx) -> Result<Json<Value>> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let a = app(cx);
    let j = db::job(&*a.db, job_id(cx)?).await.ah()?.filter(|j| j.project == p).ok_or_else(not_found)?;
    let after = query_params::<After>(cx)?.after.unwrap_or(0);
    let lines: Vec<Value> = db::job_log(&*a.db, j.id, after).await.ah()?.into_iter().map(|(seq, speaker, text)| json!({"seq": seq, "speaker": speaker, "text": text})).collect();
    Ok(Json(json!({"state": j.state, "label": label(&j), "lines": lines})))
}

/// Follows a running job: polls for new lines, adds them as text, reloads when it ends.
const JOB_JS: &str = r#"(function () {
  var el = document.getElementById('log');
  if (!el || el.dataset.state === 'done') return;
  var after = Number(el.dataset.after) || 0;
  var url = '/p/' + encodeURIComponent(el.dataset.project) + '/jobs/' + el.dataset.job + '/log';
  function add(l) {
    var row = document.createElement('div');
    row.className = 'line sp-' + l.speaker;
    var who = document.createElement('span'); who.className = 'who'; who.textContent = l.speaker;
    var text = document.createElement('span'); text.className = 'text'; text.textContent = l.text;
    row.appendChild(who); row.appendChild(text); el.appendChild(row);
    after = l.seq;
  }
  function tick() {
    fetch(url + '?after=' + after, { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (d) {
        d.lines.forEach(add);
        var badge = document.getElementById('job-state');
        if (badge) badge.textContent = d.label;
        if (d.state === 'done') { location.reload(); } else { setTimeout(tick, 700); }
      })
      .catch(function () { setTimeout(tick, 2000); });
  }
  tick();
})();
"#;

#[route(GET "/static/job.js")]
pub async fn job_js(cx: &Cx) -> Result<Js<&'static str>> {
    let _ = cx;
    Ok(Js(JOB_JS))
}

#[page("/p/{project}/jobs")]
pub async fn jobs_page(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    let a = app(cx);
    let rows = db::jobs(&*a.db, p, None, 100).await.ah()?;
    let used = db::month_tokens(&*a.db, p).await.ah()?;
    let ceiling = a.cfg.project(p).and_then(|c| c.monthly_tokens);
    let worker = a.cfg.worker.is_some();
    Ok(view! {
        <h1>(p)" · jobs"</h1>
        tabs(project: p)
        <p class="muted">"One job runs at a time in this project, oldest first. "
            (match ceiling { Some(c) => format!("Tokens this month: {used} of {c}; jobs are refused once the ceiling is reached."), None => format!("Tokens this month: {used} (no ceiling).") })
        </p>
        if !worker { <p class="muted" id="no-worker">"No worker runs in this server's process; jobs wait for `egm-team worker`."</p> }
        if rows.is_empty() { <p class="muted" id="jobs-empty">"No jobs yet."</p> } else {
            <table id="jobs">
                <tr><th>"Job"</th><th>"Feature"</th><th>"Command"</th><th>"State"</th><th>"Tokens"</th><th>"Queued"</th></tr>
                for j in &rows {
                    <tr data-job=(j.id.to_string()) data-state=(&j.state)>
                        <td><a href=(format!("/p/{p}/jobs/{}", j.id))>(j.id)</a></td>
                        <td>(&j.feature)</td><td>(&j.command)</td>
                        <td class=(label_class(j))>(label(j))</td><td>(j.tokens)</td>
                        <td class="nowrap">(crate::app::when(&j.queued_at))</td>
                    </tr>
                }
            </table>
        }
    })
}

/// The new-feature form on the board, for people who may start features.
#[component]
pub async fn new_feature_form(cx: &Cx, p: &str) -> Result<impl View> {
    let v = authz::signed_in(cx).await?;
    let roles = authz::roles_of(cx, &v, p).await?;
    let can = allowed(&roles, false, egm_core::evidence::Tier::Low, Action::CreateFeature).is_ok();
    let open: Vec<db::JobRow> = db::jobs(&*app(cx).db, p, None, 20).await.ah()?.into_iter().filter(|j| j.state != "done").collect();
    Ok(view! {
        if can {
            <details id="new-feature" style="margin:12px 0">
                <summary>"New feature"</summary>
                <form method="POST" action=(format!("/p/{p}/new"))>
                    <p><label>"Name (letters, digits, - and _): "<input name="slug" required="required" maxlength="64" pattern="[A-Za-z0-9_-]+"></label></p>
                    <p><label>"The idea, in a sentence or two:"<br><textarea name="idea" rows="3" cols="70" required="required" maxlength="4000"></textarea></label></p>
                    <button>"Start: the elephant reads the idea"</button>
                </form>
            </details>
        }
        if !open.is_empty() {
            <p class="muted" id="open-jobs">"Running or queued: "
                for j in &open { <a href=(format!("/p/{p}/jobs/{}", j.id))>(format!("{} {} ({})", j.command, j.feature, j.state))</a>" " }
            </p>
        }
    })
}

/// The elephant chat, the run buttons, the feature's jobs and its cost, for the feature page.
#[component]
pub async fn run_panel(cx: &Cx, p: &str, slug: &str) -> Result<impl View> {
    let a = app(cx);
    let v = authz::signed_in(cx).await?;
    let st = db::feature_state(&*a.db, p, slug).await.ah()?.ok_or_else(not_found)?;
    let roles = authz::roles_of(cx, &v, p).await?;
    let can_run = allowed(&roles, st.author == v.id, authz::tier_of(&st), action_for("say")).is_ok();
    let jobs = db::jobs(&*a.db, p, Some(slug), 200).await.ah()?;
    let mut chat = vec![];
    for j in jobs.iter().rev().filter(|j| j.command == "elephant" || j.command == "say") {
        let who = db::user_login(&*a.db, &j.requester).await.ah()?.map(|(l, _)| l).unwrap_or_else(|| j.requester.clone());
        let reply: Vec<String> = db::job_log(&*a.db, j.id, 0).await.ah()?.into_iter().filter(|(_, s, _)| s == "block").map(|(_, _, t)| t).collect();
        chat.push((j.id, who, summary(j), reply.join("\n\n"), j.state.clone()));
    }
    let usage = db::feature_usage(&*a.db, p, slug).await.ah()?;
    let total_tokens: i64 = usage.iter().map(|u| u.3).sum();
    let total_cost: f64 = usage.iter().filter_map(|u| u.4).sum();
    let estimated = usage.iter().any(|u| u.5);
    let recent: Vec<db::JobRow> = jobs.iter().take(10).cloned().collect();
    let buttons: Vec<(&str, &str)> = COMMANDS.iter().filter(|(c, _)| ["goldfish", "build", "review", "gate"].contains(c)).copied().collect();
    Ok(view! {
        <h2 id="chat">"Design chat"</h2>
        if chat.is_empty() { <p class="muted">"This feature was not started from the app, so there is no chat here."</p> } else {
            <div id="chat-log">
                for (id, who, said, reply, state) in &chat {
                    <div class="said" style="background:#eef;padding:8px;margin:6px 0;border-radius:8px"><span class="muted">(who)": "</span>(said)</div>
                    if state.as_str() == "done" {
                        <div class="reply" style="background:#fff;border:1px solid #ddd;padding:8px;margin:6px 0 12px 24px;border-radius:8px;white-space:pre-wrap"><span class="muted">"elephant: "</span>(reply)</div>
                    } else {
                        <div class="muted" style="margin:6px 0 12px 24px"><a href=(format!("/p/{p}/jobs/{id}"))>"the elephant is thinking…"</a></div>
                    }
                }
            </div>
        }
        if can_run {
            <form method="POST" action=(format!("/p/{p}/f/{slug}/run")) id="say-form">
                <input type="hidden" name="command" value="say">
                <textarea name="text" rows="3" cols="70" required="required" maxlength="4000" placeholder="Answer the elephant"></textarea>
                <br>
                <button>"Send to the elephant"</button>
            </form>

            <h2 id="run">"Run"</h2>
            <form method="POST" action=(format!("/p/{p}/f/{slug}/run")) id="draft-form">
                <input type="hidden" name="command" value="draft">
                <span class="muted">"Redraft only: "</span>
                for (i, s) in egm_core::doc::SECTIONS.iter().enumerate() {
                    <label style="margin-right:8px"><input type="checkbox" name=(format!("s{i}")) value="on">(" ".to_string() + s)</label>
                }
                <br>
                <button>"Draft the design doc"</button>
                <span class="muted">" (nothing ticked: the whole doc)"</span>
            </form>
            <div id="run-buttons" style="margin-top:8px">
                for (c, what) in &buttons {
                    <form method="POST" action=(format!("/p/{p}/f/{slug}/run")) style="display:inline-block;margin:0 8px 8px 0">
                        <input type="hidden" name="command" value=(*c)>
                        <button title=(*what) data-command=(*c)>(c[..1].to_uppercase() + &c[1..])</button>
                    </form>
                }
            </div>
            <p class="muted">"egm-dsh decides whether a step may run: a build before a goldfish passes the doc fails in its job, as on the command line."</p>
        }

        <h2 id="jobs">"Jobs"</h2>
        if recent.is_empty() { <p class="muted">"No jobs for this feature yet."</p> } else {
            <table id="feature-jobs">
                <tr><th>"Job"</th><th>"Command"</th><th>"State"</th><th>"Tokens"</th><th>"Queued"</th></tr>
                for j in &recent {
                    <tr data-command=(&j.command)>
                        <td><a href=(format!("/p/{p}/jobs/{}", j.id))>(j.id)</a></td><td>(&j.command)</td>
                        <td class=(label_class(j))>(label(j))</td><td>(j.tokens)</td><td class="nowrap">(crate::app::when(&j.queued_at))</td>
                    </tr>
                }
            </table>
        }

        <h2 id="cost">"Cost"</h2>
        if usage.is_empty() { <p class="muted">"No model calls recorded for this feature."</p> } else {
            <table id="cost-table">
                <tr><th>"Role"</th><th>"Model"</th><th>"Calls"</th><th>"Tokens"</th><th>"Cost"</th></tr>
                for (role, model, calls, tokens, cost, _) in &usage {
                    <tr><td>(role)</td><td>(model)</td><td>(*calls)</td><td>(*tokens)</td><td>(cost.map(|c| format!("${c:.4}")).unwrap_or_else(|| "no price".into()))</td></tr>
                }
                <tr><td><strong>"Total"</strong></td><td></td><td></td><td id="cost-tokens"><strong>(total_tokens)</strong></td><td id="cost-usd"><strong>(format!("${total_cost:.4}"))</strong></td></tr>
            </table>
            if estimated { <p class="muted" id="cost-estimated">"Some counts are estimated from text length (the offline mock agent, or a provider that reports no usage)."</p> }
        }
    })
}
