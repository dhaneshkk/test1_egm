//! Design documents: the elephant's output and the goldfish's only input.

use std::path::{Path, PathBuf};

/// Sections every design doc carries, in order. The elephant drafts them one
/// at a time; the goldfish reports any that are missing or empty.
/// Goals and Risks follow the upstream EGM Feature Specification Standard.
pub const SECTIONS: [&str; 7] = [
    "Problem",
    "Goals and non-goals",
    "Technical plan",
    "Alternatives considered",
    "Risks and mitigations",
    "Implementation map",
    "Acceptance criteria",
];

/// Headings the upstream EGM `egm-draft` template (`references/design_doc.md`)
/// uses for the same content, so an adopted doc is read as written.
pub fn aliases(section: &str) -> &'static [&'static str] {
    match section {
        "Problem" => &["The Problem", "Problem Statement"],
        "Goals and non-goals" => &["Goals & Non-Goals", "Goals"],
        "Technical plan" => &["Technical Plan (Architecture)", "Proposed Architecture"],
        "Risks and mitigations" => &["Risks & Mitigations", "Risks"],
        "Implementation map" => &["Detailed Implementation Plan", "Detailed Implementation"],
        _ => &[],
    }
}

/// Sections a doc may add; never reported missing.
pub const OPTIONAL_SECTIONS: [&str; 3] = ["Examples", "Decisions", "Verification"];

/// What the elephant is asked for in each section.
pub fn guidance(section: &str) -> &'static str {
    match section {
        "Problem" => "Three to five plain sentences a newcomer understands: who is affected and what they cannot do today. Describe no solution.",
        "Goals and non-goals" => "Two short lists: what this change will deliver, and what it deliberately will not (so nobody builds it by accident).",
        "Risks and mitigations" => "Each risk in bold (failure modes, security, data loss, scale limits), followed by how the design mitigates it. Say plainly when a risk is accepted rather than mitigated.",
        "Technical plan" => "A jargon-light explanation of the components involved and how they fit together. Say how existing data, files and callers are affected by the change. Leave no design decision to the implementer: name data shapes, error handling and edge-case behavior.",
        "Alternatives considered" => "Every approach we discussed and rejected, one paragraph each, starting with the approach in bold and saying why it lost. A spec that proposes one path with no documented rejected options fails review.",
        "Implementation map" => "A bullet list of EVERY file to create or change, each formatted exactly as: - `relative/path` — reason. Paths are relative to the repository root. If the change needs more than three files, group the bullets under `### Phase 1: name`, `### Phase 2: name`, ..., where each phase leaves the code building and its tests passing.",
        "Acceptance criteria" => "A bullet list. Each item states one observable behavior, then ends with `Check: ` and, in backticks, the one test that fails if the behavior breaks, as `path/to/test_file.py::TestClass::test_name` (or `path::test_name`). For an algorithmic property, use `Check: lean proofs/Name.lean#theorem_name`. Use `Check: manual — reason` only when nothing can be automated. Optionally, start an item with an ID in brackets (`[AC1]`) and end it with `Risk: low|medium|high` and `Scope:` plus the backticked code it covers (`path/file.py` or `path/file.py::function`); a seeded bug that survives in the scope of a high-risk criterion fails the evidence gate.",
        "Verification" => "Required when the project sets a Verification checklist (`egm-dsh set-verification`): define every item on it, with concrete values. egm-dsh checks each item itself.",
        "Decisions" => "Written by `egm-dsh redraft`: each question a goldfish raised and how it was settled, by the repository (with the file and its exact words) or by a person. Keep earlier entries.",
        "Examples" => "Optional. One or more ```console blocks: lines starting with `$ ` are commands run from the repository root, and the lines after each are its exact expected output. They run as tests, so show only output the finished feature will really print.",
        _ => "",
    }
}

pub fn template(title: &str, idea: &str) -> String {
    let mut s = format!("# Design: {title}\n\nStatus: draft\n\n> Idea: {idea}\n");
    for name in SECTIONS {
        s.push_str(&format!("\n## {name}\n\n_Not drafted yet._\n"));
    }
    s
}

/// 64-bit FNV-1a. Stable across platforms and Rust versions, unlike `DefaultHasher`.
pub fn hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[derive(Debug, Clone, PartialEq)]
pub struct Criterion {
    pub text: String,
    /// The check named after `Check:`; `None` when the item names none.
    pub check: Option<String>,
}

