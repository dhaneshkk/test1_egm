/-
  EgmTune.lean — what a tuned goldfish setup (Harness² on egm-dsh) can and
  cannot change. Mirrors `src/tuning.rs` and `adopt_ok` in `bench/tune/tune.py`.
  Lean 4 core only, no Mathlib.

  1. A setup is prompt text only. Applying one changes the prompts and nothing
     else, so the verdict rule, the gate and the frozen spec are untouched
     whatever the setup says.
  2. A setup with any file outside the allowed surface is refused whole.
  3. Adoption needs all of: a strictly better held-out MCC, input cost within
     5/4 of the base, egm-dsh accepting the setup, and a human's approval.
-/

/-- What egm-dsh is configured with. `prompts` is the only part a setup reaches. -/
structure Rules where
  passRule  : Bool → Nat → Nat → Bool      -- ready, questions, blockers ↦ pass
  maxFiles  : Nat
  frozen    : List String                 -- frozen test files

structure Config where
  prompts : List String
  rules   : Rules

/-- A tuned setup: text, nothing else. -/
structure Setup where
  guidance : String
  personas : List String

/-- `tuning::active` feeding `official::persona_block`: only the prompts change. -/
def applySetup (s : Setup) (c : Config) : Config :=
  { c with prompts := c.prompts.map (fun p => p ++ s.guidance) ++ s.personas }

theorem setup_never_touches_rules (s : Setup) (c : Config) : (applySetup s c).rules = c.rules := rfl

/-- So the pass rule, the step limit and the frozen spec are the same, for every setup. -/
theorem verdict_rule_unchanged (s : Setup) (c : Config) (r : Bool) (q b : Nat) :
    (applySetup s c).rules.passRule r q b = c.rules.passRule r q b := rfl

theorem frozen_spec_unchanged (s : Setup) (c : Config) : (applySetup s c).rules.frozen = c.rules.frozen := rfl

/-! ## Only the prompt surface is accepted -/

def stages : List String := ["comprehension", "critic", "zoom-out", "readiness"]
def guidanceFiles : List String := ["systemprompt.md", "guardrails.md", "LongTermMEMORY.md", "tool_descriptions/tool_guidance.md"]

/-- `tuning::allowed` (README stubs omitted: they are never read). -/
def allowed (rel : String) : Bool :=
  guidanceFiles.contains rel || stages.any (fun s => rel == "sub_agents/" ++ s ++ "/AGENT.md")

/-- `tuning::load`: refused whole if any file is outside the surface. -/
def load (files : List String) : Option (List String) :=
  if files.all allowed then some files else none

theorem load_refuses_outside (files : List String) (f : String) (hf : f ∈ files) (hb : allowed f = false) :
    load files = none := by
  unfold load
  have : files.all allowed = false := by
    rw [List.all_eq_false]
    exact ⟨f, hf, by simp [hb]⟩
  simp [this]

theorem loaded_files_are_allowed (files out : List String) (h : load files = some out) :
    ∀ f ∈ out, allowed f = true := by
  unfold load at h
  split at h
  · rename_i hall
    injection h with h; subst h
    intro f hf
    exact (List.all_eq_true.mp hall) f hf
  · simp at h

example : allowed "scripts/hack.py" = false := by decide
example : allowed ".egm-dsh/config.json" = false := by decide
example : allowed "guardrails.md" = true := by decide

/-! ## Adoption -/

/-- Held-out results, MCC scaled by 1000 to stay in integers. -/
structure Eval where
  mcc1000 : Int
  inputTokens : Nat

/-- `adopt_ok` in tune.py. -/
def adopt (base tuned : Eval) (accepted approved : Bool) : Bool :=
  decide (tuned.mcc1000 > base.mcc1000) && decide (4 * tuned.inputTokens ≤ 5 * base.inputTokens) && accepted && approved

theorem adopt_needs_everything (base tuned : Eval) (accepted approved : Bool)
    (h : adopt base tuned accepted approved = true) :
    tuned.mcc1000 > base.mcc1000 ∧ 4 * tuned.inputTokens ≤ 5 * base.inputTokens ∧ accepted = true ∧ approved = true := by
  unfold adopt at h
  simp only [Bool.and_eq_true, decide_eq_true_eq] at h
  exact ⟨h.1.1.1, h.1.1.2, h.1.2, h.2⟩

/-- No human approval, no adoption, whatever the numbers say. -/
theorem no_approval_no_adoption (base tuned : Eval) (accepted : Bool) : adopt base tuned accepted false = false := by
  unfold adopt; simp

/-- A setup that is not strictly better on held-out documents is never adopted. -/
theorem no_gain_no_adoption (base tuned : Eval) (accepted approved : Bool) (h : tuned.mcc1000 ≤ base.mcc1000) :
    adopt base tuned accepted approved = false := by
  unfold adopt
  have : decide (tuned.mcc1000 > base.mcc1000) = false := by simp; omega
  simp [this]
