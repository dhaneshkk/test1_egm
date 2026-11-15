//! PlantUML sequence diagrams in design docs.
//!
//! A flow told in prose is long and easy to misread: who calls whom, in what
//! order, and what happens on the error path. A sequence diagram says it in a few
//! lines, and a goldfish reads it as easily as text. With the diagram policy on
//! (`egm-dsh set-diagrams prefer|require`), the elephant is asked to show every
//! interaction the design changes as a ```plantuml block and to keep prose for
//! what a diagram cannot say (data shapes, invariants, rationale).
//!
//! egm-dsh checks every diagram itself, so a malformed one is egm-dsh's own
//! question (no model can drop it, and fail-fast catches it with no model call):
//!
//! * **structure**, always, with no dependency: `@startuml`/`@enduml`, at least one
//!   message, every `alt`/`opt`/`loop`/`par`/`break`/`critical`/`group` closed by
//!   `end` and `else` only inside `alt`/`par`/`critical`, notes and refs closed,
//!   no `deactivate` without `activate`, and no preprocessor lines (`!include`
//!   and the like could read files or the network when the diagram is rendered).
//!   PlantUML itself closes an unclosed `alt` silently; here it is an error,
//!   because the reader cannot tell where the branch ends.
//! * **PlantUML's own parser**, when `plantuml_cmd` is set (or `plantuml` is on the
//!   PATH): `-syntax` on each block that passed the structure check.
//!
//! With `require`, a Technical plan without a valid sequence diagram is a question
//! too. Diagram lines do not count against the draft word limit.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    /// No diagram guidance; diagrams that are there are still checked.
    Off,
    /// Ask the elephant for sequence diagrams; check the ones it writes.
    #[default]
    Prefer,
    /// Also: a Technical plan without a valid sequence diagram is a question.
    Require,
}

/// Who checks PlantUML's own syntax.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Checker {
    /// The built-in Rust port of PlantUML (`plantuml-core`): no Java, no process, no I/O.
    #[default]
    Builtin,
    /// An external PlantUML command (`-syntax` is added), e.g. `java -jar plantuml.jar`.
    External(Vec<String>),
    /// Structure check only.
    None,
}

impl Checker {
    pub fn describe(&self) -> String {
        match self {
            Checker::Builtin => format!("built-in plantuml-core {} (Rust port of PlantUML {}; no Java)", BUILTIN_VERSION, builtin_plantuml_version()),
            Checker::External(c) => format!("`{}`", c.join(" ")),
            Checker::None => "none (egm-dsh's structure check only)".into(),
        }
    }
}

pub const BUILTIN_VERSION: &str = "0.5.4";

fn builtin_plantuml_version() -> &'static str {
    #[cfg(feature = "plantuml-core")]
    {
        plantuml_core::VERSION
    }
    #[cfg(not(feature = "plantuml-core"))]
    {
        "(not compiled in)"
    }
}

/// The checker in force: off, an external command, the built-in one, or `plantuml` on the PATH.
pub fn checker(enabled: bool, configured: &[String]) -> Checker {
    if !enabled {
        return Checker::None;
    }
    if !configured.is_empty() {
        return Checker::External(configured.to_vec());
    }
    if cfg!(feature = "plantuml-core") {
        return Checker::Builtin;
    }
    let path = plantuml_cmd(&[]);
    if path.is_empty() { Checker::None } else { Checker::External(path) }
}

/// One verdict from whichever checker is in force (see `plantuml_syntax` for the meaning).
pub fn syntax(checker: &Checker, body: &[String]) -> Result<Option<String>, String> {
    match checker {
        Checker::Builtin => builtin_syntax(&body.join("\n")),
        Checker::External(cmd) => plantuml_syntax(cmd, body),
        Checker::None => Ok(None),
    }
}

