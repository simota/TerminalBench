//! Statistics primitives shared by every scenario: median/p95/stddev,
//! 1.5×IQR outlier filtering, and the bootstrap CI used both by
//! `--runs auto` convergence and by `compare`'s detail tables.
//!
//! Estimators and indices, unchanged since the original zsh
//! implementation so numbers stay continuous across it:
//! - median: mean of the two middle elements for even n
//! - p95: nearest-rank rounded up (`(n*95 + 99) / 100`, clamped to [1, n])
//! - stddev: population (divide by n)
//! - IQR: Tukey fences at Q1/Q3 via the `(n+1)/4` / `3(n+1)/4` indices

pub fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

pub fn stddev(samples: &[f64]) -> f64 {
    let n = samples.len();
    if n <= 1 {
        return 0.0;
    }
    let mean = samples.iter().sum::<f64>() / n as f64;
    let sqsum = samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>();
    (sqsum / n as f64).sqrt()
}

pub fn p95(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    let mut idx = (n * 95).div_ceil(100); // nearest-rank, rounded up (1-based)
    if idx < 1 {
        idx = 1;
    }
    if idx > n {
        idx = n;
    }
    sorted[idx - 1]
}

/// p95 for a result row: taken from every timed trial, not from the
/// outlier-filtered set. The filter exists to steady the median; applied
/// before a tail statistic it removes the tail and biases p95 low (see
/// docs/p95-outlier-simulation.md). Never below the reported median, which
/// Reporting Spec v1 requires and which can only differ when the filter
/// dropped fast trials.
pub fn tail_p95(all_trials: &[f64], filtered_median: f64) -> f64 {
    p95(all_trials).max(filtered_median)
}

/// 1.5×IQR outlier rejection (Tukey fence). Returns (removed_count, kept),
/// with `kept` in the order the samples were taken so drift across a run
/// stays visible in stored samples. Skips filtering below 4 samples
/// (quartiles aren't meaningful that small) and never filters down to
/// zero: a degenerate series keeps the unfiltered set instead.
pub fn iqr_filter(samples: &[f64]) -> (usize, Vec<f64>) {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    if n < 4 {
        return (0, samples.to_vec());
    }
    // 1-based indices from the zsh implementation, clamped the same way.
    let q1_idx = ((n + 1) / 4).max(1);
    let q3_idx = ((3 * (n + 1)) / 4).min(n);
    let q1 = sorted[q1_idx - 1];
    let q3 = sorted[q3_idx - 1];
    let iqr = q3 - q1;
    let lower = q1 - 1.5 * iqr;
    let upper = q3 + 1.5 * iqr;

    let kept: Vec<f64> = samples
        .iter()
        .copied()
        .filter(|v| *v >= lower && *v <= upper)
        .collect();
    if kept.is_empty() {
        return (0, samples.to_vec());
    }
    let removed = n - kept.len();
    (removed, kept)
}

/// Deterministic xorshift64* PRNG — fixed seeding keeps bootstrap CIs
/// reproducible from the same samples, without pulling in a rand crate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Bootstrap 95% CI on the median: resample-with-replacement `b` times,
/// take the 2.5%/97.5% quantiles of the resampled medians. Returns None
/// below 2 samples.
pub fn bootstrap_median_ci(samples: &[f64], b: usize, seed: u64) -> Option<(f64, f64)> {
    let n = samples.len();
    if n < 2 {
        return None;
    }
    let mut rng = Rng::new(seed.wrapping_add(0x9E3779B97F4A7C15));
    let mut medians: Vec<f64> = (0..b)
        .map(|_| {
            let resample: Vec<f64> = (0..n).map(|_| samples[rng.below(n)]).collect();
            median(&resample)
        })
        .collect();
    medians.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let lo_idx = ((0.025 * b as f64).round() as usize).saturating_sub(1);
    let hi_idx = ((0.975 * b as f64).round() as usize)
        .saturating_sub(1)
        .min(b - 1);
    Some((medians[lo_idx], medians[hi_idx]))
}

/// Bootstrap CI half-width as a percentage of the median — the `--runs
/// auto` convergence signal (B=150 resamples). Returns a
/// large sentinel when it can't be computed, so the caller keeps sampling.
pub fn bootstrap_ci_halfwidth_pct(samples: &[f64]) -> f64 {
    if samples.len() < 3 {
        return 999.0;
    }
    let med = median(samples);
    if med == 0.0 {
        return 999.0;
    }
    match bootstrap_median_ci(samples, 150, 0) {
        Some((lo, hi)) => ((hi - lo) / 2.0) / med * 100.0,
        None => 999.0,
    }
}

