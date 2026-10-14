//! Fetches a symbol's earlier session from Alpaca the first time its order
//! flow is asked for in full, so the numbers cover the session and not just
//! the time since the server happened to start tracking it.
//!
//! The stitching of history onto live data lives in
//! `market_data::order_flow_store` (`begin_backfill` / `finish_backfill`);
//! this module only does the part that needs the network.
//!
//! **Off unless `ORDER_FLOW_BACKFILL` is set**, on top of
//! `ORDER_FLOW_ENABLED`. It makes real requests to the data provider on a
//! user's behalf, which is a decision for whoever runs the server, not a
//! default.
//!
//! What is fetched, and why the two differ:
//! - **Trades from the market-day open (04:00 ET).** These are what
//!   cumulative delta and the volume profile are built from, and a
//!   session's trades are a manageable size.
//! - **Quotes for the last 30 minutes only.** The quote tape is roughly
//!   ten times the trade tape, and a busy name's full session of quotes is
//!   too much to pull on demand. So trades older than 30 minutes are
//!   classified by the tick rule alone (or left unknown), and only the
//!   recent ones get the stronger quote rule. The summary's own
//!   unknown-volume figure shows the effect; it is not hidden, and the
//!   result is reported as `coverage: session` for the engine, never as a
//!   proven complete picture of the day.
//!
//! **What bounds this.** The provider allowance is shared with discovery,
//! the movers scan and every chart backfill, and this spends it on behalf
//! of whoever happens to open a chart:
//!
//! - one backfill at a time across the whole server, and one per symbol
//!   per market day. A request that finds the slot busy simply does not
//!   start one; the client's next full fetch tries again;
//! - `MAX_PER_DAY` backfills per market day in total, so a client cycling
//!   through symbols cannot spend the day's allowance on history. The
//!   allowance counts backfills that actually *started*: every full fetch
//!   of an already-backfilled symbol comes through here, and a budget
//!   charged before eligibility would have been drained in half an hour by
//!   one open chart, without a single provider request being made (see
//!   `decide`);
//! - an explicit page budget per fetch (`PageBudget`), so a symbol whose
//!   tape is far larger than expected fails instead of paging on;
//! - `FETCH_TIMEOUT`, after which the live engine carries on without
//!   history.
//!
//! Every one of those gives up rather than degrading: a refused or failed
//! backfill leaves the symbol reporting `coverage: partial`, which is what
//! it actually has.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use market_data::order_flow_store;
use market_data::{market_day, market_day_open, AlpacaConfig};
use replay_engine::{fetch_historical_quotes_bounded, fetch_historical_trades_bounded, PageBudget};
use tokio::sync::Semaphore;
use tracing::{info, warn};

pub const BACKFILL_ENV: &str = "ORDER_FLOW_BACKFILL";
const QUOTE_LOOKBACK_MINUTES: i64 = 30;
/// A history request that takes longer than this is abandoned; the live
/// engine carries on without it.
const FETCH_TIMEOUT: Duration = Duration::from_secs(90);
/// 10,000 rows a page. Thirty pages is 300,000 trades, far more than a
/// small-cap session, and the point past which this is not a chart
/// backfill any more.
const MAX_TRADE_PAGES: usize = 30;
/// The quote window is half an hour, so a handful of pages covers it even
/// on a very busy name.
const MAX_QUOTE_PAGES: usize = 10;
/// Backfills started per market day, across all symbols and clients.
const MAX_PER_DAY: u32 = 64;

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(BACKFILL_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")))
}

fn slot() -> &'static Arc<Semaphore> {
    static SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOT.get_or_init(|| Arc::new(Semaphore::new(1)))
}

/// How many backfills this process has started today. Its own state, not
/// the store's: this is a provider-spending budget, and it must hold even
/// if the store evicts and readmits the symbols involved.
#[derive(Default)]
struct DayBudget {
    day: Option<NaiveDate>,
    started: u32,
}

impl DayBudget {
    /// Reserves one, or reports that the day's allowance is spent. A
    /// reservation that does not become a request must be handed back with
    /// `refund`.
    fn reserve(&mut self, day: NaiveDate) -> bool {
        if self.day != Some(day) {
            self.day = Some(day);
            self.started = 0;
        }
        if self.started >= MAX_PER_DAY {
            return false;
        }
        self.started += 1;
        true
    }

    /// Gives a reservation back, so the counter holds the number of
    /// backfills that actually started. Ignored if the market day has
    /// rolled since it was taken: the new day's count is not this
    /// reservation's to decrement.
    fn refund(&mut self, day: NaiveDate) {
        if self.day == Some(day) {
            self.started = self.started.saturating_sub(1);
        }
    }
}

fn budget() -> &'static Mutex<DayBudget> {
    static BUDGET: OnceLock<Mutex<DayBudget>> = OnceLock::new();
    BUDGET.get_or_init(Default::default)
}

