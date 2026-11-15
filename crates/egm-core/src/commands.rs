//! One function per `egm` subcommand. The CLI owns files, state and the gate;
//! agents only answer turns.


use anyhow::{bail, Context};
use serde_json::json;

use crate::agent::{Access, Agent, Role, RoleName, Turn, Usage};
use crate::cost;
use crate::estimate;
use crate::doc::{self, Doc, SECTIONS};
use crate::goldfish::{self, Verdict};
use crate::metrics::{Confusion, MIN_LABELS};
use crate::phase::{Event, Gate, Phase};
use crate::project::{now_secs, Feature, GoldfishMode, Project};
use crate::ui;

fn after_note(after: &[String]) -> String {
    if after.is_empty() { String::new() } else { format!(", after {}", after.join(", ")) }
}

/// Tell the elephant which earlier steps this one builds on.
fn chain_context(p: &Project, after: &[String]) -> String {
    let docs: Vec<String> = after.iter().filter_map(|d| p.load_feature(d).ok()).map(|f| format!("`{}`", f.doc)).collect();
    if docs.is_empty() {
        return String::new();
    }
    format!(
        "\n\nThis feature is one small step in a chain. It comes after: {}. Read those design docs first. Design only what this step adds, and link those docs from the Technical plan with relative Markdown links instead of repeating them.",
        docs.join(", ")
    )
}

pub fn elephant_start_prompt(idea: &str) -> String {
    format!(
        "You are the elephant in an Elephant-Goldfish design session for this repository. Your job is to help design one feature before any code exists.\n\nRules for this whole conversation:\n1. Do not create, edit or delete files, and do not write code. You are in read-only mode; we are only designing.\n2. First read the parts of the repository this idea touches and summarize, briefly, how they work today.\n3. Then interrogate me. Ask the two or three questions that most expose the edges of this idea, and challenge assumptions you doubt. Agreeing with me is not helpful; finding gaps is.\n4. Keep each reply short.\n\nIdea: {idea}"
    )
}

pub fn elephant_say_prompt(msg: &str) -> String {
    format!("{msg}\n\n(Keep interrogating: ask the next most important question, or challenge an answer you doubt. If you have no questions left, say so. Do not write code or files.)")
}

fn title_of(slug: &str) -> String {
    slug.split('-').map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ")
}

fn check_slug(slug: &str) -> anyhow::Result<()> {
    if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("feature names use lowercase letters, digits and dashes, like `jot-tags`");
    }
    Ok(())
}

/// `egm-dsh adopt <slug> --doc path` registers a doc that already exists.
/// The doc is not rewritten; sections egm-dsh expects but the doc lacks become
/// questions at the first goldfish review, like any other gap.
pub fn adopt(p: &Project, slug: &str, doc_path: &std::path::Path, after: &[String]) -> anyhow::Result<()> {
    check_slug(slug)?;
    if p.load_feature(slug).is_ok() {
        bail!("feature `{slug}` already exists");
    }
    crate::steps::check_deps(&p.features()?, slug, after)?;
    let abs = if doc_path.is_absolute() { doc_path.to_path_buf() } else { std::env::current_dir()?.join(doc_path) };
    let abs = abs.canonicalize().with_context(|| format!("cannot find {}", doc_path.display()))?;
    let root = p.root.canonicalize()?;
    let rel = abs.strip_prefix(&root).map_err(|_| anyhow::anyhow!("{} is outside the repository {}", abs.display(), root.display()))?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    let d = Doc::load(&abs)?;
    let f = Feature {
        slug: slug.into(),
        title: title_of(slug),
        idea: format!("(adopted from {rel})"),
        doc: rel.clone(),
        elephant_session: None,
        gate: Gate::new(d.hash()),
        goldfish_runs: 0,
        first_verdict: None,
        first_verdict_model: None,
        label: None,
        built_files: Default::default(),
        built_doc_hash: None,
        depends_on: after.to_vec(),
        built_seq: None,
        phases_done: 0,
        phase_doc_hash: None,
        spec: None,
        git_branch: None,
                lane: None,
    };
    p.save_feature(&f)?;
    ui::egm(&format!("adopted {rel} as `{slug}` (phase: designing){}", after_note(after)));
    let missing = d.missing_sections();
    if !missing.is_empty() {
        ui::warn(&format!("the doc has no usable {} section(s); the goldfish will ask for them. Add them by hand, or start an elephant session with `egm-dsh elephant {slug} --say ...` and redraft with `egm-dsh draft {slug} --section ...`", missing.join(", ")));
    }
    if d.implementation_map().is_empty() {
        ui::warn("no Implementation map bullets of the form - `path` — reason; `egm-dsh build` limits changes to the files listed there");
    }
    ui::egm(&format!("next: egm-dsh goldfish {slug}"));
    Ok(())
}

/// `egm-dsh elephant <slug> --idea ...` starts a design session; `--say ...` continues it.
pub fn elephant(p: &Project, slug: &str, idea: Option<&str>, say: Option<&str>, after: &[String]) -> anyhow::Result<()> {
    check_slug(slug)?;
    if !after.is_empty() && say.is_some() {
        bail!("--after goes with --idea; to change an existing step's dependencies use `egm-dsh after {slug} <steps...>`");
    }
    let agent = p.agent_for(RoleName::Elephant);
    let (mut feature, prompt) = match (idea, say) {
        (Some(idea), None) => {
            if p.load_feature(slug).is_ok() {
                bail!("feature `{slug}` already exists; continue it with --say");
            }
            crate::steps::check_deps(&p.features()?, slug, after)?;
            let rel = format!("{}/{slug}.md", p.config.designs_dir);
            let text = doc::template(&title_of(slug), idea);
            let f = Feature {
                slug: slug.into(),
                title: title_of(slug),
                idea: idea.into(),
                doc: rel,
                elephant_session: None,
                gate: Gate::new(doc::hash(&text)),
                goldfish_runs: 0,
                first_verdict: None,
                first_verdict_model: None,
                label: None,
                built_files: Default::default(),
                built_doc_hash: None,
                depends_on: after.to_vec(),
                built_seq: None,
                phases_done: 0,
                phase_doc_hash: None,
                spec: None,
                git_branch: None,
                lane: None,
            };
            ui::egm(&format!("new feature `{slug}`; design doc at {}{}", f.doc, after_note(after)));
            // Chain context goes before the idea, which stays the prompt's last part.
            (f, elephant_start_prompt(idea).replacen("\n\nIdea: ", &format!("{}\n\nIdea: ", chain_context(p, after)), 1))
        }
        (None, Some(msg)) => (p.load_feature(slug)?, elephant_say_prompt(msg)),
        _ => bail!("pass exactly one of --idea (start) or --say (continue)"),
    };
    // One elephant turn: refused before anything is written when the budget is used up.
    let act = p.begin(slug, feature.gate, crate::run::Kind::Elephant)?;
    let path = p.root.join(&feature.doc);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, doc::template(&feature.title, &feature.idea))?;
    }
    let view = elephant_view(p, &feature)?;
    ui::egm(&format!("asking {} (read-only)…", agent.name()));
    let reply = agent.run(Turn {
        prompt,
        session: feature.elephant_session.clone(),
        access: Access::ReadOnly,
        role: Role::Elephant,
        doc: Some(p.root.join(&feature.doc)),
        workdir: view_dir(&view),
    })?;
    note_view_exit(&view, &reply.touched);
    if feature.elephant_session.is_none() {
        feature.elephant_session = reply.session_id.clone();
    }
    act.finish(None)?;
    p.save_feature(&feature)?;
    record(p, slug, "elephant", agent.as_ref(), &reply.usage)?;
    ui::elephant("");
    ui::block(&reply.text);
    Ok(())
}

/// The elephant's view of the repository for this feature, when the project asked
/// for one (a scope or a `.egmignore`; scope.rs). `None`: the elephant reads the project.
fn elephant_view(p: &Project, feature: &Feature) -> anyhow::Result<Option<crate::stage::Stage>> {
    let scope = p.config.scope_for(&feature.slug);
    if !p.config.elephant_view && !crate::scope::wanted(&p.root, &scope) {
        return Ok(None);
    }
    let root = p.root.canonicalize()?;
    let doc_path = root.join(&feature.doc);
    let mut forced = vec![doc_path.clone()];
    if let Ok(d) = Doc::load(&doc_path) {
        forced.extend(d.context_files(&root).into_iter().filter_map(|f| f.canonicalize().ok()));
    }
    for dep in &feature.depends_on {
        if let Ok(f) = p.load_feature(dep) {
            forced.push(root.join(&f.doc));
        }
    }
    let (files, left_out) = crate::scope::view_files(&root, &scope, &forced);
    let dir = std::env::temp_dir().join(format!("egm-dsh-view-{:016x}-{}", doc::hash(&root.to_string_lossy()), feature.slug));
    let stage = crate::stage::Stage::at(dir, &root, &files)?;
    ui::egm(&format!(
        "elephant view: {} file(s){}{}",
        stage.files.len(),
        if scope.is_empty() { String::new() } else { format!(" under {}", scope.join(", ")) },
        if left_out > 0 { format!("; {left_out} left out by .gitignore/.egmignore/size") } else { String::new() }
    ));
    Ok(Some(stage))
}

fn view_dir(v: &Option<crate::stage::Stage>) -> Option<std::path::PathBuf> {
    v.as_ref().map(|s| s.dir.clone())
}

/// The elephant is trusted to read the project, so leaving the view only warns.
fn note_view_exit(v: &Option<crate::stage::Stage>, touched: &[crate::agent::Touch]) {
    if let Some(why) = v.as_ref().and_then(|s| s.escape(touched)) {
        ui::warn(&format!("the elephant read outside its view ({why}); add the path to the scope (`egm-dsh set-scope`) if it needs it"));
    }
}

/// The word limit asked of the elephant per section, if any.
fn with_words(prompt: String, limit: Option<u32>) -> String {
    match limit {
        Some(n) => format!("{prompt}\n\nBe concise: at most {n} words per section, not counting diagrams. Use short bullets, state each fact once, no preamble or summary, and do not repeat what another section says; link instead."),
        None => prompt,
    }
}

/// What the elephant is asked when it drafts: diagrams (by policy), then the word limit.
fn drafting(p: &Project, prompt: String, limit: Option<u32>) -> String {
    let prompt = match crate::diagram::guidance(p.config.diagrams) {
        Some(g) => format!("{prompt}\n\n{g}"),
        None => prompt,
    };
    with_words(prompt, limit)
}

/// Words per section after a draft (diagrams not counted); warn about any over the limit
/// (the text is kept as written).
fn report_words(doc: &Doc, names: &[&str], limit: Option<u32>) {
    let mut over = Vec::new();
    let mut parts = Vec::new();
    for name in names {
        let body = doc.section(name).unwrap_or_default();
        let n = crate::diagram::without_diagrams(&body).split_whitespace().count();
        let d = crate::diagram::blocks(&body).len();
        parts.push(if d > 0 { format!("{name} {n} + {d} diagram{}", if d == 1 { "" } else { "s" }) } else { format!("{name} {n}") });
        if limit.is_some_and(|l| n > l as usize) {
            over.push(format!("{name} ({n})"));
        }
    }
    ui::egm(&format!("words per section: {}", parts.join(", ")));
    if let Some(l) = limit.filter(|_| !over.is_empty()) {
        ui::warn(&format!("over the {l}-word limit: {}; kept as written (a goldfish will say if it is unclear, not if it is long)", over.join(", ")));
    }
}

/// When the project has a Verification checklist and the turn drafts that section, say
/// exactly what it must define (egm-dsh checks it afterwards).
fn verification_lead(p: &Project, lead: &str, sections: &[&str]) -> String {
    if sections.contains(&"Verification") && !p.config.verification.is_empty() {
        format!("{lead}\n\nThis project's `## Verification` section must define:\n{}", crate::verify::guidance(&p.config.verification))
    } else {
        lead.to_string()
    }
}

/// `egm-dsh draft <slug> [--section NAME] [--per-section]`: the elephant writes the doc.
/// The whole doc comes in one turn; any section the reply lacks is drafted on its own.
pub fn draft(p: &Project, slug: &str, only: &[String], per_section: bool, max_words: Option<u32>) -> anyhow::Result<()> {
    let words = max_words.or(p.config.draft_words);
    let mut feature = p.load_feature(slug)?;
    let Some(session) = feature.elephant_session.clone() else { bail!("no elephant session for `{slug}`") };
    let sections: Vec<&'static str> = if only.is_empty() {
        let mut all = SECTIONS.to_vec();
        if !p.config.verification.is_empty() {
            all.push("Verification");
        }
        all
    } else {
        only.iter()
            .map(|name| {
                SECTIONS.iter().chain(doc::OPTIONAL_SECTIONS.iter()).copied().find(|s| s.eq_ignore_ascii_case(name)).with_context(|| {
                    format!("unknown section `{name}`; one of: {}, {}", SECTIONS.join(", "), doc::OPTIONAL_SECTIONS.join(", "))
                })
            })
            .collect::<anyhow::Result<_>>()?
    };
    let agent = p.agent_for(RoleName::Elephant);
    let act = p.begin(slug, p.sync(&mut feature).map(|_| feature.gate)?, crate::run::Kind::Elephant)?;
    let view = elephant_view(p, &feature)?;
    let all_names = sections.clone();
    let mut sections = sections;
    if sections.len() > 1 && !per_section {
        ui::egm(&format!("elephant drafts all {} sections in one turn…", sections.len()));
        let reply = agent.run(Turn {
            prompt: drafting(p, doc::draft_all_prompt(&sections, &verification_lead(p, "Draft the whole design doc for this feature, based on everything we discussed.", &sections)), words),
            session: Some(session.clone()),
            access: Access::ReadOnly,
            role: Role::DraftAll,
            doc: Some(p.root.join(&feature.doc)),
            workdir: view_dir(&view),
        })?;
        note_view_exit(&view, &reply.touched);
        record(p, slug, "elephant", agent.as_ref(), &reply.usage)?;
        let got = doc::extract_sections(&reply.text, &sections);
        let current = p.doc(&feature)?;
        let mut text = current.text.clone();
        for (name, sec) in sections.iter().zip(&got) {
            if let Some(sec) = sec {
                text = Doc { path: current.path.clone(), text }.with_section(name, sec);
            }
        }
        std::fs::write(&current.path, text)?;
        sections = sections.into_iter().zip(got).filter(|(_, g)| g.is_none()).map(|(n, _)| n).collect();
        if !sections.is_empty() {
            ui::warn(&format!("the one-turn draft left out {}; drafting those one at a time", sections.join(", ")));
        }
    }
    for name in sections {
        ui::egm(&format!("elephant drafts `## {name}`…"));
        let reply = agent.run(Turn {
            prompt: drafting(p, verification_lead(
                p,
                &format!(
                    "Draft the `## {name}` section of the design doc for this feature, based on everything we discussed. {}\n\nReply with only that section in Markdown, starting with the line `## {name}`. Do not create or edit files; I will save it.",
                    doc::guidance(name)
                ),
                &[name],
            ), words),
            session: Some(session.clone()),
            access: Access::ReadOnly,
            role: Role::Draft(name),
            doc: Some(p.root.join(&feature.doc)),
            workdir: view_dir(&view),
        })?;
        note_view_exit(&view, &reply.touched);
        record(p, slug, "elephant", agent.as_ref(), &reply.usage)?;
        let current = p.doc(&feature)?;
        let updated = current.with_section(name, &doc::extract_section(&reply.text, name));
        std::fs::write(&current.path, updated)?;
    }
    let doc = p.doc(&feature)?;
    feature.gate = act.finish(Some(Event::EditDoc(doc.hash())))?;
    p.save_feature(&feature)?;
    ui::ok(&format!("saved {} ({} lines, {} words)", feature.doc, doc.text.lines().count(), doc.text.split_whitespace().count()));
    report_words(&doc, &all_names, words);
    let missing = doc.missing_sections();
    if !missing.is_empty() {
        ui::warn(&format!("still missing: {}", missing.join(", ")));
    }
    Ok(())
}

