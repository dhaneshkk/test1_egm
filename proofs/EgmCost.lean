/-
  The cost model behind `egm usage` and the role presets, mirrored by
  `src/cost.rs` and `src/project.rs`. Kernel-only: no Mathlib, no imports.
  Checked with Lean 4.31.0:

      lean proofs/EgmCost.lean

  Prices are whole micro-dollars per million tokens, exactly as `MicroPrice`
  in Rust; `cost` is the numerator `cost::numerator` computes.

  What is proved, for EVERY possible list of turns and token counts:
  * `cost_mono`, `total_mono`: a componentwise cheaper price never costs more.
  * `cache_hit_never_costs_more`: moving input from cache-miss to cache-hit never costs more.
  * `economy_le_balanced`, `balanced_le_quality`: with the same token counts per
    turn, the economy routing never costs more than balanced, and balanced never
    more than quality, using the official DeepSeek price table.
  Not claimed: that a cheaper model uses the same number of tokens, or that it
  does the job as well. `egm metrics --by-model` measures the second.
-/

structure Price where
  hit  : Nat
  miss : Nat
  out  : Nat

structure Usage where
  hit  : Nat
  miss : Nat
  out  : Nat

def cost (p : Price) (u : Usage) : Nat :=
  u.hit * p.hit + u.miss * p.miss + u.out * p.out

def Price.le (p q : Price) : Prop :=
  p.hit ≤ q.hit ∧ p.miss ≤ q.miss ∧ p.out ≤ q.out

theorem cost_mono {p q : Price} (u : Usage) (h : p.le q) : cost p u ≤ cost q u := by
  obtain ⟨h1, h2, h3⟩ := h
  unfold cost
  exact Nat.add_le_add (Nat.add_le_add (Nat.mul_le_mul_left _ h1) (Nat.mul_le_mul_left _ h2))
    (Nat.mul_le_mul_left _ h3)

/-- Billing `k` input tokens as cache hits instead of misses never costs more,
    whenever a hit is priced no higher than a miss (true for every DeepSeek model). -/
theorem cache_hit_never_costs_more (p : Price) (u : Usage) (k : Nat)
    (hp : p.hit ≤ p.miss) (hk : k ≤ u.miss) :
    cost p ⟨u.hit + k, u.miss - k, u.out⟩ ≤ cost p u := by
  unfold cost
  simp only
  have split : u.miss * p.miss = (u.miss - k) * p.miss + k * p.miss := by
    rw [← Nat.add_mul, Nat.sub_add_cancel hk]
  have hk' : k * p.hit ≤ k * p.miss := Nat.mul_le_mul_left _ hp
  rw [Nat.add_mul, split]
  omega

/-- The five roles egm routes, as in `RoleName`. -/
inductive Role where
  | elephant | goldfish | goldfishCritic | builder | reviewer
  deriving DecidableEq

structure Turn where
  role  : Role
  usage : Usage

def total (route : Role → Price) : List Turn → Nat
  | [] => 0
  | t :: ts => cost (route t.role) t.usage + total route ts

theorem total_mono {r s : Role → Price} (h : ∀ x, (r x).le (s x)) :
    ∀ ts : List Turn, total r ts ≤ total s ts
  | [] => Nat.le_refl 0
  | t :: ts => Nat.add_le_add (cost_mono t.usage (h t.role)) (total_mono h ts)

/-- Official DeepSeek peak prices, micro-dollars per million tokens (`cost::default_prices`). -/
def flash : Price := ⟨6000, 300000, 1200000⟩
def pro   : Price := ⟨44000, 1320000, 3960000⟩

theorem flash_le_pro : flash.le pro := by
  unfold Price.le flash pro; decide

theorem price_le_refl (p : Price) : p.le p := ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩

theorem hit_le_miss_flash : flash.hit ≤ flash.miss := by decide
theorem hit_le_miss_pro : pro.hit ≤ pro.miss := by decide

/-- Model per role in each preset (`Preset::roles`). -/
def economy : Role → Price := fun _ => flash

def balanced : Role → Price
  | .elephant => pro
  | .builder  => pro
  | _         => flash

def quality : Role → Price := fun _ => pro

theorem economy_le_balanced (ts : List Turn) : total economy ts ≤ total balanced ts :=
  total_mono (fun x => by cases x <;> first | exact flash_le_pro | exact price_le_refl _) ts

theorem balanced_le_quality (ts : List Turn) : total balanced ts ≤ total quality ts :=
  total_mono (fun x => by cases x <;> first | exact flash_le_pro | exact price_le_refl _) ts

/-- Dividing by 10^6 to get micro-dollars keeps the order (`cost::dollars`). -/
theorem dollars_mono {a b : Nat} (h : a ≤ b) : a / 1000000 ≤ b / 1000000 :=
  Nat.div_le_div_right h
