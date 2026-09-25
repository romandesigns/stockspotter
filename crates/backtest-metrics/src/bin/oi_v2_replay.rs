//! Early V2 isolated paired replay (Option C).
//!
//! ```text
//! oi_v2_replay <evaluation-<date>.ndjson> <session-date> <out.ndjson>
//! ```
//!
//! Reads the frozen evaluation artifact, reconstructs the minimal causal state
//! Early V2 needs, and calls **the real `early_quality_v2`** — not a
//! reimplementation — so replay cannot diverge from live by construction.
//!
//! # Why only Early
//!
//! `OpportunityScoreSnapshot` does not persist `observed_high`, `observed_low`,
//! `min_move_pct` or `max_move_pct`. RiskQuality's preregistered core
//! (`risk.drawdownFromHigh`) is therefore not reconstructable from these
//! artifacts, and Priority V2 depends on RiskQuality. Both are skipped rather
//! than approximated: substituting a proxy would silently evaluate a formula
//! nobody preregistered.
//!
//! # Faithful reconstruction
//!
//! Every Early V2 input is taken from the record, never inferred:
//!   move_before_detection_pct  <- record
//!   latest_price               <- currentPrice
//!   session_low_observed       <- features.preDetection
//!   ignition.*                 <- features.ignition
//!   invalidations_absorbed     <- record
//!   episode_fragments          <- record
//!   opened_at                  <- scoreTimestamp − opportunityAgeSecs
//!     (so `at − opened_at` reproduces the persisted age exactly rather than
//!      re-deriving it from a field the artifact does not carry)

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufWriter, Write};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use backtest_metrics::context::{IgnitionFeatures, IgnitionPhase, MomentumFeatures,
                                PreDetectionContext, SignalContext};
use backtest_metrics::opportunity::{Opportunity, OpportunityId, OPPORTUNITY_SCHEMA_VERSION};
use backtest_metrics::opportunity_v2::{
    early_quality_v2, momentum_availability, MomentumAvailability, ScoringMode, V2Config,
};
use backtest_metrics::signals::Strategy;

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}
fn u32_of(v: &Value, k: &str) -> u32 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32
}
fn ts(v: &Value, k: &str) -> Option<DateTime<Utc>> {
    v.get(k)?.as_str()?.parse::<DateTime<Utc>>().ok()
}

/// Is this **individual ranking record** inside the regular session?
///
/// `lo` is inclusive, `hi` exclusive, both `YYYY-MM-DDTHH:MM:SS` with no zone
/// suffix; `at_str` is RFC3339 and is compared on its first 19 characters, so
/// sub-second digits never affect the boundary.
///
/// # D2
///
/// The original replay asked this question **once per opportunity, about its
/// first record**, and applied the answer to every later record. That is wrong
/// in both directions: an opportunity opened overnight was dropped for the
/// whole session even while it ranked all day (DCX contributed 1,921 snapshots
/// and was excluded entirely), and an opportunity opened at 13:31 kept emitting
/// long after 20:00. Regular-session membership is a property of the
/// observation, not of the opportunity.
fn in_regular_session(at_str: &str, lo: &str, hi: &str) -> bool {
    at_str.len() >= 19 && &at_str[..19] >= lo && &at_str[..19] < hi
}

/// Per-opportunity lifecycle state accumulated over its **full** history.
///
/// # D2, the half that is easy to get wrong
///
/// Fixing the window filter is not enough on its own. This state must keep
/// being updated by records that fall *outside* the evaluation window, because
/// it is causal: an opportunity whose momentum first appeared at 09:00
/// premarket is `SeenPreviouslyButStale` at 13:30, not `NeverSeenYet`. Filter
/// first and you silently rewrite the momentum lifecycle of every opportunity
/// that predates the open — which is precisely the population D2 lost.
///
/// Reading a pre-session record is not lookahead: it is strictly in the past
/// of the record being scored.
#[derive(Default)]
struct Lifecycle {
    momentum_seen: BTreeMap<String, Option<DateTime<Utc>>>,
}