/// `egm-dsh goldfish <slug>`: memoryless reviewers judge the doc. Returns the verdict.
pub fn goldfish(p: &Project, slug: &str, cross: bool, verify: bool, fail_fast: bool) -> anyhow::Result<Verdict> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    if matches!(feature.gate.phase, Phase::Built | Phase::Hardened) {
        bail!("`{slug}` is already built from this doc; edit the doc to reopen the design");
    }
    // The fix lane: one combined session, fail-fast, no verifier, no Verification checklist.
    let fix_lane = feature.is_fix();
    let (verify, fail_fast) = if fix_lane { (false, true) } else { (verify, fail_fast) };
    let checklist: &[crate::verify::Item] = if fix_lane { &[] } else { &p.config.verification };
    let settings = estimate::Settings {
        parallel: !fix_lane && p.config.goldfish_mode == GoldfishMode::Parallel,
        verify: verify && p.config.verify_alarms,
        cross,
        fail_fast,
        precheck_alarms: goldfish::precheck(&doc, p.config.step_limits, checklist, &p.config.diagram_check(fix_lane)).2.len(),
        votes: p.config.readiness_votes as u64,
        invariants: p.config.invariants_stage,
    };
    ceiling_check(p, slug, &estimate::goldfish_round(&settings), &doc, "this goldfish round")?;
    let act = p.begin(slug, feature.gate, crate::run::Kind::Goldfish)?;
    let reader = p.agent_for(RoleName::Goldfish);
    let critic = p.agent_for(RoleName::GoldfishCritic);
    let zoom = p.zoom_out_agent();
    let cross_agent = if cross {
        Some(p.cross_agent().context("--cross-family needs `cross_family` in .egm-dsh/config.json")?)
    } else {
        None
    };
    let mode = if fix_lane { GoldfishMode::Combined } else { p.config.goldfish_mode };
    let stages = goldfish::STAGES.len() - usize::from(!p.config.invariants_stage);
    let votes = if mode == GoldfishMode::Parallel && !fix_lane { p.config.readiness_votes.max(1) } else { 1 };
    let n = stages + votes - 1;
    let readers = 1 + usize::from(p.config.invariants_stage) + votes;   // comprehension, invariants if on, the readiness readers
    match mode {
        GoldfishMode::Parallel if zoom.is_some() => ui::goldfish(&format!(
            "{n} fresh reviewers read only {} and its links: {readers} readers ({}), the critic ({}) and zoom-out ({}, so it can search the repository)…",
            feature.doc,
            reader.name(),
            critic.name(),
            zoom.as_ref().map(|z| z.name()).unwrap_or_default()
        )),
        GoldfishMode::Parallel if reader.name() == critic.name() => {
            ui::goldfish(&format!("{n} fresh reviewers ({}) read only {} and its links…", reader.name(), feature.doc))
        }
        GoldfishMode::Parallel => ui::goldfish(&format!(
            "{n} fresh reviewers read only {} and its links: {readers} readers ({}), and the critic and zoom-out ({})…",
            feature.doc, reader.name(), critic.name()
        )),
        GoldfishMode::Combined => ui::goldfish(&format!("1 fresh reviewer ({}) answers all {n} stages from {} and its links…", reader.name(), feature.doc)),
    }
    let verifier = (verify && p.config.verify_alarms).then(|| p.agent_for(RoleName::GoldfishVerifier));
    let who = goldfish::Reviewers {
        reader: reader.as_ref(),
        critic: critic.as_ref(),
        zoom: zoom.as_deref(),
        cross: cross_agent.as_deref(),
        verifier: verifier.as_deref(),
        triage: p.config.triage.as_ref(),
        mode,
        run: feature.goldfish_runs + 1,
        limits: p.config.step_limits,
        verification: checklist,
        fail_fast,
        diagrams: p.config.diagram_check(fix_lane),
        votes: if fix_lane { 1 } else { p.config.readiness_votes.max(1) },
        off: if p.config.invariants_stage { vec![] } else { vec![goldfish::Stage::Invariants] },
    };
    if p.builtin_reviewer(RoleName::Goldfish) {
        ui::egm(&reviewers_line(p));
    }
    if fail_fast {
        ui::egm("fail-fast: egm-dsh's own checks first, then readiness alone; the other stages run only if both pass");
    }
    let review = goldfish::review(&who, &doc, &p.root);
    for call in &review.calls {
        p.record_usage(slug, &call.role, &call.agent, &call.model, &call.usage)?;
    }

    if let Some(c) = &review.comprehension {
        ui::goldfish("here is what I understood:");
        ui::block(&format!("problem: {}\nchange:  {}", clip(&c.problem), clip(&c.proposed_change)));
    }
    show_alarms(&review, verifier.as_deref());
    for (who, critic) in [("", &review.critic), ("", &review.cross_critic), ("zoom-out ", &review.zoom_out)] {
        for f in critic.iter().flat_map(|c| &c.findings).filter(|f| !crate::official::is_blocker(&f.severity)) {
            ui::block(&format!("{who}[{}] {} ({})", f.severity, f.issue, f.cites));
        }
    }
    if let Some(inv) = &review.invariants {
        for i in &inv.criteria {
            let exists = i.check.starts_with("manual") || p.root.join(&i.check).exists();
            ui::block(&format!("invariant: {} → {}{}", i.predicate, i.check, if exists { "" } else { " (to be written)" }));
        }
    }

    feature.goldfish_runs += 1;
    let path = p.save_review(slug, feature.goldfish_runs, &review)?;
    p.append_ledger(json!({
        "kind": "goldfish", "slug": slug, "run": feature.goldfish_runs, "doc_hash": format!("{:016x}", doc.hash()),
        "verdict": review.verdict, "questions": review.questions().len(), "blocking": review.blocking().len(),
        "notes": review.notes().len(), "caught_by_checks": review.caught().len(),
        "alarms_raised": review.alarm_counts().0, "dropped_by_grounding": review.alarm_counts().1,
        "dropped_by_verifier": review.alarm_counts().2, "confirmed": review.alarm_counts().3, "verified": review.verified,
        "verifier_model": verifier.as_ref().map(|v| v.model()),
        "triage_model": p.config.triage.as_ref().map(|t| t.model.clone()),
        "triage_kept": review.alarms.iter().filter(|a| a.skip_verify).count(),
        "triage_explored": review.alarms.iter().filter(|a| a.triage.as_ref().is_some_and(|t| t.explored)).count(),
        "reviewers": review.reviewers, "reader_model": reader.model(), "critic_model": critic.model(),
        "mode": mode, "tokens": review.tokens, "calls": review.calls.len(),
        "stopped_at": review.stopped_at, "skipped": review.skipped, "ts": now_secs(),
    }))?;
    if review.verdict != Verdict::Incomplete && feature.first_verdict.is_none() {
        feature.first_verdict = Some(review.verdict.to_string());
        feature.first_verdict_model = Some(reader.model());
    }
    let outcome = (review.verdict != Verdict::Incomplete).then_some(Event::Goldfish(review.verdict == Verdict::Ready));
    feature.gate = act.finish(outcome)?;
    p.save_feature(&feature)?;
    if let Some(at) = &review.stopped_at {
        let how = if at == "precheck" { "egm-dsh's own checks found gaps, so no model was called".to_string() } else { format!("the readiness stage was enough to show it; {} other stage(s) were not run", review.skipped.len()) };
        ui::egm(&format!("fail-fast stop at {at}: {how}"));
    }
    match review.verdict {
        Verdict::Ready => ui::ok(&format!("verdict: READY — `egm-dsh build {slug}` is unlocked (report: {})", rel(p, &path))),
        Verdict::NotReady => ui::warn(&format!("verdict: NOT READY — `egm-dsh escalate {slug}` turns the questions into decisions (or answer them with `egm-dsh elephant {slug} --say` and redraft); then run the goldfish again (report: {})", rel(p, &path))),
        Verdict::Incomplete => ui::warn(&format!("verdict: INCOMPLETE — a reviewer failed ({:?}); rerun (report: {})", review.failed, rel(p, &path))),
    }
    Ok(review.verdict)
}

fn usage_text(p: &Project) -> String {
    std::fs::read_to_string(Project::egm_dir(&p.root).join("usage.jsonl")).unwrap_or_default()
}

/// Refuse a step before it starts when its estimate would cross the token ceiling.
fn ceiling_check(p: &Project, slug: &str, plan: &estimate::Plan, doc: &Doc, what: &str) -> anyhow::Result<()> {
    let Some(ceiling) = p.config.token_ceiling else { return Ok(()) };
    let usage = usage_text(p);
    let spent = estimate::spent(&usage, slug);
    let prof = estimate::profiles(&usage, Some(slug));
    let est: f64 = estimate::tokens(plan, &prof, estimate::doc_tokens(doc, &p.root), elephant_turns(&usage, slug)).iter().map(|r| r.3).sum();
    let est = est.ceil() as u64;
    if !estimate::admit(spent, est, Some(ceiling)) {
        bail!("refused before starting: `{slug}` has used {spent} tokens; {what} is estimated at {est} more, which would cross the ceiling of {ceiling} (`egm-dsh estimate {slug}` shows the breakdown; raise it with `egm-dsh set-ceiling <tokens>`)");
    }
    Ok(())
}

fn elephant_turns(usage: &str, slug: &str) -> usize {
    usage.lines().filter(|l| l.contains(&format!("\"feature\":\"{slug}\"")) && l.contains("\"role\":\"elephant\"")).count()
}

/// `egm-dsh estimate <slug>`: sessions and tokens the next goldfish round (or `--rounds N`
/// of `loop`) would use, before any model is called.
pub fn estimate(p: &Project, slug: &str, rounds: u32, fail_fast: bool, cross: bool, verify: bool) -> anyhow::Result<()> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    let pre = goldfish::precheck(&doc, p.config.step_limits, &p.config.verification, &p.config.diagram_check(feature.is_fix())).2.len();
    let settings = estimate::Settings {
        parallel: p.config.goldfish_mode == GoldfishMode::Parallel,
        verify: verify && p.config.verify_alarms,
        cross,
        fail_fast,
        precheck_alarms: pre,
        votes: p.config.readiness_votes as u64,
        invariants: p.config.invariants_stage,
    };
    let mut plan = estimate::goldfish_round(&settings);
    // later rounds: assume the precheck gaps were fixed by the redraft
    let later = estimate::goldfish_round(&estimate::Settings { precheck_alarms: 0, ..settings });
    for _ in 1..rounds {
        for (role, n) in &later.sessions {
            *plan.sessions.entry(role).or_default() += n;
        }
    }
    if rounds > 1 {
        // each round that does not pass: one escalate and one redraft turn of the elephant
        *plan.sessions.entry("elephant").or_default() += 2 * u64::from(rounds - 1);
    }
    let usage = usage_text(p);
    let prof = estimate::profiles(&usage, Some(slug));
    let dt = estimate::doc_tokens(&doc, &p.root);
    let rows = estimate::tokens(&plan, &prof, dt, elephant_turns(&usage, slug));
    let models = estimate::models(&usage);
    let prices = &p.config.prices;
    ui::egm(&format!("estimate for `{slug}`: {} ({} round{}), doc and links ≈ {dt:.0} tokens", if rounds > 1 { "loop" } else { "goldfish" }, rounds, if rounds == 1 { "" } else { "s" }));
    let mut lines = vec![format!("{:<18} {:>8} {:>12} {:>10}  {}", "role", "sessions", "tokens/call", "total", "from")];
    let (mut total, mut dollars, mut priced) = (0.0, 0.0, true);
    for (role, n, pc, t) in &rows {
        let from = if pc.samples > 0 { format!("{} recorded call(s){}", pc.samples, if pc.growth > 0.0 { format!(", +{:.0}/turn history", pc.growth) } else { String::new() }) } else { "heuristic (nothing recorded yet; expect up to ±2×)".into() };
        lines.push(format!("{role:<18} {n:>8} {:>12.0} {t:>10.0}  {from}", pc.input + pc.output));
        total += t;
        let configured = match role.as_str() {
            "goldfish" => Some(p.agent_for(RoleName::Goldfish).model()),
            "goldfish-critic" => Some(p.agent_for(RoleName::GoldfishCritic).model()),
            "goldfish-verifier" => Some(p.agent_for(RoleName::GoldfishVerifier).model()),
            "elephant" => Some(p.agent_for(RoleName::Elephant).model()),
            "cross-family" => p.cross_agent().map(|a| a.model()),
            _ => None,
        };
        let model = configured.or_else(|| models.get(role).cloned()).unwrap_or_default();
        match crate::cost::lookup(prices, &model) {
            // priced as all cache misses: an upper bound on the dollar figure
            Some(mp) => dollars += crate::cost::dollars(mp, &Usage { cache_miss: (pc.input * *n as f64) as u64, output: (pc.output * *n as f64) as u64, ..Usage::default() }),
            None => priced = false,
        }
    }
    let sessions: u64 = plan.sessions.values().sum();
    lines.push(format!("{:<18} {sessions:>8} {:>12} {total:>10.0}", "total", ""));
    ui::block(&lines.join("\n"));
    if let Some(n) = &plan.note {
        ui::egm(n);
    }
    if priced && sessions > 0 {
        ui::egm(&format!("≈ ${dollars:.4} at peak price, counting all input as cache misses (an upper bound for these token counts)"));
    }
    let spent = estimate::spent(&usage, slug);
    match p.config.token_ceiling {
        Some(c) => ui::egm(&format!("used so far: {spent} tokens; ceiling {c}: {}", if estimate::admit(spent, total.ceil() as u64, Some(c)) { "fits" } else { "would be refused" })),
        None => ui::egm(&format!("used so far: {spent} tokens; no ceiling (`egm-dsh set-ceiling <tokens>`)")),
    }
    ui::egm("sessions are exact upper bounds for these settings; tokens per session are estimates");
    Ok(())
}

