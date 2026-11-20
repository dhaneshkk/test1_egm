/-
  The egm feature gate, mirrored line for line by `src/phase.rs`.
  Kernel-only: no Mathlib, no imports. Checked with Lean 4.31.0:

      lean proofs/EgmGate.lean

  Main result (`built_means_goldfish_passed`): after ANY finite sequence of
  events, a feature that is built or hardened was passed by a goldfish
  review on exactly the design-doc revision currently in force.
-/

inductive Phase where
  | designing | ready | built | hardened
  deriving DecidableEq, Repr

structure Gate where
  phase  : Phase
  doc    : Nat            -- hash of the design doc in force
  passed : Option Nat     -- doc hash the last passing goldfish saw
  deriving DecidableEq, Repr

inductive Event where
  | editDoc  (h : Nat)
  | goldfish (ok : Bool)
  | build
  | review   (clean : Bool)

/-- One transition. A refused event leaves the gate unchanged
    (`Gate.step` returns `Err` in Rust and the caller keeps the old gate). -/
def step (g : Gate) : Event → Gate
  | .editDoc h =>
      if h = g.doc then g else ⟨.designing, h, none⟩
  | .goldfish ok =>
      match g.phase with
      | .designing | .ready => if ok then ⟨.ready, g.doc, some g.doc⟩ else ⟨.designing, g.doc, none⟩
      | .built | .hardened => g
  | .build =>
      match g.phase with
      | .ready => { g with phase := .built }
      | _ => g
  | .review clean =>
      match g.phase with
      | .built | .hardened => { g with phase := if clean then .hardened else g.phase }
      | _ => g

def run (g : Gate) : List Event → Gate
  | [] => g
  | e :: es => run (step g e) es

def init (h : Nat) : Gate := ⟨.designing, h, none⟩

/-- The safety invariant, `Gate::invariant` in Rust. -/
def Safe (g : Gate) : Prop := g.phase = .designing ∨ g.passed = some g.doc

theorem init_inv (h : Nat) : Safe (init h) := Or.inl rfl

theorem step_inv (g : Gate) (e : Event) (hg : Safe g) : Safe (step g e) := by
  rcases g with ⟨p, d, pa⟩
  simp only [Safe] at hg ⊢
  cases e with
  | editDoc h => by_cases hh : h = d <;> simp_all [step]
  | goldfish ok => cases p <;> cases ok <;> simp_all [step]
  | build => cases p <;> simp_all [step]
  | review clean => cases p <;> cases clean <;> simp_all [step]

theorem run_inv (g : Gate) (es : List Event) (hg : Safe g) : Safe (run g es) := by
  induction es generalizing g with
  | nil => exact hg
  | cons e es ih => exact ih (step g e) (step_inv g e hg)

/-- Main theorem: built or hardened code always comes from a doc revision
    that a goldfish passed. -/
theorem built_means_goldfish_passed (h : Nat) (es : List Event)
    (hb : (run (init h) es).phase = .built ∨ (run (init h) es).phase = .hardened) :
    (run (init h) es).passed = some (run (init h) es).doc := by
  rcases run_inv (init h) es (init_inv h) with hd | hp
  · rcases hb with hb | hb <;> rw [hd] at hb <;> cases hb
  · exact hp

/-- The build event only ever enters `built` from `ready`. -/
theorem build_enters_only_from_ready (g : Gate)
    (hnot : g.phase ≠ .built) (hb : (step g .build).phase = .built) : g.phase = .ready := by
  rcases g with ⟨p, d, pa⟩
  cases p <;> simp [step] at hb hnot ⊢

/-- Editing the doc after a build reopens the design: the gate must be passed again. -/
theorem edit_reopens (g : Gate) (h : Nat) (hne : h ≠ g.doc) :
    (step g (.editDoc h)).phase = .designing := by
  simp [step, hne]

/-- The goldfish pass rule, `goldfish::passes` in Rust. -/
def verdict (ready : Bool) (questions blocking : Nat) : Bool :=
  ready && questions == 0 && blocking == 0

theorem verdict_blocking_fails (r : Bool) (q b : Nat) : verdict r q (b + 1) = false := by
  simp [verdict]

theorem verdict_question_fails (r : Bool) (q b : Nat) : verdict r (q + 1) b = false := by
  simp [verdict]

theorem verdict_not_ready_fails (q b : Nat) : verdict false q b = false := by
  simp [verdict]

theorem verdict_exact (r : Bool) (q b : Nat) :
    verdict r q b = true ↔ r = true ∧ q = 0 ∧ b = 0 := by
  cases r <;> simp [verdict]

/-! ## False-alarm mitigation (`src/alarm.rs`)

  An alarm blocks the build until it is dropped. Only a model-raised alarm can
  be dropped, and only with a recorded reason (a quote the doc does not
  contain, or a verifier judgment egm could check). egm's own alarms (missing
  sections, criteria without a check) can never be dropped. -/

inductive DropReason where
  | quoteMissing      -- grounding: the alarm quoted text that is not in the doc
  | answeredByDoc     -- verifier quoted the passage that answers it, and egm found it
  | notBlocking       -- verifier gave a reason any reasonable choice is fine
  deriving DecidableEq

structure Alarm where
  fromModel : Bool
  dropped   : Option DropReason

