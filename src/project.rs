//! Everything egm keeps on disk, under `.egm-dsh/` in the project root.
//!
//! ```text
//! .egm-dsh/config.json            agents, per-role models, prices, commands
//! .egm-dsh/features/<slug>.json   per-feature gate, sessions, build fingerprints
//! .egm-dsh/reviews/<slug>-N.json  full goldfish reports
//! .egm-dsh/ledger.jsonl           one line per goldfish verdict (for metrics)
//! .egm-dsh/usage.jsonl            one line per agent turn: role, model, tokens (for `egm-dsh usage`)
//! .egm-dsh/dsh/<role>.patch.yml   DeepSeek Harness overlay per role (model, effort, tools)
//! .egm-dsh/sessions/              conversation history for agents without their own
//! ```

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

use crate::agent::{dsh::Dsh, mock::Mock, openai::{Endpoint, OpenAi}, Agent, Effort, RoleName};
use crate::cost::{default_prices, Price};
use crate::doc::Doc;
use crate::phase::{Event, Gate};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// DeepSeek Harness headless profile (the default coding agent).
    Dsh,
    /// An OpenAI-compatible /chat/completions endpoint (no tools: design and review only).
    Openai,
    /// Offline scripted agent for the bundled sample.
    Mock,
}

/// The evidence gate's settings (`egm-dsh set-gate`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateSettings {
    /// Minimum share of seeded bugs the frozen tests must catch, in percent.
    pub mutation_min: u32,
    /// At most this many seeded bugs per feature (spread over the change).
    pub mutants: usize,
    /// A test run longer than this with a seeded bug counts as caught.
    pub mutant_timeout_secs: u64,
    /// A command that runs the tests with coverage and writes an lcov file.
    pub coverage_cmd: Vec<String>,
    /// That lcov file, relative to the project.
    pub coverage_file: String,
    /// Paths that make a change high risk, besides the fix lane's protected ones.
    pub risk_paths: Vec<String>,
}

impl Default for GateSettings {
    fn default() -> Self {
        GateSettings { mutation_min: 60, mutants: 30, mutant_timeout_secs: 60, coverage_cmd: vec![], coverage_file: "lcov.info".into(), risk_paths: vec![] }
    }
}

/// What runs the read-only goldfish roles (reader, critic, verifier) of a dsh project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewerClient {
    /// egm-dsh's own `/chat/completions` request: no dsh process, no tools, no history.
    #[default]
    Builtin,
    /// A DeepSeek Harness session in a staged reading room, as before.
    Dsh,
}

/// How one role runs. Every field is optional and falls back to the defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleSpec {
    /// Agent kind for this role; defaults to the top-level `agent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    /// Model id, e.g. `deepseek-flash` or `deepseek-v4-pro`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort for DeepSeek Harness: off, low, high, max.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Give dsh only file-reading tools. Ignored for the builder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lean_tools: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum GoldfishMode {
    /// Four independent sessions, one per stage (the method as written).
    Parallel,
    /// One session answers all four stages (about a quarter of the requests; the stages can sway each other).
    Combined,
}

/// Role routing presets. `balanced` puts the large model where judgment is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Preset {
    /// Pro for the elephant and the builder; Flash for goldfish and review.
    Balanced,
    /// Flash everywhere, cheap efforts, combined goldfish.
    Economy,
    /// Pro everywhere at high effort, full tool lists.
    Quality,
}

pub const FLASH: &str = "deepseek-flash";
pub const PRO: &str = "deepseek-v4-pro";

fn spec(model: &str, effort: Effort, lean: bool) -> RoleSpec {
    RoleSpec { agent: None, model: Some(model.into()), effort: Some(effort), lean_tools: Some(lean) }
}

impl Preset {
    pub fn roles(self) -> BTreeMap<RoleName, RoleSpec> {
        use Effort::*;
        use RoleName::*;
        let rows = match self {
            Preset::Balanced => [
                (Elephant, spec(PRO, High, true)),
                (Goldfish, spec(FLASH, Low, true)),
                (GoldfishCritic, spec(FLASH, High, true)),
                (GoldfishVerifier, spec(PRO, Low, true)),
                (Builder, spec(PRO, High, false)),
                (Reviewer, spec(FLASH, High, true)),
            ],
            Preset::Economy => [
                (Elephant, spec(FLASH, High, true)),
                (Goldfish, spec(FLASH, Off, true)),
                (GoldfishCritic, spec(FLASH, Low, true)),
                (GoldfishVerifier, spec(FLASH, High, true)),
                (Builder, spec(FLASH, High, false)),
                (Reviewer, spec(FLASH, Low, true)),
            ],
            Preset::Quality => [
                (Elephant, spec(PRO, High, false)),
                (Goldfish, spec(PRO, High, false)),
                (GoldfishCritic, spec(PRO, Max, false)),
                (GoldfishVerifier, spec(PRO, High, false)),
                (Builder, spec(PRO, High, false)),
                (Reviewer, spec(PRO, High, false)),
            ],
        };
        rows.into_iter().collect()
    }

