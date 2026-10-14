//! Independent, minute-bar research strategies. No I/O, float inference, OI,
//! orders, retrospective relabeling, or dependencies on another detector.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const CONTRACT: &str = "iac-extreme-minute-v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Iac,
    ExtremeMomentum,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Candidate,
    Held,
    Continued,
    Failed,
    Expired,
    Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Bar {
    pub market_open_ms: i64,
    pub received_at_ms: i64,
    pub decision_at_ms: i64,
    pub market_day: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
    pub is_final: bool,
    pub interval_secs: u32,
}

impl Bar {
    pub fn close_at_ms(&self) -> Option<i64> {
        self.market_open_ms.checked_add(60_000)
    }
    fn range(&self) -> f64 {
        self.high - self.low
    }
    fn same_measurements(&self, other: &Self) -> bool {
        self.open == other.open
            && self.high == other.high
            && self.low == other.low
            && self.close == other.close
            && self.volume == other.volume
    }
    fn valid(&self) -> bool {
        [self.open, self.high, self.low, self.close]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0)
            && self.high >= self.open.max(self.close)
            && self.low <= self.open.min(self.close)
            && self.high >= self.low
            && self.volume <= 9_007_199_254_740_991
            && self.market_open_ms >= 0
            && self.market_open_ms % 60_000 == 0
            && self.market_day.len() == 10
            && self.close_at_ms().is_some_and(|end| {
                self.received_at_ms >= end
                    && self.decision_at_ms >= self.received_at_ms
                    && self.decision_at_ms - end <= 90_000
            })
    }
}

/// Frozen measurements belong to the original candidate, not subsequent bars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub baseline_bars: usize,
    pub baseline_range: f64,
    pub baseline_volume: f64,
    pub compression_ratio: Option<f64>,
    pub expansion_ratio: f64,
    pub participation_ratio: f64,
    pub move_fraction: f64,
    pub path_efficiency: Option<f64>,
    pub float_evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    #[serde(rename = "type")]
    pub event_type: String,
    pub contract_version: String,
    pub event_id: String,
    pub lifecycle_id: String,
    pub supersedes: Option<String>,
    pub symbol: String,
    pub strategy: Strategy,
    pub kind: Kind,
    pub market_day: String,
    pub event_time_ms: i64,
    pub received_at_ms: i64,
    pub decision_at_ms: i64,
    pub anchor_available_at_ms: i64,
    pub anchor_event_time_ms: i64,
    pub anchor_price: f64,
    pub observed_price: f64,
    pub reason: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub baseline_bars: usize,
    pub compression_bars: usize,
    pub compression_ratio_max: f64,
    pub compression_envelope_max: f64,
    pub expansion_ratio_min: f64,
    pub iac_participation_min: f64,
    pub anchor_body_min: f64,
    pub extreme_bars: usize,
    pub extreme_move_min: f64,
    pub extreme_range_multiple: f64,
    pub extreme_participation_min: f64,
    pub extreme_efficiency_min: f64,
    pub extreme_close_location_min: f64,
    pub hold_ms: i64,
    pub expiry_ms: i64,
    pub cooldown_ms: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            baseline_bars: 20,
            compression_bars: 5,
            compression_ratio_max: 0.6,
            compression_envelope_max: 2.0,
            expansion_ratio_min: 2.0,
            iac_participation_min: 2.0,
            anchor_body_min: 0.6,
            extreme_bars: 3,
            extreme_move_min: 0.03,
            extreme_range_multiple: 3.0,
            extreme_participation_min: 3.0,
            extreme_efficiency_min: 0.7,
            extreme_close_location_min: 0.75,
            hold_ms: 120_000,
            expiry_ms: 300_000,
            cooldown_ms: 600_000,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.baseline_bars < 2
            || self.compression_bars < 2
            || self.extreme_bars < 2
            || self.baseline_bars > 32
            || self.compression_bars > 31
            || self.extreme_bars > 31
            || self.baseline_bars + self.compression_bars > 63
            || self.baseline_bars + self.extreme_bars > 63
        {
            return Err("invalid bounded windows");
        }
        if self.hold_ms < 120_000
            || self.expiry_ms <= self.hold_ms
            || self.cooldown_ms < self.expiry_ms
            || self.cooldown_ms > 86_400_000
        {
            return Err("invalid lifecycle timing");
        }
        let ratios = [
            self.compression_ratio_max,
            self.compression_envelope_max,
            self.expansion_ratio_min,
            self.iac_participation_min,
            self.anchor_body_min,
            self.extreme_move_min,
            self.extreme_range_multiple,
            self.extreme_participation_min,
            self.extreme_efficiency_min,
            self.extreme_close_location_min,
        ];
        if ratios.iter().any(|x| !x.is_finite() || *x <= 0.0)
            || self.compression_ratio_max > 1.0
            || self.anchor_body_min > 1.0
            || self.extreme_efficiency_min > 1.0
            || self.extreme_close_location_min > 1.0
        {
            return Err("invalid research ratios");
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    pub accepted_bars: u64,
    pub ignored_provisional: u64,
    pub duplicates: u64,
    pub revisions: u64,
    pub gaps: u64,
    pub invalid_inputs: u64,
    pub warmup_bars: u64,
    pub candidates: u64,
    pub events: u64,
}