/// Print every alarm with what happened to it, so a dropped alarm is never silent.
fn show_alarms(review: &goldfish::Review, verifier: Option<&dyn Agent>) {
    use crate::alarm::Status;
    let open: Vec<&crate::alarm::Alarm> = review.alarms.iter().filter(|a| a.status == Status::Open && a.counts()).collect();
    let notes = review.notes();
    let confirmed: Vec<&crate::alarm::Alarm> = review.alarms.iter().filter(|a| a.status == Status::Confirmed).collect();
    let dropped: Vec<&crate::alarm::Alarm> = review.alarms.iter().filter(|a| a.status == Status::Dropped).collect();
    let line = |a: &crate::alarm::Alarm| {
        let mark = if a.kind == crate::alarm::Kind::Blocking { "[blocking]" } else { "?" };
        format!("{mark} {}", a.text)
    };
    if !open.is_empty() || !confirmed.is_empty() {
        ui::goldfish("I could not proceed without answers to:");
        ui::block(&confirmed.iter().map(|a| format!("{} (confirmed by the verifier)", line(a))).chain(open.iter().map(|a| line(a))).collect::<Vec<_>>().join("\n"));
    }
    if !notes.is_empty() {
        ui::egm(&format!("{} comprehension note(s), for the elephant; they do not block:", notes.len()));
        ui::block(&notes.iter().map(|a| format!("· {}", a.text)).collect::<Vec<_>>().join("\n"));
    }
    if !dropped.is_empty() {
        ui::egm(&format!("{} false alarm(s) dropped:", dropped.len()));
        ui::block(&dropped.iter().map(|a| format!("✗ {}\n    {} ({})", a.text, a.reason.as_deref().unwrap_or(""), a.dropped_by.as_deref().unwrap_or(""))).collect::<Vec<_>>().join("\n"));
    }
    let kept: Vec<&crate::alarm::Alarm> = review.alarms.iter().filter(|a| a.skip_verify).collect();
    let explored = review.alarms.iter().filter(|a| a.triage.as_ref().is_some_and(|t| t.explored)).count();
    if let Some(t) = review.alarms.iter().find_map(|a| a.triage.as_ref()) {
        ui::egm(&format!(
            "triage ({}): {} alarm(s) kept without verifying, {} sent to the verifier{}",
            t.model,
            kept.len(),
            review.alarms.iter().filter(|a| a.triage.is_some() && !a.skip_verify).count(),
            if explored > 0 { format!(" ({explored} kept by the model but verified anyway, for exploration)") } else { String::new() }
        ));
    }
    if let Some(v) = verifier.filter(|_| review.verified) {
        ui::egm(&format!("alarms checked by {}", v.name()));
    }
}

fn record(p: &Project, slug: &str, role: &str, agent: &dyn Agent, usage: &Usage) -> anyhow::Result<()> {
    p.record_usage(slug, role, &agent.name(), &agent.model(), usage)
}

fn clip(s: &str) -> String {
    let s = s.lines().next().unwrap_or("");
    if s.chars().count() > 140 { format!("{}…", s.chars().take(140).collect::<String>()) } else { s.to_string() }
}

fn rel(p: &Project, path: &std::path::Path) -> String {
    path.strip_prefix(&p.root).unwrap_or(path).display().to_string()
}

/// `egm-dsh build <slug> [--phase k]`: only a goldfish-approved doc may be
/// built, only after the steps it depends on, and phase by phase. The feature
/// becomes `built` after its last phase.
pub fn build(p: &Project, slug: &str, only: Option<u32>) -> anyhow::Result<()> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    p.save_feature(&feature)?;
    if let Err(refusal) = feature.gate.step(Event::Build) {
        bail!("build refused: {refusal}");
    }
    let all = p.features()?;
    let waiting = crate::steps::unbuilt_deps(&feature, &all);
    if !waiting.is_empty() {
        bail!("build refused: `{slug}` comes after {}, which {} not built yet", waiting.iter().map(|w| format!("`{w}`")).collect::<Vec<_>>().join(", "), if waiting.len() == 1 { "is" } else { "are" });
    }
    let frozen = frozen_for(&feature, &doc).cloned();
    let manual_fix = feature.is_fix() && checks_of(&doc).iter().all(|(_, _, k)| matches!(k, crate::spec::Check::Manual));
    if p.config.tests_first && frozen.is_none() && !manual_fix {
        if feature.spec.is_some() {
            bail!("build refused: the doc changed since its tests were frozen; run `egm-dsh tests {slug}` again");
        }
        bail!("build refused: tests come first; write, review and freeze them with `egm-dsh tests {slug}`");
    }
    let agent = p.agent_for(RoleName::Builder);
    if !agent.can_write() {
        bail!("{} cannot write code; set \"agent\": \"dsh\" in .egm-dsh/config.json", agent.name());
    }
    let use_git = prepare_git(p, &mut feature, &doc, &all, frozen.as_ref())?;
    let phases = doc.implementation_phases();
    anyhow::ensure!(!phases.is_empty(), "the Implementation map lists no files");
    let total = phases.len() as u32;
    // Progress belongs to one doc revision; an edit starts the phases again.
    if feature.phase_doc_hash != Some(doc.hash()) {
        feature.phases_done = 0;
        feature.phase_doc_hash = Some(doc.hash());
    }
    let ks: Vec<u32> = match only {
        Some(k) => vec![crate::steps::phase_step(feature.phases_done, total, k).map_err(|e| anyhow::anyhow!("build refused: {e}"))?],
        None => (feature.phases_done + 1..=total).collect(),
    };
    // One build attempt: everything above was a cheap check; from here on, model work.
    let act = p.begin(slug, feature.gate, crate::run::Kind::Build)?;
    for k in ks {
        let end = build_phase(p, &mut feature, &doc, &phases, k, agent.as_ref(), frozen.as_ref(), use_git)?;
        if let PhaseEnd::Promoted(why) = end {
            // Too big for the fix lane: reverted already. The design reopens and the
            // feature leaves the lane, so the next goldfish is the full review (EgmFix).
            feature.gate = act.finish(Some(Event::Goldfish(false)))?;
            feature.lane = None;
            p.save_feature(&feature)?;
            p.append_ledger(json!({"kind": "fix-promoted", "slug": slug, "why": why, "doc_hash": format!("{:016x}", doc.hash()), "ts": now_secs()}))?;
            bail!(
                "not a tiny fix: {why}. The change was reverted and `{slug}` moved to the full lane (designing). \
                 Widen the doc with `egm-dsh elephant {slug} --say ...` and `egm-dsh draft {slug}`, or run `egm-dsh loop {slug}` for the full review"
            );
        }
        if let PhaseEnd::Reopened(alarms, assumptions) = end {
            // The builder is the last goldfish: what it had to decide, the review missed.
            // The build reports `goldfish false`, so the design reopens (EgmExecutor).
            let n = alarms.iter().filter(|a| a.counts()).count();
            feature.gate = act.finish(Some(Event::Goldfish(false)))?;
            feature.goldfish_runs += 1;
            let review = goldfish::Review::from_builder(&doc, agent.name(), alarms, assumptions);
            let path = p.save_review(slug, feature.goldfish_runs, &review)?;
            p.save_feature(&feature)?;
            p.append_ledger(json!({"kind": "builder-reopened", "slug": slug, "run": feature.goldfish_runs, "phase": k,
                "doc_hash": format!("{:016x}", doc.hash()), "blocking_assumptions": n, "ts": now_secs()}))?;
            bail!(
                "build not accepted: the builder had to make {n} decision(s) the design doc leaves open, so the design is reopened ({}). \
                 `egm-dsh escalate {slug}` turns them into repository answers or decisions; then redraft and run a fresh goldfish (report: {})",
                format!("{:?}", feature.gate.phase).to_lowercase(),
                rel(p, &path)
            );
        }
    }
    if feature.phases_done < total {
        act.finish(None)?;
        ui::egm(&format!("next: `egm-dsh build {slug}` (phase {}/{total})", feature.phases_done + 1));
        return Ok(());
    }
    let after = p.fingerprint();
    let map = doc.implementation_map();
    feature.gate = act.finish(Some(Event::Build))?;
    feature.built_files = map.iter().filter_map(|f| after.get(f).map(|h| (f.clone(), *h))).collect();
    feature.built_doc_hash = Some(doc.hash());
    feature.built_seq = Some(all.iter().filter_map(|f| f.built_seq).max().unwrap_or(0) + 1);
    p.save_feature(&feature)?;
    let shared: Vec<String> = all.iter().filter(|f| f.slug != slug && f.built_seq.is_some())
        .flat_map(|f| map.iter().filter(|m| f.built_files.contains_key(*m)).map(move |m| format!("`{m}` from `{}`", f.slug))).collect();
    if !shared.is_empty() {
        ui::egm(&format!("drift checks now follow this build for {}", shared.join(", ")));
    }
    let phase_note = if total > 1 { format!(" in {total} phases") } else { String::new() };
    ui::ok(&format!("built{phase_note}: every change inside the Implementation map, acceptance passed"));
    Ok(())
}

/// Before a build in a git repository: the steps this one depends on must be
/// merged, the feature gets its own branch, the doc and frozen tests are
/// committed, and the files the builder may change must have no uncommitted
/// edits (so a rejected build can be reverted exactly). Returns whether git is used.
fn prepare_git(p: &Project, feature: &mut Feature, doc: &Doc, all: &[Feature], frozen: Option<&crate::spec::FrozenSpec>) -> anyhow::Result<bool> {
    if !p.config.safety.git {
        return Ok(false);
    }
    if !crate::vcs::is_repo(&p.root) {
        ui::warn("not a git repository: a rejected build's changes stay on disk (run `git init` and commit to get branches and automatic revert)");
        return Ok(false);
    }
    if !crate::vcs::has_commits(&p.root) {
        bail!("build refused: the git repository has no commits yet; make an initial commit first");
    }
    let here = crate::vcs::current_branch(&p.root);
    for d in &feature.depends_on {
        if let Some(b) = all.iter().find(|f| &f.slug == d).and_then(|f| f.git_branch.as_deref())
            && !crate::vcs::is_merged(&p.root, b)
        {
            bail!("build refused: `{d}` was built on branch `{b}`, which is not merged into `{here}`; review and merge it first");
        }
    }
    let branch = format!("egm-dsh/{}", feature.slug);
    if crate::vcs::ensure_branch(&p.root, &branch)? {
        ui::egm(&format!("building on new branch `{branch}` (from `{here}`); merging is up to you"));
    } else if here != branch {
        ui::egm(&format!("building on branch `{branch}`"));
    }
    feature.git_branch = Some(branch);
    let mut spec_files = vec![feature.doc.clone()];
    if let Some(s) = frozen {
        spec_files.extend(s.test_files.keys().cloned());
        spec_files.extend(s.lean_files.keys().cloned());
    }
    if let Some(id) = crate::vcs::commit(&p.root, &spec_files, &format!("egm-dsh design {}: {}{}", feature.slug, feature.doc, if frozen.is_some() { " and frozen tests" } else { "" }))? {
        ui::egm(&format!("committed the design{} as {id}", if frozen.is_some() { " and frozen tests" } else { "" }));
    }
    let builder_files: Vec<String> = doc.implementation_map().into_iter().filter(|f| !spec_files.contains(f)).collect();
    let dirty = crate::vcs::dirty(&p.root, &builder_files)?;
    if !dirty.is_empty() {
        bail!("build refused: uncommitted changes in {}; commit or stash them first so a rejected build can be reverted exactly", dirty.join(", "));
    }
    p.save_feature(feature)?;
    Ok(true)
}

