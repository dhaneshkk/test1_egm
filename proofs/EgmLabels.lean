/-
  Goldfish MCC from reviewers' labels (egm-team phase 3). Kernel-only: no Mathlib, no imports.
  Checked with Lean 4.31.0 and 4.22.0:

      lean proofs/EgmLabels.lean

  Each goldfish alarm a reviewer has labelled is one case. The goldfish predicts "real" when it
  kept the alarm (status open or confirmed, not dropped); the reviewer's label says whether it
  was real (valid) or noise. `confusion` in `crates/egm-web/src/db.rs` counts the four cells
  over the latest label of each alarm. Proved here:

  * `confusion_total`: the four cells add up to the number of labelled alarms, so every label
    is counted exactly once;
  * `perfect_agreement`: when the goldfish kept exactly the alarms reviewers called valid, there
    are no false positives and no false negatives (MCC is then 1, or undefined when a margin is
    empty);
  * `relabel_moves_one`: changing one label moves exactly one case between two cells; the total
    is unchanged.
-/

namespace EgmLabels

structure Case where
  kept : Bool
  valid : Bool
  deriving DecidableEq, Repr

def tp (l : List Case) : Nat := (l.filter (fun c => c.kept && c.valid)).length
def fp (l : List Case) : Nat := (l.filter (fun c => c.kept && !c.valid)).length
def fn (l : List Case) : Nat := (l.filter (fun c => !c.kept && c.valid)).length
def tn (l : List Case) : Nat := (l.filter (fun c => !c.kept && !c.valid)).length

theorem confusion_total (l : List Case) : tp l + fp l + fn l + tn l = l.length := by
  induction l with
  | nil => rfl
  | cons c cs ih =>
    rcases c with ⟨k, v⟩
    cases k <;> cases v <;> simp [tp, fp, fn, tn] at ih ⊢ <;> omega

theorem perfect_agreement (l : List Case) (h : ∀ c ∈ l, c.kept = c.valid) : fp l = 0 ∧ fn l = 0 := by
  constructor
  · unfold fp
    rw [List.length_eq_zero_iff, List.filter_eq_nil_iff]
    intro c hc
    have := h c hc
    cases hk : c.kept <;> simp_all
  · unfold fn
    rw [List.length_eq_zero_iff, List.filter_eq_nil_iff]
    intro c hc
    have := h c hc
    cases hk : c.kept <;> simp_all

theorem relabel_moves_one (pre post : List Case) (k v v' : Bool) :
    (tp (pre ++ ⟨k, v'⟩ :: post) + fp (pre ++ ⟨k, v'⟩ :: post) + fn (pre ++ ⟨k, v'⟩ :: post) + tn (pre ++ ⟨k, v'⟩ :: post))
      = (tp (pre ++ ⟨k, v⟩ :: post) + fp (pre ++ ⟨k, v⟩ :: post) + fn (pre ++ ⟨k, v⟩ :: post) + tn (pre ++ ⟨k, v⟩ :: post)) := by
  rw [confusion_total, confusion_total]
  simp

example : tp [⟨true, true⟩, ⟨true, false⟩, ⟨false, false⟩] = 1 := by decide

end EgmLabels
