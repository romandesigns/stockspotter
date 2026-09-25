//! Historical replay of the opportunity-native outcome contract.
//!
//! ```text
//! oi_outcome_replay <traj.ndjson> <session-date> <out.ndjson> [symbol-prefix]
//!                   [--closures=<opportunity-intelligence-markers-*.ndjson>]...
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
//!
//! # Dispositions (D4)
//!
//! Given the capture's `opportunity_closed` markers (`--closures=`, repeatable,
//! because a session's markers can span two UTC-dated files), each close is
//! applied to the real collector before the first settlement at or after the
//! instant it was observed -- the live order -- so rows carry the same
//! `opportunityDisposition` the live collector wrote, and are stamped
//! `opportunity-outcome-v2`.
//!
//! Without them the dispositions are **unknown**, not `still_open`, so rows
//! are stamped `opportunity-outcome-v1` -- the contract that did not measure
//! disposition and is otherwise identical. Pre-D4 sessions have no closes to
//! supply and can never be given dispositions; they are not inferred.
//!
//! Binding a close needs the anchor's exact `openedAt`. A trajectory row that
//! carries only `age` (whole seconds) cannot be bound, so `--closures` with
//! such an input is refused rather than silently writing `still_open` under a
//! v2 stamp. Before trusting the result, reconcile the marker count against
//! `opportunitiesClosed` in the capture's `capture_finished` marker: markers
//! are not counted as data loss by the writer.

