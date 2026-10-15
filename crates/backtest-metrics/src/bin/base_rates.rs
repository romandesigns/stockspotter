//! Builds the shortlist's historical base rates from cached sessions.
//!
//!   base_rates --input-dir DIR --out FILE [--note TEXT]
//!
//! DIR holds cached `replay_engine::ReplayData` JSON files, one symbol-day
//! each (`<SYMBOL>-<YYYY-MM-DD>-sip-v1.json`, as the 2026 audit-validation
//! runs wrote them). Nothing is fetched: this reads what is on disk, measures
//! each session at 09:35 ET (see `backtest_metrics::base_rates`), and writes
//! one JSON document the live server serves on each shortlist card.
//!
//! `--note` is stored verbatim in `source.note` and should say how the
//! sessions were chosen; the default states the known bias of the 2026
//! cache.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use backtest_metrics::base_rates::{aggregate, observe, SessionInput, Skipped};

const DEFAULT_NOTE: &str = "Measured on cached market sessions: 9 fixed symbols every day, plus small caps saved on the days they moved. That second group was picked because it moved, so these rates lean optimistic.";

fn main() -> Result<()> {
    let mut input = None;
    let mut out = None;
    let mut note = DEFAULT_NOTE.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input-dir" => input = args.next().map(PathBuf::from),
            "--out" => out = args.next().map(PathBuf::from),
            "--note" => note = args.next().context("--note needs a value")?,
            other => bail!("unknown argument {other}; usage: base_rates --input-dir DIR --out FILE [--note TEXT]"),
        }
    }
    let (Some(input), Some(out)) = (input, out) else { bail!("usage: base_rates --input-dir DIR --out FILE [--note TEXT]") };

    let mut files: Vec<PathBuf> = std::fs::read_dir(&input)
        .with_context(|| format!("reading {}", input.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("-sip-v1.json")))
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("no *-sip-v1.json session files in {}", input.display());
    }

    let mut observations = Vec::new();
    let mut skipped = Skipped::default();
    let mut unreadable = 0usize;
    for (i, path) in files.iter().enumerate() {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                unreadable += 1;
                continue;
            }
        };
        match serde_json::from_str::<SessionInput>(&text) {
            Ok(session) => match observe(&session) {
                Ok(o) => observations.push(o),
                Err(why) => skipped.count(why),
            },
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                unreadable += 1;
            }
        }
        if (i + 1) % 50 == 0 {
            eprintln!("{}/{} sessions read, {} observations", i + 1, files.len(), observations.len());
        }
    }
    if unreadable > 0 {
        note = format!("{note} {unreadable} file(s) could not be read and are excluded.");
    }
    let rates = aggregate(&observations, files.len() - unreadable, &skipped, &note, chrono::Utc::now());
    std::fs::write(&out, serde_json::to_string_pretty(&rates)?).with_context(|| format!("writing {}", out.display()))?;
    eprintln!("wrote {}: {} observations from {} sessions", out.display(), rates.source.observations, rates.source.sessions_read);
    Ok(())
}
