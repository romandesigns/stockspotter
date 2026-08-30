//! Append-only JSON-lines persistence for backtest results — flat files,
//! not a database. Chosen because the actual scale here is modest
//! (backtest runs, not high-frequency data) and it keeps this decoupled
//! from any infra decision; revisit if/when a real DB is actually needed
//! (e.g. for the panels to query results interactively).

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::outcome::SignalOutcome;
use crate::signals::Strategy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedSignal {
    pub symbol: String,
    pub strategy: Strategy,
    pub timestamp: DateTime<Utc>,
    pub signal_price: f64,
    pub outcome: SignalOutcome,
    /// When this backtest run itself happened — distinct from
    /// `timestamp` (when the historical signal fired) — lets duplicate
    /// re-runs of the same historical window be told apart later.
    pub logged_at: DateTime<Utc>,
}

/// Appends every logged signal as one JSON line each. Creates the file
/// (and its parent directory) if it doesn't exist yet.
pub fn append(path: &Path, entries: &[LoggedSignal]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;

    for entry in entries {
        let line = serde_json::to_string(entry).context("serializing logged signal")?;
        writeln!(file, "{line}").context("writing to backtest log")?;
    }
    Ok(())
}

/// Reads every previously logged signal back — used to compute
/// aggregate metrics across *all* historical runs, not just the most
/// recent one. Missing file reads as empty, not an error (nothing logged
/// yet is a valid starting state).
pub fn read_all(path: &Path) -> Result<Vec<LoggedSignal>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {} of {}", i + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: LoggedSignal = serde_json::from_str(&line)
            .with_context(|| format!("parsing line {} of {}", i + 1, path.display()))?;
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample(price: f64) -> LoggedSignal {
        LoggedSignal {
            symbol: "TEST".to_string(),
            strategy: Strategy::FastFunnel,
            timestamp: Utc.timestamp_opt(0, 0).unwrap(),
            signal_price: price,
            outcome: SignalOutcome {
                hit: true,
                max_favorable_pct: 5.0,
                bars_to_target: Some(3),
            },
            logged_at: Utc.timestamp_opt(100, 0).unwrap(),
        }
    }

    #[test]
    fn missing_file_reads_as_empty_not_an_error() {
        let path = std::env::temp_dir().join(format!("stockspotter-test-missing-{}.jsonl", std::process::id()));
        let result = read_all(&path).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn append_then_read_all_round_trips() {
        let path = std::env::temp_dir().join(format!("stockspotter-test-roundtrip-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        append(&path, &[sample(1.0), sample(2.0)]).unwrap();
        append(&path, &[sample(3.0)]).unwrap(); // append doesn't clobber

        let all = read_all(&path).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].signal_price, 1.0);
        assert_eq!(all[2].signal_price, 3.0);

        std::fs::remove_file(&path).unwrap();
    }
}