/// One build phase: a name and the files it may change.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildPhase {
    pub name: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub path: PathBuf,
    pub text: String,
}

impl Doc {
    pub fn load(path: &Path) -> anyhow::Result<Doc> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read design doc {}: {e}", path.display()))?;
        Ok(Doc { path: path.to_path_buf(), text })
    }

    pub fn hash(&self) -> u64 {
        hash(&self.text)
    }

    /// Body of `## name` (or an upstream alias of it), trimmed, or `None`
    /// when no such heading is present.
    pub fn section(&self, name: &str) -> Option<String> {
        if let Some(body) = section_of(&self.text, name) {
            return Some(body);
        }
        let body = aliases(name).iter().find_map(|a| section_of(&self.text, a))?;
        // The upstream template splits goals and non-goals into two headings.
        match (name, section_of(&self.text, "Non-Goals")) {
            ("Goals and non-goals", Some(non)) if !body.contains(&non) => Some(format!("{body}\n\nNon-goals:\n{non}")),
            _ => Some(body),
        }
    }

    /// Sections that are absent, empty, or still the template placeholder.
    pub fn missing_sections(&self) -> Vec<&'static str> {
        SECTIONS
            .into_iter()
            .filter(|s| match self.section(s) {
                None => true,
                Some(body) => body.is_empty() || body.contains("_Not drafted yet._"),
            })
            .collect()
    }

    /// Paths listed in the Implementation map: the first backticked token per
    /// bullet, or per numbered file heading in the upstream template
    /// (`#### 1. `path` (Modify)`).
    pub fn implementation_map(&self) -> Vec<String> {
        let Some(body) = self.section("Implementation map") else { return vec![] };
        let mut out: Vec<String> = Vec::new();
        for path in body.lines().filter_map(map_item) {
            if !out.contains(&path) {
                out.push(path);
            }
        }
        out
    }

    /// The Implementation map split into build phases. A phase starts at a
    /// `### Phase N: name` heading inside the section; files listed before any
    /// phase heading, or a map with no phase headings, form one phase.
    pub fn implementation_phases(&self) -> Vec<BuildPhase> {
        let Some(body) = self.section("Implementation map") else { return vec![] };
        let mut phases: Vec<BuildPhase> = Vec::new();
        let mut current = BuildPhase { name: "all files".into(), files: vec![] };
        for line in body.lines().map(str::trim_start) {
            if let Some(h) = line.strip_prefix("### ") {
                let title = h.trim();
                if title.to_lowercase().starts_with("phase") {
                    if !current.files.is_empty() {
                        phases.push(current);
                    }
                    let name = title.split_once([':', '—', '-']).map(|(_, n)| n.trim()).filter(|n| !n.is_empty()).unwrap_or(title);
                    current = BuildPhase { name: name.to_string(), files: vec![] };
                }
                continue;
            }
            if let Some(path) = map_item(line)
                && !current.files.contains(&path)
            {
                current.files.push(path);
            }
        }
        if !current.files.is_empty() {
            phases.push(current);
        }
        phases
    }

    pub fn criteria(&self) -> Vec<Criterion> {
        let Some(body) = self.section("Acceptance criteria") else { return vec![] };
        body.lines()
            .filter_map(|l| l.trim_start().strip_prefix("- "))
            .map(|item| {
                let check = item.find("Check:").map(|i| {
                    // the check ends where the optional `Risk:` or `Scope:` fields begin
                    let rest = &item[i + 6..];
                    let end = ["Risk:", "Scope:"].iter().filter_map(|l| rest.find(l)).min().unwrap_or(rest.len());
                    let rest = rest[..end].trim();
                    first_backticked(rest).unwrap_or_else(|| rest.to_string())
                });
                Criterion { text: item.to_string(), check }
            })
            .collect()
    }

    /// Files a goldfish may read: relative Markdown links plus Implementation
    /// map entries that already exist. Nothing else.
    pub fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        let base = self.path.parent().unwrap_or(root);
        let mut out: Vec<PathBuf> = Vec::new();
        // egm-dsh's own state (reviews, decisions, sessions) is never goldfish context,
        // even if the doc links it: a goldfish knows only the doc and the repository.
        let state = root.join(".egm-dsh").canonicalize().ok();
        let mut push = |p: PathBuf| {
            let is_state = state.as_ref().is_some_and(|st| p.canonicalize().is_ok_and(|c| c.starts_with(st)));
            if p.is_file() && !is_state && !out.contains(&p) {
                out.push(p);
            }
        };
        let mut rest = self.text.as_str();
        while let Some(i) = rest.find("](") {
            rest = &rest[i + 2..];
            if let Some(end) = rest.find(')') {
                let target = rest[..end].split('#').next().unwrap_or("");
                if !target.is_empty() && !target.contains("://") {
                    push(base.join(target));
                }
            }
        }
        for p in self.implementation_map() {
            push(root.join(p));
        }
        out
    }

    /// Replace (or append) `## name` with `new_section`, which must start with that heading.
    /// An upstream alias heading is replaced in place rather than duplicated.
    pub fn with_section(&self, name: &str, new_section: &str) -> String {
        let target = if section_of(&self.text, name).is_some() {
            name
        } else {
            aliases(name).iter().copied().find(|a| section_of(&self.text, a).is_some()).unwrap_or(name)
        };
        replace_section(&self.text, target, new_section)
    }
}