    /// Pro goldfish raise few false alarms, so `quality` skips the extra check.
    pub fn verify_alarms(self) -> bool {
        self != Preset::Quality
    }

    pub fn goldfish_mode(self) -> GoldfishMode {
        if self == Preset::Economy { GoldfishMode::Combined } else { GoldfishMode::Parallel }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default agent for every role.
    pub agent: AgentKind,
    /// Per-role agent, model, effort and tools. Missing roles use the defaults.
    pub roles: BTreeMap<RoleName, RoleSpec>,
    pub goldfish_mode: GoldfishMode,
    /// Send alarms that survive grounding to the `goldfish-verifier` role before they block a build.
    pub verify_alarms: bool,
    /// Learned triage in front of the verifier (OpenAI-compatible endpoint). It can only skip verification.
    pub triage: Option<crate::triage::TriageConfig>,
    /// How to launch DeepSeek Harness.
    pub dsh_command: Vec<String>,
    /// Endpoint for the `openai` agent kind; a role's `model` overrides its model.
    pub openai: Endpoint,
    /// Optional second model family for the cross-family critic.
    pub cross_family: Option<Endpoint>,
    /// USD per million tokens at the peak rate, by model id (off-peak is half).
    pub prices: BTreeMap<String, Price>,
    /// Command that must succeed for a build to count. Runs in the project root.
    pub acceptance_cmd: Vec<String>,
    pub designs_dir: String,
    /// How big one step may be before the goldfish refuses it.
    #[serde(default)]
    pub step_limits: crate::steps::StepLimits,
    /// Refuse `build` until the acceptance tests are written, reviewed and frozen (`egm-dsh tests`).
    #[serde(default)]
    pub tests_first: bool,
    /// Runs ONE named check; `{check}`, `{file}` and `{unittest}` are filled in.
    #[serde(default = "default_check_cmd")]
    pub check_cmd: Vec<String>,
    /// Lean 4 for `Check: lean file#theorem`.
    #[serde(default = "default_lean_cmd")]
    pub lean_cmd: Vec<String>,
    /// How model-written commands are run, and whether builds use git.
    #[serde(default)]
    pub safety: crate::safety::Safety,
    /// An adopted goldfish setup directory (relative to the project root); see `bench/tune`.
    #[serde(default)]
    pub goldfish_harness: Option<String>,
    /// Paths (repository-relative) the elephant reads; empty: the whole repository.
    /// With a scope or a `.egmignore`, elephant turns run in a view (scope.rs).
    #[serde(default)]
    pub scope: Vec<String>,
    /// Elephant turns run in a view of the repository (scope.rs) even with no scope and no
    /// `.egmignore`: egm-dsh's own state, VCS internals, build output and huge files stay out.
    /// A real run showed the elephant reading `.egm-dsh/` otherwise.
    #[serde(default = "yes")]
    pub elephant_view: bool,
    /// Per-feature scopes, by feature name.
    #[serde(default)]
    pub scope_overrides: BTreeMap<String, Vec<String>>,
    /// Sequence diagrams in design docs: asked for when drafting, and checked.
    #[serde(default)]
    pub diagrams: crate::diagram::Policy,
    /// PlantUML's own syntax check after the structure check (built-in plantuml-core by default).
    #[serde(default = "yes")]
    pub plantuml_check: bool,
    /// An external PlantUML command for that check instead (e.g. `["java", "-jar", "plantuml.jar"]`);
    /// empty: the built-in checker.
    #[serde(default)]
    pub plantuml_cmd: Vec<String>,
    /// What counts as a tiny fix (`egm-dsh fix`); anything bigger goes to the full lane.
    #[serde(default)]
    pub fix: crate::fix::FixLimits,
    /// Word limit per section asked of the elephant when it drafts or redrafts.
    #[serde(default)]
    pub draft_words: Option<u32>,
    /// Most tokens one feature may use; a step whose estimate would cross it is refused before it starts.
    #[serde(default)]
    pub token_ceiling: Option<u64>,
    /// Per-feature run budgets; every activity and agent session is checked against them.
    #[serde(default)]
    pub budgets: crate::run::Limits,
    /// Per-feature overrides of `budgets`, by feature name.
    #[serde(default)]
    pub budget_overrides: BTreeMap<String, crate::run::Limits>,
    /// What every doc's `## Verification` section must define (empty: no checklist).
    #[serde(default)]
    pub verification: Vec<crate::verify::Item>,
    /// The read-only goldfish roles of a dsh project: the built-in client (default) or dsh.
    #[serde(default)]
    pub reviewers: ReviewerClient,
    /// With built-in reviewers, zoom-out still runs in dsh: it is the one stage that
    /// searches the repository for components that already exist.
    #[serde(default = "yes")]
    pub zoom_out_in_dsh: bool,
    /// Independent readiness readers; readiness blocks when a strict majority object
    /// (`EgmVote`). 1 is the old single reader. Parallel mode only.
    #[serde(default = "three")]
    pub readiness_votes: usize,
    /// Run the invariants stage. Its answer is display only (no alarm comes from it), so it is off by default.
    #[serde(default)]
    pub invariants_stage: bool,
    /// The evidence gate: mutation minimum, coverage command, risk paths.
    #[serde(default)]
    pub gate: GateSettings,
}

impl Config {
    pub fn limits_for(&self, slug: &str) -> crate::run::Limits {
        self.budget_overrides.get(slug).copied().unwrap_or(self.budgets)
    }

