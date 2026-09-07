//! Answers the one question the project's own gate turns on: **is there
//! any exit bracket under which these signals actually make money?**
//!
//! Reads `data/backtest_log.jsonl` and re-evaluates every logged signal
//! against a grid of (target, stop, lookforward) combinations, entirely
//! offline. No Alpaca calls, no re-replaying — it works off
//! `LoggedSignal::forward_path_pct`, the raw post-signal price path
//! stored with each signal precisely so this is possible.
//!
//! Run with: `cargo run --release -p backtest-metrics --bin sweep_brackets`
//!
//! Optional: `--strategy IgnitionDetector` to sweep one strategy,
//! `--log <path>` for a different log file, `--cost-pct <n>` to charge a
//! round-trip trading cost against every signal (default 0, i.e. raw
//! gross numbers).
//!
//! **Costs are the whole argument on these names.** Every expectancy
//! here is a GROSS mean realized move unless `--cost-pct` is given, and
//! gross is not what a trader keeps. On the sub-$3 low-float stocks this
//! scanner targets, crossing the spread once can cost a full percent —
//! a penny of spread on a $1.50 stock is 0.67%, and that is paid twice
//! per round trip. So every row also prints its **cost tolerance**: the
//! round-trip cost at which that bracket breaks even. A bracket earning
//! +0.06%/signal gross is not a strategy, it is a rounding error that
//! any real fill destroys, and printing the tolerance next to it makes
//! that unmissable rather than something the reader has to work out.
//!
//! **Why this is a separate tool from `tune`/`tune_broad`.** Those sweep
//! *detector* configuration — what fires a signal — and each variant
//! costs a full re-replay of real market data. This sweeps *exit*
//! configuration — what you do after a signal fires — which needs no
//! re-replay at all, because the future price path is the same
//! regardless of which target you would have taken. Conflating the two
//! is what made the exit bracket effectively untested for so long: it
//! was expensive by association with something it doesn't actually
//! depend on.
//!
//! **On reading the output honestly.** Sweeping many brackets over one
//! sample and reporting the best one is a good way to fool yourself —
//! with enough combinations something always looks profitable. Two
//! guards, both enforced below rather than left to the reader:
//! every row prints its own sample size, and the sweep splits the data
//! chronologically into an in-sample and out-of-sample half, selects
//! the bracket on the first half only, and reports what it did on the
//! second. A bracket that only works in-sample is overfitting, and the
//! output says so in as many words.

use std::collections::HashMap;

use anyhow::{Context, Result};
use backtest_metrics::{evaluate_outcome_from_path, read_all, LoggedSignal, OutcomeKind, OutcomeThresholds, Strategy};

const LOG_PATH: &str = "data/backtest_log.jsonl";

/// Grids kept deliberately coarse. A fine grid over one sample finds
/// noise and calls it signal; these are wide enough to show the SHAPE of
/// the response surface (does expectancy improve with a wider target? a
/// tighter stop? a longer hold?) without inviting a false precision the
/// sample can't support.
const TARGETS: &[f64] = &[1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 7.0];
const STOPS: &[f64] = &[1.0, 1.5, 2.0, 3.0, 4.0, 5.0];
const LOOKFORWARDS: &[usize] = &[5, 10, 20, 30];

/// Below this, a row's expectancy is noise and is not reported as a
/// candidate. 30 is low for a confident claim but high enough to exclude
/// the obviously meaningless.
const MIN_SAMPLE: usize = 30;

#[derive(Debug, Clone, Copy)]
struct SweepRow {
    thresholds: OutcomeThresholds,
    n: usize,
    hits: usize,
    stopped: usize,
    timed_out: usize,
    /// Round-trip cost charged per signal to produce
    /// `net_expectancy_pct`. Zero unless `--cost-pct` was passed.
    cost_pct: f64,
    /// Expectancy computed from REAL realized moves (`final_pct`), not
    /// from assuming every win banked exactly `target_pct` and every
    /// loss cost exactly `stop_pct`. Those assumptions are both wrong in
    /// the optimistic direction on a real tape: price gaps through a
    /// stop as readily as through a target, and a timeout resolves
    /// wherever it happens to be, which is usually neither.
    expectancy_pct: f64,
    hit_rate_pct: f64,
}

