//! §19 group L (tests 66-72). Each pins one way the attribution could lie.

use super::*;

fn ev(symbol: &str) -> SymbolEvidence {
    SymbolEvidence::new(symbol, "2026-09-14")
}

/// l66. A symbol evidenced at every stage attributes to the deepest one.
#[test]
fn l66_attribution_is_the_deepest_stage_actually_evidenced() {
    let mut e = ev("AAA");
    e.observe(Stage::Visible, true)
        .observe(Stage::SelectionInput, true)
        .observe(Stage::Qualified, true)
        .observe(Stage::DetectorProduced, true)
        .observe(Stage::OpportunityRanked, true);
    e.detector_events.insert("IgnitionDetector".into(), 12);
    e.ranking_windows = 4;

    let a = attribute(&e);
    assert_eq!(a.attribution, Attribution::Reached { stage: Stage::OpportunityRanked });
    // The evidence travels with the verdict, so it stays checkable.
    assert_eq!(a.stages["visible"], Some(true));
    assert_eq!(a.detector_events["IgnitionDetector"], 12);
    assert_eq!(a.ranking_windows, 4);
}

/// l67. `nosnap` is an observation, not an absence.
///
/// This is the distinction the whole module exists for: a symbol the scan
/// asked about and got nothing back for is evidence about *data*, whereas a
/// symbol with no rows at all is evidence about *nothing*. Reporting both as
/// "not found" would let a data outage read as a market fact.
#[test]
fn l67_no_snapshot_is_distinct_from_no_evidence() {
    let mut requested = ev("AAA");
    requested.observe(Stage::Visible, false);
    requested.nosnap_events = 7;
    assert_eq!(attribute(&requested).attribution, Attribution::NotVisible);

    let silent = ev("BBB");
    assert_eq!(
        attribute(&silent).attribution,
        Attribution::Unknown { reason: UnknownReason::NoEvidence },
        "a symbol with no rows at all must not be reported as not-visible"
    );

    // Observed absent, but with no nosnap row to justify it: still unknown.
    let mut unjustified = ev("CCC");
    unjustified.observe(Stage::Visible, false);
    assert_eq!(
        attribute(&unjustified).attribution,
        Attribution::Unknown { reason: UnknownReason::VisibilityUnknown }
    );
}

/// l68. Missing evidence never becomes a negative finding.
#[test]
fn l68_unknown_never_degrades_to_observed_absent() {
    // Visible, nothing known after that.
    let mut partial = ev("AAA");
    partial.observe(Stage::Visible, true);
    let a = attribute(&partial);
    assert_eq!(a.attribution, Attribution::Reached { stage: Stage::Visible });
    for stage in ["selectionInput", "qualified", "quietSelected", "detectorProduced"] {
        assert_eq!(a.stages[stage], None, "{stage} must stay unknown, not false");
    }

    // Visible, and every later stage positively observed absent. A different
    // claim, and the only one of the two that supports "the funnel rejected it".
    let mut definite = ev("BBB");
    definite.observe(Stage::Visible, true);
    for stage in Stage::ALL.iter().copied().filter(|s| s.depth() > 0) {
        definite.observe(stage, false);
    }
    assert_eq!(attribute(&definite).attribution, Attribution::StoppedAtVisibility);
}

/// l69. A deeper stage resting on an explicitly-absent shallower one is
/// reported as a contradiction, not silently promoted.
#[test]
fn l69_a_contradictory_chain_is_reported_not_resolved() {
    let mut e = ev("AAA");
    e.observe(Stage::Visible, false);
    e.observe(Stage::DetectorProduced, true);
    e.nosnap_events = 3;

    let a = attribute(&e);
    assert_eq!(
        a.attribution,
        Attribution::Unknown {
            reason: UnknownReason::Inconsistent {
                shallower: Stage::Visible,
                deeper: Stage::DetectorProduced,
            }
        },
        "detector output from an invisible symbol means the capture is incomplete"
    );
    // And the underlying evidence is not rewritten to make the verdict tidy.
    assert_eq!(a.stages["detectorProduced"], Some(true));
    assert_eq!(a.stages["visible"], Some(false));
}