/// One builder session for phase `k`, then egm-dsh's own checks.
#[allow(clippy::too_many_arguments)]
fn build_phase(
    p: &Project,
    feature: &mut Feature,
    doc: &Doc,
    phases: &[crate::doc::BuildPhase],
    k: u32,
    agent: &dyn crate::agent::Agent,
    frozen: Option<&crate::spec::FrozenSpec>,
    use_git: bool,
) -> anyhow::Result<PhaseEnd> {
    let total = phases.len() as u32;
    let phase = &phases[(k - 1) as usize];
    let last = k == total;
    let run_hint = if p.config.acceptance_cmd.is_empty() {
        "Run the project's tests and fix failures until they pass.".to_string()
    } else {
        format!("Run `{}` and fix failures until it passes.", p.config.acceptance_cmd.join(" "))
    };
    let prompt = if total == 1 {
        format!(
            "Implement the feature described in the design document `{}`.\n\n1. Read the document, every file it links, and every existing file in its Implementation map.\n2. Change only the files listed in the Implementation map. If the design turns out to need another file, stop and explain why instead of changing it.\n3. Add or update the checks named under Acceptance criteria.\n4. {run_hint}\n5. Do not edit the design document.\n\nReply with a short summary of what you changed.",
            feature.doc
        )
    } else {
        let files: Vec<String> = phase.files.iter().map(|f| format!("`{f}`")).collect();
        format!(
            "Implement phase {k} of {total} (\"{}\") of the design document `{}`.\n\n1. Read the document, every file it links, and every existing file in its Implementation map.\n2. Change only the files of this phase: {}. Earlier phases are already built; later phases are not part of this session. If this phase needs another file, stop and explain why instead of changing it.\n3. {}\n4. Leave the code building and its tests passing at the end of this phase. {run_hint}\n5. Do not edit the design document.\n\nReply with a short summary of what you changed.",
            phase.name,
            feature.doc,
            files.join(", "),
            if last { "Add or update the checks named under Acceptance criteria." } else { "Add tests for what this phase adds where its files include tests." }
        )
    };
    let prompt = prompt + crate::executor::ASK;
    let prompt = match frozen {
        Some(spec) => {
            let mut note = String::new();
            if !spec.test_files.is_empty() {
                note.push_str(&format!(
                    "\n\nNote: the acceptance tests are already written and frozen: {}. Do not edit them or add to them; make them pass.",
                    spec.test_files.keys().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
                ));
            }
            for (f, (thm, _)) in &spec.lean_files {
                note.push_str(&format!("\n\nIn `{f}`, replace only the `sorry` proof of `{thm}` with a real proof; change nothing else in that file."));
            }
            prompt + &note
        }
        None => prompt,
    };
    let label = if total > 1 { format!(" phase {k}/{total} ({})", phase.name) } else { String::new() };
    let before = p.fingerprint();
    ui::egm(&format!("{} builds {}{label} (workspace-write)…", agent.name(), feature.doc));
    let reply = agent.run(Turn { prompt, session: None, access: Access::WorkspaceWrite, role: Role::Builder, doc: Some(doc.path.clone()), workdir: None })?;
    record(p, &feature.slug, "builder", agent, &reply.usage)?;
    ui::block(&reply.text);

    // What the builder had to decide on its own (the executor is a goldfish too).
    let listed = crate::executor::parse(&reply.text);
    let corpus = goldfish::corpus(doc, &p.root);
    let raised = listed.as_deref().map(|l| crate::executor::alarms(l, doc, &corpus, 1)).unwrap_or_default();
    let reopen = raised.iter().any(|a| a.counts());
    match &listed {
        None => ui::warn("the builder did not list the decisions it made; nothing to check against the doc"),
        Some(l) if l.is_empty() => ui::egm("the builder reports no decision the doc left open"),
        Some(l) => {
            for a in l {
                ui::block(&format!("{} builder decided: {} ({})", if a.blocking { "[blocking]" } else { "  assumed" }, a.decision, a.section));
            }
            for a in raised.iter().filter(|a| !a.counts()) {
                ui::egm(&format!("dropped: {} — {}", a.text, a.reason.as_deref().unwrap_or("")));
            }
            save_assumptions(p, &feature.slug, l)?;
        }
    }

    let after = p.fingerprint();
    let changed: Vec<String> = after.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, _)| k.clone())
        .chain(before.keys().filter(|k| !after.contains_key(*k)).cloned()).collect();
    let is_frozen_test = |f: &String| frozen.is_some_and(|s| s.test_files.contains_key(f));
    let allowed: Vec<String> = phase.files.iter().filter(|f| !is_frozen_test(f)).cloned().collect();
    let mut problems = Vec::new();
    if reopen {
        problems.push(format!("the builder had to make {} decision(s) the design doc leaves open", raised.iter().filter(|a| a.counts()).count()));
    }
    if changed.contains(&feature.doc) {
        problems.push("the builder edited the design doc".to_string());
    }
    // The fix lane's size limit, on the real change (egm-dsh counts; no model decides).
    let mut too_big = None;
    if feature.is_fix() && use_git {
        let code: Vec<String> = changed.iter().filter(|f| allowed.contains(f)).cloned().collect();
        let n = crate::fix::changed_lines(&p.root, &code)?;
        ui::egm(&format!("fix size: {n} changed line(s) in {} file(s) (limit {})", code.len(), p.config.fix.max_lines));
        if n > p.config.fix.max_lines {
            let why = format!("{n} changed lines (the fix lane allows {})", p.config.fix.max_lines);
            problems.push(why.clone());
            too_big = Some(why);
        }
    }
    for f in changed.iter().filter(|f| is_frozen_test(f)) {
        problems.push(format!("frozen test `{f}` was changed; egm-dsh restores it"));
    }
    for f in changed.iter().filter(|f| **f != feature.doc && !allowed.contains(f) && !is_frozen_test(f)) {
        let where_ = if total > 1 { "this phase's part of the Implementation map" } else { "the Implementation map" };
        problems.push(format!("`{f}` changed but is not in {where_} (revert it, or add it to the map and re-run the goldfish)"));
    }
    for f in allowed.iter().filter(|f| !changed.contains(f) && frozen.is_none_or(|s| !s.lean_files.contains_key(*f))) {
        ui::warn(&format!("`{f}` is in the Implementation map but did not change"));
    }
    if let Some(spec) = frozen {
        for (f, (thm, h)) in &spec.lean_files {
            let now = crate::spec::frozen_hash(&p.root, &crate::spec::Check::Lean { file: f.clone(), theorem: thm.clone() });
            if now != Some(*h) {
                problems.push(format!("`{f}` changed outside the proof of `{thm}`; egm-dsh restores it"));
            }
        }
    }
    if !p.config.acceptance_cmd.is_empty() {
        ui::egm(&format!("acceptance: {}", p.config.acceptance_cmd.join(" ")));
        // Sandboxed, in a scratch copy: the acceptance command runs code a model wrote.
        let out = crate::safety::run_in_copy(&p.config.safety, &p.root, &p.config.acceptance_cmd)?;
        let log = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
        ui::block(&log.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
        if !out.status.success() {
            problems.push(format!("acceptance command failed ({})", out.status));
        }
    }
    if last {
        // Green: the executable spec must pass. Each frozen test runs on its own.
        let results = run_spec(p, feature, doc, frozen.is_some());
        for (name, r) in &results {
            if let Err(e) = r {
                problems.push(format!("spec check `{name}`: {e}"));
            }
        }
        if frozen.is_some() && !results.is_empty() && problems.is_empty() {
            ui::egm(&format!("green: all {} spec check(s) pass, each frozen test run on its own", results.len()));
        }
    }
    if !problems.is_empty() {
        for pr in &problems {
            ui::warn(pr);
        }
        if let Some(spec) = frozen {
            restore_frozen(p, &feature.slug, spec)?;
        }
        if use_git {
            let created: Vec<String> = changed.iter().filter(|f| !before.contains_key(*f)).cloned().collect();
            let undo: Vec<String> = changed.iter().filter(|f| !is_frozen_test(f)).cloned().collect();
            let lost = crate::vcs::revert(&p.root, &undo, &created)?;
            ui::egm(&format!("reverted the rejected build: {} file(s) restored from git, {} created file(s) removed", undo.len() - created.len() - lost.len(), created.len()));
            for f in lost {
                ui::warn(&format!("`{f}` was untracked before the build and changed; restore it by hand"));
            }
        }
        let _ = p.sync(feature);
        p.save_feature(feature)?;
        // EgmFix.outcome: red was checked when the test was frozen (or the check is manual).
        let red = frozen.is_some() || checks_of(doc).iter().all(|(_, _, k)| matches!(k, crate::spec::Check::Manual));
        let others = problems.len() - usize::from(reopen) - usize::from(too_big.is_some());
        if feature.is_fix() && crate::fix::outcome(red, others == 0, too_big.is_none(), reopen) == crate::fix::Outcome::Promoted {
            if !raised.is_empty() {
                save_assumptions(p, &feature.slug, listed.as_deref().unwrap_or(&[]))?;
            }
            let why = too_big.unwrap_or_else(|| "the builder had to make a decision the short doc leaves open".to_string());
            return Ok(PhaseEnd::Promoted(why));
        }
        if reopen {
            return Ok(PhaseEnd::Reopened(raised, listed.unwrap_or_default()));
        }
        bail!("build{label} not accepted; the feature stays {:?} with {} of {total} phase(s) built", feature.gate.phase, feature.phases_done);
    }
    feature.phases_done = k;
    p.save_feature(feature)?;
    if use_git {
        let label = if total > 1 { format!("phase {k}/{total} ({})", phase.name) } else { "build".to_string() };
        if let Some(id) = crate::vcs::commit(&p.root, &allowed, &format!("egm-dsh build {}: {label}\n\nDesign: {}", feature.slug, feature.doc))? {
            ui::egm(&format!("committed {id} on {}", feature.git_branch.as_deref().unwrap_or("?")));
        }
    }
    if total > 1 {
        ui::ok(&format!("phase {k}/{total} accepted: {} file(s) changed, all inside the phase", changed.len()));
    }
    Ok(PhaseEnd::Accepted)
}

/// How one build phase ended, when it did not fail outright.
enum PhaseEnd {
    Accepted,
    /// The builder reported blocking decisions the doc leaves open (alarms, every assumption).
    Reopened(Vec<crate::alarm::Alarm>, Vec<crate::executor::Assumption>),
    /// A fix too big for its lane (or needing a decision): reverted; it goes to the full lane.
    Promoted(String),
}

/// Keep every decision a builder reported, per feature, for review and the next design round.
fn save_assumptions(p: &Project, slug: &str, list: &[crate::executor::Assumption]) -> anyhow::Result<()> {
    let path = Project::egm_dir(&p.root).join("assumptions").join(format!("{slug}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    for a in list {
        use std::io::Write;
        writeln!(f, "{}", json!({"ts": now_secs(), "decision": a.decision, "section": a.section, "quote": a.quote, "blocking": a.blocking}))?;
    }
    Ok(())
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct MeanFinding {
    severity: String,
    file: String,
    issue: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct MeanReview {
    findings: Vec<MeanFinding>,
}

/// `egm-dsh review <slug>`: a fresh, deliberately hostile code review.
pub fn review(p: &Project, slug: &str) -> anyhow::Result<bool> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    if !matches!(feature.gate.phase, Phase::Built | Phase::Hardened) {
        p.save_feature(&feature)?;
        bail!("{}", crate::phase::Refusal::ReviewBeforeBuild);
    }
    let act = p.begin(slug, feature.gate, crate::run::Kind::Review)?;
    let agent = p.agent_for(RoleName::Reviewer);
    let ask = "Report every way the code departs from the design, mishandles an edge case, or is hard to read; flag any stretch of 10 or more code lines with no comment. Do not edit files. Mark a finding blocking only if it ships a defect, major if it should be fixed before merge, and minor otherwise.\n\nReply with only one JSON object: {\"findings\": [{\"severity\": \"blocking | major | minor\", \"file\": \"...\", \"issue\": \"...\"}]}";
    let prompt = if agent.reads_files() {
        format!("Assume the code written for the design document `{}` has defects. Read the document, then every file in its Implementation map. {ask}", feature.doc)
    } else {
        let mut ctx = format!("=== {} ===\n{}\n", feature.doc, doc.text);
        for f in doc.implementation_map() {
            if let Ok(t) = std::fs::read_to_string(p.root.join(&f)) {
                ctx.push_str(&format!("\n=== {f} ===\n{t}\n"));
            }
        }
        format!("Assume the code below, written for the design document that comes first, has defects. {ask}\n\n{ctx}")
    };
    ui::egm(&format!("mean review by a fresh {} session…", agent.name()));
    let reply = agent.run(Turn { prompt, session: None, access: Access::ReadOnly, role: Role::Critic, doc: Some(doc.path.clone()), workdir: None })?;
    record(p, slug, "reviewer", agent.as_ref(), &reply.usage)?;
    let parsed: MeanReview = goldfish::parse(&reply.text).context("the reviewer did not return JSON findings")?;
    for f in &parsed.findings {
        ui::block(&format!("[{}] {}: {}", f.severity, f.file, f.issue));
    }
    let serious = parsed.findings.iter().filter(|f| matches!(f.severity.to_lowercase().as_str(), "blocking" | "major")).count();
    feature.gate = act.finish(Some(Event::Review(serious == 0)))?;
    p.save_feature(&feature)?;
    if serious == 0 {
        ui::ok(&format!("no blocking or major findings ({} minor); `{slug}` is hardened", parsed.findings.len()));
    } else {
        ui::warn(&format!("{serious} blocking/major finding(s); fix them and review again"));
    }
    Ok(serious == 0)
}

/// `egm-dsh fix <slug> "<problem>" --files F --check TEST`: a tiny fix with less
/// ceremony (fix.rs). Run it again to resume from wherever the fix stopped.
pub fn fix(p: &Project, slug: &str, problem: Option<&str>, files: &[String], test: Option<&str>, manual: Option<&str>) -> anyhow::Result<()> {
    check_slug(slug)?;
    if !p.config.safety.git || !crate::vcs::is_repo(&p.root) || !crate::vcs::has_commits(&p.root) {
        bail!("the fix lane needs git (a repository with a commit, and `safety.git` on): it reverts an oversized fix and its reviewer reads the diff");
    }
    let mut feature = match p.load_feature(slug) {
        Ok(f) => {
            if problem.is_some() || !files.is_empty() || test.is_some() || manual.is_some() {
                bail!("`{slug}` already exists; run `egm-dsh fix {slug}` with no other arguments to continue it");
            }
            if !f.is_fix() {
                bail!("`{slug}` is in the full design lane (a fix is promoted there when it turns out bigger); continue with `egm-dsh loop {slug}`");
            }
            f
        }
        Err(_) => {
            let problem = problem.context("describe the problem: egm-dsh fix <name> \"what is wrong\" --files F --check TEST")?;
            let why = crate::fix::eligible(files, &p.config.fix);
            if !why.is_empty() {
                bail!("not a tiny fix: {}. Use the full lane: `egm-dsh elephant {slug} --idea ...`", why.join("; "));
            }
            let check = crate::fix::check_text(test, manual).context("name the check: --check tests/x.py::test_y (red now, green after the fix), or --manual \"how to verify\" when no test can show it")?;
            if let Some(t) = test {
                let file = t.split("::").next().unwrap_or(t);
                anyhow::ensure!(!files.iter().any(|f| f == file), "the test file `{file}` cannot also be a file the fix changes");
            }
            let rel = format!("docs/fixes/{slug}.md");
            let text = crate::fix::doc_text(&title_of(slug), problem, files, &check, &p.config.fix);
            let path = p.root.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, &text)?;
            let f = Feature {
                slug: slug.into(),
                title: title_of(slug),
                idea: problem.into(),
                doc: rel.clone(),
                elephant_session: None,
                gate: Gate::new(doc::hash(&text)),
                goldfish_runs: 0,
                first_verdict: None,
                first_verdict_model: None,
                label: None,
                built_files: Default::default(),
                built_doc_hash: None,
                depends_on: vec![],
                built_seq: None,
                phases_done: 0,
                phase_doc_hash: None,
                spec: None,
                git_branch: None,
                lane: Some("fix".into()),
            };
            p.save_feature(&f)?;
            ui::egm(&format!("fix `{slug}`: short doc at {rel} (no elephant); limits: {} file(s), {} changed line(s)", p.config.fix.max_files, p.config.fix.max_lines));
            f
        }
    };
    loop {
        let doc = p.sync(&mut feature)?;
        p.save_feature(&feature)?;
        match feature.gate.phase {
            Phase::Designing => {
                ui::banner(&format!("fix `{slug}`: one goldfish reads the short doc"));
                if goldfish(p, slug, false, false, true)? != Verdict::Ready {
                    bail!("the short doc is not clear enough to fix from. Edit {} (the goldfish's questions are above), then `egm-dsh fix {slug}` again", feature.doc);
                }
            }
            Phase::Ready if frozen_for(&feature, &doc).is_none() && checks_of(&doc).iter().any(|(_, _, k)| !matches!(k, crate::spec::Check::Manual)) => {
                // Red first. A test a person already wrote is only run; a missing one is written and judged.
                let missing = checks_of(&doc).iter().any(|(_, _, k)| match k {
                    crate::spec::Check::Test { file, id } => {
                        let name = id.as_deref().map(|i| i.rsplit("::").next().unwrap_or(i).to_string());
                        !std::fs::read_to_string(p.root.join(file)).is_ok_and(|t| name.is_none_or(|n| t.contains(&n)))
                    }
                    _ => false,
                });
                ui::banner(&format!("fix `{slug}`: red first{}", if missing { " (the test is written and judged)" } else { " (your test runs as written)" }));
                tests_with(p, slug, missing, missing)?;
            }
            Phase::Ready => {
                ui::banner(&format!("fix `{slug}`: one builder, the listed files only"));
                build(p, slug, None)?;
            }
            Phase::Built => {
                ui::banner(&format!("fix `{slug}`: a fresh reviewer reads only the diff"));
                if !fix_review(p, slug)? {
                    bail!("the diff review found blocking or major problems (above). The fix stays built on its branch; correct it and run `egm-dsh fix {slug}` again, or `egm-dsh label {slug} rework`");
                }
            }
            Phase::Hardened => break,
        }
        feature = p.load_feature(slug)?;
    }
    let usage = usage_text(p);
    let (mut sessions, mut tokens) = (0, 0);
    for l in usage.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).filter(|v| v["feature"] == slug) {
        sessions += 1;
        tokens += ["cache_hit", "cache_miss", "output"].iter().map(|k| l[k].as_u64().unwrap_or(0)).sum::<u64>();
    }
    p.append_ledger(json!({"kind": "fix-done", "slug": slug, "sessions": sessions, "tokens": tokens, "ts": now_secs()}))?;
    ui::ok(&format!("fix `{slug}` is hardened on branch `{}`: {sessions} model session(s), {tokens} tokens. Merging is up to you", feature.git_branch.as_deref().unwrap_or("?")));
    Ok(())
}