    /// How a feature's diagrams are checked.
    pub fn diagram_check(&self, fix_lane: bool) -> crate::diagram::Check {
        crate::diagram::Check { policy: self.diagrams, checker: crate::diagram::checker(self.plantuml_check, &self.plantuml_cmd), exempt: fix_lane }
    }

    /// The elephant's scope for a feature.
    pub fn scope_for(&self, slug: &str) -> Vec<String> {
        self.scope_overrides.get(slug).cloned().unwrap_or_else(|| self.scope.clone())
    }
}

fn three() -> usize {
    3
}

fn yes() -> bool {
    true
}

pub fn default_check_cmd() -> Vec<String> {
    ["python3", "-m", "pytest", "-q", "{check}"].map(String::from).to_vec()
}

pub fn default_lean_cmd() -> Vec<String> {
    vec![std::env::var("LEAN_BIN").unwrap_or_else(|_| "lean".into())]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            agent: AgentKind::Dsh,
            roles: BTreeMap::new(),
            goldfish_mode: GoldfishMode::Parallel,
            verify_alarms: false,
            triage: None,
            dsh_command: vec!["npx".into(), "-y".into(), "@deepseek-ai/dsh@latest".into()],
            openai: Endpoint::default(),
            cross_family: None,
            prices: default_prices(),
            acceptance_cmd: vec![],
            designs_dir: "docs/designs".into(),
            step_limits: Default::default(),
            tests_first: false,
            check_cmd: default_check_cmd(),
            lean_cmd: default_lean_cmd(),
            safety: Default::default(),
            goldfish_harness: None,
            token_ceiling: None,
            scope: vec![],
            elephant_view: true,
            scope_overrides: BTreeMap::new(),
            draft_words: None,
            fix: Default::default(),
            diagrams: Default::default(),
            plantuml_cmd: vec![],
            plantuml_check: true,
            reviewers: ReviewerClient::Builtin,
            zoom_out_in_dsh: true,
            readiness_votes: 3,
            invariants_stage: false,
            gate: GateSettings::default(),
            budgets: Default::default(),
            budget_overrides: BTreeMap::new(),
            verification: Vec::new(),
        }
    }
}

impl Config {
    pub fn apply_preset(&mut self, preset: Preset) {
        self.roles = preset.roles();
        self.goldfish_mode = preset.goldfish_mode();
        self.verify_alarms = preset.verify_alarms();
    }

