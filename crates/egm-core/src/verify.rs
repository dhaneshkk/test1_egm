//! The Verification checklist: what a doc's `## Verification` section must define,
//! checked by egm-dsh itself, not by a model.
//!
//! A project chooses its items (`egm-dsh set-verification --require ...`; none by
//! default, so a CLI tweak is not asked for SAM settings). Each required item that the
//! section does not define becomes an egm-dsh question, which no model can drop, so
//! the doc cannot pass while one is missing (`EgmVerify` in `proofs/EgmGate.lean`).
//! The checks are about form, not truth: a theorem *statement* is present, an MCC
//! *threshold* is a number in [-1, 1], SAM names ρ and a base optimizer, the
//! `/chat/completions` contract names its request and response fields.

use serde::{Deserialize, Serialize};

use crate::doc::Doc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Item {
    /// At least one Lean 4 theorem statement (`theorem name ... :`).
    Lean,
    /// An MCC acceptance threshold (`MCC ≥ 0.6`) and the sample it is measured on.
    Mcc,
    /// SAM hyperparameters: ρ and the base optimizer.
    Sam,
    /// The OpenAI-compatible `/chat/completions` request and response shape.
    ChatCompletions,
}

impl Item {
    pub fn describe(self) -> &'static str {
        match self {
            Item::Lean => "the Lean 4 theorem statements (`theorem name … : …`)",
            Item::Mcc => "an MCC acceptance threshold (e.g. `MCC ≥ 0.6`) and the number of labelled cases it is measured on",
            Item::Sam => "the SAM hyperparameters: ρ (e.g. `ρ = 0.05`) and the base optimizer (SGD, Adam, AdamW, …)",
            Item::ChatCompletions => "the `/chat/completions` contract: the request (`model`, `messages`) and the response (`choices`)",
        }
    }
}

/// What the elephant is asked to write in `## Verification`, given the project's items.
pub fn guidance(items: &[Item]) -> String {
    let list: Vec<String> = items.iter().map(|i| format!("- {}", i.describe())).collect();
    format!("Define, in plain text and code blocks, exactly:\n{}\nUse concrete values, not placeholders.", list.join("\n"))
}

fn number_after(text: &str, at: usize) -> Option<f64> {
    let rest = &text[at..];
    let start = rest.find(|c: char| c.is_ascii_digit() || c == '-' || c == '.')?;
    if start > 4 {
        return None; // the number must follow the comparator closely
    }
    let num: String = rest[start..].chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    num.parse().ok()
}

fn has_lean(t: &str) -> bool {
    t.lines().any(|l| {
        let l = l.trim_start();
        let rest = l.strip_prefix("theorem ").or_else(|| l.strip_prefix("lemma "));
        rest.is_some_and(|r| {
            let name: String = r.chars().take_while(|c| !c.is_whitespace() && *c != ':' && *c != '(').collect();
            !name.is_empty() && r.contains(':')
        })
    })
}

fn has_mcc(t: &str) -> bool {
    let upper = t.to_uppercase();
    let mut from = 0;
    while let Some(i) = upper[from..].find("MCC") {
        let at = from + i + 3;
        let tail = &t[at.min(t.len())..];
        for cmp in ["≥", ">=", ">"] {
            if let Some(j) = tail.find(cmp).filter(|j| *j <= 12)
                && number_after(tail, j + cmp.len()).is_some_and(|x| (-1.0..=1.0).contains(&x))
            {
                return true;
            }
        }
        from = at;
    }
    false
}

fn has_sam(t: &str) -> bool {
    let lower = t.to_lowercase();
    let names_sam = lower.contains("sam") || lower.contains("sharpness");
    let rho = ["ρ", "rho"].iter().any(|r| {
        lower.match_indices(r).any(|(i, _)| {
            let tail = &lower[i + r.len()..];
            tail.trim_start().starts_with('=') && number_after(tail, tail.find('=').unwrap() + 1).is_some_and(|x| x > 0.0)
        })
    });
    let base = ["sgd", "adamw", "adam", "lion", "rmsprop", "adagrad"].iter().any(|o| lower.contains(o));
    names_sam && rho && base
}