#[derive(Clone)]
struct Active {
    anchor: Bar,
    evidence: Evidence,
    lifecycle: String,
    last_event: String,
    subsequent: usize,
    held: bool,
}

pub struct Monitor {
    symbol: String,
    run_id: String,
    config: Config,
    enabled: [bool; 2],
    history: VecDeque<Bar>,
    active: [Option<Active>; 2],
    cooldown: [Option<i64>; 2],
    last_open: Option<i64>,
    last_received: Option<i64>,
    day: Option<String>,
    sequence: u64,
    pub counters: Counters,
}

impl Monitor {
    pub fn new(
        symbol: String,
        run_id: String,
        config: Config,
        iac: bool,
        extreme: bool,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        if symbol.is_empty()
            || symbol.len() > 16
            || !symbol
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        {
            return Err("invalid symbol");
        }
        Ok(Self {
            symbol,
            run_id,
            config,
            enabled: [iac, extreme],
            history: VecDeque::new(),
            active: [None, None],
            cooldown: [None, None],
            last_open: None,
            last_received: None,
            day: None,
            sequence: 0,
            counters: Counters::default(),
        })
    }

    fn emit(
        &mut self,
        slot: usize,
        kind: Kind,
        bar: &Bar,
        active: &mut Active,
        reason: &str,
    ) -> Event {
        self.sequence += 1;
        self.counters.events += 1;
        let id = format!("{}:{}:{}", self.run_id, self.symbol, self.sequence);
        let supersedes = (!active.last_event.is_empty()).then(|| active.last_event.clone());
        active.last_event = id.clone();
        Event {
            event_type: "experimental_strategy_update".into(),
            contract_version: CONTRACT.into(),
            event_id: id,
            lifecycle_id: active.lifecycle.clone(),
            supersedes,
            symbol: self.symbol.clone(),
            strategy: if slot == 0 {
                Strategy::Iac
            } else {
                Strategy::ExtremeMomentum
            },
            kind,
            market_day: active.anchor.market_day.clone(),
            event_time_ms: bar.close_at_ms().unwrap_or(bar.market_open_ms),
            received_at_ms: bar.received_at_ms,
            decision_at_ms: bar.decision_at_ms,
            anchor_available_at_ms: active.anchor.decision_at_ms,
            anchor_event_time_ms: active.anchor.close_at_ms().unwrap(),
            anchor_price: active.anchor.close,
            observed_price: if bar.close.is_finite() && bar.close > 0.0 {
                bar.close
            } else {
                active.anchor.close
            },
            reason: reason.into(),
            evidence: active.evidence.clone(),
        }
    }

    /// Invalidates incomplete observations without claiming structural failure.
    pub fn invalidate(&mut self, at: &Bar, reason: &str) -> Vec<Event> {
        let mut events = Vec::new();
        for slot in 0..2 {
            if let Some(mut active) = self.active[slot].take() {
                events.push(self.emit(slot, Kind::Invalidated, at, &mut active, reason));
            }
        }
        self.history.clear();
        events
    }

    pub fn on_gap(&mut self, now_ms: i64, reason: &str) -> Vec<Event> {
        let Some(mut at) = self.history.back().cloned() else {
            debug_assert!(self.active.iter().all(Option::is_none));
            return Vec::new();
        };
        at.received_at_ms = now_ms.max(at.received_at_ms);
        at.decision_at_ms = at.received_at_ms;
        self.invalidate(&at, reason)
    }

    pub fn last_available_ms(&self) -> Option<i64> {
        self.last_received
    }

