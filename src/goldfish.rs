//! The goldfish: memoryless reviewers that see only the design doc and the
//! files it links. If they cannot explain and implement the plan from that,
//! the doc is missing context.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::agent::{Access, Agent, Role, Turn, Usage};
use crate::alarm::{self, Alarm, Ask, Grounding, Kind, Source, Status};
use crate::project::GoldfishMode;
use crate::doc::Doc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Comprehension,
    Critic,
    /// Upstream EGM's Skeptical Architect: simplicity, YAGNI, reuse.
    ZoomOut,
    Readiness,
    Invariants,
}

/// Run order. Zoom-out sits after the critic so alarm numbering for the
/// original stages is unchanged when it finds nothing blocking.
pub const STAGES: [Stage; 5] = [Stage::Comprehension, Stage::Critic, Stage::ZoomOut, Stage::Readiness, Stage::Invariants];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Comprehension {
    pub problem: String,
    pub current_mechanism: String,
    pub proposed_change: String,
    pub unclear: Vec<Ask>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Finding {
    pub severity: String,
    pub issue: String,
    pub cites: String,
    /// Exact words from the doc the finding is about (grounding evidence).
    pub quote: String,
    pub search_terms: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Critic {
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Readiness {
    pub ready: bool,
    pub blocking_questions: Vec<Ask>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Invariant {
    pub criterion: String,
    pub predicate: String,
    pub check: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Invariants {
    pub criteria: Vec<Invariant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Ready,
    NotReady,
    /// A reviewer failed to answer; this says nothing about the doc.
    Incomplete,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::Ready => "ready",
            Verdict::NotReady => "not-ready",
            Verdict::Incomplete => "incomplete",
        })
    }
}

/// The pass rule. Mirrors `verdict` in `proofs/EgmGate.lean`, where it is
/// proved that any open question or blocking finding forces `false`.
pub fn passes(ready: bool, questions: usize, blocking: usize) -> bool {
    ready && questions == 0 && blocking == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub verdict: Verdict,
    pub comprehension: Option<Comprehension>,
    pub critic: Option<Critic>,
    pub cross_critic: Option<Critic>,
    #[serde(default)]
    pub zoom_out: Option<Critic>,
    pub readiness: Option<Readiness>,
    /// Every readiness reader's answer when several vote (the first is `readiness`).
    #[serde(default)]
    pub readiness_votes: Vec<Readiness>,
    pub invariants: Option<Invariants>,
    /// Criteria that name no check (counted as open questions).
    pub unchecked: Vec<String>,
    /// Sections the doc lacks (counted as open questions).
    pub missing_sections: Vec<String>,
    pub failed: Vec<Stage>,
    pub tokens: u64,
    pub reviewers: Vec<String>,
    /// One entry per reviewer session, for usage accounting.
    #[serde(default)]
    pub calls: Vec<Call>,
    /// Every reason the doc might not be ready, with what grounding and the
    /// verifier made of it. Only alarms that still count block the build.
    #[serde(default)]
    pub alarms: Vec<Alarm>,
    /// Whether the verifier ran for this review.
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub context: crate::triage::ReviewContext,
    /// Why triage was not used, when it was configured but failed.
    #[serde(default)]
    pub triage_error: Option<String>,
    /// Fingerprint of the tuned goldfish setup in use, if any.
    #[serde(default)]
    pub setup: Option<String>,
    /// The doc revision reviewed (16 hex digits).
    #[serde(default)]
    pub doc_hash: Option<String>,
    /// Reviewers whose answers were discarded because they left the reading room.
    #[serde(default)]
    pub escapes: Vec<String>,
    /// Files the reviewers could read (repository-relative), when they ran staged.
    #[serde(default)]
    pub reading_room: Vec<String>,
    /// For a review written by a build: every decision the builder reported making.
    #[serde(default)]
    pub assumptions: Vec<crate::executor::Assumption>,
    /// Stages not run because a fail-fast review stopped early (never READY then).
    #[serde(default)]
    pub skipped: Vec<Stage>,
    /// Where a fail-fast review stopped: `precheck` (no model call) or `readiness`.
    #[serde(default)]
    pub stopped_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    /// Stage name, `combined`, or `cross-critic`.
    pub stage: String,
    pub role: String,
    pub agent: String,
    pub model: String,
    pub usage: Usage,
}

impl Review {
    /// The not-ready review a build writes when its builder had to decide what the doc
    /// left open. `egm-dsh escalate` reads it like any goldfish review.
    /// A review with nothing in it yet.
    pub fn empty(doc: &Doc, verdict: Verdict) -> Review {
        Review {
            verdict,
            comprehension: None,
            critic: None,
            cross_critic: None,
            zoom_out: None,
            readiness: None,
            readiness_votes: vec![],
            invariants: None,
            unchecked: vec![],
            missing_sections: vec![],
            failed: vec![],
            tokens: 0,
            reviewers: vec![],
            calls: vec![],
            alarms: vec![],
            verified: false,
            context: Default::default(),
            triage_error: None,
            setup: None,
            doc_hash: Some(format!("{:016x}", doc.hash())),
            escapes: vec![],
            reading_room: vec![],
            assumptions: vec![],
            skipped: vec![],
            stopped_at: None,
        }
    }

    /// The not-ready review a build writes when its builder had to decide what the doc
    /// left open. `egm-dsh escalate` reads it like any goldfish review.
    pub fn from_builder(doc: &Doc, builder: String, alarms: Vec<Alarm>, assumptions: Vec<crate::executor::Assumption>) -> Review {
        Review { reviewers: vec![builder], alarms, assumptions, ..Review::empty(doc, Verdict::NotReady) }
    }

    /// Alarms that still block the build.
    pub fn open_alarms(&self) -> Vec<&Alarm> {
        self.alarms.iter().filter(|a| a.counts()).collect()
    }

    pub fn questions(&self) -> Vec<String> {
        self.open_alarms().into_iter().filter(|a| a.kind == Kind::Question).map(|a| a.text.clone()).collect()
    }

    /// Comprehension notes: shown and saved, never counted against the doc.
    pub fn notes(&self) -> Vec<&Alarm> {
        self.alarms.iter().filter(|a| a.kind == Kind::Note).collect()
    }

    /// Questions the verifier dropped because an acceptance check catches a wrong guess.
    pub fn caught(&self) -> Vec<&Alarm> {
        self.alarms.iter().filter(|a| a.caught_by.is_some()).collect()
    }

    pub fn blocking(&self) -> Vec<&Alarm> {
        self.open_alarms().into_iter().filter(|a| a.kind == Kind::Blocking).collect()
    }

    /// (raised by models, dropped by grounding, dropped by the verifier, confirmed)
    pub fn alarm_counts(&self) -> (usize, usize, usize, usize) {
        let model: Vec<&Alarm> = self.alarms.iter().filter(|a| a.model_raised()).collect();
        let by = |who: &str| model.iter().filter(|a| a.dropped_by.as_deref() == Some(who)).count();
        (model.len(), by("grounding"), by("verifier"), model.iter().filter(|a| a.status == Status::Confirmed).count())
    }
}

const SCHEMAS: [(Stage, &str, &str); 5] = [
    (
        Stage::Comprehension,
        "In your own words, state the problem the document solves, how the system works today in the area it changes, and the change it proposes. List every point you could not determine from what you read, including undefined terms and assumed prerequisites. For each, name the section where you expected the answer and 1-3 search terms you looked for there.",
        r#"{"problem": "...", "current_mechanism": "...", "proposed_change": "...", "unclear": [{"point": "...", "section": "...", "search_terms": ["..."]}]}"#,
    ),
    (
        Stage::Critic,
        "Act as an expert reviewer. List faulty assumptions, missing edge cases, alternatives the document ignores, gaps against the Target Standard, and risks to existing data or callers. Cite the section or file behind each finding, and copy the exact words of the document it is about into the quote field (leave it empty if the problem is something missing, and give search terms instead). Mark a finding Blocker only if building the document as written would ship a defect. Report only real problems; an empty list is a valid answer.",
        r#"{"findings": [{"severity": "Blocker | Major | Nit", "issue": "...", "cites": "...", "quote": "...", "search_terms": ["..."]}]}"#,
    ),
    (
        Stage::ZoomOut,
        "Zoom out and judge the design from high altitude against the Zoom Out Standard: context sufficiency, root problem focus, simplicity and YAGNI, altitude, and reuse of what already exists. Cite the section behind each finding and copy the exact words it is about into the quote field (or give search terms if something is missing). Put any existing component that could be reused in the issue text, with its path. Mark a finding Blocker only if the objective cannot be determined from the document or the design as written cannot solve the problem; over-engineering and missed reuse are Major. Report only real problems; an empty list is a valid answer.",
        r#"{"findings": [{"severity": "Blocker | Major | Nit", "issue": "...", "cites": "...", "quote": "...", "search_terms": ["..."]}]}"#,
    ),
    (
        Stage::Readiness,
        "You are the engineer who must implement this. Could you implement it correctly in one pass using only what you read, without making a design decision the document leaves open? List only questions where you would otherwise have to guess and a wrong guess would ship a defect; leave out what any reasonable choice would satisfy, and leave out how the tests are written or which test files exist yet (the build writes the tests from the acceptance criteria). For each, name the section where the answer should be, the exact words it concerns if any (copied exactly), and 1-3 search terms you looked for there.",
        r#"{"ready": true, "blocking_questions": [{"question": "...", "section": "...", "quote": "...", "search_terms": ["..."]}]}"#,
    ),
    (
        Stage::Invariants,
        "For each item under 'Acceptance criteria', restate it as a precise predicate over observable behavior, and name the test or check that would fail if the predicate were false.",
        r#"{"criteria": [{"criterion": "...", "predicate": "...", "check": "..."}]}"#,
    ),
];

/// The part every stage shares. It comes first in each prompt so that four
/// prompts over one doc share a prefix, which DeepSeek's context cache bills
/// at the cache-hit rate.
fn brief(doc: &Doc, root: &Path, reads_files: bool) -> String {
    let rel = doc.path.strip_prefix(root).unwrap_or(&doc.path).display().to_string();
    if reads_files {
        format!(
            "You are a reviewer with no prior knowledge of this project. Read the design document `{rel}`, then every file it links with a relative Markdown link and every existing file named in its Implementation map. Do not search the repository or open any other file. Do not create, edit or delete files. Base every answer only on what you read."
        )
    } else {
        let mut ctx = format!("=== {rel} ===\n{}\n", doc.text);
        let mut left_out = Vec::new();
        for p in doc.context_files(root) {
            let r = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            match embeddable(root, &p) {
                Ok(()) => match std::fs::read_to_string(&p) {
                    Ok(text) => ctx.push_str(&format!("\n=== {r} ===\n{text}\n")),
                    Err(_) => left_out.push(format!("{r} (not text)")),
                },
                Err(why) => left_out.push(format!("{} ({why})", p.display())),
            }
        }
        if !left_out.is_empty() {
            ctx.push_str(&format!("\n(Linked but not shown: {}.)\n", left_out.join(", ")));
        }
        format!(
            "You are a reviewer with no prior knowledge of this project. Below are a design document ({rel}) and the only files you may consider. Base every answer only on them.\n\n{ctx}"
        )
    }
}

/// Whether a linked file may be put in a prompt that leaves this machine: inside
/// the project, not under `.git` (egm-dsh's state is already excluded by
/// `context_files`), and no bigger than the elephant view's limit. A link to
/// `../../secrets` is listed as left out, never sent (`EgmBuiltin.sent_only_allowed`).
pub fn embeddable(root: &Path, p: &Path) -> Result<(), &'static str> {
    let (Ok(r), Ok(c)) = (root.canonicalize(), p.canonicalize()) else { return Err("missing") };
    if !c.starts_with(&r) {
        return Err("outside the project");
    }
    if c.strip_prefix(&r).is_ok_and(|rel| rel.components().any(|x| x.as_os_str() == ".git" || x.as_os_str() == ".egm-dsh")) {
        return Err("repository internals");
    }
    match std::fs::metadata(&c) {
        Ok(m) if m.len() > crate::scope::MAX_FILE_BYTES => Err("over 512 KiB"),
        Ok(_) => Ok(()),
        Err(_) => Err("missing"),
    }
}

fn schema(stage: Stage) -> (&'static str, &'static str) {
    let (_, ask, schema) = SCHEMAS.iter().find(|(s, _, _)| *s == stage).expect("every stage has a schema");
    (ask, schema)
}

/// Build the prompt for one stage. Agents that read files get paths; others
/// get the files' contents, and nothing more.
pub fn prompt(stage: Stage, doc: &Doc, root: &Path, reads_files: bool) -> String {
    let (ask, schema) = schema(stage);
    format!("{}\n\n{}{ask}{}\n\n{BRIEF_ANSWERS}\nReply with only one JSON object of this shape, and no other text:\n{schema}", brief(doc, root, reads_files), persona_part(stage), search_note(stage, reads_files))
}

/// Output costs most and takes longest, so every stage is asked for a short answer.
/// A list keeps the most important items (a Blocker before a Nit), so a cap never hides
/// the one that blocks; egm-dsh's own checks are not affected.
pub const BRIEF_ANSWERS: &str = "Be brief: at most 5 items in any list, most important first, and at most 40 words in any text field.";

/// `EgmVote`: count the readiness readers whose questions still count after grounding
/// and the verifier. Without a strict majority objecting, those questions are dropped by
/// `vote` (kept on record). Nothing but readiness alarms is touched.
pub fn apply_vote(alarms: &mut [Alarm], n: usize) {
    let objecting: std::collections::BTreeSet<usize> =
        alarms.iter().filter(|a| a.source == Source::Readiness && a.counts()).filter_map(|a| a.vote).collect();
    if objecting.len() * 2 > n {
        return;
    }
    let k = objecting.len();
    for a in alarms.iter_mut().filter(|a| a.source == Source::Readiness && a.counts()) {
        a.drop_with("vote", format!("raised by {k} of {n} readiness readers; the majority found the doc ready"));
    }
}

/// The upstream persona and standard, placed after the shared brief so the
/// cacheable prefix is unchanged.
fn persona_part(stage: Stage) -> String {
    crate::official::persona_block(stage).map(|p| format!("{p}\n\nYour task: ")).unwrap_or_default()
}

/// Zoom-out is the one stage allowed to look past the doc, to check for reuse.
fn search_note(stage: Stage, reads_files: bool) -> &'static str {
    match (stage, reads_files) {
        (Stage::ZoomOut, true) => " For this task only, you may search and read other files in the repository (read-only) to check whether components the design proposes already exist.",
        (Stage::ZoomOut, false) => " You cannot search the repository, so judge reuse only from the files shown.",
        _ => "",
    }
}

