//! Offline opportunity ↔ outcome evaluation join (Correction 1).
//!
//! ```text
//! cargo run -p backtest-metrics --bin oi_evaluate -- \
//!     opportunity-intelligence-<date>.ndjson episodes-<date>.ndjson \
//!     > evaluation-<date>.ndjson
//! ```
//!
//! * argument 1 — Opportunity Intelligence shadow log (`OpportunityScoreSnapshot`
//!   per line), written live during the session.
//! * argument 2..n — measurement episode artifacts (`OpportunityEpisode` per
//!   line), each carrying its settled `HorizonOutcome` or explicit censoring.
//!
//! Output is one `OpportunityEvaluationRecord` per scoring window, plus the
//! membership report and a coverage summary on stderr.
//!
//! # This runs after the fact, by construction
//!
//! It reads two files that already exist. Nothing it computes can reach the
//! live shadow record, because the live record has no outcome field to put it
//! in — the causal purity of the scoring log is a property of the schema, not
//! of this tool's discipline.
//!
//! # What it does not compute
//!
//! No hit rate, precision, expectancy, feature correlation or effectiveness
//! measure. It joins scores to outcomes and reports how completely it managed
//! to. The evaluation itself is GPT's, and a tool that quietly grew those
//! statistics would be the easiest way to cross that line.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use backtest_metrics::episode::OpportunityEpisode;
use backtest_metrics::evaluation::{
    build_evaluation, reconstruct_opportunities_from_snapshots,
};
use backtest_metrics::opportunity::OpportunityScoreSnapshot;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let snapshot_path = args
        .next()
        .context("usage: oi_evaluate <shadow-log.ndjson> <episodes.ndjson> [more-episodes...]")?;
    let episode_paths: Vec<String> = args.collect();
    anyhow::ensure!(
        !episode_paths.is_empty(),
        "at least one episode artifact is required; without outcomes there is nothing to join"
    );

    let mut malformed = 0usize;

    let mut snapshots: Vec<OpportunityScoreSnapshot> = Vec::new();
    for_each_line(&snapshot_path, |n, line| {
        match serde_json::from_str::<OpportunityScoreSnapshot>(line) {
            Ok(s) => snapshots.push(s),
            Err(error) => {
                malformed += 1;
                eprintln!("{snapshot_path} line {n}: {error}");
            }
        }
    })?;

    let mut episodes: Vec<OpportunityEpisode> = Vec::new();
    for path in &episode_paths {
        for_each_line(path, |n, line| {
            match serde_json::from_str::<OpportunityEpisode>(line) {
                Ok(e) => episodes.push(e),
                Err(error) => {
                    malformed += 1;
                    eprintln!("{path} line {n}: {error}");
                }
            }
        })?;
    }

    anyhow::ensure!(
        malformed == 0,
        "{malformed} unparseable line(s); refusing to evaluate from a partial join"
    );

    // Opportunity records are not persisted anywhere, so their windows are
    // derived from the snapshots. The derivation recovers the open instant
    // exactly and deliberately leaves the close unknown -- see
    // `reconstruct_opportunities_from_snapshots`. The consequence is
    // under-assignment at the tail, reported in the membership lists rather
    // than absorbed.
    let opportunities = reconstruct_opportunities_from_snapshots(&snapshots);

    eprintln!(
        "joining {} score snapshots across {} reconstructed opportunities \
         against {} episodes",
        snapshots.len(),
        opportunities.len(),
        episodes.len()
    );

    let set = build_evaluation(&snapshots, &opportunities, &episodes);

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for record in &set.records {
        serde_json::to_writer(&mut out, record)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;

    eprintln!("{}", serde_json::to_string_pretty(&set.coverage)?);
    eprintln!(
        "membership: {} mappings, {} ambiguous episodes, {} unassigned episodes \
         (rules {})",
        set.membership.mappings.len(),
        set.membership.ambiguous_episodes.len(),
        set.membership.unassigned_episodes.len(),
        set.membership.rules_version
    );
    Ok(())
}

fn for_each_line(path: &str, mut f: impl FnMut(usize, &str)) -> Result<()> {
    anyhow::ensure!(
        !path.ends_with(".gz"),
        "{path}: gzip input is not read here; decompress first, so the line count \
         this tool reports is the line count it read"
    );
    let file = std::fs::File::open(path).with_context(|| format!("opening {path}"))?;
    for (i, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading {path} line {}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        f(i + 1, &line);
    }
    Ok(())
}
