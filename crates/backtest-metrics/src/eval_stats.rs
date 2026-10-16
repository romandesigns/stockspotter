//! Shared evaluation statistics: a deterministic RNG, a percentile
//! bootstrap over clusters of trades, and a median.
//!
//! Ported (2026-10-16) from the preregistered momentum experiment on branch
//! `ss-momentum-experiment-20261016`
//! (`crates/backtest-metrics/src/momentum_experiment.rs`: `SplitMix64`,
//! `session_bootstrap`, `percentile_ci`, `median`), which its sibling
//! `reclaim_experiment.rs` also reuses. Semantics are kept exactly:
//!
//! * `SplitMix64` is the same generator and the same `below` mapping, so a
//!   seed produces the same stream as in the experiment.
//! * `cluster_bootstrap` is `session_bootstrap` with the cluster key
//!   generalised from a session index to any ordered key. Trades are grouped
//!   by cluster (in key order), each replicate draws as many clusters (with
//!   replacement) as there are, and the statistics are ratio-of-sums means:
//!   mean return, and mean (return minus its matched random mean).
//!
//! One deliberate addition, from `alpha::stats::cluster_bootstrap`: fewer
//! than two clusters returns `None`. Resampling a single cluster reproduces
//! it every time, and a zero-width "interval" would read as certainty.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// splitmix64: tiny, deterministic, good enough for resampling.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..n (n > 0), by 128-bit multiply.
    pub fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }
}

/// FNV-1a over bytes: a stable (cross-platform, cross-version) hash used to
/// derive per-item seeds and deterministic sampling orders. `std`'s hasher is
/// randomly keyed and must not be used for anything that has to reproduce.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ci {
    pub lo: f64,
    pub hi: f64,
}

/// One trade: its cluster, its net return, and the mean net return of its
/// matched random entries.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired<K> {
    pub cluster: K,
    pub ret: f64,
    pub random_mean: f64,
}

/// Percentile bootstrap over clusters for (mean return, mean return minus
/// mean random). `None` with no trades, `b == 0`, or fewer than two clusters.
pub fn cluster_bootstrap<K: Ord + Clone>(trades: &[Paired<K>], b: usize, seed: u64) -> Option<(Ci, Ci)> {
    let mut groups: BTreeMap<K, (f64, f64, usize)> = BTreeMap::new();
    for t in trades {
        let g = groups.entry(t.cluster.clone()).or_default();
        g.0 += t.ret;
        g.1 += t.random_mean;
        g.2 += 1;
    }
    let g: Vec<(f64, f64, usize)> = groups.into_values().collect();
    if g.len() < 2 || b == 0 {
        return None;
    }
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(b);
    let mut diffs = Vec::with_capacity(b);
    for _ in 0..b {
        let (mut r, mut q, mut n) = (0.0, 0.0, 0usize);
        for _ in 0..g.len() {
            let x = g[rng.below(g.len())];
            r += x.0;
            q += x.1;
            n += x.2;
        }
        means.push(r / n as f64);
        diffs.push((r - q) / n as f64);
    }
    Some((percentile_ci(means), percentile_ci(diffs)))
}

/// 2.5th and 97.5th percentiles (nearest rank on the sorted replicates).
pub fn percentile_ci(mut v: Vec<f64>) -> Ci {
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Ci { lo: at(0.025), hi: at(0.975) }
}

pub fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len().is_multiple_of(2) { (v[m - 1] + v[m]) / 2.0 } else { v[m] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_matches_the_reference_stream() {
        // First output of splitmix64 seeded with 0 (Vigna's reference).
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
        let mut r = SplitMix64::new(7);
        for _ in 0..1000 {
            assert!(r.below(13) < 13);
        }
    }

    #[test]
    fn bootstrap_is_deterministic_and_needs_two_clusters() {
        let trades: Vec<Paired<u32>> = (0..40).map(|i| Paired { cluster: i % 8, ret: (i as f64 * 0.37).sin(), random_mean: 0.1 }).collect();
        let a = cluster_bootstrap(&trades, 2000, 42).unwrap();
        let b = cluster_bootstrap(&trades, 2000, 42).unwrap();
        assert_eq!(a, b);
        assert!(a.0.lo <= a.0.hi && a.1.lo <= a.1.hi);
        // diff is mean - 0.1 for every replicate.
        assert!((a.0.lo - 0.1 - a.1.lo).abs() < 1e-12);
        let one: Vec<Paired<u32>> = trades.iter().map(|t| Paired { cluster: 0, ..t.clone() }).collect();
        assert!(cluster_bootstrap(&one, 2000, 42).is_none());
        assert!(cluster_bootstrap::<u32>(&[], 2000, 42).is_none());
    }

    #[test]
    fn median_even_and_odd() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }
}
