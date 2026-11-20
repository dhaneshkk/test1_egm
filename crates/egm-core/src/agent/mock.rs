//! An offline stand-in for the coding agent, so the whole Elephant-Goldfish
//! loop can be demonstrated without an API key.
//!
//! It is scripted for the bundled `jot` sample ("add tags to notes"), but its
//! goldfish is a real rule-based reader: it opens the design doc and judges it.
//! The headline rule — a plan that changes stored data must say what happens
//! to data saved before the change — is what makes the first review fail in
//! the demo, and editing the doc is what makes the second one pass.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{Access, Agent, Effort, Reply, Role, Turn, Usage};
use crate::doc::Doc;
use crate::goldfish::Stage;
use crate::ui;

pub struct Mock {
    pub root: PathBuf,
    pub sessions_dir: PathBuf,
    /// Model and effort only label the simulated usage; the script is the same.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub lean_tools: bool,
}

/// dsh's fixed per-request input on 0.2.1-rc.2, measured with egm's fake
/// endpoint: system prompt, plus all 25 tool schemas or the 7 left by lean tools.
const DSH_SYSTEM_TOKENS: u64 = 656;
const DSH_TOOLS_FULL: u64 = 5_095;
const DSH_TOOLS_LEAN: u64 = 1_213;

/// Reasoning tokens per request the simulation ASSUMES for each effort level.
/// Real values vary widely; only real runs report them.
fn assumed_reasoning(effort: Option<Effort>) -> u64 {
    match effort {
        Some(Effort::Off) => 0,
        Some(Effort::Low) => 300,
        None | Some(Effort::High) => 1_200,
        Some(Effort::Max) => 3_000,
    }
}

const QUESTIONS: [&str; 6] = [
    "Who needs this, and what are they doing at the moment they need it?",
    "What exactly gets stored, and where does it live?",
    "What happens to data that already exists before this change?",
    "What should happen with odd input: empty, duplicate, or unknown values?",
    "How will you know it works? Name the test that would fail if it broke.",
    "What is the smallest version you would still ship?",
];

impl Mock {
    /// Simulate what a dsh turn like this one would bill: the first request
    /// pays for the turn's content, the fixed prefix is assumed cached, and
    /// every later request re-reads everything from cache plus what tools returned.
    fn usage(&self, prompt: &str, read_chars: usize, reply: &str, tool_calls: usize) -> Usage {
        let fixed = DSH_SYSTEM_TOKENS + if self.lean_tools { DSH_TOOLS_LEAN } else { DSH_TOOLS_FULL };
        let content = (prompt.len() / 4) as u64;
        let read = (read_chars / 4) as u64;
        let requests = tool_calls as u64 + 1;
        let later = requests - 1;
        Usage {
            cache_hit: fixed * requests + later * (content + read),
            cache_miss: content + read,
            output: (reply.len() / 4) as u64 + requests * assumed_reasoning(self.effort),
            requests,
            estimated: true,
            reasoning: requests * assumed_reasoning(self.effort),
        }
    }

    fn transcript(&self, id: &str) -> Vec<String> {
        std::fs::read_to_string(self.sessions_dir.join(format!("{id}.json")))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn remember(&self, id: &str, turns: &[String]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.sessions_dir)?;
        std::fs::write(self.sessions_dir.join(format!("{id}.json")), serde_json::to_string_pretty(turns)?)?;
        Ok(())
    }