fn has_chat(t: &str) -> bool {
    t.contains("/chat/completions") && t.contains("messages") && t.contains("choices")
}

/// The required items the doc's `## Verification` section does not define.
pub fn gaps(doc: &Doc, required: &[Item]) -> Vec<Item> {
    if required.is_empty() {
        return Vec::new();
    }
    let body = doc.section("Verification").unwrap_or_default();
    required
        .iter()
        .copied()
        .filter(|i| {
            !match i {
                Item::Lean => has_lean(&body),
                Item::Mcc => has_mcc(&body),
                Item::Sam => has_sam(&body),
                Item::ChatCompletions => has_chat(&body),
            }
        })
        .collect()
}

/// The questions egm-dsh raises for the gaps (`collect_alarms` adds them as egm alarms).
pub fn questions(doc: &Doc, required: &[Item]) -> Vec<String> {
    gaps(doc, required).into_iter().map(|i| format!("The `## Verification` section does not define {}.", i.describe())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn doc(verification: &str) -> Doc {
        Doc { path: PathBuf::from("d.md"), text: format!("# D\n\n## Problem\n\nx\n\n## Verification\n\n{verification}\n") }
    }

    const ALL: [Item; 4] = [Item::Lean, Item::Mcc, Item::Sam, Item::ChatCompletions];

    #[test]
    fn a_complete_section_has_no_gaps() {
        let full = "```lean\ntheorem filter_sound (ns : List Note) (t : String) : ∀ n ∈ filter ns t, t ∈ n.tags\n```\n\
            Acceptance: MCC ≥ 0.60 on 40 labelled docs.\n\
            Training uses SAM with ρ = 0.05 over AdamW (lr 3e-4).\n\
            POST /chat/completions with {\"model\": \"m\", \"messages\": [...]} returns {\"choices\": [{\"message\": {...}}]}.";
        assert_eq!(gaps(&doc(full), &ALL), vec![]);
    }

    #[test]
    fn each_missing_or_malformed_item_is_a_gap() {
        assert_eq!(gaps(&doc("We will prove it correct."), &[Item::Lean]), vec![Item::Lean], "no theorem statement");
        assert_eq!(gaps(&doc("theorem foo"), &[Item::Lean]), vec![Item::Lean], "a name without a statement");
        assert_eq!(gaps(&doc("MCC should be high."), &[Item::Mcc]), vec![Item::Mcc], "no threshold");
        assert_eq!(gaps(&doc("MCC ≥ 7"), &[Item::Mcc]), vec![Item::Mcc], "MCC lies in [-1, 1]");
        assert_eq!(gaps(&doc("MCC >= 0.5"), &[Item::Mcc]), vec![]);
        assert_eq!(gaps(&doc("SAM with AdamW"), &[Item::Sam]), vec![Item::Sam], "no rho");
        assert_eq!(gaps(&doc("SAM, rho = 0.05"), &[Item::Sam]), vec![Item::Sam], "no base optimizer");
        assert_eq!(gaps(&doc("SAM, rho = 0.05, base SGD"), &[Item::Sam]), vec![]);
        assert_eq!(gaps(&doc("POST /chat/completions"), &[Item::ChatCompletions]), vec![Item::ChatCompletions], "no request/response fields");
        let no_section = Doc { path: PathBuf::from("d.md"), text: "# D\n\n## Problem\n\nx\n".into() };
        assert_eq!(gaps(&no_section, &ALL), ALL.to_vec(), "no section: everything is missing");
    }

    #[test]
    fn nothing_required_means_no_gaps_even_without_the_section() {
        let no_section = Doc { path: PathBuf::from("d.md"), text: "# D\n".into() };
        assert!(gaps(&no_section, &[]).is_empty());
    }
}
