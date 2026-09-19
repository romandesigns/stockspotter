//! **Blocking.** Model and rank freeze proof (brief section 7).
//!
//! The capacity repair is only admissible if it changed storage and bounds and
//! nothing else. This drives the engine **exactly as deployed** at
//! `79c21e16c3fac00f36d52a20828ff65f56657acd` (compiled from `frozen/mod.rs`)
//! and the repaired engine through one identical causal fixture, and requires
//! the research output to be byte-identical wherever the old bound was not the
//! thing that differed.
//!
//! # The two regimes, and why both are needed
//!
//! * **Below the old bound**, nothing may differ at all. Same opportunity IDs,
//!   same open and close timestamps, same regimes, same scores bit-for-bit,
//!   same ranks, same cohort sizes, same missingness. This is what proves the
//!   `by_last_seen` index, the range-query expiry and the first-key eviction
//!   lookup are neutral — a configuration flag could not have proved any of
//!   them, because it would leave the same code running on both sides.
//!
//! * **Above the old bound**, output legitimately differs, and the point is to
//!   show *how*. The repaired engine preserves opportunities the deployed one
//!   evicted at 3,750. Those extra opportunities then sit in later ranking
//!   cohorts, so cohort sizes grow and cross-sectional ranks move. That is not
//!   a model change and must not be reported as one: for every opportunity
//!   present in **both** runs, the model inputs and the model outputs are
//!   identical, and only its position relative to a now-larger cohort differs.

mod frozen;

use std::collections::{BTreeMap, BTreeSet};

use backtest_metrics::opportunity::{
    OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use market_data::events::ConsolidationStrategy;
use market_data::{ConsolidationEventKind, IgnitionEventKind, ScanEvent};

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
}

// --- fixture ----------------------------------------------------------------

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    }
}

fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughRejected,
    }
}

fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
    ScanEvent::BarUpdate {
        symbol: symbol.into(),
        timestamp: t,
        interval_secs: 60,
        open: close * 0.995,
        high: close * 1.01,
        low: close * 0.99,
        close,
        volume: 1_000,
        is_final: true,
    }
}

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.5,
        structure: 0.45,
        ma_slope: 0.6,
        wick_rejection: 0.7,
        overall,
        qualifies: overall >= 0.6,
    }
}

fn micro(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::ConsolidationEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: ConsolidationEventKind::EntryTriggered,
        strategy: ConsolidationStrategy::Micropullback,
    }
}

/// A deterministic mixed stream across `symbols` distinct symbols and `secs`
/// seconds of synthetic time.
///
/// Mixed on purpose: confirmations open opportunities, rejections exercise
/// absorption (the single deliberate divergence from `EpisodeTracker`), bars
/// move the price path so regimes actually differ between symbols, momentum
/// drives the early-quality inputs, and micropullbacks add a second opening
/// detector so detector confluence is non-trivial.
fn fixture(symbols: i64, secs: i64, per_sec: i64) -> Vec<(ScanEvent, DateTime<Utc>)> {
    let mut out = Vec::new();
    for i in 0..(secs * per_sec) {
        let t = at(i / per_sec);
        let symbol = format!("S{:05}", i % symbols);
        let price = 10.0 + (i % 97) as f64 * 0.05 + (i as f64 * 0.001);
        // Keyed on the *round*, so symbol `s` receives confirmed, then momentum,
        // then a bar, then a rejection, then a micropullback, in that order.
        //
        // Keying on `i % 5` instead silently degenerates whenever `symbols` is a
        // multiple of 5: for a fixed symbol `i` differs by multiples of
        // `symbols`, so `i % 5` is constant and every symbol receives one event
        // kind forever. That fixture gave no opportunity any momentum, so every
        // early-quality score was unavailable and the proof was comparing two
        // columns of `null` — passing, and proving nothing.
        //
        // Round 0 being a confirmation for every symbol also makes the open
        // population exactly `symbols`, which is what lets these tests sit
        // deliberately either side of the 3,750 bound.
        let event = match (i / symbols) % 5 {
            0 => confirmed(&symbol, t, price),
            1 => momentum(&symbol, t, 0.30 + (i % 7) as f64 * 0.11),
            2 => bar(&symbol, t, price * 1.004),
            3 => rejected(&symbol, t, price),
            _ => micro(&symbol, t, price),
        };
        out.push((event, t));
    }
    out
}

