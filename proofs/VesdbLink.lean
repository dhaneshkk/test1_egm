/-
  The link between egm-team and Vesdb. Kernel-only: no Mathlib, no imports.
  Checked with Lean 4.31.0 and 4.22.0:

      lean proofs/VesdbLink.lean

  Vesdb proves its own log in its repository (`lean/Merkle.lean`, `lean/Store.lean`):
  `consistency_sound`, `receipt_binding` (a leaf a receipt or checkpoint committed to stays at
  its index in every later tree proven consistent with it), and, for the shared-bucket
  high-availability design, `no_fork`, `views_prefix`, `ack_durable`, `receipt_sound`,
  `idempotent`, `redaction_keeps_proofs`. Those are not repeated here. This file proves what
  egm-team adds on top, over the log as a list of events whose position is the event id:

  * `export_exact`: the auditor's export of a feature (a bundle for the ids of its events)
    holds exactly the events the app replays for that feature, in the same order;
  * `history_prefix`: when the log grows, a feature's history only grows at the end, so a
    phase the app showed is never contradicted by events appearing earlier;
  * `retry_once`: writing the same batch twice under one idempotency key records it once
    (the egm-audit client's retry, against Vesdb's `idempotent`).
-/

namespace VesdbLink

structure Entry where
  subject : String
  kind    : String
  deriving DecidableEq, Repr

abbrev Log := List Entry

/-- `(id, entry)` pairs, ids counting from `start`. -/
def indexed : Nat → List Entry → List (Nat × Entry)
  | _, [] => []
  | n, e :: es => (n, e) :: indexed (n + 1) es

/-- What `read-events` returns for one subject (not recursive): the app replays this. -/
def history (log : Log) (s : String) : List (Nat × Entry) :=
  (indexed 0 log).filter (fun p => p.2.subject == s)

/-- The ids the app puts in an export. -/
def exportIds (log : Log) (s : String) : List Nat := (history log s).map (·.1)

/-- What a bundle for these ids contains. -/
def bundle (log : Log) (ids : List Nat) : List (Nat × Entry) :=
  ids.filterMap (fun i => (log[i]?).map (fun e => (i, e)))

theorem indexed_get (log : Log) (n : Nat) :
    ∀ p ∈ indexed n log, p.1 ≥ n ∧ log[p.1 - n]? = some p.2 := by
  induction log generalizing n with
  | nil => intro p hp; simp [indexed] at hp
  | cons e es ih =>
    intro p hp
    simp only [indexed, List.mem_cons] at hp
    rcases hp with rfl | hp
    · simp
    · obtain ⟨h1, h2⟩ := ih (n + 1) p hp
      refine ⟨by omega, ?_⟩
      have : p.1 - n = (p.1 - (n + 1)) + 1 := by omega
      rw [this]
      simpa using h2

theorem filterMap_self (log : Log) : ∀ l : List (Nat × Entry), (∀ p ∈ l, log[p.1]? = some p.2) →
    l.filterMap (fun p => (log[p.1]?).map (fun e => (p.1, e))) = l
  | [], _ => rfl
  | p :: ps, h => by
    have hp : log[p.1]? = some p.2 := h p (by simp)
    have ih := filterMap_self log ps (fun q hq => h q (by simp [hq]))
    simp [hp, ih]

theorem export_exact (log : Log) (s : String) : bundle log (exportIds log s) = history log s := by
  unfold bundle exportIds
  rw [List.filterMap_map]
  simp only [Function.comp_def]
  apply filterMap_self
  intro p hp
  have hp' : p ∈ indexed 0 log := (List.mem_filter.mp hp).1
  obtain ⟨_, hget⟩ := indexed_get log 0 p hp'
  simpa using hget

theorem indexed_append (a b : List Entry) (n : Nat) :
    indexed n (a ++ b) = indexed n a ++ indexed (n + a.length) b := by
  induction a generalizing n with
  | nil => simp [indexed]
  | cons e es ih => simp [indexed, ih, Nat.add_assoc, Nat.add_comm 1]

theorem history_prefix (log more : Log) (s : String) :
    ∃ t, history (log ++ more) s = history log s ++ t := by
  refine ⟨(indexed log.length more).filter (fun p => p.2.subject == s), ?_⟩
  simp [history, indexed_append, List.filter_append]

/-- An idempotent append: a key seen before returns the earlier answer and adds nothing. -/
def appendOnce (st : Log × List Nat) (key : Nat) (batch : List Entry) : Log × List Nat :=
  if st.2.contains key then st else (st.1 ++ batch, key :: st.2)

theorem retry_once (st : Log × List Nat) (key : Nat) (batch : List Entry) :
    appendOnce (appendOnce st key batch) key batch = appendOnce st key batch := by
  by_cases h : key ∈ st.2
  · simp [appendOnce, h]
  · simp [appendOnce, h]

example : exportIds [⟨"/egm/jot/a", "feature.created"⟩, ⟨"/egm/jot/b", "feature.created"⟩,
    ⟨"/egm/jot/a", "doc.revised"⟩] "/egm/jot/a" = [0, 2] := by decide

end VesdbLink