/// The built-in check. plantuml-core keeps per-thread state that is slow to build (about
/// 170 ms), so one worker thread serves every call. A panic inside it, or no answer within
/// 10 s, is a tool failure (`Err`): never a pass, never a blocked doc.
#[cfg(feature = "plantuml-core")]
fn builtin_syntax(text: &str) -> Result<Option<String>, String> {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Mutex;
    type Job = (String, Sender<Result<Result<String, plantuml_core::Error>, ()>>);
    static WORKER: Mutex<Option<Sender<Job>>> = Mutex::new(None);
    let tx = {
        let mut w = WORKER.lock().unwrap_or_else(|p| p.into_inner());
        w.get_or_insert_with(|| {
            let (tx, rx) = channel::<Job>();
            std::thread::spawn(move || {
                for (src, reply) in rx {
                    let prev = std::panic::take_hook();
                    std::panic::set_hook(Box::new(|_| {}));
                    let r = std::panic::catch_unwind(|| plantuml_core::render(&src)).map_err(|_| ());
                    std::panic::set_hook(prev);
                    let _ = reply.send(r);
                }
            });
            tx
        })
        .clone()
    };
    let (rtx, rrx) = channel();
    if tx.send((text.to_string(), rtx)).is_err() {
        return Err("the built-in PlantUML check stopped".into());
    }
    match rrx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(Ok(_svg))) => Ok(None),
        Ok(Ok(Err(plantuml_core::Error::Syntax(m)))) => Ok(Some(format!("PlantUML: {}", m.trim()))),
        Ok(Ok(Err(plantuml_core::Error::NoDiagram))) => Ok(Some("PlantUML: no @startuml block".into())),
        Ok(Ok(Err(plantuml_core::Error::Unsupported(k)))) => Err(format!("plantuml-core does not support `{k}` diagrams")),
        Ok(Err(())) => Err("plantuml-core panicked on this diagram".into()),
        Err(_) => {
            // a hung worker serves no one: drop it so the next call starts a fresh one
            *WORKER.lock().unwrap_or_else(|p| p.into_inner()) = None;
            Err("plantuml-core gave no answer within 10 s".into())
        }
    }
}

#[cfg(not(feature = "plantuml-core"))]
fn builtin_syntax(_text: &str) -> Result<Option<String>, String> {
    Err("egm-dsh was built without the `plantuml-core` feature".into())
}

/// What a review checks about diagrams.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    pub policy: Policy,
    /// Who checks PlantUML syntax after the structure check.
    pub checker: Checker,
    /// The fix lane: never required to have a diagram.
    pub exempt: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// The `## ` section it sits in ("" before the first one).
    pub section: String,
    /// 1-based line of the opening fence in the doc.
    pub line: usize,
    pub body: Vec<String>,
}

/// Every ```plantuml (or ```puml / ```uml) block, with its section.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut fence: Option<(bool, usize, Vec<String>)> = None; // (is plantuml, line, body)
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if let Some(info) = t.strip_prefix("```") {
            match fence.take() {
                Some((is_uml, at, body)) => {
                    if !info.trim().is_empty() {
                        // an opening fence inside a fence: keep it as content
                        let mut body = body;
                        body.push(line.to_string());
                        fence = Some((is_uml, at, body));
                        continue;
                    }
                    if is_uml {
                        out.push(Block { section: section.clone(), line: at, body });
                    }
                }
                None => {
                    let lang = info.trim().to_ascii_lowercase();
                    fence = Some((matches!(lang.as_str(), "plantuml" | "puml" | "uml"), i + 1, Vec::new()));
                }
            }
            continue;
        }
        match fence.as_mut() {
            Some((_, _, body)) => body.push(line.to_string()),
            None => {
                if let Some(h) = t.strip_prefix("## ") {
                    section = h.trim().to_string();
                }
            }
        }
    }
    out
}

/// The text with every fenced diagram removed (for word counts).
pub fn without_diagrams(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_uml = false;
    let mut in_other = false;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(info) = t.strip_prefix("```") {
            let lang = info.trim().to_ascii_lowercase();
            if in_uml && lang.is_empty() {
                in_uml = false;
                continue;
            }
            if !in_uml && !in_other && matches!(lang.as_str(), "plantuml" | "puml" | "uml") {
                in_uml = true;
                continue;
            }
            if !in_uml {
                in_other = if in_other { !lang.is_empty() } else { true };
            }
        }
        if !in_uml {
            out.push(line);
        }
    }
    out.join("\n")
}

const OPENERS: [&str; 7] = ["alt", "opt", "loop", "par", "break", "critical", "group"];
const DECLS: [&str; 8] = ["participant", "actor", "boundary", "control", "entity", "database", "collections", "queue"];

fn first_word(l: &str) -> &str {
    l.split(|c: char| c.is_whitespace()).next().unwrap_or("")
}

