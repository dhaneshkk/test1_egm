/-
  Which queued job egm-team's worker starts next, mirrored line for line by
  `crates/egm-core/src/queue.rs`. Kernel-only: no Mathlib, no imports.
  Checked with Lean 4.31.0 and 4.22.0:

      lean proofs/EgmQueue.lean

  Proved for every queue and every ceiling:
  * `start_keeps_one_per_project`: if at most one job per project is running, starting the
    picked job keeps it so (egm-dsh's state is files in one checkout per project);
  * `finish_keeps_one_per_project`: finishing a job keeps it so too;
  * `start_is_oldest`: the started job is the oldest queued job of its project;
  * `start_under_ceiling`, `refuse_only_over`: a job starts only while its project is under its
    token ceiling, and is refused only when it is at or over it.
-/

namespace EgmQueue

inductive State where
  | queued | running | done
  deriving DecidableEq, Repr

structure Job where
  id : Nat
  project : String
  state : State
  deriving DecidableEq, Repr

inductive Pick where
  | start (j : Job)
  | refuse (j : Job)
  deriving DecidableEq, Repr

/-- Some job of the project is running. -/
def running (js : List Job) (p : String) : Bool :=
  js.any (fun k => k.project == p && k.state == .running)

/-- The oldest queued job of a project (the queue is oldest first). -/
def oldest (js : List Job) (p : String) : Option Job :=
  js.find? (fun k => k.project == p && k.state == .queued)

def eligible (js : List Job) (j : Job) : Bool :=
  j.state == .queued && !running js j.project && oldest js j.project == some j

/-- `over p`: the project is at or over its token ceiling. -/
def pick (js : List Job) (over : String → Bool) : Option Pick :=
  match js.find? (eligible js) with
  | none => none
  | some j => if over j.project then some (.refuse j) else some (.start j)

def overCeiling (spent : Nat) (ceiling : Option Nat) : Bool :=
  match ceiling with
  | none => false
  | some c => decide (c ≤ spent)

def setState (s : State) (j : Job) (js : List Job) : List Job :=
  js.map (fun k => if k = j then { k with state := s } else k)

/-- At most one running job per project. -/
def OnePer (js : List Job) : Prop :=
  ∀ a ∈ js, ∀ b ∈ js, a.state = .running → b.state = .running → a.project = b.project → a = b

theorem pick_start {js : List Job} {over : String → Bool} {j : Job}
    (h : pick js over = some (.start j)) : j ∈ js ∧ eligible js j = true ∧ over j.project = false := by
  unfold pick at h
  split at h
  · exact absurd h (by simp)
  · rename_i j' hf
    by_cases ho : over j'.project = true
    · simp [ho] at h
    · simp [ho] at h
      subst h
      exact ⟨List.mem_of_find?_eq_some hf, List.find?_some hf, by simpa using ho⟩

theorem refuse_only_over {js : List Job} {over : String → Bool} {j : Job}
    (h : pick js over = some (.refuse j)) : over j.project = true := by
  unfold pick at h
  split at h
  · exact absurd h (by simp)
  · rename_i j' _
    by_cases ho : over j'.project = true
    · simp [ho] at h
      subst h
      exact ho
    · simp [ho] at h

theorem start_under_ceiling {js : List Job} {spent : String → Nat} {ceiling : String → Option Nat} {j : Job}
    (h : pick js (fun p => overCeiling (spent p) (ceiling p)) = some (.start j)) :
    ∀ c, ceiling j.project = some c → spent j.project < c := by
  intro c hc
  have h3 := (pick_start h).2.2
  simp [overCeiling, hc] at h3
  exact h3

theorem start_is_oldest {js : List Job} {over : String → Bool} {j : Job}
    (h : pick js over = some (.start j)) : j.state = .queued ∧ oldest js j.project = some j := by
  have he := (pick_start h).2.1
  simp [eligible] at he
  exact ⟨he.1.1, he.2⟩

theorem not_running_of {js : List Job} {p : String} (h : running js p = false) :
    ∀ k ∈ js, k.project = p → k.state ≠ .running := by
  intro k hk hp hs
  have : running js p = true := List.any_eq_true.mpr ⟨k, hk, by simp [hp, hs]⟩
  simp [this] at h

theorem mem_setState {s : State} {j a : Job} {js : List Job} (ha : a ∈ setState s j js) :
    (a = { j with state := s } ∧ j ∈ js) ∨ (a ∈ js ∧ a ≠ j) := by
  unfold setState at ha
  rw [List.mem_map] at ha
  obtain ⟨k, hk, rfl⟩ := ha
  by_cases hkj : k = j
  · subst hkj
    left
    simp [hk]
  · right
    simp [hkj, hk]

theorem start_keeps_one_per_project {js : List Job} {over : String → Bool} {j : Job}
    (hinv : OnePer js) (h : pick js over = some (.start j)) : OnePer (setState .running j js) := by
  obtain ⟨_, he, _⟩ := pick_start h
  have hnr : running js j.project = false := by
    simp [eligible] at he
    simpa using he.1.2
  intro a ha b hb hsa hsb hp
  rcases mem_setState ha with ⟨rfl, _⟩ | ⟨ha', hane⟩ <;> rcases mem_setState hb with ⟨rfl, _⟩ | ⟨hb', hbne⟩
  · rfl
  · exact absurd hsb (not_running_of hnr b hb' (by simpa using hp.symm))
  · exact absurd hsa (not_running_of hnr a ha' (by simpa using hp))
  · exact hinv a ha' b hb' hsa hsb hp

theorem finish_keeps_one_per_project {js : List Job} {j : Job}
    (hinv : OnePer js) : OnePer (setState .done j js) := by
  intro a ha b hb hsa hsb hp
  rcases mem_setState ha with ⟨rfl, _⟩ | ⟨ha', _⟩
  · simp at hsa
  · rcases mem_setState hb with ⟨rfl, _⟩ | ⟨hb', _⟩
    · simp at hsb
    · exact hinv a ha' b hb' hsa hsb hp

-- a busy project does not hold up another; a project over its ceiling is refused
example : pick [⟨1, "a", .running⟩, ⟨2, "a", .queued⟩, ⟨3, "b", .queued⟩] (fun _ => false)
    = some (.start ⟨3, "b", .queued⟩) := by decide
example : pick [⟨1, "a", .running⟩, ⟨2, "a", .queued⟩, ⟨3, "b", .queued⟩] (fun p => p == "b")
    = some (.refuse ⟨3, "b", .queued⟩) := by decide
example : pick [⟨1, "a", .queued⟩, ⟨2, "a", .queued⟩] (fun _ => false) = some (.start ⟨1, "a", .queued⟩) := by decide

end EgmQueue
