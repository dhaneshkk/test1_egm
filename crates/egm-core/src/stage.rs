//! The goldfish's reading room. A memoryless reviewer must know only the design
//! doc and what it links, so egm-dsh does not rely on telling it so: it gets the
//! reading room and nothing else.
//!
//! 1. A **stage**: a fresh folder outside the project holding copies of the doc,
//!    the files it links and the Implementation-map files that exist, at their
//!    repository-relative paths. The reviewer's session runs there. egm-dsh's own
//!    state (`.egm-dsh/`: earlier reviews, decisions, config) and every unlinked
//!    file are simply absent.
//! 2. **Read-only tools**: staged sessions always run with the lean tool list
//!    (no shell, web, sub-agents or jobs).
//! 3. An **audit**: every tool call DeepSeek Harness reports is checked. A call
//!    to any tool outside [`READ_TOOLS`], or a path that resolves outside the
//!    stage, discards that reviewer's answer. Fail closed: a discarded goldfish
//!    makes the review incomplete, never ready, and a discarded verifier drops
//!    no alarm.
//!
//! Without a filesystem sandbox around dsh itself, the audit is what enforces
//! this; it relies on dsh reporting its tool calls, which come from the harness,
//! not from the model.

use std::path::{Component, Path, PathBuf};

use crate::agent::Touch;

/// Tools a staged reviewer may use. Anything else (shell, web, sub-agents, writes,
/// jobs) is outside the reading room.
pub const READ_TOOLS: [&str; 6] = ["read", "read_image", "glob", "grep", "working_directory", "todo_write"];

pub struct Stage {
    pub dir: PathBuf,
    /// Repository-relative paths copied in.
    pub files: Vec<String>,
}

/// Stages alive in this process, so an interrupted run still removes them
/// (`remove_all`, called from the Ctrl-C handler).
static LIVE: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

fn live() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    LIVE.lock().unwrap_or_else(|p| p.into_inner())
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        live().retain(|d| d != &self.dir);
    }
}

/// Remove every stage this process still has (interrupt path).
pub fn remove_all() {
    for d in live().drain(..) {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Remove stages left by egm-dsh processes that are gone (killed outright). Returns how many.
pub fn sweep_stale() -> usize {
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else { return 0 };
    let mut n = 0;
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(pid) = name.strip_prefix("egm-dsh-stage-").and_then(|r| r.split('-').next()).and_then(|p| p.parse::<u32>().ok()) else { continue };
        if pid != std::process::id() && crate::session::start_ticks(pid).is_none() && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Is `rel` egm-dsh or VCS state, never reviewer context?
fn is_state(rel: &Path) -> bool {
    matches!(rel.components().next(), Some(Component::Normal(c)) if c == ".egm-dsh" || c == ".git")
}

impl Stage {
    /// Copy `files` (absolute, inside `root`) into a new stage. Files outside the
    /// repository or inside egm-dsh/VCS state are left out.
    pub fn new(root: &Path, files: &[PathBuf]) -> anyhow::Result<Stage> {
        let dir = std::env::temp_dir().join(format!("egm-dsh-stage-{}-{}", std::process::id(), crate::project::now_nanos()));
        Stage::at(dir, root, files)
    }

    /// The same, at a fixed `dir` (emptied first): the elephant's view keeps one path
    /// for every turn, so its resumed session keeps its working directory.
    pub fn at(dir: PathBuf, root: &Path, files: &[PathBuf]) -> anyhow::Result<Stage> {
        let root = root.canonicalize()?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let mut stage = Stage { dir: dir.canonicalize()?, files: Vec::new() };
        live().push(stage.dir.clone());
        for f in files {
            let Ok(abs) = f.canonicalize() else { continue };
            let Ok(rel) = abs.strip_prefix(&root) else { continue };
            if is_state(rel) || !abs.is_file() {
                continue;
            }
            let to = stage.dir.join(rel);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&abs, &to)?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !stage.files.contains(&rel) {
                stage.files.push(rel);
            }
        }
        Ok(stage)
    }

    /// The first way this session left the reading room, if any.
    pub fn escape(&self, touched: &[Touch]) -> Option<String> {
        escape(&self.dir, touched)
    }
}

/// Lexical normalization: resolve `.` and `..` without touching the disk (a path
/// that does not exist still has to stay inside the stage).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Pure check, for testing: did any tool call leave `stage`?
pub fn escape(stage: &Path, touched: &[Touch]) -> Option<String> {
    for t in touched {
        if !READ_TOOLS.contains(&t.tool.as_str()) {
            return Some(format!("used the `{}` tool", t.tool));
        }
        if let Some(p) = t.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            let raw = Path::new(p.strip_prefix("~").map(|_| "/~").unwrap_or(p));
            let abs = if raw.is_absolute() { normalize(raw) } else { normalize(&stage.join(raw)) };
            if !abs.starts_with(stage) {
                return Some(format!("`{}` {p}, outside the doc and its links", t.tool));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(tool: &str, path: Option<&str>) -> Touch {
        Touch { tool: tool.into(), path: path.map(String::from) }
    }

    #[test]
    fn the_audit_allows_reading_inside_and_nothing_else() {
        let s = Path::new("/tmp/egm-dsh-stage-1");
        for ok in [t("read", Some("docs/designs/x.md")), t("read", Some("/tmp/egm-dsh-stage-1/jot/cli.py")), t("glob", None),
                   t("grep", Some(".")), t("working_directory", None), t("read", Some("docs/../jot/cli.py"))] {
            assert_eq!(escape(s, std::slice::from_ref(&ok)), None, "{ok:?}");
        }
        for bad in [t("read", Some("/content/repo/.egm-dsh/reviews/x-1.json")), t("read", Some("../../etc/passwd")),
                    t("glob", Some("/")), t("grep", Some("/content/repo")), t("read", Some("~/.bashrc")),
                    t("read", Some("/tmp/egm-dsh-stage-1/../egm-dsh-stage-2/x")), t("bash", None), t("web_fetch", None),
                    t("subagent", None), t("write", Some("x.md"))] {
            assert!(escape(s, &[t("read", Some("docs/x.md")), bad.clone()]).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn the_stage_holds_only_the_given_repository_files() {
        let root = std::env::temp_dir().join(format!("egm-stage-root-{}", crate::project::now_nanos()));
        for (rel, text) in [("docs/designs/x.md", "doc"), ("jot/cli.py", "code"), ("README.md", "readme"),
                            (".egm-dsh/reviews/x-1.json", "{}"), (".git/HEAD", "ref")] {
            std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
            std::fs::write(root.join(rel), text).unwrap();
        }
        let outside = std::env::temp_dir().join(format!("egm-outside-{}.md", crate::project::now_nanos()));
        std::fs::write(&outside, "outside").unwrap();
        let files = [root.join("docs/designs/x.md"), root.join("jot/cli.py"), root.join(".egm-dsh/reviews/x-1.json"),
                     root.join(".git/HEAD"), outside.clone(), root.join("missing.py")];
        let stage = Stage::new(&root, &files).unwrap();
        assert_eq!(stage.files, vec!["docs/designs/x.md", "jot/cli.py"]);
        assert_eq!(std::fs::read_to_string(stage.dir.join("jot/cli.py")).unwrap(), "code");
        assert!(!stage.dir.join("README.md").exists() && !stage.dir.join(".egm-dsh").exists() && !stage.dir.join(".git").exists());
        let dir = stage.dir.clone();
        drop(stage);
        assert!(!dir.exists(), "the stage is removed afterwards");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }
}