impl Lifecycle {
    /// Record one observation and return `(ever_seen, first_momentum_at)` as of
    /// this instant. Must be called for every record, in timestamp order,
    /// before the window filter.
    fn observe(
        &mut self,
        oid: &str,
        at: DateTime<Utc>,
        has_momentum: bool,
    ) -> (bool, Option<DateTime<Utc>>) {
        let entry = self.momentum_seen.entry(oid.to_string()).or_insert(None);
        if has_momentum && entry.is_none() {
            *entry = Some(at);
        }
        (entry.is_some(), *entry)
    }
}

/// One emitted row. Deliberately narrow: this is a paired comparison file,
/// not a second copy of the session.
#[derive(serde::Serialize)]
struct Row<'a> {
    #[serde(rename = "opportunityId")]
    oid: &'a str,
    symbol: &'a str,
    #[serde(rename = "scoreTimestamp")]
    at: &'a str,
    #[serde(rename = "windowId")]
    window: &'a str,
    regime: &'a Value,
    #[serde(rename = "opportunityAgeSecs")]
    age: f64,
    #[serde(rename = "moveBeforeDetectionPct")]
    move_before: Option<f64>,
    #[serde(rename = "moveFromStartPct")]
    move_from_start: Option<f64>,

    #[serde(rename = "v1EarlyValue")]
    v1_early: Option<f64>,
    #[serde(rename = "v1EarlyRank")]
    v1_early_rank: Option<u64>,
    #[serde(rename = "v1EarlyCohort")]
    v1_early_cohort: Option<u64>,
    #[serde(rename = "v1ContinuationValue")]
    v1_cont: Option<f64>,
    #[serde(rename = "v1ContinuationRank")]
    v1_cont_rank: Option<u64>,
    #[serde(rename = "v1ContinuationCohort")]
    v1_cont_cohort: Option<u64>,

    #[serde(rename = "v2EarlyValue")]
    v2_early: Option<f64>,
    #[serde(rename = "v2ModeATerm")]
    v2_mode_a: Option<f64>,
    #[serde(rename = "v2MomentumTerm")]
    v2_mterm: Option<f64>,
    #[serde(rename = "v2ScoringMode")]
    v2_mode: &'static str,
    #[serde(rename = "v2MomentumAvailability")]
    v2_avail: &'static str,
    #[serde(rename = "v2Coverage")]
    v2_coverage: f64,
    #[serde(rename = "v2PresentInputs")]
    v2_inputs: usize,
    #[serde(rename = "v2Unrankable")]
    v2_unrankable: Option<String>,

    /// Outcome, carried straight through from the frozen join. Never recomputed.
    #[serde(rename = "outcome", skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a Value>,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().context("usage: oi_v2_replay <eval.ndjson> <date> <out.ndjson>")?;
    let session = args.next().context("session date required")?;
    let out_path = args.next().context("output path required")?;

    // The regular session on the New York clock (D13). Was a fixed
    // 13:30-20:00Z, which is 09:30-16:00 ET only under EDT.
    let session_day: chrono::NaiveDate =
        session.parse().with_context(|| format!("session date {session} is not YYYY-MM-DD"))?;
    let (open, close) = backtest_metrics::alpha::labels::session_bounds(session_day)
        .with_context(|| format!("{session} has no regular session"))?;
    let lo = open.format("%Y-%m-%dT%H:%M:%S").to_string();
    let hi = close.format("%Y-%m-%dT%H:%M:%S").to_string();

    let cfg = V2Config::default();
    eprintln!("  v2 config fingerprint: {}", cfg.fingerprint());

    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::with_capacity(1 << 20, file);
    let out = std::fs::File::create(&out_path)?;
    let mut writer = BufWriter::with_capacity(1 << 20, out);

    // Bounded: one optional timestamp per opportunity.
    let mut lifecycle = Lifecycle::default();

    // D2 population accounting (§5 of the authorization).
    let mut first_record_outside: BTreeMap<String, bool> = BTreeMap::new();
    let mut emitted_opps: BTreeSet<String> = BTreeSet::new();
    let mut recovered_opps: BTreeSet<String> = BTreeSet::new();
    let mut recovered_records = 0u64;

    let mut records = 0u64;
    let mut emitted = 0u64;
    let mut malformed = 0u64;
    let mut out_of_window = 0u64;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            malformed += 1;
            continue;
        };
        records += 1;

        let oid = v.get("opportunityId").and_then(|x| x.as_str()).unwrap_or("");
        let at_str = v.get("scoreTimestamp").and_then(|x| x.as_str()).unwrap_or("");

        // D2: decided per record, not once per opportunity. Evaluated here but
        // NOT acted on until the causal lifecycle below has been updated.
        let in_window = in_regular_session(at_str, &lo, &hi);
        let first_outside = *first_record_outside
            .entry(oid.to_string())
            .or_insert(!in_window);

        let Some(at) = ts(&v, "scoreTimestamp") else { malformed += 1; continue };
        let feats = v.get("features").cloned().unwrap_or(Value::Null);

        // --- momentum -----------------------------------------------------
        let m = feats.get("momentum").and_then(|m| {
            Some(MomentumFeatures {
                overall: f(m, "overall")?,
                volume_confirmation: f(m, "volumeConfirmation")?,
                structure: f(m, "structure")?,
                ma_slope: f(m, "maSlope")?,
                wick_rejection: f(m, "wickRejection")?,
                qualifies: m.get("qualifies").and_then(|x| x.as_bool()).unwrap_or(false),
                observed_at: ts(m, "observedAt")?,
            })
        });
        // Causal: updated by EVERY record, including pre-open ones. See the
        // `Lifecycle` doc comment for why filtering before this point is the
        // subtle half of D2.
        let (ever_seen, first_momentum_at) = lifecycle.observe(oid, at, m.is_some());

        // Only now is it safe to drop the record from the evaluation set.
        if !in_window {
            out_of_window += 1;
            continue;
        }
        if first_outside {
            recovered_records += 1;
            recovered_opps.insert(oid.to_string());
        }
        emitted_opps.insert(oid.to_string());

        // --- ignition -----------------------------------------------------
        let ign = feats.get("ignition").and_then(|i| {
            let phase = match i.get("phase").and_then(|x| x.as_str())? {
                "follow_through_confirmed" | "FollowThroughConfirmed" => {
                    IgnitionPhase::FollowThroughConfirmed
                }
                "follow_through_rejected" | "FollowThroughRejected" => {
                    IgnitionPhase::FollowThroughRejected
                }
                _ => IgnitionPhase::CandidateOpened,
            };
            Some(IgnitionFeatures {
                phase,
                candidates_opened: u32_of(i, "candidatesOpened"),
                confirmations: u32_of(i, "confirmations"),
                rejections: u32_of(i, "rejections"),
                price_at_phase: f(i, "priceAtPhase").unwrap_or(0.0),
                phase_at: ts(i, "phaseAt").unwrap_or(at),
            })
        });

        // --- pre-detection -------------------------------------------------
        let pre = feats.get("preDetection").map(|p| PreDetectionContext {
            price_1m_before: f(p, "price1mBefore"),
            price_3m_before: f(p, "price3mBefore"),
            price_5m_before: f(p, "price5mBefore"),
            session_low_observed: f(p, "sessionLowObserved"),
            first_observed_price: f(p, "firstObservedPrice"),
            // Absent stays absent. Defaulting to `at` (as this did before
            // 2026-09-25) fabricated "first seen at the signal" for every row
            // that had no baseline -- measurement-correctness contract, D3.
            first_observed_at: ts(p, "firstObservedAt"),
            move_before_detection_pct: f(p, "moveBeforeDetectionPct"),
            // Carried through verbatim so a replayed context keeps declaring
            // which baseline contract its values were measured under. Absent
            // on schema-1 rows, and must stay absent.
            market_day: p
                .get("marketDay")
                .and_then(|x| x.as_str())
                .and_then(|d| d.parse().ok()),
            observation_started_at: ts(p, "observationStartedAt"),
            baseline_truncated: p.get("baselineTruncated").and_then(|x| x.as_bool()),
        });

        let ctx = SignalContext {
            // The schema the row was WRITTEN under, not the one this binary
            // was compiled with: a schema-1 row re-labelled 2 would claim a
            // market-day baseline it does not have. Rows always carry it; the
            // fallback is the oldest contract, never the newest.
            schema_version: feats
                .get("schemaVersion")
                .and_then(|x| x.as_u64())
                .map(|x| x as u32)
                .unwrap_or(1),
            symbol: v.get("symbol").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            session_date: session.clone(),
            strategy: Strategy::IgnitionDetector,
            detected_at: at,
            captured_at: at,
            signal_price: f(&v, "currentPrice").unwrap_or(0.0),
            market: None,
            funnel: None,
            ignition: ign,
            momentum: m,
            consolidation: None,
            halt: None,
            catalyst: None,
            pre_detection: pre,
            episode_id: None,
        };

        let age = f(&v, "opportunityAgeSecs").unwrap_or(0.0);
        // opened_at is NOT in the artifact; derive it so that `at − opened_at`
        // reproduces the persisted age exactly.
        let opened_at = at - Duration::seconds(age as i64);

        let op = Opportunity {
            schema_version: OPPORTUNITY_SCHEMA_VERSION,
            id: OpportunityId {
                symbol: ctx.symbol.clone(),
                session_date: session.clone(),
                sequence: 1,
            },
            symbol: ctx.symbol.clone(),
            session_date: session.clone(),
            first_seen_at: opened_at,
            opened_at,
            last_seen_at: at,
            opening_price: 0.0,
            latest_price: f(&v, "currentPrice").unwrap_or(0.0),
            first_detector: Strategy::IgnitionDetector,
            detectors_seen: BTreeMap::new(),
            raw_event_count: u32_of(&v, "rawEventCount"),
            detector_transitions: 0,
            episode_fragments: u32_of(&v, "episodeFragments"),
            invalidations_absorbed: u32_of(&v, "invalidationsAbsorbed"),
            move_before_detection_pct: f(&v, "moveBeforeDetectionPct"),
            move_from_start_pct: f(&v, "moveFromStartPct"),
            // Deliberately absent from the artifact -- see module docs. Left at
            // defaults because Early V2 does not read them; RiskQuality, which
            // does, is not evaluated here.
            max_move_pct: None,
            min_move_pct: None,
            observed_high: 0.0,
            observed_low: 0.0,
            latest_context: Some(ctx),
            detection_context: None,
            detection_context_emitted: false,
            closed_at: None,
            close_reason: None,
        };

        let avail = momentum_availability(&op, at, ever_seen, &cfg);
        let early = early_quality_v2(&op, at, avail, first_momentum_at, &cfg);

        let eq = v.get("earlyQuality");
        let co = v.get("continuation");

        let row = Row {
            oid,
            symbol: v.get("symbol").and_then(|x| x.as_str()).unwrap_or(""),
            at: at_str,
            window: v.get("windowId").and_then(|x| x.as_str()).unwrap_or(""),
            regime: v.get("regime").unwrap_or(&Value::Null),
            age,
            move_before: f(&v, "moveBeforeDetectionPct"),
            move_from_start: f(&v, "moveFromStartPct"),
            v1_early: eq.and_then(|e| e.get("value")).and_then(|x| x.as_f64()),
            v1_early_rank: v.get("earlyQualityRank").and_then(|x| x.as_u64()),
            v1_early_cohort: v.get("earlyCohortSize").and_then(|x| x.as_u64()),
            v1_cont: co.and_then(|c| c.get("value")).and_then(|x| x.as_f64()),
            v1_cont_rank: v.get("continuationRank").and_then(|x| x.as_u64()),
            v1_cont_cohort: v.get("continuationCohortSize").and_then(|x| x.as_u64()),
            v2_early: early.score.value,
            v2_mode_a: early.mode_a_term,
            v2_mterm: early.momentum_term,
            v2_mode: match early.scoring_mode {
                ScoringMode::MomentumIndependent => "momentum_independent",
                ScoringMode::MomentumInformed => "momentum_informed",
            },
            v2_avail: match early.momentum_availability {
                MomentumAvailability::NeverSeenYet => "never_seen_yet",
                MomentumAvailability::CurrentlyAvailable => "currently_available",
                MomentumAvailability::SeenPreviouslyButStale => "seen_previously_but_stale",
            },
            v2_coverage: early.score.coverage,
            v2_inputs: early.score.present_inputs,
            v2_unrankable: early.score.unrankable_reason.map(|r| format!("{r:?}")),
            outcome: v.get("outcome"),
        };
        writeln!(writer, "{}", serde_json::to_string(&row)?)?;
        emitted += 1;
    }
    writer.flush()?;

    eprintln!(
        "  {session}: {records} records read, {emitted} in-session rows emitted, \
         {out_of_window} outside window, {malformed} malformed, {} opportunities",
        emitted_opps.len()
    );
    eprintln!(
        "  D2 recovery: {recovered_records} records and {} opportunities recovered \
         (first record outside the window, contributes in-window rows)",
        recovered_opps.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LO: &str = "2026-09-17T13:30:00";
    const HI: &str = "2026-09-17T20:00:00";

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    // ---- D2 window predicate -------------------------------------------

    /// TEST F: the lower bound is inclusive.
    #[test]
    fn boundary_1330_exactly_is_included() {
        assert!(in_regular_session("2026-09-17T13:30:00.000000000Z", LO, HI));
        assert!(in_regular_session("2026-09-17T13:30:00Z", LO, HI));
    }

    /// TEST G: the upper bound is exclusive.
    #[test]
    fn boundary_2000_exactly_is_excluded() {
        assert!(!in_regular_session("2026-09-17T20:00:00.000000000Z", LO, HI));
        assert!(!in_regular_session("2026-09-17T20:00:00Z", LO, HI));
    }

    /// TEST H: sub-second digits never move a record across either edge.
    #[test]
    fn subsecond_boundaries_are_stable() {
        assert!(!in_regular_session("2026-09-17T13:29:59.999999999Z", LO, HI));
        assert!(in_regular_session("2026-09-17T13:30:00.000000001Z", LO, HI));
        assert!(in_regular_session("2026-09-17T19:59:59.999999999Z", LO, HI));
        assert!(!in_regular_session("2026-09-17T20:00:00.000000001Z", LO, HI));
    }

    #[test]
    fn premarket_and_afterhours_are_excluded() {
        assert!(!in_regular_session("2026-09-17T00:00:30Z", LO, HI));
        assert!(!in_regular_session("2026-09-17T08:05:36Z", LO, HI));
        assert!(!in_regular_session("2026-09-17T23:59:57Z", LO, HI));
    }

    #[test]
    fn a_truncated_timestamp_is_never_in_session() {
        assert!(!in_regular_session("", LO, HI));
        assert!(!in_regular_session("2026-09-17", LO, HI));
    }

    // ---- D2 lifecycle ---------------------------------------------------

    /// TEST C: pre-open causal state survives the filter and drives the first
    /// regular-session record. This is the defect's subtle half: momentum first
    /// seen at 09:00 must read as "seen previously", never "never seen".
    #[test]
    fn premarket_momentum_is_remembered_at_the_open() {
        let mut lc = Lifecycle::default();
        let (seen, first) = lc.observe("DCX:2026-09-17:1", at("2026-09-17T09:00:00Z"), true);
        assert!(seen);
        assert_eq!(first, Some(at("2026-09-17T09:00:00Z")));

        // First regular-session record, no momentum block of its own.
        let (seen, first) = lc.observe("DCX:2026-09-17:1", at("2026-09-17T13:30:00Z"), false);
        assert!(seen, "premarket momentum must still count as ever-seen");
        assert_eq!(
            first,
            Some(at("2026-09-17T09:00:00Z")),
            "first-momentum instant must be the premarket one, not the open"
        );
    }

    /// The inverse: an opportunity that has genuinely never shown momentum
    /// still reports NeverSeenYet, so the fix cannot manufacture history.
    #[test]
    fn never_seen_stays_never_seen() {
        let mut lc = Lifecycle::default();
        for t in ["2026-09-17T09:00:00Z", "2026-09-17T13:30:00Z", "2026-09-17T15:00:00Z"] {
            let (seen, first) = lc.observe("QUIET:2026-09-17:1", at(t), false);
            assert!(!seen);
            assert!(first.is_none());
        }
    }

    #[test]
    fn lifecycle_is_per_opportunity() {
        let mut lc = Lifecycle::default();
        lc.observe("A:2026-09-17:1", at("2026-09-17T09:00:00Z"), true);
        let (seen, _) = lc.observe("B:2026-09-17:1", at("2026-09-17T09:00:00Z"), false);
        assert!(!seen, "momentum on A must not leak into B");
    }

    #[test]
    fn first_momentum_instant_is_not_overwritten_by_later_sightings() {
        let mut lc = Lifecycle::default();
        lc.observe("A:2026-09-17:1", at("2026-09-17T09:00:00Z"), true);
        let (_, first) = lc.observe("A:2026-09-17:1", at("2026-09-17T14:00:00Z"), true);
        assert_eq!(first, Some(at("2026-09-17T09:00:00Z")));
    }

    // ---- D2 end-to-end selection ----------------------------------------

    /// Drives the real predicate + lifecycle over a synthetic record stream,
    /// returning (emitted timestamps, ever_seen at each emitted record).
    fn run(records: &[(&str, bool)]) -> (Vec<String>, Vec<bool>) {
        let mut lc = Lifecycle::default();
        let (mut kept, mut seen_at_kept) = (vec![], vec![]);
        for (t, has_mom) in records {
            let in_window = in_regular_session(t, LO, HI);
            let (ever, _) = lc.observe("X:2026-09-17:1", at(t), *has_mom);
            if !in_window {
                continue;
            }
            kept.push((*t).to_string());
            seen_at_kept.push(ever);
        }
        (kept, seen_at_kept)
    }

    /// TEST A + TEST B: an opportunity straddling the open contributes exactly
    /// its regular-session records — and TEST I, the DCX shape: opened around
    /// midnight, alive all day.
    #[test]
    fn straddling_opportunity_contributes_only_its_session_records() {
        let (kept, _) = run(&[
            ("2026-09-17T00:00:30Z", false),
            ("2026-09-17T08:05:36Z", false),
            ("2026-09-17T13:30:00Z", false),
            ("2026-09-17T15:11:25Z", false),
            ("2026-09-17T19:59:59Z", false),
            ("2026-09-17T20:00:00Z", false),
            ("2026-09-17T23:59:57Z", false),
        ]);
        assert_eq!(
            kept,
            vec![
                "2026-09-17T13:30:00Z",
                "2026-09-17T15:11:25Z",
                "2026-09-17T19:59:59Z"
            ],
            "the old harness dropped all seven of these"
        );
    }

    /// TEST C again, at the level the replay actually runs it.
    #[test]
    fn straddling_opportunity_carries_its_premarket_momentum_across_the_open() {
        let (kept, seen) = run(&[
            ("2026-09-17T08:05:36Z", true), // momentum, premarket
            ("2026-09-17T13:30:00Z", false),
            ("2026-09-17T14:00:00Z", false),
        ]);
        assert_eq!(kept.len(), 2);
        assert_eq!(seen, vec![true, true], "premarket momentum must survive the open");
    }

    /// TEST D: an opportunity that opens inside the session is unaffected by
    /// the fix — the regression guard on the common case.
    #[test]
    fn intraday_opportunity_behaviour_is_unchanged() {
        let (kept, seen) = run(&[
            ("2026-09-17T14:00:00Z", false),
            ("2026-09-17T14:00:30Z", true),
            ("2026-09-17T14:01:00Z", false),
        ]);
        assert_eq!(kept.len(), 3);
        assert_eq!(seen, vec![false, true, true]);
    }

    /// TEST E: dies before the open, contributes nothing.
    #[test]
    fn opportunity_ending_before_the_open_contributes_nothing() {
        let (kept, _) = run(&[
            ("2026-09-17T08:00:00Z", true),
            ("2026-09-17T09:30:00Z", true),
            ("2026-09-17T13:29:59Z", true),
        ]);
        assert!(kept.is_empty());
    }

    /// The other direction of D2: the old filter was sticky both ways, so an
    /// opportunity opening at 13:31 kept emitting past 20:00.
    #[test]
    fn session_opportunity_stops_emitting_after_the_close() {
        let (kept, _) = run(&[
            ("2026-09-17T13:31:00Z", false),
            ("2026-09-17T19:59:00Z", false),
            ("2026-09-17T20:30:00Z", false),
            ("2026-09-17T22:00:00Z", false),
        ]);
        assert_eq!(kept, vec!["2026-09-17T13:31:00Z", "2026-09-17T19:59:00Z"]);
    }
}