// --- drivers ----------------------------------------------------------------

/// Runs the repaired engine, returning every snapshot as its serialized form.
fn run_repaired(config: OiConfig, events: &[(ScanEvent, DateTime<Utc>)]) -> (Vec<String>, u64) {
    let mut engine = OpportunityIntelligence::new(config);
    let mut out = Vec::new();
    for (event, received_at) in events {
        engine.observe(event, *received_at);
        if let Some(rows) = engine.rank(*received_at) {
            for row in rows {
                out.push(serde_json::to_string(&row).unwrap());
            }
        }
    }
    let evictions = engine.health().capacity_evictions;
    (out, evictions)
}

/// Runs the engine as deployed, identically driven.
fn run_frozen(events: &[(ScanEvent, DateTime<Utc>)]) -> (Vec<String>, u64) {
    let mut engine = frozen::OpportunityIntelligence::new(frozen::OiConfig::default());
    let mut out = Vec::new();
    for (event, received_at) in events {
        engine.observe(event, *received_at);
        if let Some(rows) = engine.rank(*received_at) {
            for row in rows {
                out.push(serde_json::to_string(&row).unwrap());
            }
        }
    }
    let evictions = engine.health().capacity_evictions;
    (out, evictions)
}

/// The repaired engine configured to the deployed bound of 3,750.
///
/// `supported_lifetime_secs: 300` reproduces the old formula's multiplier and
/// `supported_symbol_universe: usize::MAX` takes the structural bound out of
/// play, so `min(10.00/s x 300s, MAX) x 5/4 = 3,750` exactly.
fn deployed_bound_config() -> OiConfig {
    let config = OiConfig {
        supported_lifetime_secs: 300,
        supported_symbol_universe: usize::MAX,
        ..OiConfig::default()
    };
    assert_eq!(config.max_open_opportunities(), 3_750, "the deployed bound, reconstructed");
    config
}

fn parse(rows: &[String]) -> Vec<OpportunityScoreSnapshot> {
    rows.iter().map(|r| serde_json::from_str(r).unwrap()).collect()
}

/// Replaces the OI config fingerprint with a fixed placeholder.
///
/// Every snapshot carries `versions.configFingerprint`, and capacity is a
/// configuration value, so a capacity change necessarily changes it — on every
/// record in the capture. Literal byte-identity is therefore impossible after
/// this repair, and demanding it would only prove that the fingerprint does its
/// job.
///
/// Normalising it out, by a targeted substitution rather than a re-serialize,
/// leaves every other byte in its original position and order. The fingerprint
/// change itself is asserted separately and deliberately in
/// `model_and_ranking_versions_are_unchanged_and_only_the_fingerprint_moves`.
fn normalize_fingerprint(row: &str) -> String {
    const KEY: &str = "\"configFingerprint\":\"";
    let Some(start) = row.find(KEY) else { return row.to_string() };
    let value_start = start + KEY.len();
    let Some(len) = row[value_start..].find('"') else { return row.to_string() };
    let mut out = String::with_capacity(row.len());
    out.push_str(&row[..value_start]);
    out.push_str("oi-cfg-NORMALIZED");
    out.push_str(&row[value_start + len..]);
    out
}

