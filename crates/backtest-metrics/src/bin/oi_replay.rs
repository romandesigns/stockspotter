//! Offline Opportunity Intelligence driver (§13, §23 Phase E).
//!
//! Reads an ordered NDJSON sequence of recorded `ScanEvent` observations and
//! writes the shadow scoring snapshots the engine produces, as NDJSON, to
//! stdout. Same engine as the live subscriber in `ws-server` — that is the
//! point of it existing, and the reason replay parity is testable at all.
//!
//! ```text
//! cargo run -p backtest-metrics --bin oi_replay -- events.ndjson > snapshots.ndjson
//! ```
//!
//! Input, one object per line:
//!
//! ```json
//! {"receivedAt":"2026-09-14T13:30:00Z","event":{ ... a ScanEvent ... }}
//! ```
//!
//! # No production capture writes this format yet
//!
//! Stated plainly because it would otherwise be inferred wrongly from the
//! existence of this binary: nothing in production currently persists raw
//! `ScanEvent`s. The measurement capture persists *episodes*, and the
//! discovery capture persists *scan* records; neither is a raw event log.
//!
//! So this driver's real inputs today are hand-built or test-generated
//! sequences. It is deliberately not accompanied by a new production raw-event
//! writer: that would be a new high-volume write path on the market-dispatch
//! side, which is exactly the kind of change this milestone is not authorised
//! to make. Defining the format now means a future capture has a target to
//! write to, rather than this layer growing a second, divergent one.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use backtest_metrics::opportunity::{replay_events, OiConfig, ReplayObservation};

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context(
        "usage: oi_replay <events.ndjson>  (one {\"receivedAt\":..,\"event\":..} per line)",
    )?;
    let file = std::fs::File::open(&path).with_context(|| format!("opening {path}"))?;

    let mut events: Vec<ReplayObservation> = Vec::new();
    let mut malformed = 0usize;
    for (n, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading {path} line {}", n + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ReplayObservation>(&line) {
            Ok(observation) => events.push(observation),
            Err(error) => {
                // Counted and reported, never silently skipped: a replay run
                // over a partially-parsed input that claims to be complete is
                // the failure this whole programme has been correcting.
                malformed += 1;
                eprintln!("line {}: {error}", n + 1);
            }
        }
    }
    anyhow::ensure!(
        malformed == 0,
        "{malformed} unparseable line(s); refusing to replay a partial sequence"
    );

    // Input order is authoritative. It is NOT re-sorted by timestamp: the
    // engine's whole subject is the order in which information actually
    // arrived, and reordering here would silently manufacture a sequence that
    // never occurred.
    let config = OiConfig::default();
    eprintln!(
        "replaying {} observations  config={}  ranking_cadence={}s",
        events.len(),
        config.fingerprint(),
        config.ranking_cadence_secs
    );

    let snapshots = replay_events(config, &events);
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for snapshot in &snapshots {
        serde_json::to_writer(&mut out, snapshot)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    eprintln!("wrote {} snapshots", snapshots.len());
    Ok(())
}