/// Two-sided Mann-Whitney U test p-value, normal approximation with tie
/// correction.
pub fn mann_whitney_p(a: &[f64], b: &[f64]) -> Option<f64> {
    let (n1, n2) = (a.len(), b.len());
    if n1 == 0 || n2 == 0 {
        return None;
    }
    let mut combined: Vec<(f64, u8)> = a
        .iter()
        .map(|v| (*v, 0u8))
        .chain(b.iter().map(|v| (*v, 1u8)))
        .collect();
    combined.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let n = combined.len();
    let mut ranks = vec![0.0f64; n];
    let mut tie_term = 0.0f64;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && combined[j + 1].0 == combined[i].0 {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0 + 1.0; // 1-based, averaged over ties
        for r in ranks.iter_mut().take(j + 1).skip(i) {
            *r = avg_rank;
        }
        let t = (j - i + 1) as f64;
        if t > 1.0 {
            tie_term += t * t * t - t;
        }
        i = j + 1;
    }
    let r1: f64 = ranks
        .iter()
        .zip(&combined)
        .filter(|(_, (_, g))| *g == 0)
        .map(|(r, _)| r)
        .sum();
    let u1 = r1 - (n1 * (n1 + 1)) as f64 / 2.0;
    let mu_u = (n1 * n2) as f64 / 2.0;
    if n <= 1 {
        return Some(1.0);
    }
    let nf = n as f64;
    let sigma_u = ((n1 * n2) as f64 / 12.0 * ((nf + 1.0) - tie_term / (nf * (nf - 1.0)))).sqrt();
    if sigma_u == 0.0 {
        return Some(1.0);
    }
    let z = if u1 > mu_u {
        (u1 - mu_u - 0.5) / sigma_u
    } else if u1 < mu_u {
        (u1 - mu_u + 0.5) / sigma_u
    } else {
        0.0
    };
    let p = 2.0 * (1.0 - normal_cdf(z.abs()));
    Some(p.clamp(0.0, 1.0))
}

fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// Abramowitz–Stegun 7.1.26 rational approximation of erf (|error| ≤
/// 1.5e-7) — plenty for a p-value compared against 0.05, and keeps the
/// binary free of a libm/special-functions dependency.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_even_odd() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    fn p95_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        assert_eq!(p95(&v), 19.0); // (20*95+99)/100 = 19
        let v: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        assert_eq!(p95(&v), 10.0); // (10*95+99)/100 = 10
    }

    #[test]
    fn tail_p95_sees_trials_the_filter_dropped() {
        let mut raw: Vec<f64> = (0..18).map(|i| 10.0 + (i % 3) as f64).collect();
        raw.extend([40.0, 45.0]);
        let (removed, kept) = iqr_filter(&raw);
        assert_eq!(removed, 2);
        assert_eq!(p95(&kept), 12.0);
        assert_eq!(tail_p95(&raw, median(&kept)), 40.0);
        // never below the median it is reported with
        assert_eq!(tail_p95(&[1.0, 2.0], 5.0), 5.0);
    }

    #[test]
    fn stddev_population() {
        assert_eq!(stddev(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]), 2.0);
    }

    #[test]
    fn iqr_keeps_small_sets() {
        assert_eq!(iqr_filter(&[1.0, 100.0, 2.0]), (0, vec![1.0, 100.0, 2.0]));
    }

    #[test]
    fn iqr_removes_outlier() {
        let mut v: Vec<f64> = (1..=19).map(|i| 10.0 + (i % 3) as f64).collect();
        v.push(1000.0);
        let (removed, kept) = iqr_filter(&v);
        assert_eq!(removed, 1);
        assert_eq!(kept.len(), 19);
    }

    #[test]
    fn iqr_keeps_trial_order() {
        let v = [12.0, 10.0, 11.0, 900.0, 10.5, 11.5, 10.2, 11.8];
        let (removed, kept) = iqr_filter(&v);
        assert_eq!(removed, 1);
        assert_eq!(kept, vec![12.0, 10.0, 11.0, 10.5, 11.5, 10.2, 11.8]);
    }

    #[test]
    fn mann_whitney_identical_is_insignificant() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let p = mann_whitney_p(&a, &a).unwrap();
        assert!(p > 0.9, "p={p}");
    }

    #[test]
    fn mann_whitney_disjoint_is_significant() {
        let a: Vec<f64> = (1..=15).map(|i| i as f64).collect();
        let b: Vec<f64> = (100..=115).map(|i| i as f64).collect();
        let p = mann_whitney_p(&a, &b).unwrap();
        assert!(p < 0.001, "p={p}");
    }
}