/// One prompt for all four stages. Cheaper, but the answers are no longer independent.
pub fn combined_prompt(doc: &Doc, root: &Path, reads_files: bool, off: &[Stage]) -> String {
    let mut s = format!("{}\n\nComplete five separate tasks. Answer each one on its own terms; do not let one answer soften another.\n", brief(doc, root, reads_files));
    let mut shape = Vec::new();
    for (i, stage) in STAGES.iter().filter(|s| !off.contains(s)).enumerate() {
        let (ask, schema) = schema(*stage);
        let key = stage_key(*stage);
        let persona = crate::official::persona_block(*stage).map(|p| format!("\n{p}\n\n")).unwrap_or_default();
        s.push_str(&format!("\nTask {} (`{key}`): {persona}{ask}{}\n", i + 1, search_note(*stage, reads_files)));
        shape.push(format!("  \"{key}\": {schema}"));
    }
    s.push_str(&format!("\n{BRIEF_ANSWERS}\nReply with only one JSON object of this shape, and no other text:\n{{\n{}\n}}", shape.join(",\n")));
    s
}

fn stage_key(stage: Stage) -> &'static str {
    match stage {
        Stage::Comprehension => "comprehension",
        Stage::Critic => "critic",
        Stage::ZoomOut => "zoom_out",
        Stage::Readiness => "readiness",
        Stage::Invariants => "invariants",
    }
}