    pub fn on_bar(&mut self, bar: Bar) -> Vec<Event> {
        if !self.enabled.iter().any(|v| *v) {
            return Vec::new();
        }
        if !bar.is_final || bar.interval_secs != 60 {
            self.counters.ignored_provisional += 1;
            return Vec::new();
        }
        if !bar.valid() || self.last_received.is_some_and(|t| bar.received_at_ms < t) {
            self.counters.invalid_inputs += 1;
            return self.on_gap(self.last_received.unwrap_or(0), "invalid_or_stale_input");
        }
        self.last_received = Some(bar.received_at_ms);
        let mut out = Vec::new();
        if self.last_open.is_some_and(|t| bar.market_open_ms <= t) {
            if self
                .history
                .iter()
                .find(|b| b.market_open_ms == bar.market_open_ms)
                .is_some_and(|b| b.same_measurements(&bar))
            {
                self.counters.duplicates += 1;
                return out;
            }
            self.counters.revisions += 1;
            return self.invalidate(&bar, "revision_or_out_of_order");
        }
        if self.day.as_ref().is_some_and(|d| *d != bar.market_day) {
            out.extend(self.invalidate(&bar, "market_day_changed"));
            self.cooldown = [None, None];
        } else if self
            .last_open
            .is_some_and(|t| bar.market_open_ms - t != 60_000)
        {
            self.counters.gaps += 1;
            out.extend(self.invalidate(&bar, "missing_minute"));
        }
        self.last_open = Some(bar.market_open_ms);
        self.day = Some(bar.market_day.clone());
        self.counters.accepted_bars += 1;
        for slot in 0..2 {
            if !self.enabled[slot] {
                continue;
            }
            if let Some(mut active) = self.active[slot].take() {
                active.subsequent += 1;
                let event_elapsed =
                    bar.close_at_ms().unwrap() - active.anchor.close_at_ms().unwrap();
                // Never use a bar beyond the declared horizon to claim a failure or hold.
                if event_elapsed > self.config.expiry_ms {
                    out.push(self.emit(
                        slot,
                        Kind::Expired,
                        &bar,
                        &mut active,
                        "observation_horizon_ended",
                    ));
                } else if bar.low < active.anchor.low {
                    out.push(self.emit(slot, Kind::Failed, &bar, &mut active, "anchor_low_broken"));
                } else {
                    if !active.held
                        && event_elapsed >= self.config.hold_ms
                        && active.subsequent >= 2
                        && bar.close >= active.anchor.close
                    {
                        active.held = true;
                        out.push(self.emit(
                            slot,
                            Kind::Held,
                            &bar,
                            &mut active,
                            "two_minute_observed_hold",
                        ));
                    }
                    if active.held && bar.close >= active.anchor.close + active.anchor.range() {
                        out.push(self.emit(
                            slot,
                            Kind::Continued,
                            &bar,
                            &mut active,
                            "one_anchor_range_extension",
                        ));
                    } else if event_elapsed >= self.config.expiry_ms {
                        out.push(self.emit(
                            slot,
                            Kind::Expired,
                            &bar,
                            &mut active,
                            "observation_horizon_ended",
                        ));
                    } else {
                        self.active[slot] = Some(active);
                    }
                }
            } else if self.cooldown[slot]
                .is_none_or(|at| bar.close_at_ms().unwrap() - at >= self.config.cooldown_ms)
            {
                let evidence = if slot == 0 {
                    self.iac(&bar)
                } else {
                    self.extreme(&bar)
                };
                if let Some(evidence) = evidence {
                    let mut active = Active {
                        anchor: bar.clone(),
                        evidence,
                        lifecycle: format!(
                            "{}:{}:{}:{}",
                            self.run_id, self.symbol, slot, bar.market_open_ms
                        ),
                        last_event: String::new(),
                        subsequent: 0,
                        held: false,
                    };
                    out.push(self.emit(
                        slot,
                        Kind::Candidate,
                        &bar,
                        &mut active,
                        "research_thresholds_met",
                    ));
                    self.active[slot] = Some(active);
                    self.cooldown[slot] = bar.close_at_ms();
                    self.counters.candidates += 1;
                }
            }
        }
        if self.history.len() < self.config.baseline_bars + self.config.compression_bars {
            self.counters.warmup_bars += 1;
        }
        self.history.push_back(bar);
        while self.history.len() > 64 {
            self.history.pop_front();
        }
        out
    }

