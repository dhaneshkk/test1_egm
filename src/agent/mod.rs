//! Agents that do the thinking. The CLI owns files, state and the gate;
//! agents only answer turns.

pub mod builtin;
pub mod dsh;
pub mod mock;
pub mod openai;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::goldfish::Stage;

/// File access a turn is allowed. For the DeepSeek Harness backend this maps to
/// `DSH_PERMISSION_MODE`, which DSH enforces in its own sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    WorkspaceWrite,
}

impl Access {
    pub fn dsh_mode(self) -> &'static str {
        match self {
            Access::ReadOnly => "read-only",
            Access::WorkspaceWrite => "workspace-write",
        }
    }
}

/// Who is speaking. Each role can run on its own agent, model and effort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoleName {
    Elephant,
    Goldfish,
    /// The goldfish's critic stage, which hunts for flaws rather than reading for sense.
    GoldfishCritic,
    /// Checks the goldfish's alarms before they block a build.
    GoldfishVerifier,
    Builder,
    Reviewer,
}

impl RoleName {
    pub const ALL: [RoleName; 6] = [
        RoleName::Elephant,
        RoleName::Goldfish,
        RoleName::GoldfishCritic,
        RoleName::GoldfishVerifier,
        RoleName::Builder,
        RoleName::Reviewer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RoleName::Elephant => "elephant",
            RoleName::Goldfish => "goldfish",
            RoleName::GoldfishCritic => "goldfish-critic",
            RoleName::GoldfishVerifier => "goldfish-verifier",
            RoleName::Builder => "builder",
            RoleName::Reviewer => "reviewer",
        }
    }

    /// Roles that only read. They can run with a trimmed tool list.
    pub fn read_only(self) -> bool {
        !matches!(self, RoleName::Builder)
    }
}

/// DeepSeek Harness reasoning effort (`reasoningEffort`); `off` disables thinking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Effort {
    Off,
    Low,
    High,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Low => "low",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }
}

/// What the turn is for. Real agents only see the prompt; the mock agent uses
/// the role to play its scripted part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Elephant,
    Draft(&'static str),
    /// Several doc sections in one turn.
    DraftAll,
    /// Answer the goldfish's open questions from the repository, or escalate them.
    Escalate,
    Goldfish(Stage),
    /// All four goldfish stages answered in one session.
    GoldfishCombined,
    /// Judge the alarms a goldfish raised.
    Verify,
    Builder,
    Critic,
    /// Write the acceptance tests before any code exists.
    Tester,
    /// Judge whether each test really checks its criterion.
    TestReview,
}

#[derive(Clone, Debug)]
pub struct Turn {
    pub prompt: String,
    /// Continue this conversation; `None` starts a fresh, memoryless one.
    pub session: Option<String>,
    pub access: Access,
    pub role: Role,
    /// The design doc the turn concerns, when there is one.
    pub doc: Option<PathBuf>,
    /// Run in this folder instead of the project root: a staged reviewer that may
    /// touch nothing outside it (`stage.rs`). `None` for everyone else.
    pub workdir: Option<PathBuf>,
}

/// One tool call an agent reported, for the stage audit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Touch {
    pub tool: String,
    /// The file or directory it named, if any.
    pub path: Option<String>,
}

/// Tokens one turn consumed, split the way DeepSeek bills them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input served from DeepSeek's prefix cache (billed at the cache-hit rate).
    pub cache_hit: u64,
    /// Input not in the cache (billed at the cache-miss rate).
    pub cache_miss: u64,
    /// Output, including reasoning tokens.
    pub output: u64,
    /// Model requests the turn made (one per agent step).
    pub requests: u64,
    /// True when the counts are estimated from text length rather than reported by the provider.
    #[serde(default)]
    pub estimated: bool,
    /// Of `output`, the reasoning tokens, when the provider reports them (the built-in
    /// reviewer does; dsh does not). Shows where output goes, stage by stage.
    #[serde(default)]
    pub reasoning: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.cache_hit + self.cache_miss + self.output
    }

    pub fn add(&mut self, other: &Usage) {
        self.cache_hit += other.cache_hit;
        self.cache_miss += other.cache_miss;
        self.output += other.output;
        self.requests += other.requests;
        self.estimated |= other.estimated;
        self.reasoning += other.reasoning;
    }
}

