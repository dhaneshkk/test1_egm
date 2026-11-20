/-
  Who may do what in egm-team, mirrored line for line by `crates/egm-core/src/policy.rs`.
  Kernel-only: no Mathlib, no imports. Checked with Lean 4.31.0 and 4.22.0:

      lean proofs/EgmPolicy.lean

  Proved for every set of roles, author flag, risk tier and action:
  * `author_never_approves`: an author never reviews or approves their own feature;
  * `high_risk_needs_approver`: a high-risk merge is approved only by someone holding
    the approver role;
  * `worker_limited`: the worker alone can never review, approve, change roles or
    settings, or start a feature;
  * `monotone`: adding a role never takes an action away (so removing one never grants
    an action);
  * `read_iff_member`: egm-team's read-only pages open to anyone holding any role in the
    project, and to no one else.
-/

namespace EgmPolicy

inductive Role where
  | developer | reviewer | approver | admin | auditor | worker
  deriving DecidableEq, Repr

inductive Tier where
  | low | medium | high
  deriving DecidableEq, Repr

inductive Action where
  | read | createFeature | runJob | comment | labelAlarm | submitReview
  | approveMerge | recordJob | changeRoles | changeSettings | export
  deriving DecidableEq, Repr

/-- `grants` in Rust: what one role allows on its own. -/
def grants : Role → Action → Bool
  | _, .read => true
  | r, .createFeature => r == .developer
  | r, .runJob => r == .developer
  | r, .comment => r == .developer || r == .reviewer || r == .approver
  | r, .labelAlarm => r == .reviewer || r == .approver
  | r, .submitReview => r == .reviewer || r == .approver
  | r, .approveMerge => r == .reviewer || r == .approver
  | r, .recordJob => r == .worker
  | r, .changeRoles => r == .admin
  | r, .changeSettings => r == .admin
  | r, .export => r == .approver || r == .admin || r == .auditor

def selfReview : Action → Bool
  | .submitReview | .approveMerge => true
  | _ => false

/-- `allowed` in Rust (`Ok` = `true`). -/
def allowed (rs : List Role) (author : Bool) (t : Tier) (a : Action) : Bool :=
  rs.any (fun r => grants r a)
  && !(author && selfReview a)
  && !(a == .approveMerge && t == .high && !rs.contains .approver)

theorem author_never_approves (rs : List Role) (t : Tier) (a : Action)
    (ha : selfReview a = true) : allowed rs true t a = false := by
  simp [allowed, ha]

theorem high_risk_needs_approver (rs : List Role) (author : Bool)
    (h : allowed rs author .high .approveMerge = true) : Role.approver ∈ rs := by
  simp [allowed] at h
  exact h.2

theorem worker_limited (author : Bool) (t : Tier) (a : Action)
    (ha : a = .submitReview ∨ a = .approveMerge ∨ a = .changeRoles ∨ a = .changeSettings ∨ a = .createFeature) :
    allowed [.worker] author t a = false := by
  rcases ha with h | h | h | h | h <;> subst h <;> cases author <;> cases t <;> decide

theorem any_mono {rs rs' : List Role} (hsub : ∀ r, r ∈ rs → r ∈ rs') (f : Role → Bool)
    (h : rs.any f = true) : rs'.any f = true := by
  rw [List.any_eq_true] at h ⊢
  obtain ⟨r, hr, hf⟩ := h
  exact ⟨r, hsub r hr, hf⟩

theorem monotone (rs rs' : List Role) (hsub : ∀ r, r ∈ rs → r ∈ rs') (author : Bool) (t : Tier) (a : Action)
    (h : allowed rs author t a = true) : allowed rs' author t a = true := by
  simp only [allowed, Bool.and_eq_true] at h ⊢
  obtain ⟨⟨h1, h2⟩, h3⟩ := h
  refine ⟨⟨any_mono hsub _ h1, h2⟩, ?_⟩
  cases hc : rs.contains .approver
  · -- without the approver role the third conjunct held only because it was not a high-risk merge
    cases hc' : rs'.contains .approver <;> simp_all
  · have : Role.approver ∈ rs' := hsub _ (by simpa using hc)
    have : rs'.contains .approver = true := by simpa using this
    simp_all

/-- egm-team's read-only pages (phase 2): any role can read a project, no role cannot. -/
theorem read_iff_member (rs : List Role) (author : Bool) (t : Tier) :
    allowed rs author t .read = !rs.isEmpty := by
  cases rs <;> simp [allowed, grants, selfReview]

-- the field-test team: 2 developers, 1 reviewer, 1 approver
example : allowed [.reviewer] false .medium .approveMerge = true := by decide
example : allowed [.reviewer] false .high .approveMerge = false := by decide
example : allowed [.approver] false .high .approveMerge = true := by decide
example : allowed [.developer, .reviewer] true .low .submitReview = false := by decide

end EgmPolicy