/// Where the arrow is, if `l` is a message line: (left, right of the arrow, before any `:`).
fn message(l: &str) -> Option<(String, String)> {
    let head = l.split(':').next().unwrap_or(l);
    let i = head.find("->").or_else(|| head.find("<-"))?;
    let left = head[..i].trim_end_matches(['-', '<', '/', '\\', 'o', 'x']).trim();
    let mut j = i + 2;
    let b = head.as_bytes();
    while j < b.len() && matches!(b[j], b'-' | b'>' | b'<' | b'x' | b'o' | b'/' | b'\\') {
        j += 1;
    }
    let right = head[j..].trim().trim_start_matches(['+', '-', '*', '!']).trim().to_string();
    Some((left.trim_start_matches(['+', '-', '*', '!']).trim().to_string(), right))
}

/// Structural problems with one diagram (empty: well formed).
pub fn check(body: &[String]) -> Vec<String> {
    let mut errs = Vec::new();
    let lines: Vec<(usize, &str)> = body.iter().enumerate().map(|(i, l)| (i + 1, l.trim())).filter(|(_, l)| !l.is_empty()).collect();
    if lines.first().is_none_or(|(_, l)| !l.starts_with("@startuml")) {
        errs.push("it does not start with `@startuml`".into());
    }
    if lines.last().is_none_or(|(_, l)| !l.starts_with("@enduml")) {
        errs.push("it does not end with `@enduml`".into());
    }
    let mut stack: Vec<(&str, usize)> = Vec::new(); // (kind, line)
    let mut active: std::collections::HashMap<String, usize> = Default::default();
    let mut messages = 0;
    for &(n, l) in lines.iter().filter(|(_, l)| !l.starts_with("@startuml") && !l.starts_with("@enduml")) {
        let w = first_word(l).to_ascii_lowercase();
        // inside a multi-line note, ref or legend, only its terminator matters
        if let Some(&(kind, _)) = stack.last()
            && matches!(kind, "note" | "ref" | "legend")
        {
            let lower = l.to_ascii_lowercase().replace(' ', "");
            let closes = match kind {
                "note" => lower == "endnote" || lower == "endhnote" || lower == "endrnote",
                "ref" => lower == "endref",
                _ => lower == "endlegend",
            };
            if closes {
                stack.pop();
            }
            continue;
        }
        if l.starts_with('\'') {
            continue;
        }
        if l.starts_with('!') {
            errs.push(format!("line {n}: preprocessor line `{}` is not allowed (it could read files or the network when rendered)", first_word(l)));
            continue;
        }
        if OPENERS.contains(&w.as_str()) {
            stack.push((OPENERS[OPENERS.iter().position(|o| *o == w).unwrap()], n));
            continue;
        }
        if w == "else" {
            match stack.last() {
                Some((k, _)) if matches!(*k, "alt" | "par" | "critical") => {}
                _ => errs.push(format!("line {n}: `else` outside `alt`, `par` or `critical`")),
            }
            continue;
        }
        if w == "end" {
            let rest = l[3..].trim().to_ascii_lowercase();
            if rest == "box" {
                match stack.last() {
                    Some(("box", _)) => {
                        stack.pop();
                    }
                    _ => errs.push(format!("line {n}: `end box` without `box`")),
                }
            } else if !rest.is_empty() {
                errs.push(format!("line {n}: `end {rest}` closes nothing open"));
            } else {
                match stack.last() {
                    Some((k, _)) if OPENERS.contains(k) => {
                        stack.pop();
                    }
                    _ => errs.push(format!("line {n}: `end` without an open `alt`, `opt`, `loop`, `par`, `break`, `critical` or `group`")),
                }
            }
            continue;
        }
        if matches!(w.as_str(), "note" | "hnote" | "rnote") && !l.contains(':') {
            stack.push(("note", n));
            continue;
        }
        if w == "ref" && !l.contains(':') {
            stack.push(("ref", n));
            continue;
        }
        if w == "legend" {
            stack.push(("legend", n));
            continue;
        }
        if w == "box" {
            stack.push(("box", n));
            continue;
        }
        if w == "activate" || w == "deactivate" {
            let who = l[w.len()..].split_whitespace().next().unwrap_or("").to_string();
            if who.is_empty() {
                errs.push(format!("line {n}: `{w}` names no participant"));
            } else if w == "activate" {
                *active.entry(who).or_default() += 1;
            } else {
                match active.get_mut(&who) {
                    Some(c) if *c > 0 => *c -= 1,
                    _ => errs.push(format!("line {n}: `deactivate {who}` without `activate {who}`")),
                }
            }
            continue;
        }
        if DECLS.contains(&w.as_str()) {
            continue;
        }
        if let Some((left, right)) = message(l) {
            if (left.is_empty() && !l.trim_start().starts_with('[')) || (right.is_empty() && !l.contains("->]")) {
                errs.push(format!("line {n}: message `{l}` is missing a participant"));
            }
            messages += 1;
        }
    }
    for (kind, n) in stack {
        errs.push(format!("`{kind}` opened on line {n} is never closed"));
    }
    if messages == 0 {
        errs.push("it has no messages (`A -> B : what`), so it is not a sequence diagram".into());
    }
    errs
}

