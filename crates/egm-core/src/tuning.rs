//! A tuned goldfish setup, as produced by `bench/tune` (Harness² on egm-dsh).
//!
//! The setup is a directory in Harness²'s component layout. Only prompt TEXT can
//! come from it:
//!
//!   systemprompt.md, guardrails.md, LongTermMEMORY.md,
//!   tool_descriptions/tool_guidance.md      -> "Reviewer guidance" for every persona stage
//!   sub_agents/<stage>/AGENT.md             -> replaces that stage's persona
//!                                              (comprehension, critic, zoom-out, readiness)
//!
//! Everything that decides a verdict stays in code and cannot be reached from
//! here: the JSON answer schemas, grounding, the verifier rules, egm-dsh's own
//! alarms, the pass rule, the gate, frozen tests and step limits. A setup with
//! any other file, or a file over the size cap, is refused outright.
//! Mirrors `proofs/EgmTune.lean`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use crate::goldfish::Stage;

/// Environment variable naming a setup directory; it wins over the config.
pub const ENV: &str = "EGM_DSH_GOLDFISH_HARNESS";
/// Per-file cap: every token of guidance is paid on every goldfish request.
pub const MAX_FILE_CHARS: usize = 8_000;
const GUIDANCE_FILES: [&str; 4] = ["systemprompt.md", "guardrails.md", "LongTermMEMORY.md", "tool_descriptions/tool_guidance.md"];
/// Harness² writes these explanatory stubs into every tree; they carry no instructions.
const STUBS: [&str; 5] = ["scripts/README.md", "sub_agents/README.md", "plugin/README.md", "skills/README.md", "README.md"];

pub fn stage_dir(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::Comprehension => Some("comprehension"),
        Stage::Critic => Some("critic"),
        Stage::ZoomOut => Some("zoom-out"),
        Stage::Readiness => Some("readiness"),
        Stage::Invariants => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Harness {
    pub guidance: String,
    /// stage dir name -> persona text (frontmatter removed)
    pub personas: BTreeMap<String, String>,
    /// FNV hash of every file, for the review record.
    pub fingerprint: u64,
}

fn strip_frontmatter(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---")
    {
        return rest[end + 4..].trim_start_matches('\n');
    }
    s
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if e.file_type()?.is_dir() {
            walk(base, &p, out)?;
        } else {
            out.push(p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Is this file allowed in a goldfish setup? Mirrors `allowed` in the Lean file.
pub fn allowed(rel: &str) -> bool {
    GUIDANCE_FILES.contains(&rel)
        || STUBS.contains(&rel)
        || ["comprehension", "critic", "zoom-out", "readiness"].iter().any(|s| rel == format!("sub_agents/{s}/AGENT.md"))
}

/// Load and check a setup directory. Any file outside the allowed set, or over the cap, refuses the whole setup.
pub fn load(dir: &Path) -> anyhow::Result<Harness> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files).map_err(|e| anyhow::anyhow!("cannot read goldfish setup {}: {e}", dir.display()))?;
    files.sort();
    let refused: Vec<&String> = files.iter().filter(|f| !allowed(f)).collect();
    if !refused.is_empty() {
        anyhow::bail!(
            "goldfish setup {} has files outside the tunable surface (only reviewer guidance and stage personas may be tuned): {}",
            dir.display(),
            refused.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    let mut h = Harness::default();
    let mut all = String::new();
    for rel in &files {
        let text = std::fs::read_to_string(dir.join(rel))?;
        if text.chars().count() > MAX_FILE_CHARS {
            anyhow::bail!("goldfish setup file {rel} is over {MAX_FILE_CHARS} characters");
        }
        all.push_str(rel);
        all.push('\0');
        all.push_str(&text);
        if STUBS.contains(&rel.as_str()) {
            continue;
        }
        if let Some(stage) = rel.strip_prefix("sub_agents/").and_then(|r| r.strip_suffix("/AGENT.md")) {
            let body = strip_frontmatter(&text).trim();
            if !body.is_empty() {
                h.personas.insert(stage.to_string(), body.to_string());
            }
        } else if !text.trim().is_empty() {
            h.guidance.push_str(text.trim());
            h.guidance.push_str("\n\n");
        }
    }
    h.guidance = h.guidance.trim().to_string();
    h.fingerprint = crate::doc::hash(&all);
    Ok(h)
}

static ACTIVE: OnceLock<Option<Harness>> = OnceLock::new();

/// Load the active setup once: `$EGM_DSH_GOLDFISH_HARNESS`, else the config's
/// `goldfish_harness` (relative to the project root). Call before any review.
pub fn init(root: &Path, configured: Option<&str>) -> anyhow::Result<()> {
    let dir = std::env::var(ENV).ok().filter(|s| !s.is_empty()).map(std::path::PathBuf::from)
        .or_else(|| configured.map(|c| root.join(c)));
    let h = match dir {
        Some(d) => Some(load(&d)?),
        None => None,
    };
    let _ = ACTIVE.set(h);
    Ok(())
}

pub fn active() -> Option<&'static Harness> {
    ACTIVE.get().and_then(|h| h.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = crate::safety::scratch_dir("harness");
        for (rel, text) in files {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d
    }

    #[test]
    fn loads_guidance_and_personas() {
        let d = dir(&[
            ("guardrails.md", "Check what happens to data saved before the change."),
            ("sub_agents/readiness/AGENT.md", "---\ndescription: tech lead\nmode: subagent\n---\nYou are the Tech Lead."),
            ("skills/README.md", "<!-- stub -->"),
        ]);
        let h = load(&d).unwrap();
        assert!(h.guidance.contains("saved before the change"));
        assert_eq!(h.personas.get("readiness").map(String::as_str), Some("You are the Tech Lead."));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn refuses_anything_outside_the_prompt_surface() {
        for bad in ["scripts/hack.py", "skills/x/SKILL.md", "plugin/p.ts", "sub_agents/judge/AGENT.md", "../escape.md", ".egm-dsh/config.json"] {
            assert!(!allowed(bad), "{bad}");
        }
        let d = dir(&[("guardrails.md", "ok"), ("scripts/hack.py", "print(1)")]);
        assert!(load(&d).unwrap_err().to_string().contains("outside the tunable surface"));
        std::fs::remove_dir_all(&d).unwrap();
        let big = "x".repeat(MAX_FILE_CHARS + 1);
        let d = dir(&[("guardrails.md", &big)]);
        assert!(load(&d).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
