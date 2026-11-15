//! The executable spec: every acceptance criterion and every example in the
//! design doc is something egm-dsh can run. Mirrors `proofs/EgmSpec.lean`.
//!
//! * Tests first, then frozen. A tester session writes the tests named by the
//!   criteria before any code exists; a goldfish confirms each test checks its
//!   criterion; egm-dsh records their hashes and keeps copies. The builder may
//!   not change them: a build that does is rejected and the copies restored.
//! * Red, then green. When frozen, each new or changed criterion's test must
//!   FAIL (it tests something the code does not do yet); after the build it
//!   must PASS. A test that passes either way is vacuous and is refused.
//! * Examples. `$ command` / expected-output pairs in the doc's `## Examples`
//!   section run in a scratch copy of the repo and must match exactly.
//! * Lean. `Check: lean proofs/X.lean#theorem` must type-check with no
//!   `sorry` and no new axioms, and the builder may change only that
//!   theorem's proof: everything else in the file is frozen.

use std::collections::BTreeMap;
use std::path::Path;

use crate::safety::Safety;

use serde::{Deserialize, Serialize};

use crate::doc::{hash, Doc};

/// What `Check:` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// A test file, optionally one test in it (`file::Class::test`).
    Test { file: String, id: Option<String> },
    /// `lean file#theorem`.
    Lean { file: String, theorem: String },
    /// `manual — reason`: not executable.
    Manual,
}

pub fn parse_check(check: &str) -> Check {
    let c = check.trim();
    if c.starts_with("manual") {
        return Check::Manual;
    }
    if let Some(rest) = c.strip_prefix("lean ") {
        let rest = rest.trim().trim_matches('`');
        if let Some((file, theorem)) = rest.split_once('#') {
            return Check::Lean { file: file.trim().into(), theorem: theorem.trim().into() };
        }
    }
    match c.split_once("::") {
        Some((file, id)) => Check::Test { file: file.into(), id: Some(id.into()) },
        None => Check::Test { file: c.into(), id: None },
    }
}

impl Check {
    pub fn file(&self) -> Option<&str> {
        match self {
            Check::Test { file, .. } | Check::Lean { file, .. } => Some(file),
            Check::Manual => None,
        }
    }
}

/// One `console` block of the `## Examples` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    pub commands: Vec<(String, String)>,
}

impl Example {
    pub fn text(&self) -> String {
        self.commands.iter().map(|(c, o)| format!("$ {c}\n{o}")).collect::<Vec<_>>().join("\n")
    }
}

/// Fenced blocks tagged `console`, `shell`, `sh` or `bash` under `## Examples`.
/// Lines starting with `$ ` are commands; the lines up to the next command are
/// its expected output (stdout and stderr together).
pub fn examples(doc: &Doc) -> Vec<Example> {
    let Some(body) = doc.section("Examples") else { return vec![] };
    let mut out = Vec::new();
    let mut in_block = false;
    let mut cur: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let t = line.trim_end();
        if let Some(info) = t.trim_start().strip_prefix("```") {
            if in_block {
                if !cur.is_empty() {
                    out.push(Example { commands: std::mem::take(&mut cur) });
                }
                in_block = false;
            } else {
                in_block = matches!(info.trim(), "console" | "shell" | "sh" | "bash");
            }
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(cmd) = t.strip_prefix("$ ") {
            cur.push((cmd.to_string(), String::new()));
        } else if let Some(last) = cur.last_mut() {
            if !last.1.is_empty() {
                last.1.push('\n');
            }
            last.1.push_str(t);
        }
    }
    out
}

fn normalize(s: &str) -> String {
    s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim_matches('\n').to_string()
}