    /// The spec in force for a role: its own entry, with the critic stage
    /// falling back to the goldfish entry.
    pub fn role(&self, role: RoleName) -> RoleSpec {
        let own = self.roles.get(&role).cloned().unwrap_or_default();
        if role == RoleName::GoldfishCritic {
            let base = self.roles.get(&RoleName::Goldfish).cloned().unwrap_or_default();
            return RoleSpec {
                agent: own.agent.or(base.agent),
                model: own.model.or(base.model),
                effort: own.effort.or(base.effort),
                lean_tools: own.lean_tools.or(base.lean_tools),
            };
        }
        own
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Feature {
    pub slug: String,
    pub title: String,
    pub idea: String,
    /// Repository-relative path of the design doc.
    pub doc: String,
    pub elephant_session: Option<String>,
    pub gate: Gate,
    pub goldfish_runs: u32,
    /// Verdict of the first goldfish review, the prediction `egm-dsh metrics` scores.
    pub first_verdict: Option<String>,
    /// Model that produced the first verdict, so `egm-dsh metrics --by-model` can compare models.
    #[serde(default)]
    pub first_verdict_model: Option<String>,
    /// Outcome recorded with `egm-dsh label`: `rework` or `clean`.
    pub label: Option<String>,
    /// Content hashes of Implementation-map files at build time.
    pub built_files: BTreeMap<String, u64>,
    /// Doc hash at build time, to tell code drift from design change.
    pub built_doc_hash: Option<u64>,
    /// Steps that must be built before this one (`--after`).
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Order of the last completed build across all features; a later build
    /// takes over drift checking for the files it shares with earlier ones.
    #[serde(default)]
    pub built_seq: Option<u64>,
    /// Build phases completed for the doc revision `phase_doc_hash`.
    #[serde(default)]
    pub phases_done: u32,
    #[serde(default)]
    pub phase_doc_hash: Option<u64>,
    /// Acceptance tests written before the code, reviewed, and frozen.
    #[serde(default)]
    pub spec: Option<crate::spec::FrozenSpec>,
    /// The git branch the feature was built on (`egm-dsh/<slug>`).
    #[serde(default)]
    pub git_branch: Option<String>,
    /// `fix` for a tiny fix (`egm-dsh fix`); `None` for the full design lane.
    #[serde(default)]
    pub lane: Option<String>,
}

impl Feature {
    pub fn is_fix(&self) -> bool {
        self.lane.as_deref() == Some("fix")
    }
}

pub struct Project {
    pub root: PathBuf,
    pub config: Config,
}

pub fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

pub fn now_secs() -> u64 {
    (now_nanos() / 1_000_000_000) as u64
}

impl Project {
    pub fn egm_dir(root: &Path) -> PathBuf {
        root.join(".egm-dsh")
    }

    /// Find the project by walking up from `start` to the nearest `.egm-dsh/`.
    pub fn open(start: &Path) -> anyhow::Result<Project> {
        let mut dir = start.canonicalize()?;
        loop {
            let cfg = Self::egm_dir(&dir).join("config.json");
            if cfg.is_file() {
                let config = serde_json::from_str(&std::fs::read_to_string(&cfg)?)
                    .with_context(|| format!("invalid {}", cfg.display()))?;
                return Ok(Project { root: dir, config });
            }
            if !dir.pop() {
                bail!("no .egm-dsh/ found here or above; run `egm-dsh init` first");
            }
        }
    }

    pub fn init(root: &Path, config: Config) -> anyhow::Result<Project> {
        let root = root.canonicalize()?;
        let dir = Self::egm_dir(&root);
        std::fs::create_dir_all(dir.join("features"))?;
        std::fs::create_dir_all(dir.join("reviews"))?;
        std::fs::create_dir_all(root.join(&config.designs_dir))?;
        std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(&config)? + "\n")?;
        std::fs::write(dir.join(".gitignore"), "sessions/\nmock-sessions/\ndsh/\nlock\n")?;
        Ok(Project { root, config })
    }

    /// The agent for one role, configured with that role's model and effort.
    /// The agent for a role. Every run of it is a budgeted, guaranteed-closed
    /// session (`session::Tracked`).
    pub fn agent_for(&self, role: RoleName) -> Box<dyn Agent> {
        Box::new(crate::session::Tracked { inner: self.raw_agent(role), role: role.as_str().to_string() })
    }

    /// Begin a budgeted activity for `slug` (see `session::begin`).
    pub fn begin(&self, slug: &str, gate: crate::phase::Gate, kind: crate::run::Kind) -> anyhow::Result<crate::session::Activity> {
        crate::session::begin(Some(&Self::egm_dir(&self.root)), slug, gate, self.config.limits_for(slug), kind)
    }

    /// Whether `role` runs on the built-in reviewer: a read-only goldfish role of a
    /// project whose agent for it is dsh, with `reviewers: builtin`.
    pub fn builtin_reviewer(&self, role: RoleName) -> bool {
        let goldfish = matches!(role, RoleName::Goldfish | RoleName::GoldfishCritic | RoleName::GoldfishVerifier);
        goldfish && self.config.reviewers == ReviewerClient::Builtin && self.config.role(role).agent.unwrap_or(self.config.agent) == AgentKind::Dsh
    }

    /// The dsh critic for zoom-out, when the other goldfish stages run built in.
    pub fn zoom_out_agent(&self) -> Option<Box<dyn Agent>> {
        (self.config.zoom_out_in_dsh && self.builtin_reviewer(RoleName::GoldfishCritic)).then(|| {
            Box::new(crate::session::Tracked { inner: self.dsh_agent(RoleName::GoldfishCritic), role: RoleName::GoldfishCritic.as_str().to_string() }) as Box<dyn Agent>
        })
    }

    fn dsh_agent(&self, role: RoleName) -> Box<dyn Agent> {
        let dir = Self::egm_dir(&self.root);
        let spec = self.config.role(role);
        Box::new(Dsh {
            command: self.config.dsh_command.clone(),
            root: self.root.clone(),
            model: spec.model,
            effort: spec.effort,
            lean_tools: spec.lean_tools.unwrap_or(false) && role.read_only(),
            patch_path: dir.join("dsh").join(format!("{}.patch.yml", role.as_str())),
        })
    }

    fn raw_agent(&self, role: RoleName) -> Box<dyn Agent> {
        let dir = Self::egm_dir(&self.root);
        let spec = self.config.role(role);
        let lean = spec.lean_tools.unwrap_or(false) && role.read_only();
        if self.builtin_reviewer(role) {
            return Box::new(crate::agent::builtin::Builtin::new(spec.model, spec.effort));
        }
        match spec.agent.unwrap_or(self.config.agent) {
            AgentKind::Dsh => Box::new(Dsh {
                command: self.config.dsh_command.clone(),
                root: self.root.clone(),
                model: spec.model,
                effort: spec.effort,
                lean_tools: lean,
                patch_path: dir.join("dsh").join(format!("{}.patch.yml", role.as_str())),
            }),
            AgentKind::Openai => {
                let mut endpoint = self.config.openai.clone();
                if let Some(m) = spec.model {
                    endpoint.model = m;
                }
                Box::new(OpenAi { endpoint, sessions_dir: dir.join("sessions") })
            }
            AgentKind::Mock => Box::new(Mock {
                root: self.root.clone(),
                sessions_dir: dir.join("mock-sessions"),
                model: spec.model,
                effort: spec.effort,
                lean_tools: lean,
            }),
        }
    }

    pub fn cross_agent(&self) -> Option<Box<dyn Agent>> {
        self.config.cross_family.clone().map(|endpoint| {
            let inner = Box::new(OpenAi { endpoint, sessions_dir: Self::egm_dir(&self.root).join("sessions") }) as Box<dyn Agent>;
            Box::new(crate::session::Tracked { inner, role: "cross-family".into() }) as Box<dyn Agent>
        })
    }

    /// Append one turn's token usage to `.egm-dsh/usage.jsonl`.
    pub fn record_usage(&self, feature: &str, role: &str, agent: &str, model: &str, usage: &crate::agent::Usage) -> anyhow::Result<()> {
        let line = serde_json::json!({
            "ts": now_secs(), "feature": feature, "role": role, "agent": agent, "model": model,
            "cache_hit": usage.cache_hit, "cache_miss": usage.cache_miss, "output": usage.output,
            "requests": usage.requests, "estimated": usage.estimated, "reasoning": usage.reasoning,
        });
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(Self::egm_dir(&self.root).join("usage.jsonl"))?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    fn feature_path(&self, slug: &str) -> PathBuf {
        Self::egm_dir(&self.root).join("features").join(format!("{slug}.json"))
    }

    pub fn load_feature(&self, slug: &str) -> anyhow::Result<Feature> {
        let path = self.feature_path(slug);
        let text = std::fs::read_to_string(&path)
            .map_err(|_| anyhow!("no feature `{slug}`; start one with `egm-dsh elephant {slug} --idea \"...\"`"))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save_feature(&self, f: &Feature) -> anyhow::Result<()> {
        // The proved invariant (proofs/EgmGate.lean) must hold for anything we persist.
        anyhow::ensure!(f.gate.invariant(), "internal error: gate invariant broken for `{}`: {:?}", f.slug, f.gate);
        std::fs::write(self.feature_path(&f.slug), serde_json::to_string_pretty(f)? + "\n")?;
        Ok(())
    }

    pub fn features(&self) -> anyhow::Result<Vec<Feature>> {
        let mut out = Vec::new();
        let dir = Self::egm_dir(&self.root).join("features");
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                if let Some(slug) = e.path().file_stem().and_then(|s| s.to_str()) {
                    out.push(self.load_feature(slug)?);
                }
            }
        }
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(out)
    }

    pub fn doc(&self, f: &Feature) -> anyhow::Result<Doc> {
        Doc::load(&self.root.join(&f.doc))
    }

    /// Fold any edit made to the doc since egm last looked into the gate.
    pub fn sync(&self, f: &mut Feature) -> anyhow::Result<Doc> {
        let doc = self.doc(f)?;
        f.gate = f.gate.step(Event::EditDoc(doc.hash())).expect("EditDoc is never refused");
        Ok(doc)
    }

    pub fn save_review(&self, slug: &str, n: u32, review: &crate::goldfish::Review) -> anyhow::Result<PathBuf> {
        let path = Self::egm_dir(&self.root).join("reviews").join(format!("{slug}-{n}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(review)? + "\n")?;
        Ok(path)
    }

    pub fn append_ledger(&self, entry: serde_json::Value) -> anyhow::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(Self::egm_dir(&self.root).join("ledger.jsonl"))?;
        writeln!(f, "{entry}")?;
        Ok(())
    }

    /// Content hashes of files, skipping tool and cache directories.
    /// In a git repository only tracked and not-ignored files are hashed, so
    /// build output and caches never count as changes.
    pub fn fingerprint(&self) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        if self.config.safety.git
            && let Some(files) = crate::vcs::listed_files(&self.root)
        {
            for rel in files.into_iter().filter(|r| !SKIP.iter().any(|s| r == s || r.starts_with(&format!("{s}/")) || r.contains(&format!("/{s}/")))) {
                if let Ok(bytes) = std::fs::read(self.root.join(&rel)) {
                    out.insert(rel, crate::doc::hash(&String::from_utf8_lossy(&bytes)));
                }
            }
            return out;
        }
        walk(&self.root, &self.root, &mut out);
        out
    }
}

const SKIP: [&str; 9] = [".egm-dsh", ".egm", ".git", "target", "node_modules", "__pycache__", ".pytest_cache", ".venv", ".dsh"];

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, u64>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        if SKIP.iter().any(|s| name == *s) {
            continue;
        }
        if p.is_dir() {
            walk(root, &p, out);
        } else if let Ok(bytes) = std::fs::read(&p) {
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            out.insert(rel, crate::doc::hash(&String::from_utf8_lossy(&bytes)));
        }
    }
}

