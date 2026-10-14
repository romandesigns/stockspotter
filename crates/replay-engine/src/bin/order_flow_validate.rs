//! Replays one symbol's past session through the order-flow engine and
//! reports how price moved after each state of each feature.
//!
//!     order_flow_validate <SYMBOL> <YYYY-MM-DD> [quote-lookback-minutes]
//!
//! This is the tool for steps 5 and 6 of the order-flow specification
//! (validate, then measure incremental value) -- the steps that have to
//! come before any of these features is allowed near ranking.
//!
//! **Running it is a research decision, not a build step.** It fetches a
//! real session's trades and quotes from the data provider and compares
//! the engine's reads against what price then did. On this project, live
//! fetches and anything that looks at real outcomes need explicit
//! authorisation each time, and some sessions are inside a preregistered
//! campaign's outcome firewall. Nothing in CI or in any service runs this,
//! and no run of it is included in this delivery. Its measuring logic
//! (`order_flow::validate`) is tested on synthetic series instead.
//!
//! What it does:
//! 1. Fetches the market day's trades (04:00 ET to 20:00 ET) and, by
//!    default, all of its quotes. A lookback in minutes can be given to
//!    limit quotes to the end of the session when the full tape is too
//!    large; earlier trades are then classified by the tick rule only.
//! 2. Replays them in time order through `OrderFlowEngine`, exactly as the
//!    live store does (same condition filter, same session buckets).
//! 3. Every 30 seconds in which the symbol traded, records the engine's
//!    reads and the last trade price.
//! 4. Prints, per feature state, the count, mean forward return and share
//!    of positive outcomes at one and five minutes, as a Markdown table.
//!
//! The output is descriptive, from one symbol and one day. It is where a
//! closer look starts, not a finding, and it says nothing about live
//! efficacy.

use anyhow::{bail, Context, Result};
use chrono::{Duration, NaiveDate};
use market_data::order_flow_store::{history_events, Event};
use market_data::{market_day_open, AlpacaConfig};
use order_flow::validate::{outcomes_by_state, to_markdown, Observation};
use order_flow::OrderFlowEngine;
use replay_engine::{fetch_historical_quotes, fetch_historical_trades};

const SAMPLE_SECS: f64 = 30.0;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        bail!("usage: order_flow_validate <SYMBOL> <YYYY-MM-DD> [quote-lookback-minutes]");
    }
    let symbol = args[0].to_ascii_uppercase();
    let day = NaiveDate::parse_from_str(&args[1], "%Y-%m-%d").context("date must be YYYY-MM-DD")?;
    let quote_lookback = args.get(2).map(|m| m.parse::<i64>()).transpose().context("quote lookback must be whole minutes")?;

    let cfg = AlpacaConfig::from_env()?;
    let start = market_day_open(day);
    let end = start + Duration::hours(16);
    let quotes_from = quote_lookback.map_or(start, |minutes| (end - Duration::minutes(minutes)).max(start));

    eprintln!("fetching {symbol} trades {start} .. {end}");
    let trades = fetch_historical_trades(&cfg, &symbol, &start.to_rfc3339(), &end.to_rfc3339()).await?;
    eprintln!("fetching {symbol} quotes {quotes_from} .. {end}");
    let quotes = fetch_historical_quotes(&cfg, &symbol, &quotes_from.to_rfc3339(), &end.to_rfc3339()).await?;
    eprintln!("replaying {} trades and {} quotes", trades.len(), quotes.len());

    let observations = observe(history_events(&trades, &quotes));
    println!("# Order-flow feature outcomes: {symbol} {day}\n");
    println!("{} trades, {} quotes, {} samples at {SAMPLE_SECS:.0}s. Descriptive only; one symbol, one session.\n", trades.len(), quotes.len(), observations.len());
    print!("{}", to_markdown(&outcomes_by_state(&observations)));
    Ok(())
}

/// Replays `events` and samples the engine at the end of every 30-second
/// bucket in which a trade happened.
fn observe(events: Vec<Event>) -> Vec<Observation> {
    let mut engine = OrderFlowEngine::default();
    let mut observations = Vec::new();
    let mut bucket: Option<f64> = None;
    let mut last_price = 0.0;
    for event in events {
        if let Event::Trade(trade, _, _) = &event {
            let this_bucket = (trade.timestamp_secs / SAMPLE_SECS).floor() * SAMPLE_SECS;
            // A trade in a new bucket closes the previous one: sample it
            // as of its own end, before this trade is applied.
            if let Some(previous) = bucket.filter(|b| *b != this_bucket) {
                let at = previous + SAMPLE_SECS;
                observations.push(Observation::from_summary(at, last_price, &engine.summary(at)));
            }
            bucket = Some(this_bucket);
            last_price = trade.price;
        }
        event.apply(&mut engine);
    }
    if let Some(previous) = bucket {
        let at = previous + SAMPLE_SECS;
        observations.push(Observation::from_summary(at, last_price, &engine.summary(at)));
    }
    observations
}

#[cfg(test)]
mod tests {
    use super::*;
    use order_flow::{Session, Trade, TradeFlags};

    fn trade(t: f64, price: f64) -> Event {
        Event::Trade(Trade { timestamp_secs: t, price, size: 100 }, Session::Regular, TradeFlags::default())
    }

    #[test]
    fn one_sample_per_traded_bucket_at_the_bucket_end_with_its_last_price() {
        let observations = observe(vec![trade(1.0, 2.00), trade(20.0, 2.02), trade(95.0, 2.10)]);
        // Bucket [0,30) and bucket [90,120); the empty ones in between
        // produce nothing.
        assert_eq!(observations.iter().map(|o| (o.at_secs, o.price)).collect::<Vec<_>>(), vec![(30.0, 2.02), (120.0, 2.10)]);
    }

    #[test]
    fn a_sample_does_not_see_the_trade_that_closed_its_bucket() {
        let observations = observe(vec![trade(1.0, 2.00), trade(31.0, 9.99)]);
        assert_eq!(observations[0].price, 2.00);
    }

    #[test]
    fn no_trades_means_no_samples() {
        assert!(observe(Vec::new()).is_empty());
    }
}