/// The V2.1 correctness repair deliberately changed the record's IDENTITY and
/// SHAPE, and nothing about the model:
///
///   * `sequence` is milliseconds-since-UTC-midnight of `opened_at`, not a
///     per-process ordinal, so `opportunityId` reads differently;
///   * top-level `schemaVersion` is 2, not 1;
///   * six causal risk/identity fields were added
///     (`observedHigh`, `observedLow`, `maxMovePct`, `minMovePct`,
///     `openingPrice`, `openedAt`).
///
/// Normalizing exactly those keeps this proof aimed at what it exists for:
/// that no score, rank, regime, cohort size, timestamp, feature snapshot or
/// missingness moved. Same treatment `normalize_fingerprint` already gives the
/// configuration fingerprint, and each change is asserted deliberately in
/// `the_v2_1_repair_changed_only_identity_and_shape`.
///
/// Both sides are routed through `serde_json::Value`, whose map is a
/// `BTreeMap`, so key order is identical on both sides by construction and the
/// comparison stays byte-for-byte after normalization.
fn normalize_v21_identity(row: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(row) else {
        return row.to_string();
    };
    if let Some(obj) = v.as_object_mut() {
        // Added fields: absent on the deployed side, present here.
        for field in [
            "observedHigh",
            "observedLow",
            "maxMovePct",
            "minMovePct",
            "openingPrice",
            "openedAt",
        ] {
            obj.remove(field);
        }
        // Top-level schema version only -- nested context versions are
        // untouched by the repair and must still be compared.
        if obj.contains_key("schemaVersion") {
            obj.insert("schemaVersion".into(), serde_json::json!("NORMALIZED"));
        }
        // `symbol:date:sequence` -> `symbol:date:SEQ`
        if let Some(id) = obj.get("opportunityId").and_then(|x| x.as_str()) {
            let parts: Vec<&str> = id.split(':').collect();
            if parts.len() == 3 {
                let masked = format!("{}:{}:SEQ", parts[0], parts[1]);
                obj.insert("opportunityId".into(), serde_json::json!(masked));
            }
        }
        // The serialized `Opportunity` (closing proof) carries a nested id.
        if let Some(id) = obj.get_mut("id").and_then(|x| x.as_object_mut()) {
            if id.contains_key("sequence") {
                id.insert("sequence".into(), serde_json::json!("SEQ"));
            }
        }
        // `versions.opportunitySchema` restates the same deliberate bump, and
        // is asserted on its own in the versions test.
        if let Some(ver) = obj.get_mut("versions").and_then(|x| x.as_object_mut()) {
            if ver.contains_key("opportunitySchema") {
                ver.insert("opportunitySchema".into(), serde_json::json!("NORMALIZED"));
            }
        }
    }
    serde_json::to_string(&v).unwrap()
}

fn normalized(rows: &[String]) -> Vec<String> {
    rows.iter().map(|r| normalize_v21_identity(&normalize_fingerprint(r))).collect()
}

// ---------------------------------------------------------------------------
// Regime 1 -- below the old bound: byte-identical, no exceptions
// ---------------------------------------------------------------------------

/// Below the deployed bound, the repaired engine's output is byte-identical to
/// the deployed engine's.
///
/// This is the load-bearing assertion of the whole repair. It is made against
/// the serialized record, so every field is compared at once — scores, ranks,
/// regimes, timestamps, identity, feature snapshots and missingness — and a
/// field added, removed or reordered would fail it just as surely as a score
/// that moved.
#[test]
fn below_the_old_bound_output_is_byte_identical() {
    // 600 symbols against a bound of 3,750: capacity cannot be what differs.
    let events = fixture(600, 400, 8);
    let (frozen_rows, frozen_evictions) = run_frozen(&events);
    let (repaired_rows, repaired_evictions) = run_repaired(OiConfig::default(), &events);

    assert_eq!(frozen_evictions, 0, "the fixture must stay inside the deployed bound");
    assert_eq!(repaired_evictions, 0);
    assert!(
        frozen_rows.len() > 2_000,
        "the fixture must produce real volume, got {}",
        frozen_rows.len()
    );
    println!("below-bound proof: {} records compared", frozen_rows.len());

    assert_eq!(
        frozen_rows.len(),
        repaired_rows.len(),
        "the repair must not change how many records are produced"
    );
    let frozen_rows = normalized(&frozen_rows);
    let repaired_rows = normalized(&repaired_rows);
    // Compared row by row so a failure names the first divergence rather than
    // printing two multi-megabyte blobs.
    for (i, (a, b)) in frozen_rows.iter().zip(&repaired_rows).enumerate() {
        assert_eq!(
            a, b,
            "record {i} differs between the deployed engine and the repaired engine.\n\
             deployed: {a}\nrepaired: {b}"
        );
    }
}

/// The same, with the configuration pinned to the deployed bound.
///
/// Separates two claims that would otherwise be entangled: that the *code*
/// changes are neutral, and that the *bound* change is what differs above it.
#[test]
fn at_the_deployed_bound_output_is_byte_identical() {
    let events = fixture(600, 400, 8);
    let (frozen_rows, _) = run_frozen(&events);
    let (repaired_rows, _) = run_repaired(deployed_bound_config(), &events);
    assert_eq!(normalized(&frozen_rows), normalized(&repaired_rows));
}

