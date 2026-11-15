//! The built-in reviewer: one DeepSeek `/chat/completions` request per turn, from
//! egm-dsh itself, for the read-only goldfish roles (reader, critic, verifier).
//!
//! It replaces a DeepSeek Harness session where a session adds nothing: these
//! roles only read the doc and the files it links, which egm-dsh already gathers
//! and embeds in the prompt. So there is no Node process to start (about 4 s per
//! session, measured), no agent system prompt or tool schemas per request, and
//! no tool round trips. The roles that write code (elephant, tester, builder) keep
//! dsh, with its sandbox, tools and session resume.
//!
//! Isolation is structural, not audited: the request is a pure function of the
//! model, the effort and the prompt (`request_body`), it carries no tools and no
//! history, and a turn that asks to resume a session is refused. Lean:
//! `EgmBuiltin` in `proofs/EgmGate.lean`.

use std::time::Duration;

use anyhow::{anyhow, bail};
use serde_json::{json, Value};

use super::{openai::parse_usage, Agent, Effort, Reply, Turn};

pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

pub struct Builtin {
    /// `DEEPSEEK_BASE_URL` (the variable dsh reads too) or DeepSeek's API.
    pub base_url: String,
    pub model: String,
    pub effort: Option<Effort>,
    /// Environment variable holding the API key (`DEEPSEEK_API_KEY`).
    pub api_key_env: String,
}