/// The file path an Implementation-map line names: a `- `path` — reason`
/// bullet, or an upstream `#### 1. `path` (Modify)` heading.
fn map_item(line: &str) -> Option<String> {
    let line = line.trim_start();
    let item = line.strip_prefix("- ").or_else(|| {
        let h = line.strip_prefix("#### ")?;
        let (num, rest) = h.split_once(". ")?;
        num.chars().all(|c| c.is_ascii_digit()).then_some(rest)
    });
    item.and_then(first_backticked)
}

fn first_backticked(s: &str) -> Option<String> {
    let start = s.find('`')? + 1;
    let len = s[start..].find('`')?;
    let token = s[start..start + len].trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn heading_range(text: &str, name: &str) -> Option<(usize, usize, usize)> {
    let heading = format!("## {name}");
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if let Some((s, body)) = start {
            if trimmed.starts_with("## ") {
                return Some((s, body, offset));
            }
        } else if trimmed.eq_ignore_ascii_case(&heading) {
            start = Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    start.map(|(s, b)| (s, b, text.len()))
}

pub fn section_of(text: &str, name: &str) -> Option<String> {
    let (_, body, end) = heading_range(text, name)?;
    Some(text[body..end].trim().to_string())
}

pub fn replace_section(text: &str, name: &str, new_section: &str) -> String {
    let block = format!("{}\n\n", new_section.trim());
    match heading_range(text, name) {
        Some((s, _, e)) => format!("{}{}{}", &text[..s], block, &text[e..]),
        None => format!("{}\n\n{block}", text.trim_end()),
    }
}

/// Pull the `## name` section out of an agent reply, tolerating chatter and
/// code fences around it. Falls back to wrapping the whole reply.
/// Split a reply holding several `## Section` blocks. A `## ` line inside a fenced
/// code block is not a heading, and one fence wrapping the whole reply is removed.
/// Returns each wanted name's section (heading included), or `None` if the reply
/// lacks it or it is empty.
pub fn extract_sections(reply: &str, names: &[&str]) -> Vec<Option<String>> {
    let mut lines: Vec<&str> = reply.lines().collect();
    let first = lines.iter().position(|l| !l.trim().is_empty());
    let last = lines.iter().rposition(|l| !l.trim().is_empty());
    if let (Some(f), Some(l)) = (first, last)
        && f < l
        && lines[f].trim_start().starts_with("```")
        && lines[l].trim() == "```"
    {
        lines = lines[f + 1..l].to_vec();
    }
    let mut found: Vec<(String, Vec<&str>)> = Vec::new();
    // A ```markdown (or ```md) fence only wraps the doc, so it is see-through; any
    // other fence is code, and a `## ` line inside it is content.
    let (mut in_code, mut in_md) = (false, false);
    // Chatter after a closed ```markdown wrapper ("Anything else?") is not doc text.
    let mut after_md = false;
    for line in lines {
        let t = line.trim_start();
        if let Some(info) = t.strip_prefix("```") {
            let info = info.trim();
            if in_code {
                in_code = !info.is_empty();
            } else if info.eq_ignore_ascii_case("markdown") || info.eq_ignore_ascii_case("md") {
                in_md = true;
                continue;
            } else if info.is_empty() && in_md {
                in_md = false;
                after_md = true;
                continue;
            } else {
                in_code = true;
            }
            if let Some((_, body)) = found.last_mut() {
                body.push(line);
            }
            continue;
        }
        if !in_code && let Some(h) = t.strip_prefix("## ") {
            found.push((h.trim().to_string(), Vec::new()));
            after_md = false;
            continue;
        }
        if after_md {
            continue;
        }
        if let Some((_, body)) = found.last_mut() {
            body.push(line);
        }
    }
    names
        .iter()
        .map(|n| {
            found.iter().find(|(h, _)| h.eq_ignore_ascii_case(n)).and_then(|(_, body)| {
                let body = body.join("\n");
                (!body.trim().is_empty()).then(|| format!("## {n}\n\n{}", body.trim()))
            })
        })
        .collect()
}

/// One elephant turn for several sections (`draft`, and redrafts of more than one).
pub fn draft_all_prompt(names: &[&str], lead: &str) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("## {n}\n{}", guidance(n))).collect();
    format!(
        "{lead}\n\nWrite these sections, in this order, each starting with its heading line exactly as given (what each must contain is under its heading):\n\n{}\n\nReply with only those sections in Markdown, nothing before or after. Do not create or edit files; I will save them.",
        list.join("\n\n")
    )
}

