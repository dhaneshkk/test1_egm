//! Review times and the gate's track record (phase 5), from the audit log.
//!
//! * Review times: from the first passing gate to the first review, to the merge approval and to
//!   the merge on the code host. Reported as medians with 95% bootstrap intervals, since a field
//!   test has few changes and their times are skewed.
//! * Gate outcomes: a merged change is a defect when it was reverted on the code host, or someone
//!   reported a defect in egm-team, within 14 days of the merge; clean when 14 days passed without
//!   either; still open before that. With the gate a required check, nearly every merged change
//!   passed it, so the gate MCC usually stays undefined (nothing in the failed column); the share
//!   of passed changes that stayed clean is shown with a Wilson interval instead.

use serde_json::Value;
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;

pub const WINDOW_DAYS: f64 = 14.0;

/// Median and 95% percentile-bootstrap interval (2,000 resamples, fixed seed, so the page is
/// the same on every load). `None` for no values; the interval is `None` below two values.
pub fn median_ci(values: &[f64]) -> Option<(f64, Option<(f64, f64)>)> {
    if values.is_empty() {
        return None;
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        let n = v.len();
        if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
    };
    let m = med(&mut values.to_vec());
    if values.len() < 2 {
        return Some((m, None));
    }
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut meds: Vec<f64> = (0..2000)
        .map(|_| {
            let mut s: Vec<f64> = (0..values.len()).map(|_| values[(next() % values.len() as u64) as usize]).collect();
            med(&mut s)
        })
        .collect();
    meds.sort_by(|a, b| a.total_cmp(b));
    Some((m, Some((meds[49], meds[1949]))))
}

/// Wilson 95% interval for `k` successes of `n`.
pub fn wilson(k: u64, n: u64) -> Option<(f64, f64, f64)> {
    if n == 0 {
        return None;
    }
    let (k, n, z) = (k as f64, n as f64, 1.96f64);
    let p = k / n;
    let d = 1.0 + z * z / n;
    let c = (p + z * z / (2.0 * n)) / d;
    let h = z * ((p * (1.0 - p) / n) + z * z / (4.0 * n * n)).sqrt() / d;
    Some((p, (c - h).max(0.0), (c + h).min(1.0)))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timing {
    /// Hours from the first passing gate.
    pub to_review: Vec<f64>,
    pub to_approval: Vec<f64>,
    pub to_merge: Vec<f64>,
}

/// Outcome of a merged change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Defect,
    Open,
}

/// From one feature's events (kind, epoch seconds, gate pass), oldest first.
pub fn feature_timing(evs: &[(String, f64, bool)], t: &mut Timing) {
    let Some(gate) = evs.iter().find(|e| e.0 == "gate.verdict" && e.2).map(|e| e.1) else { return };
    let after = |k: &str| evs.iter().find(|e| e.0 == k && e.1 >= gate).map(|e| (e.1 - gate) / 3600.0);
    if let Some(h) = after("review.submitted") {
        t.to_review.push(h);
    }
    if let Some(h) = after("merge.approved") {
        t.to_approval.push(h);
    }
    if let Some(h) = after("merge.completed") {
        t.to_merge.push(h);
    }
}

/// The outcome of a merged feature at time `now` (epoch seconds), and whether its last gate
/// before the merge passed. `None` when it was never merged.
pub fn outcome(evs: &[(String, f64, bool)], now: f64) -> Option<(bool, Outcome)> {
    let merged = evs.iter().find(|e| e.0 == "merge.completed")?.1;
    let passed = evs.iter().rfind(|e| e.0 == "gate.verdict" && e.1 <= merged).is_some_and(|e| e.2);
    let window = merged + WINDOW_DAYS * 86400.0;
    let bad = evs.iter().any(|e| (e.0 == "change.reverted" || e.0 == "defect.reported") && e.1 >= merged && e.1 <= window);
    Some((passed, if bad { Outcome::Defect } else if now >= window { Outcome::Clean } else { Outcome::Open }))
}

pub struct Report {
    pub timing: Timing,
    /// (gate passed, outcome) per merged feature.
    pub outcomes: Vec<(String, bool, Outcome)>,
}