    fn elephant(&self, turn: &Turn) -> anyhow::Result<Reply> {
        let id = turn.session.clone().unwrap_or_else(|| format!("mock-{}", crate::project::now_nanos()));
        let mut turns = self.transcript(&id);
        // Remember only what the human said; instructions in draft prompts are not design input.
        if turn.role == Role::Elephant {
            turns.push(user_text(&turn.prompt).to_string());
            self.remember(&id, &turns)?;
        }

        let text = match turn.role {
            Role::Draft(section) => draft(section, &turns.join("\n").to_lowercase()),
            Role::DraftAll => {
                let t = turns.join("\n").to_lowercase();
                crate::doc::SECTIONS.iter().filter(|s| turn.prompt.contains(&format!("## {s}\n"))).map(|s| draft(s, &t)).collect::<Vec<_>>().join("\n\n")
            }
            _ => {
                let n = turns.len();
                let mut out = String::new();
                if n == 1 {
                    out.push_str("I read the repository. `jot/cli.py` stores notes as a JSON list in `notes.json`; each note has an `id` and `text`. `tests/test_jot.py` covers adding and saving.\n\nBefore we design anything, some questions:\n");
                } else if user_text(&turn.prompt).len() < 25 {
                    out.push_str("That answer is thin, and agreeing with you would not help. Say more, or I will assume the simplest reading.\n\n");
                } else if (n - 1) * 2 < QUESTIONS.len() {
                    out.push_str("Noted. Next:\n");
                } else {
                    out.push_str("Noted. ");
                }
                let asked = (n - 1) * 2;
                let next: Vec<String> = QUESTIONS.iter().skip(asked).take(2).enumerate().map(|(i, q)| format!("{}. {q}", asked + i + 1)).collect();
                if next.is_empty() {
                    out.push_str("I have no more questions. Ask me to draft or redraft the design doc.");
                } else {
                    out.push_str(&next.join("\n"));
                }
                out
            }
        };
        let usage = self.usage(&turn.prompt, 0, &text, 0);
        Ok(Reply { text, session_id: Some(id), tool_calls: 0, usage, touched: vec![] })
    }

    fn goldfish_value(&self, stage: Stage, doc_path: &Path) -> anyhow::Result<serde_json::Value> {
        let doc = Doc::load(doc_path)?;
        let lower = doc.text.to_lowercase();
        let first = |s: &str| doc.section(s).map(|b| b.split(". ").next().unwrap_or("").trim().to_string()).unwrap_or_default();
        let gap = persistence_gap(&lower);
        let map_empty = doc.implementation_map().is_empty();
        // The scripted small model raises two false alarms once the real gap is closed:
        // one the doc already answers, and one built on a quote the doc does not contain.
        let noisy = !gap && self.model() == "deepseek-flash" && lower.contains("case-sensitive");
        let old_data_terms = json!(["existing", "saved before"]);
        let value = match stage {
            Stage::Comprehension => json!({
                "problem": first("Problem"),
                "current_mechanism": first("Technical plan"),
                "proposed_change": doc.section("Technical plan").map(|b| b.lines().next().unwrap_or("").to_string()).unwrap_or_default(),
                "unclear": doc.missing_sections().iter().map(|s| json!({"point": format!("The document has no usable `{s}` section."), "section": s})).collect::<Vec<_>>(),
            }),
            Stage::Critic => {
                let mut f = Vec::new();
                if gap {
                    f.push(json!({"severity": "blocking", "issue": "The plan changes what is stored, but never says what happens to data saved before the change.", "cites": "Technical plan",
                        "quote": "stored in `notes.json` next to `id` and `text`", "search_terms": old_data_terms}));
                }
                if noisy {
                    f.push(json!({"severity": "blocking", "issue": "Tags are lowercased before saving, so `Work` and `work` collide and the original spelling is lost.", "cites": "Technical plan",
                        "quote": "tags are lowercased before saving", "search_terms": []}));
                }
                if doc.section("Alternatives considered").is_none_or(|b| b.len() < 40) {
                    f.push(json!({"severity": "major", "issue": "No rejected alternatives are recorded, so the next session may re-propose them.", "cites": "Alternatives considered"}));
                }
                if lower.contains("tag") && !lower.contains("case") {
                    f.push(json!({"severity": "minor", "issue": "Matching rules are unstated: is `Work` the same tag as `work`?", "cites": "Technical plan"}));
                }
                json!({ "findings": f })
            }
            Stage::ZoomOut => {
                let mut f = Vec::new();
                if doc.section("Goals and non-goals").is_none_or(|b| !b.to_lowercase().contains("non-goal")) {
                    f.push(json!({"severity": "Major", "issue": "No non-goals are stated, so scope can grow without anyone deciding it should.", "cites": "Goals and non-goals",
                        "search_terms": ["non-goal"]}));
                }
                if lower.contains("index file") && !lower.contains("rejected") {
                    f.push(json!({"severity": "Major", "issue": "A separate index duplicates what `notes.json` already holds; reuse the note list.", "cites": "Technical plan"}));
                }
                f.push(json!({"severity": "Nit", "issue": "The plan stays at the right altitude and reuses `notes.json` instead of new storage.", "cites": "Technical plan"}));
                json!({ "findings": f })
            }
            Stage::Readiness => {
                let mut q = Vec::new();
                if gap {
                    q.push(json!({"question": "The plan changes the stored note format. What happens to notes saved before this change, which have no such field?",
                        "section": "Technical plan", "search_terms": old_data_terms}));
                }
                if map_empty {
                    q.push(json!({"question": "Which files change? The Implementation map lists none.", "section": "Implementation map"}));
                }
                if noisy {
                    q.push(json!({"question": "Does `jot list --tag` match tags case-insensitively? I could not tell.",
                        "section": "Technical plan", "search_terms": ["case-sensitive"]}));
                }
                json!({"ready": q.is_empty() && doc.missing_sections().is_empty(), "blocking_questions": q})
            }
            Stage::Invariants => json!({
                "criteria": doc.criteria().iter().map(|c| json!({
                    "criterion": c.text,
                    "predicate": c.text.split("Check:").next().unwrap_or("").trim(),
                    "check": c.check.clone().unwrap_or_default(),
                })).collect::<Vec<_>>()
            }),
        };
        Ok(value)
    }

