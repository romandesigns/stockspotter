//! Shapes an order-flow read for the wire, and says how old the parts of
//! it are.
//!
//! A full summary carries three arrays that exist only to draw charts:
//! `deltaBuckets` (one entry per 30 seconds of the session), `largePrints`
//! and `profile.bins`. With session backfill a busy symbol's buckets alone
//! pass 100 KB, and a client polls every few seconds -- measured on the
//! first backfilled run, 2026-10-06. Everything else in the summary is a
//! couple of kilobytes.
//!
//! So there are two shapes:
//! - **lite**: the summary without the chart arrays. What a client polls
//!   at its normal cadence, what the push carries, and what the recorder
//!   writes. `market_data::order_flow_store::lite_read` builds it, from a
//!   cache, so a fan-out of consumers does not rebuild the arrays per
//!   consumer only to have them thrown away here.
//! - **full**: everything, with `deltaBuckets` limited to the most recent
//!   `MAX_CLIENT_BUCKETS`. What a client fetches occasionally to refresh
//!   its charts.
//!
//! **Why the arrays carry their own timestamp.** A client holds the arrays
//! from its last full fetch and keeps showing them while it polls lite
//! summaries. The header numbers are then seconds old and the arrays
//! minutes old, which on screen looks identical. So a full body carries
//! `chartArraysAsOfSecs` equal to its own `asOfSecs`, the client carries
//! that value forward with the arrays, and the UI can say how stale the
//! delta pane and the profile actually are. The alternative -- refusing to
//! carry them forward at all -- makes the pane blank between full fetches,
//! which is worse and no more honest.

use market_data::order_flow_store::Read;
use serde_json::Value;

/// Three hours of 30-second buckets. The delta pane shows at most this
/// much; cumulative delta for the whole session is still exact, because
/// it is carried as its own totals and not summed from these.
pub const MAX_CLIENT_BUCKETS: usize = 360;

/// Which shape a response body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// No chart arrays. `chartArraysAsOfSecs` is absent: this body says
    /// nothing about them, and the client keeps whatever it has.
    Lite,
    /// Chart arrays included and freshly built, so their age is this
    /// body's own `asOfSecs`.
    Full,
}

/// One response or pushed frame. `enabled` and a null `summary` are
/// different answers -- the feature being off on this server, versus on
/// but this symbol not having traded while tracked -- and a client shows
/// them differently, which is why both are a 200 with the distinction in
/// the body rather than a 404.
pub fn body(symbol: &str, read: Option<Read>, detail: Detail) -> Value {
    let Some(read) = read else {
        return serde_json::json!({
            "enabled": market_data::order_flow_store::enabled(),
            "symbol": symbol,
            "summary": Value::Null,
        });
    };
    let mut summary = serde_json::to_value(&read.summary).unwrap_or(Value::Null);
    if detail == Detail::Full {
        keep_recent_buckets(&mut summary, MAX_CLIENT_BUCKETS);
        stamp_chart_arrays(&mut summary);
    }
    serde_json::json!({
        "enabled": true,
        "symbol": symbol,
        "backfill": read.backfill,
        // How much of today the numbers cover. Separate from `backfill`
        // because an engine that was evicted and recreated has a hole in
        // its day that no backfill of the earlier session repairs.
        "coverage": read.coverage,
        // Correction and cancel frames are counted, never applied. This is
        // process-wide evidence that the tape is uncorrected, not a
        // per-symbol reconciliation claim.
        "tape": {
            "provenance": read.provenance,
            "correctionsSeen": read.corrections_seen,
            "cancelsSeen": read.cancels_seen,
        },
        "summary": summary,
    })
}

/// Keeps only the newest `max` delta buckets, in place.
pub fn keep_recent_buckets(summary: &mut Value, max: usize) {
    if let Some(buckets) = summary.get_mut("deltaBuckets").and_then(Value::as_array_mut) {
        if buckets.len() > max {
            buckets.drain(..buckets.len() - max);
        }
    }
}

