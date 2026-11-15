/-
  EgmSteps.lean — iterative development as a chain of small steps.
  Mirrors `src/steps.rs` and the phase loop in `commands::build`.
  Lean 4 core only, no Mathlib.

  1. Size     an oversized step raises an egm-dsh alarm, so it never passes.
  2. Phases   phases build in order 1, 2, …, n; `built` needs all n.
  3. Handover drift stops checking a file under step f only when a LATER
              build lists it, and every built file stays checked by someone.
  4. Depends  a step builds only after its dependencies; a dependency cycle
              can never build at all.
-/

/-! ## 1. Size -/

structure Alarm where
  fromModel : Bool
  dropped   : Bool

def Alarm.counts (a : Alarm) : Bool := !(a.fromModel && a.dropped)
def clean (as : List Alarm) : Bool := (as.filter Alarm.counts).isEmpty

/-- `size_alarms`: one egm-dsh alarm when the step lists more files than the limit (0 = off). -/
def sizeAlarms (files limit : Nat) : List Alarm :=
  if limit > 0 ∧ files > limit then [⟨false, false⟩] else []

theorem oversized_never_passes (files limit : Nat) (others : List Alarm)
    (h0 : limit > 0) (h : files > limit) : clean (others ++ sizeAlarms files limit) = false := by
  unfold clean sizeAlarms
  simp [h0, h, List.filter_append, Alarm.counts]

/-! ## 2. Phases -/

/-- `phase_step`: phase k may run after `done` phases of `total` only if it is the next one. -/
def phaseStep (done total k : Nat) : Option Nat :=
  if k = done + 1 ∧ k ≤ total then some k else none

/-- Run a list of phase requests from `done`; `none` as soon as one is refused. -/
def runPhases (total : Nat) : Nat → List Nat → Option Nat
  | done, []      => some done
  | done, k :: ks => match phaseStep done total k with
    | some d => runPhases total d ks
    | none   => none

