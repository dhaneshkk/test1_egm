//! `egm-dsh gate`, `mutate`, `evidence`, `risk`, `log-time` and `ci-template`:
//! the evidence a pull request carries, checked by egm-dsh rather than written by
//! the agent that wrote the code. The pure logic lives in `evidence.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::doc::Doc;
use crate::evidence::{self, Coverage, Event, GateCheck, MutationResult, Tier};
use crate::phase::Phase;
use crate::project::{Feature, Project};
use crate::spec::{self, Check};
use crate::ui;

/// What `gate` found for one feature, saved to `.egm-dsh/evidence/<slug>.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Record {
    pub feature: String,
    pub doc: String,
    pub doc_hash: u64,
    pub checks: Vec<GateCheck>,
    pub passed: bool,
    pub mutation: Option<MutationResult>,
    pub coverage: Option<Coverage>,
    /// file → changed lines (from git, or every line when git has no build commit).
    pub changed: BTreeMap<String, Vec<usize>>,
    pub changed_from_git: bool,
    pub tier: Option<Tier>,
    pub risk_reasons: Vec<String>,
    pub reviewer: String,
    pub human_minutes: f64,
    pub logged_minutes: f64,
    pub churn: Churn,
    /// Per criterion: text, check, red before the build, passes now.
    pub criteria: Vec<(String, String, String, String)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Churn {
    pub design_commits: usize,
    pub build_commits: usize,
    pub goldfish_rounds: u32,
    /// Files this feature shares with other features' Implementation maps.
    pub shared_files: Vec<String>,
}

fn is_test_file(path: &str, frozen: &BTreeSet<String>) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    frozen.contains(path) || path.starts_with("tests/") || path.contains("/tests/") || name.starts_with("test_") || name.ends_with("_test.rs") || name.ends_with(".test.ts")
}

/// The feature's code files: its Implementation map, without docs and tests.
fn code_files(doc: &Doc, frozen: &BTreeSet<String>) -> Vec<String> {
    doc.implementation_map().into_iter().filter(|f| !f.ends_with(".md") && !is_test_file(f, frozen)).collect()
}

/// Changed lines of the code files: the build commit against the design commit,
/// or every line when there is no such pair of commits (said so in the record).
fn changed_lines(p: &Project, f: &Feature, code: &[String]) -> (BTreeMap<String, BTreeSet<usize>>, bool) {
    let design = crate::vcs::last_commit_with(&p.root, &format!("egm-dsh design {}:", f.slug));
    let build = crate::vcs::last_commit_with(&p.root, &format!("egm-dsh build {}:", f.slug));
    if let (Some(d), Some(b)) = (design, build)
        && let Ok(diff) = crate::vcs::diff_unified0(&p.root, &d, &b, code) {
            return (evidence::parse_diff(&diff), true);
        }
    let mut all = BTreeMap::new();
    for c in code {
        if let Ok(t) = std::fs::read_to_string(p.root.join(c)) {
            all.insert(c.clone(), (1..=t.lines().count()).collect());
        }
    }
    (all, false)
}

fn frozen_tests(f: &Feature) -> BTreeSet<String> {
    f.spec.as_ref().map(|s| s.test_files.keys().cloned().collect()).unwrap_or_default()
}

/// ", N timed out (not counted)" when any seeded bug ran past the timeout.
fn timeouts_note(m: &MutationResult) -> String {
    if m.timed_out > 0 { format!(", {} timed out (not counted)", m.timed_out) } else { String::new() }
}

/// The commands whose failure kills a mutant: each frozen test, then acceptance.
fn kill_commands(p: &Project, f: &Feature) -> Vec<Vec<String>> {
    let mut cmds: Vec<Vec<String>> = Vec::new();
    if let Some(s) = &f.spec {
        for c in &s.criteria {
            if let Check::Test { file, id } = spec::parse_check(&c.check) {
                let cmd = spec::test_command(&p.config.check_cmd, &file, id.as_deref());
                if !cmds.contains(&cmd) {
                    cmds.push(cmd);
                }
            }
        }
    }
    if !p.config.acceptance_cmd.is_empty() {
        cmds.push(p.config.acceptance_cmd.clone());
    }
    cmds
}