/// The fix lane's review: one fresh, hostile session that sees the short doc and the
/// diff of the build, nothing else. Ends with the gate's `Review` event.
fn fix_review(p: &Project, slug: &str) -> anyhow::Result<bool> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    anyhow::ensure!(matches!(feature.gate.phase, Phase::Built | Phase::Hardened), "{}", crate::phase::Refusal::ReviewBeforeBuild);
    let files = doc.implementation_map();
    let mut args = vec!["diff".to_string(), "HEAD~1".into(), "HEAD".into(), "--".into()];
    args.extend(files.iter().cloned());
    let out = std::process::Command::new("git").args(&args).current_dir(&p.root).output()?;
    let diff = String::from_utf8_lossy(&out.stdout).to_string();
    anyhow::ensure!(out.status.success() && !diff.trim().is_empty(), "no diff to review for {}; was the fix committed?", files.join(", "));
    let act = p.begin(slug, feature.gate, crate::run::Kind::Review)?;
    let agent = p.agent_for(RoleName::Reviewer);
    let prompt = format!(
        "Assume this diff, written to fix the problem described in the short design document below, has defects. Everything you need is in this message; do not read other files. \
Report every way the diff fails to fix the problem, breaks an existing caller or saved data, mishandles an edge case, or does more than the fix needs. Do not edit files. \
Mark a finding blocking only if it ships a defect, major if it should be fixed before merge, and minor otherwise.\n\n\
Reply with only one JSON object: {{\"findings\": [{{\"severity\": \"blocking | major | minor\", \"file\": \"...\", \"issue\": \"...\"}}]}}\n\n=== {} ===\n{}\n\n=== diff ===\n{diff}",
        feature.doc, doc.text
    );
    // The reviewer runs in a room holding only the doc; the diff is in the prompt.
    let stage = crate::stage::Stage::new(&p.root, std::slice::from_ref(&doc.path))?;
    let workdir = agent.reads_files().then(|| stage.dir.clone());
    ui::egm(&format!("diff review by a fresh {} session ({} diff lines)…", agent.name(), diff.lines().count()));
    let reply = agent.run(Turn { prompt, session: None, access: Access::ReadOnly, role: Role::Critic, doc: Some(doc.path.clone()), workdir })?;
    record(p, slug, "reviewer", agent.as_ref(), &reply.usage)?;
    if let Some(why) = agent.reads_files().then(|| stage.escape(&reply.touched)).flatten() {
        // Fail closed: a reviewer that looked elsewhere does not get to approve.
        act.finish(None)?;
        bail!("the diff reviewer left its room ({why}); its review is discarded. Run `egm-dsh fix {slug}` again");
    }
    let parsed: MeanReview = goldfish::parse(&reply.text).context("the reviewer did not return JSON findings")?;
    for f in &parsed.findings {
        ui::block(&format!("[{}] {}: {}", f.severity, f.file, f.issue));
    }
    let serious = parsed.findings.iter().filter(|f| matches!(f.severity.to_lowercase().as_str(), "blocking" | "major")).count();
    feature.gate = act.finish(Some(Event::Review(serious == 0)))?;
    p.save_feature(&feature)?;
    if serious == 0 {
        ui::ok(&format!("diff review: no blocking or major findings ({} minor)", parsed.findings.len()));
    }
    Ok(serious == 0)
}

/// `egm-dsh diagrams FILE...`: every PlantUML diagram's problems. Returns whether any has one.
pub fn diagrams(files: &[std::path::PathBuf], checker: &crate::diagram::Checker, json_out: bool, plantuml_only: bool) -> anyhow::Result<bool> {
    let mut bad = false;
    for f in files {
        let text = std::fs::read_to_string(f).with_context(|| format!("cannot read {}", f.display()))?;
        for (k, b) in crate::diagram::blocks(&text).iter().enumerate() {
            let mut errs = if plantuml_only { vec![] } else { crate::diagram::check(&b.body) };
            let mut tool = None;
            if errs.is_empty() && *checker != crate::diagram::Checker::None {
                match crate::diagram::syntax(checker, &b.body) {
                    Ok(Some(e)) => errs.push(e),
                    Ok(None) => {}
                    Err(e) => tool = Some(e),
                }
            }
            bad |= !errs.is_empty();
            if json_out {
                crate::ui::out(format!("{}", json!({"file": f, "diagram": k + 1, "line": b.line, "section": b.section, "ok": errs.is_empty(), "problems": errs, "plantuml_error": tool})));
            } else if errs.is_empty() {
                ui::block(&format!("✔ {}:{} diagram {}{}", f.display(), b.line, k + 1, tool.map(|t| format!(" (structure only: {t})")).unwrap_or_default()));
            } else {
                ui::block(&format!("✖ {}:{} diagram {}: {}", f.display(), b.line, k + 1, errs.join("; ")));
            }
        }
    }
    Ok(bad)
}

/// `egm-dsh drift`: does shipped code still match its design doc? No model involved.
pub fn drift(p: &Project) -> anyhow::Result<bool> {
    let now = p.fingerprint();
    let mut any = false;
    let all = p.features()?;
    for mut f in all.clone() {
        let was_built = f.built_doc_hash.is_some();
        let Ok(doc) = p.sync(&mut f) else {
            ui::warn(&format!("{}: design doc {} is missing", f.slug, f.doc));
            any = true;
            continue;
        };
        p.save_feature(&f)?;
        if !was_built {
            continue;
        }
        if f.built_doc_hash != Some(doc.hash()) {
            ui::warn(&format!("{}: design doc changed after the build; the feature is back in design — run the goldfish", f.slug));
            any = true;
        }
        for path in doc.implementation_map() {
            // A later step's build that lists this file checks it from now on.
            if let Some(g) = crate::steps::handed_over(&f, &path, &all) {
                ui::egm(&format!("{}: `{path}` is checked under `{}`, built later", f.slug, g.slug));
                continue;
            }
            match (now.get(&path), f.built_files.get(&path)) {
                (None, _) => {
                    ui::warn(&format!("{}: `{path}` is in the Implementation map but no longer exists", f.slug));
                    any = true;
                }
                (Some(a), Some(b)) if a != b => {
                    ui::warn(&format!("{}: `{path}` changed since the build without a design change", f.slug));
                    any = true;
                }
                _ => {}
            }
        }
    }
    if !any {
        ui::ok("no drift: every built feature's files match the build and its doc is unchanged");
    }
    Ok(any)
}

