//! Reading detections: the `panel-detection-v1` recorder files, and the
//! `live_pending_signals.jsonl` fallback for days before the recorder
//! existed. Both become `Detection`s for one market day; every line that
//! is not used is counted in `SourceReport`, never silently dropped.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use market_data::trading_session::market_day;
use serde::{Deserialize, Serialize};

use super::{Panel, COOLDOWN_SECS, IGNITION_MAX_PRICE};

pub const DETECTION_SCHEMA: &str = "panel-detection-v1";

/// One thing a panel newly showed.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub panel: Panel,
    pub symbol: String,
    pub detected_at: DateTime<Utc>,
    pub price: Option<f64>,
    pub price_source: String,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub quote_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    /// Ignition's "under $25" flag (`extra.under25`); `None` when absent.
    pub under25: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    PanelDetections,
    LivePendingFallback,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReport {
    pub kind: SourceKind,
    /// File name only (no directories), for traceability.
    pub file: Option<String>,
    pub lines: usize,
    pub used: usize,
    pub unparsable: usize,
    pub wrong_schema: usize,
    pub unknown_panel: usize,
    pub other_day: usize,
    pub note: String,
}

impl SourceReport {
    pub fn empty(kind: SourceKind, note: &str) -> Self {
        Self { kind, file: None, lines: 0, used: 0, unparsable: 0, wrong_schema: 0, unknown_panel: 0, other_day: 0, note: note.into() }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetection {
    schema: Option<String>,
    panel: String,
    symbol: String,
    detected_at: DateTime<Utc>,
    price: Option<f64>,
    price_source: Option<String>,
    bid: Option<f64>,
    ask: Option<f64>,
    quote_at: Option<DateTime<Utc>>,
    score: Option<f64>,
    #[serde(default)]
    extra: Option<serde_json::Value>,
}

/// Parses a `data/panel-detections/YYYY-MM-DD.jsonl` file's text, keeping
/// rows whose `detectedAt` falls in market day `day`.
pub fn parse_detections(text: &str, day: NaiveDate) -> (Vec<Detection>, SourceReport) {
    let mut rep = SourceReport::empty(SourceKind::PanelDetections, "recorder rows (panel-detection-v1)");
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        rep.lines += 1;
        let Ok(r) = serde_json::from_str::<RawDetection>(line) else {
            rep.unparsable += 1;
            continue;
        };
        if r.schema.as_deref().is_some_and(|s| s != DETECTION_SCHEMA) {
            rep.wrong_schema += 1;
            continue;
        }
        let Some(panel) = Panel::from_wire(&r.panel) else {
            rep.unknown_panel += 1;
            continue;
        };
        if market_day(r.detected_at) != day {
            rep.other_day += 1;
            continue;
        }
        let under25 = r.extra.as_ref().and_then(|e| e.get("under25")).and_then(|v| v.as_bool());
        out.push(Detection {
            panel,
            symbol: r.symbol.trim().to_ascii_uppercase(),
            detected_at: r.detected_at,
            price: r.price.filter(|p| p.is_finite() && *p > 0.0),
            price_source: r.price_source.unwrap_or_else(|| "unknown".into()),
            bid: r.bid,
            ask: r.ask,
            quote_at: r.quote_at,
            score: r.score.filter(|s| s.is_finite()),
            under25,
        });
        rep.used += 1;
    }
    (out, rep)
}

#[derive(Deserialize)]
struct RawPending {
    symbol: String,
    strategy: String,
    timestamp: DateTime<Utc>,
    signal_price: Option<f64>,
}

/// The panel a `live_signals::PendingSignal` strategy stands for, and the
/// `priceSource` its price had: momentum's price is the last bar close
/// (`LiveSignalTracker::last_price`), the funnel's comes off a bar-driven
/// funnel signal; the other three carry their own event price.
pub fn pending_strategy_panel(strategy: &str) -> Option<(Panel, &'static str)> {
    Some(match strategy {
        "MomentumScorer" => (Panel::Momentum, "bar_close"),
        "FastFunnel" => (Panel::GapAndGo, "bar_close"),
        "IgnitionDetector" => (Panel::Ignition, "event"),
        "Micropullback" => (Panel::Micropullback, "event"),
        "ConsolidationBreakout" => (Panel::ConsolidationBreakout, "event"),
        _ => return None,
    })
}

/// Parses `data/live_pending_signals.jsonl` (the pre-recorder fallback).
/// No bid/ask exists there, so every row is scored with the cost proxy.
/// `under25` is derived from the signal price (the client's ignition panel
/// keeps price <= $25, `userAttentionCap`).
pub fn parse_pending(text: &str, day: NaiveDate) -> (Vec<Detection>, SourceReport) {
    let mut rep = SourceReport::empty(
        SourceKind::LivePendingFallback,
        "fallback: live_pending_signals.jsonl (no quotes; edge state not reset per market day, so momentum/gap_and_go may include a first-reading-of-day row the recorder would treat as a baseline)",
    );
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        rep.lines += 1;
        let Ok(r) = serde_json::from_str::<RawPending>(line) else {
            rep.unparsable += 1;
            continue;
        };
        let Some((panel, source)) = pending_strategy_panel(&r.strategy) else {
            rep.unknown_panel += 1;
            continue;
        };
        if market_day(r.timestamp) != day {
            rep.other_day += 1;
            continue;
        }
        let price = r.signal_price.filter(|p| p.is_finite() && *p > 0.0);
        out.push(Detection {
            panel,
            symbol: r.symbol.trim().to_ascii_uppercase(),
            detected_at: r.timestamp,
            price,
            price_source: source.into(),
            bid: None,
            ask: None,
            quote_at: None,
            score: None,
            under25: (panel == Panel::Ignition).then(|| price.is_some_and(|p| p <= IGNITION_MAX_PRICE)),
        });
        rep.used += 1;
    }
    (out, rep)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filtering {
    /// Ignition rows dropped for not being under $25.
    pub ignition_over25_dropped: usize,
    /// Rows dropped by the 10-minute per-symbol cooldown (ignition,
    /// micropullback, consolidation_breakout). Recorder rows already obey
    /// it, so this is normally 0 there; it matters for the fallback.
    pub cooldown_dropped: usize,
    /// Exact duplicate rows (same panel, symbol and detectedAt).
    pub duplicates_dropped: usize,
}

/// Applies the SCHEMA edge rules this scorer can enforce: ignition under
/// $25 only (rows without the flag fall back to price <= $25; no price ->
/// kept), and the 10-minute per-symbol cooldown for the cooldown panels.
pub fn apply_edge_rules(mut dets: Vec<Detection>) -> (Vec<Detection>, Filtering) {
    let mut f = Filtering::default();
    dets.sort_by(|a, b| (a.panel, &a.symbol, a.detected_at).cmp(&(b.panel, &b.symbol, b.detected_at)));
    let before = dets.len();
    dets.dedup_by(|b, a| a.panel == b.panel && a.symbol == b.symbol && a.detected_at == b.detected_at);
    f.duplicates_dropped = before - dets.len();

    let mut last: BTreeMap<(Panel, String), DateTime<Utc>> = BTreeMap::new();
    let mut out = Vec::with_capacity(dets.len());
    for d in dets {
        if d.panel == Panel::Ignition {
            let under = d.under25.or_else(|| d.price.map(|p| p <= IGNITION_MAX_PRICE)).unwrap_or(true);
            if !under {
                f.ignition_over25_dropped += 1;
                continue;
            }
        }
        if d.panel.has_cooldown() {
            let key = (d.panel, d.symbol.clone());
            if let Some(prev) = last.get(&key) {
                if (d.detected_at - *prev).num_seconds() < COOLDOWN_SECS {
                    f.cooldown_dropped += 1;
                    continue;
                }
            }
            last.insert(key, d.detected_at);
        }
        out.push(d);
    }
    out.sort_by(|a, b| (a.detected_at, a.panel, &a.symbol).cmp(&(b.detected_at, b.panel, &b.symbol)));
    (out, f)
}