/// Run one example block in a fresh scratch copy of the repo, sandboxed.
/// `Err` names the first command whose output differs.
pub fn run_example(safety: &Safety, root: &Path, ex: &Example) -> Result<(), String> {
    let mode = crate::safety::effective(safety).map_err(|e| e.to_string())?;
    let env = crate::safety::allowed_env(safety);
    let dir = crate::safety::scratch_copy(root, "example", &[]).map_err(|e| format!("cannot copy the repo for the example: {e}"))?;
    let result = (|| {
        for (cmd, want) in &ex.commands {
            let out = crate::safety::wrap(mode, &env, &dir, "sh", &["-c".to_string(), cmd.clone()]).output().map_err(|e| format!("`{cmd}`: {e}"))?;
            let got = normalize(&(String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)));
            if got != normalize(want) {
                return Err(format!("`{cmd}` printed {:?}, the doc expects {:?}", clip(&got), clip(&normalize(want))));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn clip(s: &str) -> String {
    if s.chars().count() > 160 { format!("{}…", s.chars().take(160).collect::<String>()) } else { s.to_string() }
}

/// The command that runs one check: `check_cmd` with `{check}` (as written),
/// `{file}`, and `{unittest}` (`tests/t.py::C::m` → `tests.t.C.m`) filled in.
pub fn test_command(check_cmd: &[String], file: &str, id: Option<&str>) -> Vec<String> {
    let raw = match id {
        Some(i) => format!("{file}::{i}"),
        None => file.to_string(),
    };
    let module = file.trim_end_matches(".py").replace(['/', '\\'], ".");
    let unittest = match id {
        Some(i) => format!("{module}.{}", i.replace("::", ".")),
        None => module,
    };
    check_cmd.iter().map(|a| a.replace("{check}", &raw).replace("{file}", file).replace("{unittest}", &unittest)).collect()
}

/// Does the named test pass right now? Runs sandboxed in a scratch copy.
pub fn run_test(safety: &Safety, root: &Path, check_cmd: &[String], file: &str, id: Option<&str>) -> Result<bool, String> {
    let cmd = test_command(check_cmd, file, id);
    let out = crate::safety::run_in_copy(safety, root, &cmd).map_err(|e| e.to_string())?;
    if let Some(why) = crate::safety::could_not_run(&out) {
        return Err(format!("could not run `{}`: {why}", cmd.join(" ")));
    }
    Ok(out.status.success())
}

/// Why a Lean check did not pass: the proof is not done (`Red`: sorry, or Lean
/// rejected the file) or the check itself could not run (`Harness`).
#[derive(Debug, PartialEq, Eq)]
pub enum LeanFail {
    Red(String),
    Harness(String),
}

impl std::fmt::Display for LeanFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeanFail::Red(m) | LeanFail::Harness(m) => f.write_str(m),
        }
    }
}

/// Where a theorem's proof body is: from the `:=` after `theorem NAME` to the
/// next top-level declaration (a line starting in column 0) or the file end.
fn proof_span(text: &str, theorem: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        if start.is_none() {
            let t = line.trim_start();
            for kw in ["theorem ", "lemma "] {
                if let Some(rest) = t.strip_prefix(kw)
                    && rest.split(|c: char| c.is_whitespace() || c == '(' || c == ':' || c == '{' || c == '[').next() == Some(theorem)
                {
                    start = Some(offset);
                }
            }
        }
        offset += line.len();
    }
    let s = start?;
    let body = s + text[s..].find(":=")? + 2;
    let mut end = text.len();
    let mut off = body;
    for line in text[body..].split_inclusive('\n') {
        if off > body && line.chars().next().is_some_and(|c| !c.is_whitespace()) {
            end = off;
            break;
        }
        off += line.len();
    }
    Some((body, end))
}

/// The file with the theorem's proof body cut out: everything the builder must
/// leave alone. `None` if the theorem is not there.
pub fn lean_frozen_part(text: &str, theorem: &str) -> Option<String> {
    let (b, e) = proof_span(text, theorem)?;
    Some(format!("{}<proof>{}", &text[..b], &text[e..]))
}

