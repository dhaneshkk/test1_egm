//! Who may do what in egm-team. A line-for-line mirror of `proofs/EgmPolicy.lean`, which proves
//! for every case: an author never reviews or approves their own feature; only an approver
//! approves a high-risk merge; the worker never reviews, approves or changes roles; and taking a
//! role away never grants an action. The test at the bottom re-checks the same four facts over
//! every combination of roles, so the two cannot drift apart.
//!
//! Every page, shard and API handler in the web app calls [`allowed`] before acting, and every
//! refusal is written to the audit log as `policy.refused`.

use crate::evidence::Tier;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Drives features: starts them, talks to the elephant, runs jobs.
    Developer,
    /// Judges designs, tests and evidence.
    Reviewer,
    /// Signs off merges, and is the only one who may approve a high-risk change.
    Approver,
    /// Manages members, roles and project settings.
    Admin,
    /// Reads everything and exports evidence; changes nothing.
    Auditor,
    /// The egm-worker process: records what a job did, pushes branches.
    Worker,
}

pub const ROLES: [Role; 6] = [Role::Developer, Role::Reviewer, Role::Approver, Role::Admin, Role::Auditor, Role::Worker];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Read,
    CreateFeature,
    RunJob,
    Comment,
    LabelAlarm,
    SubmitReview,
    ApproveMerge,
    RecordJob,
    ChangeRoles,
    ChangeSettings,
    Export,
}

pub const ACTIONS: [Action; 11] = [
    Action::Read,
    Action::CreateFeature,
    Action::RunJob,
    Action::Comment,
    Action::LabelAlarm,
    Action::SubmitReview,
    Action::ApproveMerge,
    Action::RecordJob,
    Action::ChangeRoles,
    Action::ChangeSettings,
    Action::Export,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Denial {
    /// No role the actor holds allows this action.
    NoRole,
    /// Authors may not review or approve their own feature.
    OwnFeature,
    /// A high-risk merge needs an approver.
    NeedsApprover,
}

impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Denial::NoRole => "none of your roles allows this",
            Denial::OwnFeature => "you cannot review or approve a feature you wrote",
            Denial::NeedsApprover => "a high-risk change needs an approver",
        })
    }
}

/// What one role allows on its own, before the author and risk rules.
pub fn grants(r: Role, a: Action) -> bool {
    use Action::*;
    use Role::*;
    match a {
        Read => true,
        CreateFeature | RunJob => r == Developer,
        Comment => matches!(r, Developer | Reviewer | Approver),
        LabelAlarm | SubmitReview | ApproveMerge => matches!(r, Reviewer | Approver),
        RecordJob => r == Worker,
        ChangeRoles | ChangeSettings => r == Admin,
        Export => matches!(r, Approver | Admin | Auditor),
    }
}

/// Mirrors `allowed` in `proofs/EgmPolicy.lean`. `author`: the actor wrote the feature acted
/// on; `tier`: the feature's risk tier (it matters only for `ApproveMerge`).
pub fn allowed(roles: &[Role], author: bool, tier: Tier, a: Action) -> Result<(), Denial> {
    if !roles.iter().any(|r| grants(*r, a)) {
        return Err(Denial::NoRole);
    }
    if author && matches!(a, Action::SubmitReview | Action::ApproveMerge) {
        return Err(Denial::OwnFeature);
    }
    if a == Action::ApproveMerge && tier == Tier::High && !roles.contains(&Role::Approver) {
        return Err(Denial::NeedsApprover);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subsets() -> impl Iterator<Item = Vec<Role>> {
        (0u32..1 << ROLES.len()).map(|m| ROLES.iter().enumerate().filter(|(i, _)| m & (1 << i) != 0).map(|(_, r)| *r).collect())
    }

    const TIERS: [Tier; 3] = [Tier::Low, Tier::Medium, Tier::High];

    /// The four theorems of `EgmPolicy.lean`, checked over all 64 role sets × author × tier ×
    /// action (4,224 cases).
    #[test]
    fn the_proved_facts_hold_in_every_case() {
        let mut n = 0;
        for rs in subsets() {
            for author in [false, true] {
                for t in TIERS {
                    for a in ACTIONS {
                        n += 1;
                        let ok = allowed(&rs, author, t, a).is_ok();
                        if author && matches!(a, Action::SubmitReview | Action::ApproveMerge) {
                            assert!(!ok, "author_never_approves: {rs:?} {a:?}");
                        }
                        if a == Action::ApproveMerge && t == Tier::High && ok {
                            assert!(rs.contains(&Role::Approver), "high_risk_needs_approver: {rs:?}");
                        }
                        if rs == [Role::Worker] && matches!(a, Action::SubmitReview | Action::ApproveMerge | Action::ChangeRoles | Action::ChangeSettings | Action::CreateFeature) {
                            assert!(!ok, "worker_limited: {a:?}");
                        }
                        // monotone: every superset of an allowed role set is allowed too
                        if ok {
                            for more in subsets().filter(|s| rs.iter().all(|r| s.contains(r))) {
                                assert!(allowed(&more, author, t, a).is_ok(), "monotone: {rs:?} ⊆ {more:?} {a:?}");
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 64 * 2 * 3 * ACTIONS.len());
    }

    #[test]
    fn the_field_test_team() {
        // 2 developers, 1 reviewer, 1 approver
        let dev = [Role::Developer];
        let rev = [Role::Reviewer];
        let appr = [Role::Approver];
        assert!(allowed(&rev, false, Tier::Medium, Action::ApproveMerge).is_ok());
        assert_eq!(allowed(&rev, false, Tier::High, Action::ApproveMerge), Err(Denial::NeedsApprover));
        assert!(allowed(&appr, false, Tier::High, Action::ApproveMerge).is_ok());
        assert_eq!(allowed(&dev, true, Tier::Low, Action::ApproveMerge), Err(Denial::NoRole));
        assert_eq!(allowed(&[Role::Developer, Role::Reviewer], true, Tier::Low, Action::SubmitReview), Err(Denial::OwnFeature));
        // the approver stands in for the reviewer
        assert!(allowed(&appr, false, Tier::Low, Action::SubmitReview).is_ok());
    }
}
