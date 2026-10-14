//! Volume at price (specification section 23), built from trades.
//!
//! Answers "where did volume trade?": point of control (POC), value area,
//! and high/low volume nodes. It covers only the trades this process has
//! seen for the symbol, which starts when the symbol becomes tracked, not
//! at the session open. `since_secs` says how far back that is so a
//! consumer can show it rather than imply a full-session profile.
//!
//! Nodes are found on the display-resolution bins (a few dozen), not on
//! raw one-cent ticks: at tick resolution every other price is a local
//! maximum and "node" stops meaning anything.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::classify::TradeSide;

/// One price bin. `volume` is everything that traded there; the buy and
/// sell parts are the aggressive volume on each side (the "footprint"),
/// and whatever is left over could not be given a side.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBin {
    pub price: f64,
    pub volume: u64,
    pub buy_volume: u64,
    pub sell_volume: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    /// Price with the most volume.
    pub poc: f64,
    /// Smallest contiguous price range around the POC holding 70% of volume.
    pub value_area_low: f64,
    pub value_area_high: f64,
    /// High-volume nodes other than the POC, heaviest first. At most two.
    pub hvn: Vec<f64>,
    /// Low-volume nodes, thinnest first. At most two.
    pub lvn: Vec<f64>,
    pub bins: Vec<ProfileBin>,
    pub total_volume: u64,
    pub since_secs: f64,
}

const VALUE_AREA_SHARE: f64 = 0.7;
/// A node must be this prominent relative to the POC to be reported:
/// an HVN at least this heavy, an LVN at most this heavy.
const NODE_SHARE_OF_POC: f64 = 0.4;

#[derive(Debug, Clone, Default)]
pub struct VolumeProfile {
    tick: Option<f64>,
    /// Per tick: total, aggressive buy, aggressive sell.
    bins: BTreeMap<i64, [u64; 3]>,
    total: u64,
    since_secs: Option<f64>,
}

impl VolumeProfile {
    pub fn add(&mut self, timestamp_secs: f64, price: f64, size: u64, side: TradeSide) {
        if price <= 0.0 || size == 0 {
            return;
        }
        // Fixed at the first print so bins never shift under existing
        // volume. Sub-dollar names trade in finer increments.
        let tick = *self.tick.get_or_insert(if price >= 1.0 { 0.01 } else { 0.001 });
        let bin = self.bins.entry((price / tick).round() as i64).or_insert([0; 3]);
        bin[0] += size;
        match side {
            TradeSide::Buy => bin[1] += size,
            TradeSide::Sell => bin[2] += size,
            TradeSide::Unknown => {}
        }
        self.total += size;
        self.since_secs.get_or_insert(timestamp_secs);
    }

