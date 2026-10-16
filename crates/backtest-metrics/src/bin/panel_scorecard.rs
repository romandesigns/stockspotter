//! Scores one market day's panel detections (see
//! `backtest_metrics::panel_scorecard` for every definition).
//!
//!   panel_scorecard --day YYYY-MM-DD --detections DIR --out DIR
//!                   [--pending FILE] [--bars-cache DIR] [--max-symbols N]
//!
//! * `--detections DIR` holds the recorder's `YYYY-MM-DD.jsonl` files
//!   (`data/panel-detections`). When the day's file is missing and
//!   `--pending` names `data/live_pending_signals.jsonl`, that is used as
//!   the fallback source (and the report says so).
//! * `--out DIR` receives `YYYY-MM-DD.json` (this day, per-detection rows)
//!   and `latest.json` (rolling summary over the last 20 scored days in DIR,
//!   plus this day's panel stats).
//! * `--bars-cache DIR` caches 1-minute bars per symbol-day, so a rerun
//!   makes no API calls. Bars come from Alpaca REST with the `ALPACA_*`
//!   credentials in the environment / `.env` (never printed); they are only
//!   needed for symbols not already cached.
//! * `--max-symbols N` (default 200) caps API use; sampling is disclosed in
//!   the report's `sampling` block.
//!
//! The round-trip cost proxy for rows without a quote is
//! `ROUND_TRIP_COST_PCT` (default 0.5%), as everywhere else in this crate.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use backtest_metrics::panel_scorecard::{
    build_day_report, build_latest, load_day_bars, load_day_reports, parse_detections, parse_pending, plan, AlpacaBars, BarSource, SourceKind,
    BOOTSTRAP_B, MAX_SYMBOLS_PER_DAY,
};
use backtest_metrics::round_trip_cost_pct;
use chrono::{DateTime, NaiveDate, Utc};
use market_data::{AlpacaConfig, Bar};

const USAGE: &str = "usage: panel_scorecard --day YYYY-MM-DD --detections DIR --out DIR [--pending FILE] [--bars-cache DIR] [--max-symbols N]";

/// Alpaca when credentials exist; otherwise every uncached symbol fails
/// with a clear reason (cached symbols still score).
enum Source {
    Alpaca(AlpacaBars),
    Missing(String),
}

impl BarSource for Source {
    async fn minute_bars(&self, symbol: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<Bar>> {
        match self {
            Source::Alpaca(a) => a.minute_bars(symbol, start, end).await,
            Source::Missing(why) => bail!("no Alpaca credentials ({why})"),
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let (mut day, mut detections, mut pending, mut out, mut cache, mut max_symbols) = (None, None, None, None, None, MAX_SYMBOLS_PER_DAY);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} needs a value; {USAGE}"));
        match a.as_str() {
            "--day" => day = Some(NaiveDate::parse_from_str(&val()?, "%Y-%m-%d").context("--day must be YYYY-MM-DD")?),
            "--detections" => detections = Some(PathBuf::from(val()?)),
            "--pending" => pending = Some(PathBuf::from(val()?)),
            "--out" => out = Some(PathBuf::from(val()?)),
            "--bars-cache" => cache = Some(PathBuf::from(val()?)),
            "--max-symbols" => max_symbols = val()?.parse().context("--max-symbols must be a number")?,
            other => bail!("unknown argument {other}; {USAGE}"),
        }
    }
    let (Some(day), Some(detections), Some(out)) = (day, detections, out) else { bail!(USAGE) };

    let det_file = detections.join(format!("{day}.jsonl"));
    let (dets, mut source) = if det_file.exists() {
        let text = std::fs::read_to_string(&det_file).with_context(|| format!("reading {}", det_file.display()))?;
        parse_detections(&text, day)
    } else if let Some(p) = &pending {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let (d, mut s) = parse_pending(&text, day);
        s.file = p.file_name().map(|n| n.to_string_lossy().into_owned());
        (d, s)
    } else {
        bail!("no detections for {day}: {} does not exist and no --pending fallback was given", det_file.display());
    };
    if source.kind == SourceKind::PanelDetections {
        source.file = det_file.file_name().map(|n| n.to_string_lossy().into_owned());
    }

    let plan = plan(day, dets, max_symbols);
    eprintln!(
        "{day}: {} detections kept of {} ({} symbols of {}{}); fetching bars",
        plan.sampling.detections_kept,
        plan.sampling.detections_available,
        plan.sampling.symbols_scored,
        plan.sampling.symbols_available,
        if plan.sampling.sampled { ", SAMPLED" } else { "" }
    );

    dotenvy::dotenv().ok();
    let src = match AlpacaConfig::from_env() {
        Ok(cfg) => Source::Alpaca(AlpacaBars { cfg, min_spacing: std::time::Duration::from_millis(350) }),
        Err(e) => Source::Missing(e.to_string()),
    };
    let now = Utc::now();
    let (bars, bars_report) = load_day_bars(&src, cache.as_deref(), &plan.symbols, day, now).await?;
    if bars_report.not_cached_incomplete_day > 0 {
        eprintln!("note: {day} is not over yet; bars were not cached and results are partial");
    }
    if !bars_report.failed.is_empty() {
        eprintln!("warning: no bars for {} symbol(s); their detections are skipped as noBars", bars_report.failed.len());
    }

    let cost = round_trip_cost_pct();
    let report = build_day_report(&plan, source, &bars, bars_report, cost, now, BOOTSTRAP_B);
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    // Compact: the day file carries every row; latest.json has none and stays readable.
    write_atomic(&out.join(format!("{day}.json")), &serde_json::to_vec(&report)?)?;

    let history = load_day_reports(&out)?;
    let latest = build_latest(&report, &history, now, BOOTSTRAP_B);
    write_atomic(&out.join("latest.json"), &serde_json::to_vec_pretty(&latest)?)?;

    eprintln!("rolling over {} day(s):", latest.rolling.days.len());
    for p in &latest.rolling.panels {
        let h = &p.horizons[backtest_metrics::panel_scorecard::PRIMARY];
        eprintln!(
            "  {:<24} n={:<5} days={:<3} mean={:>8} base={:>8}  {}: {}",
            p.panel.as_str(),
            h.n,
            p.sessions,
            h.mean_net_pct.map_or("-".into(), |v| format!("{v:.2}%")),
            h.baseline_mean_net_pct.map_or("-".into(), |v| format!("{v:.2}%")),
            p.verdict.as_str(),
            p.verdict_reason
        );
    }
    Ok(())
}
