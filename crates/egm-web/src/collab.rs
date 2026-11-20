//! Collaboration (phase 3): label alarms, comment, review, approve the merge, and the reviewer
//! inbox. Every action is an event written to Vesdb first (`authz::record_now`); the views
//! follow from the event. Every route starts with `authz::require_feature`, which applies the
//! policy with the feature's author and risk tier; `tests/policy_lint.rs` checks that.

use crate::authz::{self, app};
use crate::{checkout, db, Ah};
use egm_audit::AuditEvent;
use egm_core::policy::{allowed, may_approve_merge, Action, MergeDenial};
use serde::Deserialize;
use serde_json::{json, Value};
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::{bad_request, forbidden, not_found, see_other, SeeOther};
use topcoat::router::{page, path_param, route};
use topcoat::view::{component, view, View};
use topcoat::Result;

path_param!(project);
path_param!(feature);

const MAX_TEXT: usize = 4000;

fn back(p: &str, slug: &str, anchor: &str) -> SeeOther {
    see_other(format!("/p/{p}/f/{slug}#{anchor}"))
}

fn event(kind: &str, p: &str, slug: &str, actor: &str, data: Value) -> AuditEvent {
    AuditEvent { kind: kind.into(), project: p.into(), feature: slug.into(), actor: actor.into(), data }
}

/// The alarms of every goldfish round a reviewer can label: `(round, id, text, kind, kept)`.
/// Notes are left out: they are never counted against the doc, so they predict nothing.
pub fn alarms(reviews: &[(u32, Value)]) -> Vec<(u32, i64, String, String, bool)> {
    let mut out = vec![];
    for (n, r) in reviews {
        for a in r["alarms"].as_array().into_iter().flatten() {
            let kind = a["kind"].as_str().unwrap_or("").to_string();
            if kind == "note" {
                continue;
            }
            let kept = a["status"].as_str().is_some_and(|s| s != "dropped");
            out.push((*n, a["id"].as_i64().unwrap_or(0), a["text"].as_str().unwrap_or("").to_string(), kind, kept));
        }
    }
    out
}

/// How a comment target reads: "the doc", "section Problem", "alarm 2 (round 1)".
pub fn target_label(t: &str) -> String {
    if let Some(s) = t.strip_prefix("section:") {
        format!("section {s}")
    } else if let Some((n, id)) = t.strip_prefix("alarm:").and_then(|r| r.split_once(':')) {
        format!("alarm {id} (round {n})")
    } else {
        "the doc".into()
    }
}

/// The doc's `## ` headings, for comment targets.
pub fn sections(doc: &str) -> Vec<String> {
    doc.lines().filter_map(|l| l.strip_prefix("## ").map(|h| h.trim().to_string())).filter(|h| !h.is_empty()).collect()
}

// ---------------------------------------------------------------------------------- routes

#[derive(Deserialize)]
struct LabelForm {
    round: u32,
    alarm: i64,
    label: String,
}

#[route(POST "/p/{project}/f/{feature}/label")]
pub async fn label(cx: &Cx, Form(form): Form<LabelForm>) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, st) = authz::require_feature(cx, p, slug, Action::LabelAlarm).await?;
    if form.label != "valid" && form.label != "noise" {
        return Err(bad_request("label must be valid or noise").into());
    }
    let co = app(cx).cfg.project(p).map(|c| c.checkout.clone()).ok_or_else(not_found)?;
    let found = alarms(&checkout::reviews(&co, slug)).into_iter().find(|(n, id, ..)| *n == form.round && *id == form.alarm);
    let Some((_, _, _, _, kept)) = found else { return Err(not_found().into()) };
    let data = json!({"round": form.round, "alarm": form.alarm, "label": form.label, "kept": kept, "doc_hash": st.doc_hash});
    authz::record_now(cx, event("alarm.labelled", p, slug, &v.id, data)).await?;
    db::touch(&*app(cx).db, &v.id, p, slug).await.ah()?;
    Ok(back(p, slug, "alarms"))
}

#[derive(Deserialize)]
struct CommentForm {
    target: String,
    text: String,
}

fn valid_target(t: &str) -> bool {
    t == "doc"
        || t.strip_prefix("section:").is_some_and(|s| !s.is_empty() && s.len() <= 120 && !s.contains('\n'))
        || t.strip_prefix("alarm:").is_some_and(|s| s.split_once(':').is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<i64>().is_ok()))
}

