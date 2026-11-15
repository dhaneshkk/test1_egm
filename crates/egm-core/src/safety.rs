//! Running model-written code safely.
//!
//! egm-dsh runs commands that came from models: the acceptance command over
//! code a builder wrote, single tests a tester wrote, and the `$ ` lines of a
//! doc's `## Examples`, which an elephant drafted. dsh's sandbox does not
//! cover these, because egm-dsh runs them itself. So every one of them:
//!
//! * runs in a scratch copy of the repository, so it cannot change the repo;
//! * gets a cleared environment with only allow-listed variables, and never
//!   one whose name looks like a secret (`DEEPSEEK_API_KEY` included);
//! * runs under bubblewrap when available: read-only root filesystem, only the
//!   scratch directory writable, no network, its own process namespace.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxKind {
    /// bubblewrap if it is installed and works, otherwise none (with a warning on every run).
    #[default]
    Auto,
    /// bubblewrap, required: refuse to run anything without it.
    Bwrap,
    /// No sandbox: scratch copy and cleared environment only.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Safety {
    pub sandbox: SandboxKind,
    /// Extra environment variables passed to commands (never secret-looking ones).
    pub env_pass: Vec<String>,
    /// Builds work on a branch per feature, commit accepted phases, and revert rejected ones.
    pub git: bool,
}

impl Default for Safety {
    fn default() -> Self {
        Safety { sandbox: SandboxKind::Auto, env_pass: vec![], git: true }
    }
}

/// Always passed when set: enough for interpreters and test runners.
const BASE_ENV: [&str; 9] = ["PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ", "USER", "LEAN_PATH"];

/// Names that are never passed, whatever the allow-list says.
pub fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION"].iter().any(|w| n.contains(w))
}

/// The environment a sandboxed command gets.
pub fn allowed_env(safety: &Safety) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for name in BASE_ENV.iter().map(|s| s.to_string()).chain(safety.env_pass.iter().cloned()) {
        if looks_secret(&name) || out.iter().any(|(n, _)| *n == name) {
            continue;
        }
        if let Ok(v) = std::env::var(&name) {
            out.push((name, v));
        }
    }
    out.push(("PYTHONDONTWRITEBYTECODE".into(), "1".into()));
    out.push(("TMPDIR".into(), "/tmp".into()));
    out
}

/// Does bubblewrap work here (installed, and user namespaces allowed)?
pub fn bwrap_works() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--unshare-net", "--die-with-parent", "true"])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// Which sandbox a run will actually use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effective {
    Bwrap,
    None,
}

pub fn effective(safety: &Safety) -> anyhow::Result<Effective> {
    match safety.sandbox {
        SandboxKind::None => Ok(Effective::None),
        SandboxKind::Bwrap if bwrap_works() => Ok(Effective::Bwrap),
        SandboxKind::Bwrap => anyhow::bail!("sandbox is set to bwrap, but bubblewrap is not installed or not allowed here (apt-get install bubblewrap)"),
        SandboxKind::Auto => {
            if bwrap_works() {
                Ok(Effective::Bwrap)
            } else {
                static WARNED: OnceLock<()> = OnceLock::new();
                WARNED.get_or_init(|| crate::ui::warn("no sandbox: bubblewrap is not available, so model-written tests and examples run with your user's file and network access (scratch copy and cleared environment still apply)"));
                Ok(Effective::None)
            }
        }
    }
}

/// Read-only paths that must stay visible although the sandbox replaces /tmp:
/// the toolchain of a program installed under the temp dir (its `bin/..`).
fn tmp_toolchain(program: &str) -> Option<String> {
    let p = Path::new(program);
    let tmp = std::env::temp_dir();
    if p.is_absolute() && (p.starts_with("/tmp") || p.starts_with(&tmp)) {
        let dir = p.parent()?;
        let root = if dir.file_name().is_some_and(|n| n == "bin") { dir.parent()? } else { dir };
        return Some(root.to_string_lossy().to_string());
    }
    None
}

/// Did the harness fail to start the command (as opposed to the command failing)?
/// A check that could not run must never count as a failing (red) test.
pub fn could_not_run(out: &Output) -> Option<String> {
    let err = String::from_utf8_lossy(&out.stderr);
    let first = err.lines().next().unwrap_or("").trim().to_string();
    match out.status.code() {
        Some(126) | Some(127) => Some(if first.is_empty() { "command not found or not executable".into() } else { first }),
        _ if first.starts_with("bwrap: ") => Some(first),
        _ => None,
    }
}

/// The command line that runs `program args` in `workdir` under the sandbox.
pub fn wrap(mode: Effective, env: &[(String, String)], workdir: &Path, program: &str, args: &[String]) -> Command {
    match mode {
        Effective::Bwrap => {
            let mut c = Command::new("bwrap");
            let w = workdir.to_string_lossy().to_string();
            // Order matters: the scratch dir is bound after /tmp is replaced.
            c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]);
            if let Some(extra) = tmp_toolchain(program) {
                c.args(["--ro-bind", &extra, &extra]);
            }
            c.args(["--bind", &w, &w, "--unshare-net", "--unshare-pid", "--die-with-parent", "--clearenv"]);
            for (k, v) in env {
                c.args(["--setenv", k, v]);
            }
            c.args(["--chdir", &w, "--", program]).args(args);
            c.env_clear().envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            c
        }
        Effective::None => {
            let mut c = Command::new(program);
            c.args(args).current_dir(workdir).env_clear().envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            c
        }
    }
}

