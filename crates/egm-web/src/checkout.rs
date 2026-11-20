//! Read-only access to a project's checkout: the design doc and its git history, the goldfish
//! reviews and the evidence record egm-dsh wrote under `.egm-dsh/`. The audit log holds only
//! hashes and verdicts; the words people read come from here.
//!
//! Every path is built from a validated feature slug or a doc path checked to stay inside the
//! checkout, and git runs with fixed arguments (no shell).

use crate::config::valid_name;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub commit: String,
    pub date: String,
    pub author: String,
    pub subject: String,
}

/// `doc` must be a plain relative path inside the checkout.
pub fn doc_path(checkout: &Path, doc: &str) -> Option<PathBuf> {
    let p = Path::new(doc);
    if doc.is_empty() || p.is_absolute() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(checkout.join(p))
}

pub fn read_doc(checkout: &Path, doc: &str) -> Option<String> {
    std::fs::read_to_string(doc_path(checkout, doc)?).ok()
}

fn git(checkout: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(checkout).args(args).env("GIT_CONFIG_NOSYSTEM", "1").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The doc's commits, newest first (at most 50).
pub fn revisions(checkout: &Path, doc: &str) -> Vec<Revision> {
    if doc_path(checkout, doc).is_none() {
        return vec![];
    }
    let Some(out) = git(checkout, &["log", "-n", "50", "--format=%H%x09%cI%x09%an%x09%s", "--", doc]) else { return vec![] };
    out.lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, '\t');
            Some(Revision { commit: it.next()?.into(), date: it.next()?.into(), author: it.next()?.into(), subject: it.next().unwrap_or_default().into() })
        })
        .collect()
}

pub fn valid_commit(c: &str) -> bool {
    (7..=40).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The doc as it was at `commit`.
pub fn doc_at(checkout: &Path, doc: &str, commit: &str) -> Option<String> {
    if !valid_commit(commit) || doc_path(checkout, doc).is_none() {
        return None;
    }
    git(checkout, &["show", &format!("{commit}:{doc}")])
}

/// The goldfish reviews of a feature, oldest first: `(round, review JSON)`.
pub fn reviews(checkout: &Path, slug: &str) -> Vec<(u32, Value)> {
    if !valid_name(slug) {
        return vec![];
    }
    let dir = checkout.join(".egm-dsh").join("reviews");
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<(u32, Value)> = rd
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let n: u32 = name.strip_prefix(&format!("{slug}-"))?.strip_suffix(".json")?.parse().ok()?;
            let v = serde_json::from_str(&std::fs::read_to_string(dir.join(&name)).ok()?).ok()?;
            Some((n, v))
        })
        .collect();
    out.sort_by_key(|(n, _)| *n);
    out
}

/// The evidence record `egm-dsh gate` wrote, if any.
pub fn evidence(checkout: &Path, slug: &str) -> Option<Value> {
    if !valid_name(slug) {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(checkout.join(".egm-dsh").join("evidence").join(format!("{slug}.json"))).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_checkout() {
        let c = Path::new("/srv/jot");
        assert_eq!(doc_path(c, "docs/a.md"), Some(PathBuf::from("/srv/jot/docs/a.md")));
        for bad in ["", "/etc/passwd", "../x", "docs/../../x", "./a.md"] {
            assert_eq!(doc_path(c, bad), None, "{bad}");
        }
        assert!(valid_commit("abc1234") && !valid_commit("abc") && !valid_commit("--help") && !valid_commit("HEAD~1"));
    }

    #[test]
    fn reads_reviews_and_history_from_a_repo() {
        let dir = std::env::temp_dir().join(format!("egm-web-co-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".egm-dsh/reviews")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        let g = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).status().unwrap().success());
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "T"]);
        std::fs::write(dir.join("docs/f.md"), "one\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "first"]);
        std::fs::write(dir.join("docs/f.md"), "two\n").unwrap();
        g(&["commit", "-qam", "second"]);
        let revs = revisions(&dir, "docs/f.md");
        assert_eq!(revs.iter().map(|r| r.subject.as_str()).collect::<Vec<_>>(), ["second", "first"]);
        assert_eq!(doc_at(&dir, "docs/f.md", &revs[1].commit).as_deref(), Some("one\n"));
        assert_eq!(read_doc(&dir, "docs/f.md").as_deref(), Some("two\n"));
        std::fs::write(dir.join(".egm-dsh/reviews/f-2.json"), r#"{"verdict":"ready"}"#).unwrap();
        std::fs::write(dir.join(".egm-dsh/reviews/f-1.json"), r#"{"verdict":"not-ready"}"#).unwrap();
        std::fs::write(dir.join(".egm-dsh/reviews/fx-1.json"), r#"{}"#).unwrap();
        let r = reviews(&dir, "f");
        assert_eq!(r.iter().map(|(n, v)| (*n, v["verdict"].as_str().unwrap())).collect::<Vec<_>>(), [(1, "not-ready"), (2, "ready")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
