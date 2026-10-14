//! Bounded streaming replay of supplied bars; stdout only. No fetching or writes.
use momentum_strategies::{Bar, Config, Monitor};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, Write};

fn summary(id: &str, monitor: Monitor) -> serde_json::Value {
    let config = format!("{}\n", serde_json::to_string(&Config::default()).unwrap());
    serde_json::json!({"type":"replay_summary","sessionId":id,"counters":monitor.counters,
        "contractVersion":momentum_strategies::CONTRACT,"availability":"idealized_completed_bar_close",
        "configSha256":format!("{:x}",Sha256::digest(config.as_bytes()))})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    session_id: String,
    symbol: String,
    bar: Bar,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--config" {
        println!("{}", serde_json::to_string(&Config::default())?);
        return Ok(());
    }
    if args.len() != 2 || args[1] != "--idealized-close-availability" {
        return Err(
            "usage: replay_strategies --idealized-close-availability (NDJSON stdin), or --config"
                .into(),
        );
    }
    let mut current: Option<(String, Monitor)> = None;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut line = Vec::new();
    let mut completed = std::collections::BTreeSet::new();
    loop {
        line.clear();
        let mut limited = std::io::Read::take(&mut input, 16_385);
        let n = limited.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if n > 16_384 {
            return Err("input line exceeds 16KiB".into());
        }
        if line.last() != Some(&b'\n') {
            return Err("unterminated NDJSON record".into());
        }
        let record: Record = serde_json::from_slice(&line)?;
        if record.bar.close_at_ms() != Some(record.bar.received_at_ms)
            || record.bar.received_at_ms != record.bar.decision_at_ms
        {
            return Err(
                "declared idealized availability requires receipt=decision=bar close".into(),
            );
        }
        if record.session_id.len() > 80 {
            return Err("invalid session identity".into());
        }
        if current
            .as_ref()
            .is_none_or(|(id, _)| *id != record.session_id)
        {
            if let Some((id, monitor)) = current.take() {
                writeln!(out, "{}", summary(&id, monitor))?;
                completed.insert(id);
            }
            if completed.len() > 10_000 || completed.contains(&record.session_id) {
                return Err("noncontiguous or excessive sessions".into());
            }
            let monitor = Monitor::new(
                record.symbol.clone(),
                record.session_id.clone(),
                Config::default(),
                true,
                true,
            )?;
            current = Some((record.session_id.clone(), monitor));
        }
        let (_, monitor) = current.as_mut().unwrap();
        // Session identity includes symbol; mixing symbols would make a false baseline.
        if record.session_id != format!("{}/{}", record.symbol, record.bar.market_day) {
            return Err("session/symbol mismatch".into());
        }
        for event in monitor.on_bar(record.bar) {
            writeln!(out, "{}", serde_json::to_string(&event)?)?;
        }
    }
    if let Some((id, monitor)) = current {
        writeln!(out, "{}", summary(&id, monitor))?;
    } else {
        return Err("no replay records".into());
    }
    out.flush()?;
    Ok(())
}