impl SweepRow {
    /// Gross expectancy minus the assumed round-trip cost — what a
    /// trader would actually keep per signal.
    fn net_expectancy_pct(&self) -> f64 {
        self.expectancy_pct - self.cost_pct
    }
}

fn sweep_one(signals: &[&LoggedSignal], thresholds: OutcomeThresholds, cost_pct: f64) -> SweepRow {
    let mut hits = 0usize;
    let mut stopped = 0usize;
    let mut timed_out = 0usize;
    let mut total_pct = 0.0_f64;
    let mut n = 0usize;

    for s in signals {
        if s.forward_path_pct.is_empty() {
            continue; // logged before paths were recorded; not scoreable
        }
        let o = evaluate_outcome_from_path(&s.forward_path_pct, &thresholds);
        n += 1;
        total_pct += o.final_pct;
        match o.kind {
            OutcomeKind::Hit => hits += 1,
            OutcomeKind::StoppedOut => stopped += 1,
            _ => timed_out += 1,
        }
    }

    SweepRow {
        thresholds,
        n,
        hits,
        stopped,
        timed_out,
        cost_pct,
        expectancy_pct: if n > 0 { total_pct / n as f64 } else { 0.0 },
        hit_rate_pct: if n > 0 { hits as f64 / n as f64 * 100.0 } else { 0.0 },
    }
}

fn sweep_all(signals: &[&LoggedSignal], cost_pct: f64) -> Vec<SweepRow> {
    let mut rows = Vec::new();
    for &target_pct in TARGETS {
        for &stop_pct in STOPS {
            for &lookforward_bars in LOOKFORWARDS {
                rows.push(sweep_one(signals, OutcomeThresholds { target_pct, stop_pct, lookforward_bars }, cost_pct));
            }
        }
    }
    rows
}