/// PlantUML's own verdict on one block: `Ok(None)` when it parses, `Ok(Some(error))`
/// for a syntax error, `Err(why)` when PlantUML itself could not run. A run counts as a
/// pass only if PlantUML exits 0; silence from a crashed Java is never a pass.
pub fn plantuml_syntax(cmd: &[String], body: &[String]) -> Result<Option<String>, String> {
    use std::sync::Mutex;
    type Memo = std::collections::HashMap<u64, Result<Option<String>, String>>;
    static MEMO: Mutex<Option<Memo>> = Mutex::new(None);
    let text = body.join("\n");
    let key = crate::doc::hash(&format!("{}\n{text}", cmd.join(" ")));
    if let Some(v) = MEMO.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(Default::default).get(&key) {
        return v.clone();
    }
    let Some((prog, args)) = cmd.split_first() else { return Err("no PlantUML command".into()) };
    let run = || -> anyhow::Result<std::process::Output> {
        use std::io::Write;
        let mut child = std::process::Command::new(prog)
            .args(args)
            .arg("-syntax")
            .env("PLANTUML_SECURITY_PROFILE", "SANDBOX")
            .env("JAVA_TOOL_OPTIONS", std::env::var("JAVA_TOOL_OPTIONS").unwrap_or_default() + " -Djava.awt.headless=true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        // A PlantUML that crashes at start exits before reading its input; the broken
        // pipe is not the error to report, its exit status and stderr are.
        let _ = child.stdin.take().expect("piped").write_all(text.as_bytes());
        Ok(child.wait_with_output()?)
    };
    let v = match run() {
        Err(e) => Err(format!("cannot start `{}`: {e}", cmd.join(" "))),
        Ok(out) => verdict(out.status.code(), &String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr)),
    };
    MEMO.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(Default::default).insert(key, v.clone());
    v
}

/// Read PlantUML's `-syntax` output. Pure, for testing.
pub fn verdict(code: Option<i32>, stdout: &str, stderr: &str) -> Result<Option<String>, String> {
    let lines: Vec<&str> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(i) = lines.iter().position(|l| *l == "ERROR") {
        return Ok(Some(format!("PlantUML: {} (line {})", lines.get(i + 2).unwrap_or(&"syntax error"), lines.get(i + 1).unwrap_or(&"?"))));
    }
    if code == Some(0) && !lines.is_empty() {
        return Ok(None);
    }
    let tail: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Picked up") && !l.starts_with("at ")).collect();
    Err(format!("PlantUML exited with {} and no verdict{}", code.map(|c| c.to_string()).unwrap_or_else(|| "a signal".into()),
        tail.last().map(|t| format!(": {t}")).unwrap_or_default()))
}

/// The PlantUML command in force: the configured one, else `plantuml` on the PATH.
pub fn plantuml_cmd(configured: &[String]) -> Vec<String> {
    if !configured.is_empty() {
        return configured.to_vec();
    }
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("plantuml").is_file()));
    if on_path { vec!["plantuml".into()] } else { vec![] }
}

fn warn_once(msg: &str) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| crate::ui::warn(msg));
}

