//! Acceptance criteria as named requirements: an ID, a risk tier and the code each one
//! covers, written inline in the design doc:
//!
//! ```text
//! - [AC1] Tag filters return only tagged notes. Check: `tests/test_tags.py::t` Risk: high Scope: `jot/cli.py::filter_notes`, `jot/store.py`
//! ```
//!
//! All three are optional, so docs without them work as before. With them, the evidence
//! gate attributes each surviving seeded bug and each uncovered changed line to the
//! criteria whose scope holds it:
//!
//! * a seeded bug that survives in the scope of a **high**-risk criterion fails the gate;
//! * one in a **medium** or **low**-risk scope raises the risk tier and names the
//!   criterion, and never blocks on its own.
//!
//! egm-dsh checks the form itself (`problems`): duplicate IDs, an unknown risk word, a
//! risk without a scope, a scope file outside the Implementation map. Each becomes an
//! egm-dsh question no model can drop. Lean: `EgmEvidence.criteria_ok` and its theorems.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::doc::Criterion;
use crate::evidence::Mutant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn parse(word: &str) -> Option<Risk> {
        match word.trim().trim_end_matches(['.', ',', ';']).to_ascii_lowercase().as_str() {
            "low" => Some(Risk::Low),
            "medium" => Some(Risk::Medium),
            "high" => Some(Risk::High),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

/// One `Scope:` entry: a file, or a named function, class or type in it (`file::name`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeItem {
    pub file: String,
    pub symbol: Option<String>,
}

/// What a criterion's text says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub id: Option<String>,
    pub risk: Option<Risk>,
    /// The word after `Risk:` when it is not low, medium or high.
    pub bad_risk: Option<String>,
    pub scope: Vec<ScopeItem>,
}

/// Every backticked span in `text`, in order.
fn backticked(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find('`') {
        let Some(b) = rest[a + 1..].find('`') else { break };
        out.push(rest[a + 1..a + 1 + b].trim().to_string());
        rest = &rest[a + 2 + b..];
    }
    out
}

/// The text after `label` up to the next field label, or the end.
fn field<'a>(item: &'a str, label: &str) -> Option<&'a str> {
    let i = item.find(label)? + label.len();
    let rest = &item[i..];
    let end = ["Check:", "Risk:", "Scope:"].iter().filter_map(|l| rest.find(l)).min().unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// Read the ID, risk and scope from one Acceptance-criteria item.
pub fn parse_meta(item: &str) -> Meta {
    let t = item.trim();
    let id = t.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(id, _)| id.trim().to_string()).filter(|id| {
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    });
    let (risk, bad_risk) = match field(t, "Risk:") {
        None => (None, None),
        Some(w) => {
            let word = w.split_whitespace().next().unwrap_or("");
            match Risk::parse(word) {
                Some(r) => (Some(r), None),
                None => (None, Some(word.to_string())),
            }
        }
    };
    let scope = field(t, "Scope:")
        .map(|s| {
            backticked(s)
                .into_iter()
                .filter(|x| !x.is_empty())
                .map(|x| match x.split_once("::") {
                    Some((f, sym)) => ScopeItem { file: f.trim().to_string(), symbol: Some(sym.trim().to_string()) },
                    None => ScopeItem { file: x, symbol: None },
                })
                .collect()
        })
        .unwrap_or_default();
    Meta { id, risk, bad_risk, scope }
}

/// egm-dsh's own questions about the criteria's form. `map` is the Implementation map.
pub fn problems(criteria: &[Criterion], map: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for c in criteria {
        let m = parse_meta(&c.text);
        let name = m.id.clone().unwrap_or_else(|| short(&c.text));
        if let Some(id) = &m.id {
            *seen.entry(id.clone()).or_default() += 1;
        }
        if let Some(w) = &m.bad_risk {
            out.push(format!("Criterion {name}: `Risk: {w}` is not a risk tier. Use low, medium or high."));
        }
        if m.risk.is_some() && m.scope.is_empty() {
            out.push(format!("Criterion {name} has a risk tier but no `Scope:`. Name the code it covers, e.g. Scope: `path/file.py::function`."));
        }
        for s in &m.scope {
            if !map.iter().any(|f| f == &s.file) {
                out.push(format!("Criterion {name}: scope file `{}` is not in the Implementation map.", s.file));
            }
        }
    }
    for (id, n) in seen {
        if n > 1 {
            out.push(format!("The criterion ID [{id}] is used {n} times. Each criterion needs its own ID."));
        }
    }
    out
}