#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub text: String,
    pub session_id: Option<String>,
    pub tool_calls: usize,
    pub usage: Usage,
    /// Every tool call, in order (agents that report them).
    pub touched: Vec<Touch>,
}

/// A plain explanation for provider refusals that no retry will fix.
pub fn provider_hint(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.contains("insufficient balance") || t.contains("status: 402") {
        Some("the DeepSeek account has no credit left: top it up at https://platform.deepseek.com/top_up (`egm-dsh doctor` shows the balance)")
    } else if t.contains("authentication fails") || t.contains("status: 401") {
        Some("the API key was rejected: check DEEPSEEK_API_KEY")
    } else {
        None
    }
}

/// `text`, followed by the plain explanation when there is one.
pub fn with_hint(text: String) -> String {
    match provider_hint(&text) {
        Some(h) => format!("{text} — {h}"),
        None => text,
    }
}

/// The DeepSeek account balance (`GET /user/balance`): whether it can pay for calls,
/// and each currency's total. `None` when no key is set.
pub fn deepseek_balance() -> Option<Result<(bool, String), String>> {
    let key = std::env::var("DEEPSEEK_API_KEY").ok().filter(|k| !k.trim().is_empty())?;
    let base = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".into());
    let url = format!("{}/user/balance", base.trim_end_matches('/'));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(8))).build().into();
    let got = agent.get(&url).header("Authorization", &format!("Bearer {key}")).call();
    Some(match got {
        Ok(mut r) => match r.body_mut().read_json::<serde_json::Value>() {
            Ok(v) => {
                let totals: Vec<String> = v["balance_infos"]
                    .as_array()
                    .map(|a| a.iter().map(|b| format!("{} {}", b["total_balance"].as_str().unwrap_or("?"), b["currency"].as_str().unwrap_or(""))).collect())
                    .unwrap_or_default();
                Ok((v["is_available"].as_bool().unwrap_or(false), if totals.is_empty() { "no balance listed".into() } else { totals.join(", ") }))
            }
            Err(e) => Err(format!("unreadable answer from {url}: {e}")),
        },
        Err(e) => Err(with_hint(format!("GET {url}: {e}"))),
    })
}

pub trait Agent: Send + Sync {
    fn name(&self) -> String;
    /// The model this agent asks for, for usage accounting and pricing.
    fn model(&self) -> String;
    /// Reasoning effort, for feature records; `default` when the agent has none.
    fn effort(&self) -> String {
        "default".into()
    }
    /// Whether the agent can open files itself. When false, callers embed the
    /// files the turn may use directly in the prompt.
    fn reads_files(&self) -> bool;
    /// Whether the agent can change the workspace (needed by `egm-dsh build`).
    fn can_write(&self) -> bool;
    fn run(&self, turn: Turn) -> anyhow::Result<Reply>;
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn provider_refusals_get_a_plain_explanation() {
        let dsh = with_hint("DeepSeek Harness turn failed: Insufficient Balance (request_id: 84)".into());
        assert!(dsh.ends_with("— the DeepSeek account has no credit left: top it up at https://platform.deepseek.com/top_up (`egm-dsh doctor` shows the balance)"));
        assert!(with_hint("POST http://x/chat/completions: http status: 402".into()).contains("no credit left"));
        assert!(with_hint("POST http://x/chat/completions: http status: 401".into()).contains("API key was rejected"));
        assert_eq!(with_hint("dsh exited with signal 15".into()), "dsh exited with signal 15", "anything else is left as it was");
    }
}