/// `egm-dsh after <slug> <steps...>`: replace the steps this one depends on.
pub fn set_after(p: &Project, slug: &str, steps: &[String]) -> anyhow::Result<()> {
    let mut f = p.load_feature(slug)?;
    let others: Vec<Feature> = p.features()?.into_iter().filter(|g| g.slug != slug).collect();
    crate::steps::check_deps(&others, slug, steps)?;
    f.depends_on = steps.to_vec();
    p.save_feature(&f)?;
    if steps.is_empty() {
        ui::ok(&format!("`{slug}` depends on no other step"));
    } else {
        ui::ok(&format!("`{slug}` builds only after {}", steps.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")));
    }
    Ok(())
}

pub fn label(p: &Project, slug: &str, outcome: &str) -> anyhow::Result<()> {
    if !matches!(outcome, "rework" | "clean") {
        bail!("outcome is `rework` (the build needed design-level changes) or `clean`");
    }
    let mut f = p.load_feature(slug)?;
    f.label = Some(outcome.into());
    p.save_feature(&f)?;
    ui::ok(&format!("{slug} labeled {outcome}"));
    Ok(())
}

fn print_confusion(c: &Confusion) {
    ui::block(&format!(
        "                 rework   clean\n  not-ready      {:>6}  {:>6}\n  ready          {:>6}  {:>6}\n\n  n = {}   MCC = {:+.3}",
        c.tp, c.fp, c.fn_, c.tn, c.n(), c.mcc()
    ));
    if c.n() < MIN_LABELS {
        ui::warn(&format!("only {} labeled feature(s); treat MCC as noise until there are at least {MIN_LABELS}", c.n()));
    }
}

pub fn metrics(p: &Project, by_model: bool) -> anyhow::Result<()> {
    let mut all = Confusion::default();
    let mut per: std::collections::BTreeMap<String, Confusion> = Default::default();
    for f in p.features()? {
        if let (Some(v), Some(l)) = (&f.first_verdict, &f.label) {
            all.add(v == "not-ready", l == "rework");
            let model = f.first_verdict_model.clone().unwrap_or_else(|| "unknown".into());
            per.entry(model).or_default().add(v == "not-ready", l == "rework");
        }
    }
    ui::egm("first goldfish verdict (not-ready = positive) vs outcome (rework = positive)");
    print_confusion(&all);
    if by_model {
        for (model, c) in &per {
            ui::egm(&format!("goldfish model: {model}"));
            print_confusion(c);
        }
        ui::egm("a cheaper goldfish model is justified when its MCC holds up against the larger one on enough features");
        alarm_precision(p)?;
    }
    Ok(())
}

/// Of the alarms each reader model raised and that were then judged, how many held up.
fn alarm_precision(p: &Project) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(Project::egm_dir(&p.root).join("ledger.jsonl")).unwrap_or_default();
    let mut per: std::collections::BTreeMap<String, (u64, u64, u64)> = Default::default();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["kind"] != "goldfish" || v["verified"] != true {
            continue;
        }
        let e = per.entry(v["reader_model"].as_str().unwrap_or("unknown").to_string()).or_default();
        e.0 += v["confirmed"].as_u64().unwrap_or(0);
        e.1 += v["dropped_by_grounding"].as_u64().unwrap_or(0);
        e.2 += v["dropped_by_verifier"].as_u64().unwrap_or(0);
    }
    if per.is_empty() {
        return Ok(());
    }
    ui::egm("alarms that held up, per goldfish model (verified runs only)");
    crate::ui::out(format!("{:<20} {:>9} {:>14} {:>14} {:>10}", "model", "confirmed", "dropped:quote", "dropped:check", "held up"));
    for (model, (c, g, v)) in per {
        let judged = c + g + v;
        let share = if judged == 0 { "-".to_string() } else { format!("{:.0}%", 100.0 * c as f64 / judged as f64) };
        crate::ui::out(format!("{model:<20} {c:>9} {g:>14} {v:>14} {share:>10}"));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct UsageLine {
    feature: String,
    role: String,
    model: String,
    cache_hit: u64,
    cache_miss: u64,
    output: u64,
    requests: u64,
    #[serde(default)]
    estimated: bool,
    #[serde(default)]
    reasoning: u64,
}

/// `egm-dsh usage`: tokens and cost per role, and what the same tokens would cost on one model.
pub fn usage(p: &Project, feature: Option<&str>) -> anyhow::Result<()> {
    let path = Project::egm_dir(&p.root).join("usage.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut rows: std::collections::BTreeMap<(String, String), Usage> = Default::default();
    let mut estimated = false;
    for line in text.lines() {
        let Ok(u) = serde_json::from_str::<UsageLine>(line) else { continue };
        if feature.is_some_and(|f| f != u.feature) {
            continue;
        }
        estimated |= u.estimated;
        rows.entry((u.role, u.model)).or_default().add(&Usage {
            cache_hit: u.cache_hit, cache_miss: u.cache_miss, output: u.output, requests: u.requests, estimated: u.estimated, reasoning: u.reasoning,
        });
    }
    if rows.is_empty() {
        ui::egm("no usage recorded yet");
        return Ok(());
    }
    let prices = &p.config.prices;
    let order = |r: &str| RoleName::ALL.iter().position(|n| n.as_str() == r).unwrap_or(9);
    let mut keys: Vec<_> = rows.keys().cloned().collect();
    keys.sort_by_key(|(r, m)| (order(r), m.clone()));

    crate::ui::out(format!("{:<18} {:<18} {:>5} {:>10} {:>9} {:>9} {:>10}", "role", "model", "reqs", "cache-hit", "miss", "output", "cost peak"));
    let mut total = Usage::default();
    let mut dollars = 0.0;
    let mut unpriced = Vec::new();
    for key in &keys {
        let u = &rows[key];
        total.add(u);
        let cost = match cost::lookup(prices, &key.1) {
            Some(mp) => {
                let d = cost::dollars(mp, u);
                dollars += d;
                format!("${d:.4}")
            }
            None => {
                unpriced.push(key.1.clone());
                "?".into()
            }
        };
        crate::ui::out(format!("{:<18} {:<18} {:>5} {:>10} {:>9} {:>9} {:>10}", key.0, key.1, u.requests, u.cache_hit, u.cache_miss, u.output, cost));
    }
    crate::ui::out(format!("{:<18} {:<18} {:>5} {:>10} {:>9} {:>9} {:>10}", "TOTAL", "", total.requests, total.cache_hit, total.cache_miss, total.output, format!("${dollars:.4}")));
    let input = total.cache_hit + total.cache_miss;
    if input > 0 {
        ui::egm(&format!("{:.0}% of input tokens were cache hits; off-peak hours cost half: ${:.4}", 100.0 * total.cache_hit as f64 / input as f64, dollars / 2.0));
    }
    for (label, model) in [("everything on Pro", crate::project::PRO), ("everything on Flash", crate::project::FLASH)] {
        if let Some(mp) = cost::lookup(prices, model) {
            let d: f64 = rows.values().map(|u| cost::dollars(mp, u)).sum();
            ui::block(&format!("same tokens with {label}: ${d:.4}"));
        }
    }
    ui::block("(repricing the observed tokens; a different model would also use a different number of tokens)");
    if !unpriced.is_empty() {
        ui::warn(&format!("no price for {}; add it under \"prices\" in .egm-dsh/config.json", unpriced.join(", ")));
    }
    if estimated {
        ui::warn("some rows are estimates from the mock agent (it simulates dsh's measured per-request overhead and assumes reasoning tokens); real runs report actual usage");
    }
    Ok(())
}

pub fn roles(p: &Project) -> anyhow::Result<()> {
    crate::ui::out(format!("{:<18} {:<44} used by", "role", "agent"));
    let used = |r: RoleName| match r {
        RoleName::Elephant => "egm-dsh elephant, egm-dsh draft",
        RoleName::Goldfish => if p.config.goldfish_mode == GoldfishMode::Combined { "egm-dsh goldfish (all 4 stages, one session)" } else { "egm-dsh goldfish: comprehension, readiness, invariants" },
        RoleName::GoldfishCritic => if p.config.goldfish_mode == GoldfishMode::Combined { "(not used in combined mode)" } else { "egm-dsh goldfish: critic stage" },
        RoleName::GoldfishVerifier => if p.config.verify_alarms { "egm-dsh goldfish: checks alarms, only when there are some" } else { "(off: verify_alarms is false)" },
        RoleName::Builder => "egm-dsh build",
        RoleName::Reviewer => "egm-dsh review",
    };
    for r in RoleName::ALL {
        crate::ui::out(format!("{:<18} {:<44} {}", r.as_str(), p.agent_for(r).name(), used(r)));
    }
    Ok(())
}

pub fn status(p: &Project) -> anyhow::Result<()> {
    let mut features = p.features()?;
    if features.is_empty() {
        ui::egm("no features yet; start one with `egm-dsh elephant <name> --idea \"...\"`");
        return Ok(());
    }
    for f in features.iter_mut() {
        let _ = p.sync(f);
        p.save_feature(f)?;
    }
    // Steps in dependency order, so the table reads as the chain.
    crate::ui::out(format!("{:<18} {:<10} {:>6} {:>8} {:<10} {:<8} {:<18} design doc", "step", "phase", "build", "goldfish", "first", "label", "after"));
    for f in crate::steps::chain_order(&features) {
        let phases = p.doc(f).map(|d| d.implementation_phases().len()).unwrap_or(0);
        let done = if f.built_doc_hash.is_some() && matches!(f.gate.phase, crate::phase::Phase::Built | crate::phase::Phase::Hardened) {
            phases as u32
        } else if p.doc(f).ok().map(|d| d.hash()) == f.phase_doc_hash {
            f.phases_done
        } else {
            0
        };
        let waiting = crate::steps::unbuilt_deps(f, &features);
        let after = if f.depends_on.is_empty() { "-".to_string() } else { f.depends_on.join(",") };
        crate::ui::out(format!(
            "{:<18} {:<10} {:>6} {:>8} {:<10} {:<8} {:<18} {}{}",
            f.slug,
            format!("{:?}", f.gate.phase).to_lowercase(),
            format!("{done}/{phases}"),
            f.goldfish_runs,
            f.first_verdict.as_deref().unwrap_or("-"),
            f.label.as_deref().unwrap_or("-"),
            after,
            f.doc,
            if waiting.is_empty() { String::new() } else { format!("  (waits for {})", waiting.join(", ")) }
        ));
    }
    Ok(())
}

pub fn show(p: &Project, slug: &str) -> anyhow::Result<()> {
    let f = p.load_feature(slug)?;
    let d: Doc = p.doc(&f)?;
    crate::ui::out(d.text.to_string());
    Ok(())
}

// ---------------------------------------------------------------------------
// The executable spec: tests first and frozen, red then green, examples, Lean.

fn frozen_dir(p: &Project, slug: &str) -> std::path::PathBuf {
    Project::egm_dir(&p.root).join("frozen").join(slug)
}

/// Put every frozen test and Lean file back as it was frozen.
fn restore_frozen(p: &Project, slug: &str, spec: &crate::spec::FrozenSpec) -> anyhow::Result<()> {
    for f in spec.test_files.keys().chain(spec.lean_files.keys()) {
        let saved = frozen_dir(p, slug).join(f);
        if saved.is_file() {
            std::fs::copy(&saved, p.root.join(f))?;
        }
    }
    Ok(())
}

/// The frozen spec for the doc as it is now, if there is one.
fn frozen_for<'a>(f: &'a Feature, doc: &Doc) -> Option<&'a crate::spec::FrozenSpec> {
    f.spec.as_ref().filter(|s| s.doc_hash == doc.hash())
}

/// Criteria with a check, parsed.
fn checks_of(doc: &Doc) -> Vec<(String, String, crate::spec::Check)> {
    doc.criteria()
        .into_iter()
        .filter_map(|c| c.check.clone().map(|ch| (c.text.clone(), ch.clone(), crate::spec::parse_check(&ch))))
        .collect()
}

/// `egm-dsh tests <slug>`: write the acceptance tests before any code, check
/// that each one fails now (red), have a goldfish confirm each tests its
/// criterion, then freeze them.
pub fn tests(p: &Project, slug: &str) -> anyhow::Result<()> {
    tests_with(p, slug, true, true)
}

/// `write`: a tester session writes the tests; `review`: a goldfish judges them.
/// The fix lane skips both for a test a person already wrote (it is only checked red).
fn tests_with(p: &Project, slug: &str, write: bool, review: bool) -> anyhow::Result<()> {
    use crate::spec::{self, Check};
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    p.save_feature(&feature)?;
    if feature.gate.phase != Phase::Ready {
        bail!("tests come after a goldfish passes the current doc (phase is {:?}); run `egm-dsh goldfish {slug}`", feature.gate.phase);
    }
    let crits = checks_of(&doc);
    let mut files: Vec<String> = Vec::new();
    for (_, _, k) in &crits {
        if let Some(f) = k.file()
            && !files.iter().any(|x| x == f)
        {
            files.push(f.to_string());
        }
    }
    let examples = spec::examples(&doc);
    if files.is_empty() && examples.is_empty() {
        bail!("nothing executable: name a test (`Check: tests/x.py::test_y`) or a Lean theorem (`Check: lean proofs/X.lean#thm`) after each criterion, or add a `## Examples` section");
    }
    let prev = feature.spec.clone();

    // One test-writing run (counts against build attempts): tester + review sessions.
    let act = p.begin(slug, feature.gate, crate::run::Kind::Tests)?;

    // 1. A fresh tester session writes the tests; it may touch nothing else.
    if write && !files.is_empty() {
        let agent = p.agent_for(RoleName::Builder);
        if !agent.can_write() {
            bail!("{} cannot write files; set \"agent\": \"dsh\" in .egm-dsh/config.json", agent.name());
        }
        let list: Vec<String> = files.iter().map(|f| format!("`{f}`")).collect();
        let prompt = format!(
            "Write the acceptance tests for the design document `{}` before any code exists.\n\n1. Read the document, every file it links, and the existing code it names.\n2. For each acceptance criterion, write the test its `Check:` names, in that file and under that name. Each test must fail today, because the feature is not built yet, and pass once the code does what its criterion says. Assert observable behavior, not implementation details.\n3. For a `Check: lean FILE#THEOREM` criterion, write FILE in core Lean 4 (no Mathlib): a small model of the behavior and the theorem stating the criterion, with `sorry` as its proof.\n4. Change only these files: {}. Do not write implementation code.\n5. Do not edit the design document.\n\nReply with a short summary of the tests you wrote.",
            feature.doc,
            list.join(", ")
        );
        let prompt = prompt + &caught_hint(p, slug);
        let before = p.fingerprint();
        ui::egm(&format!("{} writes the acceptance tests first (workspace-write, tests only)…", agent.name()));
        let reply = agent.run(Turn { prompt, session: None, access: Access::WorkspaceWrite, role: Role::Tester, doc: Some(doc.path.clone()), workdir: None })?;
        record(p, slug, "tester", agent.as_ref(), &reply.usage)?;
        ui::block(&reply.text);
        let after = p.fingerprint();
        let stray: Vec<String> = after.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, _)| k.clone())
            .filter(|k| !files.contains(k)).collect();
        if !stray.is_empty() {
            bail!("the tester changed files that are not tests: {}; revert them and run `egm-dsh tests {slug}` again", stray.join(", "));
        }
    }

    // 2. Mechanical checks: every named test exists, and every check fails now (red).
    let mut problems: Vec<String> = Vec::new();
    let mut frozen = spec::FrozenSpec { doc_hash: doc.hash(), ..Default::default() };
    for (text, check, kind) in &crits {
        let Some(fh) = spec::frozen_hash(&p.root, kind) else {
            problems.push(format!("`{check}`: the file is missing{}", if matches!(kind, Check::Lean { .. }) { " or has no such theorem" } else { "" }));
            continue;
        };
        if let Check::Test { file, id: Some(id) } = kind {
            let name = id.rsplit("::").next().unwrap_or(id);
            if !std::fs::read_to_string(p.root.join(file)).is_ok_and(|t| t.contains(name)) {
                problems.push(format!("`{check}`: no test named `{name}` in {file}"));
                continue;
            }
        }
        let was_carried = spec::carried(prev.as_ref(), text, check, fh);
        let red = match kind {
            Check::Manual => {
                ui::warn(&format!("`{check}` is manual: it is not part of the executable spec"));
                "manual"
            }
            _ if was_carried => "carried",
            Check::Test { file, id } => match spec::run_test(&p.config.safety, &p.root, &p.config.check_cmd, file, id.as_deref()) {
                Ok(passes) if spec::red_ok(!passes, false) => "red",
                Ok(_) => {
                    problems.push(format!("`{check}` already passes before any code is written, so it cannot tell built code from unbuilt (vacuous test)"));
                    "green-too-early"
                }
                Err(e) => {
                    problems.push(format!("`{check}`: {e}"));
                    "error"
                }
            },
            Check::Lean { file, theorem } => match spec::lean_check(&p.config.safety, &p.root, &p.config.lean_cmd, file, theorem) {
                Ok(()) => {
                    problems.push(format!("`{check}` is already proved; the theorem's proof should be `sorry` until the build"));
                    "green-too-early"
                }
                Err(spec::LeanFail::Red(_)) => "red",
                // A check that could not run proves nothing; it never counts as red.
                Err(spec::LeanFail::Harness(e)) => {
                    problems.push(format!("`{check}`: {e}"));
                    "error"
                }
            },
        };
        ui::block(&format!("{} {check}", match red { "red" => "✗ red    ", "carried" => "· carried", "manual" => "- manual ", _ => "! passes " }));
        match kind {
            Check::Test { file, .. } => {
                frozen.test_files.insert(file.clone(), fh);
            }
            Check::Lean { file, theorem } => {
                frozen.lean_files.insert(file.clone(), (theorem.clone(), fh));
            }
            Check::Manual => {}
        }
        frozen.criteria.push(spec::FrozenCriterion { text: text.clone(), check: check.clone(), file_hash: fh, red: red.into() });
    }
    for (i, ex) in examples.iter().enumerate() {
        let text = ex.text();
        let was_carried = prev.as_ref().is_some_and(|s| s.examples.iter().any(|(t, _)| *t == text));
        let red = if was_carried {
            "carried"
        } else if spec::run_example(&p.config.safety, &p.root, ex).is_err() {
            "red"
        } else {
            problems.push(format!("example {} already matches before the build, so it describes nothing new", i + 1));
            "green-too-early"
        };
        ui::block(&format!("{} example {} ({} command(s))", if red == "red" { "✗ red    " } else { "· carried" }, i + 1, ex.commands.len()));
        frozen.examples.push((text, red.into()));
    }
    if !problems.is_empty() {
        for pr in &problems {
            ui::warn(pr);
        }
        bail!("tests not frozen; fix the tests (run `egm-dsh tests {slug}` again) or the criteria");
    }

    // 3. A goldfish judges whether each test really checks its criterion.
    let judged: Vec<&(String, String, Check)> = crits.iter().filter(|(_, _, k)| !matches!(k, Check::Manual)).collect();
    if review && !judged.is_empty() {
        let reviewer = p.agent_for(RoleName::Goldfish);
        let mut prompt = format!(
            "Judge whether each acceptance test checks its criterion. Read the design document `{}` and the test files {}. For each criterion below, decide whether its named test would fail if the criterion were violated and pass if it holds. A test that asserts nothing relevant, or something weaker than the criterion, does not test it. For a Lean check, judge whether the theorem states the criterion about a faithful model of the behavior.\n\nCriteria:\n",
            feature.doc,
            files.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
        );
        for (text, check, _) in &judged {
            prompt.push_str(&format!("- `{check}`: {}\n", text.split("Check:").next().unwrap_or(text).trim()));
        }
        if !reviewer.reads_files() {
            prompt.push_str(&format!("\n=== {} ===\n{}\n", feature.doc, doc.text));
            for f in &files {
                if let Ok(t) = std::fs::read_to_string(p.root.join(f)) {
                    prompt.push_str(&format!("\n=== {f} ===\n{t}\n"));
                }
            }
        }
        prompt.push_str("\nReply with only one JSON object, and no other text: {\"criteria\": [{\"check\": \"...\", \"tests_it\": true, \"reason\": \"...\"}]}");
        ui::goldfish(&format!("a fresh reviewer ({}) checks that each test tests its criterion…", reviewer.name()));
        // The reading room: the doc, its links and map files, and the test files; nothing else.
        let mut room_files = vec![doc.path.clone()];
        room_files.extend(doc.context_files(&p.root));
        room_files.extend(files.iter().map(|f| p.root.join(f)));
        let stage = crate::stage::Stage::new(&p.root, &room_files).context("cannot prepare the test reviewer's reading room")?;
        let workdir = reviewer.reads_files().then(|| stage.dir.clone());
        let reply = reviewer.run(Turn { prompt, session: None, access: Access::ReadOnly, role: Role::TestReview, doc: Some(doc.path.clone()), workdir })?;
        record(p, slug, "goldfish", reviewer.as_ref(), &reply.usage)?;
        if let Some(why) = reviewer.reads_files().then(|| stage.escape(&reply.touched)).flatten() {
            // Fail closed: a reviewer that saw more than the doc and tests does not get to approve them.
            bail!("the test reviewer left the doc and tests ({why}); its judgment is discarded and the tests are not frozen. Run `egm-dsh tests {slug}` again");
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct J {
            check: String,
            tests_it: bool,
            reason: String,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Js {
            criteria: Vec<J>,
        }
        let js: Js = goldfish::parse(&reply.text).context("the test reviewer did not return JSON")?;
        for (_, check, _) in &judged {
            match js.criteria.iter().find(|j| j.check.trim_matches('`') == check.as_str()) {
                Some(j) if j.tests_it => {}
                Some(j) => problems.push(format!("`{check}` does not test its criterion: {}", j.reason)),
                None => problems.push(format!("the reviewer gave no judgment for `{check}`")),
            }
        }
        if !problems.is_empty() {
            for pr in &problems {
                ui::warn(pr);
            }
            bail!("tests not frozen; fix the tests and run `egm-dsh tests {slug}` again");
        }
    }

    // 4. Freeze: record hashes and keep copies so a build cannot change them.
    let dir = frozen_dir(p, slug);
    let _ = std::fs::remove_dir_all(&dir);
    for f in frozen.test_files.keys().chain(frozen.lean_files.keys()) {
        let dst = dir.join(f);
        std::fs::create_dir_all(dst.parent().unwrap())?;
        std::fs::copy(p.root.join(f), dst)?;
    }
    let (n_tests, n_lean, n_ex) = (frozen.test_files.len(), frozen.lean_files.len(), frozen.examples.len());
    act.finish(None)?;
    crate::events::emit("criteria.frozen", slug, serde_json::json!({
        "doc_hash": feature.gate.doc_hash.to_string(),
        "criteria": frozen.criteria.iter().map(|c| serde_json::json!({"check": c.check, "file_hash": c.file_hash, "red": c.red})).collect::<Vec<_>>(),
        "examples": frozen.examples.len(),
    }));
    feature.spec = Some(frozen);
    p.save_feature(&feature)?;
    ui::ok(&format!("spec frozen: {n_tests} test file(s), {n_lean} Lean file(s), {n_ex} example(s); every check is red now and the build may not change them"));
    Ok(())
}

/// Run the executable spec: (check, result) for every criterion and example.
fn run_spec(p: &Project, feature: &Feature, doc: &Doc, per_test: bool) -> Vec<(String, Result<(), String>)> {
    use crate::spec::{self, Check};
    let mut out = Vec::new();
    let frozen = frozen_for(feature, doc);
    for (_, check, kind) in checks_of(doc) {
        let r = match &kind {
            Check::Manual => continue,
            Check::Test { file, id } => {
                let tampered = frozen.and_then(|s| s.test_files.get(file)).is_some_and(|h| spec::frozen_hash(&p.root, &kind) != Some(*h));
                if tampered {
                    Err(format!("{file} differs from the frozen tests"))
                } else if !per_test {
                    if p.root.join(file).is_file() { Ok(()) } else { Err(format!("{file} does not exist")) }
                } else {
                    match spec::run_test(&p.config.safety, &p.root, &p.config.check_cmd, file, id.as_deref()) {
                        Ok(true) => Ok(()),
                        Ok(false) => Err("fails".into()),
                        Err(e) => Err(e),
                    }
                }
            }
            Check::Lean { file, theorem } => {
                let tampered = frozen.and_then(|s| s.lean_files.get(file)).is_some_and(|(_, h)| spec::frozen_hash(&p.root, &kind) != Some(*h));
                if tampered {
                    Err(format!("{file} changed outside the proof of `{theorem}`"))
                } else {
                    spec::lean_check(&p.config.safety, &p.root, &p.config.lean_cmd, file, theorem).map_err(|e| e.to_string())
                }
            }
        };
        out.push((check, r));
    }
    for (i, ex) in spec::examples(doc).iter().enumerate() {
        out.push((format!("example {}", i + 1), spec::run_example(&p.config.safety, &p.root, ex)));
    }
    out
}

/// `egm-dsh check <slug>`: run the executable spec now. Exit 2 if anything fails.
pub fn check(p: &Project, slug: &str) -> anyhow::Result<bool> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    p.save_feature(&feature)?;
    let results = run_spec(p, &feature, &doc, true);
    if results.is_empty() {
        ui::warn("nothing executable in this doc: no named tests, Lean checks or examples");
        return Ok(false);
    }
    let mut ok = true;
    for (name, r) in &results {
        match r {
            Ok(()) => ui::block(&format!("✔ {name}")),
            Err(e) => {
                ok = false;
                ui::block(&format!("✖ {name}: {e}"));
            }
        }
    }
    if ok {
        ui::ok(&format!("the spec runs green: {} check(s)", results.len()));
    } else {
        ui::warn("the spec does not run green");
    }
    Ok(ok)
}