/-- An alarm still blocks unless a model raised it AND it carries a drop reason
    (`Alarm::counts` together with `drop_with`, which ignores egm's own alarms). -/
def Alarm.counts (a : Alarm) : Bool := !(a.fromModel && a.dropped.isSome)

def remaining (as : List Alarm) : List Alarm := as.filter Alarm.counts

/-- Anything mitigation removed was raised by a model and has a recorded reason. -/
theorem dropped_has_reason (as : List Alarm) (a : Alarm) (hin : a ∈ as) (hout : a ∉ remaining as) :
    a.fromModel = true ∧ a.dropped.isSome = true := by
  unfold remaining at hout
  rw [List.mem_filter] at hout
  have hc : a.counts = false := by
    cases h : a.counts
    · rfl
    · exact absurd ⟨hin, h⟩ hout
  unfold Alarm.counts at hc
  cases hm : a.fromModel <;> cases hd : a.dropped.isSome <;> simp_all

/-- egm's own alarms always survive mitigation. -/
theorem egm_alarms_survive (as : List Alarm) (a : Alarm) (hin : a ∈ as) (h : a.fromModel = false) :
    a ∈ remaining as := by
  unfold remaining
  rw [List.mem_filter]
  exact ⟨hin, by simp [Alarm.counts, h]⟩

/-- The goldfish passes after mitigation only if EVERY alarm was raised by a
    model and dropped with a recorded reason. With `verdict_exact`, this is the
    whole pass rule: no open question, no blocking finding, no unexplained
    "not ready", and no silently ignored alarm. -/
theorem pass_after_mitigation (as : List Alarm) (h : remaining as = []) :
    ∀ a ∈ as, a.fromModel = true ∧ a.dropped.isSome = true := by
  intro a hin
  apply dropped_has_reason as a hin
  rw [h]
  exact List.not_mem_nil

/-! ## Run budgets and agent sessions (`src/run.rs`, `src/session.rs`)

  Everything above is unchanged. This section wraps the gate in a run state:
  per-feature budgets and the set of live agent sessions. egm-dsh executes
  `rstep` itself (Rust `RunState::step`) for every activity and session it
  starts. Human edits to the doc are not egm events; they enter through the
  gate's own `editDoc`, which `step_inv` already covers.

  An activity is one command's model work: an elephant turn or draft
  (`redraft` budget), a goldfish round (`goldfish`), a build or test-writing
  run (`build`), a mean review (`review`). Inside it, each agent session costs
  one model call (`calls`). An activity ends with `finish o` (applying gate
  event `o`, if any) only once all its sessions are closed, or with `cancel`.

  Proved below, following the AgentBeacon pattern (an invariant plus a ranking
  function that drops on every accepted step):
  (a) the gate part of any run is `run (init h) (gateEvents es)`, so
      `built_means_goldfish_passed` holds for runs too;
  (b) `rank` strictly decreases on every accepted event, so no infinite run exists;
  (c) a state that accepts no event has no live session and no open activity;
  (d) every unit of budget spent was available: spent + left = initial, exactly. -/

namespace EgmRun

inductive Kind where
  | elephant | goldfish | build | tests | review
  deriving DecidableEq, Repr

inductive Counter where
  | redraft | goldfish | build | review
  deriving DecidableEq, Repr

/-- Which budget an activity draws on. Test-writing runs count as builds. -/
def Kind.counter : Kind → Counter
  | .elephant => .redraft
  | .goldfish => .goldfish
  | .build    => .build
  | .tests    => .build
  | .review   => .review

/-- What is LEFT of each budget (Rust: limit minus spent, saturating). -/
structure Budget where
  redraft  : Nat
  goldfish : Nat
  build    : Nat
  review   : Nat
  calls    : Nat
  deriving DecidableEq, Repr

def Budget.get (b : Budget) : Counter → Nat
  | .redraft  => b.redraft
  | .goldfish => b.goldfish
  | .build    => b.build
  | .review   => b.review

def Budget.spend (b : Budget) : Counter → Budget
  | .redraft  => { b with redraft := b.redraft - 1 }
  | .goldfish => { b with goldfish := b.goldfish - 1 }
  | .build    => { b with build := b.build - 1 }
  | .review   => { b with review := b.review - 1 }

def Budget.total (b : Budget) : Nat := b.redraft + b.goldfish + b.build + b.review

structure RState where
  gate   : Gate
  budget : Budget
  live   : List Nat          -- ids of agent sessions that are open
  next   : Nat               -- next session id
  active : Option Kind       -- the activity in progress, if any

inductive REvent where
  | begin (k : Kind)         -- start an activity: spends one unit of its budget
  | openSession              -- start an agent session: spends one model call
  | close (id : Nat)         -- a session ended (normally, on error, panic or Ctrl-C)
  | finish (o : Option Event)-- the activity ends, applying gate event `o`
  | cancel                   -- the activity ends without a gate event

/-- Which gate event an activity may produce. `none` is always allowed:
    an incomplete review, a rejected build, a conversation turn. -/
def fits : Kind → Option Event → Bool
  | _,         none                 => true
  | .elephant, some (.editDoc _)    => true
  | .goldfish, some (.goldfish _)   => true
  | .build,    some .build          => true
  | .build,    some (.goldfish false) => true   -- the builder found a decision the doc left open
  | .review,   some (.review _)     => true
  | _,         _                    => false

def applyO (g : Gate) : Option Event → Gate
  | none   => g
  | some e => step g e

/-- One run transition: `some` if accepted, `none` if refused (state unchanged). -/
def rstep (s : RState) : REvent → Option RState
  | .begin k =>
      if s.active = none ∧ 0 < s.budget.get k.counter then
        some { s with budget := s.budget.spend k.counter, active := some k }
      else none
  | .openSession =>
      if s.active ≠ none ∧ 0 < s.budget.calls then
        some { s with budget := { s.budget with calls := s.budget.calls - 1 },
                      live := s.next :: s.live, next := s.next + 1 }
      else none
  | .close id =>
      if id ∈ s.live then some { s with live := s.live.erase id } else none
  | .finish o =>
      match s.active with
      | some k => if s.live = [] ∧ fits k o = true then
                    some { s with active := none, gate := applyO s.gate o }
                  else none
      | none => none
  | .cancel =>
      if s.active ≠ none ∧ s.live = [] then some { s with active := none } else none

def rrun (s : RState) : List REvent → Option RState
  | [] => some s
  | e :: es => match rstep s e with
    | some s' => rrun s' es
    | none => none

def rinit (h : Nat) (b : Budget) : RState := ⟨init h, b, [], 0, none⟩

/-- The gate events a run applied, in order. -/
def gateEvents : List REvent → List Event
  | [] => []
  | .finish (some e) :: es => e :: gateEvents es
  | _ :: es => gateEvents es

/-! ### (a) The gate inside a run is the gate -/

theorem rstep_gate (s s' : RState) (e : REvent) (h : rstep s e = some s') :
    s'.gate = run s.gate (gateEvents [e]) := by
  cases e with
  | begin k =>
    simp only [rstep] at h; split at h
    · injection h with h; subst h; rfl
    · cases h
  | openSession =>
    simp only [rstep] at h; split at h
    · injection h with h; subst h; rfl
    · cases h
  | close id =>
    simp only [rstep] at h; split at h
    · injection h with h; subst h; rfl
    · cases h
  | finish o =>
    simp only [rstep] at h
    cases ha : s.active with
    | none => rw [ha] at h; cases h
    | some k =>
      rw [ha] at h; simp only at h; split at h
      · injection h with h; subst h
        cases o with
        | none => rfl
        | some ev => rfl
      · cases h
  | cancel =>
    simp only [rstep] at h; split at h
    · injection h with h; subst h; rfl
    · cases h

theorem rrun_gate (s s' : RState) (es : List REvent) (h : rrun s es = some s') :
    s'.gate = run s.gate (gateEvents es) := by
  induction es generalizing s with
  | nil => simp [rrun] at h; subst h; rfl
  | cons e es ih =>
    simp only [rrun] at h
    cases hs : rstep s e with
    | none => rw [hs] at h; cases h
    | some s1 =>
      rw [hs] at h
      have g1 := rstep_gate s s1 e hs
      rw [ih s1 h, g1]
      cases e with
      | finish o => cases o <;> rfl
      | _ => rfl

/-- (a) for runs: built or hardened code came from a doc revision a goldfish passed. -/
theorem run_built_means_goldfish_passed (h : Nat) (b : Budget) (es : List REvent) (s' : RState)
    (hr : rrun (rinit h b) es = some s')
    (hb : s'.gate.phase = .built ∨ s'.gate.phase = .hardened) :
    s'.gate.passed = some s'.gate.doc := by
  have hg : s'.gate = run (init h) (gateEvents es) := rrun_gate _ _ es hr
  rw [hg] at hb ⊢
  exact built_means_goldfish_passed h (gateEvents es) hb

/-! ### (b) Termination: a ranking function that drops on every accepted event -/

def rank (s : RState) : Nat :=
  4 * s.budget.total + 2 * s.budget.calls + s.live.length + (if s.active = none then 0 else 1)

theorem spend_total (b : Budget) (c : Counter) (h : 0 < b.get c) : (b.spend c).total + 1 = b.total := by
  cases c <;> simp [Budget.spend, Budget.total, Budget.get] at * <;> omega

theorem spend_calls (b : Budget) (c : Counter) : (b.spend c).calls = b.calls := by
  cases c <;> rfl

theorem rank_decreases (s s' : RState) (e : REvent) (h : rstep s e = some s') : rank s' < rank s := by
  cases e with
  | begin k =>
    simp only [rstep] at h; split at h
    · rename_i hc
      injection h with h; subst h
      have ht := spend_total s.budget k.counter hc.2
      have hcl := spend_calls s.budget k.counter
      simp only [rank, hc.1, hcl]
      simp
      omega
    · cases h
  | openSession =>
    simp only [rstep] at h; split at h
    · rename_i hc
      injection h with h; subst h
      simp only [rank, Budget.total, List.length_cons]
      have := hc.2
      split <;> omega
    · cases h
  | close id =>
    simp only [rstep] at h; split at h
    · rename_i hm
      injection h with h; subst h
      have hl := List.length_erase_of_mem hm
      have hp := List.length_pos_of_mem hm
      simp only [rank]
      omega
    · cases h
  | finish o =>
    simp only [rstep] at h
    cases ha : s.active with
    | none => rw [ha] at h; cases h
    | some k =>
      rw [ha] at h; simp only at h; split at h
      · injection h with h; subst h
        simp [rank, ha]
      · cases h
  | cancel =>
    simp only [rstep] at h; split at h
    · rename_i hc
      injection h with h; subst h
      simp [rank, hc.1]
    · cases h

/-- Every accepted run is at most `rank` long. -/
theorem rrun_bounded (s s' : RState) (es : List REvent) (h : rrun s es = some s') :
    es.length + rank s' ≤ rank s := by
  induction es generalizing s with
  | nil => simp [rrun] at h; subst h; simp
  | cons e es ih =>
    simp only [rrun] at h
    cases hs : rstep s e with
    | none => rw [hs] at h; cases h
    | some s1 =>
      rw [hs] at h
      have h1 := ih s1 h
      have h2 := rank_decreases s s1 e hs
      simp only [List.length_cons]
      omega

/-- (b) There is no infinite sequence of accepted events. -/
theorem no_infinite_run (f : Nat → RState) (es : Nat → REvent)
    (h : ∀ n, rstep (f n) (es n) = some (f (n + 1))) : False := by
  have key : ∀ n, rank (f n) + n ≤ rank (f 0) := by
    intro n
    induction n with
    | zero => simp
    | succ n ih => have := rank_decreases _ _ _ (h n); omega
  have := key (rank (f 0) + 1)
  omega

/-! ### (c) Nothing left running when nothing can happen -/

def Stuck (s : RState) : Prop := ∀ e, rstep s e = none

theorem stuck_no_live (s : RState) (h : Stuck s) : s.live = [] ∧ s.active = none := by
  cases hl : s.live with
  | cons id t =>
    have := h (.close id)
    simp [rstep, hl] at this
  | nil =>
    refine ⟨rfl, ?_⟩
    cases ha : s.active with
    | none => rfl
    | some k =>
      have := h .cancel
      simp [rstep, ha, hl] at this

/-- Sessions can always be closed: from any state, closing the live sessions
    one by one is accepted, and then the activity can be cancelled. -/
theorem close_accepted (s : RState) (id : Nat) (hm : id ∈ s.live) : (rstep s (.close id)).isSome := by
  simp [rstep, hm]

/-! ### (d) Budgets: spent plus left equals the initial budget, exactly -/

def charges (c : Counter) : REvent → Bool
  | .begin k => k.counter == c
  | _ => false

def isOpen : REvent → Bool
  | .openSession => true
  | _ => false

theorem spend_get (b : Budget) (c c' : Counter) :
    (b.spend c').get c = if c' = c then b.get c - 1 else b.get c := by
  cases c <;> cases c' <;> rfl

theorem rstep_budget (s s' : RState) (e : REvent) (c : Counter) (h : rstep s e = some s') :
    (if charges c e then 1 else 0) + s'.budget.get c = s.budget.get c ∧
    (if isOpen e then 1 else 0) + s'.budget.calls = s.budget.calls := by
  cases e with
  | begin k =>
    simp only [rstep] at h; split at h
    · rename_i hc
      injection h with h; subst h
      refine ⟨?_, ?_⟩
      · simp only [charges, spend_get]
        by_cases hk : k.counter = c
        · subst hk; simp; have := hc.2; omega
        · simp [hk]
      · simp [isOpen, spend_calls]
    · cases h
  | openSession =>
    simp only [rstep] at h; split at h
    · rename_i hc
      injection h with h; subst h
      refine ⟨?_, ?_⟩
      · cases c <;> simp [charges, Budget.get]
      · simp [isOpen]; have := hc.2; omega
    · cases h
  | close id =>
    simp only [rstep] at h; split at h
    · injection h with h; subst h; simp [charges, isOpen]
    · cases h
  | finish o =>
    simp only [rstep] at h
    cases ha : s.active with
    | none => rw [ha] at h; cases h
    | some k =>
      rw [ha] at h; simp only at h; split at h
      · injection h with h; subst h; simp [charges, isOpen]
      · cases h
  | cancel =>
    simp only [rstep] at h; split at h
    · injection h with h; subst h; simp [charges, isOpen]
    · cases h

/-- (d) Over any accepted run: activities begun on counter `c`, plus what is
    left of `c`, equals what there was. So egm never begins more activities than
    the budget allows; a budget cannot go "below zero". The same for model calls. -/
theorem budget_accounting (s s' : RState) (es : List REvent) (c : Counter) (h : rrun s es = some s') :
    es.countP (charges c) + s'.budget.get c = s.budget.get c ∧
    es.countP isOpen + s'.budget.calls = s.budget.calls := by
  induction es generalizing s with
  | nil => simp [rrun] at h; subst h; simp
  | cons e es ih =>
    simp only [rrun] at h
    cases hs : rstep s e with
    | none => rw [hs] at h; cases h
    | some s1 =>
      rw [hs] at h
      obtain ⟨i1, i2⟩ := ih s1 h
      obtain ⟨j1, j2⟩ := rstep_budget s s1 e c hs
      simp only [List.countP_cons]
      constructor <;> split <;> simp_all <;> omega

theorem activities_within_budget (s s' : RState) (es : List REvent) (c : Counter) (h : rrun s es = some s') :
    es.countP (charges c) ≤ s.budget.get c := by
  have := (budget_accounting s s' es c h).1; omega

theorem sessions_within_budget (s s' : RState) (es : List REvent) (h : rrun s es = some s') :
    es.countP isOpen ≤ s.budget.calls := by
  have := (budget_accounting s s' es .redraft h).2; omega

end EgmRun

/-! # Structured escalation (`src/escalate.rs`, `egm-dsh escalate | decide | redraft`)

  After a not-ready verdict, each open question is answered from the repository or
  escalated to a person. Three facts carry the design:

  1. Only an answer egm-dsh grounded (quote found in the cited file) is accepted
     without a person; every other question becomes a decision with at least one
     labelled option (`classify`).
  2. A redraft is refused while any decision is open (`canRedraft`).
  3. Escalation and decisions are not gate events. After the not-ready verdict the
     gate is in design, and no sequence of events without a fresh passing goldfish
     reaches `built` — whatever the redraft wrote, even if it changed nothing.
-/
namespace EgmEscalate

/-- What the elephant replied to one question. -/
inductive Reply where
  | none                      -- did not address it
  | answer (grounded : Bool)  -- answered from the repository; egm-dsh checked the quote
  | options (n : Nat)         -- escalated with n options of its own

/-- What egm-dsh makes of it. `pending k`: a person must choose among k labelled options. -/
inductive Res where
  | answered
  | pending (k : Nat)
  | decided

/-- `escalate::classify`: at most 4 elephant options, plus egm-dsh's own `Other`;
    an unconfirmed answer becomes an option itself. -/
def classify : Reply → Res
  | .none => .pending 1
  | .answer true => .answered
  | .answer false => .pending 2
  | .options n => .pending (min n 4 + 1)

theorem answered_only_if_grounded (r : Reply) (h : classify r = .answered) : r = .answer true := by
  cases r with
  | none => simp [classify] at h
  | answer g => cases g <;> simp_all [classify]
  | options n => simp [classify] at h

/-- Nothing is dropped: every question is answered from the repository or goes to a person. -/
theorem every_question_resolved_or_escalated (r : Reply) :
    classify r = .answered ∨ ∃ k, classify r = .pending k ∧ 1 ≤ k := by
  cases r with
  | none => exact Or.inr ⟨1, rfl, Nat.le_refl 1⟩
  | answer g => cases g
                · exact Or.inr ⟨2, rfl, by decide⟩
                · exact Or.inl rfl
  | options n => exact Or.inr ⟨min n 4 + 1, rfl, by omega⟩

theorem options_bounded (r : Reply) (k : Nat) (h : classify r = .pending k) : 1 ≤ k ∧ k ≤ 5 := by
  cases r with
  | none => simp [classify] at h; omega
  | answer g => cases g <;> simp [classify] at h; omega
  | options n => simp [classify] at h; omega

def isPending : Res → Bool
  | .pending _ => true
  | _ => false

/-- `escalate::can_redraft`. -/
def canRedraft (items : List Res) : Bool := items.all (fun r => !isPending r)

theorem redraft_needs_every_decision (items : List Res) (h : canRedraft items = true) :
    ∀ r ∈ items, ∀ k, r ≠ .pending k := by
  intro r hr k he
  have := (List.all_eq_true.mp h) r hr
  subst he; simp [isPending] at this

/-- A person deciding (`decide`) only turns a pending item into a decided one. -/
def decide : Res → Res
  | .pending _ => .decided
  | r => r

/-- Deciding settles a pending item, and leaves an answered one as it was:
    a person's choice never counts as a repository answer. -/
theorem decide_settles (k : Nat) : decide (.pending k) = .decided := rfl

theorem decide_answered_iff (r : Res) : decide r = .answered ↔ r = .answered := by
  cases r <;> simp [decide]

/-- Once every escalated item is decided, the redraft is allowed. -/
theorem redraft_after_deciding_all (items : List Res) : canRedraft (items.map decide) = true := by
  simp only [canRedraft, List.all_eq_true, List.mem_map]
  rintro _ ⟨r, _, rfl⟩
  cases r <;> simp [decide, isPending]

/-! ## The gate does not weaken -/

/-- From design, the gate stays in design until a goldfish passes. -/
theorem designing_until_pass (g : Gate) (es : List Event) (hg : g.phase = .designing)
    (hno : Event.goldfish true ∉ es) : (run g es).phase = .designing := by
  induction es generalizing g with
  | nil => exact hg
  | cons e es ih =>
    have hno' : Event.goldfish true ∉ es := fun h => hno (List.mem_cons_of_mem _ h)
    have he : e ≠ Event.goldfish true := fun h => hno (h ▸ List.mem_cons_self ..)
    apply ih _ _ hno'
    rcases g with ⟨p, d, pa⟩
    simp only at hg; subst hg
    cases e with
    | editDoc h => by_cases hh : h = d <;> simp [step, hh]
    | goldfish ok => cases ok
                     · simp [step]
                     · exact absurd rfl he
    | build => simp [step]
    | review c => simp [step]

/-- A doc edit made in design leaves the gate in design (changed or unchanged hash). -/
theorem edit_keeps_designing (g : Gate) (h : Nat) (hg : g.phase = .designing) :
    (step g (.editDoc h)).phase = .designing := by
  simp only [step]; split
  · exact hg
  · rfl

/-- The main property. A goldfish said not ready (from design or ready); then any
    events — the escalation and decisions add none, the redraft adds `editDoc h`
    for whatever the doc now hashes to — reach `built` or `hardened` only if a
    goldfish passes again after the redraft. -/
theorem escalation_needs_fresh_pass (g : Gate) (h : Nat) (es : List Event)
    (hg : g.phase = .designing ∨ g.phase = .ready)
    (hb : (run (step (step g (.goldfish false)) (.editDoc h)) es).phase = .built ∨
          (run (step (step g (.goldfish false)) (.editDoc h)) es).phase = .hardened) :
    Event.goldfish true ∈ es := by
  have h1 : (step g (.goldfish false)).phase = .designing := by
    rcases g with ⟨p, d, pa⟩
    rcases hg with hg | hg <;> simp only at hg <;> subst hg <;> simp [step]
  have h2 := edit_keeps_designing _ h h1
  refine Classical.byContradiction fun hno => ?_
  have := designing_until_pass _ es h2 hno
  rcases hb with hb | hb <;> rw [this] at hb <;> cases hb

/-- And when it is built, the passing goldfish saw exactly the doc in force. -/
theorem escalation_build_saw_doc (h0 : Nat) (pre : List Event) (h : Nat) (es : List Event)
    (hb : (run (init h0) (pre ++ [.goldfish false, .editDoc h] ++ es)).phase = .built) :
    (run (init h0) (pre ++ [.goldfish false, .editDoc h] ++ es)).passed =
      some (run (init h0) (pre ++ [.goldfish false, .editDoc h] ++ es)).doc :=
  built_means_goldfish_passed h0 _ (Or.inl hb)

end EgmEscalate

/-! # The goldfish reading room (`src/stage.rs`)

  Every goldfish and verifier session runs in a stage holding only the doc and its
  links, and every tool call it makes is audited. Fail closed: a review counts only
  if every session stayed inside; otherwise it is incomplete (no gate event at all),
  and a verifier that left drops no alarm.
-/
namespace EgmRoom

structure Touch where
  tool   : String
  inside : Bool        -- the path resolves inside the stage (or names none)

def readTools : List String := ["read", "read_image", "glob", "grep", "working_directory", "todo_write"]

/-- `stage::escape` returns `None` for this touch. -/
def allowed (t : Touch) : Bool := readTools.contains t.tool && t.inside

/-- A session's answer is used only if every tool call was allowed. -/
def kept (s : List Touch) : Bool := s.all allowed

/-- The review's gate outcome: `none` = incomplete (no gate event), else the verdict. -/
def outcome (sessions : List (List Touch)) (ready : Bool) : Option Bool :=
  if sessions.all kept then some ready else none

/-- A pass means every reviewer of that review stayed in the reading room. -/
theorem pass_means_room_kept (ss : List (List Touch)) (r : Bool) (h : outcome ss r = some true) :
    ∀ s ∈ ss, ∀ t ∈ s, readTools.contains t.tool = true ∧ t.inside = true := by
  unfold outcome at h
  split at h
  · rename_i hall
    intro s hs t ht
    have hk := List.all_eq_true.mp hall s hs
    have ha := List.all_eq_true.mp hk t ht
    simp only [allowed, Bool.and_eq_true] at ha
    exact ha
  · simp at h

/-- One call outside the room by any reviewer makes the review incomplete. -/
theorem one_escape_blocks (ss : List (List Touch)) (r : Bool) (s : List Touch) (t : Touch)
    (hs : s ∈ ss) (ht : t ∈ s) (hb : allowed t = false) : outcome ss r = none := by
  unfold outcome
  have hk : kept s = false := by
    unfold kept; rw [List.all_eq_false]; exact ⟨t, ht, by simp [hb]⟩
  have : ss.all kept = false := by
    rw [List.all_eq_false]; exact ⟨s, hs, by simp [hk]⟩
  simp [this]

/-- So an escaped review never produces `goldfish true`: the gate cannot open on it. -/
theorem escape_never_opens_gate (ss : List (List Touch)) (r : Bool) (s : List Touch) (t : Touch)
    (hs : s ∈ ss) (ht : t ∈ s) (hb : allowed t = false) : outcome ss r ≠ some true := by
  rw [one_escape_blocks ss r s t hs ht hb]; simp

/-- The verifier's drops are applied only if its session was kept. -/
def drops (verifier : List Touch) (judged : List Nat) : List Nat :=
  if kept verifier then judged else []

theorem escaped_verifier_drops_nothing (v : List Touch) (js : List Nat) (t : Touch)
    (ht : t ∈ v) (hb : allowed t = false) : drops v js = [] := by
  unfold drops
  have : kept v = false := by unfold kept; rw [List.all_eq_false]; exact ⟨t, ht, by simp [hb]⟩
  simp [this]

example : allowed ⟨"bash", true⟩ = false := by decide
example : allowed ⟨"read", false⟩ = false := by decide
example : allowed ⟨"read", true⟩ = true := by decide

end EgmRoom

/-! # The builder as a goldfish, the Verification checklist, and `egm-dsh loop`

  * Executor (`commands::build`): the builder lists every decision the doc left open.
    A blocking one rejects the phase and reports `goldfish false`: the design reopens,
    and only a later passing goldfish (on a doc that now decides it) unlocks the build.
  * Verification (`verify.rs`): each required checklist item missing from the doc is an
    egm-dsh alarm, which no model can drop, so the doc cannot pass while one is missing.
  * Loop (`commands::loop_`): rounds of goldfish → escalate → redraft. It redrafts on its
    own only when every question was answered from the repository (egm-dsh checked the
    quote); any decision stops it for a person. It also stops when findings stall.
-/
namespace EgmExecutor
open EgmEscalate

/-- What a build attempt reports to the gate (`fits .build`): built, or the doc reopened. -/
def buildOutcome (blockingAssumptions : Nat) (checksPass : Bool) : Option Event :=
  if blockingAssumptions > 0 then some (.goldfish false)
  else if checksPass then some .build
  else none

theorem blocking_assumption_never_builds (n : Nat) (c : Bool) (h : 0 < n) :
    buildOutcome n c = some (.goldfish false) := by
  unfold buildOutcome; simp [h]

/-- Every outcome a build reports is one a build activity may produce (`EgmRun.fits`). -/
theorem outcome_fits (n : Nat) (c : Bool) : EgmRun.fits .build (buildOutcome n c) = true := by
  unfold buildOutcome; split
  · rfl
  · split <;> rfl

/-- After the builder reports a blocking assumption, whatever happens next reaches
    `built` only through a later passing goldfish. -/
theorem assumption_needs_fresh_pass (g : Gate) (es : List Event)
    (hg : g.phase = .designing ∨ g.phase = .ready)
    (hb : (run (step g (.goldfish false)) es).phase = .built ∨ (run (step g (.goldfish false)) es).phase = .hardened) :
    Event.goldfish true ∈ es := by
  have h1 : (step g (.goldfish false)).phase = .designing := by
    rcases g with ⟨p, d, pa⟩
    rcases hg with hg | hg <;> simp only at hg <;> subst hg <;> simp [step]
  refine Classical.byContradiction fun hno => ?_
  have := designing_until_pass _ es h1 hno
  rcases hb with hb | hb <;> rw [this] at hb <;> cases hb

end EgmExecutor

namespace EgmVerify

inductive Item where
  | lean | mcc | sam | chat
  deriving DecidableEq

/-- `verify::gaps`: the required items the doc does not define. -/
def gaps (required : List Item) (present : Item → Bool) : List Item :=
  required.filter (fun i => !present i)

/-- Each gap is an egm-dsh question, on top of whatever the models raise. -/
def questions (required : List Item) (present : Item → Bool) (modelQuestions : Nat) : Nat :=
  (gaps required present).length + modelQuestions

theorem missing_item_fails (required : List Item) (present : Item → Bool) (i : Item)
    (hi : i ∈ required) (hp : present i = false) (r : Bool) (q b : Nat) :
    verdict r (questions required present q) b = false := by
  have : 0 < (gaps required present).length := by
    apply List.length_pos_of_mem
    exact List.mem_filter.mpr ⟨hi, by simp [hp]⟩
  obtain ⟨k, hk⟩ : ∃ k, questions required present q = k + 1 := ⟨(gaps required present).length + q - 1, by unfold questions; omega⟩
  rw [hk]; exact verdict_question_fails r k b

/-- With nothing required, the checklist adds nothing: projects that do not use it are unchanged. -/
theorem empty_checklist_adds_nothing (present : Item → Bool) (q : Nat) : questions [] present q = q := by
  simp [questions, gaps]

end EgmVerify

namespace EgmLoop
open EgmEscalate

/-- `loop`'s stall rule: the open-finding count has not fallen over the last `k` rounds. -/
def stalled (history : List Nat) (k : Nat) : Bool :=
  match history.reverse with
  | [] => false
  | last :: older => k > 0 && decide (older.length ≥ k) && decide (last ≥ older.getD (k - 1) 0)

/-- The loop redrafts on its own only when every item was answered from the repository. -/
def autoRedraft (items : List Res) : Bool :=
  items.all (fun r => match r with | .answered => true | _ => false)

theorem auto_redraft_decides_nothing (items : List Res) (h : autoRedraft items = true) :
    ∀ r ∈ items, r = .answered := by
  intro r hr
  have := List.all_eq_true.mp h r hr
  cases r <;> simp_all

/-- So any question that needs a person (pending) stops the loop. -/
theorem pending_stops_loop (items : List Res) (k : Nat) (hk : Res.pending k ∈ items) :
    autoRedraft items = false := by
  unfold autoRedraft
  rw [List.all_eq_false]
  exact ⟨_, hk, by simp⟩

/-- A loop of at most `n` rounds runs at most `n` goldfish reviews. -/
def rounds (n : Nat) (stop : Nat → Bool) : Nat :=
  match n with
  | 0 => 0
  | m + 1 => if stop m then 1 else 1 + rounds m stop

theorem loop_bounded (n : Nat) (stop : Nat → Bool) : rounds n stop ≤ n := by
  induction n with
  | zero => simp [rounds]
  | succ m ih => unfold rounds; split <;> omega

example : stalled [5, 4, 4, 4] 2 = true := by decide
example : stalled [5, 4, 3] 2 = false := by decide
example : stalled [5] 2 = false := by decide

end EgmLoop

/-! ## Fail-fast goldfish (`goldfish --fail-fast`, the default in `loop`)

The stage answers of one review, abstracted to what the verdict reads: how many
alarms of each kind still count after grounding and the verifier, and whether a
stage that ran failed. egm-dsh's own alarms (`egm`) need no model and no model can
drop them. Fail-fast checks `egm` first (no session), then readiness alone, and
runs the other stages only if both are clear. It is the same decision as the full
review: READY exactly when the full review is READY, so the gate is unchanged;
it never starts more sessions, and a precheck gap starts none. -/
namespace EgmFailFast

structure Answers where
  egm : Nat
  readinessFailed : Bool
  readiness : Nat
  othersFailed : Bool
  others : Nat

inductive V | ready | notReady | incomplete
  deriving DecidableEq, Repr

/-- `goldfish::verdict` with nothing skipped. -/
def full (a : Answers) : V :=
  if a.readinessFailed || a.othersFailed then .incomplete
  else if a.egm + a.readiness + a.others = 0 then .ready else .notReady

/-- `goldfish::review` with `fail_fast`. -/
def failFast (a : Answers) : V :=
  if a.egm ≠ 0 then .notReady
  else if a.readinessFailed then .incomplete
  else if a.readiness ≠ 0 then .notReady
  else full a

/-- Stage sessions started (the verifier is counted in `EgmCeiling`'s estimates). -/
def fullSessions : Nat := 5
def ffSessions (a : Answers) : Nat :=
  if a.egm ≠ 0 then 0 else if a.readinessFailed || a.readiness ≠ 0 then 1 else 5

theorem ready_iff (a : Answers) : failFast a = .ready ↔ full a = .ready := by
  unfold failFast full
  by_cases h1 : a.egm = 0 <;> by_cases h2 : a.readinessFailed = true <;>
    by_cases h3 : a.readiness = 0 <;> by_cases h4 : a.othersFailed = true <;>
    by_cases h5 : a.others = 0 <;> simp_all

/-- A fail-fast NOT READY or INCOMPLETE is never a doc the full review would pass. -/
theorem stop_sound (a : Answers) (h : failFast a ≠ .ready) : full a ≠ .ready :=
  fun hf => h ((ready_iff a).mpr hf)

theorem never_more_sessions (a : Answers) : ffSessions a ≤ fullSessions := by
  unfold ffSessions fullSessions; split <;> (try split) <;> omega

theorem precheck_is_free (a : Answers) (h : a.egm ≠ 0) : ffSessions a = 0 ∧ failFast a = .notReady := by
  simp [ffSessions, failFast, h]

/-- With stages skipped, `goldfish::verdict` is never READY. -/
def verdictSkipped (failed pass : Bool) (skipped : Nat) : V :=
  if failed then .incomplete else if pass && skipped == 0 then .ready else .notReady

theorem skipped_never_ready (failed pass : Bool) (s : Nat) (h : s ≠ 0) : verdictSkipped failed pass s ≠ .ready := by
  unfold verdictSkipped; split
  · simp
  · split <;> simp_all

end EgmFailFast

/-! ## Token ceiling (`set-ceiling`, checked before every goldfish round)

A step runs only when what the feature has spent plus the step's estimate fits
the ceiling (`estimate::admit`). Over any sequence of steps, the spend stays within
the ceiling plus the largest amount by which one admitted step overran its
estimate; when every estimate is an upper bound, it never crosses the ceiling. -/
namespace EgmCeiling

def admit (spent est ceil : Nat) : Bool := spent + est ≤ ceil

/-- Steps as (estimate, actual); refused steps spend nothing. -/
def run (ceil : Nat) : Nat → List (Nat × Nat) → Nat
  | spent, [] => spent
  | spent, (e, a) :: rest => if admit spent e ceil then run ceil (spent + a) rest else run ceil spent rest

theorem admitted_fits (spent est ceil : Nat) (h : admit spent est ceil = true) : spent + est ≤ ceil := by
  simpa [admit] using h

/-- Never more than the ceiling plus the worst overrun `err` of a step. -/
theorem within_ceiling (ceil err : Nat) (steps : List (Nat × Nat)) (herr : ∀ s ∈ steps, s.2 ≤ s.1 + err) :
    ∀ spent, spent ≤ ceil + err → run ceil spent steps ≤ ceil + err := by
  induction steps with
  | nil => intro spent h; simpa [run] using h
  | cons s rest ih =>
    intro spent h
    obtain ⟨e, a⟩ := s
    have hs := herr (e, a) (by simp)
    have hr : ∀ x ∈ rest, x.2 ≤ x.1 + err := fun x hx => herr x (by simp [hx])
    simp only [run]
    split
    · rename_i hadm
      have := admitted_fits spent e ceil hadm
      exact ih hr _ (by simp at hs; omega)
    · exact ih hr _ h

/-- Upper-bound estimates: the ceiling is never crossed. -/
theorem never_crossed (ceil : Nat) (steps : List (Nat × Nat)) (h : ∀ s ∈ steps, s.2 ≤ s.1) (spent : Nat) (hs : spent ≤ ceil) :
    run ceil spent steps ≤ ceil := by
  have := within_ceiling ceil 0 steps (fun s hm => by simpa using h s hm) spent (by omega)
  simpa using this

/-- Once the ceiling is reached, only a step that calls no model can still run. -/
theorem at_ceiling_only_free (spent est ceil : Nat) (h : ceil ≤ spent) (hadm : admit spent est ceil = true) : est = 0 := by
  have := admitted_fits spent est ceil hadm; omega

example : run 100 0 [(40, 50), (40, 30), (40, 10)] = 80 := by decide

end EgmCeiling

/-! ## The elephant's view (`.egmignore`, `set-scope`; scope.rs)

A view holds the repository files that are in scope and not ignored, plus the
forced files (the feature's doc, its links and map files, earlier steps' docs),
and never egm-dsh or VCS state. Ignoring more never adds a file; a forced file
is always there. The view only changes what the elephant reads: no gate rule
mentions it. -/
namespace EgmView

structure F where
  id : Nat
  state : Bool
  inScope : Bool
  deriving DecidableEq

def view (ign : F → Bool) (all forced : List F) : List F :=
  (all.filter (fun f => !f.state && f.inScope && !ign f)) ++ forced.filter (fun f => !f.state)

theorem state_never_in_view (ign : F → Bool) (all forced : List F) (f : F) (h : f ∈ view ign all forced) :
    f.state = false := by
  unfold view at h
  rcases List.mem_append.mp h with h | h
  · have h2 := (List.mem_filter.mp h).2
    cases hs : f.state <;> simp_all
  · have h2 := (List.mem_filter.mp h).2
    cases hs : f.state <;> simp_all

theorem forced_in_view (ign : F → Bool) (all forced : List F) (f : F) (hf : f ∈ forced) (hs : f.state = false) :
    f ∈ view ign all forced := by
  unfold view
  exact List.mem_append_right _ (List.mem_filter.mpr ⟨hf, by simp [hs]⟩)

/-- An ignored or out-of-scope file is in the view only because it was forced. -/
theorem ignored_only_if_forced (ign : F → Bool) (all forced : List F) (f : F) (h : f ∈ view ign all forced)
    (hi : ign f = true ∨ f.inScope = false) : f ∈ forced := by
  unfold view at h
  rcases List.mem_append.mp h with h | h
  · have h2 := (List.mem_filter.mp h).2
    exfalso
    cases hs : f.state <;> cases hsc : f.inScope <;> cases hg : ign f <;> simp_all
  · exact (List.mem_filter.mp h).1

/-- Ignoring more never adds a file to the view. -/
theorem more_ignores_smaller (ign₁ ign₂ : F → Bool) (hmore : ∀ f, ign₁ f = true → ign₂ f = true)
    (all forced : List F) (f : F) (h : f ∈ view ign₂ all forced) : f ∈ view ign₁ all forced := by
  unfold view at h ⊢
  rcases List.mem_append.mp h with h | h
  · apply List.mem_append_left
    have ⟨ha, h2⟩ := List.mem_filter.mp h
    apply List.mem_filter.mpr ⟨ha, ?_⟩
    have hm := hmore f
    cases hs : f.state <;> cases hsc : f.inScope <;> cases g1 : ign₁ f <;> cases g2 : ign₂ f <;> simp_all
  · exact List.mem_append_right _ h

end EgmView

/-! ## The fix lane (`egm-dsh fix`; fix.rs)

A tiny fix goes through the same gate with the same events; each phase is
cut down (one combined goldfish, one builder, one diff reviewer). A fix build
ends in one of three ways: built (red before, green after, small, nothing left
for the builder to decide), promoted (too big, or a decision was needed: the
change is reverted, `goldfish false` reopens the design and the fix leaves the
lane) or rejected (reverted, still ready). Being events of the ordinary gate,
every fix-lane trace keeps `Safe`, so built or hardened fix code always comes
from a doc revision a goldfish passed. -/
namespace EgmFix

inductive Outcome | built | promoted | rejected
  deriving DecidableEq, Repr

def outcome (red green small blocking : Bool) : Outcome :=
  if blocking || !small then .promoted else if red && green then .built else .rejected

/-- The gate event a fix build reports (`none`: the attempt ends with no gate change). -/
def event : Outcome → Option Event
  | .built => some .build
  | .promoted => some (.goldfish false)
  | .rejected => none

theorem built_iff (r g s b : Bool) : outcome r g s b = .built ↔ r = true ∧ g = true ∧ s = true ∧ b = false := by
  cases r <;> cases g <;> cases s <;> cases b <;> decide

theorem promoted_iff (r g s b : Bool) : outcome r g s b = .promoted ↔ b = true ∨ s = false := by
  cases r <;> cases g <;> cases s <;> cases b <;> decide

/-- Too big is never built, whatever the tests say. -/
theorem too_big_never_built (r g b : Bool) : outcome r g false b ≠ .built := by
  cases r <;> cases g <;> cases b <;> decide

/-- A promoted fix leaves the gate designing: only a fresh passing goldfish can reopen the build. -/
theorem promoted_reopens (gt : Gate) (h : gt.phase = .ready) : (step gt (.goldfish false)).phase = .designing := by
  rcases gt with ⟨p, d, pa⟩; simp at h; subst h; simp [step]

/-- Hardened needs a built fix and a clean diff review on top. -/
theorem hardened_needs_clean_review (gt : Gate) (h : gt.phase = .built) (clean : Bool) :
    (step gt (.review clean)).phase = .hardened ↔ clean = true := by
  rcases gt with ⟨p, d, pa⟩; simp at h; subst h; cases clean <;> simp [step]

/-- The fix lane only emits gate events, so the gate's theorem covers it:
    built or hardened fix code comes from a doc revision a goldfish passed. -/
theorem fix_built_means_goldfish_passed (h : Nat) (es : List Event)
    (hb : (run (init h) es).phase = .built ∨ (run (init h) es).phase = .hardened) :
    (run (init h) es).passed = some (run (init h) es).doc :=
  built_means_goldfish_passed h es hb

end EgmFix

/-! ## Diagram nesting (`diagram::check`)

The structure check reads a diagram's lines as tokens: an opener (`alt`, `opt`,
`loop`, …), `end`, or anything else. `go` tracks the open depth and fails on an
`end` with nothing open; a diagram is accepted when it ends at depth 0. Accepted
diagrams are closed under wrapping in a group and under concatenation, and an
opener left open at the end is always rejected. A malformed diagram is an
egm-dsh question (`Alarm.fromModel = false`), so `egm_alarms_survive` applies:
no model can drop it. -/
namespace EgmDiagram

inductive Tok | op | cl | other
  deriving DecidableEq, Repr

def go : Nat → List Tok → Option Nat
  | d, [] => some d
  | d, .op :: r => go (d + 1) r
  | d, .cl :: r => match d with
    | 0 => none
    | e + 1 => go e r
  | d, .other :: r => go d r

def ok (xs : List Tok) : Bool := go 0 xs == some 0

theorem go_append (d : Nat) (xs ys : List Tok) : go d (xs ++ ys) = (go d xs).bind (fun e => go e ys) := by
  induction xs generalizing d with
  | nil => simp [go]
  | cons t r ih =>
    cases t with
    | op => simp [go, ih]
    | other => simp [go, ih]
    | cl => cases d <;> simp [go, ih]

theorem go_shift (d e k : Nat) (xs : List Tok) (h : go d xs = some e) : go (d + k) xs = some (e + k) := by
  induction xs generalizing d with
  | nil => simp [go] at h ⊢; omega
  | cons t r ih =>
    cases t with
    | op => simp [go] at h ⊢; have := ih (d + 1) h; rw [show d + k + 1 = d + 1 + k by omega]; exact this
    | other => simp [go] at h ⊢; exact ih d h
    | cl =>
      cases d with
      | zero => simp [go] at h
      | succ d' => simp [go] at h ⊢; have := ih d' h; rw [show d' + 1 + k = (d' + k) + 1 by omega]; simpa [go] using this

theorem wrap_ok (xs : List Tok) (h : ok xs = true) : ok (.op :: xs ++ [.cl]) = true := by
  simp [ok] at h ⊢
  have h1 : go 1 xs = some 1 := by simpa using go_shift 0 0 1 xs h
  simp [go, go_append, h1]

theorem append_ok (xs ys : List Tok) (hx : ok xs = true) (hy : ok ys = true) : ok (xs ++ ys) = true := by
  simp [ok] at hx hy ⊢
  simp [go_append, hx, hy]

theorem unclosed_rejected (xs : List Tok) (h : ok xs = true) : ok (xs ++ [.op]) = false := by
  simp [ok] at h ⊢
  simp [go_append, h, go]

theorem stray_end_rejected : ok [.cl] = false := by decide

example : ok [.op, .other, .op, .cl, .other, .cl] = true := by decide
example : ok [.op, .other] = false := by decide

/-- PlantUML's own check (built-in plantuml-core or an external command) after the
    structure check: a syntax error always blocks; a checker that could not run
    (panic, timeout, crashed Java) never blocks and never counts as a pass of its own:
    the diagram then stands on the structure check alone. -/
inductive Syn | ok | error | toolFailed
  deriving DecidableEq, Repr

def blocks (structureOk : Bool) (s : Syn) : Bool := !structureOk || s == .error

theorem syntax_error_blocks (st : Bool) : blocks st .error = true := by cases st <;> decide

theorem tool_failure_is_structure_only (st : Bool) : blocks st .toolFailed = blocks st .ok := by cases st <;> decide

theorem structure_error_blocks (s : Syn) : blocks false s = true := by cases s <;> decide

end EgmDiagram

/-! ## Advice and questions the checks catch (`collect_alarms`, `apply_judgments`)

Two rules cut false alarms without touching the gate above.

* A comprehension reviewer's "unclear" points are notes: recorded, shown and handed
  to the elephant, but they never count against the doc. Only the comprehension
  stage writes notes, so egm-dsh's own alarms, readiness questions and critic
  blockers keep blocking exactly as before.
* The verifier may drop a question as `caught-by-check` only by naming an
  acceptance criterion that exists and has a check: a wrong guess then fails that
  check before anything ships. A blocking finding (the design itself is wrong) and
  egm-dsh's own alarms can never be dropped this way. -/
namespace EgmAdvice

inductive Src | egm | comprehension | readiness | critic | zoomOut | builder
  deriving DecidableEq, Repr

inductive Kind | question | blocking | note
  deriving DecidableEq, Repr

/-- What `collect_alarms` gives each stage's items (critic and zoom-out items are
    blockers; everything else is a question, except comprehension's notes). -/
def kindOf : Src → Kind
  | .comprehension => .note
  | .critic | .zoomOut | .builder => .blocking
  | _ => .question

structure A where
  src : Src
  kind : Kind
  dropped : Bool
  deriving DecidableEq, Repr

def fromModel (a : A) : Bool := a.src != .egm

/-- `Alarm::counts`. -/
def counts (a : A) : Bool := !a.dropped && a.kind != .note

/-- The pass rule over alarms (readiness is carried by its alarms). -/
def passes (xs : List A) : Bool := xs.all (fun a => !counts a)

theorem notes_only_from_comprehension (s : Src) : kindOf s = .note ↔ s = .comprehension := by
  cases s <;> decide

theorem note_never_counts (a : A) (h : a.kind = .note) : counts a = false := by
  simp [counts, h]

theorem notes_never_change_the_verdict (xs ns : List A) (h : ∀ n ∈ ns, n.kind = .note) :
    passes (xs ++ ns) = passes xs := by
  simp only [passes, List.all_append]
  have : ns.all (fun a => !counts a) = true := by
    rw [List.all_eq_true]; intro n hn; simp [note_never_counts n (h n hn)]
  simp [this]

theorem counted_alarm_blocks (xs : List A) (a : A) (hm : a ∈ xs) (hc : counts a = true) : passes xs = false := by
  cases hp : passes xs
  · rfl
  · simp only [passes, List.all_eq_true] at hp; have := hp a hm; simp [hc] at this

theorem open_question_blocks (xs : List A) (a : A) (hm : a ∈ xs) (hk : a.kind = .question) (hd : a.dropped = false) :
    passes xs = false := counted_alarm_blocks xs a hm (by simp [counts, hk, hd])

/-- The verifier's `caught-by-check`: criterion `c` (numbered from 1); `checks[i]`
    says whether criterion `i + 1` names a check. -/
def caughtOk (a : A) (c : Nat) (checks : List Bool) : Bool :=
  fromModel a && a.kind == .question && c ≥ 1 && checks[c - 1]? == some true

def applyCaught (a : A) (c : Nat) (checks : List Bool) : A :=
  if caughtOk a c checks then { a with dropped := true } else a

theorem egm_alarm_survives_caught (a : A) (c : Nat) (cs : List Bool) (h : a.src = .egm) : applyCaught a c cs = a := by
  simp [applyCaught, caughtOk, fromModel, h]

theorem blocker_survives_caught (a : A) (c : Nat) (cs : List Bool) (h : a.kind = .blocking) : applyCaught a c cs = a := by
  simp [applyCaught, caughtOk, h]

theorem caught_needs_a_checked_criterion (a : A) (c : Nat) (cs : List Bool)
    (h0 : a.dropped = false) (h1 : (applyCaught a c cs).dropped = true) :
    c ≥ 1 ∧ cs[c - 1]? = some true := by
  unfold applyCaught at h1
  split at h1
  · rename_i hk; simp [caughtOk] at hk; refine ⟨?_, ?_⟩ <;> simp_all
  · simp [h0] at h1

theorem unchecked_criterion_catches_nothing (a : A) (c : Nat) (cs : List Bool) (h : cs[c - 1]? ≠ some true) :
    applyCaught a c cs = a := by
  simp [applyCaught, caughtOk, h]

example : passes [⟨.comprehension, .note, false⟩] = true := by decide
example : passes [⟨.comprehension, .note, false⟩, ⟨.readiness, .question, false⟩] = false := by decide
example : (applyCaught ⟨.readiness, .question, false⟩ 2 [true, true]).dropped = true := by decide
example : (applyCaught ⟨.readiness, .question, false⟩ 2 [true, false]).dropped = false := by decide
example : (applyCaught ⟨.critic, .blocking, false⟩ 1 [true]).dropped = false := by decide

end EgmAdvice

/-! ## The built-in reviewer (`agent/builtin.rs`, `goldfish::embeddable`, `Project::builtin_reviewer`)

The read-only goldfish roles of a dsh project make one `/chat/completions`
request from egm-dsh itself instead of a DeepSeek Harness session.

* Routing: only the reader, critic and verifier roles of a dsh project with
  `reviewers: builtin` use it; the elephant, tester and builder always keep dsh.
* The request is a function of the model, effort and prompt only: one user
  message, no tools, no history, so the goldfish stays memoryless by construction.
* The prompt shows the doc and only those linked files that are inside the
  project, outside `.git`/`.egm-dsh`, and within the size limit. -/
namespace EgmBuiltin

inductive Role | elephant | draft | escalate | goldfish | goldfishCritic | goldfishVerifier | builder | reviewer
  deriving DecidableEq, Repr

inductive AgentKind | dsh | openai | mock
  deriving DecidableEq, Repr

inductive Client | builtin | dsh | other
  deriving DecidableEq, Repr

def goldfishRole : Role → Bool
  | .goldfish | .goldfishCritic | .goldfishVerifier => true
  | _ => false

/-- `Project::builtin_reviewer` and `raw_agent`'s choice. -/
def route (builtinSetting : Bool) (kind : AgentKind) (r : Role) : Client :=
  if goldfishRole r && builtinSetting && kind == .dsh then .builtin
  else if kind == .dsh then .dsh else .other

theorem writers_keep_dsh (b : Bool) (r : Role) (h : r = .elephant ∨ r = .builder ∨ r = .draft ∨ r = .escalate ∨ r = .reviewer) :
    route b .dsh r = .dsh := by
  rcases h with h | h | h | h | h <;> subst h <;> simp [route, goldfishRole]

theorem builtin_only_for_goldfish (b : Bool) (k : AgentKind) (r : Role) (h : route b k r = .builtin) : goldfishRole r = true := by
  unfold route at h; split at h
  · rename_i hc; simp at hc; exact hc.1.1
  · split at h <;> simp at h

theorem dsh_setting_keeps_dsh (k : AgentKind) (r : Role) : route false k r ≠ .builtin := by
  unfold route; split
  · rename_i hc; simp at hc
  · split <;> simp

structure Req where
  model : String
  effort : Option String
  messages : List String
  tools : List String
  deriving DecidableEq, Repr

/-- `builtin::request_body`: whatever came before (`history`) is not an argument it uses. -/
def request (_history : List String) (model : String) (effort : Option String) (prompt : String) : Req :=
  { model, effort, messages := [prompt], tools := [] }

theorem request_memoryless (h₁ h₂ : List String) (m : String) (e : Option String) (p : String) :
    request h₁ m e p = request h₂ m e p := rfl

theorem no_tools (h : List String) (m : String) (e : Option String) (p : String) : (request h m e p).tools = [] := rfl

theorem only_the_prompt (h : List String) (m : String) (e : Option String) (p : String) : (request h m e p).messages = [p] := rfl

/-- A linked file, as `embeddable` sees it. -/
structure File where
  name : String
  inside : Bool
  internal : Bool
  size : Nat
  deriving DecidableEq, Repr

def cap : Nat := 512 * 1024

def embeddable (f : File) : Bool := f.inside && !f.internal && f.size ≤ cap

/-- The files `brief` puts in the prompt: the doc, then each allowed linked file. -/
def shown (doc : File) (links : List File) : List File := doc :: links.filter embeddable

theorem sent_only_allowed (doc f : File) (links : List File) (h : f ∈ shown doc links) :
    f = doc ∨ (f ∈ links ∧ embeddable f = true) := by
  simp only [shown, List.mem_cons, List.mem_filter] at h
  exact h

theorem outside_never_sent (doc f : File) (links : List File) (hd : f ≠ doc) (ho : f.inside = false) : f ∉ shown doc links := by
  intro h
  rcases sent_only_allowed doc f links h with h | ⟨_, he⟩
  · exact hd h
  · simp [embeddable, ho] at he

theorem internals_never_sent (doc f : File) (links : List File) (hd : f ≠ doc) (hi : f.internal = true) : f ∉ shown doc links := by
  intro h
  rcases sent_only_allowed doc f links h with h | ⟨_, he⟩
  · exact hd h
  · simp [embeddable, hi] at he

example : route true .dsh .goldfishVerifier = .builtin := by decide
example : route true .dsh .builder = .dsh := by decide
example : route true .mock .goldfish = .other := by decide
example : shown ⟨"d", true, false, 1⟩ [⟨"../../etc/passwd", false, false, 1⟩, ⟨"a.py", true, false, 9⟩] = [⟨"d", true, false, 1⟩, ⟨"a.py", true, false, 9⟩] := by decide

end EgmBuiltin

/-! ## Readiness by majority vote (`goldfish::apply_vote`)

The readiness question ("could you implement this in one pass?") is asked of `n`
independent readers. After grounding and the verifier, a reader *objects* when one of
its questions still counts. Readiness blocks exactly when a strict majority objects;
otherwise the minority's questions are dropped (by `vote`) and kept on record. One
reader (`n = 1`) is the old rule. Only readiness alarms are ever voted on: egm-dsh's
own alarms and critic or zoom-out blockers block as before. -/
namespace EgmVote

def objections (objs : List Bool) : Nat := (objs.filter id).length

/-- Readiness blocks when a strict majority of the readers object. -/
def blocks (objs : List Bool) : Bool := decide (objections objs * 2 > objs.length)

theorem one_reader_is_the_old_rule (b : Bool) : blocks [b] = b := by
  cases b <;> decide

theorem unanimous_objection_blocks (objs : List Bool) (hn : objs ≠ []) (h : ∀ b ∈ objs, b = true) : blocks objs = true := by
  have hf : objs.filter id = objs := List.filter_eq_self.mpr (by intro b hb; simp [h b hb])
  have hl : 0 < objs.length := List.length_pos_iff.mpr hn
  simp only [blocks, objections, hf, decide_eq_true_eq]; omega

theorem no_objection_never_blocks (objs : List Bool) (h : ∀ b ∈ objs, b = false) : blocks objs = false := by
  have hf : objs.filter id = [] := List.filter_eq_nil_iff.mpr (by intro b hb; simp [h b hb])
  simp [blocks, objections, hf]

/-- One more objecting reader never turns a block into a pass. -/
theorem objecting_never_unblocks (xs ys : List Bool) (h : blocks (xs ++ false :: ys) = true) : blocks (xs ++ true :: ys) = true := by
  simp only [blocks, objections, List.filter_append, List.length_append, List.length_cons, decide_eq_true_eq] at *
  simp at *; omega

inductive Src | egm | readiness | critic | zoomOut | comprehension
  deriving DecidableEq, Repr

structure A where
  src : Src
  vote : Nat
  counts : Bool
  deriving DecidableEq, Repr

/-- `apply_vote`: with no majority, the readiness alarms stop counting; nothing else changes. -/
def applyVote (majority : Bool) (a : A) : A :=
  if a.src = .readiness ∧ majority = false then { a with counts := false } else a

theorem only_readiness_is_voted (m : Bool) (a : A) (h : a.src ≠ .readiness) : applyVote m a = a := by
  simp [applyVote, h]

theorem majority_keeps_every_alarm (a : A) : applyVote true a = a := by
  simp [applyVote]

example : blocks [true, true, false] = true := by decide
example : blocks [true, false, false] = false := by decide
example : blocks [true, false] = false := by decide

end EgmVote

/-! ## The evidence gate (`evidence.rs`, `gate.rs`)

`egm-dsh gate` is what CI runs before a merge. It needs no model.

* It passes exactly when every check passes, and never with no checks.
* The mutation check: killed / tried ≥ min%, in integer arithmetic; nothing tried
  never passes, and catching one more seeded bug never turns a pass into a fail.
* Risk: each signal raises the tier, and the tier is the highest one raised, so one
  more signal never lowers it.
* Human minutes: each gap between commands counts at most `cap`, so the estimate is
  bounded by `cap` times the number of gaps (plus minutes logged by hand).
* Uncovered changed lines are always changed lines. -/
namespace EgmEvidence

def gatePasses (checks : List Bool) : Bool := !checks.isEmpty && checks.all id

theorem gate_passes_iff (cs : List Bool) : gatePasses cs = true ↔ cs ≠ [] ∧ ∀ c ∈ cs, c = true := by
  simp [gatePasses, List.all_eq_true]

theorem one_failing_check_fails (cs : List Bool) (h : false ∈ cs) : gatePasses cs = false := by
  cases hg : gatePasses cs
  · rfl
  · have := ((gate_passes_iff cs).mp hg).2 false h; simp at this

def mutationOk (killed tried minPct : Nat) : Bool := decide (0 < tried) && decide (tried * minPct ≤ killed * 100)

theorem nothing_tried_never_passes (k m : Nat) : mutationOk k 0 m = false := by
  simp [mutationOk]

theorem one_more_caught_never_fails (k t m : Nat) (h : mutationOk k t m = true) : mutationOk (k + 1) t m = true := by
  simp [mutationOk] at *; omega

theorem everything_caught_passes (t m : Nat) (ht : 0 < t) (hm : m ≤ 100) : mutationOk t t m = true := by
  simp [mutationOk, ht]; exact Nat.mul_le_mul_left t hm

/-- Seeded bugs end as caught, survived or timed out. A timeout is neither a catch nor a
survivor: the verdict is `mutationOk` over the judged bugs, caught + survived. The Rust
`MutationResult::passes` mirrors this (`judged_rule_matches_lean` checks it). -/
def judgedOk (caught survived _timedOut minPct : Nat) : Bool := mutationOk caught (caught + survived) minPct

/-- `timeout_never_counts`: the number of timeouts never changes the verdict. -/
theorem timeout_never_counts (k s o o' m : Nat) : judgedOk k s o m = judgedOk k s o' m := rfl

/-- A run in which every seeded bug timed out never passes, whatever the minimum. -/
theorem all_timed_out_never_passes (o m : Nat) : judgedOk 0 0 o m = false := by
  simp [judgedOk, mutationOk]

/-- Catching one of the same bugs that survived never turns a pass into a fail, with
timeouts present or not. -/
theorem judged_one_more_caught (k s o m : Nat) (h : judgedOk k (s + 1) o m = true) : judgedOk (k + 1) s o m = true := by
  have e : k + (s + 1) = k + 1 + s := by omega
  unfold judgedOk at *
  rw [e] at h
  exact one_more_caught_never_fails k (k + 1 + s) m h

/-- The old rule counted a timeout as a catch: 30 timeouts and no catch passed at 60%. -/
example : mutationOk (0 + 30) (0 + 0 + 30) 60 = true := by decide
example : judgedOk 0 0 30 60 = false := by decide
example : judgedOk 6 2 2 60 = true := by decide
example : judgedOk 4 4 7 60 = false := by decide

/-- Tiers 0 (low) < 1 (medium) < 2 (high); the tier is the highest signal raised. -/
def tier (signals : List Nat) : Nat := signals.foldl max 0

theorem foldl_max_ge (xs : List Nat) (acc s : Nat) (h : s ∈ xs ∨ s ≤ acc) : s ≤ xs.foldl max acc := by
  induction xs generalizing acc with
  | nil =>
    rcases h with h | h
    · simp at h
    · exact h
  | cons y ys ih =>
    simp only [List.foldl_cons]
    apply ih
    rcases h with h | h
    · rcases List.mem_cons.mp h with h | h
      · right; subst h; exact Nat.le_max_right _ _
      · left; exact h
    · right; exact Nat.le_trans h (Nat.le_max_left _ _)

/-- Every raised signal is at most the tier. -/
theorem tier_ge_signal (xs : List Nat) (s : Nat) (h : s ∈ xs) : s ≤ tier xs := foldl_max_ge xs 0 s (Or.inl h)

theorem foldl_max_mono (xs : List Nat) (a b : Nat) (h : a ≤ b) : xs.foldl max a ≤ xs.foldl max b := by
  induction xs generalizing a b with
  | nil => simpa using h
  | cons y ys ih => simp only [List.foldl_cons]; exact ih _ _ (by omega)

/-- `tier_monotone`: one more risk signal never lowers the tier. -/
theorem tier_monotone (xs : List Nat) (s : Nat) : tier xs ≤ tier (s :: xs) := by
  unfold tier; simp only [List.foldl_cons]; exact foldl_max_mono xs 0 (max 0 s) (Nat.zero_le _)

/-- Gaps in minutes, each counted at most `cap`. -/
def counted (cap : Nat) (gaps : List Nat) : Nat := (gaps.map (fun g => min g cap)).sum

theorem minutes_bounded (cap : Nat) (gaps : List Nat) : counted cap gaps ≤ cap * gaps.length := by
  induction gaps with
  | nil => simp [counted]
  | cons g gs ih => simp [counted, List.sum_cons, Nat.mul_succ] at *; omega

/-- Changed lines no test ran: the changed lines whose hit count is zero. -/
def uncovered (changed : List Nat) (zeroHit : Nat → Bool) : List Nat := changed.filter zeroHit

theorem uncovered_subset (changed : List Nat) (z : Nat → Bool) (n : Nat) (h : n ∈ uncovered changed z) : n ∈ changed :=
  (List.mem_filter.mp h).1

example : gatePasses [true, true, false] = false := by decide
example : gatePasses [] = false := by decide
example : mutationOk 6 10 60 = true := by decide
example : mutationOk 5 10 60 = false := by decide
example : tier [0, 2, 1] = 2 := by decide

end EgmEvidence