    fn goldfish(&self, prompt: &str, stage: Option<Stage>, doc_path: &Path) -> anyhow::Result<Reply> {
        let value = match stage {
            Some(st) => self.goldfish_value(st, doc_path)?,
            None => json!({
                "comprehension": self.goldfish_value(Stage::Comprehension, doc_path)?,
                "critic": self.goldfish_value(Stage::Critic, doc_path)?,
                "zoom_out": self.goldfish_value(Stage::ZoomOut, doc_path)?,
                "readiness": self.goldfish_value(Stage::Readiness, doc_path)?,
                "invariants": self.goldfish_value(Stage::Invariants, doc_path)?,
            }),
        };
        let text = value.to_string();
        let doc_chars = std::fs::metadata(doc_path).map(|m| m.len() as usize).unwrap_or(0);
        let usage = self.usage(prompt, doc_chars, &text, 1);
        Ok(Reply { text, session_id: None, tool_calls: 1, usage, touched: vec![] })
    }

    /// Judge each alarm: answered when one sentence of the doc contains all of
    /// its search terms (quoting that sentence), otherwise confirmed.
    fn verify(&self, prompt: &str, doc_path: &Path) -> anyhow::Result<Reply> {
        let doc = Doc::load(doc_path)?;
        let sentences: Vec<&str> = doc.text.split(['\n', '.']).map(str::trim).filter(|s| !s.is_empty()).collect();
        let items: Vec<serde_json::Value> = crate::alarm::items_from_prompt(prompt)
            .iter()
            .map(|it| {
                let terms: Vec<String> = it["search_terms"].as_array().map(|a| a.iter().filter_map(|t| t.as_str()).map(str::to_lowercase).collect()).unwrap_or_default();
                let hit = (!terms.is_empty()).then(|| sentences.iter().find(|s| terms.iter().all(|t| s.to_lowercase().contains(t.as_str())))).flatten();
                match hit {
                    Some(s) => json!({"id": it["id"], "verdict": "answered", "quote": s, "reason": ""}),
                    None => json!({"id": it["id"], "verdict": "confirmed", "quote": "", "reason": "the doc does not say this"}),
                }
            })
            .collect();
        let text = json!({ "items": items }).to_string();
        let usage = self.usage(prompt, std::fs::metadata(doc_path).map(|m| m.len() as usize).unwrap_or(0), &text, 1);
        Ok(Reply { text, session_id: None, tool_calls: 1, usage, touched: vec![] })
    }

