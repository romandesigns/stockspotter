//! Per-symbol order-flow engine: the one stateful entry point the
//! ingestion layer drives, and the summary everything downstream reads.
//!
//! Ties together classification (`classify`), rolling windows (`window`),
//! large trades (`large`), volume at price (`profile`) and top-of-book
//! changes (`book`), and adds the features that need more than one of
//! them: absorption and price impact (specification sections 9 and 14),
//! exhaustion (10), sweeps (11) and the plain-language interpretation
//! layer (25).
//!
//! **Every threshold in this file is an untuned default.** They exist so
//! the features can be recorded and looked at; none has been validated
//! against outcomes, and the specification forbids using any of this to
//! gate an alert until that has happened. The level names (LOW / HIGH /
//! EXTREME) are relative to the symbol's own recent behaviour, not to any
//! absolute standard.
//!
//! Normalisation, which is what lets one set of thresholds apply to a
//! $0.40 name and a $400 name: aggressive volume and price displacement
//! in a 30-second window are both expressed as multiples of that symbol's
//! own median over its last completed 30-second buckets. "Delta is 4x
//! normal while price moved 0.2x normal" means the same thing on any
//! stock; "580K shares and one cent" does not.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::book::{BookSummary, BookTopTracker};
use crate::classify::{Classifier, ClassifierConfig, TradeSide};
use crate::large::LargeTradeDetector;
pub use crate::levels::{AbsorptionLevel, AbsorptionSide};
use crate::levels::AbsorptionLevels;
use crate::profile::{ProfileSummary, VolumeProfile};
use crate::tick::{Quote, Trade};
use crate::window::{ClassifiedTrade, FlowTracker, Session, WindowStats};

