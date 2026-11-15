//! Structured escalation: turn a goldfish's open questions into decisions.
//!
//! After a `not-ready` verdict, the elephant (who can read the repository) takes
//! each question that still counts. It answers what the repository settles, citing
//! a file and exact words from it, and escalates the rest to a person as a decision
//! with labelled options. egm-dsh checks every repository answer itself: an answer
//! whose quote is not in the cited file is escalated, never trusted.
//!
//! The gate does not move here. Nothing in this module produces a gate event, and
//! decisions are never shown to a goldfish. They reach the design only by being
//! written into the doc (a `## Decisions` section plus a redraft of the sections
//! concerned), which changes the doc hash. A fresh goldfish must then pass the new
//! revision before `build` unlocks (`EgmEscalate` in `proofs/EgmGate.lean`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::alarm::{self, Alarm};

/// The shortest repository quote egm-dsh accepts as evidence (normalized characters).
pub const MIN_QUOTE: usize = 12;
/// At most this many options from the elephant, before egm-dsh's own `Other`.
pub const MAX_OPTIONS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opt {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub consequence: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoAnswer {
    pub text: String,
    pub file: String,
    pub quote: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// The option key chosen, or `other`.
    pub key: String,
    pub text: String,
    pub ts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// The repository settles it; egm-dsh found the quote in the cited file.
    Answered,
    /// Waiting for a person.
    Pending,
    /// A person chose.
    Decided,
}

/// Mirrors `Res` in `proofs/EgmGate.lean` (`namespace EgmEscalate`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: usize,
    /// The goldfish alarm this item resolves.
    pub alarm: usize,
    pub question: String,
    #[serde(default)]
    pub section: String,
    pub status: Status,
    #[serde(default)]
    pub answer: Option<RepoAnswer>,
    /// Why a person must decide.
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub options: Vec<Opt>,
    #[serde(default)]
    pub recommended: Option<String>,
    #[serde(default)]
    pub choice: Option<Choice>,
}

/// `.egm-dsh/decisions/<slug>.json`: one escalation round.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub slug: String,
    /// The doc revision the goldfish judged; decisions apply only to it.
    pub doc_hash: String,
    pub review_run: u32,
    pub items: Vec<Item>,
    /// Set once the decisions were written into the doc (the redraft's hash).
    #[serde(default)]
    pub applied_hash: Option<String>,
}

impl Record {
    pub fn pending(&self) -> Vec<&Item> {
        self.items.iter().filter(|i| i.status == Status::Pending).collect()
    }
}

/// `canRedraft` in the Lean model: every item answered or decided.
pub fn can_redraft(items: &[Item]) -> bool {
    items.iter().all(|i| i.status != Status::Pending)
}

