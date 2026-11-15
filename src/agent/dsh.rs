//! DeepSeek Harness as the coding agent, driven through its headless profile:
//!
//! `dsh --profile headless --patch <role>.patch.yml --json [--session-id <id>] -`
//!
//! The task goes over stdin, newline-delimited run events come back on stdout,
//! and the first `session` event names the Session a later turn can resume
//! with `--session-id`. File access is set per turn with `DSH_PERMISSION_MODE`;
//! under `read-only`, DSH's sandbox denies writes, and headless mode has no
//! approval channel, so escalation requests are refused too.
//!
//! Each role gets its own patch overlay, which sets the model
//! (`agent-default-model`), the reasoning effort (`llm-deepseek`), turns off
//! the LLM session-title side request, and for read-only roles removes every
//! tool plugin except file reading and search. A resumed session follows the
//! patch's model, so the elephant's model can change between turns.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context};
use serde_json::Value;

use super::{Agent, Effort, Reply, Role, Turn};
use crate::ui;

/// Tool plugins removed for read-only roles. What stays: `read`, `glob`, `grep`,
/// `read_image`, `working_directory` (plus `write`/`edit`, which the read-only
/// sandbox denies). Measured on dsh 0.2.1-rc.2: tool schemas fall from about
/// 5,100 to 1,200 tokens per request.
pub const LEAN_DISABLE: [&str; 12] = [
    "tool-bash", "tool-jobs", "tool-skill", "tool-goal", "plan-mode", "tool-subagent-control",
    "tool-subagent-list-agents", "tool-subagent", "tool-subagent-fork", "tool-workflow", "tool-todo", "tool-web",
];

pub struct Dsh {
    /// Command that launches dsh, e.g. `["npx", "-y", "@deepseek-ai/dsh@latest"]` or `["dsh"]`.
    pub command: Vec<String>,
    pub root: PathBuf,
    /// Model id for `agent-default-model`; `None` keeps the composition default (`deepseek-flash`).
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub lean_tools: bool,
    /// Where this role's patch overlay is written.
    pub patch_path: PathBuf,
}

/// The overlay for one role. Pure, for testing.
pub fn patch_yaml(model: Option<&str>, effort: Option<Effort>, lean_tools: bool) -> String {
    let mut s = String::from("# Written by egm-dsh for one role. Regenerated on every turn; edit .egm-dsh/config.json instead.\n");
    if let Some(m) = model {
        s.push_str(&format!("- id: agent-default-model\n  config:\n    provider: deepseek-official\n    model: {}\n", serde_json::to_string(m).unwrap()));
    }
    if let Some(e) = effort {
        s.push_str(&format!("- id: llm-deepseek\n  config:\n    reasoningEffort: \"{}\"\n", e.as_str()));
    }
    // A title is useless for one-shot egm turns and costs a model request per new session.
    s.push_str("- id: session-title-llm\n  disabled: true\n");
    if lean_tools {
        for id in LEAN_DISABLE {
            s.push_str(&format!("- id: {id}\n  disabled: true\n"));
        }
    }
    s
}

impl Dsh {
    /// Write this role's overlay. A staged reviewer always gets the lean (read-only)
    /// tool list, in its own file so it never races the role's normal overlay.
    fn write_patch(&self, staged: bool) -> anyhow::Result<PathBuf> {
        if let Some(dir) = self.patch_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let path = if staged { self.patch_path.with_extension("staged.yml") } else { self.patch_path.clone() };
        std::fs::write(&path, patch_yaml(self.model.as_deref(), self.effort, self.lean_tools || staged))?;
        Ok(path)
    }
}

impl Agent for Dsh {
    fn name(&self) -> String {
        let effort = self.effort.map(|e| format!("/{}", e.as_str())).unwrap_or_default();
        format!("dsh · {}{effort}{}", self.model(), if self.lean_tools { " · lean tools" } else { "" })
    }

    fn model(&self) -> String {
        self.model.clone().unwrap_or_else(|| "deepseek-flash".into())
    }

    fn effort(&self) -> String {
        self.effort.map(|e| e.as_str().to_string()).unwrap_or_else(|| "default".into())
    }

    fn reads_files(&self) -> bool {
        true
    }

    fn can_write(&self) -> bool {
        true
    }