#[cfg(test)]
mod reviewer_routing_tests {
    use super::*;

    #[test]
    fn only_the_read_only_goldfish_roles_of_a_dsh_project_run_built_in() {
        // EgmBuiltin.writers_keep_dsh, builtin_only_for_goldfish, dsh_setting_keeps_dsh
        let mut p = Project { root: std::env::temp_dir(), config: Config::default() };
        let builtin: Vec<RoleName> = RoleName::ALL.into_iter().filter(|r| p.builtin_reviewer(*r)).collect();
        assert_eq!(builtin, vec![RoleName::Goldfish, RoleName::GoldfishCritic, RoleName::GoldfishVerifier]);
        assert_eq!(p.agent_for(RoleName::Goldfish).name(), format!("built-in · {FLASH}"));
        assert!(p.agent_for(RoleName::Elephant).name().starts_with("dsh"));
        assert!(p.agent_for(RoleName::Builder).name().starts_with("dsh"));
        assert!(p.zoom_out_agent().is_some_and(|a| a.name().starts_with("dsh")), "zoom-out stays in dsh by default");
        p.config.zoom_out_in_dsh = false;
        assert!(p.zoom_out_agent().is_none());
        p.config.reviewers = ReviewerClient::Dsh;
        assert!(RoleName::ALL.into_iter().all(|r| !p.builtin_reviewer(r)));
        p.config.reviewers = ReviewerClient::Builtin;
        p.config.agent = AgentKind::Mock;
        assert!(RoleName::ALL.into_iter().all(|r| !p.builtin_reviewer(r)), "a mock project keeps its mock reviewers");
        let old: Config = serde_json::from_str(&serde_json::to_string(&Config::default()).unwrap().replace("\"reviewers\":\"builtin\",", "")).unwrap();
        assert_eq!(old.reviewers, ReviewerClient::Builtin, "an older config gets the built-in reviewer");
    }
}