// ---------------------------------------------------------------------------------------
// the elephant's reply

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReplyOption {
    #[serde(alias = "option", alias = "choice")]
    pub label: String,
    #[serde(alias = "effect", alias = "tradeoff")]
    pub consequence: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReplyItem {
    #[serde(alias = "id")]
    pub alarm: usize,
    /// Set when the repository settles the question.
    pub answer: String,
    pub file: String,
    pub quote: String,
    /// Set when a person must decide.
    pub question: String,
    #[serde(alias = "why_repo_cannot_decide")]
    pub why: String,
    pub options: Vec<ReplyOption>,
    /// 1-based index into `options`.
    pub recommend: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Reply {
    pub items: Vec<ReplyItem>,
}

pub fn prompt(alarms: &[&Alarm]) -> String {
    let list: Vec<String> = alarms
        .iter()
        .map(|a| {
            let at = if a.section.is_empty() { String::new() } else { format!(" (section: {})", a.section) };
            format!("#{} {}{at}", a.id, a.text)
        })
        .collect();
    format!(
        "A memoryless goldfish reviewer read only the design doc and could not proceed. Its open questions:\n\n{}\n\n\
For each question, do exactly one of these:\n\
1. If the repository already settles it, answer it. Give the file you read (path relative to the repository root) and copy exact words from that file that show the answer. egm-dsh will check that those words are in that file; if they are not, the question goes to the person.\n\
2. Otherwise it is a genuine design decision. Escalate it as a short question for the person, say in one sentence why the repository cannot decide it, and give two to four concrete options, each with its consequence. Recommend one if you have a reason.\n\n\
Do not invent answers to escalate fewer questions, and do not escalate what the code already decides. Do not create or edit files.\n\n\
Reply with only this JSON:\n\
{{\"items\": [{{\"alarm\": 1, \"answer\": \"...\", \"file\": \"path/in/repo\", \"quote\": \"exact words from that file\"}}, \
{{\"alarm\": 2, \"question\": \"...\", \"why\": \"...\", \"options\": [{{\"label\": \"...\", \"consequence\": \"...\"}}], \"recommend\": 1}}]}}",
        list.join("\n")
    )
}

/// Is `rel` a readable repository file, and does it contain `quote`?
pub fn grounded(root: &Path, rel: &str, quote: &str) -> Result<(), String> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() {
        return Err("it names no file".into());
    }
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("`{rel}` is not a path inside the repository"));
    }
    if rel.starts_with(".egm-dsh") || rel.starts_with(".git/") {
        return Err(format!("`{rel}` is egm-dsh or git state, not the repository"));
    }
    let abs = root.join(p);
    let (Ok(abs), Ok(root)) = (abs.canonicalize(), root.canonicalize()) else { return Err(format!("`{rel}` does not exist")) };
    if !abs.starts_with(&root) {
        return Err(format!("`{rel}` resolves outside the repository"));
    }
    let text = std::fs::read_to_string(&abs).map_err(|_| format!("`{rel}` is not a readable text file"))?;
    let q = alarm::normalize(quote);
    if q.chars().count() < MIN_QUOTE {
        return Err(format!("the quote is shorter than {MIN_QUOTE} characters"));
    }
    if !alarm::quote_found(&alarm::normalize(&text), quote) {
        return Err(format!("the quoted words are not in `{rel}`"));
    }
    Ok(())
}

fn key(i: usize) -> String {
    char::from(b'A' + i as u8).to_string()
}

/// egm-dsh's own last option, on every escalation.
fn other(i: usize) -> Opt {
    Opt { key: key(i), label: "Other: give your own answer".into(), consequence: "egm-dsh decide … --other \"your answer\"".into() }
}

/// One alarm and the elephant's reply to it (if any) → one item. Mirrors `classify`:
/// only an answer egm-dsh grounded is `Answered`; everything else is `Pending` with at
/// least one labelled option.
pub fn classify(id: usize, a: &Alarm, r: Option<&ReplyItem>, check: impl Fn(&str, &str) -> Result<(), String>) -> Item {
    let mut item = Item {
        id,
        alarm: a.id,
        question: a.text.clone(),
        section: a.section.clone(),
        status: Status::Pending,
        answer: None,
        why: String::new(),
        options: Vec::new(),
        recommended: None,
        choice: None,
    };
    let Some(r) = r else {
        item.why = "the elephant did not address this question".into();
        item.options.push(other(0));
        return item;
    };
    let mut opts: Vec<Opt> = Vec::new();
    if !r.answer.trim().is_empty() {
        match check(&r.file, &r.quote) {
            Ok(()) => {
                item.status = Status::Answered;
                item.answer = Some(RepoAnswer { text: r.answer.trim().into(), file: r.file.trim().into(), quote: r.quote.trim().into() });
                return item;
            }
            Err(why) => {
                item.why = format!("the elephant answered from the repository, but egm-dsh could not confirm it: {why}");
                opts.push(Opt { key: String::new(), label: format!("Accept the elephant's answer: {}", r.answer.trim()), consequence: "not confirmed against the repository".into() });
            }
        }
    }
    if !r.question.trim().is_empty() {
        item.question = format!("{} (goldfish: {})", r.question.trim(), a.text);
    }
    if item.why.is_empty() {
        item.why = if r.why.trim().is_empty() { "a design decision the repository does not settle".into() } else { r.why.trim().into() };
    }
    for o in r.options.iter().filter(|o| !o.label.trim().is_empty()) {
        if opts.len() >= MAX_OPTIONS {
            break;
        }
        if !opts.iter().any(|x| x.label.eq_ignore_ascii_case(o.label.trim())) {
            let rec = r.recommend.is_some_and(|n| r.options.get(n.wrapping_sub(1)).is_some_and(|x| std::ptr::eq(x, o)));
            opts.push(Opt { key: String::new(), label: o.label.trim().into(), consequence: o.consequence.trim().into() });
            if rec {
                item.recommended = Some(key(opts.len() - 1));
            }
        }
    }
    for (i, o) in opts.iter_mut().enumerate() {
        o.key = key(i);
    }
    let n = opts.len();
    opts.push(other(n));
    item.options = opts;
    item
}

