//! Historical replay of the opportunity-native outcome contract.
//!
//! ```text
//! oi_outcome_replay <traj.ndjson> <session-date> <out.ndjson> [symbol-prefix]
//! ```
//!
//! Drives **the real `OpportunityOutcomeCollector`** -- not a reimplementation
//! -- over the price series the frozen artifact contains, so a historical row
//! cannot diverge from a live one by construction.
//!
//! # What it can and cannot do
//!
//! The frozen evaluation artifact carries one price per opportunity per 30s
//! ranking window, and nothing between windows. That is enough to service
//! 84.56% of 2026-09-17 anchors fully; the rest are censored, exactly as the
//! live collector would censor them. **No price is interpolated, extrapolated
//! or invented** -- where the series stops, the horizon is censored.
//!
//! The optional `symbol-prefix` bounds the run to a slice, which is what the
//! equivalence gate uses before anyone runs a full session.

use std::io::{BufRead, BufWriter, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use backtest_metrics::opportunity_outcome::{
    AnchorProvenance, AnchorRequest, OpportunityOutcomeCollector, OPPORTUNITY_OUTCOME_VERSION,
    OUTCOME_SETTLE_AFTER_SECS,
};

/// One buffered ranking row: identity, instant, price and the opportunity's
/// opening instant. A named type because the tuple is wide enough that
/// repeating it twice obscures both signatures.
struct BufferedRow {
    opportunity_id: String,
    window_id: String,
    symbol: String,
    at: DateTime<Utc>,
    price: f64,
    opened_at: Option<DateTime<Utc>>,
}

/// Processes one complete ranking window: publish its prices, create its
/// anchors, then settle. Prices first, because a price at instant `T` is
/// forward information for anchors created in EARLIER windows and is rejected
/// as non-forward by anchors created at `T` -- so the order is safe and the
/// intent is explicit.
#[allow(clippy::too_many_arguments)]
fn flush(
    collector: &mut OpportunityOutcomeCollector,
    buf: &mut Vec<BufferedRow>,
    session: &str,
    session_end: DateTime<Utc>,
    provenance: &Arc<AnchorProvenance>,
    anchored: &mut u64,
    settled: &mut u64,
    writer: &mut BufWriter<std::fs::File>,
) -> Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let at = buf[0].at;
    for row in buf.iter() {
        collector.observe_price(&row.symbol, row.at, row.price);
    }
    for row in buf.drain(..) {
        collector.anchor(AnchorRequest {
            opportunity_id: row.opportunity_id,
            window_id: row.window_id,
            symbol: row.symbol,
            session_date: session.to_string(),
            anchor_at: row.at,
            signal_price: row.price,
            opened_at: row.opened_at,
            session_end,
            provenance: Arc::clone(provenance),
        });
        *anchored += 1;
    }
    for row in collector.settle_due(at) {
        writeln!(writer, "{}", serde_json::to_string(&row)?)?;
        *settled += 1;
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .context("usage: oi_outcome_replay <traj.ndjson> <date> <out.ndjson> [prefix]")?;
    let session = args.next().context("session date required")?;
    let out_path = args.next().context("output path required")?;
    let prefix = args.next();

    let session_end: DateTime<Utc> = format!("{session}T20:00:00Z").parse()?;

    let provenance = Arc::new(AnchorProvenance {
        measurement_version: OPPORTUNITY_OUTCOME_VERSION.to_string(),
        opportunity_schema: backtest_metrics::opportunity::OPPORTUNITY_SCHEMA_VERSION,
        feature_schema: backtest_metrics::context::SIGNAL_CONTEXT_SCHEMA_VERSION,
        early_quality_model: backtest_metrics::opportunity::EARLY_QUALITY_MODEL_VERSION.to_string(),
        continuation_model: backtest_metrics::opportunity::CONTINUATION_MODEL_VERSION.to_string(),
        ranking: backtest_metrics::opportunity::RANKING_VERSION.to_string(),
        score_policy: backtest_metrics::opportunity::SCORE_POLICY_VERSION.to_string(),
        config_fingerprint: "historical-replay".to_string(),
    });

    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::with_capacity(1 << 20, file);
    let out = std::fs::File::create(&out_path)?;
    let mut writer = BufWriter::with_capacity(1 << 20, out);

    let mut collector = OpportunityOutcomeCollector::new();
    let mut rows_in = 0u64;
    let mut anchored = 0u64;
    let mut settled = 0u64;
    let mut malformed = 0u64;

    // Rows are buffered per ranking window and flushed as a unit.
    //
    // Every row in a window carries the SAME timestamp, and settling in the
    // middle of one is a race: an anchor whose deadline falls inside that
    // window would settle before the rows later in the window had been
    // observed, losing its final observation -- and which rows those are
    // depends on nothing more meaningful than symbol order in the file. The
    // first equivalence run showed exactly that, as 687 anchors with one
    // observation too few. Buffering makes settlement window-aligned and
    // therefore deterministic.
    let mut window: Option<String> = None;
    let mut buf: Vec<BufferedRow> = Vec::new();

    // The artifact is globally time-ordered, so a single forward pass is a
    // faithful simulation of the live event order.
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            malformed += 1;
            continue;
        };
        rows_in += 1;

        let (Some(sym), Some(ts), Some(px), Some(oid), Some(win)) = (
            v.get("sym").and_then(|x| x.as_str()),
            v.get("ts").and_then(|x| x.as_str()),
            v.get("px").and_then(|x| x.as_f64()),
            v.get("oid").and_then(|x| x.as_str()),
            v.get("win").and_then(|x| x.as_str()),
        ) else {
            malformed += 1;
            continue;
        };
        if let Some(p) = &prefix {
            if !sym.starts_with(p.as_str()) {
                continue;
            }
        }
        let Ok(at) = ts.parse::<DateTime<Utc>>() else { malformed += 1; continue };
        if !px.is_finite() || px <= 0.0 {
            malformed += 1;
            continue;
        }

        let opened_at = v
            .get("age")
            .and_then(|x| x.as_i64())
            .map(|age| at - Duration::seconds(age));

        if window.as_deref() != Some(win) {
            if let Some(_prev) = window.take() {
                flush(&mut collector, &mut buf, &session, session_end, &provenance,
                      &mut anchored, &mut settled, &mut writer)?;
            }
            window = Some(win.to_string());
        }
        buf.push(BufferedRow {
            opportunity_id: oid.to_string(),
            window_id: win.to_string(),
            symbol: sym.to_string(),
            at,
            price: px,
            opened_at,
        });
    }
    if window.is_some() {
        flush(&mut collector, &mut buf, &session, session_end, &provenance,
              &mut anchored, &mut settled, &mut writer)?;
    }

    // Anything still outstanding is censored `CaptureEnded`, never dropped.
    for row in collector.finish(session_end) {
        writeln!(writer, "{}", serde_json::to_string(&row)?)?;
        settled += 1;
    }
    writer.flush()?;

    let health = collector.health();
    eprintln!(
        "  {session}: {rows_in} rows read, {anchored} anchored, {settled} settled, \
         {malformed} malformed"
    );
    eprintln!(
        "  peak outstanding {} of capacity {} ({:.1}%), capacity evictions {}",
        health.peak_outstanding,
        health.capacity,
        100.0 * health.peak_outstanding as f64 / health.capacity as f64,
        health.capacity_evictions
    );
    eprintln!("  settle window {OUTCOME_SETTLE_AFTER_SECS}s");
    assert_eq!(anchored, settled, "every anchor must settle exactly one row");

    Ok(())
}
