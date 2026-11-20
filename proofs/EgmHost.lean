/-
  The commit statuses egm-team sets on GitHub and GitLab, mirrored line for line by
  `gate_status` and `merge_status` in `crates/egm-web/src/host.rs`. Kernel-only: no Mathlib,
  no imports. Checked with Lean 4.31.0 and 4.22.0:

      lean proofs/EgmHost.lean

  With both statuses required in branch protection, the code host merges only what these allow.
  Proved for every history of builds, gate verdicts and approvals:
  * `gate_success_current`: `egm/evidence-gate` is green only for a passing gate verdict recorded
    after the latest build, on the current doc;
  * `merge_success_needs_gate`: `egm/merge-approval` is green only when the gate status is;
  * `merge_success_other_approver`: ... and only with an approval recorded after that gate
    verdict, for the current doc, by someone other than the author;
  * `author_alone_never_merges`: approvals by the author alone never turn it green;
  * `new_build_resets`: a build recorded after everything else turns both statuses back to
    waiting, so a change pushed after approval needs a new gate run and a new approval.
-/

namespace EgmHost

inductive State where
  | pending | success | failure
  deriving DecidableEq, Repr

structure Approval where
  id : Nat
  by_ : Nat
  doc : Nat
  deriving DecidableEq, Repr

structure Facts where
  lastBuild : Option Nat
  /-- (id, passed, doc judged) -/
  lastGate : Option (Nat × Bool × Nat)
  doc : Nat
  author : Nat
  approvals : List Approval
  deriving Repr

def gateStatus (f : Facts) : State :=
  match f.lastBuild, f.lastGate with
  | none, _ => .pending
  | some b, some (g, pass, d) => if b < g ∧ d = f.doc then (if pass then .success else .failure) else .pending
  | some _, none => .pending

def counts (f : Facts) (g : Nat) (a : Approval) : Bool :=
  decide (g < a.id) && decide (a.doc = f.doc) && decide (a.by_ ≠ f.author)

def mergeStatus (f : Facts) : State :=
  if gateStatus f = .success then
    match f.lastGate with
    | some (g, _, _) => if f.approvals.any (counts f g) then .success else .pending
    | none => .pending
  else .pending

theorem gate_success_current (f : Facts) (h : gateStatus f = .success) :
    ∃ b g d, f.lastBuild = some b ∧ f.lastGate = some (g, true, d) ∧ b < g ∧ d = f.doc := by
  unfold gateStatus at h
  split at h
  · exact absurd h (by decide)
  · rename_i b g pass d hb hg
    by_cases hc : b < g ∧ d = f.doc
    · cases pass
      · simp [hc] at h
      · exact ⟨b, g, d, hb, hg, hc.1, hc.2⟩
    · simp [hc] at h
  · exact absurd h (by decide)

theorem merge_success_needs_gate (f : Facts) (h : mergeStatus f = .success) : gateStatus f = .success := by
  unfold mergeStatus at h
  by_cases hg : gateStatus f = .success
  · exact hg
  · simp [hg] at h

theorem merge_success_other_approver (f : Facts) (h : mergeStatus f = .success) :
    ∃ g pass d, f.lastGate = some (g, pass, d) ∧
      ∃ a ∈ f.approvals, g < a.id ∧ a.doc = f.doc ∧ a.by_ ≠ f.author := by
  have hg := merge_success_needs_gate f h
  unfold mergeStatus at h
  simp only [hg, if_true] at h
  split at h
  · rename_i g pass d hlg
    by_cases ha : f.approvals.any (counts f g) = true
    · obtain ⟨a, hm, hc⟩ := List.any_eq_true.mp ha
      simp [counts] at hc
      exact ⟨g, pass, d, hlg, a, hm, hc.1.1, hc.1.2, hc.2⟩
    · simp [ha] at h
  · exact absurd h (by decide)

theorem author_alone_never_merges (f : Facts) (h : ∀ a ∈ f.approvals, a.by_ = f.author) :
    mergeStatus f ≠ .success := by
  intro hs
  obtain ⟨_, _, _, _, a, hm, _, _, hne⟩ := merge_success_other_approver f hs
  exact hne (h a hm)

theorem new_build_resets (f : Facts) (b : Nat)
    (hb : ∀ g p d, f.lastGate = some (g, p, d) → g < b) :
    gateStatus { f with lastBuild := some b } = .pending ∧ mergeStatus { f with lastBuild := some b } = .pending := by
  have hgate : gateStatus { f with lastBuild := some b } = .pending := by
    unfold gateStatus
    cases hl : f.lastGate with
    | none => simp
    | some t =>
      obtain ⟨g, p, d⟩ := t
      have := hb g p d hl
      have : ¬ (b < g) := by omega
      simp [this]
  refine ⟨hgate, ?_⟩
  unfold mergeStatus
  simp [hgate]

-- the field-test flow: alice (1) builds, the gate passes, ada (4) approves
example : mergeStatus ⟨some 5, some (6, true, 9), 9, 1, [⟨7, 4, 9⟩]⟩ = .success := by decide
example : mergeStatus ⟨some 5, some (6, true, 9), 9, 1, [⟨7, 1, 9⟩]⟩ = .pending := by decide
example : mergeStatus ⟨some 8, some (6, true, 9), 9, 1, [⟨7, 4, 9⟩]⟩ = .pending := by decide

end EgmHost