/// Build the round's record from the review's counting alarms and the elephant's reply.
pub fn build_record(slug: &str, doc_hash: u64, run: u32, alarms: &[&Alarm], reply: Option<&Reply>, root: &Path) -> Record {
    let items = alarms
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let r = reply.and_then(|r| r.items.iter().find(|x| x.alarm == a.id));
            classify(i + 1, a, r, |f, q| grounded(root, f, q))
        })
        .collect();
    Record { slug: slug.into(), doc_hash: format!("{doc_hash:016x}"), review_run: run, items, applied_hash: None }
}

/// Record a person's choice. `key` is an option key (case-insensitive); `other` takes
/// free text instead. Only a pending or already-decided item can be (re)decided.
pub fn decide(rec: &mut Record, id: usize, key: Option<&str>, other: Option<&str>, ts: u64) -> anyhow::Result<String> {
    let item = rec.items.iter_mut().find(|i| i.id == id).ok_or_else(|| anyhow::anyhow!("no decision #{id}; `egm-dsh decide {}` lists them", rec.slug))?;
    if item.status == Status::Answered {
        anyhow::bail!("#{id} was answered from the repository; nothing to decide");
    }
    let choice = match (key, other.map(str::trim).filter(|s| !s.is_empty())) {
        (_, Some(text)) => Choice { key: "other".into(), text: text.into(), ts },
        (Some(k), None) => {
            let o = item
                .options
                .iter()
                .find(|o| o.key.eq_ignore_ascii_case(k.trim()))
                .ok_or_else(|| anyhow::anyhow!("#{id} has options {}", item.options.iter().map(|o| o.key.as_str()).collect::<Vec<_>>().join(", ")))?;
            if o.label.starts_with("Other:") {
                anyhow::bail!("option {} needs your answer: `--other \"...\"`", o.key);
            }
            Choice { key: o.key.clone(), text: o.label.clone(), ts }
        }
        (None, None) => anyhow::bail!("give an option key or --other \"your answer\""),
    };
    let text = choice.text.clone();
    item.choice = Some(choice);
    item.status = Status::Decided;
    Ok(text)
}

