//! Git integration for builds: one branch per feature (`egm-dsh/<slug>`), a
//! commit per accepted phase, an exact revert of a rejected phase, and merges
//! left to a human. A step builds only after the branches of the steps it
//! depends on are merged into HEAD.

use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").args(args).current_dir(root).output().map_err(|e| anyhow::anyhow!("cannot run git: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn is_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s.trim() == "true")
}

pub fn has_commits(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "HEAD"]).is_ok()
}

/// Tracked and untracked-but-not-ignored files, relative to `root`.
pub fn listed_files(root: &Path) -> Option<Vec<String>> {
    let s = git(root, &["ls-files", "-co", "--exclude-standard", "-z"]).ok()?;
    Some(s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
}

/// Paths among `paths` with uncommitted changes (modified, staged or untracked).
pub fn dirty(root: &Path, paths: &[String]) -> anyhow::Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let mut args = vec!["status", "--porcelain", "-z", "--"];
    args.extend(paths.iter().map(String::as_str));
    let s = git(root, &args)?;
    Ok(s.split('\0').filter(|e| e.len() > 3).map(|e| e[3..].to_string()).collect())
}

pub fn current_branch(root: &Path) -> String {
    git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn branch_exists(root: &Path, name: &str) -> bool {
    git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok()
}

/// Switch to `name`, creating it from HEAD if needed. Uncommitted changes carry over.
pub fn ensure_branch(root: &Path, name: &str) -> anyhow::Result<bool> {
    if current_branch(root) == name {
        return Ok(false);
    }
    if branch_exists(root, name) {
        git(root, &["switch", name])?;
        Ok(false)
    } else {
        git(root, &["switch", "-c", name])?;
        Ok(true)
    }
}

/// Is `branch` merged into HEAD? A branch that does not exist counts as merged
/// (the step was built without git).
pub fn is_merged(root: &Path, branch: &str) -> bool {
    !branch_exists(root, branch) || git(root, &["merge-base", "--is-ancestor", branch, "HEAD"]).is_ok()
}

fn in_head(root: &Path, path: &str) -> bool {
    has_commits(root) && git(root, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok()
}

/// Commit exactly `paths` (additions, changes, deletions) as egm-dsh. Returns
/// the short commit id, or `None` when nothing in `paths` changed.
pub fn commit(root: &Path, paths: &[String], message: &str) -> anyhow::Result<Option<String>> {
    let changed = dirty(root, paths)?;
    if changed.is_empty() {
        return Ok(None);
    }
    let mut add = vec!["add", "-A", "--"];
    add.extend(changed.iter().map(String::as_str));
    git(root, &add)?;
    let mut c = vec!["-c", "user.name=egm-dsh", "-c", "user.email=egm-dsh@localhost", "commit", "-q", "--no-verify", "-m", message, "--"];
    c.extend(changed.iter().map(String::as_str));
    git(root, &c)?;
    Ok(Some(git(root, &["rev-parse", "--short", "HEAD"])?.trim().to_string()))
}

/// Undo a rejected build: tracked files go back to HEAD, files the build
/// created are removed. Returns files it could not restore (untracked files
/// that existed before the build and were changed).
pub fn revert(root: &Path, changed: &[String], created: &[String]) -> anyhow::Result<Vec<String>> {
    let mut lost = Vec::new();
    for f in changed {
        if created.contains(f) {
            let _ = std::fs::remove_file(root.join(f));
        } else if in_head(root, f) {
            git(root, &["checkout", "HEAD", "--", f])?;
        } else {
            lost.push(f.clone());
        }
    }
    Ok(lost)
}

/// The newest commit whose message starts with `prefix` (egm-dsh's own design and
/// build commits), if any.
pub fn last_commit_with(root: &Path, prefix: &str) -> Option<String> {
    let grep = format!("--grep=^{}", regex_escape(prefix));
    git(root, &["log", "--all", "--format=%H", &grep, "-n", "1"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn regex_escape(s: &str) -> String {
    s.chars().map(|c| if "\\.^$|?*+()[]{}".contains(c) { format!("\\{c}") } else { c.to_string() }).collect()
}

/// Whether `a` is an ancestor of `b` (or the same commit).
pub fn is_ancestor(root: &Path, a: &str, b: &str) -> bool {
    Command::new("git").args(["merge-base", "--is-ancestor", a, b]).current_dir(root).status().is_ok_and(|s| s.success())
}

/// `git diff --unified=0 from to -- paths`.
pub fn diff_unified0(root: &Path, from: &str, to: &str, paths: &[String]) -> anyhow::Result<String> {
    let mut args = vec!["diff", "--unified=0", "--no-color", from, to, "--"];
    args.extend(paths.iter().map(String::as_str));
    git(root, &args)
}

/// Files changed between two commits under `paths`.
pub fn changed_between(root: &Path, from: &str, to: &str, paths: &[String]) -> Vec<String> {
    let mut args = vec!["diff", "--name-only", from, to, "--"];
    args.extend(paths.iter().map(String::as_str));
    git(root, &args).map(|s| s.lines().map(str::to_string).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        let d = crate::safety::scratch_dir("git");
        std::fs::create_dir_all(&d).unwrap();
        git(&d, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(d.join("a.txt"), "one\n").unwrap();
        commit(&d, &["a.txt".into()], "init").unwrap();
        d
    }

    #[test]
    fn branch_commit_and_revert() {
        let d = repo();
        assert!(ensure_branch(&d, "egm-dsh/x").unwrap());
        std::fs::write(d.join("a.txt"), "two\n").unwrap();
        std::fs::write(d.join("new.txt"), "n\n").unwrap();
        assert_eq!(dirty(&d, &["a.txt".into(), "new.txt".into()]).unwrap().len(), 2);
        // rejected: revert restores a.txt and removes new.txt
        let lost = revert(&d, &["a.txt".into(), "new.txt".into()], &["new.txt".into()]).unwrap();
        assert!(lost.is_empty());
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "one\n");
        assert!(!d.join("new.txt").exists());
        // accepted: commit only the named file
        std::fs::write(d.join("a.txt"), "three\n").unwrap();
        std::fs::write(d.join("other.txt"), "untouched\n").unwrap();
        assert!(commit(&d, &["a.txt".into()], "phase 1").unwrap().is_some());
        assert_eq!(dirty(&d, &["a.txt".into()]).unwrap().len(), 0);
        assert_eq!(dirty(&d, &["other.txt".into()]).unwrap().len(), 1, "files outside the commit stay uncommitted");
        // the feature branch is not merged into main until a human merges it
        git(&d, &["switch", "-q", "main"]).unwrap();
        assert!(!is_merged(&d, "egm-dsh/x"));
        git(&d, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-edit", "egm-dsh/x"]).unwrap();
        assert!(is_merged(&d, "egm-dsh/x"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