fn short(text: &str) -> String {
    let t: String = text.chars().take(48).collect();
    format!("\"{}{}\"", t.trim(), if text.chars().count() > 48 { "…" } else { "" })
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The 1-based, inclusive line range of a definition named `symbol` in `text`: a line
/// that starts `fn`, `def`, `class`, `struct`, `enum`, `trait`, `function` or `impl`
/// with that name (after modifiers such as `pub` or `async`), through the last line
/// indented more deeply, plus a closing `}` or `)` at its own indent. `None` if absent.
pub fn symbol_lines(text: &str, symbol: &str) -> Option<(usize, usize)> {
    const KEYWORDS: [&str; 8] = ["fn", "def", "class", "struct", "enum", "trait", "function", "impl"];
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| {
        let words: Vec<&str> = l.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
        words.windows(2).any(|w| KEYWORDS.contains(&w[0]) && w[1] == symbol)
            && !l.trim_start().starts_with("//")
            && !l.trim_start().starts_with('#')
    })?;
    let base = indent(lines[start]);
    let mut end = start;
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        if l.trim().is_empty() {
            continue;
        }
        if indent(l) > base {
            end = i;
            continue;
        }
        let t = l.trim_start();
        if indent(l) == base && (t.starts_with('}') || t.starts_with(')')) {
            end = i;
        }
        break;
    }
    Some((start + 1, end + 1))
}

/// Is `line` of `file` inside one of `scope`'s entries? `texts` holds file contents.
pub fn in_scope(file: &str, line: usize, scope: &[ScopeItem], texts: &BTreeMap<String, String>) -> bool {
    scope.iter().any(|s| {
        s.file == file
            && match &s.symbol {
                None => true,
                Some(sym) => texts.get(file).and_then(|t| symbol_lines(t, sym)).is_some_and(|(a, b)| a <= line && line <= b),
            }
    })
}

/// What the gate found for one criterion that has an ID, a risk or a scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionResult {
    pub id: String,
    pub text: String,
    pub risk: Option<Risk>,
    /// Seeded bugs that survived inside this criterion's scope.
    pub survivors: Vec<Mutant>,
    /// Changed lines in its scope that no test ran: file → lines.
    pub uncovered: BTreeMap<String, Vec<usize>>,
}

/// Attribute survivors and uncovered changed lines to criteria. Returns one result per
/// criterion with a scope, and the survivors outside every scope.
pub fn attribute(
    criteria: &[Criterion],
    survivors: &[Mutant],
    uncovered: &BTreeMap<String, Vec<usize>>,
    texts: &BTreeMap<String, String>,
) -> (Vec<CriterionResult>, Vec<Mutant>) {
    let mut results = Vec::new();
    let mut claimed = vec![false; survivors.len()];
    for (n, c) in criteria.iter().enumerate() {
        let m = parse_meta(&c.text);
        if m.scope.is_empty() {
            continue;
        }
        let mut r = CriterionResult {
            id: m.id.clone().unwrap_or_else(|| format!("#{}", n + 1)),
            text: c.text.clone(),
            risk: m.risk,
            survivors: vec![],
            uncovered: BTreeMap::new(),
        };
        for (i, s) in survivors.iter().enumerate() {
            if in_scope(&s.file, s.line, &m.scope, texts) {
                r.survivors.push(s.clone());
                claimed[i] = true;
            }
        }
        for (f, lines) in uncovered {
            let mine: Vec<usize> = lines.iter().copied().filter(|&l| in_scope(f, l, &m.scope, texts)).collect();
            if !mine.is_empty() {
                r.uncovered.insert(f.clone(), mine);
            }
        }
        results.push(r);
    }
    let unscoped = survivors.iter().zip(claimed).filter(|(_, c)| !c).map(|(s, _)| s.clone()).collect();
    (results, unscoped)
}

/// `EgmEvidence.blocks`: a criterion blocks the gate only when it is high risk and a
/// seeded bug survived in its scope. Medium and low never block.
pub fn blocks(risk: Option<Risk>, survivors: usize) -> bool {
    risk == Some(Risk::High) && survivors > 0
}