/// Model versions, the score policy and the ranking version are untouched.
///
/// The config fingerprint is the one field that legitimately moves: capacity is
/// a configuration value, so a capacity change is a configuration change, and a
/// fingerprint that did not move would be lying about which configuration
/// produced the records.
#[test]
fn model_and_ranking_versions_are_unchanged_and_only_the_fingerprint_moves() {
    let frozen = frozen::OiConfig::default().versions();
    let repaired = OiConfig::default().versions();

    assert_eq!(repaired.early_quality_model, frozen.early_quality_model);
    assert_eq!(repaired.continuation_model, frozen.continuation_model);
    assert_eq!(repaired.ranking, frozen.ranking);
    assert_eq!(repaired.score_policy, frozen.score_policy);
    assert_eq!(repaired.regime_classifier, frozen.regime_classifier);
    assert_eq!(repaired.price_regime, frozen.price_regime);
    // Deliberately moved by the V2.1 repair: `sequence` changed MEANING and six
    // causal fields were added, so an artifact must be able to say which shape
    // it is. Asserted here rather than silently tolerated.
    assert_eq!(frozen.opportunity_schema, 1, "the deployed engine wrote schema 1");
    assert_eq!(repaired.opportunity_schema, 2, "the repair writes schema 2");
    assert_eq!(repaired.feature_schema, frozen.feature_schema);

    assert_ne!(
        repaired.config_fingerprint, frozen.config_fingerprint,
        "a capacity change is a configuration change and must be visible as one"
    );
    println!("deployed fingerprint : {}", frozen.config_fingerprint);
    println!("repaired fingerprint : {}", repaired.config_fingerprint);
}

/// The inactivity boundary, the ranking cadence and the cohort bound are
/// untouched. Section 2 puts all three out of scope.
#[test]
fn out_of_scope_configuration_is_untouched() {
    let frozen = frozen::OiConfig::default();
    let repaired = OiConfig::default();
    assert_eq!(repaired.inactivity_secs, frozen.inactivity_secs);
    assert_eq!(repaired.inactivity_secs, 300);
    assert_eq!(repaired.ranking_cadence_secs, frozen.ranking_cadence_secs);
    assert_eq!(repaired.ranking_cadence_secs, 30);
    assert_eq!(repaired.max_rank_cohort, frozen.max_rank_cohort);
    assert_eq!(repaired.max_history_per_opportunity, frozen.max_history_per_opportunity);
    assert_eq!(repaired.early_max_prior_move_pct, frozen.early_max_prior_move_pct);
    assert_eq!(repaired.continuation_min_prior_move_pct, frozen.continuation_min_prior_move_pct);
    assert_eq!(repaired.reversal_max_prior_move_pct, frozen.reversal_max_prior_move_pct);
    assert_eq!(repaired.price_regime_bounds, frozen.price_regime_bounds);
    assert_eq!(repaired.supported_open_rate_centi, frozen.supported_open_rate_centi);
    assert_eq!(repaired.bound_safety_num, frozen.bound_safety_num);
    assert_eq!(repaired.bound_safety_den, frozen.bound_safety_den);
}

// ---------------------------------------------------------------------------
// Regime 2 -- above the old bound: differences are preservation, not model drift
// ---------------------------------------------------------------------------

