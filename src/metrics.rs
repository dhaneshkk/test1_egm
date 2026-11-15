//! Does the goldfish predict trouble? Matthews correlation between the first
//! goldfish verdict on a feature (`not-ready` = positive) and its recorded
//! outcome (`rework` = positive).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Confusion {
    pub tp: u64,
    pub fp: u64,
    pub tn: u64,
    pub fn_: u64,
}

impl Confusion {
    pub fn add(&mut self, predicted_positive: bool, actual_positive: bool) {
        match (predicted_positive, actual_positive) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, false) => self.tn += 1,
            (false, true) => self.fn_ += 1,
        }
    }

    pub fn n(&self) -> u64 {
        self.tp + self.fp + self.tn + self.fn_
    }

    /// MCC in [-1, 1]. Returns 0 when any marginal is empty (the usual convention).
    pub fn mcc(&self) -> f64 {
        let (tp, fp, tn, fn_) = (self.tp as f64, self.fp as f64, self.tn as f64, self.fn_ as f64);
        let denom = ((tp + fp) * (tp + fn_) * (tn + fp) * (tn + fn_)).sqrt();
        if denom == 0.0 { 0.0 } else { (tp * tn - fp * fn_) / denom }
    }
}

/// Fewest labeled features before egm reports an MCC without a warning.
pub const MIN_LABELS: u64 = 29;

#[cfg(test)]
mod tests {
    use super::*;

    fn c(tp: u64, fp: u64, tn: u64, fn_: u64) -> Confusion {
        Confusion { tp, fp, tn, fn_ }
    }

    #[test]
    fn mcc_known_values() {
        assert_eq!(c(10, 0, 10, 0).mcc(), 1.0);
        assert_eq!(c(0, 10, 0, 10).mcc(), -1.0);
        assert_eq!(c(5, 5, 5, 5).mcc(), 0.0);
        assert_eq!(c(3, 0, 0, 0).mcc(), 0.0); // empty marginals
        let m = c(6, 2, 18, 3).mcc(); // (108-6)/sqrt(8*9*20*21)
        assert!((m - 102.0 / (8.0f64 * 9.0 * 20.0 * 21.0).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn mcc_is_symmetric_under_relabeling() {
        let a = c(6, 2, 18, 3);
        let flipped = c(18, 3, 6, 2); // swap positive and negative classes
        assert!((a.mcc() - flipped.mcc()).abs() < 1e-12);
    }
}