/// l70. Qualified and quiet-selected are parallel, so one being absent does
/// not contradict the other being present.
#[test]
fn l70_the_two_selection_paths_do_not_contradict_each_other() {
    let mut quiet = ev("AAA");
    quiet
        .observe(Stage::Visible, true)
        .observe(Stage::SelectionInput, true)
        .observe(Stage::Qualified, false)
        .observe(Stage::QuietSelected, true);

    assert_eq!(
        attribute(&quiet).attribution,
        Attribution::Reached { stage: Stage::QuietSelected },
        "arriving via quiet-watch is not a contradiction of the movers path"
    );
    assert_eq!(Stage::Qualified.depth(), Stage::QuietSelected.depth());
}

/// l70b. One parallel path being absent is not a contradiction of a deeper
/// stage — the case a real run caught.
///
/// A symbol that qualified via movers, was not quiet-selected, and then fired
/// a detector is the *ordinary* path through this platform. An earlier version
/// of the rule checked each stage individually and reported it as a capture
/// contradiction.
#[test]
fn l70b_one_absent_parallel_path_does_not_contradict_a_deeper_stage() {
    let mut e = ev("AAA");
    e.observe(Stage::Visible, true)
        .observe(Stage::SelectionInput, true)
        .observe(Stage::Qualified, true)
        .observe(Stage::QuietSelected, false)
        .observe(Stage::DetectorProduced, true);

    assert_eq!(
        attribute(&e).attribution,
        Attribution::Reached { stage: Stage::DetectorProduced },
        "qualifying via one path and not the other is the normal case"
    );

    // But *both* paths observed absent under a deeper stage still contradicts:
    // nothing at that depth selected the symbol, yet a detector ran on it.
    let mut neither = ev("BBB");
    neither
        .observe(Stage::Visible, true)
        .observe(Stage::SelectionInput, true)
        .observe(Stage::Qualified, false)
        .observe(Stage::QuietSelected, false)
        .observe(Stage::DetectorProduced, true);
    assert!(
        matches!(
            attribute(&neither).attribution,
            Attribution::Unknown { reason: UnknownReason::Inconsistent { .. } }
        ),
        "a detector firing on a symbol nothing selected is a capture contradiction"
    );
}

/// l71. A symbol that leaves a selection set it was previously in keeps the
/// positive observation.
///
/// Selection is re-evaluated every scan, so a later "absent" is a normal state
/// change, not a retraction. Letting it overwrite would make attribution
/// depend on which scan happened to be last.
#[test]
fn l71_leaving_a_selection_set_does_not_retract_having_been_in_it() {
    let mut e = ev("AAA");
    e.observe(Stage::Visible, true);
    e.observe(Stage::Qualified, true);
    e.observe(Stage::Qualified, false);
    assert_eq!(e.get(Stage::Qualified), Some(true));
    assert_eq!(attribute(&e).attribution, Attribution::Reached { stage: Stage::Qualified });

    // Order must not matter either.
    let mut reversed = ev("BBB");
    reversed.observe(Stage::Qualified, false);
    reversed.observe(Stage::Qualified, true);
    assert_eq!(reversed.get(Stage::Qualified), Some(true));
}

/// l72. Attribution is deterministic, order-independent and round-trips.
#[test]
fn l72_attribution_is_deterministic_and_serializes_stably() {
    let mut a = ev("BBB");
    a.observe(Stage::Visible, true).observe(Stage::DetectorProduced, true);
    let mut b = ev("AAA");
    b.observe(Stage::Visible, true);
    let mut c = ev("CCC");
    c.nosnap_events = 1;
    c.observe(Stage::Visible, false);

    let first = attribute_all(&[a.clone(), b.clone(), c.clone()]);
    let second = attribute_all(&[c, b, a]);
    assert_eq!(first, second, "input order must not change the output");
    assert_eq!(
        first.iter().map(|x| x.symbol.as_str()).collect::<Vec<_>>(),
        vec!["AAA", "BBB", "CCC"],
        "output is sorted by session then symbol"
    );

    let json = serde_json::to_string(&first[1]).unwrap();
    let parsed: AttributedSymbol = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, first[1]);
    assert!(json.contains(r#""verdict":"reached""#), "verdict is explicit on the wire");

    let cov = coverage(&first);
    assert_eq!(cov.symbols, 3);
    assert_eq!(cov.not_visible, 1);
    assert_eq!(cov.reached.get("DetectorProduced"), Some(&1));
    assert_eq!(cov.reached.get("Visible"), Some(&1));
    assert_eq!(cov.unknown_no_evidence, 0);
}
