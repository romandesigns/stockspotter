//! Preregistered Halt Early-Warning speed-alert tournament (2026-10-16).
//!
//!   halt_tournament explore --input-dir DIR --out-dir OUT --prereg-sha HEX
//!   halt_tournament holdout --input-dir DIR --out-dir OUT --prereg-sha HEX
//!
//! See outputs/halt-tournament-20261016/PREREGISTRATION.md. `explore` opens
//! only files dated on or before the cut, evaluates all 8 candidates,
//! applies the fixed selection rule and writes explore.json and
//! results.json. `holdout` runs only if explore's winner QUALIFIED: it
//! opens only files after the cut, evaluates that one candidate once (it
//! refuses if holdout.json exists) and writes holdout.json and the final
//! results.json. Nothing is fetched.
//!
//!   halt_tournament explore ... --sensitivity price-eligible-prints
//!
//! is a POST-HOC sensitivity added after the EXPLORE outcomes were seen: the
//! same 8 candidates on EXPLORE with the gauge fed only price-eligible prints.
//! It writes only `sensitivity-price-eligible-prints.json`, never explore.json
//! or results.json, and cannot run on the holdout.

use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use backtest_metrics::halt_tournament::*;
use chrono::NaiveDate;
use market_data::trading_session::market_day;
use serde_json::{json, Value};

const PREREGISTERED_CUT: &str = "2026-06-18";
const PREREGISTERED_DATES: usize = 109;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let phase = args.next().context("usage: halt_tournament explore|holdout --input-dir DIR --out-dir OUT --prereg-sha HEX")?;
    let (mut input, mut out, mut sha) = (None, None, None);
    let mut filter = PrintFilter::All;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input-dir" => input = args.next().map(PathBuf::from),
            "--out-dir" => out = args.next().map(PathBuf::from),
            "--prereg-sha" => sha = args.next(),
            "--sensitivity" => match args.next().as_deref() {
                Some("price-eligible-prints") => filter = PrintFilter::PriceEligibleOnly,
                other => bail!("unknown sensitivity {other:?}"),
            },
            other => bail!("unknown argument {other}"),
        }
    }
    let (Some(input), Some(out), Some(sha)) = (input, out, sha) else { bail!("--input-dir, --out-dir and --prereg-sha are required") };

    let mut files: Vec<(NaiveDate, PathBuf)> = std::fs::read_dir(&input)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| Some((date_from_file_name(p.file_name()?.to_str()?)?, p)))
        .collect();
    files.sort();
    let dates: Vec<NaiveDate> = files.iter().map(|f| f.0).collect();
    let distinct: std::collections::BTreeSet<NaiveDate> = dates.iter().copied().collect();
    ensure!(distinct.len() == PREREGISTERED_DATES, "{} distinct dates, preregistered {PREREGISTERED_DATES}", distinct.len());
    let cut = split_cut(&dates).context("no session files")?;
    ensure!(cut.to_string() == PREREGISTERED_CUT, "cut {cut} differs from the preregistered {PREREGISTERED_CUT}");

    match phase.as_str() {
        "explore" if filter == PrintFilter::PriceEligibleOnly => sensitivity(files.into_iter().filter(|f| f.0 <= cut).collect(), cut, &out, &sha),
        "explore" => explore(files.into_iter().filter(|f| f.0 <= cut).collect(), cut, &out, &sha),
        "holdout" if filter != PrintFilter::All => bail!("sensitivities never run on the holdout"),
        "holdout" => holdout(files.into_iter().filter(|f| f.0 > cut).collect(), cut, &out, &sha),
        other => bail!("unknown phase {other}"),
    }
}

#[derive(Default)]
struct Diag {
    trades: usize,
    readings_premarket: usize,
    readings_regular: usize,
    max_pct_premarket: u8,
    max_pct_regular: u8,
}

struct SessionOut {
    symbol: String,
    date: NaiveDate,
    per_candidate: Vec<(Vec<AlertRow>, Counts)>,
    diag: Diag,
    day_mismatch: bool,
}

fn process(cands: &[Candidate], date: NaiveDate, path: &Path, filter: PrintFilter) -> Result<SessionOut> {
    let text = std::fs::read_to_string(path)?;
    let f: DayFile = serde_json::from_str(&text)?;
    drop(text);
    let clock = Clock::of(date).context("no regular session on the file date")?;
    let day_mismatch = f.trades.first().is_some_and(|t| market_day(t.timestamp) != date);
    // Only this market day's prints from 04:00 ET up to the close.
    let trades: Vec<TradeIn> = f.trades.iter().filter(|t| clock.window(t.timestamp.timestamp()).is_some()).cloned().collect();
    let rs = readings_with(&trades, f.avg_daily_volume, filter);
    let market = Market::new(clock, date, &f.bars, &f.quotes);
    let mut diag = Diag { trades: trades.len(), ..Diag::default() };
    for r in &rs {
        let p = gauge_pct(r.ratio);
        if r.luld_in_effect {
            diag.readings_regular += 1;
            diag.max_pct_regular = diag.max_pct_regular.max(p);
        } else {
            diag.readings_premarket += 1;
            diag.max_pct_premarket = diag.max_pct_premarket.max(p);
        }
    }
    let per_candidate = evaluate_session(cands, &f.symbol, date, clock, &market, &rs, K_RANDOM, RANDOM_SEED);
    Ok(SessionOut { symbol: f.symbol, date, per_candidate, diag, day_mismatch })
}

