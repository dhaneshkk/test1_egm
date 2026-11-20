//! Iterative development as a chain of small steps (features).
//!
//! Four rules, each mirrored in `proofs/EgmSteps.lean`:
//! 1. Size: a step over the configured limits gets an egm-dsh alarm, which no
//!    model can drop, so an oversized step never passes the goldfish.
//! 2. Phases: a step's build runs phase by phase, in order; the feature only
//!    becomes `built` after the last phase, and a doc edit resets progress.
//! 3. Handover: when a later step's build changes a file an earlier step also
//!    owns, drift checks that file against the later build. A file is handed
//!    over only when a later build lists it.
//! 4. Dependencies: a step builds only after every step it depends on is
//!    built; cycles are refused, and a cycle could never build anyway.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::doc::Doc;
use crate::phase::Phase;
use crate::project::Feature;

/// How big one step may be. Zero turns a limit off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StepLimits {
    /// Files in the Implementation map.
    pub max_files: usize,
    /// Items under Acceptance criteria.
    pub max_criteria: usize,
}

impl Default for StepLimits {
    fn default() -> Self {
        StepLimits { max_files: 6, max_criteria: 8 }
    }
}

/// Rule 1: one alarm text per limit the doc exceeds.
pub fn size_alarms(doc: &Doc, limits: StepLimits) -> Vec<String> {
    let mut out = Vec::new();
    let files = doc.implementation_map().len();
    if limits.max_files > 0 && files > limits.max_files {
        out.push(format!(
            "This step changes {files} files (limit {}). Split it into smaller steps chained with `--after`, or raise `step_limits.max_files` in .egm-dsh/config.json.",
            limits.max_files
        ));
    }
    let criteria = doc.criteria().len();
    if limits.max_criteria > 0 && criteria > limits.max_criteria {
        out.push(format!(
            "This step has {criteria} acceptance criteria (limit {}). Split it into smaller steps, or raise `step_limits.max_criteria`.",
            limits.max_criteria
        ));
    }
    out
}

/// Rule 2: may phase `k` (1-based) of `total` run after `done` phases?
/// Mirrors `phaseStep` in the Lean file.
pub fn phase_step(done: u32, total: u32, k: u32) -> Result<u32, String> {
    if k == done + 1 && k <= total {
        Ok(k)
    } else if k <= done {
        Err(format!("phase {k} is already built; the next is {}", done + 1))
    } else if k > total {
        Err(format!("the doc has {total} phase(s)"))
    } else {
        Err(format!("phases build in order; build phase {} first", done + 1))
    }
}

/// Rule 3: is `path`, built by `f`, handed over to a later build?
/// Mirrors `handedOver` in the Lean file: some feature built later lists it.
pub fn handed_over<'a>(f: &Feature, path: &str, all: &'a [Feature]) -> Option<&'a Feature> {
    let seq = f.built_seq.unwrap_or(0);
    all.iter()
        .filter(|g| g.slug != f.slug && g.built_seq.unwrap_or(0) > seq && g.built_files.contains_key(path))
        .max_by_key(|g| g.built_seq)
}

/// Rule 4: is the dependency graph with `slug -> deps` acyclic, and does every dependency exist?
pub fn check_deps(all: &[Feature], slug: &str, deps: &[String]) -> anyhow::Result<()> {
    let mut graph: BTreeMap<String, Vec<String>> = all.iter().map(|f| (f.slug.clone(), f.depends_on.clone())).collect();
    for d in deps {
        anyhow::ensure!(d != slug, "a step cannot depend on itself");
        anyhow::ensure!(graph.contains_key(d), "no feature `{d}`; create it first");
    }
    graph.insert(slug.to_string(), deps.to_vec());
    // A cycle through `slug` exists iff `slug` is reachable from one of its deps.
    let mut stack: Vec<&String> = deps.iter().collect();
    let mut seen = BTreeSet::new();
    while let Some(n) = stack.pop() {
        anyhow::ensure!(n != slug, "that would make a cycle: `{slug}` would (indirectly) depend on itself");
        if seen.insert(n.clone()) {
            stack.extend(graph.get(n).into_iter().flatten());
        }
    }
    Ok(())
}

/// Rule 4: dependencies of `f` that are not built yet.
pub fn unbuilt_deps(f: &Feature, all: &[Feature]) -> Vec<String> {
    f.depends_on
        .iter()
        .filter(|d| !all.iter().any(|g| &g.slug == *d && matches!(g.gate.phase, Phase::Built | Phase::Hardened)))
        .cloned()
        .collect()
}