/// What `maybe_start` resolved to. Separate from the spawn so the order of
/// the checks -- which is the whole correctness of the provider budget --
/// is testable without a provider, a runtime or a flipped switch.
#[derive(Debug)]
enum Decision {
    /// One of the two switches is off. Nothing was touched.
    Off,
    /// Another backfill holds the single slot. The client's next full
    /// fetch tries again.
    SlotBusy,
    /// The day's allowance is spent. The symbol is NOT marked running: a
    /// refused backfill must leave the symbol able to try again another
    /// day rather than stuck reporting a fetch that never happened.
    DayBudgetSpent,
    /// Nothing to fetch -- unknown symbol, or a backfill already ran, is
    /// running, or failed for it today. The reservation is handed back,
    /// because no provider request follows.
    NotEligible,
    /// Go: the engine generation to hand back on completion, and the slot
    /// permit to hold until then.
    Start(u64, tokio::sync::OwnedSemaphorePermit),
}

/// The admission rules, in the order they have to run in:
///
/// 1. **both switches**, before a slot or a reservation is touched at all;
/// 2. the single slot;
/// 3. an atomic budget reservation;
/// 4. eligibility *last*, because `begin` is what flips the symbol to
///    "running" and that must not happen for a fetch that never starts.
///
/// Step 4 can refuse (the ordinary case: every full fetch of a symbol
/// whose backfill is already done arrives here), so its reservation is
/// refunded. The counter therefore holds requests made, not requests
/// considered.
fn decide(switches_on: bool, day: NaiveDate, begin: impl FnOnce() -> Option<u64>) -> Decision {
    if !switches_on {
        return Decision::Off;
    }
    let Ok(permit) = slot().clone().try_acquire_owned() else {
        return Decision::SlotBusy;
    };
    if !budget().lock().unwrap_or_else(|e| e.into_inner()).reserve(day) {
        return Decision::DayBudgetSpent;
    }
    match begin() {
        Some(generation) => Decision::Start(generation, permit),
        None => {
            budget().lock().unwrap_or_else(|e| e.into_inner()).refund(day);
            Decision::NotEligible
        }
    }
}