/// Above the deployed bound the two engines legitimately diverge, and this
/// characterises the divergence precisely.
///
/// For every opportunity ranked in **both** runs, every model input and every
/// model output is identical. What differs is cohort size, and therefore
/// cross-sectional rank — because the repaired engine still holds
/// opportunities the deployed one had already evicted.
#[test]
fn above_the_old_bound_only_preservation_differs_never_the_model() {
    // 5,000 symbols, all opened in round 0: comfortably past the deployed bound
    // of 3,750 and comfortably inside the repaired 16,375. Compressed into 120s
    // so the run crosses only a handful of ranking windows -- every window emits
    // the whole cohort, and at this population a longer run would serialize
    // hundreds of megabytes of snapshots to prove nothing extra.
    let events = fixture(5_000, 120, 150);
    let (frozen_rows, frozen_evictions) = run_frozen(&events);
    let (repaired_rows, repaired_evictions) = run_repaired(OiConfig::default(), &events);

    assert!(
        frozen_evictions > 0,
        "the fixture must actually push the deployed engine past its bound, \
         or this test characterises nothing"
    );
    assert_eq!(
        repaired_evictions, 0,
        "and the repaired engine must absorb the same population without evicting"
    );
    println!(
        "deployed evicted {frozen_evictions}; rows {} -> {}",
        frozen_rows.len(),
        repaired_rows.len()
    );
    assert!(
        repaired_rows.len() > frozen_rows.len(),
        "preserving opportunities must produce strictly more ranking rows"
    );

    let frozen_snaps = parse(&frozen_rows);
    let repaired_snaps = parse(&repaired_rows);

    // Key by (window, symbol): the pair identifies one scoring decision. It used
    // to key on `opportunity_id`, but the V2.1 repair made `sequence`
    // time-derived, so the two engines legitimately mint different ids for the
    // same opportunity and an id-keyed join would match nothing at all.
    let mut repaired_by_key: BTreeMap<(String, String), &OpportunityScoreSnapshot> =
        BTreeMap::new();
    for s in &repaired_snaps {
        repaired_by_key.insert((s.window_id.clone(), s.symbol.clone()), s);
    }

    /// The instant an opportunity opened, recovered from the row alone.
    fn opened_at_of(s: &OpportunityScoreSnapshot) -> DateTime<Utc> {
        s.timestamp - Duration::seconds(s.opportunity_age_secs)
    }

    let mut compared = 0usize;
    let mut rank_moved = 0usize;
    let mut cohort_grew = 0usize;
    let mut reopened_by_eviction = 0usize;
    for f in &frozen_snaps {
        let Some(r) = repaired_by_key.get(&(f.window_id.clone(), f.symbol.clone())) else {
            panic!(
                "the repaired engine ranked no row at all for {} in {} -- that                  would be the repair losing a symbol, not re-identifying one",
                f.symbol, f.window_id
            );
        };

        // Same symbol and window, but is it the same OPPORTUNITY?
        //
        // A symbol the deployed engine evicted and later reopened comes back as
        // a different, younger opportunity, while the repaired engine never
        // evicted it and still holds the original. Those two rows describe
        // different things and must not be asserted equal. The fixture holds no
        // inactivity expiry (its 120s span is well inside the 300s boundary),
        // so eviction is the only thing that can reopen a symbol here.
        //
        // This replaces the old "sequence > 1 means re-opened" discriminator,
        // which only worked while `sequence` was a per-open ordinal.
        if opened_at_of(f) != opened_at_of(r) {
            assert!(
                opened_at_of(f) > opened_at_of(r),
                "a re-opened identity must be YOUNGER than the preserved one,                  got {} vs {} for {}",
                opened_at_of(f),
                opened_at_of(r),
                f.symbol
            );
            reopened_by_eviction += 1;
            continue;
        }
        compared += 1;

        // --- model inputs, identical -------------------------------------
        assert_eq!(f.symbol, r.symbol);
        assert_eq!(f.session_date, r.session_date);
        assert_eq!(f.opportunity_age_secs, r.opportunity_age_secs);
        assert_eq!(f.current_price, r.current_price);
        assert_eq!(f.move_from_start_pct, r.move_from_start_pct);
        assert_eq!(f.move_before_detection_pct, r.move_before_detection_pct);
        assert_eq!(f.raw_event_count, r.raw_event_count);
        assert_eq!(f.episode_fragments, r.episode_fragments);
        assert_eq!(f.invalidations_absorbed, r.invalidations_absorbed);
        assert_eq!(f.confirmation_span_secs, r.confirmation_span_secs);
        assert_eq!(
            serde_json::to_string(&f.features).unwrap(),
            serde_json::to_string(&r.features).unwrap(),
            "feature snapshots must be identical for {}",
            f.opportunity_id
        );
        assert_eq!(f.versions.early_quality_model, r.versions.early_quality_model);
        assert_eq!(f.versions.continuation_model, r.versions.continuation_model);
        assert_eq!(f.versions.ranking, r.versions.ranking);
        assert_eq!(f.versions.score_policy, r.versions.score_policy);
        assert_eq!(
            serde_json::to_string(&f.detection_features).unwrap(),
            serde_json::to_string(&r.detection_features).unwrap(),
        );

        // --- detector confluence, identical -------------------------------
        assert_eq!(f.detectors_seen, r.detectors_seen);
        assert_eq!(f.confluence_count, r.confluence_count);

        // --- regimes, identical -------------------------------------------
        assert_eq!(
            serde_json::to_string(&f.regime).unwrap(),
            serde_json::to_string(&r.regime).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&f.price_regime).unwrap(),
            serde_json::to_string(&r.price_regime).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&f.shadow_state).unwrap(),
            serde_json::to_string(&r.shadow_state).unwrap()
        );

        // --- scores, bit-for-bit ------------------------------------------
        assert_eq!(
            serde_json::to_string(&f.early_quality).unwrap(),
            serde_json::to_string(&r.early_quality).unwrap(),
            "EarlyQualityScore must be bit-for-bit identical for {}",
            f.opportunity_id
        );
        assert_eq!(
            serde_json::to_string(&f.continuation).unwrap(),
            serde_json::to_string(&r.continuation).unwrap(),
            "ContinuationConfidence must be bit-for-bit identical for {}",
            f.opportunity_id
        );

        // --- ranks: may move, and only for the documented reason ----------
        if f.early_quality_rank != r.early_quality_rank
            || f.continuation_rank != r.continuation_rank
        {
            rank_moved += 1;
            assert!(
                r.early_cohort_size >= f.early_cohort_size
                    || r.continuation_cohort_size >= f.continuation_cohort_size,
                "a rank moved without the cohort growing, which would be a model change: \
                 {} in {}",
                f.opportunity_id,
                f.window_id
            );
        }
        if r.early_cohort_size > f.early_cohort_size {
            cohort_grew += 1;
        }
        assert!(
            r.early_cohort_size >= f.early_cohort_size,
            "a preserved opportunity can only add to a cohort, never remove from it"
        );
        assert!(r.continuation_cohort_size >= f.continuation_cohort_size);
    }

    assert!(compared > 1_000, "only {compared} rows compared; the fixture is too small");
    assert!(
        cohort_grew > 0,
        "the fixture must actually produce larger cohorts, or it is not exercising preservation"
    );
    assert!(
        reopened_by_eviction > 0,
        "the fixture must produce re-opened identities, or the eviction path is untested"
    );
    println!(
        "compared {compared} shared rows: {cohort_grew} in grown cohorts, \
         {rank_moved} with moved ranks; {reopened_by_eviction} deployed-only rows, \
         every one a re-opened identity"
    );

    // Every repaired-only row belongs to an opportunity the deployed engine had
    // already evicted, and every such opportunity is still on its *first*
    // sequence -- because the repaired engine never evicted it, so it never
    // needed re-opening.
    let frozen_keys: BTreeSet<(String, String)> =
        frozen_snaps.iter().map(|s| (s.window_id.clone(), s.symbol.clone())).collect();
    let extra: Vec<&OpportunityScoreSnapshot> = repaired_snaps
        .iter()
        .filter(|s| !frozen_keys.contains(&(s.window_id.clone(), s.symbol.clone())))
        .collect();
    // Under (window, symbol) keying a repaired row falls into exactly one of
    // three buckets, and they must account for the run with nothing left over:
    //   compared            -- the same opportunity in both engines
    //   reopened_by_eviction -- the deployed engine evicted and reopened the
    //                           symbol, so its row describes a younger
    //                           opportunity than the one preserved here
    //   extra               -- a (window, symbol) the deployed engine stopped
    //                           producing altogether once it evicted
    assert_eq!(
        compared + extra.len() + reopened_by_eviction,
        repaired_snaps.len(),
        "compared + additional + re-identified must account for every repaired row"
    );
    // "Still on its first sequence" was the old way of saying "the repaired
    // engine never evicted and reopened this symbol". With `sequence` now
    // time-derived, say it directly: the repaired run only ever saw ONE opening
    // instant for that symbol.
    let mut repaired_opens: BTreeMap<String, BTreeSet<DateTime<Utc>>> = BTreeMap::new();
    for s in &repaired_snaps {
        repaired_opens.entry(s.symbol.clone()).or_default().insert(opened_at_of(s));
    }
    for s in &extra {
        assert_eq!(
            repaired_opens.get(&s.symbol).map(|o| o.len()).unwrap_or(0),
            1,
            "a preserved opportunity must never have been reopened: {}",
            s.symbol
        );
    }
    println!(
        "{} additional rows, all from opportunities the deployed engine evicted",
        extra.len()
    );

    // And the two accounts reconcile against the engine's own counter: the
    // deployed run evicted `frozen_evictions` opportunities, which is the only
    // mechanism that could have produced its extra identities.
    assert!(
        reopened_by_eviction > 0 && frozen_evictions > 0,
        "the characterisation rests on eviction having happened"
    );
}