    fn build(&self, prompt: &str) -> anyhow::Result<Reply> {
        if !self.root.join("jot/cli.py").is_file() {
            anyhow::bail!("the mock agent can only build the bundled `jot` sample; set \"agent\": \"dsh\" in .egm-dsh/config.json for real projects");
        }
        for (rel, text) in [
            ("jot/cli.py", include_str!("../../../../sample/after/jot/cli.py")),
            ("tests/test_tags.py", include_str!("../../../../sample/after/tests/test_tags.py")),
        ] {
            ui::tool("write", rel);
            std::fs::write(self.root.join(rel), text)?;
        }
        ui::tool("bash", "python3 -m unittest discover -s tests -t .");
        let text = "Implemented tags as described: `add --tag` (repeatable), `list --tag`, `filter_by_tag`, and tests in `tests/test_tags.py`. Notes without a `tags` key load unchanged.\n\n{\"assumptions\": [{\"decision\": \"render shows tags in brackets after the text\", \"section\": \"Technical plan\", \"quote\": \"\", \"blocking\": false}]}".to_string();
        let read = include_str!("../../../../sample/before/jot/cli.py").len() + include_str!("../../../../sample/after/jot/cli.py").len() * 2;
        let usage = self.usage(prompt, read, &text, 3);
        Ok(Reply { text, session_id: None, tool_calls: 3, usage, touched: vec![] })
    }

    /// Tests first: write the bundled tag tests, which fail until the build.
    fn tester(&self, prompt: &str) -> anyhow::Result<Reply> {
        if !self.root.join("jot/cli.py").is_file() {
            anyhow::bail!("the mock agent can only write tests for the bundled `jot` sample");
        }
        ui::tool("write", "tests/test_tags.py");
        std::fs::write(self.root.join("tests/test_tags.py"), include_str!("../../../../sample/after/tests/test_tags.py"))?;
        let text = "Wrote tests/test_tags.py: one test per acceptance criterion. They fail until tags exist.".to_string();
        let usage = self.usage(prompt, include_str!("../../../../sample/before/jot/cli.py").len(), &text, 2);
        Ok(Reply { text, session_id: None, tool_calls: 2, usage, touched: vec![] })
    }

    /// Every criterion whose named test exists in its file counts as covered.
    fn test_review(&self, prompt: &str, doc_path: &Path) -> anyhow::Result<Reply> {
        let doc = Doc::load(doc_path)?;
        let items: Vec<serde_json::Value> = doc.criteria().iter().map(|c| {
            let check = c.check.clone().unwrap_or_default();
            let covered = match crate::spec::parse_check(&check) {
                crate::spec::Check::Test { file, id } => std::fs::read_to_string(self.root.join(&file))
                    .is_ok_and(|t| id.as_deref().is_none_or(|i| t.contains(i.rsplit("::").next().unwrap_or(i)))),
                crate::spec::Check::Lean { file, .. } => self.root.join(file).is_file(),
                crate::spec::Check::Manual => true,
            };
            json!({"check": check, "tests_it": covered, "reason": if covered { "" } else { "the named test is missing" }})
        }).collect();
        let text = json!({ "criteria": items }).to_string();
        let usage = self.usage(prompt, 2000, &text, 2);
        Ok(Reply { text, session_id: None, tool_calls: 2, usage, touched: vec![] })
    }

