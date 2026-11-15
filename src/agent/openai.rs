//! Any OpenAI-compatible `/chat/completions` endpoint (DeepSeek's API, a local
//! vLLM or llama.cpp server, a gateway). It has no tools, so it cannot open
//! files or build code: callers embed the files a turn may use in the prompt.
//! That makes it a strict goldfish — it sees exactly the doc and its links —
//! and the cheapest one: no agent system prompt and no tool schemas per request.

use std::path::PathBuf;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Agent, Reply, Turn, Usage};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Endpoint {
    /// Base URL; `/chat/completions` is appended. Example: `https://api.deepseek.com`.
    pub base_url: String,
    pub model: String,
    /// Environment variable holding the bearer token. Empty for no auth.
    pub api_key_env: String,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
        }
    }
}

pub struct OpenAi {
    pub endpoint: Endpoint,
    /// Conversation histories live here so elephant sessions survive between runs.
    pub sessions_dir: PathBuf,
}

impl OpenAi {
    fn history_path(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{id}.json"))
    }
}

/// Read usage from a chat-completions response. DeepSeek reports
/// `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`; other servers only
/// `prompt_tokens`, which then counts as all miss.
pub fn parse_usage(u: &Value) -> Usage {
    let prompt = u["prompt_tokens"].as_u64().unwrap_or(0);
    // DeepSeek reports the split at the top level and again under `prompt_tokens_details`.
    let field = |k: &str| u[k].as_u64().or_else(|| u["prompt_tokens_details"][k].as_u64());
    let hit = field("prompt_cache_hit_tokens").or_else(|| u["prompt_tokens_details"]["cached_tokens"].as_u64()).unwrap_or(0);
    let miss = field("prompt_cache_miss_tokens").unwrap_or(prompt.saturating_sub(hit));
    let reasoning = u["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0);
    Usage { cache_hit: hit, cache_miss: miss, output: u["completion_tokens"].as_u64().unwrap_or(0), requests: 1, estimated: false, reasoning }
}

impl Agent for OpenAi {
    fn name(&self) -> String {
        format!("{} @ {}", self.endpoint.model, self.endpoint.base_url)
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn reads_files(&self) -> bool {
        false
    }

    fn can_write(&self) -> bool {
        false
    }

    fn run(&self, turn: Turn) -> anyhow::Result<Reply> {
        let (id, mut messages) = match &turn.session {
            Some(id) => {
                let text = std::fs::read_to_string(self.history_path(id))
                    .with_context(|| format!("unknown session {id}"))?;
                (id.clone(), serde_json::from_str::<Vec<Value>>(&text)?)
            }
            None => (format!("oa-{}", crate::project::now_nanos()), Vec::new()),
        };
        messages.push(json!({"role": "user", "content": turn.prompt}));

        let url = format!("{}/chat/completions", self.endpoint.base_url.trim_end_matches('/'));
        let body = json!({"model": self.endpoint.model, "messages": messages, "temperature": 0});
        let mut req = ureq::post(&url);
        let key;
        if !self.endpoint.api_key_env.is_empty() {
            key = std::env::var(&self.endpoint.api_key_env)
                .map_err(|_| anyhow!("set {} to call {url}", self.endpoint.api_key_env))?;
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let mut resp = req.send_json(&body).map_err(|e| anyhow!(crate::agent::with_hint(format!("POST {url}: {e}"))))?;
        let value: Value = resp.body_mut().read_json()?;
        let text = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("no message content in response from {url}"))?
            .to_string();

        messages.push(json!({"role": "assistant", "content": text}));
        std::fs::create_dir_all(&self.sessions_dir)?;
        std::fs::write(self.history_path(&id), serde_json::to_string_pretty(&messages)?)?;
        Ok(Reply { text, session_id: Some(id), tool_calls: 0, usage: parse_usage(&value["usage"]), touched: vec![] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_deepseek_and_plain_usage() {
        let ds = parse_usage(&json!({"prompt_tokens": 1000, "prompt_cache_hit_tokens": 900, "prompt_cache_miss_tokens": 100, "completion_tokens": 50}));
        assert_eq!((ds.cache_hit, ds.cache_miss, ds.output), (900, 100, 50));
        let plain = parse_usage(&json!({"prompt_tokens": 1000, "completion_tokens": 50}));
        assert_eq!((plain.cache_hit, plain.cache_miss), (0, 1000));
        let nested = parse_usage(&json!({"prompt_tokens": 1000, "prompt_tokens_details": {"cached_tokens": 600}, "completion_tokens": 5}));
        assert_eq!((nested.cache_hit, nested.cache_miss), (600, 400));
    }
}