#[route(POST "/p/{project}/f/{feature}/comment")]
pub async fn comment(cx: &Cx, Form(form): Form<CommentForm>) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, st) = authz::require_feature(cx, p, slug, Action::Comment).await?;
    let text = form.text.trim();
    if text.is_empty() || text.len() > MAX_TEXT || !valid_target(&form.target) {
        return Err(bad_request(format!("a comment needs a target and 1 to {MAX_TEXT} characters")).into());
    }
    let data = json!({"target": form.target, "doc_hash": st.doc_hash});
    authz::record_with_words(cx, event("comment.added", p, slug, &v.id, data), "text", text).await?;
    db::touch(&*app(cx).db, &v.id, p, slug).await.ah()?;
    Ok(back(p, slug, "comments"))
}

#[derive(Deserialize)]
struct ReviewForm {
    verdict: String,
    note: String,
}

#[route(POST "/p/{project}/f/{feature}/review")]
pub async fn review(cx: &Cx, Form(form): Form<ReviewForm>) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, st) = authz::require_feature(cx, p, slug, Action::SubmitReview).await?;
    if form.verdict != "approve" && form.verdict != "changes" {
        return Err(bad_request("verdict must be approve or changes").into());
    }
    if form.note.len() > MAX_TEXT || (form.verdict == "changes" && form.note.trim().is_empty()) {
        return Err(bad_request("asking for changes needs a note saying which").into());
    }
    if !st.merge.built {
        return Err(bad_request("the feature is not built yet; review the design doc with a comment instead").into());
    }
    let data = json!({"verdict": form.verdict, "doc_hash": st.doc_hash});
    authz::record_with_words(cx, event("review.submitted", p, slug, &v.id, data), "note", form.note.trim()).await?;
    db::touch(&*app(cx).db, &v.id, p, slug).await.ah()?;
    Ok(back(p, slug, "review"))
}

#[route(POST "/p/{project}/f/{feature}/approve")]
pub async fn approve(cx: &Cx) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, st) = authz::require_feature(cx, p, slug, Action::ApproveMerge).await?;
    let roles = authz::roles_of(cx, &v, p).await?;
    let tier = authz::tier_of(&st);
    match may_approve_merge(&roles, st.author == v.id, tier, &st.merge) {
        Ok(()) => {}
        Err(MergeDenial::Policy(d)) => {
            authz::refuse(app(cx).log.clone(), p, &v.id, Action::ApproveMerge, &d.to_string()).await;
            return Err(forbidden().into());
        }
        Err(e) => {
            authz::refuse(app(cx).log.clone(), p, &v.id, Action::ApproveMerge, &e.to_string()).await;
            return Err(bad_request(format!("cannot approve yet: {e}")).into());
        }
    }
    if st.approved {
        return Ok(back(p, slug, "merge"));
    }
    let data = json!({"doc_hash": st.doc_hash, "tier": st.tier.clone().unwrap_or_else(|| "unknown".into())});
    authz::record_now(cx, event("merge.approved", p, slug, &v.id, data)).await?;
    db::touch(&*app(cx).db, &v.id, p, slug).await.ah()?;
    Ok(back(p, slug, "merge"))
}

#[page("/inbox")]
pub async fn inbox(cx: &Cx) -> Result<impl View> {
    let v = authz::signed_in(cx).await?;
    let mut projects = vec![];
    for p in authz::readable_projects(cx, &v).await? {
        let roles = authz::roles_of(cx, &v, &p).await?;
        if allowed(&roles, false, egm_core::evidence::Tier::Low, Action::SubmitReview).is_ok() {
            projects.push(p);
        }
    }
    let rows = db::inbox(&*app(cx).db, &projects, &v.id).await.ah()?;
    Ok(view! {
        <h1>"Review inbox"</h1>
        <p class="muted">"Built features whose evidence gate passed for the current doc and that nobody has approved yet, highest risk first. Features you wrote are not listed: you cannot review them."</p>
        if rows.is_empty() {
            <p id="inbox-empty">"Nothing is waiting for your review."</p>
        } else {
            <table id="inbox">
                <tr><th>"Risk"</th><th>"Feature"</th><th>"Project"</th><th>"Last change"</th></tr>
                for (p, slug, tier, at) in &rows {
                    <tr data-feature=(slug)>
                        <td><span class=(format!("phase tier-{tier}"))>(tier)</span></td>
                        <td><a href=(format!("/p/{p}/f/{slug}#review"))>(slug)</a></td>
                        <td>(p)</td>
                        <td class="nowrap">(crate::app::when(at))</td>
                    </tr>
                }
            </table>
        }
    })
}