/// Features in dependency order (Kahn's algorithm, ties by name). Features in
/// a cycle, which `check_deps` prevents, are appended at the end.
pub fn chain_order(all: &[Feature]) -> Vec<&Feature> {
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<&Feature> = Vec::new();
    loop {
        let next = all
            .iter()
            .filter(|f| !placed.contains(f.slug.as_str()))
            .find(|f| f.depends_on.iter().all(|d| placed.contains(d.as_str()) || !all.iter().any(|g| &g.slug == d)));
        match next {
            Some(f) => {
                placed.insert(&f.slug);
                out.push(f);
            }
            None => break,
        }
    }
    out.extend(all.iter().filter(|f| !placed.contains(f.slug.as_str())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::Gate;
    use std::path::PathBuf;

    fn feat(slug: &str, deps: &[&str], seq: Option<u64>, files: &[&str], phase: Phase) -> Feature {
        Feature {
            slug: slug.into(),
            title: slug.into(),
            idea: String::new(),
            doc: format!("docs/designs/{slug}.md"),
            elephant_session: None,
            gate: Gate { phase, doc_hash: 1, passed_hash: if phase == Phase::Designing { None } else { Some(1) } },
            goldfish_runs: 0,
            first_verdict: None,
            first_verdict_model: None,
            label: None,
            built_files: files.iter().map(|f| (f.to_string(), 7)).collect(),
            built_doc_hash: seq.map(|_| 1),
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            built_seq: seq,
            phases_done: 0,
            phase_doc_hash: None,
            spec: None,
            git_branch: None,
                lane: None,
        }
    }

    #[test]
    fn oversized_steps_raise_alarms() {
        let map: String = (1..=7).map(|i| format!("- `f{i}.py` — x\n")).collect();
        let doc = Doc { path: PathBuf::from("d.md"), text: format!("## Implementation map\n\n{map}\n## Acceptance criteria\n\n- a. Check: `t.py`\n") };
        let a = size_alarms(&doc, StepLimits::default());
        assert_eq!(a.len(), 1);
        assert!(a[0].contains("7 files (limit 6)"));
        assert!(size_alarms(&doc, StepLimits { max_files: 0, max_criteria: 0 }).is_empty());
    }

    #[test]
    fn phases_run_in_order_only() {
        assert_eq!(phase_step(0, 3, 1), Ok(1));
        assert!(phase_step(0, 3, 2).is_err());
        assert!(phase_step(1, 3, 1).is_err());
        assert!(phase_step(3, 3, 4).is_err());
        // exhaustive over small sizes: accepted k is always done + 1 and never past total
        for total in 0..5 {
            for done in 0..=total {
                for k in 0..7 {
                    if let Ok(n) = phase_step(done, total, k) {
                        assert!(n == done + 1 && n <= total);
                    }
                }
            }
        }
    }

    #[test]
    fn later_builds_take_over_shared_files() {
        let a = feat("tags", &[], Some(1), &["jot/cli.py", "tests/test_tags.py"], Phase::Built);
        let b = feat("search", &["tags"], Some(2), &["jot/cli.py", "tests/test_search.py"], Phase::Built);
        let all = vec![a.clone(), b.clone()];
        assert_eq!(handed_over(&a, "jot/cli.py", &all).map(|g| g.slug.as_str()), Some("search"));
        assert!(handed_over(&a, "tests/test_tags.py", &all).is_none());
        assert!(handed_over(&b, "jot/cli.py", &all).is_none(), "the latest build owns the file");
    }

    #[test]
    fn dependencies_must_exist_be_acyclic_and_be_built_first() {
        let a = feat("a", &[], None, &[], Phase::Ready);
        let b = feat("b", &["a"], None, &[], Phase::Designing);
        let all = vec![a.clone(), b.clone()];
        assert!(check_deps(&all, "c", &["b".into()]).is_ok());
        assert!(check_deps(&all, "a", &["b".into()]).is_err(), "a -> b -> a");
        assert!(check_deps(&all, "a", &["a".into()]).is_err());
        assert!(check_deps(&all, "c", &["zzz".into()]).is_err());
        assert_eq!(unbuilt_deps(&b, &all), vec!["a".to_string()]);
        let built_a = feat("a", &[], Some(1), &[], Phase::Built);
        assert!(unbuilt_deps(&b, &[built_a, b.clone()]).is_empty());
        let both = [b.clone(), a.clone()];
        let order: Vec<&str> = chain_order(&both).iter().map(|f| f.slug.as_str()).collect();
        assert_eq!(order, vec!["a", "b"]);
    }
}
