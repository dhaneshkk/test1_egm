//! Reviewer personas and document standards from Google's Elephant-Goldfish
//! Model plugin (<https://github.com/google/elephant-goldfish-model>, commit
//! 3d0b348, Apache-2.0). The files under `vendor/egm-official/` are unmodified
//! copies; see `vendor/egm-official/NOTICE`. egm-dsh is not affiliated with
//! Google and the upstream project is "not an officially supported Google product".
//!
//! Each persona file serves two modes (BRAINSTORM and REVIEW) and ends with a
//! free-text output format. A goldfish stage only needs the role and the REVIEW
//! instructions; egm-dsh asks for JSON instead, so the output format is left out.

use crate::goldfish::Stage;

pub const UPSTREAM: &str = "google/elephant-goldfish-model@3d0b348";

const COMPREHENSION: &str = include_str!("../vendor/egm-official/agents/comprehension.md");
const DESIGN_CRITIC: &str = include_str!("../vendor/egm-official/agents/design_critic.md");
const READINESS: &str = include_str!("../vendor/egm-official/agents/implementation_readiness.md");
const ZOOM_OUT: &str = include_str!("../vendor/egm-official/agents/zoom_out.md");

pub const FEATURE_SPEC_STANDARD: &str = include_str!("../vendor/egm-official/resources/document_standards/feature-spec-standard.md");
pub const READINESS_STANDARD: &str = include_str!("../vendor/egm-official/resources/document_standards/implementation-readiness-standard.md");
pub const ZOOM_OUT_STANDARD: &str = include_str!("../vendor/egm-official/resources/document_standards/zoom-out-standard.md");

/// The persona file and the standard a stage is judged against, if any.
fn sources(stage: Stage) -> Option<(&'static str, Option<&'static str>)> {
    match stage {
        Stage::Comprehension => Some((COMPREHENSION, None)),
        Stage::Critic => Some((DESIGN_CRITIC, Some(FEATURE_SPEC_STANDARD))),
        Stage::ZoomOut => Some((ZOOM_OUT, Some(ZOOM_OUT_STANDARD))),
        Stage::Readiness => Some((READINESS, Some(READINESS_STANDARD))),
        Stage::Invariants => None, // egm-dsh's own stage; upstream has no counterpart
    }
}

/// Text between the first line starting with `start` and the first later line
/// starting with `end` (or the end of the file).
fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let Some(i) = text.find(&format!("\n{start}")) else { return "" };
    let rest = &text[i + 1..];
    let j = rest[start.len()..].find(&format!("\n{end}")).map(|j| j + start.len()).unwrap_or(rest.len());
    let body = &rest[..j];
    body.split_once('\n').map(|(_, b)| b).unwrap_or("").trim()
}

/// The persona a stage runs as: role, REVIEW instructions and audit vectors.
pub fn persona(stage: Stage) -> Option<String> {
    let (file, _) = sources(stage)?;
    let role = between(file, "# Role", "---");
    let review = between(file, "### IF Mode: REVIEW", "### Audit Vectors");
    let vectors = between(file, "### Audit Vectors", "---");
    let text = format!("{role}\n\nReview instructions:\n{review}\n\nAudit Vectors:\n{vectors}")
        .replace("(`resources/document_standards/zoom-out-standard.md`)", "(below)")
        .replace("(provided by the orchestrator)", "(below)");
    Some(text)
}

/// The persona and its standard, as one block for a stage prompt.
pub fn persona_block(stage: Stage) -> Option<String> {
    let p = persona(stage)?;
    let tuned = crate::tuning::active();
    let mut s = match tuned.and_then(|h| crate::tuning::stage_dir(stage).and_then(|d| h.personas.get(d))) {
        Some(t) => format!("Review persona (tuned setup):\n\n{t}"),
        None => format!("Review persona (adapted from {UPSTREAM}, Apache-2.0):\n\n{p}"),
    };
    if let Some((_, Some(standard))) = sources(stage) {
        s.push_str(&format!("\n\nTarget Standard:\n\n{}", standard.trim()));
    }
    if let Some(h) = tuned.filter(|h| !h.guidance.is_empty()) {
        s.push_str(&format!("\n\nReviewer guidance:\n\n{}", h.guidance));
    }
    Some(s)
}

/// Map upstream severities (Blocker | Major | Nit) and egm-dsh's older names
/// (blocking | major | minor) to one scale.
pub fn is_blocker(severity: &str) -> bool {
    let s = severity.trim().trim_start_matches('🔴').trim().to_lowercase();
    s == "blocker" || s == "blocking"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personas_keep_review_mode_and_drop_brainstorm_and_output_format() {
        for st in [Stage::Comprehension, Stage::Critic, Stage::ZoomOut, Stage::Readiness] {
            let p = persona(st).unwrap();
            assert!(p.starts_with("You are **"), "{st:?}: {}", &p[..40.min(p.len())]);
            assert!(p.contains("Audit Vectors"), "{st:?}");
            assert!(!p.contains("BRAINSTORM"), "{st:?}");
            assert!(!p.contains("Output Format"), "{st:?}");
            assert!(!p.contains("Core Mandates"), "{st:?}");
        }
        assert!(persona(Stage::Invariants).is_none());
    }

    #[test]
    fn standards_are_attached_where_upstream_uses_them() {
        assert!(persona_block(Stage::ZoomOut).unwrap().contains("# Zoom Out Standard"));
        assert!(persona_block(Stage::Critic).unwrap().contains("The Alternatives Rule"));
        assert!(persona_block(Stage::Readiness).unwrap().contains("The Zero-Decision Rule"));
        assert!(!persona_block(Stage::Comprehension).unwrap().contains("Target Standard"));
        assert!(!persona_block(Stage::ZoomOut).unwrap().contains("resources/document_standards"));
    }

    #[test]
    fn severity_scales_agree() {
        for s in ["Blocker", "blocking", "🔴 Blocker", " BLOCKER "] {
            assert!(is_blocker(s), "{s}");
        }
        for s in ["Major", "Nit", "minor", ""] {
            assert!(!is_blocker(s), "{s}");
        }
    }
}