/// Opportunity identity and lifecycle timestamps survive the repair.
///
/// Checked separately from the byte comparison because identity is the field
/// most likely to shift for a subtle reason: the sequence counter increments on
/// every open, so an opportunity evicted and later reopened takes a *new* id.
/// Below the bound that must never happen.
#[test]
fn opportunity_identity_and_lifecycle_are_unchanged_below_the_bound() {
    let events = fixture(600, 400, 8);
    let (frozen_rows, _) = run_frozen(&events);
    let (repaired_rows, _) = run_repaired(OiConfig::default(), &events);

    let frozen_snaps = parse(&frozen_rows);
    let repaired_snaps = parse(&repaired_rows);

    // `sequence` deliberately changed meaning in the V2.1 repair, so the literal
    // ids no longer match. What this test is actually for -- that identity does
    // not SHIFT for a subtle reason, e.g. an evicted-then-reopened opportunity
    // silently taking a new id -- is checked two ways instead.
    let masked = |v: &[OpportunityScoreSnapshot]| -> Vec<String> {
        v.iter()
            .map(|s| {
                let p: Vec<&str> = s.opportunity_id.split(':').collect();
                format!("{}:{}", p[0], p[1])
            })
            .collect()
    };
    assert_eq!(
        masked(&frozen_snaps),
        masked(&repaired_snaps),
        "symbol and session date must be identical, row for row"
    );
    // 1:1 correspondence: the same grouping of rows into opportunities, so no
    // opportunity split into two or merged with another.
    let pairs: BTreeSet<(String, String)> = frozen_snaps
        .iter()
        .zip(&repaired_snaps)
        .map(|(f, r)| (f.opportunity_id.clone(), r.opportunity_id.clone()))
        .collect();
    let frozen_distinct: BTreeSet<&String> =
        frozen_snaps.iter().map(|s| &s.opportunity_id).collect();
    let repaired_distinct: BTreeSet<&String> =
        repaired_snaps.iter().map(|s| &s.opportunity_id).collect();
    assert_eq!(
        pairs.len(),
        frozen_distinct.len(),
        "each deployed id must map to exactly one repaired id"
    );
    assert_eq!(
        frozen_distinct.len(),
        repaired_distinct.len(),
        "and the opportunity count must be unchanged"
    );

    let windows = |v: &[OpportunityScoreSnapshot]| -> Vec<String> {
        v.iter().map(|s| s.window_id.clone()).collect()
    };
    assert_eq!(windows(&frozen_snaps), windows(&repaired_snaps), "ranking windows must align");

    // Age is `timestamp - opened_at`, so equal ages at equal timestamps means
    // equal open times without needing the opportunity record itself.
    for (f, r) in frozen_snaps.iter().zip(&repaired_snaps) {
        assert_eq!(f.timestamp, r.timestamp);
        assert_eq!(f.opportunity_age_secs, r.opportunity_age_secs);
    }
}