/// egm-dsh's own questions about a doc's diagrams: (section, text).
pub fn questions(text: &str, policy: Policy, checker: &Checker, require_exempt: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let all = blocks(text);
    let mut valid_in_plan = 0;
    for (k, b) in all.iter().enumerate() {
        let mut errs = check(&b.body);
        if errs.is_empty() && *checker != Checker::None {
            match syntax(checker, &b.body) {
                Ok(Some(e)) => errs.push(e),
                Ok(None) => {}
                // A broken tool is not a broken doc: the structure check still ran.
                Err(why) => warn_once(&format!("PlantUML could not check the diagrams ({why}); only egm-dsh's structure check ran. `egm-dsh doctor` shows the PlantUML status")),
            }
        }
        let section = if b.section.is_empty() { "Technical plan".to_string() } else { b.section.clone() };
        if errs.is_empty() {
            if section.eq_ignore_ascii_case("Technical plan") {
                valid_in_plan += 1;
            }
        } else {
            out.push((section, format!("Diagram {} (doc line {}) is malformed: {}. Fix it so a reader can follow the flow.", k + 1, b.line, errs.join("; "))));
        }
    }
    if policy == Policy::Require && !require_exempt && valid_in_plan == 0 {
        out.push(("Technical plan".into(), "The Technical plan has no valid PlantUML sequence diagram; show each interaction the design changes as a ```plantuml block (this project requires one).".into()));
    }
    out
}

