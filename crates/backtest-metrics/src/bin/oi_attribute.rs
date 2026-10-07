//! Offline discovery-attribution join (§16, §23 Phase F).
//!
//! ```text
//! cargo run -p backtest-metrics --bin oi_attribute -- \
//!     evidence.ndjson [snapshots.ndjson ...] > attributed.ndjson
//! ```
//!
//! * `evidence.ndjson` — one `SymbolEvidence` per line, covering the discovery
//!   stages (visible / nosnap / selection-input / qualified / quiet-selected /
//!   detector-produced). Produced from a reduced discovery archive by
//!   `python/discovery_evidence.py`, which owns the change-based
//!   decoding and the per-symbol forward-fill.
//! * `snapshots.ndjson` — zero or more Opportunity Intelligence shadow logs.
//!   These contribute the `opportunityRanked` stage and the ranking-window
//!   count, and nothing else.
//!
//! Output is one `AttributedSymbol` per line, plus a coverage summary on
//! stderr.
//!
//! # What this does not do
//!
//! It states how far each symbol got and whether the evidence can say. It
//! computes no hit rate, no precision, no expectancy and no effectiveness
//! measure of any kind — those are not this programme's to produce, and a
//! utility that quietly grew them would be the easiest possible way to cross
//! that line. The single aggregate it emits (`AttributionCoverage`) describes
//! coverage of the evidence, not behaviour of the platform.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use backtest_metrics::attribution::{
    attribute_all, coverage, Stage, SymbolEvidence,
};
use backtest_metrics::opportunity::OpportunityScoreSnapshot;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let evidence_path = args
        .next()
        .context("usage: oi_attribute <evidence.ndjson> [snapshots.ndjson ...]")?;
    let snapshot_paths: Vec<String> = args.collect();

    // Keyed by (sessionDate, symbol): the same symbol on two sessions is two
    // independent attributions, and merging them would smear one session's
    // capture gaps across another's.
    let mut evidence: BTreeMap<(String, String), SymbolEvidence> = BTreeMap::new();

    let mut malformed = 0usize;
    for_each_line(&evidence_path, |n, line| {
        match serde_json::from_str::<SymbolEvidence>(line) {
            Ok(row) => {
                let key = (row.session_date.clone(), row.symbol.clone());
                match evidence.get_mut(&key) {
                    // Merged rather than replaced, so an evidence file may be
                    // produced in several passes without the last pass
                    // silently winning.
                    Some(existing) => merge(existing, &row),
                    None => {
                        evidence.insert(key, row);
                    }
                }
            }
            Err(error) => {
                malformed += 1;
                eprintln!("{evidence_path} line {n}: {error}");
            }
        }
        Ok(())
    })?;

    let mut snapshot_rows = 0usize;
    for path in &snapshot_paths {
        for_each_line(path, |n, line| {
            match serde_json::from_str::<OpportunityScoreSnapshot>(line) {
                Ok(snapshot) => {
                    snapshot_rows += 1;
                    let key = (snapshot.session_date.clone(), snapshot.symbol.clone());
                    let entry = evidence.entry(key).or_insert_with(|| {
                        SymbolEvidence::new(&snapshot.symbol, &snapshot.session_date)
                    });
                    // Deliberately only `true`. A symbol missing from the
                    // shadow log is NOT evidence that it was never ranked --
                    // the log may simply not cover it (capture off, writer
                    // drops, opportunity closed before its first window). So
                    // the absent case stays unknown rather than becoming
                    // `Some(false)`.
                    entry.observe(Stage::OpportunityRanked, true);
                    // A ranked opportunity is by construction detector-produced:
                    // nothing enters the engine except detector events.
                    entry.observe(Stage::DetectorProduced, true);
                    entry.ranking_windows += 1;
                }
                Err(error) => {
                    malformed += 1;
                    eprintln!("{path} line {n}: {error}");
                }
            }
            Ok(())
        })?;
    }

    anyhow::ensure!(
        malformed == 0,
        "{malformed} unparseable line(s); refusing to attribute from a partial join"
    );

    let rows: Vec<SymbolEvidence> = evidence.into_values().collect();
    let attributed = attribute_all(&rows);

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for row in &attributed {
        serde_json::to_writer(&mut out, row)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;

    let cov = coverage(&attributed);
    eprintln!("{}", serde_json::to_string_pretty(&cov)?);
    eprintln!(
        "attributed {} symbols from {} evidence rows and {} snapshot rows",
        attributed.len(),
        rows.len(),
        snapshot_rows
    );
    Ok(())
}

fn merge(into: &mut SymbolEvidence, from: &SymbolEvidence) {
    for stage in Stage::ALL {
        if let Some(present) = from.get(stage) {
            // `observe` is monotonic in the positive direction, so a later
            // `false` cannot retract an earlier `true`. See its own comment.
            into.observe(stage, present);
        }
    }
    into.nosnap_events += from.nosnap_events;
    into.ranking_windows += from.ranking_windows;
    for (detector, count) in &from.detector_events {
        *into.detector_events.entry(detector.clone()).or_insert(0) += count;
    }
}

fn for_each_line(
    path: &str,
    mut f: impl FnMut(usize, &str) -> Result<()>,
) -> Result<()> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {path}"))?;
    let reader: Box<dyn BufRead> = if path.ends_with(".gz") {
        anyhow::bail!("{path}: gzip input is not read here; decompress first, so the \
                       byte count this tool reports is the byte count it read")
    } else {
        Box::new(std::io::BufReader::new(file))
    };
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading {path} line {}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        f(i + 1, &line)?;
    }
    Ok(())
}
