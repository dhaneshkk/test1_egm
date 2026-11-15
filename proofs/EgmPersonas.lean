/-
  EgmPersonas.lean — what adopting the upstream EGM personas can and cannot do
  to the build gate. Mirrors `official::is_blocker` and `collect_alarms` in
  `src/official.rs` and `src/goldfish.rs`. Lean 4 core only, no Mathlib.

  1. Severity: only an upstream `Blocker` (or egm-dsh's older `blocking`)
     becomes a blocking alarm; `Major` and `Nit` are shown but never block.
  2. Monotonicity: adding a review stage (zoom-out) appends alarms. It can turn
     a passing review into a failing one, never the reverse.
-/

inductive Severity where
  | blocker | major | nit
  deriving DecidableEq

/-- A finding as a stage reports it. -/
structure Finding where
  severity : Severity

/-- `is_blocker`: the one severity that raises a blocking alarm. -/
def isBlocker : Severity → Bool
  | .blocker => true
  | _        => false

/-- `collect_alarms` for one stage: one alarm per blocker finding. -/
def stageAlarms (fs : List Finding) : List Finding := fs.filter (fun f => isBlocker f.severity)

theorem only_blockers_block (fs : List Finding) (f : Finding) (h : f ∈ stageAlarms fs) :
    f.severity = .blocker := by
  unfold stageAlarms at h
  rw [List.mem_filter] at h
  cases hs : f.severity <;> simp_all [isBlocker]

theorem major_and_nit_never_block (fs : List Finding) :
    stageAlarms (fs.filter (fun f => f.severity ≠ .blocker)) = [] := by
  unfold stageAlarms
  rw [List.filter_filter, List.filter_eq_nil_iff]
  intro f _
  cases hs : f.severity <;> simp [isBlocker]

/-! ## Adding a stage never unblocks -/

structure Alarm where
  fromModel : Bool
  dropped   : Bool   -- carries a recorded drop reason

/-- Same rule as `Alarm.counts` in EgmGate.lean. -/
def Alarm.counts (a : Alarm) : Bool := !(a.fromModel && a.dropped)

def remaining (as : List Alarm) : List Alarm := as.filter Alarm.counts

/-- The review passes (as far as alarms go) exactly when nothing remains. -/
def clean (as : List Alarm) : Bool := (remaining as).isEmpty

/-- If the review with an extra stage's alarms `extra` is clean, the review
    without it was clean too: a new persona can only add reasons to block. -/
theorem adding_a_stage_never_unblocks (base extra : List Alarm) (h : clean (base ++ extra) = true) :
    clean base = true := by
  unfold clean remaining at *
  rw [List.filter_append] at h
  cases hb : List.filter Alarm.counts base with
  | nil => rfl
  | cons x xs => rw [hb] at h; simp at h

/-- Conversely, any surviving zoom-out alarm blocks, whatever the other stages said. -/
theorem surviving_stage_alarm_blocks (base extra : List Alarm) (a : Alarm)
    (hin : a ∈ extra) (hc : a.counts = true) : clean (base ++ extra) = false := by
  unfold clean remaining
  cases hr : (List.filter Alarm.counts (base ++ extra)) with
  | nil =>
    have : a ∈ List.filter Alarm.counts (base ++ extra) :=
      List.mem_filter.mpr ⟨List.mem_append_right base hin, hc⟩
    rw [hr] at this
    exact absurd this List.not_mem_nil
  | cons _ _ => rfl
