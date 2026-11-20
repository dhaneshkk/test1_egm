//! The elephant's view: what of the repository the elephant reads.
//!
//! The elephant should read the project, but on a big repository it reads far
//! more than the feature needs (vendored code, build output, generated data), and
//! every file it reads is resent on each later request of its resumed session.
//! When the project has a `.egmignore` file or a scope, elephant turns (`elephant`,
//! `draft`, `escalate`, `redraft`) run in a view: a folder outside the project
//! holding copies of
//!
//! * every repository file under the scope paths (all of them when no scope is set)
//!   that `.gitignore`, `.egmignore` and the built-in list do not exclude, and
//! * always: the feature's doc, the files it links, its Implementation-map files and
//!   the docs of the steps it comes after, even when ignored or out of scope.
//!
//! egm-dsh and VCS state (`.egm-dsh/`, `.git/`) are never in the view. The view sits
//! at the same path for every turn of a feature, so the resumed dsh session keeps its
//! working directory. `EgmView` in proofs/EgmGate.lean states the rules.
//!
//! Ignore patterns follow `.gitignore` for the common cases: `#` comments, `!`
//! re-includes (the last matching pattern wins), a trailing `/` for directories only,
//! a pattern with an inner `/` anchored at the root, `*`, `?` and `**`. Only the root
//! `.gitignore` and `.egmignore` are read, not nested ones.

use std::path::{Path, PathBuf};

/// Excluded even without an ignore file (upstream EGM's bootstrap skips the same).
pub const BUILTIN: [&str; 12] = [".git/", ".egm-dsh/", "node_modules/", "venv/", ".venv/", "dist/", "build/", "target/",
                                 "third_party/", "__pycache__/", ".pytest_cache/", ".dsh/"];

/// Files larger than this are left out (data dumps, binaries).
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub negate: bool,
    pub dir_only: bool,
    pub anchored: bool,
    pub glob: String,
}

pub fn parse(text: &str) -> Vec<Pattern> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (negate, l) = match l.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, l),
        };
        let l = l.strip_prefix("\\").unwrap_or(l);
        let (dir_only, l) = match l.strip_suffix('/') {
            Some(rest) => (true, rest),
            None => (false, l),
        };
        let anchored = l.contains('/');
        let glob = l.trim_start_matches('/').to_string();
        if !glob.is_empty() {
            out.push(Pattern { negate, dir_only, anchored, glob });
        }
    }
    out
}

/// `*` and `?` stay within a path segment; `**` crosses segments.
pub fn glob_match(pat: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p.starts_with(b"**") {
            let rest = &p[2..];
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            // `**/` may match nothing, or any number of whole segments
            if go(rest, t) {
                return true;
            }
            return (0..t.len()).any(|i| t[i] == b'/' && go(rest, &t[i + 1..])) || (rest.is_empty());
        }
        match p[0] {
            b'*' => (0..=t.len()).take_while(|&i| i == 0 || t[i - 1] != b'/').any(|i| go(&p[1..], &t[i..])),
            b'?' => !t.is_empty() && t[0] != b'/' && go(&p[1..], &t[1..]),
            c => !t.is_empty() && t[0] == c && go(&p[1..], &t[1..]),
        }
    }
    go(pat.as_bytes(), text.as_bytes())
}

fn matches(p: &Pattern, rel: &str, is_dir: bool) -> bool {
    if p.dir_only && !is_dir {
        return false;
    }
    if p.anchored {
        glob_match(&p.glob, rel)
    } else {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        glob_match(&p.glob, name)
    }
}

/// Is `rel` (repository-relative, `/`-separated) ignored? The last matching pattern wins.
pub fn ignored(patterns: &[Pattern], rel: &str, is_dir: bool) -> bool {
    let mut out = false;
    for p in patterns {
        if matches(p, rel, is_dir) {
            out = !p.negate;
        }
    }
    out
}

/// Is `rel` egm-dsh or VCS state? Never in a view, whatever the patterns say.
pub fn is_state(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or("");
    first == ".egm-dsh" || first == ".git"
}

/// Is `rel` under one of the scope paths (everything is, when there is no scope)?
pub fn in_scope(scope: &[String], rel: &str) -> bool {
    scope.is_empty()
        || scope.iter().any(|s| {
            let s = s.trim_matches('/');
            s.is_empty() || s == "." || rel == s || rel.starts_with(&format!("{s}/"))
        })
}

/// Can `rel` (a directory) still contain something in scope?
fn dir_may_hold_scope(scope: &[String], rel: &str) -> bool {
    in_scope(scope, rel) || scope.iter().any(|s| s.trim_matches('/').starts_with(&format!("{rel}/")))
}