/// The doc's `## Decisions` section after this round: earlier entries kept, this
/// round's appended. This is how decisions reach a goldfish: as part of the doc.
pub fn decisions_section(existing: Option<&str>, rec: &Record) -> String {
    let mut s = String::from("## Decisions\n\n");
    match existing.map(str::trim).filter(|t| !t.is_empty()) {
        Some(prev) => {
            s.push_str(prev);
            s.push_str("\n\n");
        }
        None => s.push_str("Questions a goldfish reviewer raised, and how each was settled. The sections above follow these answers.\n\n"),
    }
    for i in &rec.items {
        let at = if i.section.is_empty() { String::new() } else { format!(", {}", i.section) };
        s.push_str(&format!("- **Q (review {}{at}):** {}\n", rec.review_run, one_line(&i.question)));
        match (&i.answer, &i.choice) {
            (Some(a), _) => s.push_str(&format!("  **Settled by the repository:** {} (`{}`: \"{}\")\n", one_line(&a.text), a.file, one_line(&a.quote))),
            (None, Some(c)) => {
                let consequence = i.options.iter().find(|o| o.key == c.key).map(|o| o.consequence.as_str()).filter(|c| !c.is_empty());
                s.push_str(&format!("  **Decided:** {}{}\n", one_line(&c.text), consequence.map(|c| format!(" ({})", one_line(c))).unwrap_or_default()));
            }
            (None, None) => s.push_str("  **Undecided.**\n"),
        }
    }
    s
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The sections a redraft must touch: the sections the questions named, resolved to
/// the doc's known sections; `Technical plan` when none resolves.
pub fn sections_to_redraft(rec: &Record) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for i in &rec.items {
        let lc = i.section.trim().trim_start_matches('#').trim().to_lowercase();
        if lc.is_empty() {
            continue;
        }
        if let Some(s) = crate::doc::SECTIONS.iter().find(|s| lc.contains(&s.to_lowercase()) || s.to_lowercase().contains(&lc))
            && !out.contains(s)
        {
            out.push(s);
        }
    }
    if out.is_empty() {
        out.push("Technical plan");
    }
    out
}

/// What every redraft turn is told first: the settled answers.
fn settled_lead(rec: &Record) -> String {
    let settled: Vec<String> = rec
        .items
        .iter()
        .map(|i| match (&i.answer, &i.choice) {
            (Some(a), _) => format!("- {} → from the repository: {} (`{}`)", one_line(&i.question), one_line(&a.text), a.file),
            (None, Some(c)) => format!("- {} → the person decided: {}", one_line(&i.question), one_line(&c.text)),
            _ => format!("- {} → undecided", one_line(&i.question)),
        })
        .collect();
    format!(
        "A goldfish reviewer could not use the design doc. These questions are now settled:\n\n{}\n\n\
Redraft so a reader who sees only the doc finds each answer stated plainly in the section it concerns, with no decision left to the implementer. Keep everything else that is still right.",
        settled.join("\n")
    )
}

pub fn redraft_prompt(section: &str, rec: &Record) -> String {
    format!(
        "{}\n\nRedraft the `## {section}` section. {}\n\n\
Reply with only that section in Markdown, starting with the line `## {section}`. Do not create or edit files; I will save it.",
        settled_lead(rec),
        crate::doc::guidance(section)
    )
}

/// All the sections a redraft touches, in one turn.
pub fn redraft_all_prompt(sections: &[&str], rec: &Record) -> String {
    crate::doc::draft_all_prompt(sections, &settled_lead(rec))
}

/// `loop`'s stall rule (`EgmLoop.stalled`): the open-finding count has not fallen
/// over the last `k` rounds.
pub fn stalled(history: &[usize], k: usize) -> bool {
    k > 0 && history.len() > k && history[history.len() - 1] >= history[history.len() - 1 - k]
}

