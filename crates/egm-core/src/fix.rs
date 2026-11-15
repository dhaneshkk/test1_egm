//! The fix lane: tiny changes with less ceremony, on the same gate.
//!
//! `egm-dsh fix <slug> "<problem>" --files F --check TEST` goes through the same
//! phases as any feature, each cut to what a tiny fix needs:
//!
//! 1. **A short doc, no elephant.** egm-dsh writes `docs/fixes/<slug>.md` from the
//!    problem, the files and the check. It is hashed into the gate like any doc.
//! 2. **One goldfish session** (combined mode, fail-fast, no verifier) on that doc,
//!    the files and nothing else. It must pass, exactly as for a feature.
//! 3. **Red.** The named test must fail before any code changes. A test that does
//!    not exist yet is written by one tester session and judged by one reviewer.
//! 4. **Build** by one builder session, limited to the listed files, then green:
//!    the test passes and the acceptance command (the whole suite) runs.
//! 5. **Diff review.** One fresh session reads only the doc and the diff.
//!
//! What counts as tiny is decided by egm-dsh, not by a model: at most
//! `max_files` files, none protected and no dependency manifest (checked before
//! anything runs), and at most `max_lines` changed lines (checked on the real
//! change). A change over the line limit is reverted and the fix is **promoted**:
//! it reports `goldfish false`, so the design reopens, and the feature leaves the
//! fix lane, so every later goldfish is the full review. `EgmFix` in
//! proofs/EgmGate.lean proves a fix is built only with red, green, small and no
//! blocking builder decision, and hardened only with a clean diff review too.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FixLimits {
    pub max_files: usize,
    pub max_lines: usize,
    /// Globs (`.gitignore` style) a fix may never touch, e.g. `auth/`, `migrations/`.
    pub protected: Vec<String>,
}

impl Default for FixLimits {
    fn default() -> Self {
        FixLimits { max_files: 2, max_lines: 40, protected: vec![] }
    }
}

/// Dependency manifests and lockfiles: a fix that needs a new dependency is not tiny.
pub const MANIFESTS: [&str; 14] = ["Cargo.toml", "Cargo.lock", "package.json", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
                                   "pyproject.toml", "requirements.txt", "setup.py", "setup.cfg", "go.mod", "go.sum", "Gemfile", "pom.xml"];

/// Why these files are not a tiny fix (empty: they are).
pub fn eligible(files: &[String], limits: &FixLimits) -> Vec<String> {
    let mut why = Vec::new();
    if files.is_empty() {
        why.push("name the file(s) the fix changes with --files".to_string());
    }
    if files.len() > limits.max_files {
        why.push(format!("{} files (a fix changes at most {})", files.len(), limits.max_files));
    }
    let pats = crate::scope::parse(&limits.protected.join("\n"));
    for f in files {
        let name = f.rsplit('/').next().unwrap_or(f);
        if MANIFESTS.contains(&name) {
            why.push(format!("`{f}` is a dependency manifest"));
        }
        // a file under a protected directory is protected too
        let mut prefix = String::new();
        let mut hit = false;
        let parts: Vec<&str> = f.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            hit |= crate::scope::ignored(&pats, &prefix, i + 1 < parts.len());
        }
        if hit {
            why.push(format!("`{f}` is protected (`set-fix-limits --protect`)"));
        }
        if f.starts_with('/') || f.split('/').any(|c| c == "..") || crate::scope::is_state(f) {
            why.push(format!("`{f}` is not a repository file"));
        }
    }
    why
}

/// The short design doc. Every section egm-dsh expects is there, so egm-dsh's own
/// checks pass on it; the goldfish still has to understand it.
pub fn doc_text(title: &str, problem: &str, files: &[String], check: &str, limits: &FixLimits) -> String {
    let map: Vec<String> = files.iter().map(|f| format!("- `{f}` — the fix")).collect();
    format!(
        "# Fix: {title}\n\n\
## Problem\n\n{problem}\n\n\
## Goals and non-goals\n\nGoal: fix exactly the problem above. Non-goals: any other change, refactoring or new behaviour.\n\n\
## Technical plan\n\nMake the smallest change in the file(s) below that makes the check pass without breaking any existing test. \
Keep every existing caller and saved data working.\n\n\
## Alternatives considered\n\n**The full design lane.** Not needed: this is a fix of at most {} file(s) and {} changed line(s); \
egm-dsh moves it to the full lane if the change turns out bigger.\n\n\
## Risks and mitigations\n\n**A fix that breaks something else.** The whole test suite runs after the change, and a fresh reviewer reads the diff.\n\n\
## Implementation map\n\n{}\n\n\
## Acceptance criteria\n\n- The problem above no longer happens. Check: {check}\n",
        limits.max_files,
        limits.max_lines,
        map.join("\n")
    )
}