/// The patterns in force: built-ins, then the root `.gitignore`, then `.egmignore`.
pub fn patterns(root: &Path) -> Vec<Pattern> {
    let mut text = BUILTIN.join("\n");
    for f in [".gitignore", ".egmignore"] {
        if let Ok(t) = std::fs::read_to_string(root.join(f)) {
            text.push('\n');
            text.push_str(&t);
        }
    }
    parse(&text)
}

/// Does this project use a view at all? Only when it asked for one.
pub fn wanted(root: &Path, scope: &[String]) -> bool {
    !scope.is_empty() || root.join(".egmignore").is_file()
}

/// What a view of `root` holds: (files, files left out by ignore patterns or size).
pub fn view_files(root: &Path, scope: &[String], forced: &[PathBuf]) -> (Vec<PathBuf>, usize) {
    let pats = patterns(root);
    let mut out: Vec<PathBuf> = Vec::new();
    let mut left_out = 0;
    fn walk(root: &Path, dir: &Path, pats: &[Pattern], scope: &[String], out: &mut Vec<PathBuf>, left_out: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if is_state(&rel) {
                continue;
            }
            if ft.is_dir() {
                if ignored(pats, &rel, true) {
                    *left_out += 1;
                } else if dir_may_hold_scope(scope, &rel) {
                    walk(root, &p, pats, scope, out, left_out);
                }
            } else if in_scope(scope, &rel) {
                if ignored(pats, &rel, false) || e.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
                    *left_out += 1;
                } else {
                    out.push(p);
                }
            }
        }
    }
    walk(root, root, &pats, scope, &mut out, &mut left_out);
    for f in forced {
        if !out.contains(f) {
            out.push(f.clone());
        }
    }
    (out, left_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_gitignore() {
        assert!(glob_match("*.log", "a.log") && !glob_match("*.log", "dir/a.log"));
        assert!(glob_match("docs/**/*.md", "docs/a.md") && glob_match("docs/**/*.md", "docs/x/y/a.md"));
        assert!(!glob_match("docs/*.md", "docs/x/a.md"));
        assert!(glob_match("**/fixtures", "a/b/fixtures") && glob_match("**/fixtures", "fixtures"));
        assert!(glob_match("data/**", "data/x/y.csv"));
        assert!(glob_match("?.py", "a.py") && !glob_match("?.py", "ab.py"));
    }

    #[test]
    fn last_pattern_wins_and_dir_only_skips_files() {
        let p = parse("# comment\n*.csv\n!keep.csv\nbuild/\n/only_root.txt\n");
        assert!(ignored(&p, "data/x.csv", false));
        assert!(!ignored(&p, "data/keep.csv", false));
        assert!(ignored(&p, "a/build", true) && !ignored(&p, "a/build", false));
        assert!(ignored(&p, "only_root.txt", false) && !ignored(&p, "sub/only_root.txt", false));
    }

    #[test]
    fn scope_and_state() {
        let s = vec!["jot".to_string(), "docs/designs/".to_string()];
        assert!(in_scope(&s, "jot/cli.py") && in_scope(&s, "docs/designs/x.md") && !in_scope(&s, "jotter/x.py") && !in_scope(&s, "README.md"));
        assert!(in_scope(&[], "anything"));
        assert!(is_state(".egm-dsh/config.json") && is_state(".git/HEAD") && !is_state("src/.git-notes"));
        assert!(dir_may_hold_scope(&s, "docs") && !dir_may_hold_scope(&s, "vendor"));
    }

    #[test]
    fn the_view_keeps_scope_drops_ignored_and_forces_the_doc() {
        let root = std::env::temp_dir().join(format!("egm-scope-{}", crate::project::now_nanos()));
        for rel in ["jot/cli.py", "jot/big_fixture.json", "vendor/lib/x.py", "node_modules/m/i.js", "README.md",
                    "docs/designs/tags.md", ".egm-dsh/config.json", ".git/HEAD", "other/thing.py"] {
            std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
            std::fs::write(root.join(rel), "x").unwrap();
        }
        std::fs::write(root.join(".egmignore"), "vendor/\n*_fixture.json\n").unwrap();
        let rels = |v: &[PathBuf]| v.iter().map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().to_string()).collect::<Vec<_>>();
        let (all, left) = view_files(&root, &[], &[]);
        assert_eq!(rels(&all), [".egmignore", "README.md", "docs/designs/tags.md", "jot/cli.py", "other/thing.py"]);
        assert_eq!(left, 3, "vendor/, node_modules/ and the fixture");
        let (scoped, _) = view_files(&root, &["jot".into()], &[root.join("docs/designs/tags.md"), root.join("vendor/lib/x.py")]);
        assert_eq!(rels(&scoped), ["jot/cli.py", "docs/designs/tags.md", "vendor/lib/x.py"], "forced files come in even when ignored");
        assert!(wanted(&root, &[]) && !wanted(&std::env::temp_dir().join("egm-no-such-dir"), &[]));
        let _ = std::fs::remove_dir_all(&root);
    }
}