/// Run Lean on a copy of the file with `#print axioms THEOREM` appended. Ok
/// only if it checks, uses no `sorry`, and declares no axioms of its own.
pub fn lean_check(safety: &Safety, root: &Path, lean_cmd: &[String], file: &str, theorem: &str) -> Result<(), LeanFail> {
    use LeanFail::{Harness, Red};
    let text = std::fs::read_to_string(root.join(file)).map_err(|e| Harness(format!("cannot read {file}: {e}")))?;
    if proof_span(&text, theorem).is_none() {
        return Err(Harness(format!("{file} has no theorem `{theorem}`")));
    }
    if text.lines().any(|l| l.trim_start().starts_with("axiom ")) {
        return Err(Red(format!("{file} declares an axiom")));
    }
    let dir = crate::safety::scratch_dir("lean");
    std::fs::create_dir_all(&dir).map_err(|e| Harness(e.to_string()))?;
    let copy = dir.join("Check.lean");
    std::fs::write(&copy, format!("{text}\n#print axioms {theorem}\n")).map_err(|e| Harness(e.to_string()))?;
    let (prog, args) = lean_cmd.split_first().ok_or(Harness("lean_cmd is empty".into()))?;
    let mode = crate::safety::effective(safety).map_err(|e| Harness(e.to_string()))?;
    let mut all: Vec<String> = args.to_vec();
    all.push(copy.to_string_lossy().to_string());
    let out = crate::safety::wrap(mode, &crate::safety::allowed_env(safety), &dir, prog, &all).output();
    let _ = std::fs::remove_dir_all(&dir);
    // Same wording whether or not a sandbox is in the way: without one, a missing Lean
    // fails at spawn; under bubblewrap it fails inside the sandbox (`could_not_run`).
    let out = out.map_err(|e| Harness(format!("could not run Lean (`{prog}`): {e}")))?;
    if let Some(why) = crate::safety::could_not_run(&out) {
        return Err(Harness(format!("could not run Lean (`{prog}`): {why}")));
    }
    let log = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(Red(format!("Lean rejected {file}: {}", clip(log.lines().find(|l| l.contains("error")).unwrap_or(log.trim())))));
    }
    if log.contains("sorry") {
        return Err(Red(format!("`{theorem}` still uses sorry")));
    }
    Ok(())
}

/// One criterion as frozen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenCriterion {
    pub text: String,
    pub check: String,
    /// Hash of the test file, or of the Lean file's frozen part.
    pub file_hash: u64,
    /// `red` (failed before the build), `carried` (unchanged since an earlier
    /// freeze where it went red), or `manual`.
    pub red: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenSpec {
    /// The doc revision the tests were written and reviewed for.
    pub doc_hash: u64,
    pub criteria: Vec<FrozenCriterion>,
    /// Test files: content hash. The builder may not change them.
    pub test_files: BTreeMap<String, u64>,
    /// Lean files: theorem and hash of the part outside its proof.
    pub lean_files: BTreeMap<String, (String, u64)>,
    /// Example blocks and whether each went red (`carried` if unchanged).
    pub examples: Vec<(String, String)>,
}

/// The hash a criterion's file is frozen at, or `None` if it cannot be read.
pub fn frozen_hash(root: &Path, check: &Check) -> Option<u64> {
    match check {
        Check::Test { file, .. } => std::fs::read_to_string(root.join(file)).ok().map(|t| hash(&t)),
        Check::Lean { file, theorem } => std::fs::read_to_string(root.join(file)).ok().and_then(|t| lean_frozen_part(&t, theorem)).map(|t| hash(&t)),
        Check::Manual => Some(0),
    }
}

/// Is the criterion unchanged since `prev` froze it (same text, check and file)?
pub fn carried(prev: Option<&FrozenSpec>, text: &str, check: &str, file_hash: u64) -> bool {
    prev.is_some_and(|p| p.criteria.iter().any(|c| c.text == text && c.check == check && c.file_hash == file_hash && c.red != "manual"))
}