/// Parse the first JSON object in a reply, tolerating fences and chatter.
pub fn parse<T: DeserializeOwned>(text: &str) -> Option<T> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(&text[start..=end]).ok()
}

/// The agents a review uses: `reader` for comprehension, readiness and
/// invariants (and for everything in combined mode), `critic` for the critic stage.
pub struct Reviewers<'a> {
    pub reader: &'a dyn Agent,
    pub critic: &'a dyn Agent,
    /// Runs zoom-out instead of `critic` when set: dsh, when the other stages run on the
    /// built-in reviewer, because zoom-out searches the repository for existing components.
    pub zoom: Option<&'a dyn Agent>,
    pub cross: Option<&'a dyn Agent>,
    /// Checks the alarms that survive grounding; `None` keeps every alarm.
    pub verifier: Option<&'a dyn Agent>,
    /// Learned triage in front of the verifier; it can only skip verification.
    pub triage: Option<&'a crate::triage::TriageConfig>,
    pub mode: GoldfishMode,
    /// Which goldfish run this is for the feature (1-based).
    pub run: u32,
    /// Step-size limits; exceeding one is an egm-dsh alarm no model can drop.
    pub limits: crate::steps::StepLimits,
    /// The project's Verification checklist; a missing item is an egm-dsh alarm too.
    pub verification: &'a [crate::verify::Item],
    /// Stop at the first NOT READY that cheap evidence proves (READY still needs every stage).
    pub fail_fast: bool,
    /// How the doc's PlantUML diagrams are checked (egm-dsh's own questions).
    pub diagrams: crate::diagram::Check,
    /// Independent readiness readers (parallel mode); readiness blocks on a strict majority.
    pub votes: usize,
    /// Stages not run at all (the display-only invariants stage, by default).
    pub off: Vec<Stage>,
}

struct Outcome {
    text: Option<String>,
    call: Call,
}

/// Where a review's sessions run, and what left the reading room.
pub struct Room<'a> {
    pub stage: Option<&'a crate::stage::Stage>,
    pub escapes: &'a std::sync::Mutex<Vec<String>>,
}