/// Processes sessions on all cores; results sorted by (date, symbol).
fn load(cands: &[Candidate], files: &[(NaiveDate, PathBuf)], filter: PrintFilter) -> (Vec<SessionOut>, Vec<String>) {
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(10);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((date, path)) = files.get(i) else { break };
                let r = process(cands, *date, path, filter).map_err(|e| format!("{}: {e}", path.display()));
                results.lock().unwrap().push(r);
            });
        }
    });
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for r in results.into_inner().unwrap() {
        match r {
            Ok(s) => ok.push(s),
            Err(e) => bad.push(e),
        }
    }
    ok.sort_by(|a, b| (a.date, &a.symbol).cmp(&(b.date, &b.symbol)));
    bad.sort();
    (ok, bad)
}

fn data_block(files: &[(NaiveDate, PathBuf)], sessions: &[SessionOut], bad: &[String]) -> Value {
    let symbols: std::collections::BTreeSet<&str> = sessions.iter().map(|s| s.symbol.as_str()).collect();
    let dates: std::collections::BTreeSet<NaiveDate> = files.iter().map(|f| f.0).collect();
    let pct_reach = |th: u8, pre: bool| sessions.iter().filter(|s| if pre { s.diag.max_pct_premarket >= th } else { s.diag.max_pct_regular >= th }).count();
    json!({
        "files": files.len(),
        "unreadable": bad,
        "sessions": sessions.len(),
        "symbols": symbols.len(),
        "dates": dates.len(),
        "firstDate": dates.first(),
        "lastDate": dates.last(),
        "dayMismatch": sessions.iter().filter(|s| s.day_mismatch).count(),
        "tradesFed": sessions.iter().map(|s| s.diag.trades).sum::<usize>(),
        "readingsPremarket": sessions.iter().map(|s| s.diag.readings_premarket).sum::<usize>(),
        "readingsRegular": sessions.iter().map(|s| s.diag.readings_regular).sum::<usize>(),
        "sessionsReachingGauge": {
            "premarket": { "50": pct_reach(50, true), "70": pct_reach(70, true), "90": pct_reach(90, true) },
            "regular": { "50": pct_reach(50, false), "70": pct_reach(70, false), "90": pct_reach(90, false) },
        },
    })
}

fn run(cands: &[Candidate], sessions: &[SessionOut], family: usize) -> Vec<(Summary, Vec<AlertRow>)> {
    cands
        .iter()
        .enumerate()
        .map(|(j, c)| {
            let mut rows = Vec::new();
            let mut counts = Counts::default();
            for s in sessions {
                let (r, n) = &s.per_candidate[j];
                rows.extend(r.iter().cloned());
                counts.raised += n.raised;
                counts.late += n.late;
                counts.unmeasurable += n.unmeasurable;
                counts.no_baseline += n.no_baseline;
            }
            let summary = summarize(c.id, &rows, counts, family, BOOTSTRAP_B, BOOTSTRAP_SEED.wrapping_add(c.index as u64));
            (summary, rows)
        })
        .collect()
}

fn seeds() -> Value {
    json!({ "random": RANDOM_SEED, "randomPerAlert": "seed ^ fnv1a(\"{id}|{symbol}|{date}|{alertMs}\")", "bootstrap": BOOTSTRAP_SEED, "bootstrapPerCandidate": "bootstrap + candidate index", "k": K_RANDOM, "b": BOOTSTRAP_B })
}