pub async fn report(c: &impl GenericClient, project: &str) -> anyhow::Result<Report> {
    let rows = c
        .query(
            "SELECT feature, kind, EXTRACT(EPOCH FROM at::timestamptz)::float8, data FROM timeline WHERE project=$1 AND feature <> ''
             AND kind IN ('gate.verdict','review.submitted','merge.approved','merge.completed','change.reverted','defect.reported') ORDER BY id",
            &[&project],
        )
        .await?;
    let now: f64 = c.query_one("SELECT EXTRACT(EPOCH FROM now())::float8", &[]).await?.get(0);
    let mut by: BTreeMap<String, Vec<(String, f64, bool)>> = BTreeMap::new();
    for r in rows {
        let data: Value = r.get(3);
        by.entry(r.get(0)).or_default().push((r.get(1), r.get(2), data["pass"] == true));
    }
    let mut timing = Timing::default();
    let mut outcomes = vec![];
    for (f, evs) in &by {
        feature_timing(evs, &mut timing);
        if let Some((p, o)) = outcome(evs, now) {
            outcomes.push((f.clone(), p, o));
        }
    }
    Ok(Report { timing, outcomes })
}

pub fn fmt_hours(h: f64) -> String {
    if h < 1.0 { format!("{:.0} min", h * 60.0) } else if h < 48.0 { format!("{h:.1} h") } else { format!("{:.1} days", h / 24.0) }
}

pub fn fmt_median(v: &[f64]) -> String {
    match median_ci(v) {
        None => "no data".into(),
        Some((m, None)) => format!("{} (n = 1, no interval)", fmt_hours(m)),
        Some((m, Some((lo, hi)))) => format!("{} (95% interval {} to {}; n = {})", fmt_hours(m), fmt_hours(lo), fmt_hours(hi), v.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_and_intervals() {
        assert_eq!(median_ci(&[]), None);
        assert_eq!(median_ci(&[3.0]), Some((3.0, None)));
        let (m, ci) = median_ci(&[1.0, 2.0, 3.0, 4.0, 100.0]).unwrap();
        assert_eq!(m, 3.0);
        let (lo, hi) = ci.unwrap();
        assert!(lo <= m && m <= hi && lo >= 1.0 && hi <= 100.0);
        assert_eq!(median_ci(&[2.0, 4.0]).unwrap().0, 3.0);
        assert_eq!(median_ci(&[1.0, 2.0, 3.0, 4.0, 100.0]), median_ci(&[1.0, 2.0, 3.0, 4.0, 100.0]), "the same on every load");
    }

    #[test]
    fn wilson_interval() {
        assert_eq!(wilson(0, 0), None);
        let (p, lo, hi) = wilson(9, 10).unwrap();
        assert!((p - 0.9).abs() < 1e-9 && lo > 0.55 && lo < 0.6 && hi > 0.98 && hi <= 1.0);
        let (_, lo, hi) = wilson(0, 5).unwrap();
        assert!(lo == 0.0 && hi > 0.4);
    }

    #[test]
    fn outcomes_and_timing() {
        let d = 86400.0;
        let e = |k: &str, t: f64, p: bool| (k.to_string(), t, p);
        let clean = [e("gate.verdict", 0.0, true), e("review.submitted", 3600.0, false), e("merge.approved", 7200.0, false), e("merge.completed", 10800.0, false)];
        assert_eq!(outcome(&clean, 20.0 * d), Some((true, Outcome::Clean)));
        assert_eq!(outcome(&clean, 2.0 * d), Some((true, Outcome::Open)));
        let reverted = [clean.to_vec(), vec![e("change.reverted", 5.0 * d, false)]].concat();
        assert_eq!(outcome(&reverted, 2.0 * d), Some((true, Outcome::Defect)));
        let late = [clean.to_vec(), vec![e("defect.reported", 30.0 * d, false)]].concat();
        assert_eq!(outcome(&late, 40.0 * d), Some((true, Outcome::Clean)), "a defect after the window does not count");
        assert_eq!(outcome(&clean[..3], 40.0 * d), None);
        let mut t = Timing::default();
        feature_timing(&clean, &mut t);
        assert_eq!(t, Timing { to_review: vec![1.0], to_approval: vec![2.0], to_merge: vec![3.0] });
    }
}
