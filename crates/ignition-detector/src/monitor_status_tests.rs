//! P4.1 halt-status correctness: the required transition matrix, end to end
//! from `(tape, status code)` through `classify_status` and
//! `IgnitionMonitor::on_status` to the halt-lift candidate on the next trade.
//!
//! Static-metadata semantics only; no outcome or effectiveness assertion.

use crate::monitor::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
use crate::tick::Trade;
use crate::trading_status::classify_status;

use StatusTransition::{Halted, Resumed, Unchanged};

fn monitor() -> IgnitionMonitor {
    // flat_base off, so the halt-lift is the only thing under test.
    IgnitionMonitor::new(MonitorConfig { flat_base: None, ..MonitorConfig::default() })
}

fn trade(t: f64) -> Trade {
    Trade { timestamp_secs: t, price: 5.0, size: 100 }
}

/// Feeds `(tape, code)` statuses; returns the transitions.
fn feed(m: &mut IgnitionMonitor, tape: Option<&str>, codes: &[&str]) -> Vec<StatusTransition> {
    codes.iter().map(|c| m.on_status(classify_status(tape, c))).collect()
}

/// Whether the next trade opens a halt-lift candidate.
fn lifts(m: &mut IgnitionMonitor, t: f64) -> bool {
    matches!(m.on_trade(trade(t)), MonitorEvent::CandidateOpened(s) if s.halt_lift)
}

// ---------------------------------------------------------------------------
// CTA (tapes A, B)
// ---------------------------------------------------------------------------

#[test]
fn m01_cta_2_halts() {
    for tape in ["A", "B"] {
        let mut m = monitor();
        assert_eq!(feed(&mut m, Some(tape), &["2"]), vec![Halted], "tape {tape}");
        assert!(m.is_interrupted());
        assert!(!lifts(&mut m, 1.0), "no lift while halted");
    }
}

#[test]
fn m02_cta_luld_pause_2_reason_m_interrupts() {
    // The reason code (M) does not change the class: `2` is a halt; an LULD
    // pause on CTA is disseminated as `2` with reason M.
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("A"), &["2"]), vec![Halted]);
    assert!(m.is_interrupted());
}

#[test]
fn m03_cta_indication_during_a_halt_keeps_it_halted() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("A"), &["2", "5", "6"]), vec![Halted, Unchanged, Unchanged]);
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m04_cta_imbalance_during_a_halt_keeps_it_halted() {
    let mut m = monitor();
    let t = feed(&mut m, Some("B"), &["2", "7", "8", "9", "A", "C", "D"]);
    assert_eq!(t[0], Halted);
    assert!(t[1..].iter().all(|x| *x == Unchanged), "{t:?}");
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m05_cta_luld_band_info_and_ssr_during_a_halt_keep_it_halted() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("A"), &["2", "F", "E"]), vec![Halted, Unchanged, Unchanged]);
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m06_cta_2_then_3_resumes_with_exactly_one_lift() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("A"), &["2", "5", "3"]), vec![Halted, Unchanged, Resumed]);
    assert!(!m.is_interrupted());
    assert!(lifts(&mut m, 1.0), "first post-resume print opens the halt-lift");
    // The lift is consumed: later prints do not reopen one from the status.
    assert!(!lifts(&mut m, 2.0));
}

// ---------------------------------------------------------------------------
// UTP (tapes C, O)
// ---------------------------------------------------------------------------

#[test]
fn m07_utp_h_halts() {
    for tape in ["C", "O"] {
        let mut m = monitor();
        assert_eq!(feed(&mut m, Some(tape), &["H"]), vec![Halted], "tape {tape}");
        assert!(m.is_interrupted());
    }
}

#[test]
fn m08_utp_quote_only_during_a_halt_keeps_it_interrupted() {
    // UTP defines no informational status codes; its only non-halt,
    // non-resume code is Q (quotation-only), which must keep the halt.
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["H", "Q"]), vec![Halted, Unchanged]);
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0), "no lift during the quotation-only period");
    // A CTA informational code arriving on a UTP tape is Unknown: no effect.
    assert_eq!(feed(&mut m, Some("C"), &["5"]), vec![Unchanged]);
    assert!(m.is_interrupted());
}

#[test]
fn m09_utp_p_pauses() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["P"]), vec![Halted]);
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m10_utp_p_then_q_stays_interrupted() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["P", "Q"]), vec![Halted, Unchanged]);
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m11_utp_p_q_t_resumes_with_exactly_one_lift() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["P", "Q", "T"]), vec![Halted, Unchanged, Resumed]);
    assert!(lifts(&mut m, 1.0));
}

#[test]
fn m12_utp_h_then_t_resumes_with_exactly_one_lift() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["H", "T"]), vec![Halted, Resumed]);
    assert!(lifts(&mut m, 1.0));
}

// ---------------------------------------------------------------------------
// UNKNOWN
// ---------------------------------------------------------------------------