pub fn mutation(p: &Project, f: &Feature, changed: &BTreeMap<String, BTreeSet<usize>>, verbose: bool) -> anyhow::Result<MutationResult> {
    let cmds = kill_commands(p, f);
    let files: BTreeMap<String, String> =
        changed.keys().filter_map(|c| std::fs::read_to_string(p.root.join(c)).ok().map(|t| (c.clone(), t))).collect();
    let (mutants, available) = evidence::select_mutants(&files, changed, p.config.gate.mutants);
    let mut r = MutationResult { available, ..Default::default() };
    if cmds.is_empty() || mutants.is_empty() {
        return Ok(r);
    }
    let configured = Duration::from_secs(p.config.gate.mutant_timeout_secs);
    let cache = crate::safety::mutant_cache_dir(&p.root);
    // Warm the shared build cache first, with a generous limit: a cold build is not a hang.
    let warm = configured.max(Duration::from_secs(60)) * 10;
    match evidence::passes_in_copy(&p.config.safety, &p.root, &[], &cmds, warm, Some(&cache))? {
        Ok(true) => {}
        Ok(false) => {
            r.problem = Some("the frozen tests or the acceptance command fail with no seeded bug, so no seeded bug can be judged".into());
            return Ok(r);
        }
        Err(()) => {
            r.problem = Some(format!("the unmutated checks did not finish in {} s, even allowing for a cold build", warm.as_secs()));
            return Ok(r);
        }
    }
    // Then time the unmutated checks with the warm cache: the baseline for the timeout.
    let start = Instant::now();
    match evidence::passes_in_copy(&p.config.safety, &p.root, &[], &cmds, configured, Some(&cache))? {
        Ok(true) => r.baseline_secs = Some(start.elapsed().as_secs_f64()),
        Ok(false) => {
            r.problem = Some("the frozen tests or the acceptance command fail on a second run with no seeded bug (flaky checks?), so no seeded bug can be judged".into());
            return Ok(r);
        }
        Err(()) => {
            r.problem = Some(format!(
                "the unmutated checks take longer than the seeded-bug timeout ({} s) even with a warm build cache: raise it with `egm-dsh set-gate --mutant-timeout SECS`, or make the acceptance command faster",
                configured.as_secs()
            ));
            return Ok(r);
        }
    }
    let timeout = evidence::mutant_timeout(configured, start.elapsed());
    if verbose {
        ui::egm(&format!("unmutated checks: {:.1} s; each seeded bug may run {} s", r.baseline_secs.unwrap_or(0.0), timeout.as_secs()));
    }
    for (k, m) in mutants.iter().enumerate() {
        let text = evidence::apply(&files[&m.file], m);
        let verdict = evidence::passes_in_copy(&p.config.safety, &p.root, &[(m.file.clone(), text)], &cmds, timeout, Some(&cache))?;
        r.tried += 1;
        match verdict {
            Ok(true) => r.survivors.push(m.clone()),
            Ok(false) => r.killed += 1,
            // a timeout is not a catch: it is reported apart and left out of the score
            Err(()) => r.timed_out += 1,
        }
        if verbose {
            let mark = match verdict {
                Ok(true) => "survived",
                Ok(false) => "caught",
                Err(()) => "timed out (not counted)",
            };
            ui::egm(&format!("seeded bug {}/{}: {}:{} {} — {mark}", k + 1, mutants.len(), m.file, m.line, m.op));
        }
    }
    Ok(r)
}

