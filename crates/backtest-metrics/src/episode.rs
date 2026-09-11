//! `OpportunityEpisode` — the research unit of analysis.
//!
//! Milestone A measured production emitting **~650 events/second**. A single
//! continuous move can produce hundreds of `momentum_update` and
//! `ignition_event` records, and treating those as independent samples would
//! overstate n by orders of magnitude while violating independence outright.
//! An episode is one opportunity; the raw events remain untouched beneath it.
//!
//! This is a **measurement abstraction only**. It does not replace the live
//! protocol, is not broadcast to clients, and never gates a decision.
//!
//! # Boundary rules, fixed before any outcome was examined
//!
//! The rules below were chosen from contemporaneous signals only, and
//! deliberately mirror mechanisms the system already relies on, rather than
//! being invented to flatter a statistic:
//!
//! 1. **Open** — the first *qualifying* observation for a symbol in a session.
//!    Qualifying means an edge-triggered signal moment, exactly as
//!    `signals.rs` / `live_signals.rs` already define one (funnel flip to
//!    passed, momentum flip to qualifying, ignition follow-through confirmed,
//!    consolidation entry triggered). Diagnostic-only phases
//!    (`SurgeDetected`, `ConsolidationConfirmed`, `CandidateOpened`) do not
//!    open an episode; they are recorded once one is open.
//! 2. **Continue** — any further observation of the same symbol while the
//!    episode is active extends it and refreshes its activity clock.
//! 3. **Close on inactivity** — `INACTIVITY_TIMEOUT_SECS` with no observation.
//!    Set to 300s to match `UNIVERSE_MONITOR_IDLE_SECS` and
//!    `QUIET_TRANSITION_GRACE`, both already 300s in `market_data::live`.
//! 4. **Close on invalidation** — an explicit
//!    `IgnitionEventKind::FollowThroughRejected`. The detector itself is
//!    declaring the move over; nothing later is needed to know that.
//! 5. **Close on session boundary** — a UTC date change closes the episode.
//!    This matches the existing `AlreadyEnteredToday` trading gate and the
//!    discovery protocol's per-session labelling.
//!
//! **Every rule uses only information available at or before the moment it
//! fires.** None consults eventual peak, MFE, or whether a trade won. That
//! property is asserted directly by `boundaries_never_depend_on_future_prices`.

use chrono::{DateTime, Datelike, Utc};
use market_data::events::{ConsolidationEventKind, IgnitionEventKind, ScanEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::context::{FeatureCache, SignalContext};
use crate::signals::Strategy;

pub const EPISODE_SCHEMA_VERSION: u32 = 1;

/// Matches `market_data::live`'s existing 300s idle/grace constants rather
/// than introducing a third, unrelated timeout.
pub const INACTIVITY_TIMEOUT_SECS: i64 = 300;

/// Deterministic episode identity: `symbol + session_date + sequence`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeId {
    pub symbol: String,
    pub session_date: String,
    /// 1-based; a second distinct move in the same symbol on the same day is
    /// sequence 2, not a continuation of sequence 1.
    pub sequence: u32,
}