/// `loop` redrafts on its own only when every question was answered from the
/// repository (`EgmLoop.autoRedraft`); anything a person must decide stops it.
pub fn auto_redraft(items: &[Item]) -> bool {
    items.iter().all(|i| i.status == Status::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::{Grounding, Kind, Source};

    fn alarm(id: usize, text: &str, section: &str) -> Alarm {
        Alarm {
            id,
            source: Source::Readiness,
            kind: Kind::Question,
            text: text.into(),
            section: section.into(),
            quote: String::new(),
            search_terms: vec![],
            grounding: Grounding::NoEvidence,
            status: alarm::Status::Open,
            dropped_by: None,
            reason: None,
            features: None,
            triage: None,
            skip_verify: false,
            caught_by: None,
            vote: None,
        }
    }

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn repo() -> Tmp {
        let d = Tmp(std::env::temp_dir().join(format!("egm-esc-{}", crate::project::now_nanos())));
        std::fs::create_dir_all(d.path()).unwrap();
        std::fs::create_dir_all(d.path().join("jot")).unwrap();
        std::fs::write(d.path().join("jot/cli.py"), "def save(notes):\n    json.dump(notes, open('notes.json', 'w'))\n").unwrap();
        std::fs::create_dir_all(d.path().join(".egm-dsh")).unwrap();
        std::fs::write(d.path().join(".egm-dsh/x.json"), "json.dump(notes, open('notes.json', 'w'))").unwrap();
        d
    }

    #[test]
    fn grounding_needs_the_quote_in_a_repository_file() {
        let d = repo();
        assert!(grounded(d.path(), "jot/cli.py", "json.dump(notes, open('notes.json'").is_ok());
        assert!(grounded(d.path(), "./jot/cli.py", "JSON.dump(notes,   open").is_ok(), "normalized");
        assert!(grounded(d.path(), "jot/cli.py", "sqlite3.connect(db)").is_err(), "invented quote");
        assert!(grounded(d.path(), "jot/cli.py", "notes").is_err(), "too short to be evidence");
        assert!(grounded(d.path(), "jot/missing.py", "json.dump(notes, open").is_err());
        assert!(grounded(d.path(), "../etc/passwd", "root:x:0:0:root:/root").is_err());
        assert!(grounded(d.path(), "/etc/hostname", "anything at all here").is_err());
        assert!(grounded(d.path(), ".egm-dsh/x.json", "json.dump(notes, open").is_err(), "egm state is not the repository");
        assert!(grounded(d.path(), "", "json.dump(notes, open").is_err());
    }

    #[test]
    fn only_a_grounded_answer_is_answered_and_every_pending_item_has_labelled_options() {
        let d = repo();
        let a = alarm(3, "Where are notes stored?", "Technical plan");
        let check = |f: &str, q: &str| grounded(d.path(), f, q);
        let good = ReplyItem { alarm: 3, answer: "In notes.json".into(), file: "jot/cli.py".into(), quote: "json.dump(notes, open('notes.json'".into(), ..Default::default() };
        let i = classify(1, &a, Some(&good), check);
        assert_eq!(i.status, Status::Answered);
        assert!(i.options.is_empty() && i.answer.is_some());

        let bad = ReplyItem { answer: "In SQLite".into(), file: "jot/cli.py".into(), quote: "sqlite3.connect(db)".into(), ..good.clone() };
        let i = classify(1, &a, Some(&bad), check);
        assert_eq!(i.status, Status::Pending, "an unconfirmed answer goes to the person");
        assert!(i.why.contains("could not confirm"));
        assert_eq!(i.options[0].key, "A");
        assert!(i.options[0].label.contains("In SQLite"));
        assert!(i.options.last().unwrap().label.starts_with("Other"));

        let i = classify(1, &a, None, check);
        assert_eq!(i.status, Status::Pending);
        assert_eq!(i.options.len(), 1, "unaddressed: egm-dsh's Other option only");

        let many: Vec<ReplyOption> = (0..7).map(|n| ReplyOption { label: format!("option {n}"), consequence: "c".into() }).collect();
        let esc = ReplyItem { alarm: 3, question: "Keep or drop old notes?".into(), why: "a product choice".into(), options: many, recommend: Some(2), ..Default::default() };
        let i = classify(1, &a, Some(&esc), check);
        assert_eq!(i.options.len(), MAX_OPTIONS + 1);
        assert_eq!(i.options.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(), ["A", "B", "C", "D", "E"]);
        assert_eq!(i.recommended.as_deref(), Some("B"));
        assert!(i.question.contains("Keep or drop") && i.question.contains("Where are notes stored?"), "the goldfish's words are kept");
        // every pending item has at least one labelled option, whatever the reply
        for r in [None, Some(&good), Some(&bad), Some(&esc)] {
            let i = classify(1, &a, r, check);
            assert!(i.status == Status::Answered || (!i.options.is_empty() && i.options.iter().all(|o| !o.key.is_empty() && !o.label.is_empty())));
        }
    }

    #[test]
    fn decisions_are_required_before_a_redraft_and_land_in_the_doc() {
        let d = repo();
        let alarms = [alarm(1, "Where are notes stored?", "Technical plan"), alarm(2, "Keep old notes?", "Risks")];
        let refs: Vec<&Alarm> = alarms.iter().collect();
        let reply = Reply {
            items: vec![
                ReplyItem { alarm: 1, answer: "notes.json".into(), file: "jot/cli.py".into(), quote: "json.dump(notes, open('notes.json'".into(), ..Default::default() },
                ReplyItem { alarm: 2, question: "Keep old notes?".into(), options: vec![ReplyOption { label: "Keep them".into(), consequence: "no migration".into() }, ReplyOption { label: "Drop them".into(), consequence: "data loss".into() }], ..Default::default() },
            ],
        };
        let mut rec = build_record("tags", 0xabc, 2, &refs, Some(&reply), d.path());
        assert!(!can_redraft(&rec.items));
        assert!(decide(&mut rec, 1, Some("A"), None, 0).is_err(), "answered items need no decision");
        assert!(decide(&mut rec, 2, Some("Z"), None, 0).is_err());
        assert!(decide(&mut rec, 2, Some("C"), None, 0).is_err(), "Other needs text");
        assert!(decide(&mut rec, 9, Some("A"), None, 0).is_err());
        assert_eq!(decide(&mut rec, 2, Some("a"), None, 7).unwrap(), "Keep them");
        assert!(can_redraft(&rec.items));
        decide(&mut rec, 2, None, Some("Keep them, read as untagged"), 8).unwrap();
        assert_eq!(rec.items[1].choice.as_ref().unwrap().key, "other");
        let s = decisions_section(None, &rec);
        assert!(s.starts_with("## Decisions\n"));
        assert!(s.contains("Settled by the repository") && s.contains("`jot/cli.py`"));
        assert!(s.contains("Decided:** Keep them, read as untagged"));
        let again = decisions_section(crate::doc::section_of(&s, "Decisions").as_deref(), &rec);
        assert_eq!(again.matches("**Q (review 2").count(), 4, "earlier rounds are kept");
        assert_eq!(sections_to_redraft(&rec), vec!["Technical plan", "Risks and mitigations"]);
    }

    /// `classify` in the Lean model: none → 1 option, unconfirmed answer → 2,
    /// n options → min n 4 + 1, grounded answer → answered.
    #[test]
    fn classify_matches_the_lean_model() {
        let d = repo();
        let a = alarm(1, "q", "");
        let check = |f: &str, q: &str| grounded(d.path(), f, q);
        let k = |r: Option<&ReplyItem>| {
            let i = classify(1, &a, r, check);
            (i.status, i.options.len())
        };
        assert_eq!(k(None), (Status::Pending, 1));
        let ans = |q: &str| ReplyItem { alarm: 1, answer: "x".into(), file: "jot/cli.py".into(), quote: q.into(), ..Default::default() };
        assert_eq!(k(Some(&ans("json.dump(notes, open('notes.json'"))), (Status::Answered, 0));
        assert_eq!(k(Some(&ans("not in the file at all"))), (Status::Pending, 2));
        for n in 0..8 {
            let r = ReplyItem { alarm: 1, options: (0..n).map(|j| ReplyOption { label: format!("o{j}"), consequence: String::new() }).collect(), ..Default::default() };
            assert_eq!(k(Some(&r)), (Status::Pending, n.min(MAX_OPTIONS) + 1), "n = {n}");
        }
    }

    /// `escalation_needs_fresh_pass`, bounded: after a not-ready goldfish and the
    /// redraft's edit (same or new hash), every event trace up to length 6 that
    /// reaches built or hardened contains a passing goldfish.
    #[test]
    fn every_trace_after_escalation_needs_a_fresh_pass() {
        use crate::phase::{Event, Gate, Phase};
        let events = [Event::EditDoc(1), Event::EditDoc(2), Event::EditDoc(3), Event::Goldfish(false), Event::Goldfish(true), Event::Build, Event::Review(false), Event::Review(true)];
        fn walk(g: Gate, depth: usize, passed: bool, events: &[Event], n: &mut u64) {
            *n += 1;
            if matches!(g.phase, Phase::Built | Phase::Hardened) {
                assert!(passed, "built without a fresh goldfish pass: {g:?}");
                assert_eq!(g.passed_hash, Some(g.doc_hash));
            }
            if depth == 0 {
                return;
            }
            for e in events {
                let next = g.step(*e).unwrap_or(g);
                walk(next, depth - 1, passed || *e == Event::Goldfish(true), events, n);
            }
        }
        let mut n = 0;
        for start in [Phase::Designing, Phase::Ready] {
            let g = Gate { phase: start, doc_hash: 1, passed_hash: (start == Phase::Ready).then_some(1) };
            let after_review = g.step(Event::Goldfish(false)).unwrap();
            assert_eq!(after_review.phase, Phase::Designing);
            for h in [1, 2] {
                let redrafted = after_review.step(Event::EditDoc(h)).unwrap();
                walk(redrafted, 6, false, &events, &mut n);
            }
        }
        assert!(n > 1_000_000, "{n} states");
    }

    /// The memoryless-goldfish property: a doc that links egm-dsh's own state (the
    /// decisions record, reviews) does not get it into a goldfish's context.
    #[test]
    fn goldfish_context_never_includes_egm_state() {
        let d = repo();
        std::fs::create_dir_all(d.path().join("docs")).unwrap();
        std::fs::create_dir_all(d.path().join(".egm-dsh/decisions")).unwrap();
        std::fs::write(d.path().join(".egm-dsh/decisions/tags.json"), "{}").unwrap();
        let doc = d.path().join("docs/tags.md");
        std::fs::write(&doc, "# T\n\nSee [decisions](../.egm-dsh/decisions/tags.json) and [cli](../jot/cli.py).\n").unwrap();
        let files = crate::doc::Doc::load(&doc).unwrap().context_files(d.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("jot/cli.py"));
    }

    #[test]
    fn loop_rules_match_the_lean_model() {
        assert!(stalled(&[5, 4, 4, 4], 2));
        assert!(!stalled(&[5, 4, 3], 2));
        assert!(!stalled(&[5], 2));
        assert!(stalled(&[3, 3], 1) && !stalled(&[3, 2], 1) && !stalled(&[3, 3], 0));
        let d = repo();
        let a = alarm(1, "q", "");
        let answered = classify(1, &a, Some(&ReplyItem { alarm: 1, answer: "x".into(), file: "jot/cli.py".into(), quote: "json.dump(notes, open('notes.json'".into(), ..Default::default() }), |f, q| grounded(d.path(), f, q));
        let pending = classify(2, &a, None, |f, q| grounded(d.path(), f, q));
        assert!(auto_redraft(std::slice::from_ref(&answered)));
        assert!(!auto_redraft(&[answered.clone(), pending.clone()]), "a question for a person stops the loop");
        let mut decided = pending;
        decided.status = Status::Decided;
        assert!(!auto_redraft(&[answered, decided]), "the loop never redrafts on a person's decision by itself");
    }
}