    fn critic(&self, prompt: &str, doc_path: &Path) -> anyhow::Result<Reply> {
        let doc = Doc::load(doc_path)?;
        let mut findings = Vec::new();
        for rel in doc.implementation_map() {
            let Ok(code) = std::fs::read_to_string(self.root.join(&rel)) else {
                findings.push(json!({"severity": "major", "file": rel, "issue": "listed in the Implementation map but missing"}));
                continue;
            };
            if rel.ends_with("cli.py") && code.contains("tag") && !code.contains(".lower()") && !code.contains("casefold") {
                let issue = if doc.text.to_lowercase().contains("case-sensitive") {
                    "Tag matching is case-sensitive as the doc says, so `--tag Work` silently finds nothing for notes tagged `work`; consider a hint when a filter matches no notes."
                } else {
                    "Tag matching is case-sensitive; the design doc does not say whether `Work` and `work` should match."
                };
                findings.push(json!({"severity": "minor", "file": rel, "issue": issue}));
            }
            if let Some(n) = longest_uncommented_run(&code).filter(|n| *n >= 10) {
                findings.push(json!({"severity": "minor", "file": rel, "issue": format!("{n} consecutive code lines without a comment; `main` would read better with one line per branch explaining intent.")}));
            }
        }
        let text = json!({ "findings": findings }).to_string();
        let read: usize = doc.implementation_map().iter().filter_map(|f| std::fs::metadata(self.root.join(f)).ok()).map(|m| m.len() as usize).sum();
        let usage = self.usage(prompt, read, &text, 2);
        Ok(Reply { text, session_id: None, tool_calls: 2, usage, touched: vec![] })
    }
}

impl Agent for Mock {
    fn name(&self) -> String {
        let effort = self.effort.map(|e| format!("/{}", e.as_str())).unwrap_or_default();
        format!("mock · {}{effort} (offline script)", self.model())
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
        let doc = || turn.doc.clone().ok_or_else(|| anyhow::anyhow!("mock agent needs the design doc path"));
        match turn.role {
            Role::Elephant | Role::Draft(_) | Role::DraftAll => self.elephant(&turn),
            Role::Escalate => Ok(escalate(&turn)),
            Role::Goldfish(stage) => self.goldfish(&turn.prompt, Some(stage), &doc()?),
            Role::GoldfishCombined => self.goldfish(&turn.prompt, None, &doc()?),
            Role::Verify => self.verify(&turn.prompt, &doc()?),
            Role::Builder => {
                anyhow::ensure!(turn.access == Access::WorkspaceWrite, "builder needs write access");
                self.build(&turn.prompt)
            }
            Role::Critic => self.critic(&turn.prompt, &doc()?),
            Role::Tester => {
                anyhow::ensure!(turn.access == Access::WorkspaceWrite, "tester needs write access");
                self.tester(&turn.prompt)
            }
            Role::TestReview => self.test_review(&turn.prompt, &doc()?),
        }
    }
}

/// The scripted elephant escalates every question as a decision with two options.
fn escalate(turn: &Turn) -> Reply {
    let items: Vec<serde_json::Value> = turn
        .prompt
        .lines()
        .filter_map(|l| l.strip_prefix('#'))
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(n, q)| n.parse::<usize>().ok().map(|n| (n, q)))
        .map(|(n, q)| {
            serde_json::json!({"alarm": n, "question": q, "why": "the repository does not settle it",
                "options": [{"label": "Take the simplest reading", "consequence": "least code"},
                            {"label": "Make it explicit in the doc", "consequence": "one more paragraph"}], "recommend": 2})
        })
        .collect();
    let text = serde_json::json!({ "items": items }).to_string();
    Reply { usage: Usage { cache_miss: turn.prompt.len() as u64 / 4, output: text.len() as u64 / 4, requests: 1, estimated: true, ..Usage::default() }, text, session_id: turn.session.clone(), tool_calls: 0, touched: vec![] }
}

/// The human's words inside an elephant prompt: the idea on the first turn,
/// the answer on later ones. Mirrors the prompt shapes in `commands.rs`.
fn user_text(prompt: &str) -> &str {
    if let Some(i) = prompt.rfind("\n\nIdea: ") {
        return prompt[i + 8..].trim();
    }
    prompt.split("\n\n(").next().unwrap_or(prompt).trim()
}