// ------------------------------------------------------------------- feature page sections

/// Alarms with labels, comments, reviews and the merge panel, for the feature page.
#[component]
pub async fn collab_panel(cx: &Cx, p: &str, slug: &str) -> Result<impl View> {
    let a = app(cx);
    let v = authz::signed_in(cx).await?;
    db::touch(&*a.db, &v.id, p, slug).await.ah()?;
    let st = db::feature_state(&*a.db, p, slug).await.ah()?.ok_or_else(not_found)?;
    let roles = authz::roles_of(cx, &v, p).await?;
    let is_author = st.author == v.id;
    let tier = authz::tier_of(&st);
    let can_label = allowed(&roles, is_author, tier, Action::LabelAlarm).is_ok();
    let can_comment = allowed(&roles, is_author, tier, Action::Comment).is_ok();
    let can_review = allowed(&roles, is_author, tier, Action::SubmitReview).is_ok() && st.merge.built;
    let merge = may_approve_merge(&roles, is_author, tier, &st.merge);
    let co = a.cfg.project(p).map(|c| c.checkout.clone()).ok_or_else(not_found)?;
    let alarm_rows = alarms(&checkout::reviews(&co, slug));
    let labels = db::labels(&*a.db, p, slug).await.ah()?;
    let comments = db::comments(&*a.db, p, slug).await.ah()?;
    let reviews = db::reviews(&*a.db, p, slug).await.ah()?;
    let approvals = db::approvals(&*a.db, p, slug).await.ah()?;
    let doc = db::feature(&*a.db, p, slug).await.ah()?.and_then(|f| checkout::read_doc(&co, &f.doc)).unwrap_or_default();
    let targets = sections(&doc);
    let author_label = db::user_login(&*a.db, &st.author).await.ah()?.map(|(l, _)| l).unwrap_or_else(|| st.author.clone());
    let tier_text = st.tier.clone().unwrap_or_else(|| "not assessed (treated as high)".into());
    let checks = [
        ("author recorded", st.merge.author_known),
        ("built", st.merge.built),
        ("evidence gate passed for this doc", st.merge.gate_passed),
        ("approved in a review by someone else", st.merge.reviewed_by_other),
    ];
    let merge_reason = merge.err().map(|e| e.to_string());
    let action_label = |l: &str| if l == "valid" { "Real" } else { "Noise" };
    Ok(view! {
        <p class="muted" id="author">"Author: "(author_label)" · risk: "(tier_text)</p>

        <h2 id="alarms">"Goldfish alarms"</h2>
        if alarm_rows.is_empty() { <p class="muted">"No alarms to label."</p> } else {
            <p class="muted">"Label each alarm: was it a real gap in the doc, or noise? Your labels score the goldfish (Metrics)."</p>
            <table id="alarm-table">
                <tr><th>"Round"</th><th>"Alarm"</th><th>"Goldfish"</th><th>"Label"</th></tr>
                for (n, id, text, kind, kept) in &alarm_rows {
                    <tr data-alarm=(format!("{n}:{id}"))>
                        <td>(*n)</td>
                        <td>(text)<div class="muted">(kind)</div></td>
                        <td class=(if *kept { "bad" } else { "muted" })>(if *kept { "kept" } else { "dropped" })</td>
                        <td>
                            if let Some((l, by)) = labels.get(&(*n as i32, *id as i32)) {
                                <span class="label-value" data-label=(l)>(action_label(l))</span><span class="muted">" · "(by)</span>
                            }
                            if can_label {
                                <form method="POST" action=(format!("/p/{p}/f/{slug}/label")) style="display:inline;margin-left:8px">
                                    <input type="hidden" name="round" value=(n.to_string())>
                                    <input type="hidden" name="alarm" value=(id.to_string())>
                                    <button name="label" value="valid">"Real"</button>
                                    <button name="label" value="noise">"Noise"</button>
                                </form>
                            }
                        </td>
                    </tr>
                }
            </table>
        }

        <h2 id="comments">"Comments"</h2>
        if comments.is_empty() { <p class="muted">"No comments yet."</p> } else {
            <div id="comment-list">
                for (target, who, body, at) in &comments {
                    <div class="comment" style="background:#fff;border:1px solid #ddd;padding:8px;margin-bottom:8px">
                        <div class="muted">(who)" on "(target_label(target))" · "(crate::app::when(at))</div>
                        <div style="white-space:pre-wrap">(body)</div>
                    </div>
                }
            </div>
        }
        if can_comment {
            <form method="POST" action=(format!("/p/{p}/f/{slug}/comment")) id="comment-form">
                <select name="target">
                    <option value="doc">"The whole doc"</option>
                    for s in &targets { <option value=(format!("section:{s}"))>("Section: ".to_string() + s)</option> }
                    for (n, id, ..) in &alarm_rows { <option value=(format!("alarm:{n}:{id}"))>(format!("Alarm {id} (round {n})"))</option> }
                </select>
                <br>
                <textarea name="text" rows="3" cols="70" required="required" maxlength="4000"></textarea>
                <br>
                <button>"Comment"</button>
            </form>
        }

        <h2 id="review">"Reviews"</h2>
        if reviews.is_empty() { <p class="muted">"No reviews yet."</p> } else {
            <table id="review-table">
                <tr><th>"Reviewer"</th><th>"Verdict"</th><th>"Note"</th><th>"When"</th></tr>
                for (who, verdict, note, hash, at) in &reviews {
                    <tr>
                        <td>(who)</td>
                        <td class=(if verdict == "approve" { "ok" } else { "bad" })>(if verdict == "approve" { "approved" } else { "changes requested" })
                            if hash != &st.doc_hash { <div class="muted">"(an earlier revision)"</div> }
                        </td>
                        <td style="white-space:pre-wrap">(note)</td>
                        <td class="nowrap">(crate::app::when(at))</td>
                    </tr>
                }
            </table>
        }
        if can_review {
            <form method="POST" action=(format!("/p/{p}/f/{slug}/review")) id="review-form">
                <textarea name="note" rows="3" cols="70" maxlength="4000" placeholder="What you checked, or what must change"></textarea>
                <br>
                <button name="verdict" value="approve">"Approve"</button>
                <button name="verdict" value="changes">"Request changes"</button>
            </form>
        } else if is_author {
            <p class="muted" id="own-feature">"You wrote this feature, so you cannot review or approve it."</p>
        }

        <h2 id="merge">"Merge approval"</h2>
        <table id="merge-checks">
            for (name, ok) in checks {
                <tr><td>(name)</td><td class=(if ok { "ok" } else { "bad" })>(if ok { "✔" } else { "✖" })</td></tr>
            }
        </table>
        if !approvals.is_empty() {
            <ul id="approvals">
                for (who, hash, t, at) in &approvals {
                    <li>(who)" approved the merge · risk "(t)" · "(crate::app::when(at))
                        if hash != &st.doc_hash { <span class="muted">" (an earlier revision)"</span> }
                    </li>
                }
            </ul>
        }
        if st.approved {
            <p class="ok" id="merge-approved">"Approved for merge. Merging on GitHub or GitLab comes with phase 5."</p>
        } else {
            match &merge_reason {
                None => <form method="POST" action=(format!("/p/{p}/f/{slug}/approve"))><button id="approve-merge">"Approve merge"</button></form>,
                Some(why) => <p class="muted" id="merge-blocked">"You cannot approve the merge: "(why)"."</p>,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_sections() {
        assert!(valid_target("doc") && valid_target("section:Problem") && valid_target("alarm:2:7"));
        assert!(!valid_target("") && !valid_target("section:") && !valid_target("alarm:x:1") && !valid_target("file:/etc/passwd"));
        assert_eq!(target_label("section:Problem"), "section Problem");
        assert_eq!(target_label("alarm:1:7"), "alarm 7 (round 1)");
        assert_eq!(target_label("doc"), "the doc");
        assert_eq!(sections("# T\n## Problem\ntext\n## Goals and non-goals\n"), ["Problem", "Goals and non-goals"]);
    }

    #[test]
    fn notes_are_not_labelled_and_dropped_alarms_are_not_kept() {
        let r = json!({"alarms": [
            {"id": 1, "kind": "blocking", "status": "confirmed", "text": "a"},
            {"id": 2, "kind": "question", "status": "dropped", "text": "b"},
            {"id": 3, "kind": "note", "status": "open", "text": "c"},
            {"id": 4, "kind": "question", "status": "open", "text": "d"}
        ]});
        let a = alarms(&[(1, r)]);
        assert_eq!(a.iter().map(|x| (x.1, x.4)).collect::<Vec<_>>(), [(1, true), (2, false), (4, true)]);
    }
}
