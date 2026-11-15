/-
  The never-drop rule for learned alarm triage, mirrored by `src/triage.rs`.
  Kernel-only: no Mathlib, no imports. Checked with Lean 4.31.0:

      lean proofs/EgmTriage.lean

  A learned model (CatBoost or the SAM-trained net, behind an OpenAI-compatible
  endpoint) looks at each alarm that survived grounding and may answer
  "keep": skip the verifier and let the alarm stand. That is its only power.

  Everything below is proved for EVERY possible triage function `skip`, so it
  holds however good, bad, buggy or adversarial the model is:

  * `triage_never_drops`: an alarm the model keeps leaves triage exactly as it entered.
  * `triage_drop_needs_verifier`: any alarm dropped with triage in place would
    also have been dropped by the verifier alone, for the same reason.
  * `triage_only_stricter`: if the gate passes with triage, it passes without it.
    A model can make egm keep more alarms (cost), never fewer (safety).
  * `egm_alarms_untouched`: egm's own alarms are never dropped, with or without triage.
-/

inductive DropReason where
  | quoteMissing | answeredByDoc | notBlocking
  deriving DecidableEq

structure Alarm where
  fromModel : Bool
  dropped   : Option DropReason

/-- Still blocks the build (`Alarm::counts` in Rust). -/
def Alarm.counts (a : Alarm) : Bool := !(a.fromModel && a.dropped.isSome)

def remaining (as : List Alarm) : List Alarm := as.filter Alarm.counts

/-- The verifier: for an open model-raised alarm it may attach an
    evidence-backed reason (`judge` returns `none` when it confirms or is unsure). -/
def verify (judge : Alarm → Option DropReason) (a : Alarm) : Alarm :=
  if a.fromModel && a.dropped.isNone then { a with dropped := judge a } else a

/-- Triage in front of the verifier: alarms the model keeps skip it entirely. -/
def triage (skip : Alarm → Bool) (judge : Alarm → Option DropReason) (a : Alarm) : Alarm :=
  if skip a then a else verify judge a

theorem triage_never_drops (skip : Alarm → Bool) (judge : Alarm → Option DropReason)
    (a : Alarm) (h : skip a = true) : triage skip judge a = a := by
  simp [triage, h]

theorem triage_drop_needs_verifier (skip : Alarm → Bool) (judge : Alarm → Option DropReason)
    (a : Alarm) (fresh : a.dropped = none) (r : DropReason)
    (h : (triage skip judge a).dropped = some r) : (verify judge a).dropped = some r := by
  unfold triage at h
  cases hs : skip a
  · simpa [hs] using h
  · simp [hs, fresh] at h

/-- Pointwise: whatever still counts after verification alone still counts with triage. -/
theorem counts_mono (skip : Alarm → Bool) (judge : Alarm → Option DropReason) (a : Alarm)
    (h : (verify judge a).counts = true) : (triage skip judge a).counts = true := by
  unfold triage
  cases hs : skip a
  · simpa [hs] using h
  · simp only [if_true]
    unfold verify at h
    unfold Alarm.counts at h ⊢
    cases hm : a.fromModel <;> cases hd : a.dropped <;> simp_all

theorem triage_only_stricter (skip : Alarm → Bool) (judge : Alarm → Option DropReason)
    (as : List Alarm) (pass : remaining (as.map (triage skip judge)) = []) :
    remaining (as.map (verify judge)) = [] := by
  unfold remaining at *
  rw [List.filter_eq_nil_iff] at *
  intro v hv
  rw [List.mem_map] at hv
  obtain ⟨a, ha, rfl⟩ := hv
  intro hc
  exact pass (triage skip judge a) (List.mem_map.mpr ⟨a, ha, rfl⟩) (counts_mono skip judge a hc)

theorem egm_alarms_untouched (skip : Alarm → Bool) (judge : Alarm → Option DropReason)
    (a : Alarm) (h : a.fromModel = false) : (triage skip judge a).counts = true := by
  unfold triage verify Alarm.counts
  cases hs : skip a <;> simp [h]