/// Every evidence check for one built feature.
pub fn check(p: &Project, slug: &str, with_mutation: bool, verbose: bool) -> anyhow::Result<Record> {
    let mut f = p.load_feature(slug)?;
    let doc = p.sync(&mut f)?;
    let frozen = frozen_tests(&f);
    let code = code_files(&doc, &frozen);
    let mut checks: Vec<GateCheck> = Vec::new();
    let mut add = |name: &str, ok: bool, detail: String| checks.push(GateCheck { name: name.into(), ok, detail });

    let built = matches!(f.gate.phase, Phase::Built | Phase::Hardened);
    add("built", built, format!("phase: {:?}", f.gate.phase).to_lowercase());
    let same_doc = f.built_doc_hash == Some(doc.hash());
    add("design unchanged since the build", same_doc, if same_doc { "the doc is the revision that was built".into() } else { "the doc changed after the build; run the goldfish and build again".into() });

    // drift: the map's files as built
    let now = p.fingerprint();
    let all = p.features()?;
    let drifted: Vec<String> = doc
        .implementation_map()
        .into_iter()
        .filter(|path| crate::steps::handed_over(&f, path, &all).is_none())
        .filter(|path| match (now.get(path), f.built_files.get(path)) {
            (None, _) => true,
            (Some(a), Some(b)) => a != b,
            _ => false,
        })
        .collect();
    add("no drift", drifted.is_empty(), if drifted.is_empty() { "every file matches the build".into() } else { format!("changed since the build: {}", drifted.join(", ")) });

    // frozen tests and Lean statements: present, untouched
    match &f.spec {
        None => add("criteria frozen before the build", false, "no frozen tests: run `egm-dsh tests` before the build (or turn on tests first)".into()),
        Some(s) => {
            let mut changed: Vec<String> = Vec::new();
            for (file, h) in &s.test_files {
                if std::fs::read_to_string(p.root.join(file)).map(|t| crate::doc::hash(&t)).ok() != Some(*h) {
                    changed.push(file.clone());
                }
            }
            for (file, (thm, h)) in &s.lean_files {
                let part = std::fs::read_to_string(p.root.join(file)).ok().and_then(|t| spec::lean_frozen_part(&t, thm));
                if part.map(|t| crate::doc::hash(&t)) != Some(*h) {
                    changed.push(format!("{file} (outside the proof of {thm})"));
                }
            }
            add("frozen tests and Lean statements untouched", changed.is_empty(), if changed.is_empty() {
                format!("{} test file(s), {} Lean statement(s) as frozen", s.test_files.len(), s.lean_files.len())
            } else {
                format!("changed after freezing: {}", changed.join(", "))
            });
            // git: the criteria were committed before the code
            if crate::vcs::is_repo(&p.root) {
                let design = crate::vcs::last_commit_with(&p.root, &format!("egm-dsh design {slug}:"));
                let build = crate::vcs::last_commit_with(&p.root, &format!("egm-dsh build {slug}:"));
                let tests: Vec<String> = s.test_files.keys().cloned().chain(s.lean_files.keys().cloned()).collect();
                let (ok, detail) = match (&design, &build) {
                    (Some(d), Some(b)) => {
                        let touched = crate::vcs::changed_between(&p.root, d, b, &tests);
                        let before = crate::vcs::is_ancestor(&p.root, d, b);
                        (before && touched.is_empty(), if !before {
                            "the design commit is not an ancestor of the build commit".to_string()
                        } else if !touched.is_empty() {
                            format!("the build commit changed {}", touched.join(", "))
                        } else {
                            format!("criteria in {}, code in {}", &d[..d.len().min(8)], &b[..b.len().min(8)])
                        })
                    }
                    _ => (false, "no egm-dsh design and build commits for this feature".to_string()),
                };
                add("criteria committed before the code (git)", ok, detail);
            }
        }
    }

    // every check passes now
    let mut rows = Vec::new();
    let (mut exe, mut pass, mut manual) = (0, 0, 0);
    let mut failing = Vec::new();
    for c in doc.criteria() {
        let check_s = c.check.clone().unwrap_or_default();
        let red = f.spec.as_ref().and_then(|s| s.criteria.iter().find(|x| x.text == c.text)).map(|x| x.red.clone()).unwrap_or_else(|| "—".into());
        let now_s = match spec::parse_check(&check_s) {
            _ if check_s.is_empty() => "no check".to_string(),
            Check::Manual => {
                manual += 1;
                "manual".into()
            }
            Check::Test { file, id } => {
                exe += 1;
                match spec::run_test(&p.config.safety, &p.root, &p.config.check_cmd, &file, id.as_deref()) {
                    Ok(true) => {
                        pass += 1;
                        "pass".into()
                    }
                    Ok(false) => {
                        failing.push(check_s.clone());
                        "FAIL".into()
                    }
                    Err(e) => {
                        failing.push(format!("{check_s} (could not run: {e})"));
                        format!("could not run: {e}")
                    }
                }
            }
            Check::Lean { file, theorem } => {
                exe += 1;
                match spec::lean_check(&p.config.safety, &p.root, &p.config.lean_cmd, &file, &theorem) {
                    Ok(()) => {
                        pass += 1;
                        "proved (no sorry, no new axioms)".into()
                    }
                    Err(e) => {
                        failing.push(check_s.clone());
                        format!("FAIL: {e}")
                    }
                }
            }
        };
        rows.push((c.text.clone(), check_s, red, now_s));
    }
    failing.dedup();
    add("every check passes now", exe > 0 && pass == exe, format!("{pass}/{exe} executable check(s) pass{}{}", if manual > 0 { format!(", {manual} manual") } else { String::new() },
        if failing.is_empty() { String::new() } else { format!("; failing: {}", failing.join(", ")) }));
    if !p.config.acceptance_cmd.is_empty() {
        let out = crate::safety::run_in_copy(&p.config.safety, &p.root, &p.config.acceptance_cmd);
        let ok = out.as_ref().is_ok_and(|o| o.status.success());
        add("acceptance passes", ok, p.config.acceptance_cmd.join(" "));
    }

    let (changed, from_git) = changed_lines(p, &f, &code);
    let mut rec = Record { feature: slug.into(), doc: f.doc.clone(), doc_hash: doc.hash(), criteria: rows, changed_from_git: from_git, ..Default::default() };
    rec.changed = changed.iter().map(|(k, v)| (k.clone(), v.iter().copied().collect())).collect();

    if with_mutation {
        let m = mutation(p, &f, &changed, verbose)?;
        let min = p.config.gate.mutation_min;
        let ok = m.passes(min);
        let detail = match (&m.problem, m.score()) {
            (Some(why), _) => why.clone(),
            (None, None) if m.available == 0 => "no mutable lines in the change".to_string(),
            (None, None) if m.timed_out > 0 => format!("all {} seeded bugs timed out, so none could be judged", m.timed_out),
            (None, None) => "no tests to run against the seeded bugs".to_string(),
            (None, Some(s)) => format!("caught {}/{} seeded bugs ({:.0}%; minimum {min}%){}{}", m.killed, m.judged(), s * 100.0,
                if m.available > m.tried { format!(", sampled from {}", m.available) } else { String::new() },
                timeouts_note(&m)),
        };
        // a change with nothing to mutate (only data or docs) is not a failure
        let ok = ok || (m.available == 0 && built);
        checks.push(GateCheck { name: "seeded bugs caught (mutation score)".into(), ok, detail });
        rec.mutation = Some(m);
    }
    rec.passed = evidence::gate_passes(&checks);
    rec.checks = checks;

    // coverage of the changed lines, when a coverage command is configured
    if !p.config.gate.coverage_cmd.is_empty()
        && let Some(lcov) = run_coverage(p)? {
            rec.coverage = Some(evidence::coverage_of(&changed, &evidence::parse_lcov(&lcov)));
        }

    // risk and routing
    let lines_changed: usize = changed.values().map(BTreeSet::len).sum();
    let map = doc.implementation_map();
    let mut prot_pats = p.config.fix.protected.clone();
    prot_pats.extend(p.config.gate.risk_paths.iter().cloned());
    let prot = crate::scope::parse(&prot_pats.join("\n"));
    let protected: Vec<String> = map.iter().filter(|m| crate::scope::ignored(&prot, m, false)).cloned().collect();
    let manifests: Vec<String> = map.iter().filter(|m| crate::fix::MANIFESTS.contains(&m.rsplit('/').next().unwrap_or(m))).cloned().collect();
    let input = evidence::RiskInput {
        files_changed: changed.len(),
        lines_changed,
        protected,
        manifests,
        uncovered_lines: rec.coverage.as_ref().map(|c| c.uncovered.values().map(Vec::len).sum()).unwrap_or(0),
        survivors: rec.mutation.as_ref().map(|m| m.survivors.len()).unwrap_or(0),
        gate_ok: rec.passed,
        max_files: p.config.fix.max_files,
        max_lines: p.config.fix.max_lines,
    };
    let (tier, why) = evidence::risk(&input);
    let owners = std::fs::read_to_string(p.root.join(".github/CODEOWNERS"))
        .or_else(|_| std::fs::read_to_string(p.root.join("CODEOWNERS")))
        .map(|t| evidence::codeowners(&t, &changed.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default();
    rec.reviewer = evidence::routing(tier, &owners);
    rec.tier = Some(tier);
    rec.risk_reasons = why;

    let events = load_events(p);
    let (gaps, logged) = evidence::human_minutes(&events, slug);
    rec.human_minutes = gaps;
    rec.logged_minutes = logged;
    rec.churn = Churn {
        design_commits: count_commits(&p.root, &format!("egm-dsh design {slug}:")),
        build_commits: count_commits(&p.root, &format!("egm-dsh build {slug}:")),
        goldfish_rounds: f.goldfish_runs,
        shared_files: all
            .iter()
            .filter(|g| g.slug != slug)
            .filter_map(|g| Doc::load(&p.root.join(&g.doc)).ok())
            .flat_map(|d| d.implementation_map())
            .filter(|x| code.contains(x))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    };
    let dir = Project::egm_dir(&p.root).join("evidence");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{slug}.json")), serde_json::to_string_pretty(&rec)?)?;
    Ok(rec)
}

fn count_commits(root: &Path, prefix: &str) -> usize {
    std::process::Command::new("git")
        .args(["log", "--all", "--format=%H", &format!("--grep=^{prefix}")])
        .current_dir(root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}

/// Run the coverage command in a scratch copy and read back its lcov file.
fn run_coverage(p: &Project) -> anyhow::Result<Option<String>> {
    let mode = crate::safety::effective(&p.config.safety)?;
    let dir = crate::safety::scratch_copy(&p.root, "coverage", &[])?;
    let (prog, args) = p.config.gate.coverage_cmd.split_first().expect("non-empty");
    let out = crate::safety::wrap(mode, &crate::safety::allowed_env(&p.config.safety), &dir, prog, args).output();
    let lcov = std::fs::read_to_string(dir.join(&p.config.gate.coverage_file)).ok();
    let _ = std::fs::remove_dir_all(&dir);
    match (out, lcov) {
        (Ok(o), Some(l)) if o.status.success() => Ok(Some(l)),
        (Ok(o), _) => {
            ui::warn(&format!("coverage: `{}` exited {:?} or wrote no `{}`; coverage not measured", p.config.gate.coverage_cmd.join(" "), o.status.code(), p.config.gate.coverage_file));
            Ok(None)
        }
        (Err(e), _) => {
            ui::warn(&format!("coverage: cannot run `{prog}`: {e}"));
            Ok(None)
        }
    }
}

fn print_checks(rec: &Record) {
    for c in &rec.checks {
        if c.ok {
            ui::ok(&format!("{} — {}", c.name, c.detail));
        } else {
            ui::warn(&format!("{} — {}", c.name, c.detail));
        }
    }
}

/// `egm-dsh gate [FEATURE…] [--all] [--no-mutation]`: exit 1 unless every
/// feature's evidence holds. Needs no model and no API key: CI can run it.
pub fn gate(p: &Project, slugs: &[String], all: bool, with_mutation: bool) -> anyhow::Result<bool> {
    let list: Vec<String> = if all || slugs.is_empty() {
        p.features()?.into_iter().filter(|f| matches!(f.gate.phase, Phase::Built | Phase::Hardened)).map(|f| f.slug).collect()
    } else {
        slugs.to_vec()
    };
    if list.is_empty() {
        bail!("no built feature to check");
    }
    let mut ok = true;
    for slug in &list {
        ui::egm(&format!("evidence gate: {slug}"));
        let rec = check(p, slug, with_mutation, false)?;
        crate::events::emit("gate.verdict", slug, serde_json::json!({
            "pass": rec.passed,
            "doc_hash": rec.doc_hash.to_string(),
            "checks": rec.checks.iter().map(|c| serde_json::json!({"name": c.name, "ok": c.ok})).collect::<Vec<_>>(),
            "mutation": rec.mutation.as_ref().map(|m| serde_json::json!({"killed": m.killed, "tried": m.tried, "timed_out": m.timed_out})),
            "tier": rec.tier,
        }));
        print_checks(&rec);
        if let Some(m) = &rec.mutation {
            for s in m.survivors.iter().take(10) {
                ui::block(&format!("  survived: {}:{} {}  →  {}", s.file, s.line, s.op, s.mutated_line.trim()));
            }
        }
        if rec.passed {
            ui::ok(&format!("{slug}: evidence gate passed"));
        } else {
            ui::warn(&format!("{slug}: evidence gate FAILED"));
            ok = false;
        }
    }
    Ok(ok)
}

/// `egm-dsh mutate FEATURE`: the mutation run alone, with every seeded bug shown.
pub fn mutate(p: &Project, slug: &str) -> anyhow::Result<bool> {
    let mut f = p.load_feature(slug)?;
    let doc = p.sync(&mut f)?;
    let frozen = frozen_tests(&f);
    let code = code_files(&doc, &frozen);
    let (changed, from_git) = changed_lines(p, &f, &code);
    if !from_git {
        ui::egm("no egm-dsh design/build commits for this feature: seeding bugs in every line of its code files");
    }
    let m = mutation(p, &f, &changed, true)?;
    let min = p.config.gate.mutation_min;
    if let Some(why) = &m.problem {
        ui::warn(why);
        return Ok(false);
    }
    match m.score() {
        None if m.timed_out > 0 => {
            ui::warn(&format!("all {} seeded bugs timed out, so none could be judged", m.timed_out));
            Ok(false)
        }
        None => {
            ui::warn("nothing to measure: no mutable changed lines, or no frozen tests or acceptance command to run");
            Ok(false)
        }
        Some(s) => {
            let ok = m.passes(min);
            let msg = format!("caught {}/{} seeded bugs ({:.0}%; minimum {min}%){}", m.killed, m.judged(), s * 100.0, timeouts_note(&m));
            if ok {
                ui::ok(&msg);
            } else {
                ui::warn(&msg);
            }
            Ok(ok)
        }
    }
}

/// Markdown for a pull request: what the doc promised, what checks it, what ran,
/// what no test covers, who should review. The marker on the last line lets
/// `bench/review_metrics.py` score the gate against post-merge defects.
pub fn render(rec: &Record, title: &str) -> String {
    let mut s = String::new();
    let tick = |b: bool| if b { "✅" } else { "❌" };
    s += &format!("## Evidence: {title}\n\nDesign doc: [`{}`]({}) (revision `{:016x}`), reviewed by fresh goldfish before any code; built by egm-dsh.\n\n", rec.doc, rec.doc, rec.doc_hash);
    s += &format!("### Gate: {}\n\n| Check | | Detail |\n|---|---|---|\n", if rec.passed { "passed" } else { "FAILED" });
    for c in &rec.checks {
        s += &format!("| {} | {} | {} |\n", c.name, tick(c.ok), c.detail.replace('|', "\\|"));
    }
    s += "\n### Acceptance criteria\n\n| # | Criterion | Check | Before the build | Now |\n|---|---|---|---|---|\n";
    for (i, (t, c, red, now)) in rec.criteria.iter().enumerate() {
        let t = t.split(" Check:").next().unwrap_or(t).trim();   // the check has its own column
        s += &format!("| {} | {} | `{}` | {} | {} |\n", i + 1, t.replace('|', "\\|"), c, red, now);
    }
    let lines: usize = rec.changed.values().map(Vec::len).sum();
    s += &format!("\n### What changed\n\n{} file(s), {} changed line(s){}.\n\n", rec.changed.len(), lines, if rec.changed_from_git { " (design commit → build commit)" } else { " (no build commit: every line counted)" });
    match &rec.coverage {
        Some(c) => {
            s += &format!("Coverage of changed lines: {}/{} executable lines ran under the tests.\n", c.covered, c.executable);
            if c.uncovered.is_empty() {
                s += "\nEvery changed executable line ran under the tests.\n";
            } else {
                s += "\n**Not covered by any test — review these by hand:**\n\n";
                for (f, l) in &c.uncovered {
                    s += &format!("- `{f}`: lines {}\n", evidence::ranges(l));
                }
            }
        }
        None => s += "Coverage of changed lines: not measured (`egm-dsh set-gate --coverage-cmd …`).\n",
    }
    if let Some(m) = &rec.mutation {
        if let Some(why) = &m.problem {
            s += &format!("\n### Seeded bugs\n\nNot measured: {why}.\n");
        } else {
            s += &format!("\n### Seeded bugs\n\n{} of {} small bugs seeded in the changed lines made a test fail.\n", m.killed, m.judged());
            if m.timed_out > 0 {
                s += &format!("{} more ran past the timeout; a timeout is not counted as caught.\n", m.timed_out);
            }
        }
        if !m.survivors.is_empty() {
            s += "\n**No test caught these — each marks behaviour the tests do not pin down:**\n\n";
            for x in m.survivors.iter().take(15) {
                s += &format!("- `{}:{}` {} → `{}`\n", x.file, x.line, x.op, x.mutated_line.trim());
            }
        }
    }
    if let Some(t) = rec.tier {
        s += &format!("\n### Risk: {}\n\n", t.as_str());
        if rec.risk_reasons.is_empty() {
            s += "No risk signal: small, fully checked, nothing protected.\n";
        } else {
            for r in &rec.risk_reasons {
                s += &format!("- {r}\n");
            }
        }
        s += &format!("\nSuggested reviewer: {}.\n", rec.reviewer);
    }
    s += &format!(
        "\n### Cost of this change\n\nHuman time in egm-dsh (estimated from gaps between commands, each capped at {:.0} min): {:.0} min; logged by hand: {:.0} min. Goldfish rounds: {}; design commits: {}; build commits: {}.{}\n",
        evidence::GAP_CAP_MINUTES, rec.human_minutes, rec.logged_minutes, rec.churn.goldfish_rounds, rec.churn.design_commits, rec.churn.build_commits,
        if rec.churn.shared_files.is_empty() { String::new() } else { format!(" Shares {} with other features' maps.", rec.churn.shared_files.join(", ")) }
    );
    s += "\n### For the reviewer\n\n1. Is the evidence adequate for what the criteria promise?\n2. Are the uncovered lines and uncaught seeded bugs above acceptable?\n";
    s += &format!("\n<!-- egm-gate: {} -->\n", if rec.passed { "pass" } else { "fail" });
    s
}

/// `egm-dsh evidence FEATURE [--out FILE] [--no-mutation]`.
pub fn evidence_cmd(p: &Project, slug: &str, out: Option<&Path>, with_mutation: bool) -> anyhow::Result<bool> {
    let f = p.load_feature(slug)?;
    let rec = check(p, slug, with_mutation, false)?;
    let md = render(&rec, &f.title);
    match out {
        Some(path) => {
            std::fs::write(path, &md)?;
            ui::ok(&format!("evidence for `{slug}` written to {} (gate {})", path.display(), if rec.passed { "passed" } else { "FAILED" }));
        }
        None => crate::ui::out(md.to_string()),
    }
    Ok(rec.passed)
}

/// `egm-dsh risk FEATURE`.
pub fn risk_cmd(p: &Project, slug: &str) -> anyhow::Result<()> {
    let rec = check(p, slug, true, false)?;
    let t = rec.tier.unwrap_or(Tier::High);
    ui::egm(&format!("{slug}: risk {}", t.as_str()));
    for r in &rec.risk_reasons {
        ui::block(&format!("  · {r}"));
    }
    ui::egm(&format!("suggested reviewer: {}", rec.reviewer));
    if t == Tier::Low {
        ui::egm("a change this size can go through `egm-dsh fix` next time: one combined goldfish instead of the full panel");
    }
    Ok(())
}

// ---------------------------------------------------------------- events

fn events_path(p: &Project) -> std::path::PathBuf {
    Project::egm_dir(&p.root).join("events.jsonl")
}

pub fn load_events(p: &Project) -> Vec<Event> {
    std::fs::read_to_string(events_path(p)).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

pub fn record_event(root: &Path, e: &Event) {
    use std::io::Write;
    let path = Project::egm_dir(root).join("events.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", serde_json::to_string(e).unwrap_or_default());
    }
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `egm-dsh log-time FEATURE MINUTES --what …`: time spent outside the CLI
/// (reading the pull request, a meeting about the design).
pub fn log_time(p: &Project, slug: &str, minutes: f64, what: &str) -> anyhow::Result<()> {
    p.load_feature(slug)?;
    if !(minutes > 0.0 && minutes < 24.0 * 60.0) {
        bail!("minutes must be between 0 and 1440");
    }
    let t = now();
    record_event(&p.root, &Event { feature: slug.into(), cmd: format!("log-time: {what}"), start: t, end: t, logged_minutes: minutes });
    let (gaps, logged) = evidence::human_minutes(&load_events(p), slug);
    ui::ok(&format!("{slug}: logged {minutes:.0} min ({what}); human time so far: {gaps:.0} min in egm-dsh (estimated) + {logged:.0} min logged"));
    Ok(())
}

/// `egm-dsh ci-template`: a GitHub Actions workflow that runs the gate on every
/// pull request. The gate needs no model and no API key.
pub fn ci_template() -> String {
    r#"# .github/workflows/egm-gate.yml — written by `egm-dsh ci-template`
# Blocks a merge unless every built feature's evidence holds: design unchanged,
# no drift, frozen tests untouched and committed before the code, every check
# green, and enough seeded bugs caught. No model and no API key needed.
name: egm-dsh evidence gate
on:
  pull_request:
jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0          # the gate reads egm-dsh's design and build commits
      - uses: dtolnay/rust-toolchain@stable
      - name: Install egm-dsh
        # set the repository variable EGM_DSH_GIT to the git URL of your egm-dsh source
        run: cargo install --git "${{ vars.EGM_DSH_GIT }}" --locked egm-dsh
      - name: Evidence gate
        run: egm-dsh gate --all
      - name: Evidence bundle
        if: always()
        run: |
          for f in $(ls .egm-dsh/features 2>/dev/null | sed 's/\.json$//'); do
            egm-dsh evidence "$f" --no-mutation --out "evidence-$f.md" || true
          done
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: egm-evidence
          path: evidence-*.md
"#
    .to_string()
}