impl EpisodeId {
    pub fn as_key(&self) -> String {
        format!("{}:{}:{}", self.symbol, self.session_date, self.sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeCloseReason {
    Inactivity,
    Invalidated,
    SessionBoundary,
    /// Capture ended while the episode was still live. Distinct from the
    /// others because it says nothing about the opportunity -- it says
    /// something about us. Downstream this must be treated as censored.
    CaptureEnded,
}

/// One confirmation observed during an episode's life.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmation {
    pub strategy: Strategy,
    pub at: DateTime<Utc>,
    pub price: f64,
}

/// A momentum reading over the episode's life, for evolution analysis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentumPoint {
    pub at: DateTime<Utc>,
    pub overall: f64,
    pub qualifies: bool,
}

/// Auto-trader linkage. Populated from the journal, keyed by episode, so
/// detector quality and execution quality can be separated -- Milestone A
/// found this architecturally possible but empirically blocked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraderLinkage {
    pub considered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityEpisode {
    pub schema_version: u32,
    pub id: EpisodeId,
    pub opened_at: DateTime<Utc>,
    /// The strategy whose edge-trigger opened this episode.
    pub opened_by: Strategy,
    pub opening_price: f64,
    /// Everything known at the opening moment.
    pub opening_context: SignalContext,
    pub confirmations: Vec<Confirmation>,
    pub momentum_track: Vec<MomentumPoint>,
    /// Highest and lowest prices *observed while the episode was open*.
    /// These are descriptive, not outcome measures -- they are bounded by the
    /// episode's own life and must not be confused with MFE/MAE over a fixed
    /// forward horizon, which `outcome.rs` computes independently.
    pub observed_high: f64,
    pub observed_low: f64,
    pub last_observed_at: DateTime<Utc>,
    pub last_price: f64,
    pub event_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<EpisodeCloseReason>,
    #[serde(default)]
    pub trader: TraderLinkage,
    /// Research-only contemporaneous rank. See `rank` module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_rank: Option<ResearchRank>,
    /// Multi-horizon outcome, populated once enough forward observation has
    /// accumulated -- or with explicit censoring where it has not. Absent
    /// until the episode's outcome window is settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<crate::horizon::HorizonOutcome>,
}

/// Research-only ranking telemetry.
///
/// Records where this episode stood among *simultaneously live* episodes by
/// the existing continuous momentum score. It exists to answer one question:
/// **does the score Stockspotter already computes, and currently throws away
/// at the 0.60 gate, contain useful ordering information?**
///
/// It never affects clients, the auto-trader, signal ordering, or suppression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRank {
    /// 1-based position among live episodes at `ranked_at`.
    pub rank: u32,
    /// How many episodes were live in that comparison.
    pub cohort_size: u32,
    pub score: f64,
    pub ranked_at: DateTime<Utc>,
    /// Identifies the ranking window, so a cohort can be reassembled at
    /// analysis time without re-deriving it from timestamps.
    pub window_id: String,
}

impl OpportunityEpisode {
    fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }
}

/// Builds episodes incrementally from the live event stream.
///
/// Deliberately not a mutable global: it is an ordinary value the caller owns,
/// so tests can drive it deterministically and a replay can build episodes
/// from historical events with identical code.
#[derive(Debug, Default)]
pub struct EpisodeTracker {
    open: HashMap<String, OpportunityEpisode>,
    sequences: HashMap<String, u32>,
    completed: Vec<OpportunityEpisode>,
    features: FeatureCache,
}