fn describe(t: &OutcomeThresholds) -> String {
    format!("+{:.1}/-{:.1} x{:>2}", t.target_pct, t.stop_pct, t.lookforward_bars)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let log_path = arg_value(&args, "--log").unwrap_or_else(|| LOG_PATH.to_string());
    let only_strategy = arg_value(&args, "--strategy");
    let split_date = arg_value(&args, "--split-date")
        .map(|value| chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d"))
        .transpose().context("--split-date must be YYYY-MM-DD")?;
    let cost_pct: f64 = arg_value(&args, "--cost-pct").and_then(|v| v.parse().ok()).unwrap_or_else(backtest_metrics::round_trip_cost_pct);
    anyhow::ensure!(cost_pct.is_finite() && cost_pct >= 0.0, "cost must be finite and nonnegative");

    let all = read_all(std::path::Path::new(&log_path)).context("reading the backtest log")?;
    let scoreable = all.iter().filter(|s| !s.forward_path_pct.is_empty()).count();
    println!("=== {} signals in {log_path}, {scoreable} with a stored forward path ===", all.len());
    if cost_pct > 0.0 {
        println!("=== charging {cost_pct:.2}% round-trip cost per signal ===");
    } else {
        println!("=== GROSS numbers (no trading cost charged; see --cost-pct and the tolerance column) ===");
    }
    if scoreable == 0 {
        anyhow::bail!(
            "no signals carry a forward price path — re-run `backtest_broad` to regenerate the log \
             (paths were added 2026-09-06; older lines can't be re-scored)"
        );
    }
    println!();

    let mut by_strategy: HashMap<Strategy, Vec<&LoggedSignal>> = HashMap::new();
    for s in &all {
        by_strategy.entry(s.strategy).or_default().push(s);
    }

    let mut strategies: Vec<_> = by_strategy.keys().copied().collect();
    strategies.sort_by_key(|s| format!("{s:?}"));

    for strategy in strategies {
        let name = format!("{strategy:?}");
        if let Some(filter) = &only_strategy {
            if !name.eq_ignore_ascii_case(filter) {
                continue;
            }
        }
        let mut signals = by_strategy[&strategy].clone();
        // Chronological, so the in/out-of-sample split below is a real
        // forward test rather than a random shuffle -- a strategy that
        // only worked in June and stopped working in August should fail
        // this, and a random split would hide exactly that.
        signals.sort_by_key(|s| s.timestamp);

        println!("### {name} — {} signals", signals.len());

        let shipped = OutcomeThresholds::for_strategy(strategy);
        let shipped_row = sweep_one(&signals, shipped, cost_pct);
        println!(
            "  shipped bracket {}: n={} hit={:.1}% gross={:+.3}%  net={:+.3}%  survives cost up to {:.3}%",
            describe(&shipped),
            shipped_row.n,
            shipped_row.hit_rate_pct,
            shipped_row.expectancy_pct,
            shipped_row.net_expectancy_pct(),
            shipped_row.expectancy_pct.max(0.0)
        );

        let mut rows: Vec<SweepRow> = sweep_all(&signals, cost_pct).into_iter().filter(|r| r.n >= MIN_SAMPLE).collect();
        if rows.is_empty() {
            println!("  (sample too small to sweep)\n");
            continue;
        }
        rows.sort_by(|a, b| b.expectancy_pct.partial_cmp(&a.expectancy_pct).unwrap_or(std::cmp::Ordering::Equal));

        println!("  best 5 brackets over the WHOLE sample (in-sample — see the split below before believing these):");
        for r in rows.iter().take(5) {
            println!(
                "    {}  n={:>5} hit={:>5.1}%  W/L/T={}/{}/{}  gross={:+.3}%  net={:+.3}%  cost tolerance={:.3}%",
                describe(&r.thresholds),
                r.n,
                r.hit_rate_pct,
                r.hits,
                r.stopped,
                r.timed_out,
                r.expectancy_pct,
                r.net_expectancy_pct(),
                r.expectancy_pct.max(0.0)
            );
        }

        // --- the part that decides whether any of the above is real ---
        let dates: std::collections::BTreeSet<_> = signals.iter()
            .map(|s| s.timestamp.with_timezone(&chrono_tz::America::New_York).date_naive()).collect();
        let boundary = split_date.or_else(|| dates.iter().nth(dates.len() / 2).copied());
        let split = boundary.map_or(0, |date| signals.partition_point(|s|
            s.timestamp.with_timezone(&chrono_tz::America::New_York).date_naive() < date));
        if split >= MIN_SAMPLE && signals.len() - split >= MIN_SAMPLE {
            let (older, second) = signals.split_at(split);
            // Purge the longest evaluated outcome horizon before the test boundary.
            let cutoff = second[0].timestamp - chrono::Duration::minutes(30);
            let first: Vec<_> = older.iter().copied().filter(|s| s.timestamp < cutoff).collect();
            let mut in_rows: Vec<SweepRow> =
                sweep_all(&first.to_vec(), cost_pct).into_iter().filter(|r| r.n >= MIN_SAMPLE).collect();
            in_rows.sort_by(|a, b| b.expectancy_pct.partial_cmp(&a.expectancy_pct).unwrap_or(std::cmp::Ordering::Equal));

            if let Some(best_in) = in_rows.first() {
                let out = sweep_one(&second.to_vec(), best_in.thresholds, cost_pct);
                let held = out.net_expectancy_pct() > 0.0;
                println!(
                    "  walk-forward: bracket {} chosen on the older sessions (n={}, {:+.3}%) scored {:+.3}% on the newer sessions (n={}), cost tolerance {:.3}%",
                    describe(&best_in.thresholds),
                    best_in.n,
                    best_in.net_expectancy_pct(),
                    out.net_expectancy_pct(),
                    out.n,
                    out.expectancy_pct.max(0.0)
                );
                println!(
                    "  VERDICT: {}",
                    if held {
                        "positive after configured costs on held-out sessions; exploratory, not execution evidence"
                    } else {
                        "not positive after configured costs on held-out sessions; no promotion supported"
                    }
                );
            }
        } else {
            println!("  walk-forward: skipped, each half would be under {MIN_SAMPLE} signals");
        }
        println!();
    }

    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).cloned()
}