fn run_one(agent: &dyn Agent, role: Role, stage: &str, role_name: &str, prompt: String, doc: &Doc, room: &Room) -> Outcome {
    // Agents that read files run in the stage; the others get the files in the prompt.
    // Without a stage such an agent would see the whole project, so it does not run.
    if agent.reads_files() && room.stage.is_none() {
        let call = Call { stage: stage.into(), role: role_name.into(), agent: agent.name(), model: agent.model(), usage: Usage::default() };
        return Outcome { text: None, call };
    }
    let staged = room.stage.filter(|_| agent.reads_files());
    let turn = Turn { prompt, session: None, access: Access::ReadOnly, role, doc: Some(doc.path.clone()), workdir: staged.map(|s| s.dir.clone()) };
    let (text, usage) = match agent.run(turn) {
        Ok(r) => match staged.and_then(|s| s.escape(&r.touched)) {
            // Fail closed: an answer from a reviewer that saw more than the doc is not used.
            Some(why) => {
                crate::ui::warn(&format!("{stage} reviewer left the doc ({why}); its answer is discarded"));
                room.escapes.lock().unwrap_or_else(|p| p.into_inner()).push(format!("{stage}: {why}"));
                (None, r.usage)
            }
            None => (Some(r.text), r.usage),
        },
        Err(e) => {
            crate::ui::warn(&format!("{stage} reviewer failed: {e:#}"));
            (None, Usage::default())
        }
    };
    Outcome { text, call: Call { stage: stage.into(), role: role_name.into(), agent: agent.name(), model: agent.model(), usage } }
}

/// egm-dsh's own alarms, found without any model: missing sections, criteria with no
/// check, oversized steps, Verification-checklist gaps. No model can drop these.
pub fn precheck(doc: &Doc, limits: crate::steps::StepLimits, verification: &[crate::verify::Item], diagrams: &crate::diagram::Check) -> (Vec<String>, Vec<String>, Vec<Alarm>) {
    let unchecked: Vec<String> = doc.criteria().into_iter().filter(|c| c.check.is_none()).map(|c| c.text).collect();
    let missing_sections: Vec<String> = doc.missing_sections().into_iter().map(String::from).collect();
    let mut alarms = collect_alarms(&missing_sections, &unchecked, StageAnswers::default(), &crate::steps::size_alarms(doc, limits));
    push_checklist(&mut alarms, doc, verification);
    push_diagrams(&mut alarms, doc, diagrams);
    (missing_sections, unchecked, alarms)
}

/// Malformed diagrams (and, with `require`, a plan without one): egm-dsh's own questions.
fn push_diagrams(alarms: &mut Vec<Alarm>, doc: &Doc, check: &crate::diagram::Check) {
    for (section, text) in crate::diagram::questions(&doc.text, check.policy, &check.checker, check.exempt) {
        let id = alarms.len() + 1;
        alarms.push(Alarm {
            id,
            source: Source::Egm,
            kind: Kind::Question,
            text,
            section,
            quote: String::new(),
            search_terms: vec![],
            grounding: Grounding::NotApplicable,
            status: Status::Open,
            dropped_by: None,
            reason: None,
            features: None,
            triage: None,
            skip_verify: false,
            caught_by: None,
            vote: None,
        });
    }
}

/// The project's Verification checklist: each missing item is egm-dsh's own question.
fn push_checklist(alarms: &mut Vec<Alarm>, doc: &Doc, items: &[crate::verify::Item]) {
    for text in crate::verify::questions(doc, items) {
        let id = alarms.len() + 1;
        alarms.push(Alarm {
            id,
            source: Source::Egm,
            kind: Kind::Question,
            text,
            section: "Verification".into(),
            quote: String::new(),
            search_terms: vec![],
            grounding: Grounding::NotApplicable,
            status: Status::Open,
            dropped_by: None,
            reason: None,
            features: None,
            triage: None,
            skip_verify: false,
            caught_by: None,
            vote: None,
        });
    }
}

