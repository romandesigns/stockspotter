//! Research-only, one-share market-order approximation. Never routes broker orders.
use chrono::{DateTime, Duration, Utc};
use market_data::Quote;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ExecutionModel {
    pub delay_ms: i64,
    pub slippage_bps_per_side: f64,
    pub fee_bps_per_side: f64,
    pub max_quote_age_ms: i64,
    pub entry_wait_ms: i64,
    pub exit_wait_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Bracket {
    pub target_pct: f64,
    pub stop_pct: f64,
    pub hold_minutes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionResult {
    pub status: &'static str,
    pub exit_reason: Option<&'static str>,
    pub entry_at: Option<DateTime<Utc>>,
    pub exit_at: Option<DateTime<Utc>>,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub entry_spread_pct: Option<f64>,
    pub net_pct: Option<f64>,
    pub pnl_per_share: Option<f64>,
    pub adverse_excursion_pct: Option<f64>,
}

fn valid(q: &Quote) -> bool {
    q.bid_price.is_finite()
        && q.ask_price.is_finite()
        && q.bid_price > 0.0
        && q.ask_price >= q.bid_price
        && q.bid_size > 0
        && q.ask_size > 0
}

// Use the snapshot known at order arrival, never a quote from before the
// latest update. If it is missing/stale/invalid, wait only a bounded interval.
fn fill_quote(
    quotes: &[Quote],
    ready: DateTime<Utc>,
    latest: DateTime<Utc>,
    age_ms: i64,
) -> Option<(usize, DateTime<Utc>)> {
    if ready > latest {
        return None;
    }
    let after = quotes.partition_point(|q| q.timestamp <= ready);
    if after > 0 {
        let q = &quotes[after - 1];
        if valid(q) && ready - q.timestamp <= Duration::milliseconds(age_ms) {
            return Some((after - 1, ready));
        }
    }
    for (index, q) in quotes.iter().enumerate().skip(after) {
        if q.timestamp > latest {
            break;
        }
        if valid(q) {
            return Some((index, q.timestamp));
        }
    }
    None
}

/// `quotes` must be chronological. One share avoids assuming a round-lot
/// conversion or displayed capacity for a $500 order. It does not prove a fill.
pub fn evaluate(
    quotes: &[Quote],
    signal_at: DateTime<Utc>,
    session_close: DateTime<Utc>,
    bracket: Bracket,
    model: ExecutionModel,
) -> ExecutionResult {
    let mut result = ExecutionResult {
        status: "no_entry_quote",
        exit_reason: None,
        entry_at: None,
        exit_at: None,
        entry_price: None,
        exit_price: None,
        entry_spread_pct: None,
        net_pct: None,
        pnl_per_share: None,
        adverse_excursion_pct: None,
    };
    let ready = signal_at + Duration::milliseconds(model.delay_ms);
    let Some((entry_index, entry_at)) = fill_quote(
        quotes,
        ready,
        (ready + Duration::milliseconds(model.entry_wait_ms))
            .min(session_close - Duration::minutes(1)),
        model.max_quote_age_ms,
    ) else {
        return result;
    };
    let entry_quote = &quotes[entry_index];
    let slip = model.slippage_bps_per_side / 10000.0;
    let fee = model.fee_bps_per_side / 10000.0;
    let entry = entry_quote.ask_price * (1.0 + slip);
    result.entry_at = Some(entry_at);
    result.entry_price = Some(entry);
    result.entry_spread_pct =
        Some((entry_quote.ask_price - entry_quote.bid_price) / entry_quote.ask_price * 100.0);
    result.status = "unresolved_exit";
    let deadline = (entry_at + Duration::minutes(bracket.hold_minutes))
        .min(session_close - Duration::minutes(1));
    let target = entry * (1.0 + bracket.target_pct / 100.0);
    let stop = entry * (1.0 - bracket.stop_pct / 100.0);
    let mut triggered = (deadline, "timeout");
    for q in quotes.iter().skip(entry_index) {
        let at = q.timestamp.max(entry_at);
        if at > deadline {
            break;
        }
        if !valid(q) {
            continue;
        }
        if q.bid_price <= stop {
            triggered = (at, "stop");
            break;
        }
        if q.bid_price >= target {
            triggered = (at, "target");
            break;
        }
    }
    result.exit_reason = Some(triggered.1);
    let exit_ready = triggered.0 + Duration::milliseconds(model.delay_ms);
    let Some((exit_index, exit_at)) = fill_quote(
        quotes,
        exit_ready,
        (exit_ready + Duration::milliseconds(model.exit_wait_ms))
            .min(session_close - Duration::nanoseconds(1)),
        model.max_quote_age_ms,
    ) else {
        return result;
    };
    let exit = quotes[exit_index].bid_price * (1.0 - slip);
    let pnl = exit * (1.0 - fee) - entry * (1.0 + fee);
    result.status = "filled";
    result.exit_at = Some(exit_at);
    result.exit_price = Some(exit);
    result.pnl_per_share = Some(pnl);
    result.net_pct = Some(pnl / entry * 100.0);
    let worst = quotes[entry_index..=exit_index]
        .iter()
        .filter(|q| valid(q))
        .map(|q| (q.bid_price / entry - 1.0) * 100.0)
        .fold(0.0, f64::min);
    result.adverse_excursion_pct = Some(worst.min((exit / entry - 1.0) * 100.0));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(ms: i64) -> DateTime<Utc> {
        "2026-09-03T14:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::milliseconds(ms)
    }
    fn q(ms: i64, bid: f64, ask: f64) -> Quote {
        Quote {
            symbol: "TEST".into(),
            timestamp: at(ms),
            bid_price: bid,
            ask_price: ask,
            bid_size: 1,
            ask_size: 1,
        }
    }
    fn model(delay_ms: i64) -> ExecutionModel {
        ExecutionModel {
            delay_ms,
            slippage_bps_per_side: 0.0,
            fee_bps_per_side: 0.0,
            max_quote_age_ms: 1000,
            entry_wait_ms: 2000,
            exit_wait_ms: 60000,
        }
    }
    fn bracket() -> Bracket {
        Bracket {
            target_pct: 2.0,
            stop_pct: 5.0,
            hold_minutes: 1,
        }
    }
    #[test]
    fn flat_market_pays_spread_instead_of_receiving_midpoint_fills() {
        let r = evaluate(
            &[q(0, 9.9, 10.0), q(60000, 9.9, 10.0)],
            at(0),
            at(3600000),
            bracket(),
            model(0),
        );
        assert_eq!(r.status, "filled");
        assert!((r.net_pct.unwrap() + 1.0).abs() < 1e-9);
    }
    #[test]
    fn entry_latency_uses_price_known_at_arrival() {
        let r = evaluate(
            &[q(0, 9.9, 10.0), q(200, 10.9, 11.0), q(60250, 10.9, 11.0)],
            at(0),
            at(3600000),
            bracket(),
            model(250),
        );
        assert_eq!(r.entry_price, Some(11.0));
        assert_eq!(r.entry_at, Some(at(250)));
    }
    #[test]
    fn invalid_latest_quote_cannot_fall_back_to_an_older_valid_book() {
        let r = evaluate(
            &[q(0, 9.9, 10.0), q(200, 11.0, 10.0)],
            at(250),
            at(3600000),
            bracket(),
            model(0),
        );
        assert_eq!(r.status, "no_entry_quote");
    }
    #[test]
    fn stale_or_zero_quotes_do_not_create_fills() {
        for quotes in [vec![q(-2000, 9.9, 10.0)], vec![q(0, 0.0, 0.0)]] {
            assert_eq!(
                evaluate(&quotes, at(0), at(3600000), bracket(), model(0)).status,
                "no_entry_quote"
            );
        }
    }
    #[test]
    fn delayed_stop_fills_at_worse_bid_not_the_stop_threshold() {
        let r = evaluate(
            &[q(0, 9.9, 10.0), q(1000, 9.4, 9.5), q(1100, 9.0, 9.1)],
            at(0),
            at(3600000),
            bracket(),
            model(250),
        );
        assert_eq!(r.exit_reason, Some("stop"));
        assert_eq!(r.exit_price, Some(9.0));
        assert!((r.net_pct.unwrap() + 10.0).abs() < 1e-9);
    }
    #[test]
    fn missing_exit_is_unresolved_not_a_flat_profitable_or_deleted_trade() {
        let r = evaluate(&[q(0, 9.9, 10.0)], at(0), at(3600000), bracket(), model(0));
        assert_eq!(r.status, "unresolved_exit");
        assert!(r.net_pct.is_none());
        assert!(r.entry_at.is_some());
    }

    #[test]
    fn pending_exit_can_wait_for_liquidity_beyond_one_minute() {
        let mut assumptions = model(250);
        assumptions.exit_wait_ms = 23_400_000;
        // Invalidate the book before the delayed order reaches it.
        let r = evaluate(
            &[
                q(0, 9.9, 10.0),
                q(1000, 9.4, 9.5),
                q(1100, 0.0, 0.0),
                q(120000, 8.0, 8.1),
            ],
            at(0),
            at(3600000),
            bracket(),
            assumptions,
        );
        assert_eq!(r.exit_at, Some(at(120000)));
        assert_eq!(r.exit_price, Some(8.0));
    }
    #[test]
    fn timeout_is_elapsed_time_not_number_of_quotes() {
        let r = evaluate(
            &[q(0, 9.9, 10.0), q(59000, 9.9, 10.0)],
            at(0),
            at(3600000),
            bracket(),
            model(0),
        );
        assert_eq!(r.exit_at, Some(at(60000)));
        assert_eq!(r.exit_reason, Some("timeout"));
    }
}