/// `egm-dsh doctor`: is this machine and project set up safely for real use?
/// One line on what runs the goldfish's sessions.
pub fn reviewers_line(p: &Project) -> String {
    use crate::agent::RoleName;
    if !p.builtin_reviewer(RoleName::Goldfish) && !p.builtin_reviewer(RoleName::GoldfishVerifier) {
        return match p.config.reviewers {
            crate::project::ReviewerClient::Dsh => "reviewers: DeepSeek Harness reading rooms (`set-reviewers builtin` for egm-dsh's own client)".into(),
            _ => format!("reviewers: the {:?} agent configured for the goldfish roles", p.config.agent).to_lowercase(),
        };
    }
    let zoom = if p.zoom_out_agent().is_some() { "; zoom-out stays in dsh, to search the repository" } else { "; zoom-out too (it cannot search the repository)" };
    format!("reviewers: built-in /chat/completions client for reader, critic and verifier (no dsh process, no tools, no history){zoom}")
}

pub fn doctor(p: &Project) -> anyhow::Result<()> {
    use crate::safety::{self, Effective, SandboxKind};
    let s = &p.config.safety;
    let line = |ok: bool, msg: String| if ok { ui::ok(&msg) } else { ui::warn(&msg) };
    // Sandbox, with a live probe.
    match safety::effective(s) {
        Ok(Effective::Bwrap) => {
            let probe: Vec<String> = vec!["bash".into(), "-c".into(),
                "touch \"$HOME/.egm-dsh-probe\" 2>/dev/null && echo home-writable; (exec 3<>/dev/tcp/1.1.1.1/80) 2>/dev/null && echo net-open; echo ${DEEPSEEK_API_KEY:+key-visible}; echo probe-done".into()];
            let out = safety::run_in_copy(s, &p.root, &probe)?;
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let sealed = text.contains("probe-done") && !text.contains("home-writable") && !text.contains("net-open") && !text.contains("key-visible");
            line(sealed, format!("sandbox: bubblewrap{}", if sealed { " (probe: read-only root, no network, no secrets)" } else { &" — PROBE FAILED" }));
        }
        Ok(Effective::None) => line(false, format!("sandbox: none ({}) — model-written tests and examples run with your file and network access", if s.sandbox == SandboxKind::None { "set to none" } else { "bubblewrap not available" })),
        Err(e) => line(false, format!("sandbox: {e}")),
    }
    let passed: Vec<String> = safety::allowed_env(s).into_iter().map(|(k, _)| k).collect();
    line(true, format!("environment passed to commands: {}", passed.join(", ")));
    // Git.
    if !s.git {
        line(false, "git: off — rejected builds are not reverted".into());
    } else if !crate::vcs::is_repo(&p.root) {
        line(false, "git: not a repository — rejected builds are not reverted".into());
    } else if !crate::vcs::has_commits(&p.root) {
        line(false, "git: repository has no commits yet".into());
    } else {
        line(true, format!("git: on branch `{}`; builds use `egm-dsh/<feature>` branches and you merge them", crate::vcs::current_branch(&p.root)));
    }
    // Tools and credentials (never printed).
    let dsh = p.config.dsh_command.first().map(|c| which(c)).unwrap_or(false);
    line(dsh, format!("DeepSeek Harness: `{}`{}", p.config.dsh_command.join(" "), if dsh { "" } else { " not found on PATH" }));
    line(true, reviewers_line(p));
    let key = std::env::var("DEEPSEEK_API_KEY").is_ok_and(|v| !v.is_empty());
    line(key, format!("DEEPSEEK_API_KEY: {}", if key { "set (not shown; never passed to tests or examples)" } else { "not set" }));
    match crate::agent::deepseek_balance() {
        Some(Ok((true, totals))) => line(true, format!("DeepSeek balance: {totals}")),
        Some(Ok((false, totals))) => line(false, format!("DeepSeek balance: {totals} — not enough for API calls; top up at https://platform.deepseek.com/top_up")),
        Some(Err(e)) => ui::warn(&format!("could not read the DeepSeek balance: {e}")),
        None => {}
    }
    let lean = p.config.lean_cmd.first().map(|c| which(c)).unwrap_or(false);
    line(lean, format!("Lean: `{}`{}", p.config.lean_cmd.join(" "), if lean { "" } else { " not found (only needed for `Check: lean …`)" }));
    let checker = crate::diagram::checker(p.config.plantuml_check, &p.config.plantuml_cmd);
    let probe = (checker != crate::diagram::Checker::None).then(|| {
        let good = crate::diagram::syntax(&checker, &["@startuml".into(), "A -> B : ping".into(), "@enduml".into()]);
        let bad = crate::diagram::syntax(&checker, &["@startuml".into(), "A -> B : ping".into(), "this is not plantuml".into(), "@enduml".into()]);
        match (good, bad) {
            (Ok(None), Ok(Some(_))) => Ok(()),
            (Err(e), _) | (_, Err(e)) => Err(e),
            _ => Err("it does not tell a broken diagram from a good one".into()),
        }
    });
    let policy = format!("{:?}", p.config.diagrams).to_lowercase();
    match probe {
        None => line(true, format!("diagrams: {policy}; PlantUML syntax check off: egm-dsh's structure check only (`set-diagrams --plantuml-check` turns it on)")),
        Some(Ok(())) => line(true, format!("diagrams: {policy}; PlantUML syntax check: {}", checker.describe())),
        Some(Err(e)) => line(false, format!("diagrams: {policy}; PlantUML syntax check {} does not work ({e}); only the structure check runs. Fix it, or `set-diagrams --no-plantuml-cmd` for the built-in one", checker.describe())),
    }
    line(true, format!("tests first: {}; one test runs as: {}", if p.config.tests_first { "on" } else { "off" }, p.config.check_cmd.join(" ")));
    let leftover = crate::session::live();
    line(leftover.is_empty(), format!("agent sessions open: {} (registry: .egm-dsh/sessions.json; cleaned up on the next command after a crash)", leftover.len()));
    line(true, "data: every role sends repository code to the model provider; make sure that is allowed for this code".into());
    Ok(())
}

fn which(cmd: &str) -> bool {
    let path = std::path::Path::new(cmd);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|ps| std::env::split_paths(&ps).any(|d| d.join(cmd).is_file()))
}

/// `egm-dsh budget [feature]`: what each feature has used against its limits.
pub fn budget(p: &Project, only: Option<&str>) -> anyhow::Result<()> {
    let dir = Project::egm_dir(&p.root);
    let features: Vec<String> = match only {
        Some(s) => vec![p.load_feature(s)?.slug],
        None => p.features()?.into_iter().map(|f| f.slug).collect(),
    };
    crate::ui::out(format!("{:<18} {:>9} {:>9} {:>9} {:>9} {:>11}", "feature", "goldfish", "redraft", "builds", "reviews", "model calls"));
    for slug in features {
        let s = crate::session::load_spent(&dir, &slug);
        let l = p.config.limits_for(&slug);
        let cell = |used: u32, limit: u32| format!("{used}/{limit}");
        crate::ui::out(format!(
            "{:<18} {:>9} {:>9} {:>9} {:>9} {:>11}",
            slug,
            cell(s.goldfish_rounds, l.goldfish_rounds),
            cell(s.redraft_turns, l.redraft_turns),
            cell(s.build_attempts, l.build_attempts),
            cell(s.reviews, l.reviews),
            cell(s.model_calls, l.model_calls)
        ));
    }
    ui::egm("each activity and agent session is checked against these before it starts; raise one with `egm-dsh set-budget --<limit> N [--feature F]`");
    Ok(())
}

// ---------------------------------------------------------------------------------------
// structured escalation: goldfish questions → repository answers or human decisions

fn decisions_path(p: &Project, slug: &str) -> std::path::PathBuf {
    Project::egm_dir(&p.root).join("decisions").join(format!("{slug}.json"))
}

fn load_decisions(p: &Project, slug: &str) -> anyhow::Result<crate::escalate::Record> {
    let path = decisions_path(p, slug);
    let text = std::fs::read_to_string(&path).with_context(|| format!("no open escalation for `{slug}`; run `egm-dsh escalate {slug}` after a NOT READY verdict"))?;
    Ok(serde_json::from_str(&text)?)
}