/// Run the review: five fresh sessions in parallel (or one combined session),
/// plus an optional critic from a second model family.
///
/// With `fail_fast`, cheap evidence comes first and the review stops at the first
/// NOT READY it can prove: (1) egm-dsh's own alarms, no model call; (2) the readiness
/// stage alone (with the verifier on its alarms). READY still needs every stage, so
/// the gate is unchanged (`EgmFailFast` in proofs/EgmGate.lean).
pub fn review(who: &Reviewers, doc: &Doc, root: &Path) -> Review {
    if who.fail_fast {
        let (missing_sections, unchecked, mut alarms) = precheck(doc, who.limits, who.verification, &who.diagrams);
        if !alarms.is_empty() {
            for a in alarms.iter_mut() {
                a.grounding = Grounding::NotApplicable;
            }
            let mut r = Review::empty(doc, Verdict::NotReady);
            r.missing_sections = missing_sections;
            r.unchecked = unchecked;
            r.alarms = alarms;
            r.skipped = STAGES.iter().copied().filter(|s| !who.off.contains(s)).collect();
            r.stopped_at = Some("precheck".into());
            r.setup = crate::tuning::active().map(|h| format!("{:016x}", h.fingerprint));
            return r;
        }
    }
    // The reading room: only the doc, its links and its map files (stage.rs).
    let mut files = vec![doc.path.clone()];
    files.extend(doc.context_files(root));
    let stage = match crate::stage::Stage::new(root, &files) {
        Ok(s) => Some(s),
        Err(e) => {
            crate::ui::warn(&format!("could not prepare the goldfish reading room ({e:#}); reviewers that read files are not run"));
            None
        }
    };
    let escapes = std::sync::Mutex::new(Vec::new());
    let room = Room { stage: stage.as_ref(), escapes: &escapes };
    let room = &room;
    let reading_room = stage.as_ref().map(|s| s.files.clone()).unwrap_or_default();
    let agent_for = |st: Stage| match st {
        Stage::ZoomOut => (who.zoom.unwrap_or(who.critic), "goldfish-critic"),
        Stage::Critic => (who.critic, "goldfish-critic"),
        _ => (who.reader, "goldfish"),
    };
    let run_stage = |st: Stage| {
        let (agent, role_name) = agent_for(st);
        run_one(agent, Role::Goldfish(st), stage_key(st), role_name, prompt(st, doc, root, agent.reads_files()), doc, room)
    };
    let mut texts: [Option<String>; 5] = Default::default();
    let mut ran = [false; 5];
    let mut calls: Vec<Call> = Vec::new();
    let idx = |st: Stage| STAGES.iter().position(|s| *s == st).expect("stage in STAGES");

    // Fail-fast step 2: the readiness stage alone. Stop when it proves NOT READY
    // (or fails: a full review would be INCOMPLETE too).
    let votes = if who.mode == GoldfishMode::Parallel { who.votes.max(1) } else { 1 };
    let mut extra: Vec<Option<String>> = Vec::new();   // readiness readers after the first
    let run_readiness = |texts: &mut [Option<String>; 5], ran: &mut [bool; 5], extra: &mut Vec<Option<String>>, calls: &mut Vec<Call>| {
        let outs: Vec<Outcome> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..votes).map(|_| s.spawn(|| run_stage(Stage::Readiness))).collect();
            hs.into_iter().map(|h| h.join().expect("reviewer thread")).collect()
        });
        for (k, o) in outs.into_iter().enumerate() {
            if k == 0 {
                texts[idx(Stage::Readiness)] = o.text;
            } else {
                extra.push(o.text);
            }
            calls.push(o.call);
        }
        ran[idx(Stage::Readiness)] = true;
    };
    if who.fail_fast && who.mode == GoldfishMode::Parallel {
        run_readiness(&mut texts, &mut ran, &mut extra, &mut calls);
        let skipped: Vec<Stage> = STAGES.iter().copied().filter(|s| *s != Stage::Readiness && !who.off.contains(s)).collect();
        let (mut r, pass) = finish(who, doc, root, &texts, &ran, &extra, calls, None, room, skipped);
        if !pass || r.verdict == Verdict::Incomplete {
            r.stopped_at = Some("readiness".into());
            r.escapes = escapes.lock().unwrap_or_else(|p| p.into_inner()).clone();
            r.reading_room = reading_room;
            return r;
        }
        // Readiness raised nothing that still counts: run the rest. Its verifier call (if any) was spent.
        calls = r.calls;
    }

    if who.mode == GoldfishMode::Parallel && !ran[idx(Stage::Readiness)] && votes > 1 {
        run_readiness(&mut texts, &mut ran, &mut extra, &mut calls);   // all readiness readers at once
    }
    let todo: Vec<Stage> = STAGES.iter().copied().filter(|s| !ran[idx(*s)] && !who.off.contains(s)).collect();
    let cross_out = std::thread::scope(|s| {
        let cross_handle = who.cross.map(|c| {
            s.spawn(move || run_one(c, Role::Goldfish(Stage::Critic), "cross-critic", "cross-family", prompt(Stage::Critic, doc, root, c.reads_files()), doc, room))
        });
        match who.mode {
            GoldfishMode::Parallel => {
                let handles: Vec<_> = todo.iter().map(|&st| (st, s.spawn(move || run_stage(st)))).collect();
                for (st, h) in handles {
                    let o = h.join().expect("reviewer thread");
                    texts[idx(st)] = o.text;
                    ran[idx(st)] = true;
                    calls.push(o.call);
                }
            }
            GoldfishMode::Combined => {
                let o = run_one(who.reader, Role::GoldfishCombined, "combined", "goldfish", combined_prompt(doc, root, who.reader.reads_files(), &who.off), doc, room);
                let parsed: Option<serde_json::Value> = o.text.as_deref().and_then(parse);
                for (i, st) in STAGES.iter().enumerate().filter(|(_, s)| !who.off.contains(s)) {
                    texts[i] = parsed.as_ref().and_then(|v| v.get(stage_key(*st))).map(|v| v.to_string());
                    ran[i] = true;
                }
                calls.push(o.call);
            }
        }
        cross_handle.map(|h| h.join().expect("reviewer thread"))
    });
    let (mut review, _) = finish(who, doc, root, &texts, &ran, &extra, calls, cross_out, room, vec![]);
    review.escapes = escapes.lock().unwrap_or_else(|p| p.into_inner()).clone();
    review.reading_room = reading_room;
    review
}

