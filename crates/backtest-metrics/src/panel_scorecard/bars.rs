//! Price paths: 1-minute bars per (symbol, market day), behind a trait so
//! tests never touch the network, with an on-disk cache so a rerun of the
//! same day costs no API calls.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use market_data::trading_session::{market_day_open, regular_session_close};
use market_data::{AlpacaConfig, Bar};
use serde::{Deserialize, Serialize};

/// Where bars come from. `start`/`end` bound bar start times, `[start, end)`.
pub trait BarSource {
    fn minute_bars(&self, symbol: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> impl Future<Output = Result<Vec<Bar>>> + Send;
}

/// Alpaca REST via `replay_engine::historical::fetch_historical_bars` (one
/// request per symbol-day; a full 04:00-16:00 ET day is at most 720 bars, one
/// page). Alpaca's multi-symbol bars endpoint is not wrapped anywhere in this
/// workspace, and adding an HTTP client to this crate would change
/// Cargo.lock, so this stays per symbol. `min_spacing` paces requests well
/// under Alpaca's 200/minute data limit.
pub struct AlpacaBars {
    pub cfg: AlpacaConfig,
    pub min_spacing: std::time::Duration,
}

impl BarSource for AlpacaBars {
    fn minute_bars(&self, symbol: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> impl Future<Output = Result<Vec<Bar>>> + Send {
        let symbol = symbol.to_string();
        async move {
            tokio::time::sleep(self.min_spacing).await;
            replay_engine::historical::fetch_historical_bars(&self.cfg, &symbol, &start.to_rfc3339(), &end.to_rfc3339(), "1Min").await
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BarsReport {
    pub symbols_requested: usize,
    pub from_cache: usize,
    pub fetched: usize,
    /// API calls made (one per fetched symbol).
    pub api_calls: usize,
    /// Symbols whose fetch failed; their detections are skipped as `noBars`.
    pub failed: Vec<String>,
    /// Fetched but not cached because the day was not over yet.
    pub not_cached_incomplete_day: usize,
}

/// The bar window scored for a market day: 04:00 ET to the regular close
/// (early closes respected). `None` on a day with no regular session.
pub fn day_window(day: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    Some((market_day_open(day), regular_session_close(day)?))
}

/// Symbols are used as file names, so only plain ticker characters pass.
pub fn safe_symbol(symbol: &str) -> bool {
    !symbol.is_empty() && symbol.len() <= 12 && symbol.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && !symbol.starts_with('.')
}

pub fn cache_path(cache_dir: &Path, day: NaiveDate, symbol: &str) -> PathBuf {
    cache_dir.join(day.format("%Y-%m-%d").to_string()).join(format!("{symbol}.json"))
}

/// Bars for every symbol on `day`, read from `cache_dir` when present,
/// otherwise fetched from `source` and written there -- but only once the
/// day's window has ended at `now`, so a partial day is never frozen into
/// the cache. A symbol with no bars is cached as an empty list (it is a
/// real answer). Failed fetches are reported, not fatal.
pub async fn load_day_bars<S: BarSource>(
    source: &S,
    cache_dir: Option<&Path>,
    symbols: &[String],
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<(BTreeMap<String, Vec<Bar>>, BarsReport)> {
    let Some((start, end)) = day_window(day) else { bail!("{day} has no regular session") };
    let complete = now >= end;
    let mut rep = BarsReport { symbols_requested: symbols.len(), ..Default::default() };
    let mut out = BTreeMap::new();
    for symbol in symbols {
        if !safe_symbol(symbol) {
            rep.failed.push(symbol.clone());
            continue;
        }
        if let Some(dir) = cache_dir {
            let p = cache_path(dir, day, symbol);
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(bars) = serde_json::from_str::<Vec<Bar>>(&text) {
                    rep.from_cache += 1;
                    out.insert(symbol.clone(), bars);
                    continue;
                }
            }
        }
        rep.api_calls += 1;
        match source.minute_bars(symbol, start, end).await {
            Ok(bars) => {
                rep.fetched += 1;
                if let Some(dir) = cache_dir {
                    if complete {
                        let p = cache_path(dir, day, symbol);
                        if let Some(parent) = p.parent() {
                            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
                        }
                        std::fs::write(&p, serde_json::to_vec(&bars)?).with_context(|| format!("writing {}", p.display()))?;
                    } else {
                        rep.not_cached_incomplete_day += 1;
                    }
                }
                out.insert(symbol.clone(), bars);
            }
            Err(e) => {
                tracing::warn!("bars for {symbol} on {day}: {e:#}");
                rep.failed.push(symbol.clone());
            }
        }
    }
    Ok((out, rep))
}