    fn run(&self, turn: Turn) -> anyhow::Result<Reply> {
        // Parallel dsh processes can race on the profile file dsh rewrites at
        // startup. A run that failed before its session started sent nothing to
        // the model, so it is safe to start again.
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.run_once(&turn) {
                Err(RunError::Startup(msg)) if attempt < 4 => {
                    ui::tool("retry", &format!("dsh failed at startup ({}); starting it again", msg.lines().next().unwrap_or("")));
                    std::thread::sleep(std::time::Duration::from_millis(150 * attempt as u64 + (std::process::id() % 97) as u64));
                }
                Err(RunError::Startup(msg)) | Err(RunError::Other(msg)) => bail!("{msg}"),
                Ok(r) => return Ok(r),
            }
        }
    }
}

/// Why a dsh run failed: at startup (safe to retry) or during the turn.
#[derive(Debug)]
enum RunError {
    Startup(String),
    Other(String),
}

/// A failure is a startup failure when dsh never announced a session and its
/// stderr says startup failed. Separate for testing.
fn is_startup_failure(saw_session: bool, stderr: &str) -> bool {
    !saw_session && (stderr.contains("startup failed") || stderr.contains("did not activate"))
}

impl Dsh {
    fn run_once(&self, turn: &Turn) -> Result<Reply, RunError> {
        let other = |e: anyhow::Error| RunError::Other(format!("{e:#}"));
        let (program, prefix) = self.command.split_first().ok_or_else(|| RunError::Other("dsh command is empty".into()))?;
        // A goldfish reading room forces the lean (read-only) tools: its isolation is audited.
        // The elephant's view is a cost control for a trusted role, so it keeps its preset's tools.
        let elephant = matches!(turn.role, Role::Elephant | Role::Draft(_) | Role::DraftAll | Role::Escalate);
        let patch = self.write_patch(turn.workdir.is_some() && !elephant).map_err(other)?;
        let mut cmd = Command::new(program);
        // Launcher flags (`--patch`) must come before the app's own flags (`--json`).
        cmd.args(prefix).args(["--profile", "headless", "--patch"]).arg(patch).arg("--json");
        if let Some(id) = &turn.session {
            cmd.args(["--session-id", id]);
        }
        cmd.arg("-")
            .current_dir(turn.workdir.as_ref().unwrap_or(&self.root))
            .env("DSH_PERMISSION_MODE", turn.access.dsh_mode())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group (killed as a whole on close or Ctrl-C) and, on Linux,
        // killed by the kernel if egm-dsh itself dies.
        crate::session::prepare_child(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start `{}`; is DeepSeek Harness installed?", self.command.join(" ")))
            .map_err(other)?;
        crate::session::attach_pid(child.id());

        // dsh may exit at startup before reading stdin; a broken pipe here is not an error yet.
        let _ = child.stdin.take().expect("piped stdin").write_all(turn.prompt.as_bytes());
        let mut stderr = child.stderr.take().expect("piped stderr");
        let err_reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });

        let mut reply = Reply::default();
        let mut failure: Option<String> = None;
        // Read to EOF: closing the pipe early makes dsh die with EPIPE mid-turn.
        for line in BufReader::new(child.stdout.take().expect("piped stdout")).lines() {
            let line = line.map_err(|e| other(e.into()))?;
            let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
            apply_event(&event, &mut reply, &mut failure);
        }
        let status = child.wait().map_err(|e| other(e.into()))?;
        let stderr = err_reader.join().unwrap_or_default();

        if let Some(message) = failure {
            return Err(RunError::Other(crate::agent::with_hint(format!("DeepSeek Harness turn failed: {message}"))));
        }
        if !status.success() {
            let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
            let msg = format!("dsh exited with {status}: {}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
            if is_startup_failure(reply.session_id.is_some(), &stderr) {
                let first = stderr.lines().find(|l| l.contains("startup failed") || l.contains("Error")).unwrap_or("startup failed").trim().to_string();
                return Err(RunError::Startup(first));
            }
            return Err(RunError::Other(msg));
        }
        Ok(reply)
    }
}

