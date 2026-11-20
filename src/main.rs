//! egm — the Elephant-Goldfish Model as a command-line workflow, with
//! DeepSeek Harness as the coding agent.
//!
//! Feed the elephant (a long, read-only design session), test the design doc
//! against goldfish (fresh, memoryless reviewers), and only then let the agent
//! write code. A gate, proved in `proofs/EgmGate.lean`, refuses to build any
//! doc revision a goldfish has not passed. Each role can run on its own model
//! and reasoning effort; `egm-dsh usage` shows what each one cost.


use std::path::PathBuf;

use egm_core::{agent, commands, demo, diagram, evidence, gate, lock, project, safety, session, triage, tuning, verify, ui};

use clap::{Parser, Subcommand};

use agent::{Effort, RoleName};
use project::{AgentKind, Config, GoldfishMode, Preset, Project};

#[derive(Parser)]
#[command(name = "egm-dsh", version, about = "Elephant-Goldfish design workflow with DeepSeek Harness as the coding agent")]
struct Cli {
    /// Also append every event (terminal lines and domain events such as `gate.verdict`) as
    /// JSON lines to this file. The team app's worker reads it.
    #[arg(long, global = true, value_name = "FILE")]
    events: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg {
    Elephant,
    Goldfish,
    GoldfishCritic,
    GoldfishVerifier,
    Builder,
    Reviewer,
}

impl From<RoleArg> for RoleName {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Elephant => RoleName::Elephant,
            RoleArg::Goldfish => RoleName::Goldfish,
            RoleArg::GoldfishCritic => RoleName::GoldfishCritic,
            RoleArg::GoldfishVerifier => RoleName::GoldfishVerifier,
            RoleArg::Builder => RoleName::Builder,
            RoleArg::Reviewer => RoleName::Reviewer,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Set up .egm-dsh/ in the current directory.
    Init {
        #[arg(long, value_enum, default_value = "dsh")]
        agent: AgentKind,
        /// Which model and effort each role gets.
        #[arg(long, value_enum, default_value = "balanced")]
        preset: Preset,
        /// How to launch DeepSeek Harness (default: npx -y @deepseek-ai/dsh@latest).
        #[arg(long, num_args = 1.., value_name = "WORD")]
        dsh_command: Option<Vec<String>>,
        /// Command that must pass for a build to count, e.g. `--acceptance cargo test`.
        #[arg(long, num_args = 1.., value_name = "WORD", allow_hyphen_values = true)]
        acceptance: Option<Vec<String>>,
        /// Refuse builds until the acceptance tests are written, reviewed and frozen.
        #[arg(long)]
        tests_first: bool,
        /// Command that runs ONE check: {check}, {file}, {unittest} are filled in.
        #[arg(long, num_args = 1.., value_name = "WORD", allow_hyphen_values = true)]
        check_cmd: Option<Vec<String>>,
    },
    /// Write the acceptance tests before the code, check each fails now, have a goldfish review them, and freeze them.
    Tests { feature: String },
    /// Run the executable spec now: each named test, each Lean check, each example. Exit 2 on failure.
    Check { feature: String },
    /// Executable-spec settings.
    SetSpec {
        /// on or off
        #[arg(long)]
        tests_first: Option<String>,
        #[arg(long, num_args = 1.., value_name = "WORD", allow_hyphen_values = true)]
        check_cmd: Option<Vec<String>>,
        #[arg(long, num_args = 1.., value_name = "WORD", allow_hyphen_values = true)]
        lean_cmd: Option<Vec<String>>,
    },
    /// How model-written commands run: sandbox, environment allow-list, git.
    SetSafety {
        #[arg(long, value_enum)]
        sandbox: Option<safety::SandboxKind>,
        /// Extra environment variables commands may see (secret-looking names are never passed).
        #[arg(long, num_args = 1.., value_name = "NAME")]
        env_pass: Option<Vec<String>>,
        /// on or off
        #[arg(long)]
        git: Option<String>,
    },
    /// Check that this machine and project are set up safely for real use.
    Doctor,
    /// What every design doc's `## Verification` section must define, checked by egm-dsh:
    /// `--require lean,mcc,sam,chat-completions`, or `--none`.
    SetVerification {
        #[arg(long, value_enum, value_delimiter = ',')]
        require: Vec<verify::Item>,
        #[arg(long)]
        none: bool,
    },
    /// Show what each feature has used of its run budgets.
    Budget { feature: Option<String> },
    /// Set run budgets: for every feature, or with --feature for one.
    SetBudget {
        #[arg(long)]
        feature: Option<String>,
        #[arg(long)]
        goldfish_rounds: Option<u32>,
        /// Elephant conversation turns and draft runs.
        #[arg(long)]
        redraft_turns: Option<u32>,
        /// Build attempts: first build plus retries (test-writing runs count too).
        #[arg(long)]
        build_attempts: Option<u32>,
        #[arg(long)]
        reviews: Option<u32>,
        /// Agent sessions (each dsh run or chat completion is one).
        #[arg(long)]
        model_calls: Option<u32>,
    },
    /// Show which agent, model and effort each role uses.
    Roles,
    /// Change one role's model, effort, agent or tool list.
    SetRole {
        #[arg(value_enum)]
        role: RoleArg,
        /// e.g. deepseek-flash, deepseek-v4-pro
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_enum)]
        effort: Option<Effort>,
        #[arg(long, value_enum)]
        agent: Option<AgentKind>,
        /// Give a read-only role only file-reading tools.
        #[arg(long)]
        lean_tools: Option<bool>,
    },
    /// Point egm at a learned triage model, or turn triage off.
    SetTriage {
        /// Base URL of the OpenAI-compatible server, e.g. http://127.0.0.1:8765/v1
        #[arg(long)]
        url: Option<String>,
        /// Model name, e.g. egm-triage-catboost or egm-triage-sam-mlp
        #[arg(long, default_value = "egm-triage-catboost")]
        model: String,
        /// Share of `keep` answers verified anyway (0 to 1).
        #[arg(long, default_value_t = 0.1)]
        explore: f64,
        #[arg(long)]
        off: bool,
    },
    /// Apply a role preset to an existing project.
    Preset {
        #[arg(value_enum)]
        preset: Preset,
    },
    /// Talk to the elephant: start with --idea, continue with --say. Read-only.
    Elephant {
        feature: String,
        #[arg(long)]
        idea: Option<String>,
        #[arg(long)]
        say: Option<String>,
        /// Steps that must be built before this one (repeatable, or comma-separated).
        #[arg(long, value_delimiter = ',')]
        after: Vec<String>,
    },
    /// Put an existing design doc (for example one written by Google's
    /// `egm-draft` skill) under the gate, so goldfish and build can run on it.
    Adopt {
        feature: String,
        /// Path of the existing doc, inside the repository.
        #[arg(long)]
        doc: PathBuf,
        /// Steps that must be built before this one (repeatable, or comma-separated).
        #[arg(long, value_delimiter = ',')]
        after: Vec<String>,
    },
    /// Set the steps a feature depends on (replaces the list; none clears it).
    After {
        feature: String,
        steps: Vec<String>,
    },
    /// Set how big one step may be (0 turns a limit off).
    SetLimits {
        #[arg(long)]
        max_files: Option<usize>,
        #[arg(long)]
        max_criteria: Option<usize>,
    },
    /// Have the elephant write the design doc, one section per turn.
    Draft {
        feature: String,
        /// Redraft only this section, e.g. "Technical plan" (repeatable: several go in one turn).
        #[arg(long)]
        section: Vec<String>,
        /// One elephant turn per section (slower) instead of the whole doc in one turn.
        #[arg(long)]
        per_section: bool,
        /// Ask for at most this many words per section (overrides `set-draft-words`).
        #[arg(long)]
        max_words: Option<u32>,
    },
    /// After a NOT READY verdict: the elephant answers each open question the
    /// repository settles (egm-dsh checks its quotes) and turns the rest into
    /// decisions for you, with labelled options.
    Escalate { feature: String },
    /// List the open decisions, or record one: `decide F 2 B`, `decide F 2 --other "..."`.
    Decide {
        feature: String,
        /// Decision number.
        id: Option<usize>,
        /// Option key, like `A`.
        option: Option<String>,
        /// Your own answer instead of an option.
        #[arg(long)]
        other: Option<String>,
    },
    /// Write the settled answers into the doc (`## Decisions`) and have the elephant
    /// redraft the sections they concern. A fresh goldfish must then pass the doc.
    Redraft { feature: String },
    /// Rounds of goldfish → escalate → redraft. Redrafts on its own only when the
    /// repository answered every question; stops for your decisions, on stalled
    /// findings, at a budget limit, or at READY. Never builds.
    Loop {
        feature: String,
        #[arg(long, default_value_t = 5)]
        max_rounds: u32,
        /// Stop when the open findings have not fallen over this many rounds.
        #[arg(long, default_value_t = 2)]
        stall: usize,
        #[arg(long)]
        no_verify: bool,
        /// Run every stage every round (by default the loop is fail-fast).
        #[arg(long)]
        full: bool,
    },
    /// A tiny fix with less ceremony: short doc, one goldfish, red, one builder, green,
    /// a review of the diff. Bigger than the limits: reverted and moved to the full lane.
    /// Run again with only the name to continue.
    Fix {
        feature: String,
        /// What is wrong, in a sentence or two.
        problem: Option<String>,
        /// The file(s) the fix changes (comma-separated or repeated).
        #[arg(long, value_delimiter = ',')]
        files: Vec<String>,
        /// The test that fails now and passes after the fix: `tests/x.py::Class::test_y`.
        #[arg(long)]
        check: Option<String>,
        /// When no test can show it: how to verify the fix by hand.
        #[arg(long)]
        manual: Option<String>,
    },
    /// PlantUML sequence diagrams in design docs: `prefer` (asked for when drafting,
    /// checked when present; the default), `require` (a Technical plan needs one) or `off`.
    SetDiagrams {
        #[arg(value_enum)]
        policy: Option<diagram::Policy>,
        /// Use an external PlantUML for the syntax check instead of the built-in one,
        /// e.g. `--plantuml-cmd java -jar /opt/plantuml.jar`.
        #[arg(long, num_args = 1.., allow_hyphen_values = true)]
        plantuml_cmd: Vec<String>,
        /// Forget the external command (back to the built-in checker).
        #[arg(long)]
        no_plantuml_cmd: bool,
        /// Turn PlantUML's syntax check on (the default) or off (structure check only).
        #[arg(long, conflicts_with = "no_plantuml_check")]
        plantuml_check: bool,
        #[arg(long)]
        no_plantuml_check: bool,
    },
    /// What counts as a tiny fix for `egm-dsh fix`.
    SetFixLimits {
        #[arg(long)]
        max_files: Option<usize>,
        #[arg(long)]
        max_lines: Option<usize>,
        /// Paths a fix may never touch (.gitignore-style patterns; repeatable).
        #[arg(long)]
        protect: Vec<String>,
        #[arg(long)]
        clear_protected: bool,
    },
    /// Sessions and tokens the next goldfish round (or N loop rounds) would use. No model call.
    Estimate {
        feature: String,
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        #[arg(long)]
        fail_fast: bool,
        #[arg(long)]
        cross_family: bool,
        #[arg(long)]
        no_verify: bool,
    },
    /// What the elephant reads: only these repository paths (plus the doc, its links and
    /// earlier steps' docs). With a scope or a `.egmignore`, elephant turns run in a view.
    SetScope {
        paths: Vec<String>,
        /// Set it for one feature only.
        #[arg(long)]
        feature: Option<String>,
        /// Remove the scope (for --feature: fall back to the project's).
        #[arg(long)]
        none: bool,
        /// Let the elephant read the project folder itself when there is no scope and no
        /// `.egmignore` (it can then also read `.egm-dsh/`). The view is the default.
        #[arg(long, conflicts_with = "view")]
        no_view: bool,
        /// Run elephant turns in a view again (the default).
        #[arg(long)]
        view: bool,
    },
    /// What runs the read-only goldfish roles of a dsh project: `builtin` (egm-dsh's own
    /// /chat/completions request, the default) or `dsh` (a DeepSeek Harness reading room).
    SetReviewers {
        client: Option<project::ReviewerClient>,
        /// With built-in reviewers, run zoom-out built in too (it then cannot search the repository).
        #[arg(long, conflicts_with = "zoom_out_dsh")]
        zoom_out_builtin: bool,
        /// With built-in reviewers, keep zoom-out in dsh so it can search for existing components (the default).
        #[arg(long)]
        zoom_out_dsh: bool,
    },
    /// The evidence gate CI can run: every built feature's evidence holds, or exit 1.
    /// Needs no model and no API key.
    Gate {
        features: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Skip seeding bugs (faster; the mutation check is left out).
        #[arg(long)]
        no_mutation: bool,
    },
    /// Seed small bugs in the lines a build changed and count how many the frozen tests catch.
    Mutate { feature: String },
    /// A pull request description: gate result, criteria and checks, changed lines no
    /// test covers, seeded bugs no test caught, risk and suggested reviewer.
    Evidence {
        feature: String,
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        #[arg(long)]
        no_mutation: bool,
    },
    /// Risk tier of a built feature and who should review it.
    Risk { feature: String },
    /// Record minutes spent on a feature outside egm-dsh (reading its pull request, a design meeting).
    LogTime {
        feature: String,
        minutes: f64,
        #[arg(long, default_value = "review")]
        what: String,
    },
    /// Print a GitHub Actions workflow that runs the evidence gate on every pull request.
    CiTemplate,
    /// Evidence gate settings.
    SetGate {
        /// Minimum share of seeded bugs caught, in percent (default 60).
        #[arg(long)]
        mutation_min: Option<u32>,
        /// At most this many seeded bugs per feature (default 30).
        #[arg(long)]
        mutants: Option<usize>,
        /// Seconds one seeded bug's checks may run (default 60). The gate raises it to three
        /// times the unmutated checks' time when that is longer. A timeout is never counted as caught.
        #[arg(long)]
        mutant_timeout: Option<u64>,
        /// Command that runs the tests with coverage and writes an lcov file; quote it as one argument.
        #[arg(long)]
        coverage_cmd: Option<String>,
        /// The lcov file it writes (default lcov.info).
        #[arg(long)]
        coverage_file: Option<String>,
        /// A path that makes a change high risk (repeatable).
        #[arg(long)]
        risk_path: Vec<String>,
    },
    /// How a goldfish round is run: readiness readers that vote, and the invariants stage.
    SetReview {
        /// Independent readiness readers; readiness blocks when a strict majority object (default 3, 1 = a single reader).
        #[arg(long)]
        votes: Option<usize>,
        /// Run the display-only invariants stage (off by default: no alarm comes from it).
        #[arg(long, conflicts_with = "no_invariants")]
        invariants: bool,
        #[arg(long)]
        no_invariants: bool,
    },
    /// Word limit per section asked of the elephant when it drafts or redrafts.
    SetDraftWords {
        words: Option<u32>,
        #[arg(long)]
        none: bool,
    },
    /// Most tokens one feature may use; steps whose estimate would cross it are refused.
    SetCeiling {
        tokens: Option<u64>,
        #[arg(long)]
        none: bool,
    },
    /// Test the design doc against memoryless reviewers.
    Goldfish {
        feature: String,
        /// Add a critic from a second model family (needs `cross_family` in config).
        #[arg(long)]
        cross_family: bool,
        /// Override the configured mode for this run.
        #[arg(long, value_enum)]
        mode: Option<GoldfishMode>,
        /// Skip the verifier: every alarm that survives grounding counts.
        #[arg(long)]
        no_verify: bool,
        /// Stop at the first NOT READY cheap evidence proves: egm-dsh's own checks
        /// (no model call), then readiness alone. READY still needs every stage.
        #[arg(long)]
        fail_fast: bool,
    },
    /// Implement a goldfish-approved doc, then run the acceptance checks.
    /// Build a goldfish-approved doc, phase by phase. Refused until every
    /// step it depends on is built.
    Build {
        feature: String,
        /// Build only this phase (1-based); phases run in order.
        #[arg(long)]
        phase: Option<u32>,
    },
    /// A fresh, hostile review of the built code.
    Review { feature: String },
    /// Check the PlantUML diagrams in Markdown files (no project needed, no model).
    /// Exit 2 when any diagram has a problem.
    Diagrams {
        files: Vec<std::path::PathBuf>,
        /// An external PlantUML command for the syntax check (default: the project's, else built-in).
        #[arg(long, num_args = 1.., allow_hyphen_values = true)]
        plantuml_cmd: Vec<String>,
        /// Skip PlantUML's syntax check (structure check only).
        #[arg(long)]
        no_plantuml_check: bool,
        /// Only PlantUML's syntax check, without egm-dsh's structure check (for comparisons).
        #[arg(long)]
        plantuml_only: bool,
        /// One JSON line per diagram.
        #[arg(long)]
        json: bool,
    },
    /// Check that built code and design docs still agree. No model involved.
    Drift,
    /// Tokens and cost per role and model.
    Usage {
        #[arg(long)]
        feature: Option<String>,
    },
    /// Record a feature's outcome: `rework` or `clean`.
    Label { feature: String, outcome: String },
    /// MCC of first goldfish verdicts against recorded outcomes.
    Metrics {
        /// Also report MCC separately for each goldfish model.
        #[arg(long)]
        by_model: bool,
    },
    /// Phase of every feature.
    Status,
    /// Print a feature's design doc.
    Show { feature: String },
    /// Run the whole loop on a bundled sample project ("add tags to jot").
    Demo {
        #[arg(long, default_value = "egm-dsh-demo")]
        dir: PathBuf,
        #[arg(long, value_enum, default_value = "mock")]
        agent: AgentKind,
        #[arg(long, value_enum, default_value = "balanced")]
        preset: Preset,
        #[arg(long, num_args = 1.., value_name = "WORD")]
        dsh_command: Option<Vec<String>>,
        /// Use a learned triage model (OpenAI-compatible base URL) in front of the verifier.
        #[arg(long)]
        triage_url: Option<String>,
        #[arg(long, default_value = "egm-triage-catboost")]
        triage_model: String,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Some(path) = &cli.events {
        match egm_core::events::jsonl_sink(path) {
            Ok(sink) => egm_core::events::set_sink(Some(sink)),
            Err(e) => {
                ui::warn(&format!("cannot open {}: {e}", path.display()));
                std::process::exit(1);
            }
        }
    }
    let result = run(cli);
    // An interrupt may have landed while a non-agent command (a test, git) ran: its exit code wins.
    session::halt_if_interrupted();
    if let Err(e) = result {
        ui::warn(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn save_config(p: &Project) -> anyhow::Result<()> {
    std::fs::write(Project::egm_dir(&p.root).join("config.json"), serde_json::to_string_pretty(&p.config)? + "\n")?;
    Ok(())
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let here = std::env::current_dir()?;
    match cli.command {
        Cmd::Init { agent, preset, dsh_command, acceptance, tests_first, check_cmd } => {
            let mut config = Config { agent, ..Config::default() };
            config.apply_preset(preset);
            if let Some(cmd) = dsh_command {
                config.dsh_command = cmd;
            }
            config.acceptance_cmd = acceptance.unwrap_or_default();
            config.tests_first = tests_first;
            if let Some(c) = check_cmd {
                config.check_cmd = c;
            }
            let p = Project::init(&here, config)?;
            ui::ok(&format!("initialized {} (agent: {:?}, preset: {:?})", p.root.join(".egm-dsh").display(), agent, preset));
            commands::roles(&p)
        }
        Cmd::Diagrams { files, plantuml_cmd, no_plantuml_check, plantuml_only, json } => {
            let project = Project::open(&here).ok();
            let configured = if plantuml_cmd.is_empty() { project.as_ref().map(|p| p.config.plantuml_cmd.clone()).unwrap_or_default() } else { plantuml_cmd };
            let enabled = !no_plantuml_check && project.as_ref().is_none_or(|p| p.config.plantuml_check);
            if commands::diagrams(&files, &diagram::checker(enabled, &configured), json, plantuml_only)? {
                std::process::exit(2);
            }
            Ok(())
        }
        Cmd::Demo { dir, agent, preset, dsh_command, triage_url, triage_model } => {
            let triage = triage_url.map(|base_url| triage::TriageConfig { base_url, model: triage_model, api_key_env: String::new(), explore: 0.1 });
            demo::run(&dir, agent, preset, dsh_command, triage)
        }
        cmd => {
            let p = Project::open(&here)?;
            // One command at a time per project: state files have no other protection.
            let _lock = lock::acquire(&Project::egm_dir(&p.root))?;
            tuning::init(&p.root, p.config.goldfish_harness.as_deref())?;
            // Clean up sessions a crashed earlier run left behind; handle Ctrl-C/SIGTERM/SIGHUP.
            session::init(&Project::egm_dir(&p.root));
            // Human time per feature: the gaps between these commands (`evidence::human_minutes`).
            let timed: Option<(String, &'static str)> = match &cmd {
                Cmd::Elephant { feature, .. } => Some((feature.clone(), "elephant")),
                Cmd::Adopt { feature, .. } => Some((feature.clone(), "adopt")),
                Cmd::Draft { feature, .. } => Some((feature.clone(), "draft")),
                Cmd::Redraft { feature } => Some((feature.clone(), "redraft")),
                Cmd::Goldfish { feature, .. } => Some((feature.clone(), "goldfish")),
                Cmd::Escalate { feature } => Some((feature.clone(), "escalate")),
                Cmd::Loop { feature, .. } => Some((feature.clone(), "loop")),
                Cmd::Tests { feature } => Some((feature.clone(), "tests")),
                Cmd::Build { feature, .. } => Some((feature.clone(), "build")),
                Cmd::Review { feature } => Some((feature.clone(), "review")),
                Cmd::Fix { feature, .. } => Some((feature.clone(), "fix")),
                _ => None,
            };
            let started = gate::now();
            let root = p.root.clone();
            let result = run_cmd(p, cmd);
            if let Some((feature, name)) = timed {
                gate::record_event(&root, &evidence::Event { feature, cmd: name.into(), start: started, end: gate::now(), logged_minutes: 0.0 });
            }
            result
        }
    }
}

fn run_cmd(mut p: Project, cmd: Cmd) -> anyhow::Result<()> {
    {
        {
            match cmd {
                Cmd::Roles => commands::roles(&p),
                Cmd::SetRole { role, model, effort, agent, lean_tools } => {
                    let entry = p.config.roles.entry(role.into()).or_default();
                    if model.is_some() { entry.model = model; }
                    if effort.is_some() { entry.effort = effort; }
                    if agent.is_some() { entry.agent = agent; }
                    if lean_tools.is_some() { entry.lean_tools = lean_tools; }
                    save_config(&p)?;
                    commands::roles(&p)
                }
                Cmd::SetTriage { url, model, explore, off } => {
                    if off {
                        p.config.triage = None;
                        save_config(&p)?;
                        ui::ok("triage off: every alarm that survives grounding goes to the verifier");
                        return Ok(());
                    }
                    let base_url = url.ok_or_else(|| anyhow::anyhow!("pass --url, or --off"))?;
                    anyhow::ensure!((0.0..=1.0).contains(&explore), "--explore is a share between 0 and 1");
                    if !p.config.verify_alarms {
                        ui::warn("verify_alarms is off, so triage has nothing to skip; it only runs in front of the verifier");
                    }
                    p.config.triage = Some(triage::TriageConfig { base_url, model, api_key_env: String::new(), explore });
                    save_config(&p)?;
                    ui::ok("triage on: it can only keep alarms without verifying them, never drop one");
                    Ok(())
                }
                Cmd::Preset { preset } => {
                    p.config.apply_preset(preset);
                    save_config(&p)?;
                    commands::roles(&p)
                }
                Cmd::Elephant { feature, idea, say, after } => commands::elephant(&p, &feature, idea.as_deref(), say.as_deref(), &after),
                Cmd::Adopt { feature, doc, after } => commands::adopt(&p, &feature, &doc, &after),
                Cmd::After { feature, steps } => commands::set_after(&p, &feature, &steps),
                Cmd::Tests { feature } => commands::tests(&p, &feature),
                Cmd::SetSafety { sandbox, env_pass, git } => {
                    if let Some(s) = sandbox {
                        p.config.safety.sandbox = s;
                    }
                    if let Some(names) = env_pass {
                        for n in names.iter().filter(|n| safety::looks_secret(n)) {
                            ui::warn(&format!("`{n}` looks like a secret and will never be passed"));
                        }
                        p.config.safety.env_pass = names;
                    }
                    if let Some(g) = git {
                        p.config.safety.git = matches!(g.as_str(), "on" | "true" | "yes");
                    }
                    save_config(&p)?;
                    commands::doctor(&p)
                }
                Cmd::Doctor => commands::doctor(&p),
                Cmd::Fix { feature, problem, files, check, manual } => commands::fix(&p, &feature, problem.as_deref(), &files, check.as_deref(), manual.as_deref()),
                Cmd::SetDiagrams { policy, plantuml_cmd, no_plantuml_cmd, plantuml_check, no_plantuml_check } => {
                    if plantuml_check {
                        p.config.plantuml_check = true;
                    }
                    if no_plantuml_check {
                        p.config.plantuml_check = false;
                    }
                    if let Some(pol) = policy {
                        p.config.diagrams = pol;
                    }
                    if no_plantuml_cmd {
                        p.config.plantuml_cmd.clear();
                    } else if !plantuml_cmd.is_empty() {
                        p.config.plantuml_cmd = plantuml_cmd;
                    }
                    save_config(&p)?;
                    let how = match p.config.diagrams {
                        diagram::Policy::Off => "off: no diagram guidance when drafting; diagrams that are there are still checked",
                        diagram::Policy::Prefer => "prefer: the elephant is asked for sequence diagrams instead of long prose; every diagram is checked",
                        diagram::Policy::Require => "require: a Technical plan needs a valid sequence diagram (fixes are exempt); every diagram is checked",
                    };
                    ui::egm(&format!("diagrams {how}"));
                    ui::egm(&format!("PlantUML syntax check: {}", diagram::checker(p.config.plantuml_check, &p.config.plantuml_cmd).describe()));
                    Ok(())
                }
                Cmd::SetFixLimits { max_files, max_lines, protect, clear_protected } => {
                    if clear_protected {
                        p.config.fix.protected.clear();
                    }
                    if let Some(n) = max_files {
                        p.config.fix.max_files = n;
                    }
                    if let Some(n) = max_lines {
                        p.config.fix.max_lines = n;
                    }
                    p.config.fix.protected.extend(protect);
                    save_config(&p)?;
                    let l = &p.config.fix;
                    ui::egm(&format!("a fix changes at most {} file(s) and {} line(s){}", l.max_files, l.max_lines,
                        if l.protected.is_empty() { String::new() } else { format!(", never {}", l.protected.join(", ")) }));
                    Ok(())
                }
                Cmd::SetScope { paths, feature, none, no_view, view } => {
                    if no_view {
                        p.config.elephant_view = false;
                    }
                    if view {
                        p.config.elephant_view = true;
                    }
                    for path in &paths {
                        anyhow::ensure!(!path.starts_with('/') && !path.split('/').any(|c| c == ".."), "scope paths are repository-relative: {path}");
                        if !p.root.join(path).exists() {
                            ui::warn(&format!("{path} does not exist (yet)"));
                        }
                    }
                    match (&feature, none) {
                        (Some(f), true) => { p.config.scope_overrides.remove(f); }
                        (Some(f), false) => { p.config.scope_overrides.insert(f.clone(), paths.clone()); }
                        (None, true) => p.config.scope.clear(),
                        (None, false) => p.config.scope = paths.clone(),
                    }
                    save_config(&p)?;
                    let shown = feature.as_deref().map(|f| p.config.scope_for(f)).unwrap_or_else(|| p.config.scope.clone());
                    let whom = feature.as_deref().map(|f| format!("`{f}`")).unwrap_or_else(|| "every feature".into());
                    if shown.is_empty() && !p.config.elephant_view && !p.root.join(".egmignore").is_file() {
                        ui::egm(&format!("no view for {whom}: the elephant reads the project folder itself, including .egm-dsh/"));
                    } else if shown.is_empty() {
                        ui::egm(&format!("no scope for {whom}: the elephant reads a view of the whole repository minus egm-dsh's state, VCS, build output, huge files and what .gitignore/.egmignore exclude"));
                    } else {
                        ui::egm(&format!("the elephant reads only {} for {whom}, plus the doc, its links and earlier steps' docs", shown.join(", ")));
                    }
                    Ok(())
                }
                Cmd::SetReviewers { client, zoom_out_builtin, zoom_out_dsh } => {
                    if let Some(c) = client {
                        p.config.reviewers = c;
                    }
                    if zoom_out_builtin {
                        p.config.zoom_out_in_dsh = false;
                    }
                    if zoom_out_dsh {
                        p.config.zoom_out_in_dsh = true;
                    }
                    save_config(&p)?;
                    ui::egm(&commands::reviewers_line(&p));
                    Ok(())
                }
                Cmd::Gate { features, all, no_mutation } => {
                    if !gate::gate(&p, &features, all, !no_mutation)? {
                        std::process::exit(1);
                    }
                    Ok(())
                }
                Cmd::Mutate { feature } => {
                    if !gate::mutate(&p, &feature)? {
                        std::process::exit(1);
                    }
                    Ok(())
                }
                Cmd::Evidence { feature, out, no_mutation } => {
                    if !gate::evidence_cmd(&p, &feature, out.as_deref(), !no_mutation)? {
                        std::process::exit(1);
                    }
                    Ok(())
                }
                Cmd::Risk { feature } => gate::risk_cmd(&p, &feature),
                Cmd::LogTime { feature, minutes, what } => gate::log_time(&p, &feature, minutes, &what),
                Cmd::CiTemplate => {
                    ui::out(gate::ci_template().trim_end_matches('\n').to_string());
                    Ok(())
                }
                Cmd::SetGate { mutation_min, mutants, mutant_timeout, coverage_cmd, coverage_file, risk_path } => {
                    if let Some(m) = mutation_min {
                        anyhow::ensure!(m <= 100, "--mutation-min is a percentage");
                        p.config.gate.mutation_min = m;
                    }
                    if let Some(n) = mutants {
                        p.config.gate.mutants = n;
                    }
                    if let Some(t) = mutant_timeout {
                        anyhow::ensure!(t > 0, "--mutant-timeout is in seconds and must be positive");
                        p.config.gate.mutant_timeout_secs = t;
                    }
                    if let Some(c) = coverage_cmd {
                        p.config.gate.coverage_cmd = if c.trim().is_empty() { vec![] } else { vec!["sh".into(), "-c".into(), c] };
                    }
                    if let Some(f) = coverage_file {
                        p.config.gate.coverage_file = f;
                    }
                    p.config.gate.risk_paths.extend(risk_path);
                    save_config(&p)?;
                    let g = &p.config.gate;
                    ui::egm(&format!("evidence gate: catch at least {}% of up to {} seeded bugs ({} s each); coverage: {}; risk paths: {}", g.mutation_min, g.mutants, g.mutant_timeout_secs,
                        if g.coverage_cmd.is_empty() { "not measured".to_string() } else { g.coverage_cmd.last().cloned().unwrap_or_default() },
                        if g.risk_paths.is_empty() { "none".to_string() } else { g.risk_paths.join(", ") }));
                    Ok(())
                }
                Cmd::SetReview { votes, invariants, no_invariants } => {
                    if let Some(v) = votes {
                        anyhow::ensure!(v >= 1, "--votes needs at least 1 reader");
                        p.config.readiness_votes = v;
                    }
                    if invariants {
                        p.config.invariants_stage = true;
                    }
                    if no_invariants {
                        p.config.invariants_stage = false;
                    }
                    save_config(&p)?;
                    let v = p.config.readiness_votes;
                    ui::egm(&format!(
                        "readiness: {}; invariants stage {}",
                        if v == 1 { "one reader".to_string() } else { format!("{v} readers vote, a strict majority blocks") },
                        if p.config.invariants_stage { "on" } else { "off (display only)" }
                    ));
                    Ok(())
                }
                Cmd::SetDraftWords { words, none } => {
                    if none {
                        p.config.draft_words = None;
                    } else if words.is_some() {
                        p.config.draft_words = words;
                    }
                    save_config(&p)?;
                    match p.config.draft_words {
                        Some(n) => ui::egm(&format!("the elephant is asked for at most {n} words per section when it drafts or redrafts")),
                        None => ui::egm("no word limit on drafts"),
                    }
                    Ok(())
                }
                Cmd::SetCeiling { tokens, none } => {
                    if none {
                        p.config.token_ceiling = None;
                    } else if let Some(t) = tokens {
                        p.config.token_ceiling = Some(t);
                    }
                    save_config(&p)?;
                    match p.config.token_ceiling {
                        Some(c) => ui::egm(&format!("token ceiling: {c} per feature; a goldfish round whose estimate would cross it is refused before it starts")),
                        None => ui::egm("no token ceiling"),
                    }
                    Ok(())
                }
                Cmd::SetVerification { require, none } => {
                    if none {
                        p.config.verification.clear();
                    } else if !require.is_empty() {
                        p.config.verification = require;
                    }
                    save_config(&p)?;
                    if p.config.verification.is_empty() {
                        ui::egm("no Verification checklist: design docs need no `## Verification` section");
                    } else {
                        ui::egm("every design doc's `## Verification` section must define (egm-dsh checks; a missing item blocks the doc):");
                        ui::block(&crate::verify::guidance(&p.config.verification));
                    }
                    Ok(())
                }
                Cmd::Budget { feature } => commands::budget(&p, feature.as_deref()),
                Cmd::SetBudget { feature, goldfish_rounds, redraft_turns, build_attempts, reviews, model_calls } => {
                    let mut l = match &feature {
                        Some(f) => p.config.limits_for(f),
                        None => p.config.budgets,
                    };
                    if let Some(n) = goldfish_rounds { l.goldfish_rounds = n }
                    if let Some(n) = redraft_turns { l.redraft_turns = n }
                    if let Some(n) = build_attempts { l.build_attempts = n }
                    if let Some(n) = reviews { l.reviews = n }
                    if let Some(n) = model_calls { l.model_calls = n }
                    match &feature {
                        Some(f) => {
                            p.load_feature(f)?;
                            p.config.budget_overrides.insert(f.clone(), l);
                        }
                        None => p.config.budgets = l,
                    }
                    save_config(&p)?;
                    commands::budget(&p, feature.as_deref())
                }
                Cmd::Check { feature } => {
                    if !commands::check(&p, &feature)? {
                        std::process::exit(2);
                    }
                    Ok(())
                }
                Cmd::SetSpec { tests_first, check_cmd, lean_cmd } => {
                    if let Some(v) = tests_first {
                        p.config.tests_first = match v.as_str() {
                            "on" | "true" | "yes" => true,
                            "off" | "false" | "no" => false,
                            _ => anyhow::bail!("--tests-first takes on or off"),
                        };
                    }
                    if let Some(c) = check_cmd {
                        p.config.check_cmd = c;
                    }
                    if let Some(c) = lean_cmd {
                        p.config.lean_cmd = c;
                    }
                    save_config(&p)?;
                    ui::egm(&format!(
                        "tests first: {}; one check runs as: {}; Lean: {}",
                        if p.config.tests_first { "on" } else { "off" },
                        p.config.check_cmd.join(" "),
                        p.config.lean_cmd.join(" ")
                    ));
                    Ok(())
                }
                Cmd::SetLimits { max_files, max_criteria } => {
                    if let Some(n) = max_files {
                        p.config.step_limits.max_files = n;
                    }
                    if let Some(n) = max_criteria {
                        p.config.step_limits.max_criteria = n;
                    }
                    save_config(&p)?;
                    let l = p.config.step_limits;
                    ui::egm(&format!("step limits: {} files, {} acceptance criteria (0 = no limit)", l.max_files, l.max_criteria));
                    Ok(())
                }
                Cmd::Draft { feature, section, per_section, max_words } => commands::draft(&p, &feature, &section, per_section, max_words),
                Cmd::Escalate { feature } => commands::escalate(&p, &feature),
                Cmd::Loop { feature, max_rounds, stall, no_verify, full } => commands::loop_(&p, &feature, max_rounds, stall, !no_verify, !full),
                Cmd::Estimate { feature, rounds, fail_fast, cross_family, no_verify } => commands::estimate(&p, &feature, rounds.max(1), fail_fast, cross_family, !no_verify),
                Cmd::Decide { feature, id, option, other } => commands::decide(&p, &feature, id, option.as_deref(), other.as_deref()),
                Cmd::Redraft { feature } => commands::redraft(&p, &feature),
                Cmd::Goldfish { feature, cross_family, mode, no_verify, fail_fast } => {
                    if let Some(m) = mode {
                        p.config.goldfish_mode = m;
                    }
                    commands::goldfish(&p, &feature, cross_family, !no_verify, fail_fast).map(|_| ())
                }
                Cmd::Build { feature, phase } => commands::build(&p, &feature, phase),
                Cmd::Review { feature } => commands::review(&p, &feature).map(|_| ()),
                Cmd::Drift => {
                    if commands::drift(&p)? {
                        std::process::exit(2);
                    }
                    Ok(())
                }
                Cmd::Usage { feature } => commands::usage(&p, feature.as_deref()),
                Cmd::Label { feature, outcome } => commands::label(&p, &feature, &outcome),
                Cmd::Metrics { by_model } => commands::metrics(&p, by_model),
                Cmd::Status => commands::status(&p),
                Cmd::Show { feature } => commands::show(&p, &feature),
                Cmd::Init { .. } | Cmd::Demo { .. } | Cmd::Diagrams { .. } => unreachable!(),
            }
        }
    }
}
