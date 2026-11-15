//! What tokens cost. Mirrors `proofs/EgmCost.lean`: prices are whole
//! micro-dollars per million tokens and the cost numerator is
//! `hit·p_hit + miss·p_miss + out·p_out`, so the proved properties
//! (cheaper prices never cost more; a cache hit never costs more than a miss)
//! hold for exactly this arithmetic.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agent::Usage;

/// USD per one million tokens, at DeepSeek's peak rate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub cache_hit: f64,
    pub cache_miss: f64,
    pub output: f64,
}

/// Official DeepSeek API prices (api-docs.deepseek.com, Models & Pricing, read
/// 2026-10-24), peak rate. Off-peak (all hours outside 01:00–04:00 and
/// 06:00–10:00 UTC on weekdays) is exactly half.
pub fn default_prices() -> BTreeMap<String, Price> {
    let flash = Price { cache_hit: 0.006, cache_miss: 0.30, output: 1.20 };
    let pro = Price { cache_hit: 0.044, cache_miss: 1.32, output: 3.96 };
    BTreeMap::from([
        ("deepseek-flash".to_string(), flash),
        ("deepseek-v4-flash".to_string(), flash), // legacy name, billed as Flash
        ("deepseek-v4-pro".to_string(), pro),
    ])
}

/// Price in whole micro-dollars per million tokens (`$0.006` → `6000`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MicroPrice {
    pub hit: u64,
    pub miss: u64,
    pub out: u64,
}

impl From<Price> for MicroPrice {
    fn from(p: Price) -> Self {
        let m = |x: f64| (x * 1_000_000.0).round().max(0.0) as u64;
        MicroPrice { hit: m(p.cache_hit), miss: m(p.cache_miss), out: m(p.output) }
    }
}

/// `cost` in the Lean file: micro-dollars × tokens / 10^6 still to divide.
pub fn numerator(p: MicroPrice, u: &Usage) -> u128 {
    u.cache_hit as u128 * p.hit as u128 + u.cache_miss as u128 * p.miss as u128 + u.output as u128 * p.out as u128
}

/// Dollars at the peak rate.
pub fn dollars(p: MicroPrice, u: &Usage) -> f64 {
    numerator(p, u) as f64 / 1e12
}

pub fn lookup(prices: &BTreeMap<String, Price>, model: &str) -> Option<MicroPrice> {
    prices.get(model).copied().map(MicroPrice::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(hit: u64, miss: u64, out: u64) -> Usage {
        Usage { cache_hit: hit, cache_miss: miss, output: out, requests: 1, estimated: false, reasoning: 0 }
    }

    #[test]
    fn official_prices_and_ratios() {
        let p = default_prices();
        let flash = MicroPrice::from(p["deepseek-flash"]);
        let pro = MicroPrice::from(p["deepseek-v4-pro"]);
        assert_eq!((flash.hit, flash.miss, flash.out), (6_000, 300_000, 1_200_000));
        assert_eq!((pro.hit, pro.miss, pro.out), (44_000, 1_320_000, 3_960_000));
        // One million output tokens on Flash at peak: $1.20.
        assert!((dollars(flash, &u(0, 0, 1_000_000)) - 1.2).abs() < 1e-9);
        // A cache hit is 50x cheaper than a miss on Flash.
        assert_eq!(flash.miss / flash.hit, 50);
    }

    #[test]
    fn cheaper_price_never_costs_more() {
        // The Lean theorem `cost_mono`, spot-checked on the real price table.
        let p = default_prices();
        let flash = MicroPrice::from(p["deepseek-flash"]);
        let pro = MicroPrice::from(p["deepseek-v4-pro"]);
        for usage in [u(0, 0, 0), u(5_000, 1_000, 300), u(1, 2, 3), u(10_000_000, 0, 0)] {
            assert!(numerator(flash, &usage) <= numerator(pro, &usage));
        }
    }
}