/// Fold one headless run event into the reply. Separate for testing.
///
/// dsh's `usage` follows the Messages API: `inputTokens` is input NOT read from
/// the cache, `cacheReadTokens` is input that was, and `outputTokens` includes reasoning.
pub fn apply_event(event: &Value, reply: &mut Reply, failure: &mut Option<String>) {
    match event["type"].as_str() {
        Some("session") => reply.session_id = event["sessionId"].as_str().map(String::from),
        Some("tool_call") => {
            reply.tool_calls += 1;
            let tool = event["tool"].as_str().unwrap_or("?");
            ui::tool(tool, &summarize(&event["input"]));
            let path = ["file_path", "path", "filePath"].iter().find_map(|k| event["input"][*k].as_str()).map(String::from);
            reply.touched.push(crate::agent::Touch { tool: tool.into(), path });
        }
        Some("status") => {
            if event["phase"] == "step_end" {
                reply.usage.requests += 1;
                let u = &event["usage"];
                reply.usage.cache_miss += u["inputTokens"].as_u64().unwrap_or(0);
                reply.usage.cache_hit += u["cacheReadTokens"].as_u64().unwrap_or(0);
                reply.usage.output += u["outputTokens"].as_u64().unwrap_or(0);
            }
            if event["phase"] == "turn_end" && event["reason"]["kind"] != "completed" {
                let msg = event["reason"]["error"]["message"].as_str().unwrap_or("turn did not complete");
                *failure = Some(msg.to_string());
            }
        }
        Some("final") => reply.text = event["text"].as_str().unwrap_or_default().to_string(),
        Some("error") => *failure = Some(event["message"].as_str().unwrap_or("error").to_string()),
        _ => {}
    }
}

fn summarize(input: &Value) -> String {
    for key in ["file_path", "path", "pattern", "command"] {
        if let Some(s) = input[key].as_str() {
            let s = s.lines().next().unwrap_or(s);
            return if s.chars().count() > 70 { format!("{}…", s.chars().take(70).collect::<String>()) } else { s.to_string() };
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_a_real_event_stream() {
        // Shape recorded from dsh 0.2.1-rc.2.
        let stream = [
            r#"{"type":"session","sessionId":"session-1","cwd":"/w"}"#,
            r#"{"type":"tool_call","callId":"t1","tool":"read","input":{"file_path":"a.md"}}"#,
            r#"{"type":"status","phase":"step_end","turn":1,"step":1,"usage":{"inputTokens":10,"outputTokens":5,"totalTokens":915,"cacheReadTokens":900}}"#,
            r#"{"type":"status","phase":"step_end","turn":1,"step":2,"usage":{"inputTokens":40,"outputTokens":7,"totalTokens":47}}"#,
            r#"{"type":"status","phase":"turn_end","turn":1,"reason":{"kind":"completed"}}"#,
            r#"{"type":"final","text":"done"}"#,
        ];
        let (mut r, mut f) = (Reply::default(), None);
        for l in stream {
            apply_event(&serde_json::from_str(l).unwrap(), &mut r, &mut f);
        }
        assert_eq!((r.session_id.as_deref(), r.tool_calls, r.text.as_str()), (Some("session-1"), 1, "done"));
        assert_eq!((r.usage.cache_hit, r.usage.cache_miss, r.usage.output, r.usage.requests), (900, 50, 12, 2));
        assert!(f.is_none());
        assert_eq!(r.touched, vec![crate::agent::Touch { tool: "read".into(), path: Some("a.md".into()) }], "every tool call is kept for the stage audit");
    }

    #[test]
    fn only_startup_failures_are_retried() {
        let race = "dsh: startup failed: 1 required plugin did not activate\n  include (required)\n  TypeError: config file must be a top-level array";
        assert!(is_startup_failure(false, race));
        assert!(!is_startup_failure(true, race), "once a session started, the model may have been called");
        assert!(!is_startup_failure(false, "Error: no API key"));
    }

    #[test]
    fn reports_turn_errors() {
        let e: Value = serde_json::from_str(r#"{"type":"status","phase":"turn_end","turn":1,"reason":{"kind":"error","error":{"message":"no API key","code":"MISSING_CREDENTIAL"}}}"#).unwrap();
        let (mut r, mut f) = (Reply::default(), None);
        apply_event(&e, &mut r, &mut f);
        assert_eq!(f.as_deref(), Some("no API key"));
    }

    #[test]
    fn patch_sets_model_effort_and_trims_tools() {
        let y = patch_yaml(Some("deepseek-v4-pro"), Some(Effort::Off), true);
        assert!(y.contains("model: \"deepseek-v4-pro\""));
        assert!(y.contains("reasoningEffort: \"off\""));
        assert!(y.contains("- id: session-title-llm\n  disabled: true"));
        assert!(y.contains("- id: tool-bash\n  disabled: true"));
        let plain = patch_yaml(None, None, false);
        assert!(!plain.contains("agent-default-model") && !plain.contains("tool-bash"));
        assert!(plain.contains("session-title-llm"));
    }
}