/// Turn the stage answers into alarms, ground and verify them, and decide the verdict.
/// Returns whether the pass rule holds; with stages `skipped` that is never READY.
#[allow(clippy::too_many_arguments)]
fn finish(who: &Reviewers, doc: &Doc, root: &Path, texts: &[Option<String>; 5], ran: &[bool; 5], extra: &[Option<String>], mut calls: Vec<Call>, cross_out: Option<Outcome>, room: &Room, skipped: Vec<Stage>) -> (Review, bool) {
    let text = |st: Stage| texts[STAGES.iter().position(|s| *s == st).expect("stage in STAGES")].as_deref();
    let comprehension: Option<Comprehension> = text(Stage::Comprehension).and_then(parse);
    let critic: Option<Critic> = text(Stage::Critic).and_then(parse);
    let zoom_out: Option<Critic> = text(Stage::ZoomOut).and_then(parse);
    let readiness: Option<Readiness> = text(Stage::Readiness).and_then(parse);
    let invariants: Option<Invariants> = text(Stage::Invariants).and_then(parse);
    let got = [comprehension.is_some(), critic.is_some(), zoom_out.is_some(), readiness.is_some(), invariants.is_some()];
    // Only a stage that ran can fail; a skipped one is listed in `skipped`.
    let mut failed: Vec<Stage> = STAGES.iter().enumerate().filter(|(i, _)| ran[*i] && !got[*i]).map(|(_, s)| *s).collect();
    // Every readiness reader must answer: a missing vote is a failed stage, never a pass.
    let others: Vec<Option<Readiness>> = extra.iter().map(|t| t.as_deref().and_then(parse)).collect();
    if others.iter().any(Option::is_none) && !failed.contains(&Stage::Readiness) {
        failed.push(Stage::Readiness);
    }
    let others: Vec<Readiness> = others.into_iter().flatten().collect();
    let cross_critic = cross_out.and_then(|o| {
        calls.push(o.call);
        o.text.as_deref().and_then(parse)
    });

    let unchecked: Vec<String> = doc.criteria().into_iter().filter(|c| c.check.is_none()).map(|c| c.text).collect();
    let missing_sections: Vec<String> = doc.missing_sections().into_iter().map(String::from).collect();
    let mut alarms = collect_alarms(
        &missing_sections,
        &unchecked,
        StageAnswers {
            comprehension: comprehension.as_ref(),
            readiness: readiness.as_ref(),
            critic: critic.as_ref(),
            cross: cross_critic.as_ref(),
            zoom_out: zoom_out.as_ref(),
        },
        &crate::steps::size_alarms(doc, who.limits),
    );
    push_checklist(&mut alarms, doc, who.verification);
    push_diagrams(&mut alarms, doc, &who.diagrams);
    let n_votes = 1 + others.len();
    if n_votes > 1 {
        for a in alarms.iter_mut().filter(|a| a.source == Source::Readiness) {
            a.vote = Some(0);
        }
        for (k, r) in others.iter().enumerate() {
            let more = collect_alarms(&[], &[], StageAnswers { readiness: Some(r), ..Default::default() }, &[]);
            for mut a in more {
                a.id = alarms.len() + 1;
                a.vote = Some(k + 1);
                alarms.push(a);
            }
        }
    }

    // Step 1: grounding, no model involved.
    let corpus = corpus(doc, root);
    for a in alarms.iter_mut() {
        alarm::ground(a, doc, &corpus);
    }

    // Feature records for every alarm: saved with the review, sent to triage, used for training.
    let context = crate::triage::ReviewContext {
        reader_model: who.reader.model(),
        reader_effort: who.reader.effort(),
        critic_model: who.critic.model(),
        mode: format!("{:?}", who.mode).to_lowercase(),
        run: who.run,
        ..Default::default()
    }
    .with_doc(doc);
    let snapshot = alarms.clone();
    for a in alarms.iter_mut() {
        a.features = Some(crate::triage::features(a, &context, &snapshot));
    }

    // Step 1b: learned triage may mark alarms to skip the verifier. It cannot drop any.
    let mut triage_error = None;
    let use_triage = who.verifier.is_some() && failed.is_empty() && !crate::triage::candidates(&alarms).is_empty();
    if let Some(cfg) = who.triage.filter(|_| use_triage) {
        // The triage call is a model call too: it runs as a session, against the budget.
        let session = crate::session::open("triage");
        match session.and_then(|_s| crate::triage::ask(cfg, &alarms)) {
            Ok(decisions) => crate::triage::apply(&mut alarms, &decisions, &cfg.model, cfg.explore, doc.hash() ^ who.run as u64),
            Err(e) => {
                crate::ui::warn(&format!("triage unavailable ({e:#}); every alarm goes to the verifier"));
                triage_error = Some(format!("{e:#}"));
            }
        }
    }

    // Step 2: escalate surviving model alarms to the verifier, in one fresh session.
    let mut verified = false;
    let pending = failed.is_empty() && alarms.iter().any(|a| a.to_verify());
    if let Some(v) = who.verifier.filter(|_| pending) {
        let criteria = doc.criteria();
        let prompt = alarm::verification_prompt(&brief(doc, root, v.reads_files()), &alarms, &criteria);
        let out = run_one(v, Role::Verify, "verifier", "goldfish-verifier", prompt, doc, room);
        if let Some(j) = out.text.as_deref().and_then(parse::<alarm::Judgments>) {
            alarm::apply_judgments(&mut alarms, &j.items, &corpus, &criteria);
            verified = true;
        } else {
            crate::ui::warn("the verifier gave no usable answer; every alarm stands");
        }
        calls.push(out.call);
    }

    // Step 3: readiness by majority (`EgmVote`): with no strict majority objecting, the
    // minority's questions stop counting; they stay on record.
    if n_votes > 1 {
        apply_vote(&mut alarms, n_votes);
    }

    let mut review = Review::empty(doc, Verdict::Incomplete);
    let mut reviewers: Vec<String> = Vec::new();
    for c in &calls {
        if !reviewers.contains(&c.agent) {
            reviewers.push(c.agent.clone());
        }
    }
    review.tokens = calls.iter().map(|c| c.usage.total()).sum();
    review.reviewers = reviewers;
    review.calls = calls;
    review.comprehension = comprehension;
    review.critic = critic;
    review.cross_critic = cross_critic;
    review.zoom_out = zoom_out;
    if n_votes > 1 {
        review.readiness_votes = readiness.iter().cloned().chain(others).collect();
    }
    review.readiness = readiness;
    review.invariants = invariants;
    review.unchecked = unchecked;
    review.missing_sections = missing_sections;
    review.failed = failed;
    review.alarms = alarms;
    review.verified = verified;
    review.context = context;
    review.triage_error = triage_error;
    review.setup = crate::tuning::active().map(|h| format!("{:016x}", h.fingerprint));
    review.skipped = skipped;
    // The reader's "not ready" is carried by its alarms (an unexplained one becomes
    // an alarm too), so readiness holds exactly when no readiness alarm still counts.
    let ready = !review.open_alarms().iter().any(|a| a.source == Source::Readiness);
    let pass = passes(ready, review.questions().len(), review.blocking().len());
    review.verdict = verdict(!review.failed.is_empty(), pass, review.skipped.len());
    (review, pass)
}

/// The verdict rule. Mirrors `EgmFailFast.verdict`: READY needs the pass rule and no
/// skipped stage; a failed stage makes the review incomplete.
pub fn verdict(failed: bool, pass: bool, skipped: usize) -> Verdict {
    if failed {
        Verdict::Incomplete
    } else if pass && skipped == 0 {
        Verdict::Ready
    } else {
        Verdict::NotReady
    }
}

/// The doc and every file a goldfish may read, normalized, for quote checks.
pub fn corpus(doc: &Doc, root: &Path) -> String {
    let mut all = doc.text.clone();
    for p in doc.context_files(root) {
        if let Ok(t) = std::fs::read_to_string(&p) {
            all.push('\n');
            all.push_str(&t);
        }
    }
    alarm::normalize(&all)
}