#[test]
fn m13_unknown_while_normal_changes_nothing() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["T"]), vec![Unchanged]); // normal
    assert_eq!(feed(&mut m, Some("C"), &["Z9"]), vec![Unchanged]);
    assert!(!m.is_interrupted(), "an unknown code does not invent a halt");
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m14_unknown_while_halted_keeps_it_halted() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("A"), &["2", "Z9", "X"]), vec![Halted, Unchanged, Unchanged]);
    assert!(m.is_interrupted());
    assert!(!lifts(&mut m, 1.0));
}

#[test]
fn m15_unknown_never_fabricates_a_resume_or_a_lift() {
    for (tape, halt) in [("A", "2"), ("C", "H"), ("C", "P")] {
        let mut m = monitor();
        feed(&mut m, Some(tape), &[halt]);
        // Unlisted codes, other-plan codes, empty codes: none resumes.
        let t = feed(&mut m, Some(tape), &["Z9", "", if tape == "A" { "T" } else { "3" }]);
        assert!(t.iter().all(|x| *x == Unchanged), "{tape}: {t:?}");
        assert!(m.is_interrupted());
        for s in 0..5 {
            assert!(!lifts(&mut m, f64::from(s)), "{tape}: no lift from an unknown status");
        }
    }
}

// ---------------------------------------------------------------------------
// Duplicates, repeats, tape mismatch, missing tape
// ---------------------------------------------------------------------------

#[test]
fn duplicate_halts_and_resumes_transition_once() {
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["H", "H", "P", "Q", "T", "T"]), vec![Halted, Unchanged, Unchanged, Unchanged, Resumed, Unchanged]);
    assert!(lifts(&mut m, 1.0));
    // A resume with nothing before it (first-ever status) is not a lift.
    let mut fresh = monitor();
    assert_eq!(feed(&mut fresh, Some("A"), &["3", "3"]), vec![Unchanged, Unchanged]);
    assert!(!lifts(&mut fresh, 1.0));
}

#[test]
fn repeated_informational_messages_never_move_the_state() {
    let mut normal = monitor();
    let t = feed(&mut normal, Some("A"), &["7", "7", "8", "F", "E", "5", "6", "C", "D"]);
    assert!(t.iter().all(|x| *x == Unchanged));
    assert!(!normal.is_interrupted());
    assert!(!lifts(&mut normal, 1.0), "informational statuses never open a halt-lift");
    let mut halted = monitor();
    feed(&mut halted, Some("B"), &["2"]);
    feed(&mut halted, Some("B"), &["7", "7", "F", "F", "5"]);
    assert!(halted.is_interrupted());
}

#[test]
fn a_code_on_the_wrong_tape_is_unknown() {
    let mut m = monitor();
    // `H`/`T` on a CTA tape, `2`/`3` on a UTP tape: foreign code spaces.
    assert_eq!(feed(&mut m, Some("A"), &["H"]), vec![Unchanged]);
    assert!(!m.is_interrupted());
    assert_eq!(feed(&mut m, Some("C"), &["2"]), vec![Unchanged]);
    assert!(!m.is_interrupted());
    feed(&mut m, Some("C"), &["H"]);
    assert_eq!(feed(&mut m, Some("C"), &["3"]), vec![Unchanged], "a CTA resume cannot end a UTP halt");
    assert!(m.is_interrupted());
}

#[test]
fn a_missing_tape_uses_the_unambiguous_code_space() {
    // Today's tape-less `H` handling is preserved; CTA codes now work too.
    let mut m = monitor();
    assert_eq!(feed(&mut m, None, &["H", "Q", "T"]), vec![Halted, Unchanged, Resumed]);
    assert!(lifts(&mut m, 1.0));
    let mut c = monitor();
    assert_eq!(feed(&mut c, None, &["2", "7", "3"]), vec![Halted, Unchanged, Resumed]);
    assert!(lifts(&mut c, 1.0));
    let mut u = monitor();
    assert_eq!(feed(&mut u, None, &["2", "??"]), vec![Halted, Unchanged]);
    assert!(u.is_interrupted());
}

#[test]
fn a_re_halt_before_the_first_print_cancels_the_armed_lift() {
    // Halt, resume, halt again with no print in between: a print arriving
    // during the second halt (e.g. a late-reported trade) must not open a
    // halt-lift; the second resume re-arms it.
    let mut m = monitor();
    feed(&mut m, Some("C"), &["H", "T", "H"]);
    assert!(!lifts(&mut m, 1.0), "no lift from a print during an interruption");
    feed(&mut m, Some("C"), &["T"]);
    assert!(lifts(&mut m, 2.0));
}

#[test]
fn the_original_defects_are_gone() {
    // Before P4.1 every one of these was wrong.
    // 1. CTA halts were invisible.
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("A"), &["2", "3"]), vec![Halted, Resumed]);
    // 2. UTP LULD pauses were invisible (P -> T gave no lift).
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["P", "T"]), vec![Halted, Resumed]);
    // 3. Q after H fired `Resumed` during the quotation-only period.
    let mut m = monitor();
    assert_eq!(feed(&mut m, Some("C"), &["H", "Q"]), vec![Halted, Unchanged]);
}