/// Records that this body's chart arrays are as current as the rest of it.
/// Only a full body may say this.
fn stamp_chart_arrays(summary: &mut Value) {
    let as_of = summary.get("asOfSecs").cloned();
    if let (Some(object), Some(as_of)) = (summary.as_object_mut(), as_of) {
        object.insert("chartArraysAsOfSecs".into(), as_of);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use market_data::order_flow_store::{BackfillStatus, Coverage, TapeProvenance};
    use order_flow::{OrderFlowEngine, Session, Trade, TradeFlags};

    fn engine_read(detail: Detail) -> Value {
        let mut engine = OrderFlowEngine::default();
        engine.on_quote(order_flow::Quote { timestamp_secs: 0.0, bid_price: 2.00, bid_size: 5, ask_price: 2.02, ask_size: 5 });
        for i in 0..3 {
            engine.on_trade(Trade { timestamp_secs: f64::from(i) * 40.0, price: 2.02, size: 100 }, Session::Regular, TradeFlags::default());
        }
        let mut summary = engine.summary(120.0);
        if detail == Detail::Lite {
            summary.delta_buckets = Vec::new();
            summary.large_prints = Vec::new();
            if let Some(profile) = summary.profile.as_mut() {
                profile.bins = Vec::new();
            }
        }
        body(
            "AAA",
            Some(Read {
                summary,
                coverage: Coverage::Partial,
                backfill: BackfillStatus::None,
                provenance: TapeProvenance::CorrectionsPending,
                corrections_seen: 2,
                cancels_seen: 1,
            }),
            detail,
        )
    }

    #[test]
    fn a_lite_body_carries_no_arrays_and_claims_nothing_about_their_age() {
        let v = engine_read(Detail::Lite);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["symbol"], "AAA");
        assert_eq!(v["coverage"], "partial");
        assert_eq!(v["backfill"], "none");
        assert!(v["summary"]["deltaBuckets"].as_array().unwrap().is_empty());
        assert!(v["summary"]["profile"]["bins"].as_array().unwrap().is_empty());
        // Absent, not null: a client must keep the arrays it already has
        // and the age it already recorded for them.
        assert!(v["summary"].get("chartArraysAsOfSecs").is_none());
    }

    #[test]
    fn a_full_body_stamps_its_arrays_with_its_own_as_of() {
        let v = engine_read(Detail::Full);
        assert!(!v["summary"]["deltaBuckets"].as_array().unwrap().is_empty());
        assert_eq!(v["summary"]["chartArraysAsOfSecs"], v["summary"]["asOfSecs"]);
    }

    #[test]
    fn the_uncorrected_tape_is_stated_with_its_counts() {
        let v = engine_read(Detail::Lite);
        assert_eq!(v["tape"]["provenance"], "corrections_pending");
        assert_eq!(v["tape"]["correctionsSeen"], 2);
        assert_eq!(v["tape"]["cancelsSeen"], 1);
    }

    #[test]
    fn no_read_is_a_shape_a_client_can_tell_apart_from_a_summary() {
        // ORDER_FLOW_ENABLED is unset under test, so this is what a server
        // with the feature off answers.
        let v = body("AAA", None, Detail::Full);
        assert_eq!(v["enabled"], false);
        assert!(v["summary"].is_null());
        assert!(v.get("coverage").is_none());
    }

    #[test]
    fn full_keeps_the_newest_buckets_in_order() {
        let mut v = serde_json::json!({ "deltaBuckets": [{ "startSecs": 0.0 }, { "startSecs": 30.0 }, { "startSecs": 60.0 }] });
        keep_recent_buckets(&mut v, 2);
        let starts: Vec<f64> = v["deltaBuckets"].as_array().unwrap().iter().map(|b| b["startSecs"].as_f64().unwrap()).collect();
        assert_eq!(starts, vec![30.0, 60.0]);
        // Fewer than the limit: untouched.
        keep_recent_buckets(&mut v, 10);
        assert_eq!(v["deltaBuckets"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn shaping_tolerates_a_missing_profile_and_non_objects() {
        let mut no_profile = serde_json::json!({ "profile": null });
        keep_recent_buckets(&mut no_profile, 1);
        stamp_chart_arrays(&mut no_profile);
        assert!(no_profile["profile"].is_null());
        assert!(no_profile.get("chartArraysAsOfSecs").is_none());
        let mut null = Value::Null;
        keep_recent_buckets(&mut null, 1);
        stamp_chart_arrays(&mut null);
        assert!(null.is_null());
    }
}