/// Closing behaviour is unchanged: same reasons, same instants, same counts.
///
/// The expiry rewrite is the change most able to break this. It swapped a
/// `HashMap` scan for a range query over an index ordered by `last_seen_at`,
/// and an off-by-one in the range bound would retire opportunities a window
/// early or late — invisible in the ranking rows until an opportunity vanished
/// from a cohort.
#[test]
fn closing_reasons_and_instants_are_unchanged() {
    // Symbol gap is `symbols / per_sec` = 600s, past the 300s boundary, so this
    // exercises `Inactivity` closes and not only the `CaptureEnded` ones that
    // `finish` produces. A fixture whose gaps stay inside the boundary would
    // never run the expiry path at all -- which is the path the `by_last_seen`
    // range query replaced.
    let events = fixture(3_000, 900, 5);

    let mut frozen_engine = frozen::OpportunityIntelligence::new(frozen::OiConfig::default());
    let mut frozen_closed: Vec<String> = Vec::new();
    for (event, received_at) in &events {
        for op in frozen_engine.observe(event, *received_at) {
            frozen_closed.push(serde_json::to_string(&op).unwrap());
        }
        frozen_engine.rank(*received_at);
    }
    for op in frozen_engine.finish(at(100_000)) {
        frozen_closed.push(serde_json::to_string(&op).unwrap());
    }

    let mut repaired_engine = OpportunityIntelligence::new(OiConfig::default());
    let mut repaired_closed: Vec<String> = Vec::new();
    for (event, received_at) in &events {
        for op in repaired_engine.observe(event, *received_at) {
            repaired_closed.push(serde_json::to_string(&op).unwrap());
        }
        repaired_engine.rank(*received_at);
    }
    for op in repaired_engine.finish(at(100_000)) {
        repaired_closed.push(serde_json::to_string(&op).unwrap());
    }

    let frozen_closed = normalized(&frozen_closed);
    let repaired_closed = normalized(&repaired_closed);
    assert!(frozen_closed.len() > 100, "the fixture must actually close opportunities");
    let inactivity = frozen_closed.iter().filter(|r| r.contains("\"inactivity\"")).count();
    assert!(
        inactivity > 0,
        "the fixture must exercise inactivity expiry, not only capture end \
         (that is the path the range query replaced)"
    );
    println!("closing proof: {} closed, {inactivity} by inactivity", frozen_closed.len());
    assert_eq!(
        frozen_closed.len(),
        repaired_closed.len(),
        "the same number of opportunities must close"
    );

    // Order is deliberately not compared here. The deployed engine collected
    // expiring opportunities by iterating a `HashMap`, whose order varies per
    // process, so it never had a defined order to preserve — the repaired
    // engine's is deterministic, which is strictly better. What must match is
    // the *set*, and it must match exactly.
    let as_set = |v: Vec<String>| -> BTreeSet<String> { v.into_iter().collect() };
    let frozen_set = as_set(frozen_closed);
    let repaired_set = as_set(repaired_closed);
    let only_frozen: Vec<&String> = frozen_set.difference(&repaired_set).collect();
    let only_repaired: Vec<&String> = repaired_set.difference(&frozen_set).collect();
    assert!(
        only_frozen.is_empty() && only_repaired.is_empty(),
        "closed opportunities differ.\nonly deployed: {:?}\nonly repaired: {:?}",
        only_frozen.first(),
        only_repaired.first()
    );
}