/// What the elephant is asked when it drafts with diagrams on.
pub fn guidance(policy: Policy) -> Option<&'static str> {
    match policy {
        Policy::Off => None,
        _ => Some(
            "Use diagrams instead of long prose. Show every interaction or flow the design adds or changes as a PlantUML sequence diagram in a fenced ```plantuml block (`@startuml` ... `@enduml`): participants are the actors, components and files named in the Implementation map; messages are the calls, requests, commands and events, in order (`A -> B : what`, replies `B --> A : what`); show error and edge-case paths with `alt` / `else` / `end`, optional steps with `opt` ... `end`, repetition with `loop` ... `end`. No `!include` or other `!` lines, and no `--` in labels (PlantUML reads `--text--` as strikethrough). Keep prose only for what a diagram cannot show (data shapes, invariants, numbers, why), and do not restate in prose what a diagram already shows.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    const GOOD: &str = "@startuml\nactor User\nparticipant \"jot/cli.py\" as CLI\nUser -> CLI : jot add \"x\"\nactivate CLI\nalt ids have a gap\n  CLI -> CLI : next = max(ids) + 1\nelse no notes\n  CLI -> CLI : next = 1\nend\nnote over CLI\n  ids never reused\nend note\nCLI --> User : added #4\ndeactivate CLI\n@enduml";

    #[test]
    fn the_readme_diagrams_are_well_formed() {
        let readme = include_str!("../../../README.md");
        let bs = blocks(readme);
        assert!(bs.len() >= 3, "the README shows its flows as diagrams");
        for b in bs {
            assert_eq!(check(&b.body), Vec::<String>::new(), "README diagram at line {}", b.line);
        }
    }

    #[cfg(feature = "plantuml-core")]
    #[test]
    fn the_builtin_checker_agrees_on_the_basics() {
        let good = b(GOOD);
        assert_eq!(syntax(&Checker::Builtin, &good), Ok(None));
        let junk = b("@startuml\nA -> B : x\nthis is not plantuml at all\n@enduml");
        assert!(check(&junk).is_empty(), "the structure check alone misses it");
        assert!(matches!(syntax(&Checker::Builtin, &junk), Ok(Some(e)) if e.starts_with("PlantUML:")));
        // the worker is reused: a second call is fast
        let t = std::time::Instant::now();
        for _ in 0..20 {
            assert_eq!(syntax(&Checker::Builtin, &good), Ok(None));
        }
        assert!(t.elapsed().as_millis() < 2000, "{:?}", t.elapsed());
        let q = questions(&format!("## Technical plan\n\n```plantuml\n{}\n```\n", junk.join("\n")), Policy::Prefer, &Checker::Builtin, false);
        assert!(q.len() == 1 && q[0].1.contains("PlantUML:"), "{q:?}");
    }

    #[test]
    fn checker_choice() {
        assert_eq!(checker(false, &["x".into()]), Checker::None);
        assert_eq!(checker(true, &["java".into(), "-jar".into(), "p.jar".into()]), Checker::External(vec!["java".into(), "-jar".into(), "p.jar".into()]));
        if cfg!(feature = "plantuml-core") {
            assert_eq!(checker(true, &[]), Checker::Builtin);
        }
    }

    #[test]
    fn plantuml_output_is_read_strictly() {
        assert_eq!(verdict(Some(0), "SEQUENCE\n(2 participants)\n", ""), Ok(None));
        assert_eq!(verdict(Some(200), "ERROR\n4\nSyntax Error?\n", ""), Ok(Some("PlantUML: Syntax Error? (line 4)".into())));
        assert_eq!(verdict(Some(200), "some warning\nERROR\n2\nCannot create group\n", ""), Ok(Some("PlantUML: Cannot create group (line 2)".into())));
        // a crashed or silent Java is never a pass
        let e = verdict(Some(1), "", "Picked up JAVA_TOOL_OPTIONS: x\nException in thread \"main\" java.lang.NoClassDefFoundError: sun/awt/X\n\tat a.b(c)").unwrap_err();
        assert!(e.contains("exited with 1") && e.contains("NoClassDefFoundError"), "{e}");
        assert!(verdict(Some(0), "", "").is_err(), "exit 0 with no output is no verdict");
        assert!(verdict(None, "", "").is_err());
    }

    #[test]
    fn a_well_formed_diagram_passes() {
        assert_eq!(check(&b(GOOD)), Vec::<String>::new());
        assert!(check(&b("@startuml\n[-> A : start\nA ->] : out\n@enduml")).is_empty(), "lost/found messages");
        assert!(check(&b("@startuml\nloop 3 times\nA -> B : x\nend\npar\nA -> B : y\nelse\nA -> C : z\nend\n@enduml")).is_empty());
    }

    #[test]
    fn structural_errors_are_named() {
        let e = check(&b("@startuml\nA -> B : x\nalt ok\nB --> A : y\n@enduml"));
        assert!(e.iter().any(|x| x.contains("`alt` opened on line 3 is never closed")), "{e:?}");
        assert!(check(&b("@startuml\nA -> B : x\nend\n@enduml"))[0].contains("`end` without"));
        assert!(check(&b("@startuml\nopt x\nA -> B : x\nelse\nend\n@enduml"))[0].contains("`else` outside"));
        assert!(check(&b("@startuml\nA -> B : x\ndeactivate B\n@enduml"))[0].contains("without `activate B`"));
        assert!(check(&b("@startuml\n!include /etc/passwd\nA -> B : x\n@enduml"))[0].contains("preprocessor"));
        assert!(check(&b("A -> B : x")).iter().any(|x| x.contains("@startuml")));
        assert!(check(&b("@startuml\nactor A\n@enduml"))[0].contains("no messages"));
        assert!(check(&b("@startuml\nA -> : x\n@enduml"))[0].contains("missing a participant"));
        assert!(check(&b("@startuml\nA -> B : x\nnote left of A\nunclosed\n@enduml")).iter().any(|x| x.contains("`note`")));
    }

    #[test]
    fn blocks_know_their_section_and_words_skip_them() {
        let doc = format!("# D\n\n## Problem\n\nWords here.\n\n## Technical plan\n\nShort.\n\n```plantuml\n{GOOD}\n```\n\n```python\nprint('## not a heading')\n```\n");
        let bs = blocks(&doc);
        assert_eq!(bs.len(), 1);
        assert_eq!(bs[0].section, "Technical plan");
        assert_eq!(bs[0].line, 11);
        let prose = without_diagrams(&doc);
        assert!(!prose.contains("@startuml") && prose.contains("print('## not a heading')") && prose.contains("Short."));
    }

    #[test]
    fn policy_require_needs_a_valid_plan_diagram() {
        let none = "## Technical plan\n\nProse only.\n";
        assert!(questions(none, Policy::Prefer, &Checker::None, false).is_empty());
        assert_eq!(questions(none, Policy::Require, &Checker::None, false).len(), 1);
        assert!(questions(none, Policy::Require, &Checker::None, true).is_empty(), "the fix lane is exempt");
        let good = format!("## Technical plan\n\n```plantuml\n{GOOD}\n```\n");
        assert!(questions(&good, Policy::Require, &Checker::None, false).is_empty());
        let bad = "## Risks and mitigations\n\n```puml\n@startuml\nalt x\nA -> B : y\n@enduml\n```\n";
        let q = questions(bad, Policy::Off, &Checker::None, false);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].0, "Risks and mitigations");
        assert!(q[0].1.contains("Diagram 1 (doc line 3) is malformed"));
    }
}