fn explore(files: Vec<(NaiveDate, PathBuf)>, cut: NaiveDate, out: &Path, sha: &str) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let cands = candidates();
    let k = cands.len();
    let (sessions, bad) = load(&cands, &files, PrintFilter::All);
    let runs = run(&cands, &sessions, k);
    let summaries: Vec<Summary> = runs.iter().map(|r| r.0.clone()).collect();
    let win = winner(&summaries).cloned();
    let qualified = win.as_ref().is_some_and(qualifies);
    let rows: serde_json::Map<String, Value> = runs.iter().map(|(s, r)| (s.id.clone(), json!(r))).collect();
    let explore = json!({
        "phase": "explore",
        "preregistrationSha256": sha,
        "cut": cut,
        "data": data_block(&files, &sessions, &bad),
        "seeds": seeds(),
        "candidates": cands,
        "familySize": k,
        "summaries": summaries,
        "eligible": summaries.iter().filter(|s| eligible(s)).map(|s| s.id.clone()).collect::<Vec<_>>(),
        "winner": win.as_ref().map(|s| s.id.clone()),
        "qualified": qualified,
        "rows": rows,
    });
    std::fs::write(out.join("explore.json"), serde_json::to_string_pretty(&explore)?)?;
    let verdict = if qualified { "PENDING HOLDOUT" } else { "NONE QUALIFIED" };
    let mut brief = explore.clone();
    brief.as_object_mut().unwrap().remove("rows");
    std::fs::write(out.join("results.json"), serde_json::to_string_pretty(&json!({ "verdict": verdict, "holdoutOpened": false, "explore": brief, "holdout": Value::Null }))?)?;
    eprintln!("explore done: winner {:?}, qualified {qualified}", win.map(|s| s.id));
    Ok(())
}

fn holdout(files: Vec<(NaiveDate, PathBuf)>, cut: NaiveDate, out: &Path, sha: &str) -> Result<()> {
    let hold_path = out.join("holdout.json");
    ensure!(!hold_path.exists(), "holdout.json exists: the holdout is evaluated once");
    let explore: Value = serde_json::from_str(&std::fs::read_to_string(out.join("explore.json")).context("run explore first")?)?;
    ensure!(explore["preregistrationSha256"].as_str() == Some(sha), "preregistration hash differs from explore's");
    ensure!(explore["qualified"].as_bool() == Some(true), "no candidate qualified on EXPLORE: the verdict is NONE QUALIFIED and the holdout stays closed");
    let id = explore["winner"].as_str().context("no winner")?;
    let c = candidates().into_iter().find(|c| c.id == id).context("unknown candidate")?;
    let family = explore["familySize"].as_u64().context("familySize")? as usize;
    let one = [c.clone()];
    let (sessions, bad) = load(&one, &files, PrintFilter::All);
    // Same constants; `c.index` (its explore index) keeps its bootstrap seed,
    // and the random draws are keyed by id, not position.
    let runs = run(&one, &sessions, family);
    let summary = runs[0].0.clone();
    let confirmed = confirms(&summary);
    let hold = json!({
        "phase": "holdout",
        "preregistrationSha256": sha,
        "cut": cut,
        "data": data_block(&files, &sessions, &bad),
        "seeds": seeds(),
        "candidate": c,
        "summary": summary,
        "confirmed": confirmed,
        "rows": runs[0].1,
    });
    std::fs::write(&hold_path, serde_json::to_string_pretty(&hold)?)?;
    let verdict = if confirmed { "CONFIRMED" } else { "NOT CONFIRMED" };
    let mut brief_e = explore.clone();
    brief_e.as_object_mut().unwrap().remove("rows");
    let mut brief_h = hold.clone();
    brief_h.as_object_mut().unwrap().remove("rows");
    std::fs::write(out.join("results.json"), serde_json::to_string_pretty(&json!({ "verdict": verdict, "holdoutOpened": true, "explore": brief_e, "holdout": brief_h }))?)?;
    eprintln!("holdout done: {verdict}");
    Ok(())
}

/// POST-HOC sensitivity (see the module doc): EXPLORE only, its own file.
fn sensitivity(files: Vec<(NaiveDate, PathBuf)>, cut: NaiveDate, out: &Path, sha: &str) -> Result<()> {
    let cands = candidates();
    let k = cands.len();
    let (sessions, bad) = load(&cands, &files, PrintFilter::PriceEligibleOnly);
    let runs = run(&cands, &sessions, k);
    let summaries: Vec<Summary> = runs.iter().map(|r| r.0.clone()).collect();
    let rows: serde_json::Map<String, Value> = runs.iter().map(|(s, r)| (s.id.clone(), json!(r))).collect();
    let doc = json!({
        "phase": "explore-sensitivity",
        "posthoc": true,
        "note": "Added after the preregistered EXPLORE outcomes were seen. Gauge fed only price-eligible prints. Not part of the verdict; the holdout is not opened.",
        "printFilter": "price-eligible-only",
        "preregistrationSha256": sha,
        "cut": cut,
        "data": data_block(&files, &sessions, &bad),
        "seeds": seeds(),
        "familySize": k,
        "summaries": summaries,
        "winnerByPreregisteredRule": winner(&summaries).map(|s| s.id.clone()),
        "wouldQualify": winner(&summaries).is_some_and(qualifies),
        "rows": rows,
    });
    std::fs::write(out.join("sensitivity-price-eligible-prints.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!("sensitivity done");
    Ok(())
}