/// The repaired engine's expiry is deterministic across runs; the deployed
/// engine's was not.
///
/// Not a requirement of the brief, and recorded because it is a real change in
/// observable behaviour — just one in the direction of reproducibility.
#[test]
fn expiry_order_is_now_deterministic() {
    // Symbol gap is `symbols / per_sec` = 800s, well past the 300s inactivity
    // boundary, so opportunities genuinely expire rather than merely surviving
    // to `finish`.
    let events = fixture(4_000, 900, 5);
    let run = || -> Vec<String> {
        let mut engine = OpportunityIntelligence::new(OiConfig::default());
        let mut closed = Vec::new();
        for (event, received_at) in &events {
            for op in engine.observe(event, *received_at) {
                closed.push(format!("{}@{:?}", op.symbol, op.closed_at));
            }
        }
        closed
    };
    let first = run();
    assert!(first.len() > 50, "the fixture must close opportunities");
    for _ in 0..4 {
        assert_eq!(run(), first, "expiry order must be reproducible");
    }
}

/// A sanity check on the fixture itself.
///
/// Every assertion above is worthless if the fixture does not exercise the
/// things it claims to: real scores, real ranks, real regimes, real absences.
#[test]
fn the_fixture_exercises_what_the_proof_claims() {
    let events = fixture(600, 400, 8);
    let (rows, _) = run_repaired(OiConfig::default(), &events);
    let snaps = parse(&rows);

    assert!(snaps.iter().any(|s| s.early_quality.value.is_some()), "real early-quality scores");
    assert!(snaps.iter().any(|s| s.continuation.value.is_some()), "real continuation scores");
    assert!(snaps.iter().any(|s| s.early_quality_rank.is_some()), "real ranks");
    assert!(snaps.iter().any(|s| !s.early_quality.missing.is_empty()), "real missingness");
    assert!(snaps.iter().any(|s| s.invalidations_absorbed > 0), "absorption exercised");
    assert!(snaps.iter().any(|s| s.confluence_count > 1), "detector confluence exercised");
    assert!(snaps.iter().any(|s| s.detection_features.is_some()), "detection surface emitted");
    assert!(
        snaps.iter().map(|s| &s.window_id).collect::<BTreeSet<_>>().len() > 10,
        "several ranking windows"
    );
    let _ = Duration::seconds(1);
}