/// Format the `Check:` part: a test id in backticks, or `manual — how`.
pub fn check_text(test: Option<&str>, manual: Option<&str>) -> Option<String> {
    match (test, manual) {
        (Some(t), None) => Some(format!("`{}`", t.trim_matches('`'))),
        (None, Some(m)) => Some(format!("manual — {m}")),
        _ => None,
    }
}

/// Lines changed (added + removed) in `files` against HEAD; an untracked file counts in full.
pub fn changed_lines(root: &std::path::Path, files: &[String]) -> anyhow::Result<usize> {
    if files.is_empty() {
        return Ok(0);
    }
    let mut args = vec!["diff", "--numstat", "HEAD", "--"];
    args.extend(files.iter().map(String::as_str));
    let out = std::process::Command::new("git").args(&args).current_dir(root).output()?;
    anyhow::ensure!(out.status.success(), "git diff failed: {}", String::from_utf8_lossy(&out.stderr));
    let mut n = 0;
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = l.split_whitespace();
        let a = it.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0);
        let r = it.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0);
        n += a + r;
    }
    let tracked = crate::vcs::listed_files(root).unwrap_or_default();
    for f in files.iter().filter(|f| !tracked.contains(f)) {
        n += std::fs::read_to_string(root.join(f)).map(|t| t.lines().count()).unwrap_or(0);
    }
    Ok(n)
}

/// How a fix build ends. Mirrors `EgmFix.outcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Built: red before, green after, small, nothing left for the builder to decide.
    Built,
    /// Too big for the lane, or the builder had to decide: reverted, design reopened.
    Promoted,
    /// The check is not green (or was not red): reverted, still ready to build again.
    #[allow(dead_code)] // reached through `outcome`, matched by its callers as "not promoted"
    Rejected,
}

pub fn outcome(red: bool, green: bool, small: bool, blocking: bool) -> Outcome {
    if blocking || !small {
        Outcome::Promoted
    } else if red && green {
        Outcome::Built
    } else {
        Outcome::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligibility_is_decided_by_egm_dsh() {
        let l = FixLimits { protected: vec!["auth/".into(), "migrations/**".into()], ..Default::default() };
        assert!(eligible(&["jot/cli.py".into()], &l).is_empty());
        assert!(eligible(&["a.py".into(), "b.py".into()], &l).is_empty());
        assert_eq!(eligible(&["a.py".into(), "b.py".into(), "c.py".into()], &l).len(), 1);
        assert!(eligible(&[], &l)[0].contains("--files"));
        assert!(eligible(&["Cargo.toml".into()], &l)[0].contains("manifest"));
        assert!(eligible(&["web/package.json".into()], &l)[0].contains("manifest"));
        assert!(eligible(&["auth/login.py".into()], &l)[0].contains("protected"));
        assert!(eligible(&["db/migrations/0001.sql".into()], &l).is_empty(), "anchored pattern: only the root migrations/");
        assert!(eligible(&["migrations/0001.sql".into()], &l)[0].contains("protected"));
        assert!(!eligible(&["../x.py".into()], &l).is_empty() && !eligible(&[".egm-dsh/config.json".into()], &l).is_empty());
    }

    #[test]
    fn outcome_matches_egm_fix() {
        for red in [false, true] {
            for green in [false, true] {
                for small in [false, true] {
                    for blocking in [false, true] {
                        let o = outcome(red, green, small, blocking);
                        assert_eq!(o == Outcome::Built, red && green && small && !blocking);
                        assert_eq!(o == Outcome::Promoted, blocking || !small);
                    }
                }
            }
        }
    }

    #[test]
    fn the_short_doc_passes_egm_dsh_checks() {
        let t = doc_text("tag case", "list --tag Work misses notes tagged work.", &["jot/cli.py".into()], "`tests/test_jot.py::T::test_case`", &FixLimits::default());
        let d = crate::doc::Doc { path: "x.md".into(), text: t };
        assert!(d.missing_sections().is_empty(), "{:?}", d.missing_sections());
        assert_eq!(d.implementation_map(), vec!["jot/cli.py"]);
        assert!(d.criteria().iter().all(|c| c.check.is_some()));
        assert!(crate::steps::size_alarms(&d, Default::default()).is_empty());
        assert_eq!(check_text(Some("t.py::x"), None).as_deref(), Some("`t.py::x`"));
        assert_eq!(check_text(None, Some("run jot list")).as_deref(), Some("manual — run jot list"));
        assert_eq!(check_text(None, None), None);
    }
}