/// Every reason the doc might not be ready, numbered from 1.
/// What each stage answered (`None` when it failed).
#[derive(Default)]
pub struct StageAnswers<'a> {
    pub comprehension: Option<&'a Comprehension>,
    pub readiness: Option<&'a Readiness>,
    pub critic: Option<&'a Critic>,
    pub cross: Option<&'a Critic>,
    pub zoom_out: Option<&'a Critic>,
}

pub fn collect_alarms(missing_sections: &[String], unchecked: &[String], answers: StageAnswers, oversized: &[String]) -> Vec<Alarm> {
    let StageAnswers { comprehension, readiness, critic, cross, zoom_out } = answers;
    let mut out: Vec<Alarm> = Vec::new();
    let mut push = |source: Source, kind: Kind, ask: Ask| {
        out.push(Alarm {
            id: out.len() + 1,
            source,
            kind,
            text: ask.text,
            section: ask.section,
            quote: ask.quote,
            search_terms: ask.search_terms,
            grounding: Grounding::NoEvidence,
            status: Status::Open,
            dropped_by: None,
            reason: None,
            features: None,
            triage: None,
            skip_verify: false,
            caught_by: None,
            vote: None,
        })
    };
    for s in missing_sections {
        push(Source::Egm, Kind::Question, Ask { text: format!("The `{s}` section is missing or empty."), section: s.clone(), ..Ask::default() });
    }
    for c in unchecked {
        push(Source::Egm, Kind::Question, Ask { text: format!("Name the check for criterion: {c}"), section: "Acceptance criteria".into(), ..Ask::default() });
    }
    if let Some(c) = comprehension {
        for a in &c.unclear {
            push(Source::Comprehension, Kind::Note, a.clone());
        }
    }
    if let Some(r) = readiness {
        for a in &r.blocking_questions {
            push(Source::Readiness, Kind::Question, a.clone());
        }
        if !r.ready && r.blocking_questions.is_empty() {
            push(Source::Readiness, Kind::Question, Ask { text: "The reader judged the doc not ready but named no question.".into(), ..Ask::default() });
        }
    }
    for (src, c) in [(Source::Critic, critic), (Source::CrossCritic, cross), (Source::ZoomOut, zoom_out)] {
        for f in c.map(|c| c.findings.as_slice()).unwrap_or(&[]) {
            if crate::official::is_blocker(&f.severity) {
                push(src, Kind::Blocking, Ask { text: f.issue.clone(), section: f.cites.clone(), quote: f.quote.clone(), search_terms: f.search_terms.clone() });
            }
        }
    }
    // Last, so alarm numbers from the stages above do not shift.
    for text in oversized {
        push(Source::Egm, Kind::Question, Ask { text: text.clone(), section: "Implementation map".into(), ..Ask::default() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_matches_egm_fail_fast() {
        // EgmFailFast.verdictSkipped: READY needs the pass rule and no skipped stage.
        assert_eq!(verdict(false, true, 0), Verdict::Ready);
        for skipped in 1..=5 {
            assert_eq!(verdict(false, true, skipped), Verdict::NotReady);
        }
        assert_eq!(verdict(true, true, 0), Verdict::Incomplete);
        assert_eq!(verdict(false, false, 0), Verdict::NotReady);
    }

    #[test]
    fn pass_rule_matches_the_lean_model() {
        assert!(passes(true, 0, 0));
        for (r, q, b) in [(false, 0, 0), (true, 1, 0), (true, 0, 1), (false, 3, 2)] {
            assert!(!passes(r, q, b));
        }
    }

    #[test]
    fn stage_prompts_share_a_cacheable_prefix() {
        let doc = Doc { path: "/r/docs/d.md".into(), text: "## Problem\n\nx\n".into() };
        let prompts: Vec<String> = STAGES.iter().map(|s| prompt(*s, &doc, Path::new("/r"), false)).collect();
        let shared = brief(&doc, Path::new("/r"), false);
        assert!(prompts.iter().all(|p| p.starts_with(&shared)));
        let combined = combined_prompt(&doc, Path::new("/r"), false, &[]);
        for key in ["\"comprehension\"", "\"critic\"", "\"zoom_out\"", "\"readiness\"", "\"invariants\""] {
            assert!(combined.contains(key));
        }
    }

    /// Each stage prompt names its own task and no other stage's, so a reply
    /// cannot be attributed to the wrong stage (the e2e fake model relies on it).
    #[test]
    fn each_stage_prompt_carries_only_its_own_task_marker() {
        let doc = Doc { path: "/r/docs/d.md".into(), text: "## Problem\n\nx\n".into() };
        let markers = [
            (Stage::Comprehension, "In your own words"),
            (Stage::Critic, "Act as an expert reviewer"),
            (Stage::ZoomOut, "Zoom out and judge"),
            (Stage::Readiness, "engineer who must implement"),
            (Stage::Invariants, "precise predicate"),
        ];
        for (st, _) in markers {
            let p = prompt(st, &doc, Path::new("/r"), true);
            for (other, m) in markers {
                assert_eq!(p.contains(m), other == st, "{st:?} prompt vs marker {m:?}");
            }
        }
        assert!(prompt(Stage::ZoomOut, &doc, Path::new("/r"), true).contains("Skeptical Architect"));
        assert!(prompt(Stage::Readiness, &doc, Path::new("/r"), true).contains("Zero-Decision Rule"));
        assert!(!prompt(Stage::Invariants, &doc, Path::new("/r"), true).contains("Review persona"));
    }

    #[test]
    fn alarms_cover_every_blocker_and_number_from_one() {
        let r = Readiness { ready: false, blocking_questions: vec![] };
        let c = Critic { findings: vec![
            Finding { severity: "blocking".into(), issue: "b".into(), ..Finding::default() },
            Finding { severity: "minor".into(), issue: "m".into(), ..Finding::default() },
        ] };
        let z = Critic { findings: vec![
            Finding { severity: "Blocker".into(), issue: "objective unclear".into(), ..Finding::default() },
            Finding { severity: "Major".into(), issue: "reuse util".into(), ..Finding::default() },
        ] };
        let a = collect_alarms(
            &["Problem".into()],
            &["crit".into()],
            StageAnswers { readiness: Some(&r), critic: Some(&c), zoom_out: Some(&z), ..Default::default() },
            &["too big".into()],
        );
        let kinds: Vec<(usize, Source, Kind)> = a.iter().map(|x| (x.id, x.source, x.kind)).collect();
        assert_eq!(kinds, vec![
            (1, Source::Egm, Kind::Question),
            (2, Source::Egm, Kind::Question),
            (3, Source::Readiness, Kind::Question), // unexplained "not ready"
            (4, Source::Critic, Kind::Blocking),
            (5, Source::ZoomOut, Kind::Blocking), // upstream "Blocker" maps to blocking; Major does not
            (6, Source::Egm, Kind::Question),     // oversized step: egm-dsh's own, never dropped
        ]);
    }

    #[test]
    fn a_prompt_carries_only_linked_files_inside_the_project() {
        // EgmBuiltin.sent_only_allowed, outside_never_sent, internals_never_sent
        let base = std::env::temp_dir().join(format!("egm-embed-{}", std::process::id()));
        let root = base.join("proj");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(base.join("secret.txt"), "TOP-SECRET").unwrap();
        std::fs::write(root.join(".git").join("config"), "GIT-CONFIG").unwrap();
        std::fs::write(root.join("a.py"), "A-PY").unwrap();
        std::fs::write(root.join("big.txt"), "x".repeat(600 * 1024)).unwrap();
        let text = "# D\n\nSee [a](../a.py), [s](../../secret.txt), [g](../.git/config), [b](../big.txt).\n".to_string();
        let doc = Doc { path: root.join("docs").join("d.md"), text };
        let b = brief(&doc, &root, false);
        assert!(b.contains("A-PY"));
        assert!(!b.contains("TOP-SECRET") && !b.contains("GIT-CONFIG") && !b.contains("xxxxxxxx"));
        assert!(b.contains("outside the project") && b.contains("repository internals") && b.contains("over 512 KiB"));
        std::fs::remove_dir_all(&base).ok();
    }

    fn q(vote: usize, counts: bool) -> Alarm {
        let mut a = collect_alarms(&[], &[], StageAnswers { readiness: Some(&Readiness { ready: false, blocking_questions: vec![Ask { text: format!("q{vote}"), ..Ask::default() }] }), ..Default::default() }, &[]).remove(0);
        a.vote = Some(vote);
        if !counts {
            a.drop_with("verifier", "answered".into());
        }
        a
    }

    #[test]
    fn readiness_blocks_only_on_a_strict_majority() {
        // EgmVote.blocks, only_readiness_is_voted, majority_keeps_every_alarm
        let counting = |xs: &[Alarm]| xs.iter().filter(|a| a.counts()).count();
        let mut two = vec![q(0, true), q(1, true), q(2, false)];
        apply_vote(&mut two, 3);
        assert_eq!(counting(&two), 2, "2 of 3 object: every objecting question stands");
        let mut one = vec![q(0, true), q(1, false), q(2, false)];
        apply_vote(&mut one, 3);
        assert_eq!(counting(&one), 0, "1 of 3: outvoted");
        assert_eq!(one[0].dropped_by.as_deref(), Some("vote"));
        assert!(one[0].reason.as_deref().unwrap().contains("raised by 1 of 3"));
        // a reader that asked two questions is still one vote
        let mut same = vec![q(0, true), q(0, true), q(1, false)];
        apply_vote(&mut same, 3);
        assert_eq!(counting(&same), 0);
        // non-readiness alarms are never voted on
        let blocker = collect_alarms(&[], &[], StageAnswers { critic: Some(&Critic { findings: vec![Finding { severity: "Blocker".into(), issue: "x".into(), ..Finding::default() }] }), ..Default::default() }, &[]);
        let mut mixed = vec![q(0, true), blocker[0].clone()];
        apply_vote(&mut mixed, 3);
        assert!(mixed[1].counts() && !mixed[0].counts());
    }

    #[test]
    fn the_invariants_stage_feeds_no_alarm() {
        // its answer is display only, so turning it off changes no verdict
        let none = collect_alarms(&[], &[], StageAnswers::default(), &[]);
        assert!(none.is_empty());
        let doc = Doc { path: "d.md".into(), text: String::new() };
        let p = combined_prompt(&doc, Path::new("/r"), false, &[Stage::Invariants]);
        assert!(!p.contains("\"invariants\"") && p.contains("\"readiness\""));
        assert!(prompt(Stage::Readiness, &doc, Path::new("/r"), false).contains(BRIEF_ANSWERS));
    }

    #[test]
    fn comprehension_points_are_notes_and_never_block() {
        // EgmAdvice.notes_only_from_comprehension and notes_never_change_the_verdict
        let comp = Comprehension { unclear: vec![Ask { text: "what is jot?".into(), ..Ask::default() }], ..Comprehension::default() };
        let ready = Readiness { ready: true, blocking_questions: vec![] };
        let a = collect_alarms(&[], &[], StageAnswers { comprehension: Some(&comp), readiness: Some(&ready), ..Default::default() }, &[]);
        assert_eq!(a.iter().map(|x| (x.source, x.kind)).collect::<Vec<_>>(), vec![(Source::Comprehension, Kind::Note)]);
        let doc = Doc { path: "d.md".into(), text: String::new() };
        let mut review = Review::empty(&doc, Verdict::Incomplete);
        review.alarms = a;
        assert!(review.open_alarms().is_empty() && review.questions().is_empty() && review.notes().len() == 1);
        assert!(passes(true, review.questions().len(), review.blocking().len()));
    }

    #[test]
    fn parses_fenced_json() {
        let r: Readiness = parse("Here:\n```json\n{\"ready\": true, \"blocking_questions\": []}\n```").unwrap();
        assert!(r.ready && r.blocking_questions.is_empty());
        assert!(parse::<Readiness>("no json here").is_none());
    }
}