/// Starts a backfill for `symbol` if one is wanted, affordable and
/// possible right now. Returns immediately; the work happens on its own
/// task.
pub fn maybe_start(cfg: Arc<AlpacaConfig>, symbol: String) {
    let now = Utc::now();
    // Both switches, not just this module's: with `ORDER_FLOW_ENABLED` off
    // there is no engine to replay history into, so a request would be
    // spent for nothing.
    let switches_on = enabled() && order_flow_store::enabled();
    // The generation `begin_backfill` returns is what makes the completion
    // safe -- see `order_flow_store::finish_backfill`.
    let (generation, permit) = match decide(switches_on, market_day(now), || order_flow_store::begin_backfill(&symbol)) {
        Decision::Start(generation, permit) => (generation, permit),
        Decision::DayBudgetSpent => {
            warn!(%symbol, max = MAX_PER_DAY, "order-flow backfill: day budget spent; live data only");
            return;
        }
        Decision::Off | Decision::SlotBusy | Decision::NotEligible => return,
    };
    tokio::spawn(async move {
        let _permit = permit;
        let session_start = market_day_open(market_day(now));
        let quotes_from = (now - chrono::Duration::minutes(QUOTE_LOOKBACK_MINUTES)).max(session_start);
        let (end, trades_from, quotes_from) = (now.to_rfc3339(), session_start.to_rfc3339(), quotes_from.to_rfc3339());

        let fetched = tokio::time::timeout(FETCH_TIMEOUT, async {
            let trades = fetch_historical_trades_bounded(&cfg, &symbol, &trades_from, &end, PageBudget::Pages(MAX_TRADE_PAGES)).await?;
            let quotes = fetch_historical_quotes_bounded(&cfg, &symbol, &quotes_from, &end, PageBudget::Pages(MAX_QUOTE_PAGES)).await?;
            anyhow::Ok((trades, quotes))
        })
        .await;

        let (trades, quotes) = match fetched {
            Ok(Ok(history)) => history,
            Ok(Err(error)) => {
                warn!(%symbol, %error, "order-flow backfill: history request failed; live data only");
                order_flow_store::fail_backfill(&symbol, generation);
                return;
            }
            Err(_) => {
                warn!(%symbol, "order-flow backfill: history request timed out; live data only");
                order_flow_store::fail_backfill(&symbol, generation);
                return;
            }
        };

        let (trade_count, quote_count) = (trades.len(), quotes.len());
        let replay_symbol = symbol.clone();
        // The replay is pure CPU over possibly hundreds of thousands of
        // events; it must not sit on an async worker thread.
        let replay = tokio::task::spawn_blocking(move || {
            let events = order_flow_store::history_events(&trades, &quotes);
            order_flow_store::finish_backfill(&replay_symbol, generation, events);
        })
        .await;
        match replay {
            Ok(()) => info!(%symbol, trade_count, quote_count, "order-flow backfill: session history applied to this engine (quotes cover the recent window only)"),
            Err(error) => {
                warn!(%symbol, %error, "order-flow backfill: replay failed; live data only");
                order_flow_store::fail_backfill(&symbol, generation);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    /// The slot and the day budget are process-wide by design -- they are
    /// a provider allowance, not per-request state -- so the tests that
    /// exercise them hold this and run one at a time.
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Each test starts from a known count rather than assuming zero.
    fn reset_budget() {
        *budget().lock().unwrap_or_else(|e| e.into_inner()) = DayBudget::default();
    }

    fn started() -> u32 {
        budget().lock().unwrap_or_else(|e| e.into_inner()).started
    }

    #[test]
    fn the_day_budget_is_spent_once_and_resets_on_a_new_market_day() {
        let mut budget = DayBudget::default();
        for _ in 0..MAX_PER_DAY {
            assert!(budget.reserve(day(6)));
        }
        assert!(!budget.reserve(day(6)));
        assert!(budget.reserve(day(7)));
        assert_eq!(budget.started, 1);
    }

    #[test]
    fn a_reservation_handed_back_is_not_spent_and_a_rolled_day_keeps_its_own_count() {
        let mut budget = DayBudget::default();
        assert!(budget.reserve(day(6)));
        budget.refund(day(6));
        assert_eq!(budget.started, 0);
        // Nothing to give back, and nothing to underflow.
        budget.refund(day(6));
        assert_eq!(budget.started, 0);
        // A reservation taken before midnight must not decrement the new
        // day's count when it is handed back after it.
        assert!(budget.reserve(day(7)));
        budget.refund(day(6));
        assert_eq!(budget.started, 1);
        assert_eq!(budget.day, Some(day(7)));
    }

    // The four admission paths, in the order `decide` runs them. These are
    // the rules that keep a chart from spending the day's provider
    // allowance, so each one is checked for what it did to the budget as
    // well as for what it returned.

    #[test]
    fn with_either_switch_off_nothing_is_touched() {
        let _exclusive = exclusive();
        // `ORDER_FLOW_BACKFILL` is unset under test, and so is
        // `ORDER_FLOW_ENABLED`: production's own default.
        assert!(!enabled());
        assert!(!market_data::order_flow_store::enabled());
        reset_budget();
        let mut began = false;
        let decision = decide(false, day(6), || {
            began = true;
            Some(1)
        });
        assert!(matches!(decision, Decision::Off));
        // Not even eligibility is consulted: `begin_backfill` would mark
        // the symbol running for a fetch that cannot happen.
        assert!(!began);
        assert_eq!(slot().available_permits(), 1);
        assert_eq!(started(), 0);
    }

    #[test]
    fn a_completed_backfill_asked_for_again_spends_nothing() {
        let _exclusive = exclusive();
        // The ordinary case this budget exists for: a client's full fetch
        // runs every 30 seconds for as long as a chart is open, and every
        // one of them reaches `maybe_start` with the symbol already done.
        reset_budget();
        for _ in 0..(MAX_PER_DAY * 4) {
            assert!(matches!(decide(true, day(6), || None), Decision::NotEligible));
        }
        assert_eq!(started(), 0);
        // The day's allowance is intact, so a symbol that really does need
        // history can still get it.
        assert!(matches!(decide(true, day(6), || Some(7)), Decision::Start(7, _)));
        assert_eq!(started(), 1);
        // And the slot was released each time rather than leaked.
        reset_budget();
        assert_eq!(slot().available_permits(), 1);
    }

    #[test]
    fn an_exhausted_day_refuses_before_marking_the_symbol_running() {
        let _exclusive = exclusive();
        reset_budget();
        for i in 0..MAX_PER_DAY {
            assert!(matches!(decide(true, day(6), || Some(u64::from(i))), Decision::Start(_, _)));
        }
        let mut began = false;
        let decision = decide(true, day(6), || {
            began = true;
            Some(1)
        });
        assert!(matches!(decision, Decision::DayBudgetSpent));
        // The symbol must not be left reporting a backfill that never ran.
        assert!(!began);
        assert_eq!(started(), MAX_PER_DAY);
        reset_budget();
    }

    #[test]
    fn a_busy_slot_refuses_before_the_budget_is_reserved() {
        let _exclusive = exclusive();
        reset_budget();
        let held = slot().clone().try_acquire_owned().unwrap();
        let mut began = false;
        let decision = decide(true, day(6), || {
            began = true;
            Some(1)
        });
        assert!(matches!(decision, Decision::SlotBusy));
        assert!(!began);
        assert_eq!(started(), 0);
        drop(held);
        assert_eq!(slot().available_permits(), 1);
    }

    #[test]
    fn backfill_is_off_until_its_own_switch_is_set() {
        let _exclusive = exclusive();
        // Neither switch is set under test, so nothing here may start a
        // provider request. `maybe_start` returning before it takes the
        // slot is what proves it.
        assert!(!enabled());
        reset_budget();
        let cfg = market_data::AlpacaConfig {
            api_key: String::new(),
            api_secret: String::new(),
            feed: "iex".into(),
            market_ws: String::new(),
            data_base: String::new(),
            trading_base: String::new(),
            fmp_api_key: None,
        };
        maybe_start(Arc::new(cfg), "AAA".into());
        assert_eq!(slot().available_permits(), 1);
        assert_eq!(started(), 0);
    }
}