    pub fn summary(&self, max_bins: usize) -> Option<ProfileSummary> {
        let tick = self.tick?;
        let lo = *self.bins.keys().next()?;
        let hi = *self.bins.keys().next_back()?;
        let ticks = (hi - lo + 1) as usize;
        let group = ticks.div_ceil(max_bins.max(1)).max(1);
        let count = ticks.div_ceil(group);
        let mut volumes = vec![0u64; count];
        let mut sides = vec![[0u64; 2]; count];
        for (&index, bin) in &self.bins {
            let k = (index - lo) as usize / group;
            volumes[k] += bin[0];
            sides[k][0] += bin[1];
            sides[k][1] += bin[2];
        }
        let center = |k: usize| (lo as f64 + (k * group) as f64 + (group as f64 - 1.0) / 2.0) * tick;

        let mut poc = 0;
        for (k, &volume) in volumes.iter().enumerate() {
            if volume > volumes[poc] {
                poc = k;
            }
        }

        // Grow outward from the POC, always taking the heavier neighbour.
        let target = (self.total as f64 * VALUE_AREA_SHARE).ceil() as u64;
        let (mut low, mut high, mut held) = (poc, poc, volumes[poc]);
        while held < target && (low > 0 || high + 1 < count) {
            let below = if low > 0 { volumes[low - 1] } else { 0 };
            let above = if high + 1 < count { volumes[high + 1] } else { 0 };
            if high + 1 < count && (low == 0 || above >= below) {
                high += 1;
                held += above;
            } else {
                low -= 1;
                held += below;
            }
        }

        let smooth = |k: usize| {
            let from = k.saturating_sub(1);
            let to = (k + 1).min(count - 1);
            volumes[from..=to].iter().sum::<u64>() as f64 / (to - from + 1) as f64
        };
        let prominence = volumes[poc] as f64 * NODE_SHARE_OF_POC;
        let mut hvn: Vec<usize> = Vec::new();
        let mut lvn: Vec<usize> = Vec::new();
        for k in 1..count.saturating_sub(1) {
            let (prev, here, next) = (smooth(k - 1), smooth(k), smooth(k + 1));
            if k != poc && here > prev && here >= next && volumes[k] as f64 >= prominence {
                hvn.push(k);
            }
            if here < prev && here <= next && volumes[k] as f64 <= prominence {
                lvn.push(k);
            }
        }
        hvn.sort_by(|a, b| volumes[*b].cmp(&volumes[*a]));
        lvn.sort_by(|a, b| volumes[*a].cmp(&volumes[*b]));

        Some(ProfileSummary {
            poc: center(poc),
            value_area_low: center(low),
            value_area_high: center(high),
            hvn: hvn.into_iter().take(2).map(center).collect(),
            lvn: lvn.into_iter().take(2).map(center).collect(),
            bins: volumes
                .iter()
                .enumerate()
                .map(|(k, &volume)| ProfileBin { price: center(k), volume, buy_volume: sides[k][0], sell_volume: sides[k][1] })
                .collect(),
            total_volume: self.total,
            since_secs: self.since_secs?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(prints: &[(f64, u64)]) -> VolumeProfile {
        let mut p = VolumeProfile::default();
        for (i, (price, size)) in prints.iter().enumerate() {
            p.add(100.0 + i as f64, *price, *size, TradeSide::Unknown);
        }
        p
    }

    #[test]
    fn empty_profile_has_no_summary() {
        assert_eq!(VolumeProfile::default().summary(48), None);
    }

    #[test]
    fn poc_is_the_heaviest_price_and_totals_add_up() {
        let s = profile(&[(2.00, 100), (2.01, 900), (2.02, 300), (2.01, 100)]).summary(48).unwrap();
        assert!((s.poc - 2.01).abs() < 1e-9);
        assert_eq!(s.total_volume, 1400);
        assert_eq!(s.bins.iter().map(|b| b.volume).sum::<u64>(), 1400);
        assert_eq!(s.since_secs, 100.0);
    }

    #[test]
    fn value_area_holds_seventy_percent_around_the_poc() {
        // 1000 total: 2.03 alone is 500, adding 2.04 (250) reaches 750.
        let s = profile(&[(2.01, 50), (2.02, 100), (2.03, 500), (2.04, 250), (2.05, 100)]).summary(48).unwrap();
        assert!((s.poc - 2.03).abs() < 1e-9);
        assert!((s.value_area_low - 2.03).abs() < 1e-9);
        assert!((s.value_area_high - 2.04).abs() < 1e-9);
    }

    #[test]
    fn finds_a_second_high_volume_node_and_the_thin_area_between() {
        let mut prints = Vec::new();
        for (price, size) in [(2.00, 300), (2.01, 900), (2.02, 1000), (2.03, 800), (2.04, 300)] {
            prints.push((price, size));
        }
        for (price, size) in [(2.05, 60), (2.06, 20), (2.07, 60)] {
            prints.push((price, size));
        }
        for (price, size) in [(2.08, 300), (2.09, 600), (2.10, 700), (2.11, 500), (2.12, 200)] {
            prints.push((price, size));
        }
        let s = profile(&prints).summary(48).unwrap();
        assert!((s.poc - 2.02).abs() < 1e-9);
        assert_eq!(s.hvn.len(), 1);
        assert!((s.hvn[0] - 2.10).abs() < 1e-9);
        assert!((s.lvn[0] - 2.06).abs() < 1e-9);
    }

    #[test]
    fn wide_ranges_are_grouped_down_to_the_bin_limit() {
        let prints: Vec<(f64, u64)> = (0..500).map(|i| (10.0 + i as f64 * 0.01, 100)).collect();
        let s = profile(&prints).summary(48).unwrap();
        assert!(s.bins.len() <= 48);
        assert_eq!(s.bins.iter().map(|b| b.volume).sum::<u64>(), 50_000);
    }

    #[test]
    fn bins_carry_the_buy_and_sell_split_and_leave_unknown_out_of_both() {
        let mut p = VolumeProfile::default();
        p.add(1.0, 2.00, 700, TradeSide::Buy);
        p.add(2.0, 2.00, 200, TradeSide::Sell);
        p.add(3.0, 2.00, 100, TradeSide::Unknown);
        p.add(4.0, 2.01, 50, TradeSide::Sell);
        let s = p.summary(48).unwrap();
        assert_eq!((s.bins[0].volume, s.bins[0].buy_volume, s.bins[0].sell_volume), (1000, 700, 200));
        assert_eq!((s.bins[1].volume, s.bins[1].buy_volume, s.bins[1].sell_volume), (50, 0, 50));
    }

    #[test]
    fn sub_dollar_names_use_a_finer_tick() {
        let s = profile(&[(0.123, 100), (0.124, 500)]).summary(48).unwrap();
        assert!((s.poc - 0.124).abs() < 1e-9);
    }
}