use std::io::{BufRead, BufWriter, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use backtest_metrics::opportunity_outcome::{
    anchor_session_end, AnchorProvenance, AnchorRequest, ClosureNotice, OpportunityOutcomeCollector,
    OPPORTUNITY_OUTCOME_VERSION, OPPORTUNITY_OUTCOME_VERSION_WITHOUT_DISPOSITION,
    OUTCOME_SETTLE_AFTER_SECS,
};

/// Persisted closes, in observation order, with a cursor: each is applied
/// once, before the first settlement at or after its observed instant.
struct Closures {
    notices: Vec<ClosureNotice>,
    next: usize,
}

impl Closures {
    fn load(paths: &[String]) -> Result<Self> {
        let mut notices = Vec::new();
        for path in paths {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let marker: Value = serde_json::from_str(line)
                    .with_context(|| format!("{path}: a marker line did not parse"))?;
                if marker.get("kind").and_then(|k| k.as_str()) != Some("opportunity_closed") {
                    continue;
                }
                let data = marker.get("data").cloned().unwrap_or(Value::Null);
                notices.push(
                    serde_json::from_value::<ClosureNotice>(data)
                        .with_context(|| format!("{path}: malformed opportunity_closed"))?,
                );
            }
        }
        // Stable: closes observed at one instant keep their write order, which
        // is the order the engine emitted them.
        notices.sort_by_key(|n| n.close_observed_at);
        Ok(Self { notices, next: 0 })
    }

    /// Applies every close observed at or before `at`.
    fn apply_through(&mut self, collector: &mut OpportunityOutcomeCollector, at: DateTime<Utc>) {
        while let Some(notice) = self.notices.get(self.next) {
            if notice.close_observed_at > at {
                break;
            }
            collector.apply_closure(notice);
            self.next += 1;
        }
    }

    fn apply_rest(&mut self, collector: &mut OpportunityOutcomeCollector) {
        while let Some(notice) = self.notices.get(self.next) {
            collector.apply_closure(notice);
            self.next += 1;
        }
    }
}

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
    provenance: &Arc<AnchorProvenance>,
    anchored: &mut u64,
    settled: &mut u64,
    writer: &mut BufWriter<std::fs::File>,
    closures: &mut Option<Closures>,
) -> Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let at = buf[0].at;
    for row in buf.iter() {
        collector.observe_price(&row.symbol, row.at, row.price);
    }
    // Closes observed since the previous window, BEFORE this window's anchors
    // and settlement -- the live order: a close observed at `R` is applied
    // before anything anchored or settled at `R`, and live could not have
    // anchored a window between the close and its observation.
    if let Some(closures) = closures.as_mut() {
        closures.apply_through(collector, at);
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
            // Per anchor, through the same function live capture uses (D13),
            // so a replayed row cannot disagree with a live one about where
            // the session ended.
            session_end: anchor_session_end(row.at),
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
    let (flags, positional): (Vec<String>, Vec<String>) =
        std::env::args().skip(1).partition(|a| a.starts_with("--"));
    let mut closure_paths = Vec::new();
    for flag in &flags {
        match flag.strip_prefix("--closures=") {
            Some(path) => closure_paths.push(path.to_string()),
            None => anyhow::bail!("unknown flag {flag}"),
        }
    }
    let mut closures =
        if closure_paths.is_empty() { None } else { Some(Closures::load(&closure_paths)?) };
    let mut args = positional.into_iter();
    let path = args
        .next()
        .context("usage: oi_outcome_replay <traj.ndjson> <date> <out.ndjson> [prefix]")?;
    let session = args.next().context("session date required")?;
    let out_path = args.next().context("output path required")?;
    let prefix = args.next();

    // Where capture ends: the regular close, DST- and early-close-aware (D13;
    // this was a fixed 20:00Z, which is 16:00 ET only under EDT).
    let session_day: chrono::NaiveDate =
        session.parse().with_context(|| format!("session date {session} is not YYYY-MM-DD"))?;
    let session_end = market_data::trading_session::regular_session_close(session_day)
        .with_context(|| format!("{session} has no regular session"))?;

    let provenance = Arc::new(AnchorProvenance {
        // v2 only when dispositions are actually measured; see the module doc.
        measurement_version: if closures.is_some() {
            OPPORTUNITY_OUTCOME_VERSION
        } else {
            OPPORTUNITY_OUTCOME_VERSION_WITHOUT_DISPOSITION
        }
        .to_string(),
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

        // The exact opening instant when the row carries it (schema-2
        // snapshots do); otherwise the whole-second reconstruction from `age`,
        // which cannot bind a close.
        let exact_opened_at =
            v.get("openedAt").and_then(|x| x.as_str()).and_then(|s| s.parse::<DateTime<Utc>>().ok());
        if closures.is_some() && exact_opened_at.is_none() {
            anyhow::bail!(
                "--closures needs trajectory rows carrying the exact `openedAt`; a close cannot \
                 be bound to an anchor through the whole-second `age`, and writing still_open \
                 under opportunity-outcome-v2 would present an unknown as a measurement"
            );
        }
        let opened_at = exact_opened_at.or_else(|| {
            v.get("age").and_then(|x| x.as_i64()).map(|age| at - Duration::seconds(age))
        });

        if window.as_deref() != Some(win) {
            if let Some(_prev) = window.take() {
                flush(&mut collector, &mut buf, &session, &provenance,
                      &mut anchored, &mut settled, &mut writer, &mut closures)?;
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
        flush(&mut collector, &mut buf, &session, &provenance,
              &mut anchored, &mut settled, &mut writer, &mut closures)?;
    }

    // The capture's own final closes (capture end) reach the collector before
    // it force-settles, as `finish_both` orders them live.
    if let Some(closures) = closures.as_mut() {
        closures.apply_rest(&mut collector);
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
    if let Some(closures) = &closures {
        let d = health.disposition_counts;
        eprintln!(
            "  closes applied {} (anchors marked {}); dispositions: still_open {}, inactivity {}, \
             session_boundary {}, capacity_reached {}, capture_ended {}",
            closures.notices.len(),
            health.closure_anchors_marked,
            d.still_open,
            d.inactivity,
            d.session_boundary,
            d.capacity_reached,
            d.capture_ended
        );
    }
    assert_eq!(anchored, settled, "every anchor must settle exactly one row");

    Ok(())
}