/-- Every accepted run is exactly the consecutive phases after `done`, within `total`. -/
theorem phases_in_order (total : Nat) : ∀ (ks : List Nat) (done d : Nat),
    runPhases total done ks = some d → ks = List.range' (done + 1) (d - done) ∧ done ≤ d ∧ (ks ≠ [] → d ≤ total) := by
  intro ks
  induction ks with
  | nil =>
    intro done d h
    simp [runPhases] at h
    subst h
    simp
  | cons k ks ih =>
    intro done d h
    by_cases hk : k = done + 1 ∧ k ≤ total
    · obtain ⟨hk1, hk2⟩ := hk
      subst hk1
      have hs : phaseStep done total (done + 1) = some (done + 1) := by simp [phaseStep, hk2]
      have h' : runPhases total (done + 1) ks = some d := by
        have h2 := h
        simp only [runPhases, hs] at h2
        exact h2
      obtain ⟨hks, hle, htot⟩ := ih (done + 1) d h'
      refine ⟨?_, by omega, ?_⟩
      · have e : d - done = (d - (done + 1)) + 1 := by omega
        rw [e, List.range'_succ, hks]
      · intro _
        cases ks with
        | nil =>
          have : d = done + 1 := by simpa [runPhases] using h'.symm
          omega
        | cons _ _ => exact htot (by simp)
    · simp [runPhases, phaseStep, hk] at h

/-- The feature becomes `built` only when the phases reach `total`: from 0, that
    is exactly the requests 1, 2, …, total. -/
theorem built_needs_every_phase (total : Nat) (ks : List Nat)
    (h : runPhases total 0 ks = some total) : ks = List.range' 1 total := by
  have := (phases_in_order total ks 0 total h).1
  simpa using this

/-- A doc edit resets progress to 0, so the next accepted phase is phase 1 again. -/
theorem edit_restarts_phases (total k : Nat) (h : phaseStep 0 total k = some k) : k = 1 := by
  unfold phaseStep at h
  split at h
  · rename_i hk; omega
  · simp at h

/-! ## 3. Handover -/

structure Build where
  slug  : String
  seq   : Nat
  files : List String

/-- `handed_over`: some build later than `f` lists the file. -/
def handedOver (f : Build) (p : String) (bs : List Build) : Bool :=
  bs.any (fun g => decide (p ∈ g.files) && decide (g.seq > f.seq))

/-- Drift is silenced for (f, p) only by a later build that lists p. -/
theorem silenced_needs_later_build (f : Build) (p : String) (bs : List Build)
    (h : handedOver f p bs = true) : ∃ g ∈ bs, p ∈ g.files ∧ g.seq > f.seq := by
  unfold handedOver at h
  rw [List.any_eq_true] at h
  obtain ⟨g, hg, hc⟩ := h
  simp at hc
  exact ⟨g, hg, hc.1, hc.2⟩

theorem exists_latest (l : List Build) (h : l ≠ []) : ∃ g ∈ l, ∀ x ∈ l, x.seq ≤ g.seq := by
  induction l with
  | nil => exact absurd rfl h
  | cons a t ih =>
    cases t with
    | nil => exact ⟨a, by simp, by intro x hx; simp at hx; subst hx; omega⟩
    | cons b t' =>
      obtain ⟨g, hg, hmax⟩ := ih (by simp)
      by_cases hle : a.seq ≤ g.seq
      · exact ⟨g, List.mem_cons_of_mem a hg, by
          intro x hx
          cases List.mem_cons.mp hx with
          | inl e => subst e; exact hle
          | inr hx => exact hmax x hx⟩
      · exact ⟨a, by simp, by
          intro x hx
          cases List.mem_cons.mp hx with
          | inl e => subst e; omega
          | inr hx => have := hmax x hx; omega⟩

/-- Handover never leaves a file unchecked: if any build lists p, some build
    that lists p is not handed over, and drift checks p under it. -/
theorem every_built_file_stays_checked (bs : List Build) (p : String) (g0 : Build)
    (hg0 : g0 ∈ bs) (hp : p ∈ g0.files) : ∃ g ∈ bs, p ∈ g.files ∧ handedOver g p bs = false := by
  let owners := bs.filter (fun g => decide (p ∈ g.files))
  have hne : owners ≠ [] := by
    intro he
    have : g0 ∈ owners := List.mem_filter.mpr ⟨hg0, by simp [hp]⟩
    rw [he] at this
    exact absurd this List.not_mem_nil
  obtain ⟨g, hg, hmax⟩ := exists_latest owners hne
  have hgbs := (List.mem_filter.mp hg).1
  have hgp : p ∈ g.files := by simpa using (List.mem_filter.mp hg).2
  refine ⟨g, hgbs, hgp, ?_⟩
  cases h : handedOver g p bs with
  | false => rfl
  | true =>
    obtain ⟨x, hx, hxp, hxs⟩ := silenced_needs_later_build g p bs h
    have := hmax x (List.mem_filter.mpr ⟨hx, by simp [hxp]⟩)
    omega

/-! ## 4. Dependencies -/

variable (deps : String → List String)

/-- Build history, latest first: each step was built only after all its deps. -/
def ValidOrder : List String → Prop
  | []     => True
  | x :: l => ValidOrder l ∧ ∀ d ∈ deps x, d ∈ l

/-- Every dependency of a built step was built before it. -/
theorem deps_built_first (x : String) (l : List String) (h : ValidOrder deps (x :: l)) :
    ∀ d ∈ deps x, d ∈ l := h.2

/-- A set of steps where each depends on another step in the set (any cycle
    is such a set) never builds, whatever order is tried. -/
theorem cycle_never_builds (S : String → Prop) (hS : ∀ s, S s → ∃ d ∈ deps s, S d) :
    ∀ l, ValidOrder deps l → ∀ y ∈ l, ¬ S y := by
  intro l
  induction l with
  | nil => intro _ y hy; simp at hy
  | cons x l ih =>
    intro hv y hy hSy
    have hl := ih hv.1
    cases List.mem_cons.mp hy with
    | inl e =>
      subst e
      obtain ⟨d, hd, hSd⟩ := hS y hSy
      exact hl d (hv.2 d hd) hSd
    | inr hy => exact hl y hy hSy
