//! Evidence a reviewer can trust, and that the coding agent cannot fake.
//!
//! * `gate`: every check that a built feature's evidence is real, in one command
//!   with an exit code, so CI can block a merge: the design is unchanged since the
//!   build, nothing drifted, the frozen tests and Lean statements are untouched and
//!   were committed before the build, every check passes now, and the frozen tests
//!   kill enough mutants of the changed code.
//! * `mutants`: small deliberate bugs (seeded bugs) in the lines the build changed.
//!   A test suite that passes with a bug in place did not check that line. The
//!   share of mutants the frozen tests catch is the mutation score.
//! * `coverage`: which changed lines no test ran (from an lcov file).
//! * `risk`: a tier and a suggested reviewer, from signals egm-dsh can measure.
//! * `minutes`: human time per feature, estimated from the gaps between commands.
//!
//! Lean: `EgmEvidence` in `proofs/EgmGate.lean`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- mutants

/// Operators: (find, replace). Word operators are matched on word boundaries.
const OPS: &[(&str, &str)] = &[
    ("==", "!="), ("!=", "=="), ("<=", ">"), (">=", "<"), (" < ", " >= "), (" > ", " <= "),
    (" + ", " - "), (" - ", " + "), ("&&", "||"), ("||", "&&"),
    ("and", "or"), ("or", "and"), ("not ", ""), ("True", "False"), ("False", "True"), ("true", "false"), ("false", "true"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mutant {
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// What changed, e.g. `== → !=` or `1 → 2`.
    pub op: String,
    pub mutated_line: String,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte ranges of string literals on a line ('…' or "…"), roughly: a mutant inside
/// a string changes data, not logic.
fn string_spans(line: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match open {
            Some((q, start)) if c == q && prev != '\\' => {
                spans.push((start, i + 1));
                open = None;
            }
            None if c == '"' || c == '\'' => open = Some((c, i)),
            _ => {}
        }
        prev = c;
    }
    if let Some((_, s)) = open {
        spans.push((s, line.len()));
    }
    spans
}

/// Lines that are comments, docs or imports carry no logic worth mutating.
fn skip_line(t: &str) -> bool {
    let t = t.trim_start();
    t.is_empty() || ["#", "//", "/*", "*", "--", "\"\"\"", "'''", "import ", "from ", "use ", "@"].iter().any(|p| t.starts_with(p))
}

/// Every mutant of one line: each operator occurrence outside strings, and each
/// integer literal plus one.
pub fn line_mutants(file: &str, line_no: usize, line: &str) -> Vec<Mutant> {
    if skip_line(line) {
        return vec![];
    }
    let strings = string_spans(line);
    let in_string = |i: usize| strings.iter().any(|(a, b)| i >= *a && i < *b);
    let mut out = Vec::new();
    for (from, to) in OPS {
        let word = from.chars().all(is_word) || from.trim_end().chars().all(is_word);
        let mut start = 0;
        while let Some(off) = line[start..].find(from) {
            let i = start + off;
            start = i + from.len();
            if in_string(i) {
                continue;
            }
            if word {
                let before = line[..i].chars().next_back();
                let after = line[i + from.trim_end().len()..].chars().next();
                if before.is_some_and(is_word) || after.is_some_and(is_word) {
                    continue;
                }
            }
            let mutated = format!("{}{}{}", &line[..i], to, &line[i + from.len()..]);
            out.push(Mutant { file: file.into(), line: line_no, op: format!("{} → {}", from.trim(), if to.trim().is_empty() { "(removed)" } else { to.trim() }), mutated_line: mutated });
        }
    }
    // integer literals: n → n + 1
    let bytes: Vec<(usize, char)> = line.char_indices().collect();
    let mut k = 0;
    while k < bytes.len() {
        let (i, c) = bytes[k];
        if c.is_ascii_digit() && (k == 0 || !is_word(bytes[k - 1].1)) && !in_string(i) {
            let mut j = k;
            while j < bytes.len() && bytes[j].1.is_ascii_digit() {
                j += 1;
            }
            let end = if j < bytes.len() { bytes[j].0 } else { line.len() };
            let next_is_word = j < bytes.len() && (is_word(bytes[j].1) || bytes[j].1 == '.');
            if !next_is_word
                && let Ok(n) = line[i..end].parse::<u64>() {
                    let mutated = format!("{}{}{}", &line[..i], n + 1, &line[end..]);
                    out.push(Mutant { file: file.into(), line: line_no, op: format!("{n} → {}", n + 1), mutated_line: mutated });
                }
            k = j;
        } else {
            k += 1;
        }
    }
    out
}

/// Mutants of the changed lines, at most `max`, spread evenly so that every part
/// of the change is sampled (deterministic: the same change gives the same mutants).
pub fn select_mutants(files: &BTreeMap<String, String>, changed: &BTreeMap<String, BTreeSet<usize>>, max: usize) -> (Vec<Mutant>, usize) {
    let mut all = Vec::new();
    for (file, lines) in changed {
        let Some(text) = files.get(file) else { continue };
        let src: Vec<&str> = text.lines().collect();
        for &n in lines {
            if let Some(l) = src.get(n.wrapping_sub(1)) {
                all.extend(line_mutants(file, n, l));
            }
        }
    }
    let total = all.len();
    if total <= max || max == 0 {
        return (if max == 0 { vec![] } else { all }, total);
    }
    let picked = (0..max).map(|k| all[k * total / max].clone()).collect();
    (picked, total)
}

/// Apply one mutant to a file's text.
pub fn apply(text: &str, m: &Mutant) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if let Some(l) = lines.get_mut(m.line - 1) {
        *l = m.mutated_line.clone();
    }
    let mut s = lines.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MutationResult {
    pub tried: usize,
    /// Seeded bugs a check failed on. A timeout is not counted here.
    pub killed: usize,
    /// Mutants available before sampling.
    pub available: usize,
    /// Seeded bugs whose checks ran past the timeout: neither caught nor survived,
    /// and left out of the score (`EgmEvidence.timeout_never_counts`).
    pub timed_out: usize,
    /// The mutants no test caught: where a reviewer should look.
    pub survivors: Vec<Mutant>,
    /// Seconds the unmutated checks took with a warm build cache.
    #[serde(default)]
    pub baseline_secs: Option<f64>,
    /// Why no seeded bug could be judged (the unmutated checks fail or are too slow).
    #[serde(default)]
    pub problem: Option<String>,
}

impl MutationResult {
    /// Seeded bugs with a verdict: caught or survived. Timeouts are not judged.
    pub fn judged(&self) -> usize {
        self.tried - self.timed_out
    }

    /// killed / judged, or None when no seeded bug was judged.
    pub fn score(&self) -> Option<f64> {
        (self.judged() > 0).then(|| self.killed as f64 / self.judged() as f64)
    }

    /// The gate's mutation check: no problem, and `mutation_ok` over the judged bugs.
    pub fn passes(&self, min_percent: u32) -> bool {
        self.problem.is_none() && mutation_ok(self.killed, self.judged(), min_percent)
    }
}

/// How long one seeded bug may run: the configured timeout, or three times the
/// unmutated checks' time when that is longer, so a slow suite is not read as a hang.
pub fn mutant_timeout(configured: Duration, baseline: Duration) -> Duration {
    configured.max(baseline * 3)
}

/// `EgmEvidence.mutation_ok`: killed / judged ≥ min, in integer arithmetic (min is
/// given in percent). Nothing judged never passes. Callers pass the judged count
/// (caught + survived), never one that includes timeouts.
pub fn mutation_ok(killed: usize, tried: usize, min_percent: u32) -> bool {
    tried > 0 && killed * 100 >= tried * min_percent as usize
}

/// Run each command in a scratch copy of `root` with `overrides` written in place.
/// Ok(true) when every command succeeds, Ok(false) when one fails, Err on a timeout.
/// With `cache`, the commands share that build cache (`CARGO_TARGET_DIR`), writable in the sandbox.
pub fn passes_in_copy(safety: &crate::safety::Safety, root: &Path, overrides: &[(String, String)], cmds: &[Vec<String>], timeout: Duration, cache: Option<&Path>) -> anyhow::Result<Result<bool, ()>> {
    let mode = crate::safety::effective(safety)?;
    let dir = crate::safety::scratch_copy(root, "mutant", &[])?;
    for (f, text) in overrides {
        std::fs::write(dir.join(f), text)?;
    }
    let mut env = crate::safety::allowed_env(safety);
    let extra: Vec<&Path> = cache.into_iter().collect();
    if let Some(c) = cache {
        std::fs::create_dir_all(c)?;
        env.push(("CARGO_TARGET_DIR".into(), c.to_string_lossy().to_string()));
    }
    let mut verdict = Ok(true);
    for cmd in cmds {
        let Some((prog, args)) = cmd.split_first() else { continue };
        let mut child = crate::safety::wrap_rw(mode, &env, &dir, &extra, prog, args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        match status {
            None => {
                verdict = Err(());
                break;
            }
            Some(s) if !s.success() => {
                verdict = Ok(false);
                break;
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(verdict)
}

// ---------------------------------------------------------------- changed lines

/// Lines added or changed per file, from `git diff --unified=0 base..HEAD`.
pub fn parse_diff(diff: &str) -> BTreeMap<String, BTreeSet<usize>> {
    let mut out: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let mut file: Option<String> = None;
    for l in diff.lines() {
        if let Some(f) = l.strip_prefix("+++ ") {
            file = f.strip_prefix("b/").map(str::to_string).filter(|_| f != "/dev/null");
        } else if let (Some(f), Some(h)) = (&file, l.strip_prefix("@@ ")) {
            // @@ -a,b +c,d @@
            if let Some(plus) = h.split_whitespace().find(|t| t.starts_with('+')) {
                let mut it = plus[1..].split(',');
                let start: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let count: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                let set = out.entry(f.clone()).or_default();
                for n in start..start + count {
                    set.insert(n);
                }
            }
        }
    }
    out.retain(|_, s| !s.is_empty());
    out
}

/// Executable lines per file and their hit counts, from an lcov file.
pub fn parse_lcov(text: &str) -> BTreeMap<String, BTreeMap<usize, u64>> {
    let mut out: BTreeMap<String, BTreeMap<usize, u64>> = BTreeMap::new();
    let mut file: Option<String> = None;
    for l in text.lines() {
        if let Some(f) = l.strip_prefix("SF:") {
            file = Some(f.trim().to_string());
        } else if let (Some(f), Some(d)) = (&file, l.strip_prefix("DA:")) {
            let mut it = d.split(',');
            if let (Some(Ok(n)), Some(Ok(h))) = (it.next().map(str::parse::<usize>), it.next().map(|s| s.trim().parse::<u64>())) {
                *out.entry(f.clone()).or_default().entry(n).or_insert(0) += h;
            }
        } else if l.trim() == "end_of_record" {
            file = None;
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// Changed executable lines (lcov lists them).
    pub executable: usize,
    pub covered: usize,
    /// file → changed lines no test ran (`EgmEvidence.uncovered_subset`).
    pub uncovered: BTreeMap<String, Vec<usize>>,
}

/// Changed lines against lcov. A path in lcov may be absolute or relative; it is
/// matched by suffix, so a scratch copy's absolute paths still match.
pub fn coverage_of(changed: &BTreeMap<String, BTreeSet<usize>>, lcov: &BTreeMap<String, BTreeMap<usize, u64>>) -> Coverage {
    let mut c = Coverage::default();
    for (file, lines) in changed {
        let Some((_, hits)) = lcov.iter().find(|(p, _)| p.as_str() == file || p.ends_with(&format!("/{file}"))) else { continue };
        for n in lines {
            if let Some(h) = hits.get(n) {
                c.executable += 1;
                if *h > 0 {
                    c.covered += 1;
                } else {
                    c.uncovered.entry(file.clone()).or_default().push(*n);
                }
            }
        }
    }
    c
}

/// `12-14, 20`.
pub fn ranges(lines: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
            j += 1;
        }
        out.push(if i == j { lines[i].to_string() } else { format!("{}-{}", lines[i], lines[j]) });
        i = j + 1;
    }
    out.join(", ")
}

// ---------------------------------------------------------------- the gate

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// `EgmEvidence.gate_passes`: the gate passes exactly when every check does.
pub fn gate_passes(checks: &[GateCheck]) -> bool {
    !checks.is_empty() && checks.iter().all(|c| c.ok)
}

// ---------------------------------------------------------------- risk

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    Low,
    Medium,
    High,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Low => "low",
            Tier::Medium => "medium",
            Tier::High => "high",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskInput {
    pub files_changed: usize,
    pub lines_changed: usize,
    pub protected: Vec<String>,
    pub manifests: Vec<String>,
    pub uncovered_lines: usize,
    pub survivors: usize,
    pub gate_ok: bool,
    /// Fix-lane limits: within them a change is small.
    pub max_files: usize,
    pub max_lines: usize,
}

/// Each signal raises the tier; the tier is the highest one raised
/// (`EgmEvidence.tier_monotone`: one more signal never lowers it).
pub fn risk(r: &RiskInput) -> (Tier, Vec<String>) {
    let mut why: Vec<(Tier, String)> = Vec::new();
    if !r.gate_ok {
        why.push((Tier::High, "the evidence gate did not pass".into()));
    }
    for f in &r.protected {
        why.push((Tier::High, format!("touches protected `{f}`")));
    }
    for f in &r.manifests {
        why.push((Tier::High, format!("changes dependency manifest `{f}`")));
    }
    if r.uncovered_lines > 0 {
        why.push((Tier::Medium, format!("{} changed line(s) no test ran", r.uncovered_lines)));
    }
    if r.survivors > 0 {
        why.push((Tier::Medium, format!("{} seeded bug(s) no test caught", r.survivors)));
    }
    if r.files_changed > r.max_files || r.lines_changed > r.max_lines {
        why.push((Tier::Medium, format!("{} file(s), {} changed line(s): bigger than a tiny fix ({} files, {} lines)", r.files_changed, r.lines_changed, r.max_files, r.max_lines)));
    }
    let tier = why.iter().map(|(t, _)| *t).max().unwrap_or(Tier::Low);
    (tier, why.into_iter().map(|(_, w)| w).collect())
}

/// Who should review, by tier rather than by familiarity with the code.
pub fn routing(tier: Tier, owners: &[String]) -> String {
    match tier {
        Tier::Low => "any reviewer, including one new to this code: the evidence covers the change".into(),
        Tier::Medium => "any team reviewer; look first at the uncovered lines and surviving seeded bugs listed above".into(),
        Tier::High if owners.is_empty() => "a code owner of the touched files".into(),
        Tier::High => format!("a code owner: {}", owners.join(", ")),
    }
}

/// Owners of `files` from a CODEOWNERS file: the last matching pattern wins.
pub fn codeowners(text: &str, files: &[String]) -> Vec<String> {
    let rules: Vec<(String, Vec<String>)> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pat = it.next()?.trim_start_matches('/').to_string();
            Some((pat, it.map(str::to_string).collect()))
        })
        .collect();
    let mut out = BTreeSet::new();
    for f in files {
        let mut owners: Option<&Vec<String>> = None;
        for (pat, o) in &rules {
            let p = pat.trim_end_matches('/');
            let hit = pat == "*" || crate::scope::glob_match(pat, f) || f.starts_with(&format!("{p}/")) || f == p;
            if hit {
                owners = Some(o);
            }
        }
        if let Some(o) = owners {
            out.extend(o.iter().cloned());
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------- human minutes

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub feature: String,
    pub cmd: String,
    /// Unix seconds.
    pub start: f64,
    pub end: f64,
    /// Minutes logged by hand (`egm-dsh log-time`), outside the CLI.
    #[serde(default)]
    pub logged_minutes: f64,
}

/// A gap longer than this is a break, not work on the feature; it counts as this much.
pub const GAP_CAP_MINUTES: f64 = 30.0;

/// Human minutes on a feature: the gaps between one command ending and the next
/// starting (reading, deciding, answering), each capped, plus minutes logged by
/// hand. An estimate: it cannot tell thinking from lunch, so the cap bounds it
/// (`EgmEvidence.minutes_bounded`).
pub fn human_minutes(events: &[Event], feature: &str) -> (f64, f64) {
    let mut ev: Vec<&Event> = events.iter().filter(|e| e.feature == feature).collect();
    ev.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    let mut gaps = 0.0;
    for w in ev.windows(2) {
        let gap = ((w[1].start - w[0].end) / 60.0).max(0.0);
        gaps += gap.min(GAP_CAP_MINUTES);
    }
    let logged = ev.iter().map(|e| e.logged_minutes).sum();
    (gaps, logged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(killed: usize, survived: usize, timed_out: usize) -> MutationResult {
        MutationResult { tried: killed + survived + timed_out, killed, timed_out, available: killed + survived + timed_out, ..Default::default() }
    }

    #[test]
    fn a_timeout_is_not_a_catch() {
        // every seeded bug timed out: the old rule counted 30/30 caught and passed
        let all_timed_out = result(0, 0, 30);
        assert_eq!(all_timed_out.score(), None);
        assert!(!all_timed_out.passes(60));
        assert!(!all_timed_out.passes(0), "nothing judged never passes, whatever the minimum");
        // timeouts leave the score: 6 caught, 2 survived, 2 timed out is 6/8
        let m = result(6, 2, 2);
        assert_eq!(m.judged(), 8);
        assert_eq!(m.score(), Some(0.75));
        assert!(m.passes(60) && !m.passes(80));
        // a timeout never turns a failing score into a passing one
        assert!(!result(4, 4, 0).passes(60) && !result(4, 4, 7).passes(60));
    }

    #[test]
    fn judged_rule_matches_lean() {
        // EgmEvidence.judgedOk: the verdict depends on caught and survived only
        for k in 0..6 {
            for sv in 0..6 {
                for o in 0..4 {
                    for min in [0, 50, 60, 100] {
                        let lean = k + sv > 0 && (k + sv) * min <= k * 100;
                        assert_eq!(result(k, sv, o).passes(min as u32), lean, "k={k} s={sv} o={o} min={min}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_problem_never_passes() {
        let m = MutationResult { problem: Some("too slow".into()), ..result(10, 0, 0) };
        assert!(!m.passes(0));
    }

    #[test]
    fn mutant_timeout_follows_a_slow_baseline() {
        let s = Duration::from_secs;
        assert_eq!(mutant_timeout(s(60), s(10)), s(60));
        assert_eq!(mutant_timeout(s(60), s(30)), s(90));
    }

    #[test]
    fn seeded_bug_runs_share_a_writable_build_cache_and_time_out() {
        use crate::safety::{Safety, SandboxKind};
        let root = crate::safety::scratch_dir("cache-test-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let cache = crate::safety::scratch_dir("cache-test-target");
        let mut kinds = vec![SandboxKind::None];
        if crate::safety::bwrap_works() {
            kinds.push(SandboxKind::Bwrap);
        }
        for kind in kinds {
            let safety = Safety { sandbox: kind, ..Safety::default() };
            let touch = vec!["sh".to_string(), "-c".to_string(), "test -n \"$CARGO_TARGET_DIR\" && touch \"$CARGO_TARGET_DIR/warm\"".to_string()];
            let r = passes_in_copy(&safety, &root, &[], &[touch], Duration::from_secs(30), Some(&cache)).unwrap();
            assert_eq!(r, Ok(true), "{kind:?}: the cache is set and writable");
            assert!(cache.join("warm").exists(), "{kind:?}: the write lands in the shared cache, outside the scratch copy");
            std::fs::remove_file(cache.join("warm")).unwrap();
            let slow = vec!["sh".to_string(), "-c".to_string(), "sleep 5".to_string()];
            let r = passes_in_copy(&safety, &root, &[], &[slow], Duration::from_millis(200), Some(&cache)).unwrap();
            assert_eq!(r, Err(()), "{kind:?}: past the timeout is Err, never a pass");
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn mutants_flip_operators_outside_strings_and_bump_integers() {
        let m = line_mutants("a.py", 3, "    if n == 0 and name != \"a == b\": return n + 1");
        let ops: Vec<&str> = m.iter().map(|x| x.op.as_str()).collect();
        assert!(ops.contains(&"== → !=") && ops.contains(&"!= → ==") && ops.contains(&"and → or") && ops.contains(&"+ → -"));
        assert!(ops.contains(&"0 → 1") && ops.contains(&"1 → 2"));
        assert_eq!(ops.iter().filter(|o| **o == "== → !=").count(), 1, "the == inside the string is not mutated");
        assert!(line_mutants("a.py", 1, "# if a == b").is_empty() && line_mutants("a.py", 1, "import os").is_empty());
        assert!(line_mutants("a.py", 1, "brand = x").iter().all(|m| m.op != "and → or"), "`and` inside a word is not an operator");
        assert!(line_mutants("a.py", 1, "v2 = 3.5").iter().all(|m| !m.op.starts_with("2 →") && !m.op.starts_with("3 →")));
        let t = "a\nif x == 1:\n    y\n";
        let mm = &line_mutants("f", 2, "if x == 1:")[0];
        assert_eq!(apply(t, mm), "a\nif x != 1:\n    y\n");
    }

    #[test]
    fn sampling_is_even_and_deterministic() {
        let files = BTreeMap::from([("f.py".to_string(), (1..=50).map(|i| format!("x = {i}")).collect::<Vec<_>>().join("\n"))]);
        let changed = BTreeMap::from([("f.py".to_string(), (1..=50).collect::<BTreeSet<_>>())]);
        let (a, total) = select_mutants(&files, &changed, 10);
        assert_eq!((a.len(), total), (10, 50));
        assert_eq!(a.first().unwrap().line, 1);
        assert!(a.last().unwrap().line > 40);
        assert_eq!(select_mutants(&files, &changed, 10).0, a);
    }

    #[test]
    fn the_mutation_threshold_is_integer_and_nothing_tried_never_passes() {
        assert!(mutation_ok(6, 10, 60) && !mutation_ok(5, 10, 60) && !mutation_ok(0, 0, 0));
        assert!(mutation_ok(10, 10, 100));
    }

    #[test]
    fn diffs_and_lcov_give_uncovered_changed_lines() {
        let diff = "diff --git a/jot/cli.py b/jot/cli.py\n--- a/jot/cli.py\n+++ b/jot/cli.py\n@@ -10,0 +11,3 @@ def f\n+a\n+b\n+c\n@@ -20 +23 @@\n-x\n+y\n+++ /dev/null\n";
        let ch = parse_diff(diff);
        assert_eq!(ch["jot/cli.py"].iter().copied().collect::<Vec<_>>(), vec![11, 12, 13, 23]);
        let lcov = parse_lcov("SF:/tmp/x/jot/cli.py\nDA:11,3\nDA:12,0\nDA:23,0\nend_of_record\n");
        let c = coverage_of(&ch, &lcov);
        assert_eq!((c.executable, c.covered), (3, 1));
        assert_eq!(c.uncovered["jot/cli.py"], vec![12, 23]);
        assert_eq!(ranges(&[1, 2, 3, 7, 9, 10]), "1-3, 7, 9-10");
    }

    #[test]
    fn risk_takes_the_highest_signal_and_routes_by_tier() {
        let base = RiskInput { files_changed: 1, lines_changed: 10, gate_ok: true, max_files: 2, max_lines: 40, ..Default::default() };
        assert_eq!(risk(&base).0, Tier::Low);
        assert_eq!(risk(&RiskInput { uncovered_lines: 2, ..base.clone() }).0, Tier::Medium);
        assert_eq!(risk(&RiskInput { manifests: vec!["Cargo.toml".into()], uncovered_lines: 2, ..base.clone() }).0, Tier::High);
        assert_eq!(risk(&RiskInput { gate_ok: false, ..base.clone() }).0, Tier::High);
        assert!(routing(Tier::Low, &[]).contains("new to this code"));
        let owners = codeowners("# owners\n* @all\nauth/ @sec-team\n*.sql @dba\n", &["auth/login.py".into(), "x.sql".into()]);
        assert_eq!(owners, vec!["@dba".to_string(), "@sec-team".to_string()]);
        assert!(routing(Tier::High, &owners).contains("@sec-team"));
    }

    #[test]
    fn human_minutes_cap_each_gap() {
        let e = |s: f64, d: f64| Event { feature: "f".into(), cmd: "x".into(), start: s, end: s + d, logged_minutes: 0.0 };
        let mut ev = vec![e(0.0, 60.0), e(60.0 + 300.0, 10.0), e(370.0 + 7200.0, 5.0)];
        ev.push(Event { feature: "f".into(), cmd: "log-time".into(), start: 9000.0, end: 9000.0, logged_minutes: 25.0 });
        ev.push(Event { feature: "other".into(), cmd: "x".into(), start: 1.0, end: 2.0, logged_minutes: 99.0 });
        let (gaps, logged) = human_minutes(&ev, "f");
        assert!((gaps - (5.0 + 30.0 + ((9000.0 - 7575.0) / 60.0f64).min(30.0))).abs() < 1e-9);
        assert_eq!(logged, 25.0);
    }
}