impl Builtin {
    pub fn new(model: Option<String>, effort: Option<Effort>) -> Builtin {
        Builtin {
            base_url: std::env::var("DEEPSEEK_BASE_URL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            model: model.unwrap_or_else(|| crate::project::FLASH.into()),
            effort,
            api_key_env: "DEEPSEEK_API_KEY".into(),
        }
    }
}

/// The whole request: model, effort and one user message. Nothing else reaches the
/// model — no system prompt, no tools, no earlier turns (`EgmBuiltin.request_memoryless`).
/// The same thinking and effort settings dsh sends, in the OpenAI format.
pub fn request_body(model: &str, effort: Option<Effort>, prompt: &str) -> Value {
    let mut body = json!({"model": model, "messages": [{"role": "user", "content": prompt}], "stream": false});
    match effort {
        Some(Effort::Off) => body["thinking"] = json!({"type": "disabled"}),
        Some(e) => {
            body["thinking"] = json!({"type": "enabled"});
            body["reasoning_effort"] = json!(e.as_str());
        }
        None => body["thinking"] = json!({"type": "enabled"}),
    }
    body
}

/// Whether a failed attempt is worth repeating: rate limits, server trouble and a
/// dropped connection. A refusal (bad key, no balance, bad request) is not.
pub fn retryable(status: Option<u16>) -> bool {
    match status {
        None => true,
        Some(s) => s == 429 || s >= 500,
    }
}

/// The reply text, or why there is none. A cut-off or interrupted answer is an
/// error rather than half a JSON object.
pub fn reply_text(v: &Value) -> Result<String, (String, bool)> {
    let choice = &v["choices"][0];
    match choice["finish_reason"].as_str().unwrap_or("stop") {
        "stop" => {}
        "length" => return Err(("the answer hit the output limit (finish_reason: length)".into(), false)),
        r @ ("insufficient_system_resource" | "aborted") => return Err((format!("DeepSeek interrupted the answer ({r})"), true)),
        r => return Err((format!("no answer (finish_reason: {r})"), false)),
    }
    choice["message"]["content"].as_str().map(str::to_string).ok_or_else(|| ("no message content in the response".into(), false))
}

const ATTEMPTS: usize = 3;

impl Agent for Builtin {
    fn name(&self) -> String {
        let effort = self.effort.map(|e| format!("/{}", e.as_str())).unwrap_or_default();
        format!("built-in · {}{effort}", self.model)
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn effort(&self) -> String {
        self.effort.map(|e| e.as_str().to_string()).unwrap_or_else(|| "default".into())
    }

    fn reads_files(&self) -> bool {
        false
    }

    fn can_write(&self) -> bool {
        false
    }

    fn run(&self, turn: Turn) -> anyhow::Result<Reply> {
        if turn.session.is_some() {
            bail!("the built-in reviewer is memoryless and cannot resume a session");
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let key = std::env::var(&self.api_key_env).map_err(|_| anyhow!("set {} to call {url}", self.api_key_env))?;
        let body = request_body(&self.model, self.effort, &turn.prompt);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(20 * 60)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(2u64.pow(2 * attempt as u32)));   // 4 s, 16 s
            }
            let resp = agent.post(&url).header("Authorization", &format!("Bearer {key}")).send_json(&body);
            let (status, value) = match resp {
                Ok(mut r) => {
                    let status = r.status().as_u16();
                    let value: Value = r.body_mut().read_json().unwrap_or(Value::Null);
                    (Some(status), value)
                }
                Err(e) => {
                    last = format!("POST {url}: {e}");
                    continue;   // a dropped connection or timeout: retry
                }
            };
            let status = status.unwrap_or(0);
            if status != 200 {
                let msg = value["error"]["message"].as_str().unwrap_or("").to_string();
                last = format!("POST {url}: http status: {status}{}", if msg.is_empty() { String::new() } else { format!(" ({msg})") });
                if retryable(Some(status)) {
                    continue;
                }
                bail!(crate::agent::with_hint(last));
            }
            match reply_text(&value) {
                Ok(text) => {
                    return Ok(Reply { text, session_id: None, tool_calls: 0, usage: parse_usage(&value["usage"]), touched: vec![] });
                }
                Err((why, again)) => {
                    last = format!("POST {url}: {why}");
                    if !again {
                        bail!(last);
                    }
                }
            }
        }
        bail!(crate::agent::with_hint(format!("{last} (after {ATTEMPTS} attempts)")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_the_prompt_and_nothing_else() {
        // EgmBuiltin.request_memoryless, no_tools
        let b = request_body("deepseek-v4-pro", Some(Effort::Low), "PROMPT");
        assert_eq!(b["messages"], json!([{"role": "user", "content": "PROMPT"}]));
        assert!(b.get("tools").is_none() && b.get("system").is_none());
        assert_eq!((b["thinking"]["type"].as_str(), b["reasoning_effort"].as_str()), (Some("enabled"), Some("low")));
        let off = request_body("deepseek-flash", Some(Effort::Off), "P");
        assert_eq!(off["thinking"]["type"], "disabled");
        assert!(off.get("reasoning_effort").is_none());
        let default = request_body("deepseek-flash", None, "P");
        assert!(default.get("reasoning_effort").is_none(), "no effort: DeepSeek's default (high), as with dsh");
        assert_eq!(request_body("m", Some(Effort::High), "x"), request_body("m", Some(Effort::High), "x"));
    }

    #[test]
    fn only_rate_limits_server_errors_and_dropped_connections_are_retried() {
        assert!(retryable(None) && retryable(Some(429)) && retryable(Some(500)) && retryable(Some(503)));
        assert!(!retryable(Some(400)) && !retryable(Some(401)) && !retryable(Some(402)) && !retryable(Some(422)));
    }

    #[test]
    fn a_cut_off_answer_is_an_error_not_half_a_json_object() {
        let ok = json!({"choices": [{"finish_reason": "stop", "message": {"content": "{}"}}]});
        assert_eq!(reply_text(&ok).unwrap(), "{}");
        assert!(matches!(reply_text(&json!({"choices": [{"finish_reason": "length", "message": {"content": "{\"a\""}}]})), Err((_, false))));
        assert!(matches!(reply_text(&json!({"choices": [{"finish_reason": "insufficient_system_resource", "message": {"content": ""}}]})), Err((_, true))));
    }

    #[test]
    fn a_resumed_session_is_refused() {
        let b = Builtin { base_url: "http://127.0.0.1:9".into(), model: "m".into(), effort: None, api_key_env: "PATH".into() };
        let turn = Turn {
            prompt: "x".into(),
            session: Some("s1".into()),
            access: crate::agent::Access::ReadOnly,
            role: crate::agent::Role::Verify,
            doc: None,
            workdir: None,
        };
        assert!(b.run(turn).unwrap_err().to_string().contains("memoryless"));
    }
}