fn save_decisions(p: &Project, rec: &crate::escalate::Record) -> anyhow::Result<()> {
    let path = decisions_path(p, &rec.slug);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(rec)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn show_decisions(rec: &crate::escalate::Record) {
    use crate::escalate::Status;
    for i in &rec.items {
        match i.status {
            Status::Answered => {
                let a = i.answer.as_ref().expect("answered items carry their answer");
                ui::block(&format!("#{} ✔ {}\n    settled by the repository: {} (`{}`: \"{}\")", i.id, i.question, a.text, a.file, clip(&a.quote)));
            }
            Status::Pending | Status::Decided => {
                let mut s = format!("#{} {} {}\n    why you: {}", i.id, if i.status == Status::Decided { "✔" } else { "?" }, i.question, i.why);
                for o in &i.options {
                    let rec_mark = if i.recommended.as_deref() == Some(o.key.as_str()) { "  (recommended)" } else { "" };
                    let cons = if o.consequence.is_empty() { String::new() } else { format!(" — {}", o.consequence) };
                    s.push_str(&format!("\n    [{}] {}{cons}{rec_mark}", o.key, o.label));
                }
                if let Some(c) = &i.choice {
                    s.push_str(&format!("\n    decided: {}", c.text));
                }
                ui::block(&s);
            }
        }
    }
}

/// `egm-dsh escalate <slug>`: after a NOT READY verdict, the elephant answers what the
/// repository settles (egm-dsh checks each quote) and escalates the rest as decisions.
pub fn escalate(p: &Project, slug: &str) -> anyhow::Result<()> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    let review_path = Project::egm_dir(&p.root).join("reviews").join(format!("{slug}-{}.json", feature.goldfish_runs));
    let review: goldfish::Review = serde_json::from_str(
        &std::fs::read_to_string(&review_path).with_context(|| format!("`{slug}` has no goldfish review yet; run `egm-dsh goldfish {slug}`"))?,
    )?;
    let hash = format!("{:016x}", doc.hash());
    if review.verdict != Verdict::NotReady {
        bail!("the latest goldfish verdict for `{slug}` is {}; escalation is for NOT READY", review.verdict);
    }
    if review.doc_hash.as_deref() != Some(hash.as_str()) {
        bail!("the doc changed since the last goldfish review; run `egm-dsh goldfish {slug}` on this revision first");
    }
    let open = review.open_alarms();
    // One elephant turn, on the redraft budget; refused before any model call when spent.
    let act = p.begin(slug, feature.gate, crate::run::Kind::Elephant)?;
    let agent = p.agent_for(RoleName::Elephant);
    let view = elephant_view(p, &feature)?;
    ui::egm(&format!("{} open question(s); the elephant ({}) answers what the repository settles and escalates the rest…", open.len(), agent.name()));
    let reply = agent.run(Turn {
        prompt: crate::escalate::prompt(&open),
        session: feature.elephant_session.clone(),
        access: Access::ReadOnly,
        role: Role::Escalate,
        doc: Some(doc.path.clone()),
        workdir: view_dir(&view),
    });
    if let Ok(r) = &reply {
        note_view_exit(&view, &r.touched);
    }
    let reply = match reply {
        Ok(r) => r,
        Err(e) => {
            drop(act); // cancels the activity; the turn was still charged
            return Err(e);
        }
    };
    if feature.elephant_session.is_none() {
        feature.elephant_session = reply.session_id.clone();
    }
    // The gate does not move: escalation produces no gate event.
    feature.gate = act.finish(None)?;
    p.save_feature(&feature)?;
    record(p, slug, "elephant", agent.as_ref(), &reply.usage)?;
    let parsed = goldfish::parse::<crate::escalate::Reply>(&reply.text);
    if parsed.is_none() {
        ui::warn("the elephant gave no usable answer; every question goes to you");
    }
    let rec = crate::escalate::build_record(slug, doc.hash(), feature.goldfish_runs, &open, parsed.as_ref(), &p.root);
    save_decisions(p, &rec)?;
    p.append_ledger(json!({
        "kind": "escalate", "slug": slug, "run": feature.goldfish_runs, "doc_hash": hash,
        "questions": rec.items.len(), "answered_from_repo": rec.items.len() - rec.pending().len(), "escalated": rec.pending().len(), "ts": now_secs(),
    }))?;
    show_decisions(&rec);
    if rec.pending().is_empty() {
        ui::ok(&format!("the repository settles every question; write them into the doc with `egm-dsh redraft {slug}`"));
    } else {
        ui::egm(&format!(
            "{} decision(s) for you: `egm-dsh decide {slug} <#> <option>` or `egm-dsh decide {slug} <#> --other \"...\"`, then `egm-dsh redraft {slug}`",
            rec.pending().len()
        ));
    }
    Ok(())
}

/// `egm-dsh decide <slug> [<#> <option> | <#> --other TEXT]`: record a person's choice.
/// Without arguments, lists the decisions. No model, no gate event, no budget.
pub fn decide(p: &Project, slug: &str, id: Option<usize>, key: Option<&str>, other: Option<&str>) -> anyhow::Result<()> {
    let mut rec = load_decisions(p, slug)?;
    if rec.applied_hash.is_some() {
        bail!("these decisions are already in the doc; run `egm-dsh goldfish {slug}`, and `egm-dsh escalate {slug}` if it is still not ready");
    }
    let Some(id) = id else {
        show_decisions(&rec);
        return Ok(());
    };
    let text = crate::escalate::decide(&mut rec, id, key, other, now_secs())?;
    save_decisions(p, &rec)?;
    ui::ok(&format!("#{id}: {text}"));
    let left = rec.pending().len();
    if left == 0 {
        ui::egm(&format!("every question is settled; next: egm-dsh redraft {slug}"));
    } else {
        ui::egm(&format!("{left} decision(s) still open"));
    }
    Ok(())
}

/// `egm-dsh redraft <slug>`: write the settled answers into the doc (`## Decisions`)
/// and have the elephant redraft the sections they concern. The new revision is
/// back in design: only a fresh goldfish pass unlocks `build`.
pub fn redraft(p: &Project, slug: &str) -> anyhow::Result<()> {
    let mut feature = p.load_feature(slug)?;
    let doc = p.sync(&mut feature)?;
    let mut rec = load_decisions(p, slug)?;
    if rec.applied_hash.is_some() {
        bail!("these decisions are already in the doc; run `egm-dsh goldfish {slug}`");
    }
    if rec.doc_hash != format!("{:016x}", doc.hash()) {
        bail!("the doc changed since these questions were escalated; run `egm-dsh goldfish {slug}` again, then `egm-dsh escalate {slug}`");
    }
    if !crate::escalate::can_redraft(&rec.items) {
        let ids: Vec<String> = rec.pending().iter().map(|i| format!("#{}", i.id)).collect();
        bail!("refused: {} still need(s) your decision (`egm-dsh decide {slug}` lists them)", ids.join(", "));
    }
    let Some(session) = feature.elephant_session.clone() else { bail!("no elephant session for `{slug}`; run `egm-dsh escalate {slug}` first") };
    let act = p.begin(slug, feature.gate, crate::run::Kind::Elephant)?;
    let agent = p.agent_for(RoleName::Elephant);
    // 1. The decisions themselves go into the doc, so a goldfish sees them only as doc text.
    let with_log = doc.with_section("Decisions", &crate::escalate::decisions_section(doc.section("Decisions").as_deref(), &rec));
    std::fs::write(&doc.path, with_log)?;
    // 2. The elephant redrafts the sections the questions concern: several in one turn,
    //    then any the reply left out one at a time.
    let mut names = crate::escalate::sections_to_redraft(&rec);
    let view = elephant_view(p, &feature)?;
    let words = p.config.draft_words;
    let all_names = names.clone();
    if names.len() > 1 {
        ui::egm(&format!("elephant redrafts {} with the settled answers, in one turn…", names.iter().map(|n| format!("`## {n}`")).collect::<Vec<_>>().join(", ")));
        match agent.run(Turn {
            prompt: drafting(p, crate::escalate::redraft_all_prompt(&names, &rec), words),
            session: Some(session.clone()),
            access: Access::ReadOnly,
            role: Role::DraftAll,
            doc: Some(doc.path.clone()),
            workdir: view_dir(&view),
        }) {
            Ok(reply) => {
                note_view_exit(&view, &reply.touched);
                record(p, slug, "elephant", agent.as_ref(), &reply.usage)?;
                let got = doc::extract_sections(&reply.text, &names);
                let current = p.doc(&feature)?;
                let mut text = current.text.clone();
                for (name, sec) in names.iter().zip(&got) {
                    if let Some(sec) = sec {
                        text = Doc { path: current.path.clone(), text }.with_section(name, sec);
                    }
                }
                std::fs::write(&current.path, text)?;
                names = names.into_iter().zip(got).filter(|(_, g)| g.is_none()).map(|(n, _)| n).collect();
            }
            Err(e) => {
                let now = p.doc(&feature)?;
                feature.gate = act.finish(Some(Event::EditDoc(now.hash())))?;
                p.save_feature(&feature)?;
                return Err(e);
            }
        }
    }
    for name in names {
        ui::egm(&format!("elephant redrafts `## {name}` with the settled answers…"));
        let reply = match agent.run(Turn {
            prompt: drafting(p, crate::escalate::redraft_prompt(name, &rec), words),
            session: Some(session.clone()),
            access: Access::ReadOnly,
            role: Role::Draft(name),
            doc: Some(doc.path.clone()),
            workdir: view_dir(&view),
        }) {
            Ok(r) => r,
            Err(e) => {
                // The doc may be half redrafted: fold whatever it now says into the gate.
                let now = p.doc(&feature)?;
                feature.gate = act.finish(Some(Event::EditDoc(now.hash())))?;
                p.save_feature(&feature)?;
                return Err(e);
            }
        };
        record(p, slug, "elephant", agent.as_ref(), &reply.usage)?;
        let current = p.doc(&feature)?;
        std::fs::write(&current.path, current.with_section(name, &doc::extract_section(&reply.text, name)))?;
    }
    let new_doc = p.doc(&feature)?;
    feature.gate = act.finish(Some(Event::EditDoc(new_doc.hash())))?;
    p.save_feature(&feature)?;
    rec.applied_hash = Some(format!("{:016x}", new_doc.hash()));
    save_decisions(p, &rec)?;
    p.append_ledger(json!({"kind": "redraft", "slug": slug, "from": rec.doc_hash, "to": rec.applied_hash, "ts": now_secs()}))?;
    ui::ok(&format!("saved {}: the decisions are in `## Decisions` and the sections they concern", feature.doc));
    if let Ok(d) = p.doc(&feature) {
        report_words(&d, &all_names, words);
    }
    let phase = format!("{:?}", feature.gate.phase).to_lowercase();
    ui::egm(&format!("phase: {phase}. Only a fresh goldfish pass on this revision unlocks the build: egm-dsh goldfish {slug}"));
    Ok(())
}

// ---------------------------------------------------------------------------------------
// loop: goldfish → escalate → redraft, stopping at people

/// How far the last review got: 0 precheck, 1 readiness, 2 every stage.
fn depth(p: &Project, slug: &str) -> anyhow::Result<u8> {
    Ok(match last_review(p, slug)?.stopped_at.as_deref() {
        Some("precheck") => 0,
        Some(_) => 1,
        None => 2,
    })
}

/// For the tester: the questions the verifier dropped because a criterion's check
/// catches a wrong guess. The tests must then really catch it.
fn caught_hint(p: &Project, slug: &str) -> String {
    let Ok(review) = last_review(p, slug) else { return String::new() };
    let items: Vec<String> = review.caught().iter().map(|a| format!("- criterion {}: {}", a.caught_by.unwrap_or(0), a.text.trim())).collect();
    if items.is_empty() {
        return String::new();
    }
    format!("\n\nThe design review left these questions to the tests: a wrong answer must make the named criterion's test fail. Write each test so it does (for example, drive the command line rather than a helper when the criterion is about a command):\n{}", items.join("\n"))
}

fn last_review(p: &Project, slug: &str) -> anyhow::Result<goldfish::Review> {
    let f = p.load_feature(slug)?;
    let path = Project::egm_dir(&p.root).join("reviews").join(format!("{slug}-{}.json", f.goldfish_runs));
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn open_count(p: &Project, slug: &str) -> anyhow::Result<usize> {
    let f = p.load_feature(slug)?;
    let path = Project::egm_dir(&p.root).join("reviews").join(format!("{slug}-{}.json", f.goldfish_runs));
    let review: goldfish::Review = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(review.open_alarms().len())
}

/// `egm-dsh loop <slug>`: rounds of a fresh goldfish, then `escalate`. It redrafts on its
/// own only when every question was answered from the repository (quotes checked by
/// egm-dsh); it stops for any decision a person must make, when the findings stop
/// falling, at a budget refusal, or at READY. It never opens the gate itself: only a
/// goldfish pass does.
pub fn loop_(p: &Project, slug: &str, max_rounds: u32, stall: usize, verify: bool, fail_fast: bool) -> anyhow::Result<()> {
    let mut history: Vec<usize> = Vec::new();
    let mut last_depth = 0;
    for round in 1..=max_rounds {
        ui::banner(&format!("loop `{slug}`: round {round} of at most {max_rounds}"));
        // Decisions a person made since the last run: write them in before the next goldfish.
        if let Ok(rec) = load_decisions(p, slug) {
            let mut feature = p.load_feature(slug)?;
            let doc = p.sync(&mut feature)?;
            if rec.applied_hash.is_none() && rec.doc_hash == format!("{:016x}", doc.hash()) {
                if crate::escalate::can_redraft(&rec.items) {
                    ui::egm("your decisions are in; redrafting before the next goldfish");
                    redraft(p, slug)?;
                } else {
                    show_decisions(&rec);
                    ui::egm(&format!("stopped: {} decision(s) still need you (`egm-dsh decide {slug} <#> <option>`), then `egm-dsh loop {slug}` again", rec.pending().len()));
                    return Ok(());
                }
            }
        }
        match goldfish(p, slug, false, verify, fail_fast)? {
            Verdict::Ready => {
                ui::ok(&format!("loop done after {round} round(s): READY — `egm-dsh build {slug}` is unlocked"));
                return Ok(());
            }
            Verdict::Incomplete => bail!("loop stopped: a reviewer failed (INCOMPLETE); rerun `egm-dsh loop {slug}`"),
            Verdict::NotReady => {}
        }
        // A deeper fail-fast round (precheck → readiness → all stages) finds new kinds of
        // finding, so counts from shallower rounds are not compared with it.
        let d = depth(p, slug)?;
        if d > last_depth {
            history.clear();
        }
        last_depth = d;
        history.push(open_count(p, slug)?);
        escalate(p, slug)?;
        let rec = load_decisions(p, slug)?;
        if crate::escalate::stalled(&history, stall) {
            ui::warn(&format!(
                "stopped: the open findings stopped falling over the last {stall} round(s) ({}); the remaining questions need you",
                history.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" → ")
            ));
            if crate::escalate::auto_redraft(&rec.items) {
                ui::egm(&format!("every one was answered from the repository, yet the goldfish still asks: decide what the doc should say with `egm-dsh elephant {slug} --say ...`"));
            }
            return Ok(());
        }
        if crate::escalate::auto_redraft(&rec.items) {
            ui::egm("every question was answered from the repository (egm-dsh checked each quote); redrafting without you");
            redraft(p, slug)?;
            continue;
        }
        ui::egm(&format!("stopped: {} decision(s) need you: `egm-dsh decide {slug} <#> <option>`, then `egm-dsh loop {slug}` again", rec.pending().len()));
        return Ok(());
    }
    bail!("loop stopped: no goldfish pass after {max_rounds} round(s) ({}); raise `--max-rounds` or look at the open questions", history.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" → "))
}