/// Directories never copied into a scratch copy.
const SKIP: [&str; 9] = [".egm-dsh", ".egm", ".git", "target", "node_modules", ".dsh", "__pycache__", ".venv", ".pytest_cache"];

pub fn scratch_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("egm-dsh-{tag}-{}-{}", std::process::id(), crate::project::now_nanos()))
}

fn copy_tree(from: &Path, to: &Path, skip_files: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let name = e.file_name();
        if SKIP.iter().any(|s| name == *s) || skip_files.iter().any(|s| name == *s) {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_tree(&src, &dst, skip_files)?;
        } else if ft.is_file() {
            std::fs::copy(&src, &dst)?;
        }
        // symlinks are not followed: a link out of the repo would defeat the copy
    }
    Ok(())
}

/// A scratch copy of the repository, without tool directories and without
/// `skip_files` (e.g. local data files an example must not see).
pub fn scratch_copy(root: &Path, tag: &str, skip_files: &[&str]) -> std::io::Result<PathBuf> {
    let dir = scratch_dir(tag);
    copy_tree(root, &dir, skip_files)?;
    Ok(dir)
}

/// Run one command against a scratch copy of the repo, sandboxed. The copy is removed afterwards.
pub fn run_in_copy(safety: &Safety, root: &Path, cmd: &[String]) -> anyhow::Result<Output> {
    let (prog, args) = cmd.split_first().ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let mode = effective(safety)?;
    let dir = scratch_copy(root, "run", &[])?;
    let out = wrap(mode, &allowed_env(safety), &dir, prog, args).output();
    let _ = std::fs::remove_dir_all(&dir);
    out.map_err(|e| anyhow::anyhow!("cannot run `{prog}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_pass() {
        for n in ["DEEPSEEK_API_KEY", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "db_password", "OPENAI_API_KEY"] {
            assert!(looks_secret(n), "{n}");
        }
        for n in ["PATH", "HOME", "PYTHONPATH", "LANG"] {
            assert!(!looks_secret(n), "{n}");
        }
        let s = Safety { env_pass: vec!["DEEPSEEK_API_KEY".into()], ..Safety::default() };
        assert!(allowed_env(&s).iter().all(|(k, _)| !looks_secret(k)), "allow-listing a secret does not pass it");
    }

    #[test]
    fn commands_run_in_a_copy_with_a_clean_environment() {
        let root = scratch_dir("t");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.txt"), "x").unwrap();
        // SAFETY of the test: env var only read by the child we spawn.
        unsafe { std::env::set_var("EGM_TEST_SECRET_TOKEN", "leak") };
        let s = Safety::default();
        let cmd: Vec<String> = ["sh", "-c", "echo changed > f.txt; echo ${EGM_TEST_SECRET_TOKEN:-none}"].map(String::from).to_vec();
        let out = run_in_copy(&s, &root, &cmd).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "none", "secret-looking variables are not passed");
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "x", "the repo is untouched");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_command_that_cannot_start_is_not_a_failing_test() {
        let root = scratch_dir("t");
        std::fs::create_dir_all(&root).unwrap();
        let missing: Vec<String> = vec!["/nonexistent/egm-dsh-no-such-runner".into(), "x".into()];
        let out = run_in_copy(&Safety::default(), &root, &missing);
        // Either spawning fails outright, or the harness reports it could not run it.
        assert!(out.is_err() || could_not_run(out.as_ref().unwrap()).is_some());
        let failing: Vec<String> = ["sh", "-c", "exit 1"].map(String::from).to_vec();
        assert!(could_not_run(&run_in_copy(&Safety::default(), &root, &failing).unwrap()).is_none(), "a real test failure is a failure");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn toolchains_under_tmp_stay_visible() {
        assert_eq!(tmp_toolchain("/tmp/x/lean-4/bin/lean").as_deref(), Some("/tmp/x/lean-4"));
        assert_eq!(tmp_toolchain("/usr/bin/python3"), None);
        assert_eq!(tmp_toolchain("python3"), None);
    }

    #[test]
    fn bwrap_blocks_network_and_writes_outside_the_copy() {
        if !bwrap_works() {
            eprintln!("bubblewrap not available; skipping");
            return;
        }
        let root = scratch_dir("t");
        std::fs::create_dir_all(&root).unwrap();
        let s = Safety { sandbox: SandboxKind::Bwrap, ..Safety::default() };
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let probe = format!(
            "touch {home}/.egm-dsh-sandbox-probe 2>/dev/null && echo home-writable; \
             (exec 3<>/dev/tcp/1.1.1.1/80) 2>/dev/null && echo net-open; echo done"
        );
        let cmd: Vec<String> = vec!["bash".into(), "-c".into(), probe];
        let out = run_in_copy(&s, &root, &cmd).unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("done"), "{text}");
        assert!(!text.contains("home-writable"), "home must be read-only");
        assert!(!text.contains("net-open"), "network must be off");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