/// `EgmEvidence.criteria_ok`: no criterion blocks.
pub fn criteria_ok(results: &[CriterionResult]) -> bool {
    results.iter().all(|r| !blocks(r.risk, r.survivors.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crit(text: &str) -> Criterion {
        Criterion { text: text.into(), check: None }
    }

    fn mutant(file: &str, line: usize) -> Mutant {
        Mutant { file: file.into(), line, op: "== → !=".into(), mutated_line: String::new() }
    }

    #[test]
    fn meta_is_read_from_the_item_and_is_optional() {
        let m = parse_meta("[AC1] Filters work. Check: `tests/t.py::T::f` Risk: high Scope: `jot/cli.py::filter_notes`, `jot/store.py`");
        assert_eq!(m.id.as_deref(), Some("AC1"));
        assert_eq!(m.risk, Some(Risk::High));
        assert_eq!(m.scope, vec![
            ScopeItem { file: "jot/cli.py".into(), symbol: Some("filter_notes".into()) },
            ScopeItem { file: "jot/store.py".into(), symbol: None },
        ]);
        assert_eq!(parse_meta("Plain criterion. Check: `tests/t.py`"), Meta::default());
        // Scope before Risk, and a full stop after the risk word
        let m = parse_meta("[x-2] y. Scope: `a.rs` Risk: Medium. Check: `tests/a.rs`");
        assert_eq!((m.id.as_deref(), m.risk, m.scope.len()), (Some("x-2"), Some(Risk::Medium), 1));
        // a bracket that is not an ID
        assert_eq!(parse_meta("[see the RFC 1] text").id, None);
    }

    #[test]
    fn form_problems_become_questions() {
        let map = vec!["jot/cli.py".to_string()];
        let cs = vec![
            crit("[A] one. Check: `t.py` Risk: high Scope: `jot/cli.py`"),
            crit("[A] two. Check: `t.py` Risk: severe Scope: `jot/cli.py`"),
            crit("[B] three. Check: `t.py` Risk: medium"),
            crit("[C] four. Check: `t.py` Risk: low Scope: `jot/other.py`"),
            crit("five, no metadata. Check: `t.py`"),
        ];
        let p = problems(&cs, &map);
        assert!(p.iter().any(|q| q.contains("[A] is used 2 times")), "{p:?}");
        assert!(p.iter().any(|q| q.contains("`Risk: severe` is not a risk tier")), "{p:?}");
        assert!(p.iter().any(|q| q.contains("Criterion B has a risk tier but no `Scope:`")), "{p:?}");
        assert!(p.iter().any(|q| q.contains("`jot/other.py` is not in the Implementation map")), "{p:?}");
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(problems(&[crit("plain. Check: `t.py`")], &map).is_empty());
    }

    #[test]
    fn symbol_ranges_for_python_and_rust() {
        let py = "import os\n\ndef add(a, b):\n    if a:\n        return a + b\n\n    return b\n\ndef other():\n    pass\n";
        assert_eq!(symbol_lines(py, "add"), Some((3, 7)));
        assert_eq!(symbol_lines(py, "other"), Some((9, 10)));
        let rs = "use x;\n\npub fn negated(s: &str) -> bool {\n    let a = 1;\n    a == 1\n}\n\nfn next() {}\n";
        assert_eq!(symbol_lines(rs, "negated"), Some((3, 6)));
        assert_eq!(symbol_lines(rs, "next"), Some((8, 8)));
        let rs_impl = "impl Foo {\n    fn bar(&self) {\n        x();\n    }\n}\n";
        assert_eq!(symbol_lines(rs_impl, "bar"), Some((2, 4)));
        assert_eq!(symbol_lines(py, "missing"), None);
        assert_eq!(symbol_lines("# def add(x):\ndef add2():\n  pass\n", "add"), None, "comments and longer names do not match");
    }

    #[test]
    fn survivors_and_uncovered_lines_are_attributed_by_scope() {
        let py = "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n";
        let texts: BTreeMap<String, String> = [("m.py".to_string(), py.to_string())].into();
        let cs = vec![
            crit("[ADD] adds. Check: `t.py` Risk: high Scope: `m.py::add`"),
            crit("[SUB] subtracts. Check: `t.py` Risk: medium Scope: `m.py::sub`"),
            crit("[ALL] whole file. Check: `t.py` Risk: low Scope: `m.py`"),
            crit("unscoped. Check: `t.py`"),
        ];
        let survivors = vec![mutant("m.py", 2), mutant("m.py", 5), mutant("other.py", 1)];
        let uncovered: BTreeMap<String, Vec<usize>> = [("m.py".to_string(), vec![5])].into();
        let (rs, unscoped) = attribute(&cs, &survivors, &uncovered, &texts);
        assert_eq!(rs.len(), 3, "only criteria with a scope get a result");
        assert_eq!(rs[0].survivors, vec![mutant("m.py", 2)]);
        assert_eq!(rs[1].survivors, vec![mutant("m.py", 5)]);
        assert_eq!(rs[2].survivors.len(), 2, "a file scope holds every line of it");
        assert_eq!(rs[1].uncovered.get("m.py"), Some(&vec![5]));
        assert!(rs[0].uncovered.is_empty());
        assert_eq!(unscoped, vec![mutant("other.py", 1)]);
        assert!(!criteria_ok(&rs), "a survivor in a high-risk scope blocks");
        let (rs, _) = attribute(&cs[1..], &survivors, &uncovered, &texts);
        assert!(criteria_ok(&rs), "medium and low survivors never block");
    }

    #[test]
    fn blocking_rule_matches_lean() {
        // EgmEvidence.blocks: high risk and at least one survivor
        for risk in [None, Some(Risk::Low), Some(Risk::Medium), Some(Risk::High)] {
            for n in 0..4 {
                assert_eq!(blocks(risk, n), risk == Some(Risk::High) && n > 0, "{risk:?} {n}");
            }
        }
    }
}