const BUCKET_SECS: f64 = 30.0;
/// Completed buckets used for the "normal" baseline.
const BASELINE_BUCKETS: usize = 20;
/// Fewer than this and there is no baseline, so no absorption verdict.
const MIN_BASELINE_BUCKETS: usize = 5;
/// 30-second buckets kept: a whole 16-hour market day (04:00 to 20:00
/// ET), so a backfilled session keeps its start. Was 8 hours until the
/// first backfilled run showed a mid-afternoon session already past it.
/// What a client is SENT is limited separately, at the endpoint.
const MAX_BUCKETS: usize = 1920;
const MAX_LARGE_PRINTS: usize = 200;
const PROFILE_BINS: usize = 48;
const SWEEP_MAX_SECS: f64 = 1.0;
const SWEEP_MIN_LEVELS: u32 = 3;
/// Lit exchanges a run must print on to count as crossing the market.
const SWEEP_MIN_VENUES: u32 = 3;
/// Price levels such a run must still step through (one move).
const SWEEP_MIN_LEVELS_ACROSS_VENUES: u32 = 2;
const SWEEP_RETENTION_SECS: f64 = 30.0;
/// Shares per unit of quoted size. Quote sizes arrive in round lots on
/// the SIP feed this runs against (confirmed in the snapshotted Alpaca
/// docs), while trade sizes are shares; this is the one place the two
/// units meet.
const QUOTE_SIZE_SHARES: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    None,
    Low,
    Moderate,
    High,
    Extreme,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Absorption {
    pub level: Level,
    pub side: Option<AbsorptionSide>,
    /// Normalised aggressive volume over normalised displacement in the
    /// aggressor's direction. `None` without a baseline.
    pub intensity: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustionLevel {
    None,
    Possible,
    Likely,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exhaustion {
    pub level: ExhaustionLevel,
    pub velocity_fading: bool,
    pub delta_fading: bool,
    pub large_trades_fading: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeStats {
    /// Current "large" cut-off for this symbol, in shares.
    pub threshold: Option<u64>,
    pub count: u64,
    pub volume: u64,
    pub buy_volume: u64,
    pub sell_volume: u64,
    pub delta: i64,
    /// Share of the window's volume that printed as large trades.
    pub intensity: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepEvent {
    pub timestamp_secs: f64,
    pub direction: TradeSide,
    pub levels: u32,
    pub volume: u64,
    pub dollars: f64,
    pub duration_secs: f64,
    pub displacement: f64,
    /// Distinct lit exchanges the run printed on; 0 when the feed did not
    /// say (backfilled history, or a build before the exchange was read).
    #[serde(default)]
    pub venues: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepStats {
    pub state: Level,
    pub count: u32,
    pub buy_count: u32,
    pub sell_count: u32,
    /// Volume the tape itself flagged as intermarket sweep orders, split
    /// by inferred side. Independent of the price-level detection above.
    pub flagged_buy_volume: u64,
    pub flagged_sell_volume: u64,
    /// How many of `count` printed on at least `SWEEP_MIN_VENUES` lit
    /// exchanges -- the ones that look like one order routed across the
    /// market rather than a fast run on a single book.
    #[serde(default)]
    pub multi_venue_count: u32,
    pub last: Option<SweepEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureSide {
    Buy,
    Sell,
    Balanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LargeTradeRead {
    BuyDominant,
    SellDominant,
    Balanced,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowRead {
    Confirming,
    Diverging,
    Neutral,
}

/// Section 25: the raw numbers turned into the few states a person reads.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interpretation {
    pub pressure_side: PressureSide,
    pub pressure_level: Level,
    pub large_trades: LargeTradeRead,
    /// Whether order flow agrees with what price is doing.
    pub flow: FlowRead,
}

/// One 30-second bucket of aggressive volume, for the delta pane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaBucket {
    pub start_secs: f64,
    pub buy_volume: u64,
    pub sell_volume: u64,
    pub unknown_volume: u64,
    pub open: f64,
    pub close: f64,
    /// Size at the best bid and ask as of this bucket's last trade, in
    /// the feed's own quote units (round lots on SIP). `None` when no
    /// usable quote had been seen. One level only: this is the touch, not
    /// depth.
    pub bid_size: Option<u64>,
    pub ask_size: Option<u64>,
}

/// A large print, for the bubble overlay.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LargePrint {
    pub timestamp_secs: f64,
    pub price: f64,
    pub size: u64,
    pub side: TradeSide,
    /// The provider's own id for this print, when its frame carried one.
    ///
    /// Always serialized, including as `null`, so a row states identity
    /// rather than leaving it to be inferred from a missing field. `null`
    /// means no usable id was retained, not proof the provider omitted it; a row with no `id`
    /// key at all was written by a build that retained none, which is a
    /// different and weaker claim. `#[serde(default)]` so the older rows
    /// still load, as unavailable.
    #[serde(default)]
    pub id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderFlowSummary {
    pub as_of_secs: f64,
    /// False until a quote has been seen: every side is then tick-rule
    /// only, and the book fields mean nothing.
    pub has_quotes: bool,
    pub windows: Vec<WindowStats>,
    pub cumulative_delta_premarket: i64,
    pub cumulative_delta_regular: i64,
    pub cumulative_delta_after_hours: i64,
    pub large: LargeStats,
    pub absorption: Absorption,
    /// Prices where heavy aggressive volume has been absorbed and price
    /// has not traded through, highest first. Liquidity that was hit, not
    /// liquidity known to be resting (see `levels.rs`).
    pub absorption_levels: Vec<AbsorptionLevel>,
    /// Normalised displacement per unit of normalised signed delta over
    /// 30 seconds. Around 1 is ordinary; near 0 is flow that is not
    /// moving price; negative is price moving against the flow.
    pub net_impact_efficiency: Option<f64>,
    pub exhaustion: Exhaustion,
    pub sweeps: SweepStats,
    pub book: BookSummary,
    pub profile: Option<ProfileSummary>,
    pub interpretation: Interpretation,
    pub delta_buckets: Vec<DeltaBucket>,
    pub large_prints: Vec<LargePrint>,
}

/// What the ingestion layer knows about a print that this crate cannot:
/// both come from tape-specific condition codes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TradeFlags {
    /// Auction, late, average-price and similar prints: real volume, but
    /// no aggressor to infer, so the side is left `Unknown`.
    pub unclassifiable: bool,
    /// The tape marked this print as an intermarket sweep order.
    pub sweep: bool,
    /// The exchange code the print was reported on (the feed's `x`), when
    /// known. Used only to count how many venues a sweep touched.
    pub exchange: Option<char>,
    /// The provider's own id for this print (the feed's `i`), when the
    /// frame carried one.
    ///
    /// Purely observational: no classification, threshold, score or
    /// interpretation in this crate reads it, and nothing here treats two
    /// prints as the same print because their ids match. It is carried so
    /// that the bounded `large_prints` a summary already keeps can name
    /// which provider print they were, which a recording otherwise cannot
    /// say at all. Never synthesized -- `None` means no usable provider id was retained.
    pub id: Option<u64>,
}

/// One bit per lit exchange code, or 0 for a code that is not one.
///
/// `D` is the FINRA trade-reporting facility: off-exchange prints
/// (internalisers, dark pools) reported after the fact. It is where a large
/// share of small-cap volume prints, and counting it as a venue would make
/// almost any burst look like it crossed the market.
fn venue_bit(exchange: Option<char>) -> u32 {
    match exchange {
        Some(c) if c.is_ascii_uppercase() && c != 'D' => 1 << (c as u32 - 'A' as u32),
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy)]
struct SweepRun {
    side: TradeSide,
    start_secs: f64,
    last_secs: f64,
    first_price: f64,
    last_price: f64,
    levels: u32,
    volume: u64,
    dollars: f64,
    /// Bitmask of lit exchanges seen (`venue_bit`).
    venues: u32,
}

impl SweepRun {
    fn event(&self) -> Option<SweepEvent> {
        let venues = self.venues.count_ones();
        // Either the original rule (three price levels in a second, which
        // needs no exchange data), or the textbook one: the same aggressor
        // hitting three or more exchanges at once while price steps at
        // least one level. The second catches a sweep that clears size
        // across venues without walking far.
        let qualifies = self.levels >= SWEEP_MIN_LEVELS || (venues >= SWEEP_MIN_VENUES && self.levels >= SWEEP_MIN_LEVELS_ACROSS_VENUES);
        qualifies.then_some(SweepEvent {
            venues,
            timestamp_secs: self.last_secs,
            direction: self.side,
            levels: self.levels,
            volume: self.volume,
            dollars: self.dollars,
            duration_secs: self.last_secs - self.start_secs,
            displacement: self.last_price - self.first_price,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct Slice {
    buy: u64,
    sell: u64,
    unknown: u64,
    large_count: u64,
    large_buy: u64,
    large_sell: u64,
    large_unknown: u64,
    trades: u64,
    first_price: Option<f64>,
    last_price: Option<f64>,
}

impl Slice {
    fn delta(&self) -> i64 {
        self.buy as i64 - self.sell as i64
    }
    fn shares(&self) -> u64 {
        self.buy + self.sell + self.unknown
    }
    fn large_volume(&self) -> u64 {
        self.large_buy + self.large_sell + self.large_unknown
    }
    fn displacement(&self) -> f64 {
        match (self.first_price, self.last_price) {
            (Some(first), Some(last)) => last - first,
            _ => 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrderFlowEngine {
    classifier: Classifier,
    tracker: FlowTracker,
    large: LargeTradeDetector,
    profile: VolumeProfile,
    book: BookTopTracker,
    buckets: VecDeque<DeltaBucket>,
    large_prints: VecDeque<LargePrint>,
    run: Option<SweepRun>,
    sweeps: VecDeque<SweepEvent>,
    flagged_sweeps: VecDeque<(f64, TradeSide, u64)>,
    has_quotes: bool,
    last_trade_secs: Option<f64>,
    last_quote: Option<Quote>,
    levels: AbsorptionLevels,
}

impl Default for OrderFlowEngine {
    fn default() -> Self {
        Self::new(ClassifierConfig::default())
    }
}

impl OrderFlowEngine {
    pub fn new(classifier: ClassifierConfig) -> Self {
        Self {
            classifier: Classifier::new(classifier),
            tracker: FlowTracker::new(),
            large: LargeTradeDetector::default(),
            profile: VolumeProfile::default(),
            book: BookTopTracker::default(),
            buckets: VecDeque::new(),
            large_prints: VecDeque::new(),
            run: None,
            sweeps: VecDeque::new(),
            flagged_sweeps: VecDeque::new(),
            has_quotes: false,
            last_trade_secs: None,
            last_quote: None,
            levels: AbsorptionLevels::default(),
        }
    }

    pub fn on_quote(&mut self, quote: Quote) {
        self.has_quotes = true;
        self.last_quote = Some(quote);
        self.classifier.on_quote(quote);
        self.book.on_quote(quote);
    }

    pub fn on_trade(&mut self, trade: Trade, session: Session, flags: TradeFlags) -> ClassifiedTrade {
        // The classifier always sees the print so its tick memory stays
        // current; an unclassifiable print just does not get a side.
        let inferred = self.classifier.classify(&trade).side;
        let side = if flags.unclassifiable { TradeSide::Unknown } else { inferred };
        let large = self.large.observe(trade.size);
        let classified = ClassifiedTrade { timestamp_secs: trade.timestamp_secs, price: trade.price, size: trade.size, side, large };

        self.last_trade_secs = Some(trade.timestamp_secs);
        self.tracker.on_trade(classified, session);
        self.profile.add(trade.timestamp_secs, trade.price, trade.size, side);
        self.levels.on_trade(trade.price);
        // Absorption levels are re-examined once per 30-second bucket, not
        // per trade: they describe minutes of behaviour, and the scan is a
        // pass over the whole five-minute window.
        if self.record_bucket(&classified) {
            let typical = self.baseline(trade.timestamp_secs).map(|(delta, _)| delta);
            self.levels.evaluate(trade.timestamp_secs, self.tracker.trades(), typical);
        }
        if large {
            self.large_prints.push_back(LargePrint { timestamp_secs: trade.timestamp_secs, price: trade.price, size: trade.size, side, id: flags.id });
            if self.large_prints.len() > MAX_LARGE_PRINTS {
                self.large_prints.pop_front();
            }
        }
        self.record_sweep(&classified, flags.sweep, flags.exchange);
        classified
    }

    /// Returns true when this trade opened a new 30-second bucket.
    fn record_bucket(&mut self, t: &ClassifiedTrade) -> bool {
        let start = (t.timestamp_secs / BUCKET_SECS).floor() * BUCKET_SECS;
        let rolled = self.buckets.back().is_none_or(|b| b.start_secs != start);
        if rolled {
            self.buckets.push_back(DeltaBucket { start_secs: start, buy_volume: 0, sell_volume: 0, unknown_volume: 0, open: t.price, close: t.price, bid_size: None, ask_size: None });
            if self.buckets.len() > MAX_BUCKETS {
                self.buckets.pop_front();
            }
        }
        let bucket = self.buckets.back_mut().expect("just ensured");
        match t.side {
            TradeSide::Buy => bucket.buy_volume += t.size,
            TradeSide::Sell => bucket.sell_volume += t.size,
            TradeSide::Unknown => bucket.unknown_volume += t.size,
        }
        bucket.close = t.price;
        if let Some(quote) = self.last_quote.filter(Quote::is_two_sided) {
            bucket.bid_size = Some(quote.bid_size);
            bucket.ask_size = Some(quote.ask_size);
        }
        rolled
    }

    /// A sweep here is a burst of same-side aggressive prints inside one
    /// second that either steps through at least three successive price
    /// levels, or prints on at least three lit exchanges while stepping
    /// through two (`SweepRun::event`). The exchange is only known for
    /// live prints, so backfilled history is judged on price levels alone.
    /// The tape's own sweep flag is tallied separately.
    fn record_sweep(&mut self, t: &ClassifiedTrade, flagged: bool, exchange: Option<char>) {
        if flagged && t.side != TradeSide::Unknown {
            self.flagged_sweeps.push_back((t.timestamp_secs, t.side, t.size));
        }
        let cutoff = t.timestamp_secs - SWEEP_RETENTION_SECS;
        while self.flagged_sweeps.front().is_some_and(|f| f.0 <= cutoff) {
            self.flagged_sweeps.pop_front();
        }
        while self.sweeps.front().is_some_and(|s| s.timestamp_secs <= cutoff) {
            self.sweeps.pop_front();
        }

        let extends = self.run.is_some_and(|run| {
            t.side == run.side
                && t.timestamp_secs - run.start_secs <= SWEEP_MAX_SECS
                && match run.side {
                    TradeSide::Buy => t.price >= run.last_price,
                    TradeSide::Sell => t.price <= run.last_price,
                    TradeSide::Unknown => false,
                }
        });
        if extends {
            let run = self.run.as_mut().expect("checked above");
            if t.price != run.last_price {
                run.levels += 1;
            }
            run.last_price = t.price;
            run.last_secs = t.timestamp_secs;
            run.volume += t.size;
            run.dollars += t.price * t.size as f64;
            run.venues |= venue_bit(exchange);
            return;
        }
        if let Some(event) = self.run.take().and_then(|run| run.event()) {
            self.sweeps.push_back(event);
        }
        if t.side != TradeSide::Unknown {
            self.run = Some(SweepRun {
                side: t.side,
                start_secs: t.timestamp_secs,
                last_secs: t.timestamp_secs,
                first_price: t.price,
                last_price: t.price,
                levels: 1,
                volume: t.size,
                dollars: t.price * t.size as f64,
                venues: venue_bit(exchange),
            });
        }
    }

    /// Clears everything for a new trading day.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Time of the most recent trade, if any. Lets a recorder skip symbols
    /// that have gone quiet instead of rewriting an unchanged summary.
    pub fn last_trade_secs(&self) -> Option<f64> {
        self.last_trade_secs
    }

    /// Trades whose age is in `[newer, older)` seconds before `now`.
    fn slice(&self, now: f64, newer: f64, older: f64) -> Slice {
        let mut s = Slice::default();
        for t in self.tracker.trades() {
            let age = now - t.timestamp_secs;
            if age < newer || age >= older {
                continue;
            }
            match t.side {
                TradeSide::Buy => s.buy += t.size,
                TradeSide::Sell => s.sell += t.size,
                TradeSide::Unknown => s.unknown += t.size,
            }
            if t.large {
                s.large_count += 1;
                match t.side {
                    TradeSide::Buy => s.large_buy += t.size,
                    TradeSide::Sell => s.large_sell += t.size,
                    TradeSide::Unknown => s.large_unknown += t.size,
                }
            }
            s.trades += 1;
            s.first_price.get_or_insert(t.price);
            s.last_price = Some(t.price);
        }
        s
    }

    /// Median absolute delta and absolute displacement over the last
    /// completed buckets. `None` until there are enough of them.
    fn baseline(&self, now: f64) -> Option<(f64, f64)> {
        let current = (now / BUCKET_SECS).floor() * BUCKET_SECS;
        let completed: Vec<&DeltaBucket> = self.buckets.iter().rev().filter(|b| b.start_secs < current).take(BASELINE_BUCKETS).collect();
        if completed.len() < MIN_BASELINE_BUCKETS {
            return None;
        }
        let delta = median(completed.iter().map(|b| (b.buy_volume as f64 - b.sell_volume as f64).abs()).collect());
        let displacement = median(completed.iter().map(|b| (b.close - b.open).abs()).collect());
        Some((delta, displacement))
    }

    pub fn summary(&self, now: f64) -> OrderFlowSummary {
        let flow = self.tracker.snapshot(now);
        let current = self.slice(now, 0.0, 30.0);
        let previous = self.slice(now, 30.0, 60.0);
        let minute = self.slice(now, 0.0, 60.0);
        let baseline = self.baseline(now);

        let (absorption, net_impact_efficiency) = absorption_and_impact(&current, baseline);
        let exhaustion = exhaustion(&current, &previous);
        let large = LargeStats {
            threshold: self.large.threshold(),
            count: current.large_count,
            volume: current.large_volume(),
            buy_volume: current.large_buy,
            sell_volume: current.large_sell,
            delta: current.large_buy as i64 - current.large_sell as i64,
            intensity: (current.shares() > 0).then(|| current.large_volume() as f64 / current.shares() as f64),
        };
        // The book tracker alone flags "possible persistent liquidity" on
        // any quote that keeps being resized, which on a live tape is
        // nearly every active symbol (seen on the first real run,
        // 2026-10-06: both sides flagged at once on a quiet after-hours
        // name). Size that left the touch only counts as evidence when
        // aggressive volume on that side can account for at least half of
        // it; otherwise it was cancelled, not hit, and says nothing about a
        // refilling order.
        let mut book = self.book.summary(now);
        book.ask.possible_persistent_liquidity &= current.buy * 2 >= book.ask.removed_size * QUOTE_SIZE_SHARES;
        book.bid.possible_persistent_liquidity &= current.sell * 2 >= book.bid.removed_size * QUOTE_SIZE_SHARES;
        let sweeps = self.sweep_stats(now);
        let interpretation = interpret(&minute, &large, &absorption);

        OrderFlowSummary {
            as_of_secs: now,
            has_quotes: self.has_quotes,
            windows: flow.windows,
            cumulative_delta_premarket: flow.cumulative_delta_premarket,
            cumulative_delta_regular: flow.cumulative_delta_regular,
            cumulative_delta_after_hours: flow.cumulative_delta_after_hours,
            large,
            absorption,
            absorption_levels: self.levels.current(),
            net_impact_efficiency,
            exhaustion,
            sweeps,
            book,
            profile: self.profile.summary(PROFILE_BINS),
            interpretation,
            delta_buckets: self.buckets.iter().copied().collect(),
            large_prints: self.large_prints.iter().copied().collect(),
        }
    }

    fn sweep_stats(&self, now: f64) -> SweepStats {
        let recent = |secs: f64| now - secs < SWEEP_RETENTION_SECS && secs <= now;
        // A run still in progress counts as soon as it qualifies; waiting
        // for the next print to close it would hide the sweep happening
        // right now.
        let open = self.run.and_then(|run| run.event()).filter(|e| recent(e.timestamp_secs));
        let events: Vec<SweepEvent> = self.sweeps.iter().copied().filter(|e| recent(e.timestamp_secs)).chain(open).collect();
        let buy_count = events.iter().filter(|e| e.direction == TradeSide::Buy).count() as u32;
        let count = events.len() as u32;
        let flagged = |side: TradeSide| self.flagged_sweeps.iter().filter(|f| f.1 == side && recent(f.0)).map(|f| f.2).sum();
        SweepStats {
            state: match count {
                0 => Level::None,
                1 => Level::Low,
                2..=3 => Level::Moderate,
                4..=7 => Level::High,
                _ => Level::Extreme,
            },
            count,
            buy_count,
            sell_count: count - buy_count,
            flagged_buy_volume: flagged(TradeSide::Buy),
            flagged_sell_volume: flagged(TradeSide::Sell),
            multi_venue_count: events.iter().filter(|e| e.venues >= SWEEP_MIN_VENUES).count() as u32,
            last: events.last().copied(),
        }
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

/// Absorption: unusually heavy one-sided aggression that is not moving
/// price its own way. Reported only when the aggression itself is at
/// least a normal bucket's worth -- "nothing happened and price did not
/// move" is not absorption.
fn absorption_and_impact(current: &Slice, baseline: Option<(f64, f64)>) -> (Absorption, Option<f64>) {
    let none = Absorption { level: Level::None, side: None, intensity: None };
    let Some((normal_delta, normal_displacement)) = baseline else {
        return (none, None);
    };
    let delta = current.delta() as f64;
    if normal_delta <= 0.0 || delta == 0.0 {
        return (none, None);
    }
    // A flat baseline (price did not move in any recent bucket) would
    // divide by zero; one cent keeps the ratio finite and conservative.
    let normal_displacement = normal_displacement.max(0.01);
    let delta_multiple = delta.abs() / normal_delta;
    let displacement = current.displacement();
    let with_flow = (displacement * delta.signum()).max(0.0) / normal_displacement;
    let efficiency = (displacement / normal_displacement) / (delta / normal_delta);
    let impact = (delta_multiple >= 0.25).then_some(efficiency);

    if delta_multiple < 1.0 {
        return (Absorption { intensity: Some(0.0), ..none }, impact);
    }
    let intensity = delta_multiple / with_flow.max(0.25);
    let level = match intensity {
        i if i < 1.5 => Level::None,
        i if i < 3.0 => Level::Low,
        i if i < 6.0 => Level::Moderate,
        i if i < 12.0 => Level::High,
        _ => Level::Extreme,
    };
    let side = (level != Level::None).then_some(if delta > 0.0 { AbsorptionSide::SellSide } else { AbsorptionSide::BuySide });
    (Absorption { level, side, intensity: Some(intensity) }, impact)
}

/// Exhaustion: price still extending the way the previous 30 seconds'
/// flow pushed it, while the participation behind it falls away.
fn exhaustion(current: &Slice, previous: &Slice) -> Exhaustion {
    let prior_delta = previous.delta() as f64;
    let velocity_fading = previous.shares() > 0 && (current.shares() as f64) < 0.7 * previous.shares() as f64;
    let delta_fading = prior_delta != 0.0 && (current.delta() as f64 * prior_delta.signum()) < 0.5 * prior_delta.abs();
    let large_trades_fading = previous.large_volume() > 0 && (current.large_volume() as f64) < 0.5 * previous.large_volume() as f64;
    let still_extending = prior_delta != 0.0 && current.displacement() * prior_delta.signum() > 0.0;
    let signs = u8::from(velocity_fading) + u8::from(delta_fading) + u8::from(large_trades_fading);
    let level = match (still_extending, signs) {
        (true, 3) => ExhaustionLevel::Likely,
        (true, 2) => ExhaustionLevel::Possible,
        _ => ExhaustionLevel::None,
    };
    Exhaustion { level, velocity_fading, delta_fading, large_trades_fading }
}

/// Fewer prints than this in the last minute and there is no pressure or
/// flow read at all. Seen live on the first run (2026-10-06): two trades
/// on an after-hours tape came out as "buy pressure EXTREME, flow
/// diverging", which is arithmetic on noise, not a read.
const MIN_TRADES_FOR_A_READ: u64 = 10;

fn interpret(minute: &Slice, large: &LargeStats, absorption: &Absorption) -> Interpretation {
    // Large-trade dominance is still reported on a thin tape: it describes
    // prints that did happen and claims nothing about direction.
    let thin = minute.trades < MIN_TRADES_FOR_A_READ;
    let classified = if thin { 0 } else { minute.buy + minute.sell };
    let ratio = if classified > 0 { minute.delta() as f64 / classified as f64 } else { 0.0 };
    let pressure_side = match ratio {
        r if r > 0.1 => PressureSide::Buy,
        r if r < -0.1 => PressureSide::Sell,
        _ => PressureSide::Balanced,
    };
    let mut pressure_level = match ratio.abs() {
        _ if classified == 0 => Level::None,
        r if r < 0.15 => Level::Low,
        r if r < 0.35 => Level::Moderate,
        r if r < 0.6 => Level::High,
        _ => Level::Extreme,
    };
    // When most of the volume could not be given a side, a lopsided
    // classified remainder is weak evidence: cap what it can claim.
    if minute.unknown > classified {
        pressure_level = pressure_level.min(Level::Moderate);
    }

    let large_trades = match (large.buy_volume, large.sell_volume) {
        (0, 0) => LargeTradeRead::None,
        (buy, sell) if buy >= 2 * sell => LargeTradeRead::BuyDominant,
        (buy, sell) if sell >= 2 * buy => LargeTradeRead::SellDominant,
        _ => LargeTradeRead::Balanced,
    };

    let displacement = minute.displacement();
    let agrees = match pressure_side {
        PressureSide::Buy => displacement > 0.0,
        PressureSide::Sell => displacement < 0.0,
        PressureSide::Balanced => false,
    };
    let opposes = match pressure_side {
        PressureSide::Buy => displacement < 0.0,
        PressureSide::Sell => displacement > 0.0,
        PressureSide::Balanced => false,
    };
    let flow = if thin {
        FlowRead::Neutral
    } else if absorption.level >= Level::Moderate || opposes {
        FlowRead::Diverging
    } else if agrees {
        FlowRead::Confirming
    } else {
        FlowRead::Neutral
    };

    Interpretation { pressure_side, pressure_level, large_trades, flow }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(t: f64, bid: f64, ask: f64) -> Quote {
        Quote { timestamp_secs: t, bid_price: bid, bid_size: 10, ask_price: ask, ask_size: 10 }
    }

    /// Feeds one print at the ask (a buy) or at the bid (a sell).
    fn print(engine: &mut OrderFlowEngine, t: f64, price: f64, size: u64, buy: bool) -> ClassifiedTrade {
        let (bid, ask) = if buy { (price - 0.01, price) } else { (price, price + 0.01) };
        engine.on_quote(quote(t - 0.001, bid, ask));
        engine.on_trade(Trade { timestamp_secs: t, price, size }, Session::Regular, TradeFlags::default())
    }

    /// Ten ordinary 30-second buckets: about 50K net buying and a five
    /// cent rise each, so "normal" is well defined before the test proper.
    fn with_baseline() -> (OrderFlowEngine, f64) {
        let mut engine = OrderFlowEngine::default();
        let mut price = 2.00;
        for bucket in 0..10 {
            let start = bucket as f64 * 30.0;
            print(&mut engine, start + 1.0, price, 40_000, true);
            print(&mut engine, start + 10.0, price + 0.02, 15_000, false);
            price += 0.05;
            print(&mut engine, start + 20.0, price, 25_000, true);
        }
        (engine, 300.0)
    }

    #[test]
    fn heavy_buying_that_cannot_lift_price_is_sell_side_absorption() {
        // The specification's example: +580K delta for one cent.
        let (mut engine, t0) = with_baseline();
        for i in 0..5 {
            print(&mut engine, t0 + 1.0 + f64::from(i) * 0.2, 2.50, 70_000, true);
        }
        for i in 0..4 {
            print(&mut engine, t0 + 5.0 + f64::from(i) * 0.2, 2.50, 30_000, false);
        }
        for i in 0..5 {
            print(&mut engine, t0 + 8.0 + f64::from(i) * 0.2, 2.51, 70_000, true);
        }
        let s = engine.summary(t0 + 10.0);
        assert_eq!(s.absorption.side, Some(AbsorptionSide::SellSide));
        assert!(s.absorption.level >= Level::High, "got {:?}", s.absorption);
        assert_eq!(s.interpretation.flow, FlowRead::Diverging);
        // Flow this size normally moves price far more than it did.
        assert!(s.net_impact_efficiency.unwrap() < 0.2);
    }

    #[test]
    fn heavy_selling_that_cannot_drop_price_is_buy_side_absorption() {
        let (mut engine, t0) = with_baseline();
        print(&mut engine, t0 + 1.0, 2.50, 400_000, false);
        print(&mut engine, t0 + 9.0, 2.50, 400_000, false);
        let s = engine.summary(t0 + 10.0);
        assert_eq!(s.absorption.side, Some(AbsorptionSide::BuySide));
        assert!(s.absorption.level >= Level::High);
    }

    #[test]
    fn heavy_buying_that_does_move_price_is_not_absorption() {
        let (mut engine, t0) = with_baseline();
        for i in 0..6 {
            print(&mut engine, t0 + 1.0 + f64::from(i) * 0.5, 2.50, 50_000, true);
        }
        for i in 0..6 {
            print(&mut engine, t0 + 6.0 + f64::from(i) * 0.5, 3.20, 50_000, true);
        }
        let s = engine.summary(t0 + 10.0);
        assert_eq!(s.absorption.level, Level::None);
        assert_eq!(s.absorption.side, None);
        assert_eq!(s.interpretation.flow, FlowRead::Confirming);
        assert_eq!(s.interpretation.pressure_side, PressureSide::Buy);
        assert_eq!(s.interpretation.pressure_level, Level::Extreme);
    }

    #[test]
    fn no_absorption_verdict_without_a_baseline_or_without_real_aggression() {
        let mut fresh = OrderFlowEngine::default();
        print(&mut fresh, 1.0, 2.00, 500_000, true);
        let s = fresh.summary(2.0);
        assert_eq!(s.absorption, Absorption { level: Level::None, side: None, intensity: None });
        assert_eq!(s.net_impact_efficiency, None);

        let (mut quiet, t0) = with_baseline();
        print(&mut quiet, t0 + 1.0, 2.50, 500, true);
        assert_eq!(quiet.summary(t0 + 2.0).absorption.level, Level::None);
    }

    #[test]
    fn large_prints_feed_stats_bubbles_and_the_dominance_read() {
        let mut engine = OrderFlowEngine::default();
        for i in 0..100 {
            print(&mut engine, i as f64 * 0.1, 2.00, 100 + (i % 5) * 20, i % 2 == 0);
        }
        print(&mut engine, 11.0, 2.01, 9_000, true);
        print(&mut engine, 12.0, 2.01, 300, false);
        let s = engine.summary(12.5);
        assert_eq!(s.large.count, 1);
        assert_eq!(s.large.buy_volume, 9_000);
        assert_eq!(s.large.delta, 9_000);
        // 9,000 of the window's 23,300 shares.
        assert!((s.large.intensity.unwrap() - 9_000.0 / 23_300.0).abs() < 1e-9);
        assert_eq!(s.large_prints.len(), 1);
        assert_eq!(s.large_prints[0].side, TradeSide::Buy);
        assert_eq!(s.interpretation.large_trades, LargeTradeRead::BuyDominant);
    }

    fn print_on(engine: &mut OrderFlowEngine, t: f64, price: f64, size: u64, exchange: char) -> ClassifiedTrade {
        engine.on_quote(quote(t - 0.001, price - 0.01, price));
        engine.on_trade(Trade { timestamp_secs: t, price, size }, Session::Regular, TradeFlags { exchange: Some(exchange), ..Default::default() })
    }

    #[test]
    fn two_levels_across_three_exchanges_is_a_sweep_and_counts_as_multi_venue() {
        let mut engine = OrderFlowEngine::default();
        // Two price levels only: not a sweep by the price rule alone.
        print_on(&mut engine, 10.00, 2.00, 1_000, 'Q');
        print_on(&mut engine, 10.05, 2.00, 1_000, 'P');
        assert_eq!(engine.summary(10.1).sweeps.count, 0, "one level, however many venues, is not a sweep");
        print_on(&mut engine, 10.10, 2.01, 1_000, 'Z');
        let s = engine.summary(10.2).sweeps;
        assert_eq!((s.count, s.multi_venue_count), (1, 1));
        assert_eq!(s.last.unwrap().venues, 3);
    }

    #[test]
    fn off_exchange_prints_and_repeats_do_not_count_as_venues() {
        let mut engine = OrderFlowEngine::default();
        // D is the FINRA facility; Q twice is still one exchange.
        print_on(&mut engine, 10.00, 2.00, 1_000, 'Q');
        print_on(&mut engine, 10.05, 2.00, 1_000, 'D');
        print_on(&mut engine, 10.10, 2.01, 1_000, 'Q');
        let s = engine.summary(10.2).sweeps;
        assert_eq!(s.count, 0);
        // Price levels alone still qualify, and report the venues honestly.
        print_on(&mut engine, 10.15, 2.02, 1_000, 'D');
        let s = engine.summary(10.2).sweeps;
        assert_eq!((s.count, s.multi_venue_count), (1, 0));
        assert_eq!(s.last.unwrap().venues, 1);
    }

    #[test]
    fn a_sweep_with_no_exchange_data_reports_zero_venues() {
        let mut engine = OrderFlowEngine::default();
        for (i, price) in [2.00, 2.01, 2.02].iter().enumerate() {
            print(&mut engine, 10.0 + i as f64 * 0.1, *price, 2_000, true);
        }
        let s = engine.summary(10.5).sweeps;
        assert_eq!((s.count, s.multi_venue_count, s.last.unwrap().venues), (1, 0, 0));
    }

    #[test]
    fn a_fast_run_through_three_price_levels_is_a_sweep() {
        let mut engine = OrderFlowEngine::default();
        for (i, price) in [2.00, 2.01, 2.02, 2.03].iter().enumerate() {
            print(&mut engine, 10.0 + i as f64 * 0.1, *price, 2_000, true);
        }
        // Counted while still in progress.
        let open = engine.summary(10.5);
        assert_eq!(open.sweeps.count, 1);
        assert_eq!(open.sweeps.state, Level::Low);
        let last = open.sweeps.last.unwrap();
        assert_eq!((last.direction, last.levels, last.volume), (TradeSide::Buy, 4, 8_000));
        assert!((last.displacement - 0.03).abs() < 1e-9);
        // And still exactly once after a later print closes the run.
        print(&mut engine, 15.0, 2.03, 100, false);
        assert_eq!(engine.summary(15.5).sweeps.count, 1);
    }

    #[test]
    fn slow_or_mixed_prints_are_not_a_sweep() {
        let mut slow = OrderFlowEngine::default();
        for (i, price) in [2.00, 2.01, 2.02, 2.03].iter().enumerate() {
            print(&mut slow, 10.0 + i as f64 * 0.6, *price, 2_000, true);
        }
        assert_eq!(slow.summary(13.0).sweeps.count, 0);

        let mut mixed = OrderFlowEngine::default();
        print(&mut mixed, 10.0, 2.00, 2_000, true);
        print(&mut mixed, 10.1, 2.01, 2_000, true);
        print(&mut mixed, 10.2, 2.01, 2_000, false);
        print(&mut mixed, 10.3, 2.02, 2_000, true);
        assert_eq!(mixed.summary(11.0).sweeps.count, 0);
    }

    #[test]
    fn tape_flagged_sweeps_are_tallied_by_side_and_age_out() {
        let mut engine = OrderFlowEngine::default();
        engine.on_quote(quote(0.9, 1.99, 2.00));
        engine.on_trade(Trade { timestamp_secs: 1.0, price: 2.00, size: 700 }, Session::Regular, TradeFlags { sweep: true, ..Default::default() });
        assert_eq!(engine.summary(2.0).sweeps.flagged_buy_volume, 700);
        assert_eq!(engine.summary(40.0).sweeps.flagged_buy_volume, 0);
    }

    #[test]
    fn unclassifiable_prints_count_as_volume_but_never_as_a_side() {
        let mut engine = OrderFlowEngine::default();
        engine.on_quote(quote(0.9, 1.99, 2.00));
        let t = engine.on_trade(Trade { timestamp_secs: 1.0, price: 2.00, size: 5_000 }, Session::Regular, TradeFlags { unclassifiable: true, ..Default::default() });
        assert_eq!(t.side, TradeSide::Unknown);
        let w = engine.summary(1.5).windows[0];
        assert_eq!((w.buy_volume, w.unknown_volume, w.shares), (0, 5_000, 5_000));
    }

    #[test]
    fn exhaustion_needs_price_still_extending_on_fading_participation() {
        let mut engine = OrderFlowEngine::default();
        for i in 0..100 {
            print(&mut engine, i as f64 * 0.1, 2.00, 100, i % 2 == 0);
        }
        // Strong 30 seconds: heavy buying with large prints.
        print(&mut engine, 31.0, 2.10, 40_000, true);
        print(&mut engine, 45.0, 2.20, 40_000, true);
        // Next 30 seconds: price still rises on a trickle.
        print(&mut engine, 61.0, 2.21, 300, true);
        print(&mut engine, 80.0, 2.25, 300, true);
        let s = engine.summary(89.0);
        assert!(s.exhaustion.velocity_fading && s.exhaustion.delta_fading && s.exhaustion.large_trades_fading);
        assert_eq!(s.exhaustion.level, ExhaustionLevel::Likely);

        // Same fade with price no longer extending is just a quiet tape.
        let mut stalled = OrderFlowEngine::default();
        print(&mut stalled, 31.0, 2.10, 40_000, true);
        print(&mut stalled, 61.0, 2.10, 300, true);
        print(&mut stalled, 80.0, 2.10, 300, true);
        assert_eq!(stalled.summary(89.0).exhaustion.level, ExhaustionLevel::None);
    }

    #[test]
    fn mostly_unknown_volume_caps_the_pressure_level() {
        let mut engine = OrderFlowEngine::default();
        for i in 0..10 {
            print(&mut engine, 1.0 + f64::from(i) * 0.05, 2.00, 100, true);
        }
        engine.on_trade(Trade { timestamp_secs: 2.0, price: 2.00, size: 50_000 }, Session::Regular, TradeFlags { unclassifiable: true, ..Default::default() });
        let s = engine.summary(3.0);
        assert_eq!(s.interpretation.pressure_side, PressureSide::Buy);
        assert_eq!(s.interpretation.pressure_level, Level::Moderate);
    }

    #[test]
    fn refilling_ask_is_only_possibly_persistent_when_buying_explains_it() {
        let refill = |engine: &mut OrderFlowEngine| {
            for (i, size) in [20u64, 2, 18, 3, 22, 4, 19].iter().enumerate() {
                engine.on_quote(Quote { timestamp_secs: 10.0 + i as f64, bid_price: 2.24, bid_size: 5, ask_price: 2.25, ask_size: *size });
            }
        };
        // Quote resized over and over with no trades: cancels, not hits.
        let mut resized = OrderFlowEngine::default();
        refill(&mut resized);
        assert!(!resized.summary(17.0).book.ask.possible_persistent_liquidity);

        // Same quotes while buyers lift thousands of shares at that ask.
        let mut hit = OrderFlowEngine::default();
        refill(&mut hit);
        for i in 0..6 {
            hit.on_trade(Trade { timestamp_secs: 16.5 + f64::from(i) * 0.05, price: 2.25, size: 600 }, Session::Regular, TradeFlags::default());
        }
        let book = hit.summary(17.0).book;
        assert!(book.ask.possible_persistent_liquidity);
        assert!(!book.bid.possible_persistent_liquidity);
    }

    #[test]
    fn a_thin_tape_gets_no_pressure_or_flow_read() {
        // Two after-hours prints must not come out as "EXTREME".
        let mut engine = OrderFlowEngine::default();
        print(&mut engine, 1.0, 2.00, 1_000, true);
        print(&mut engine, 2.0, 2.05, 900, true);
        let s = engine.summary(3.0);
        assert_eq!(s.interpretation.pressure_side, PressureSide::Balanced);
        assert_eq!(s.interpretation.pressure_level, Level::None);
        assert_eq!(s.interpretation.flow, FlowRead::Neutral);
        // The raw numbers are still reported as they are.
        assert_eq!(s.windows[4].buy_volume, 1_900);
    }

    #[test]
    fn delta_buckets_are_thirty_seconds_and_carry_open_and_close() {
        let mut engine = OrderFlowEngine::default();
        print(&mut engine, 5.0, 2.00, 100, true);
        print(&mut engine, 25.0, 2.04, 50, false);
        print(&mut engine, 31.0, 2.05, 70, true);
        let s = engine.summary(32.0);
        assert_eq!(s.delta_buckets.len(), 2);
        let first = s.delta_buckets[0];
        assert_eq!((first.start_secs, first.buy_volume, first.sell_volume), (0.0, 100, 50));
        assert_eq!((first.open, first.close), (2.00, 2.04));
        assert_eq!(s.delta_buckets[1].start_secs, 30.0);
    }

    #[test]
    fn summary_reports_whether_quotes_were_ever_seen_and_serialises_camel_case() {
        let mut engine = OrderFlowEngine::default();
        engine.on_trade(Trade { timestamp_secs: 1.0, price: 2.00, size: 100 }, Session::Premarket, TradeFlags::default());
        let s = engine.summary(2.0);
        assert!(!s.has_quotes);
        let json = serde_json::to_string(&s).unwrap();
        for key in ["asOfSecs", "hasQuotes", "netImpactEfficiency", "deltaBuckets", "largePrints", "cumulativeDeltaPremarket", "absorptionLevels", "bidSize", "buyVolume"] {
            assert!(json.contains(key), "missing {key}");
        }
    }
}