impl EpisodeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    pub fn completed(&self) -> &[OpportunityEpisode] {
        &self.completed
    }

    pub fn open_episodes(&self) -> impl Iterator<Item = &OpportunityEpisode> {
        self.open.values()
    }

    /// Feeds one event through both the feature cache and the episode state
    /// machine. Returns episodes closed by this event, if any.
    pub fn observe(&mut self, event: &ScanEvent, now: DateTime<Utc>) -> Vec<OpportunityEpisode> {
        self.features.observe(event);

        let mut closed = self.expire_inactive(now);
        let Some((symbol, at, price)) = event_symbol_time_price(event) else {
            return closed;
        };

        // Rule 5: a session boundary closes any episode from a prior date.
        if let Some(existing) = self.open.get(&symbol) {
            if existing.opened_at.date_naive() != at.date_naive() {
                if let Some(ep) = self.close(&symbol, at, EpisodeCloseReason::SessionBoundary) {
                    closed.push(ep);
                }
            }
        }

        // Rule 4: explicit invalidation.
        if matches!(
            event,
            ScanEvent::IgnitionEvent { kind: IgnitionEventKind::FollowThroughRejected, .. }
        ) {
            if self.open.contains_key(&symbol) {
                self.record(&symbol, event, at, price);
                if let Some(ep) = self.close(&symbol, at, EpisodeCloseReason::Invalidated) {
                    closed.push(ep);
                }
            }
            return closed;
        }

        if self.open.contains_key(&symbol) {
            // Rule 2: continue.
            self.record(&symbol, event, at, price);
        } else if let Some(strategy) = qualifying_strategy(event) {
            // Rule 1: open. An episode needs a price to be measurable at all;
            // if the opening event carries none, fall back to the last price
            // observed for this symbol, and decline to open if even that is
            // unknown rather than fabricating one.
            let Some(price) = price.or_else(|| self.features.last_price(&symbol)) else {
                return closed;
            };
            let sequence = self.sequences.entry(session_key(&symbol, at)).or_insert(0);
            *sequence += 1;
            let id = EpisodeId {
                symbol: symbol.clone(),
                session_date: at.date_naive().to_string(),
                sequence: *sequence,
            };
            let mut context =
                self.features.snapshot(&symbol, strategy, at, now, price);
            context.episode_id = Some(id.as_key());
            let episode = OpportunityEpisode {
                schema_version: EPISODE_SCHEMA_VERSION,
                id,
                opened_at: at,
                opened_by: strategy,
                opening_price: price,
                opening_context: context,
                confirmations: vec![Confirmation { strategy, at, price }],
                momentum_track: Vec::new(),
                observed_high: price,
                observed_low: price,
                last_observed_at: at,
                last_price: price,
                event_count: 1,
                closed_at: None,
                close_reason: None,
                trader: TraderLinkage::default(),
                research_rank: None,
                outcome: None,
            };
            self.open.insert(symbol.clone(), episode);
        }
        closed
    }

    fn record(&mut self, symbol: &str, event: &ScanEvent, at: DateTime<Utc>, price: Option<f64>) {
        let Some(episode) = self.open.get_mut(symbol) else { return };
        episode.event_count += 1;
        episode.last_observed_at = at;
        if let Some(price) = price.filter(|p| p.is_finite() && *p > 0.0) {
            episode.last_price = price;
            episode.observed_high = episode.observed_high.max(price);
            episode.observed_low = episode.observed_low.min(price);
        }
        if let ScanEvent::MomentumUpdate { overall, qualifies, timestamp, .. } = event {
            episode.momentum_track.push(MomentumPoint {
                at: *timestamp,
                overall: *overall,
                qualifies: *qualifies,
            });
        }
        if let Some(strategy) = qualifying_strategy(event) {
            let price = price.unwrap_or(episode.last_price);
            episode.confirmations.push(Confirmation { strategy, at, price });
        }
    }

    /// Rule 3: inactivity.
    fn expire_inactive(&mut self, now: DateTime<Utc>) -> Vec<OpportunityEpisode> {
        let stale: Vec<String> = self
            .open
            .iter()
            .filter(|(_, ep)| (now - ep.last_observed_at).num_seconds() >= INACTIVITY_TIMEOUT_SECS)
            .map(|(symbol, _)| symbol.clone())
            .collect();
        stale
            .into_iter()
            .filter_map(|symbol| {
                let ep = self.open.get(&symbol)?;
                // An overnight gap is longer than any inactivity timeout, so
                // without this the session boundary would never be the
                // recorded reason -- inactivity would always pre-empt it, and
                // the two mean different things for analysis.
                let (closed_at, reason) = if ep.opened_at.date_naive() != now.date_naive() {
                    (ep.last_observed_at, EpisodeCloseReason::SessionBoundary)
                } else {
                    (
                        ep.last_observed_at + chrono::Duration::seconds(INACTIVITY_TIMEOUT_SECS),
                        EpisodeCloseReason::Inactivity,
                    )
                };
                self.close(&symbol, closed_at, reason)
            })
            .collect()
    }

    fn close(
        &mut self,
        symbol: &str,
        at: DateTime<Utc>,
        reason: EpisodeCloseReason,
    ) -> Option<OpportunityEpisode> {
        let mut episode = self.open.remove(symbol)?;
        // An episode can never close before it opened. Every close path funnels
        // through here, so clamping once covers all of them.
        //
        // Session 001 contained six episodes with `closedAt` a minute *before*
        // `openedAt`, all `SessionBoundary` at the UTC rollover. Cause: the
        // boundary rules close at a timestamp taken from the *event* --
        // `observe` uses the incoming event's `at`, `expire_inactive` uses
        // `last_observed_at`. Detector events do not arrive in timestamp order
        // across midnight (bar-derived events are stamped bar-close, trade
        // events are stamped by the trade), so an episode opened at 00:00:00Z
        // could then see an event stamped 23:59:00Z on the prior date, which
        // both satisfies "different date" and precedes the open.
        //
        // Clamping rather than rejecting: the boundary genuinely happened and
        // the episode genuinely must close; only the recorded instant was
        // wrong. A zero-length episode is honest -- it says "opened and closed
        // at the boundary" -- where a negative one is not representable.
        let at = at.max(episode.opened_at);
        episode.closed_at = Some(at);
        episode.close_reason = Some(reason);
        self.completed.push(episode.clone());
        Some(episode)
    }

    /// Closes everything still open, marking it `CaptureEnded` — censored,
    /// not concluded.
    pub fn finish(&mut self, at: DateTime<Utc>) -> Vec<OpportunityEpisode> {
        let symbols: Vec<String> = self.open.keys().cloned().collect();
        symbols
            .into_iter()
            .filter_map(|s| self.close(&s, at, EpisodeCloseReason::CaptureEnded))
            .collect()
    }

    /// Assigns a research-only rank across currently-open episodes using the
    /// most recent momentum score each has produced. Episodes without a score
    /// are left unranked rather than ranked last, which would assert an
    /// ordering the data does not support.
    ///
    /// Returns how many episodes were ranked, so a caller pacing this on a
    /// timer can tell an empty cohort from a real one and avoid spending a
    /// ranking window on nothing.
    pub fn assign_research_rank(&mut self, now: DateTime<Utc>, window_id: &str) -> usize {
        let mut scored: Vec<(String, f64)> = self
            .open
            .iter()
            .filter(|(_, ep)| ep.is_open())
            .filter_map(|(symbol, ep)| {
                ep.momentum_track.last().map(|p| (symbol.clone(), p.overall))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let cohort = scored.len() as u32;
        let ranked = scored.len();
        for (index, (symbol, score)) in scored.into_iter().enumerate() {
            if let Some(ep) = self.open.get_mut(&symbol) {
                ep.research_rank = Some(ResearchRank {
                    rank: index as u32 + 1,
                    cohort_size: cohort,
                    score,
                    ranked_at: now,
                    window_id: window_id.to_string(),
                });
            }
        }
        ranked
    }

    /// Attaches auto-trader linkage to the open episode for `symbol`.
    pub fn link_trader(&mut self, symbol: &str, apply: impl FnOnce(&mut TraderLinkage)) -> bool {
        match self.open.get_mut(symbol) {
            Some(ep) => {
                apply(&mut ep.trader);
                true
            }
            None => false,
        }
    }
}

fn session_key(symbol: &str, at: DateTime<Utc>) -> String {
    format!("{}:{}-{}-{}", symbol, at.year(), at.month(), at.day())
}

/// Which strategy, if any, this event constitutes an edge-triggered signal
/// for. Mirrors `signals.rs` / `live_signals.rs` exactly: diagnostic-only
/// phases do not qualify.
fn qualifying_strategy(event: &ScanEvent) -> Option<Strategy> {
    match event {
        ScanEvent::FunnelSignal { passed: true, .. } => Some(Strategy::FastFunnel),
        ScanEvent::MomentumUpdate { qualifies: true, .. } => Some(Strategy::MomentumScorer),
        ScanEvent::IgnitionEvent { kind: IgnitionEventKind::FollowThroughConfirmed, .. } => {
            Some(Strategy::IgnitionDetector)
        }
        ScanEvent::ConsolidationEvent {
            kind: ConsolidationEventKind::EntryTriggered,
            strategy,
            ..
        } => Some(match strategy {
            market_data::events::ConsolidationStrategy::ConsolidationBreakout => {
                Strategy::ConsolidationBreakout
            }
            market_data::events::ConsolidationStrategy::Micropullback => Strategy::Micropullback,
        }),
        _ => None,
    }
}

/// Symbol, market time, and the event's own price if it carries one.
///
/// `MomentumUpdate` and `CatalystUpdate` genuinely have no price. Returning
/// `None` rather than a sentinel keeps a non-price out of the arithmetic --
/// an earlier revision used `f64::NAN` here, which silently produced episodes
/// whose `openingPrice` serialized as JSON `null` and could not be read back.
fn event_symbol_time_price(event: &ScanEvent) -> Option<(String, DateTime<Utc>, Option<f64>)> {
    match event {
        ScanEvent::FunnelSignal { symbol, timestamp, price, .. }
        | ScanEvent::IgnitionEvent { symbol, timestamp, price, .. }
        | ScanEvent::ConsolidationEvent { symbol, timestamp, price, .. } => {
            Some((symbol.clone(), *timestamp, Some(*price)))
        }
        ScanEvent::HaltWarning { symbol, timestamp, current_price, .. } => {
            Some((symbol.clone(), *timestamp, Some(*current_price)))
        }
        ScanEvent::BarUpdate { symbol, timestamp, close, .. } => {
            Some((symbol.clone(), *timestamp, Some(*close)))
        }
        ScanEvent::MomentumUpdate { symbol, timestamp, .. }
        | ScanEvent::CatalystUpdate { symbol, timestamp, .. } => {
            Some((symbol.clone(), *timestamp, None))
        }
        ScanEvent::FunnelHealth { .. } => None,
    }
}

/// One auto-trader decision, reduced to the fields an episode join needs.
///
/// Deliberately *not* `auto_trader::JournalEntry`: `backtest-metrics` does not
/// depend on `auto-trader`, and inverting that would couple measurement to the
/// thing being measured. A tiny adapter converts a journal line into this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraderDecision {
    pub symbol: String,
    pub at: DateTime<Utc>,
    pub kind: TraderDecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraderDecisionKind {
    Considered,
    Skipped,
    Entered,
    Exited,
}

/// How far past an episode's close a decision may still be attributed to it.
///
/// An exit legitimately lands after the episode stopped producing detector
/// events -- the position outlives the signal. Entries and skips do not get
/// this latitude, because a fresh entry long after an episode went quiet is
/// more likely a new opportunity than a late reaction to an old one.
pub const EXIT_ATTRIBUTION_GRACE_SECS: i64 = 3600;

/// Attaches auto-trader decisions to the episodes they belong to.
///
/// The auto-trader runs as its own process and reaches ws-server over a
/// WebSocket, so there is no in-process handle to link through at runtime and
/// no way to create one without changing the wire protocol or the trader's
/// behaviour. Joining after the fact on `(symbol, time)` is the smallest
/// mechanism that works, and it is purely observational: it reads the journal
/// the trader already writes and never influences a decision.
///
/// Where several episodes for a symbol could match, the latest one opened at
/// or before the decision wins -- a decision cannot belong to an episode that
/// had not started when it was made.
pub fn link_trader_decisions(
    episodes: &mut [OpportunityEpisode],
    decisions: &[TraderDecision],
) -> LinkReport {
    let mut report = LinkReport::default();
    for decision in decisions {
        let grace = match decision.kind {
            TraderDecisionKind::Exited => EXIT_ATTRIBUTION_GRACE_SECS,
            _ => 0,
        };
        let mut best: Option<usize> = None;
        for (index, episode) in episodes.iter().enumerate() {
            if episode.id.symbol != decision.symbol || episode.opened_at > decision.at {
                continue;
            }
            let closed_by = episode
                .closed_at
                .map(|c| c + chrono::Duration::seconds(grace));
            if closed_by.is_some_and(|limit| decision.at > limit) {
                continue;
            }
            let better = best.is_none_or(|b| episodes[b].opened_at < episode.opened_at);
            if better {
                best = Some(index);
            }
        }
        match best {
            Some(index) => {
                apply_decision(&mut episodes[index].trader, decision);
                report.linked += 1;
            }
            // Counted, never guessed at. An unattributable decision is
            // evidence about coverage, not something to force onto the
            // nearest episode.
            None => report.unmatched += 1,
        }
    }
    report
}

fn apply_decision(linkage: &mut TraderLinkage, decision: &TraderDecision) {
    match decision.kind {
        TraderDecisionKind::Considered => linkage.considered = true,
        TraderDecisionKind::Skipped => {
            linkage.considered = true;
            linkage.skip_reason = decision.reason.clone();
        }
        TraderDecisionKind::Entered => {
            linkage.considered = true;
            linkage.entered_at = Some(decision.at);
            linkage.entry_price = decision.price;
        }
        TraderDecisionKind::Exited => {
            linkage.exited_at = Some(decision.at);
            linkage.exit_price = decision.price;
            linkage.exit_reason = decision.reason.clone();
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReport {
    pub linked: u32,
    /// Decisions with no episode to attribute them to. A non-zero value is a
    /// real finding about measurement coverage, not a failure to hide.
    pub unmatched: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
    }

    fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
        ScanEvent::IgnitionEvent {
            symbol: symbol.into(),
            timestamp: t,
            price,
            kind: IgnitionEventKind::FollowThroughConfirmed,
        }
    }

    fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
        ScanEvent::IgnitionEvent {
            symbol: symbol.into(),
            timestamp: t,
            price,
            kind: IgnitionEventKind::FollowThroughRejected,
        }
    }

    fn candidate(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
        ScanEvent::IgnitionEvent {
            symbol: symbol.into(),
            timestamp: t,
            price,
            kind: IgnitionEventKind::CandidateOpened,
        }
    }

    fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64, qualifies: bool) -> ScanEvent {
        ScanEvent::MomentumUpdate {
            symbol: symbol.into(),
            timestamp: t,
            volume_confirmation: 0.5,
            structure: 0.5,
            ma_slope: 0.5,
            wick_rejection: 0.5,
            overall,
            qualifies,
        }
    }

    #[test]
    fn the_first_qualifying_observation_opens_an_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&candidate("AAA", at(0), 10.0), at(0));
        assert_eq!(tracker.open_count(), 0, "a diagnostic phase must not open an episode");

        tracker.observe(&confirmed("AAA", at(10), 10.5), at(10));
        assert_eq!(tracker.open_count(), 1);
        let ep = tracker.open_episodes().next().unwrap();
        assert_eq!(ep.opened_by, Strategy::IgnitionDetector);
        assert_eq!(ep.id.sequence, 1);
        assert_eq!(ep.opening_price, 10.5);
    }

    #[test]
    fn repeated_updates_stay_in_the_same_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        for n in 1..200 {
            tracker.observe(&momentum("AAA", at(n), 0.7, true), at(n));
        }
        assert_eq!(tracker.open_count(), 1, "200 updates are one opportunity, not 200");
        let ep = tracker.open_episodes().next().unwrap();
        assert_eq!(ep.event_count, 200);
        assert_eq!(ep.momentum_track.len(), 199);
    }

    #[test]
    fn inactivity_closes_an_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        let closed = tracker.observe(&confirmed("BBB", at(INACTIVITY_TIMEOUT_SECS + 1), 5.0), at(INACTIVITY_TIMEOUT_SECS + 1));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id.symbol, "AAA");
        assert_eq!(closed[0].close_reason, Some(EpisodeCloseReason::Inactivity));
    }

    #[test]
    fn explicit_invalidation_closes_an_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        let closed = tracker.observe(&rejected("AAA", at(30), 9.5), at(30));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].close_reason, Some(EpisodeCloseReason::Invalidated));
        assert_eq!(tracker.open_count(), 0);
    }

    #[test]
    fn a_new_move_after_closure_creates_a_new_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        tracker.observe(&rejected("AAA", at(30), 9.5), at(30));
        tracker.observe(&confirmed("AAA", at(60), 11.0), at(60));
        assert_eq!(tracker.open_count(), 1);
        let ep = tracker.open_episodes().next().unwrap();
        assert_eq!(ep.id.sequence, 2, "a second distinct move is sequence 2");
    }

    #[test]
    fn an_episode_never_closes_before_it_opened_across_utc_midnight() {
        // Regression for the six inverted episodes in Session 001. All were
        // `session_boundary` at the UTC rollover with `closedAt` a minute
        // before `openedAt`.
        //
        // The mechanism: detector events do not arrive in timestamp order
        // across midnight -- bar-derived events are stamped bar-close while
        // trade events are stamped by the trade -- so an episode opened at
        // 00:00:00Z can then see an event stamped 23:59:00Z on the previous
        // date. That satisfies "different date" and closes the episode at a
        // timestamp preceding its own open.
        let mut tracker = EpisodeTracker::new();
        let just_after_midnight = Utc.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap();
        let late_previous_day = Utc.with_ymd_and_hms(2026, 9, 9, 23, 59, 0).unwrap();

        tracker.observe(
            &confirmed("FTFT", just_after_midnight, 2.08),
            just_after_midnight,
        );
        let closed = tracker.observe(
            &confirmed("FTFT", late_previous_day, 2.09),
            late_previous_day,
        );

        let boundary = closed
            .iter()
            .find(|e| e.close_reason == Some(EpisodeCloseReason::SessionBoundary))
            .expect("the out-of-order event must still close the prior episode");
        assert_eq!(boundary.opened_at, just_after_midnight);
        assert!(
            boundary.closed_at.unwrap() >= boundary.opened_at,
            "closedAt {:?} precedes openedAt {:?}",
            boundary.closed_at,
            boundary.opened_at
        );
    }

    #[test]
    fn every_close_path_upholds_closed_at_not_before_opened_at() {
        // The invariant is enforced in `close`, which every path funnels
        // through -- inactivity, invalidation, boundary and shutdown alike.
        let mut tracker = EpisodeTracker::new();
        let opened = Utc.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap();
        tracker.observe(&confirmed("AAA", opened, 1.0), opened);
        // Finish with a timestamp *before* the episode opened.
        let earlier = Utc.with_ymd_and_hms(2026, 9, 9, 12, 0, 0).unwrap();
        for episode in tracker.finish(earlier) {
            assert!(
                episode.closed_at.unwrap() >= episode.opened_at,
                "{} closed before it opened",
                episode.id.symbol
            );
        }
    }

    #[test]
    fn a_session_boundary_closes_an_episode() {
        let mut tracker = EpisodeTracker::new();
        let day_one = Utc.with_ymd_and_hms(2026, 9, 8, 19, 0, 0).unwrap();
        let day_two = Utc.with_ymd_and_hms(2026, 9, 9, 13, 0, 0).unwrap();
        tracker.observe(&confirmed("AAA", day_one, 10.0), day_one);
        let closed = tracker.observe(&confirmed("AAA", day_two, 12.0), day_two);
        assert!(closed.iter().any(|e| e.close_reason == Some(EpisodeCloseReason::SessionBoundary)));
        let ep = tracker.open_episodes().next().unwrap();
        assert_eq!(ep.id.session_date, "2026-09-09");
        assert_eq!(ep.id.sequence, 1, "a new session restarts the sequence");
    }

    #[test]
    fn boundaries_never_depend_on_future_prices() {
        // The same event sequence, differing only in what happens AFTER each
        // episode-opening moment, must produce identical boundaries and
        // identical opening context. This is the anti-lookahead guarantee.
        let run = |future_price: f64| {
            let mut tracker = EpisodeTracker::new();
            tracker.observe(&momentum("AAA", at(0), 0.55, false), at(0));
            tracker.observe(&confirmed("AAA", at(10), 10.0), at(10));
            tracker.observe(&confirmed("AAA", at(20), future_price), at(20));
            tracker.finish(at(30));
            tracker.completed().to_vec()
        };
        let modest = run(10.1);
        let explosive = run(99.0);

        assert_eq!(modest.len(), explosive.len());
        assert_eq!(modest[0].opened_at, explosive[0].opened_at);
        assert_eq!(modest[0].id, explosive[0].id);
        assert_eq!(
            modest[0].opening_context, explosive[0].opening_context,
            "opening context must not vary with what happened later"
        );
        assert_eq!(modest[0].opening_price, explosive[0].opening_price);
    }

    #[test]
    fn research_rank_orders_by_the_existing_momentum_score() {
        let mut tracker = EpisodeTracker::new();
        for (symbol, score) in [("AAA", 0.55), ("BBB", 0.91), ("CCC", 0.72)] {
            tracker.observe(&confirmed(symbol, at(0), 10.0), at(0));
            tracker.observe(&momentum(symbol, at(1), score, score >= 0.6), at(1));
        }
        tracker.assign_research_rank(at(2), "w-1");

        let mut ranked: Vec<(String, u32, f64)> = tracker
            .open_episodes()
            .map(|e| {
                let r = e.research_rank.clone().unwrap();
                (e.id.symbol.clone(), r.rank, r.score)
            })
            .collect();
        ranked.sort_by_key(|r| r.1);
        assert_eq!(ranked[0].0, "BBB");
        assert_eq!(ranked[1].0, "CCC");
        assert_eq!(ranked[2].0, "AAA");
        assert!(ranked.iter().all(|r| r.1 >= 1));
        assert_eq!(tracker.open_episodes().next().unwrap().research_rank.as_ref().unwrap().cohort_size, 3);
    }

    #[test]
    fn an_episode_without_a_momentum_score_is_left_unranked() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        tracker.assign_research_rank(at(1), "w-1");
        assert!(
            tracker.open_episodes().next().unwrap().research_rank.is_none(),
            "unknown score must not be ranked last -- that asserts an ordering we lack"
        );
    }

    #[test]
    fn trader_decisions_attach_to_the_intended_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        tracker.observe(&confirmed("BBB", at(1), 20.0), at(1));

        assert!(tracker.link_trader("AAA", |t| {
            t.considered = true;
            t.entered_at = Some(at(2));
            t.entry_price = Some(10.2);
        }));
        assert!(tracker.link_trader("BBB", |t| {
            t.considered = true;
            t.skip_reason = Some("max_concurrent_positions".into());
        }));
        assert!(!tracker.link_trader("ZZZ", |_| {}), "no episode, no linkage");

        let aaa = tracker.open_episodes().find(|e| e.id.symbol == "AAA").unwrap();
        assert_eq!(aaa.trader.entry_price, Some(10.2));
        assert!(aaa.trader.skip_reason.is_none());
        let bbb = tracker.open_episodes().find(|e| e.id.symbol == "BBB").unwrap();
        assert_eq!(bbb.trader.skip_reason.as_deref(), Some("max_concurrent_positions"));
        assert!(bbb.trader.entered_at.is_none());
    }

    #[test]
    fn capture_end_is_marked_distinctly_from_a_concluded_episode() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        let closed = tracker.finish(at(5));
        assert_eq!(closed.len(), 1);
        assert_eq!(
            closed[0].close_reason,
            Some(EpisodeCloseReason::CaptureEnded),
            "still-open at capture end is censored, not concluded"
        );
    }

    #[test]
    fn an_episode_opened_by_a_priceless_event_still_serializes() {
        // Regression: MomentumUpdate carries no price. An earlier revision
        // used NaN, which serde_json writes as `null` and cannot read back,
        // silently corrupting any capture containing such an episode.
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&candidate("AAA", at(0), 10.0), at(0)); // gives a price
        tracker.observe(&momentum("AAA", at(1), 0.85, true), at(1)); // opens, priceless
        let closed = tracker.finish(at(2));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].opened_by, Strategy::MomentumScorer);
        assert_eq!(closed[0].opening_price, 10.0, "falls back to the last observed price");

        let json = serde_json::to_string(&closed[0]).unwrap();
        assert!(!json.contains("null"), "no field may serialize as null: {json}");
        let back: OpportunityEpisode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, closed[0]);
    }

    #[test]
    fn an_episode_is_declined_rather_than_opened_at_an_invented_price() {
        let mut tracker = EpisodeTracker::new();
        // Momentum qualifies, but this symbol has never been priced.
        tracker.observe(&momentum("ZZZ", at(0), 0.9, true), at(0));
        assert_eq!(
            tracker.open_count(),
            0,
            "an unmeasurable opportunity must not be fabricated at price 0"
        );
    }

    #[test]
    fn an_episode_round_trips_through_json() {
        let mut tracker = EpisodeTracker::new();
        tracker.observe(&confirmed("AAA", at(0), 10.0), at(0));
        tracker.observe(&momentum("AAA", at(1), 0.8, true), at(1));
        let closed = tracker.finish(at(2));
        let json = serde_json::to_string(&closed[0]).unwrap();
        let back: OpportunityEpisode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, closed[0]);
    }
}