/// Red/green verdict for one check. Mirrors `accept` in the Lean file:
/// a fresh criterion needs red before and green after; a carried one needs green.
pub fn red_ok(failed_before: bool, was_carried: bool) -> bool {
    failed_before || was_carried
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checks() {
        assert_eq!(parse_check("tests/t.py::C::test_x"), Check::Test { file: "tests/t.py".into(), id: Some("C::test_x".into()) });
        assert_eq!(parse_check("tests/t.py"), Check::Test { file: "tests/t.py".into(), id: None });
        assert_eq!(parse_check("lean proofs/Tags.lean#filter_sound"), Check::Lean { file: "proofs/Tags.lean".into(), theorem: "filter_sound".into() });
        assert_eq!(parse_check("manual — visual"), Check::Manual);
    }

    #[test]
    fn builds_test_commands() {
        let cmd: Vec<String> = ["python3", "-m", "unittest", "{unittest}"].map(String::from).to_vec();
        assert_eq!(test_command(&cmd, "tests/test_tags.py", Some("TagTest::test_x"))[3], "tests.test_tags.TagTest.test_x");
        let py: Vec<String> = ["pytest", "{check}"].map(String::from).to_vec();
        assert_eq!(test_command(&py, "tests/t.py", Some("test_a"))[1], "tests/t.py::test_a");
    }

    #[test]
    fn parses_examples() {
        let text = "## Examples\n\n```console\n$ jot add a --tag w\nadded #1\n$ jot list --tag w\n#1 a  [w]\n```\n\n```python\nnot run\n```\n";
        let d = Doc { path: "d.md".into(), text: text.into() };
        let ex = examples(&d);
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].commands, vec![("jot add a --tag w".into(), "added #1".into()), ("jot list --tag w".into(), "#1 a  [w]".into())]);
    }

    #[test]
    fn runs_examples_in_a_scratch_copy() {
        let root = crate::safety::scratch_dir("t");
        std::fs::create_dir_all(&root).unwrap();
        let ok = Example { commands: vec![("echo hi > f.txt && cat f.txt".into(), "hi".into())] };
        let bad = Example { commands: vec![("echo hello".into(), "bye".into())] };
        let s = Safety::default();
        assert!(run_example(&s, &root, &ok).is_ok());
        assert!(run_example(&s, &root, &bad).unwrap_err().contains("hello"));
        assert!(!root.join("f.txt").exists(), "examples never touch the repo");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lean_freeze_covers_everything_but_the_proof() {
        let a = "def f (n : Nat) := n\n\ntheorem f_id (n : Nat) : f n = n := by\n  sorry\n\n#eval 1\n";
        let b = "def f (n : Nat) := n\n\ntheorem f_id (n : Nat) : f n = n := by\n  rfl\n\n#eval 1\n";
        let weakened = "def f (n : Nat) := n\n\ntheorem f_id (n : Nat) : True := by\n  trivial\n\n#eval 1\n";
        let redefined = "def f (n : Nat) := 0\n\ntheorem f_id (n : Nat) : f n = n := by\n  sorry\n\n#eval 1\n";
        assert_eq!(lean_frozen_part(a, "f_id"), lean_frozen_part(b, "f_id"), "filling the proof is allowed");
        assert_ne!(lean_frozen_part(a, "f_id"), lean_frozen_part(weakened, "f_id"), "changing the statement is not");
        assert_ne!(lean_frozen_part(a, "f_id"), lean_frozen_part(redefined, "f_id"), "changing a definition is not");
        assert!(lean_frozen_part(a, "nope").is_none());
    }

    #[test]
    fn red_green_rule() {
        assert!(red_ok(true, false));
        assert!(red_ok(false, true));
        assert!(!red_ok(false, false), "a test that passes before any code is vacuous");
    }
}
