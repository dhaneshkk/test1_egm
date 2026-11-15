/-
  EgmSpec.lean — what makes the spec executable. Mirrors `src/spec.rs` and the
  red/green and freeze checks in `commands::tests` and `commands::build`.
  Lean 4 core only, no Mathlib.

  A test is a function from code to pass/fail. A criterion is accepted only if
  its frozen test FAILED on the code before the build (red) and PASSES on the
  built code (green); a carried criterion went red at an earlier freeze. The
  builder cannot change a frozen test, and for a Lean check it can change only
  the proof.
-/

-- The code at some revision, abstractly.
variable {Code File : Type}

-- How a test file judges code.
variable (run : File → Code → Bool)

/-- `red_ok` for a fresh criterion: its test fails on the code before the build. -/
def Red (f : File) (before : Code) : Prop := run f before = false

/-- Green: the test passes on the built code. -/
def Green (f : File) (after : Code) : Prop := run f after = true

/-- A fresh criterion is accepted when its frozen file is unchanged, it went red
    before the build, and it is green after. -/
def Accepted (frozen current : File) (before after : Code) : Prop :=
  current = frozen ∧ Red run frozen before ∧ Green run current after

/-- An accepted test tells the old code from the new: it is not vacuous. -/
theorem accepted_tests_discriminate (frozen current : File) (before after : Code)
    (h : Accepted run frozen current before after) : run frozen before ≠ run frozen after := by
  obtain ⟨hc, hr, hg⟩ := h
  subst hc
  unfold Red at hr; unfold Green at hg
  rw [hr, hg]; decide

/-- A test that passes on every code is never accepted: red refuses it. -/
theorem vacuous_test_refused (f : File) (before : Code) (hv : ∀ c, run f c = true) :
    ¬ Red run f before := by
  unfold Red; rw [hv before]; decide

/-- Green is judged by the frozen test, not by whatever the builder left. -/
theorem green_uses_the_frozen_test (frozen current : File) (before after : Code)
    (h : Accepted run frozen current before after) : run frozen after = true := by
  obtain ⟨hc, _, hg⟩ := h
  subst hc
  exact hg

/-- A carried criterion (same text, same file) keeps the red it had at the freeze
    that introduced it; with green now, it still tells that old code from the new. -/
theorem carried_still_discriminates (f : File) (before₀ after : Code)
    (hred₀ : Red run f before₀) (hg : Green run f after) : run f before₀ ≠ run f after := by
  unfold Red at hred₀; unfold Green at hg
  rw [hred₀, hg]; decide

/-! ## Lean checks: only the proof may change -/

/-- A Lean file split at the checked theorem's proof. -/
structure LeanFile where
  before : String   -- definitions and the theorem statement
  proof  : String
  after  : String

/-- `lean_frozen_part`: everything except the proof. -/
def LeanFile.frozenPart (l : LeanFile) : String × String := (l.before, l.after)

/-- The build is accepted only if the frozen part matches. Then the statement
    and every definition it mentions are exactly the reviewed ones. -/
theorem lean_statement_preserved (spec built : LeanFile)
    (h : built.frozenPart = spec.frozenPart) : built.before = spec.before ∧ built.after = spec.after := by
  unfold LeanFile.frozenPart at h
  simp only [Prod.mk.injEq] at h
  exact h

/-! ## Examples: the doc's own text is the test -/

/-- An example block: commands and the exact output the doc expects. -/
abbrev Example := List (String × String)

/-- `run_example` succeeds iff every command prints exactly what the doc says. -/
def examplePasses (exec : String → String) (ex : Example) : Bool :=
  ex.all (fun (c, out) => exec c == out)

theorem example_passes_iff (exec : String → String) (ex : Example) [∀ a b : String, Decidable (a = b)] :
    examplePasses exec ex = true ↔ ∀ p ∈ ex, exec p.1 = p.2 := by
  unfold examplePasses
  simp [List.all_eq_true]

/-- A fresh example must fail before the build, so it describes new behavior. -/
theorem fresh_example_is_new (exec_before exec_after : String → String) (ex : Example)
    (hr : examplePasses exec_before ex = false) (hg : examplePasses exec_after ex = true) :
    ∃ p ∈ ex, exec_before p.1 ≠ exec_after p.1 := by
  unfold examplePasses at hr hg
  rw [List.all_eq_false] at hr
  rw [List.all_eq_true] at hg
  obtain ⟨p, hp, hne⟩ := hr
  refine ⟨p, hp, ?_⟩
  have h2 := hg p hp
  intro heq
  simp only [beq_iff_eq] at h2
  simp only [beq_iff_eq] at hne
  exact hne (heq ▸ h2)