    fn iac(&self, bar: &Bar) -> Option<Evidence> {
        let n = self.config.baseline_bars;
        let c = self.config.compression_bars;
        if self.history.len() < n + c {
            return None;
        }
        let tail: Vec<_> = self.history.iter().rev().take(n + c).rev().collect();
        let baseline = &tail[..n];
        let compressed = &tail[n..];
        let normal_range = mean_range(baseline);
        let normal_volume = mean_volume(baseline);
        let compressed_range = mean_range(compressed);
        let compressed_volume = mean_volume(compressed);
        if [
            normal_range,
            normal_volume,
            compressed_range,
            compressed_volume,
            bar.range(),
        ]
        .iter()
        .any(|x| !x.is_finite() || *x <= 0.0)
        {
            return None;
        }
        let high = compressed
            .iter()
            .map(|b| b.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let low = compressed
            .iter()
            .map(|b| b.low)
            .fold(f64::INFINITY, f64::min);
        let compression = compressed_range / normal_range;
        let expansion = bar.range() / compressed_range;
        let participation = bar.volume as f64 / compressed_volume;
        if [
            compression,
            expansion,
            participation,
            bar.close / bar.open - 1.0,
        ]
        .iter()
        .any(|x| !x.is_finite())
        {
            return None;
        }
        if compression > self.config.compression_ratio_max
            || (high - low) / normal_range > self.config.compression_envelope_max
            || expansion < self.config.expansion_ratio_min
            || participation < self.config.iac_participation_min
            || (bar.close - bar.open) / bar.range() < self.config.anchor_body_min
            || bar.close <= high
        {
            return None;
        }
        Some(Evidence {
            baseline_bars: n,
            baseline_range: normal_range,
            baseline_volume: normal_volume,
            compression_ratio: Some(compression),
            expansion_ratio: expansion,
            participation_ratio: participation,
            move_fraction: bar.close / bar.open - 1.0,
            path_efficiency: None,
            float_evidence: "unavailable".into(),
        })
    }

    fn extreme(&self, bar: &Bar) -> Option<Evidence> {
        let n = self.config.baseline_bars;
        let w = self.config.extreme_bars;
        if self.history.len() < n + w - 1 || bar.range() <= 0.0 {
            return None;
        }
        let tail: Vec<_> = self.history.iter().rev().take(n + w - 1).rev().collect();
        let baseline = &tail[..n];
        let mut recent = tail[n..].to_vec();
        recent.push(bar);
        let normal_range = mean_range(baseline);
        let normal_volume = mean_volume(baseline);
        if normal_range <= 0.0 || normal_volume <= 0.0 {
            return None;
        }
        let fractional = baseline.iter().map(|b| b.range() / b.close).sum::<f64>() / n as f64;
        let start = recent[0].open;
        let gain = bar.close - start;
        let mut prior = start;
        let mut path = 0.0;
        for b in &recent {
            path += (b.close - prior).abs();
            prior = b.close;
        }
        if path <= 0.0 {
            return None;
        }
        let efficiency = gain / path;
        let move_fraction = gain / start;
        let participation = mean_volume(&recent) / normal_volume;
        if [
            fractional,
            efficiency,
            move_fraction,
            participation,
            bar.range() / normal_range,
        ]
        .iter()
        .any(|x| !x.is_finite())
        {
            return None;
        }
        if move_fraction
            < self
                .config
                .extreme_move_min
                .max(self.config.extreme_range_multiple * fractional)
            || participation < self.config.extreme_participation_min
            || efficiency < self.config.extreme_efficiency_min
            || recent.iter().filter(|b| b.close > b.open).count() < 2
            || (bar.close - bar.low) / bar.range() < self.config.extreme_close_location_min
        {
            return None;
        }
        Some(Evidence {
            baseline_bars: n,
            baseline_range: normal_range,
            baseline_volume: normal_volume,
            compression_ratio: None,
            expansion_ratio: bar.range() / normal_range,
            participation_ratio: participation,
            move_fraction,
            path_efficiency: Some(efficiency),
            float_evidence: "unavailable".into(),
        })
    }
}

fn mean_range(bars: &[&Bar]) -> f64 {
    bars.iter().map(|b| b.range()).sum::<f64>() / bars.len() as f64
}
fn mean_volume(bars: &[&Bar]) -> f64 {
    bars.iter().map(|b| b.volume as f64).sum::<f64>() / bars.len() as f64
}

#[cfg(test)]
mod tests;