/// The doc talks about stored data but never about data that already exists.
fn persistence_gap(lower: &str) -> bool {
    let touches_storage = ["notes.json", "stored", "storage", "file format", "schema", "persist"].iter().any(|w| lower.contains(w));
    let covers_existing = ["existing", "migration", "backward", "saved before", "older"].iter().any(|w| lower.contains(w));
    touches_storage && !covers_existing
}

fn longest_uncommented_run(code: &str) -> Option<usize> {
    let mut best = 0;
    let mut run = 0;
    for line in code.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('#') || t.starts_with("\"\"\"") || t.starts_with("def ") {
            run = 0;
        } else {
            run += 1;
            best = best.max(run);
        }
    }
    (best > 0).then_some(best)
}

fn draft(section: &str, transcript: &str) -> String {
    let covers_existing = ["existing", "old notes", "saved before", "migration", "missing"].iter().any(|w| transcript.contains(w));
    if !transcript.contains("tag") {
        return format!("## {section}\n\nThe mock elephant only knows the bundled `jot` tagging scenario. Use the dsh agent to design other features.");
    }
    let body = match section {
        "Problem" => "People who keep both work and personal notes in jot cannot tell them apart. To find the notes for one context they scroll the whole list. They want to label a note when they add it and later list only the notes with that label.".to_string(),
        "Technical plan" => {
            let mut s = "Each note gains a `tags` list, stored in `notes.json` next to `id` and `text`. `jot add` accepts a repeatable `--tag` option; tags keep the order given and drop duplicates. `jot list --tag NAME` shows only notes carrying that tag. Filtering lives in a pure function, `filter_by_tag(notes, tag)` in `jot/cli.py`, so tests never touch the disk. Tag matching is exact and case-sensitive in this version; folding case is a later decision.".to_string();
            if covers_existing {
                s.push_str("\n\nNotes saved before this change have no `tags` key. Every reader treats a missing key as an empty list, so those notes still load and list, and a tag filter simply skips them. jot never rewrites old notes to add the key, so existing `notes.json` files need no migration.");
            }
            s
        }
        "Goals and non-goals" => "Goals: tag a note when adding it; list only the notes carrying one tag.\n\nNon-goals: renaming or deleting tags, filtering by several tags at once, and folding case. Each can come later without changing how tags are stored.".to_string(),
        "Risks and mitigations" => "**A typo splits one tag into two** (`wrok` vs `work`). Mitigation: `jot list` prints each note's tags, so a stray spelling is visible at a glance.\n\n**The notes file grows.** Tags add a few bytes per note; jot already loads the whole file, and nothing here changes that.".to_string(),
        "Alternatives considered" => "**A separate tag index file.** Rejected: two files can disagree after a crash or a hand edit; keeping tags on the note keeps one source of truth.\n\n**Hashtags inside the note text (`#work`).** Rejected: parsing free text is ambiguous (`#1` is a note id) and makes structured filtering fragile.".to_string(),
        "Implementation map" => format!(
            "- `jot/cli.py` — store `tags` on new notes, add `--tag` to `add` and `list`, add `filter_by_tag`, show tags in `render`.\n- `tests/test_tags.py` — new tests for tagging and filtering{}.",
            if covers_existing { ", and for notes saved before tags existed" } else { "" }
        ),
        "Acceptance criteria" => {
            let mut s = "- `jot add \"standup\" --tag work` stores the note with `tags: [\"work\"]`, and repeating a tag stores it once. Check: `tests/test_tags.py`\n- `jot list --tag work` prints only notes tagged `work`. Check: `tests/test_tags.py`".to_string();
            if covers_existing {
                s.push_str("\n- A `notes.json` written before tags existed still lists every note, and filtering by a tag skips those notes without error. Check: `tests/test_tags.py`");
            }
            s
        }
        _ => String::new(),
    };
    format!("## {section}\n\n{body}")
}