pub fn extract_section(reply: &str, name: &str) -> String {
    // Same rules as `extract_sections`: code fences (diagrams, examples) are kept.
    if let Some(Some(sec)) = extract_sections(reply, &[name]).into_iter().next() {
        return sec;
    }
    let heading = format!("## {name}");
    let cleaned: Vec<&str> = reply
        .lines()
        .filter(|l| !matches!(l.trim().to_ascii_lowercase().as_str(), "```markdown" | "```md"))
        .filter(|l| !l.trim().eq_ignore_ascii_case(&heading))
        .collect();
    format!("{heading}\n\n{}", cleaned.join("\n").trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Design: x\n\n## Problem\n\nUsers suffer.\n\n## Implementation map\n\n- `jot/cli.py` — add flag\n- `tests/test_tags.py` — new tests\n\n## Acceptance criteria\n\n- Tags stored. Check: `tests/test_tags.py`\n- Looks nice. Check: manual — visual\n- No check here\n";

    fn doc() -> Doc {
        Doc { path: PathBuf::from("docs/designs/x.md"), text: SAMPLE.into() }
    }

    #[test]
    fn parses_map_and_criteria() {
        let d = doc();
        assert_eq!(d.implementation_map(), vec!["jot/cli.py", "tests/test_tags.py"]);
        let c = d.criteria();
        assert_eq!(c[0].check.as_deref(), Some("tests/test_tags.py"));
        assert_eq!(c[1].check.as_deref(), Some("manual — visual"));
        assert_eq!(c[2].check, None);
        assert_eq!(d.missing_sections(), vec!["Goals and non-goals", "Technical plan", "Alternatives considered", "Risks and mitigations"]);
    }

    #[test]
    fn splits_the_map_into_phases() {
        let text = "## Implementation map\n\n### Phase 1: storage\n\n- `jot/cli.py` — tags field\n\n### Phase 2 — tests\n\n- `tests/test_tags.py` — new\n- `jot/cli.py` — render tags\n\n## Acceptance criteria\n\n- x. Check: `tests/test_tags.py`\n";
        let d = Doc { path: PathBuf::from("d.md"), text: text.into() };
        let ph = d.implementation_phases();
        assert_eq!(ph.len(), 2);
        assert_eq!((ph[0].name.as_str(), ph[0].files.clone()), ("storage", vec!["jot/cli.py".to_string()]));
        assert_eq!((ph[1].name.as_str(), ph[1].files.clone()), ("tests", vec!["tests/test_tags.py".to_string(), "jot/cli.py".to_string()]));
        assert_eq!(d.implementation_map(), vec!["jot/cli.py", "tests/test_tags.py"]);
        // no phase headings: one phase with every file
        assert_eq!(doc().implementation_phases(), vec![BuildPhase { name: "all files".into(), files: vec!["jot/cli.py".into(), "tests/test_tags.py".into()] }]);
    }

    #[test]
    fn reads_upstream_egm_draft_headings() {
        let text = "# X\n\n## The Problem\n\nUsers suffer.\n\n## Goals\n\n- tag\n\n## Non-Goals\n\n- rename\n\n## Technical Plan (Architecture)\n\nPlan.\n\n## Alternatives Considered\n\n### Alternative A: index\n\nRejected.\n\n## Detailed Implementation Plan\n\n### Target Files to Create/Modify\n\n#### 1. `jot/cli.py` (Modify)\n\nAdd tags.\n\n#### 2. `tests/test_tags.py` (Create)\n\nTests.\n";
        let d = Doc { path: PathBuf::from("docs/designs/x.md"), text: text.into() };
        assert_eq!(d.section("Problem").as_deref(), Some("Users suffer."));
        assert_eq!(d.section("Goals and non-goals").as_deref(), Some("- tag\n\nNon-goals:\n- rename"));
        assert!(d.section("Alternatives considered").unwrap().contains("Rejected."));
        assert_eq!(d.implementation_map(), vec!["jot/cli.py", "tests/test_tags.py"]);
        assert_eq!(d.missing_sections(), vec!["Risks and mitigations", "Acceptance criteria"]);
    }

    #[test]
    fn replaces_sections_in_place_and_appends_new_ones() {
        let t = replace_section(SAMPLE, "Problem", "## Problem\n\nNew text.");
        assert!(t.contains("New text.") && !t.contains("Users suffer."));
        assert!(t.find("## Problem").unwrap() < t.find("## Implementation map").unwrap());
        let t = replace_section(&t, "Technical plan", "## Technical plan\n\nPlan.");
        assert_eq!(section_of(&t, "Technical plan").as_deref(), Some("Plan."));
    }

    #[test]
    fn extracts_sections_from_chatty_replies() {
        let r = "Sure! Here it is:\n```markdown\n## Problem\n\nPain.\n```\nAnything else?";
        assert_eq!(extract_section(r, "Problem"), "## Problem\n\nPain.");
        assert_eq!(extract_section("Just text.", "Problem"), "## Problem\n\nJust text.");
        // a diagram inside the section survives, fences and all
        let r = "## Technical plan\n\nShort.\n\n```plantuml\n@startuml\nA -> B : x\n@enduml\n```\n\nAfter.";
        assert_eq!(extract_section(r, "Technical plan"), "## Technical plan\n\nShort.\n\n```plantuml\n@startuml\nA -> B : x\n@enduml\n```\n\nAfter.");
        let wrapped = format!("```markdown\n{r}\n```\nWant changes?");
        assert_eq!(extract_section(&wrapped, "Technical plan"), extract_section(r, "Technical plan"));
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash(""), 0xcbf29ce484222325);
        assert_ne!(hash("a"), hash("b"));
    }

    #[test]
    fn splits_a_one_turn_draft_into_sections() {
        let reply = "Here you go:\n```markdown\n## Problem\n\nUsers suffer.\n\n## technical plan\n\nStore tags.\n\n## Examples\n\n```console\n$ jot list\n## not a heading\n```\n\n## Risks and mitigations\n\n\n```";
        // the wrapper fence only counts when it wraps the whole reply; here prose comes first
        let got = extract_sections(reply, &["Problem", "Technical plan", "Examples", "Risks and mitigations", "Acceptance criteria"]);
        assert_eq!(got[0].as_deref(), Some("## Problem\n\nUsers suffer."));
        assert_eq!(got[1].as_deref(), Some("## Technical plan\n\nStore tags."), "heading match ignores case, keeps the canonical name");
        assert!(got[2].as_deref().unwrap().contains("## not a heading"), "a `## ` line inside a code block stays in the section");
        assert_eq!(got[3], None, "an empty section counts as missing, so it is drafted on its own");
        assert_eq!(got[4], None, "a missing section is reported, not invented");
        let wrapped = "```markdown\n## Problem\n\nA.\n\n## Goals and non-goals\n\nB.\n```";
        let got = extract_sections(wrapped, &["Problem", "Goals and non-goals"]);
        assert_eq!(got, vec![Some("## Problem\n\nA.".to_string()), Some("## Goals and non-goals\n\nB.".to_string())]);
    }
}
