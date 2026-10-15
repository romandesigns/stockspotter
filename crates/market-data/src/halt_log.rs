//! Remembers when each symbol was halted and resumed today, so a chart can
//! mark it.
//!
//! The live loop already receives every trading-status message and acts on
//! it (halt-lift is an ignition signal), but nothing kept the times: a
//! client could see "proximity to a halt" (`ScanEvent::HaltWarning`) and
//! never the halt itself. This is the record of what actually happened,
//! straight from the exchange's own status codes.
//!
//! Like `order_flow_store`, it is a small process-wide store read on
//! request rather than a new `ScanEvent` variant, and for the same reason:
//! `ws-server`'s observation layer matches `ScanEvent` exhaustively and is
//! frozen under a preregistration, and a halt mark is only ever wanted for
//! the symbol on screen. One call in the status arm of the live loop feeds
//! it; nothing reads it but `GET /halts/:symbol`.
//!
//! **What this is NOT, deliberately.** The record is partial and says so.
//!
//! - Marks come only from exchange trading-status messages, classified by
//!   `ignition_detector::classify_status` against the real CTA/UTP code
//!   spaces. A halt is never inferred from ordinary trades, from a gap in
//!   prints, or from silence: a quiet symbol and a halted symbol look
//!   identical on the trade tape, and guessing would put a halt marker on
//!   a chart where none happened.
//! - No SIP sequence number is invented or relied on. Alpaca's status
//!   messages do not carry one, so ordering here is by the venue's own
//!   timestamp, which is all the feed provides. Because that is the only
//!   ordering available, it is *checked* rather than trusted: a status
//!   whose timestamp is implausible against the moment its frame was read
//!   (`plausible`, the same asymmetric window as
//!   `order_flow_store::sane_event_time`), or which would land before a
//!   mark already held for the symbol, is rejected and counted. A feed
//!   timestamp can therefore never roll the market day, retro-date the
//!   record, or make `marks` anything other than oldest-first.
//! - The market day is taken from the receipt clock, not from the feed.
//!   It only ever moves forward, and it is rolled at read as well as at
//!   write, so a read after midnight-plus-04:00 never answers with
//!   yesterday's halts just because no status has arrived yet today.
//! - Coverage starts when this process started, not at the session open.
//!   A halt that began and ended before start-up is simply not known, and
//!   a resume with no halt on record marks nothing. `coverage()` reports
//!   the first moment this log could have seen anything, and `complete` is
//!   always false.
//!
//! Kept for the current market day only, using the same 04:00 ET
//! `market_day` calendar as the rest of the crate -- no separate notion of
//! "today" is introduced here.
//!
//! **Off unless `HALT_LOG_ENABLED` is set**, like every other consumer
//! added to the live loop. The status stream covers the whole tape in
//! universe mode, and taking a mutex and allocating a `String` per status
//! message is not a cost the running app should start paying because a
//! chart feature was merged. With it unset `on_status` returns before
//! locking anything and `history` reports an empty record with
//! `sinceSecs: null`, which is the honest answer rather than a misleading
//! "no halts today".

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, NaiveDate, Utc};
use ignition_detector::{classify_status, TradingStatus};

use crate::bar::Status;
use crate::trading_session::market_day;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltMarkKind {
    Halted,
    Resumed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HaltMark {
    /// Unix seconds, from the status message's own timestamp.
    pub at_secs: i64,
    pub kind: HaltMarkKind,
    /// The exchange's status code, for the tooltip (`H`, `P`, `T`, ...).
    pub code: String,
}

/// What a reader gets, with the coverage it has rather than a bare array
/// that would read as "today's halts".
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HaltHistory {
    /// Oldest first.
    pub marks: Vec<HaltMark>,
    /// Unix seconds of the earliest moment this log could have observed a
    /// status for the current market day: process start, or the day's
    /// 04:00 ET open if the process predates it. Null before any status
    /// message has arrived at all.
    pub since_secs: Option<i64>,
    /// Always false. This is what was seen, never a proven list of the
    /// day's halts -- see the module comment.
    pub complete: bool,
    /// How many status messages were classified as a trading interruption
    /// but recorded nothing because they repeated the state already held
    /// (for instance the quotation-only period inside one halt).
    pub repeated_states: u64,
    /// Resumes seen with no halt on record -- the ordinary open, or a
    /// process that started mid-halt. Disclosed rather than guessed at.
    pub unmatched_resumes: u64,
    /// Status messages refused because their own timestamp was not
    /// plausible against the moment the frame was read, or because they
    /// belong to a different market day than the one being recorded.
    /// Nothing about them reached the record.
    pub rejected_timestamps: u64,
    /// Transitions refused because they would have landed before a mark
    /// already held for this symbol. `marks` is oldest-first because these
    /// are dropped, not inserted.
    pub out_of_order: u64,
    /// Marks for THIS symbol discarded because it hit the per-symbol bound
    /// (`MAX_MARKS_PER_SYMBOL`). Non-zero means `marks` no longer begins at
    /// the day's first transition, and `sinceSecs` understates where the
    /// surviving marks start.
    pub dropped_marks: u64,
    /// Symbols that were never admitted at all because the log already held
    /// `MAX_SYMBOLS` of them. Non-zero means some other symbol's record is
    /// missing entirely; which ones is deliberately not guessed at.
    pub dropped_symbols: u64,
}

/// More transitions than any symbol has in a day; a bound, not a target.
const MAX_MARKS_PER_SYMBOL: usize = 64;
/// Symbols tracked at once. Status messages arrive for the whole tape in
/// universe mode, so this is bounded like every other per-symbol map here.
const MAX_SYMBOLS: usize = 4096;
/// A venue timestamp may sit this far ahead of the moment its frame was
/// read: clock skew, nothing more. Deliberately the same asymmetric window
/// `order_flow_store::sane_event_time` uses, and for the same reason -- a
/// tape is behind receipt, never meaningfully ahead of it.
const MAX_AHEAD_OF_RECEIPT_SECS: i64 = 5;
/// And this far behind it, which covers a slow venue or a reconnect
/// replaying recent frames, but not a frame from an earlier session.
const MAX_BEHIND_RECEIPT_SECS: i64 = 900;

/// Whether a status message's own timestamp can be believed, given when
/// this process read the frame carrying it. Checked before anything is
/// recorded and before the day is looked at, so neither an injected feed
/// timestamp nor a garbage one can move the record.
fn plausible(at: DateTime<Utc>, received: DateTime<Utc>) -> bool {
    let ahead = at.timestamp() - received.timestamp();
    ahead <= MAX_AHEAD_OF_RECEIPT_SECS && -ahead <= MAX_BEHIND_RECEIPT_SECS
}

#[derive(Default)]
struct SymbolLog {
    /// Oldest first, enforced rather than assumed.
    marks: Vec<HaltMark>,
    dropped: u64,
}

#[derive(Default)]
struct Log {
    day: Option<NaiveDate>,
    symbols: HashMap<String, SymbolLog>,
    /// Earliest status this log saw for `day`, in epoch seconds.
    since_secs: Option<i64>,
    repeated_states: u64,
    unmatched_resumes: u64,
    rejected_timestamps: u64,
    out_of_order: u64,
    dropped_symbols: u64,
}

impl Log {
    /// Moves to `day` if it is not the day already being recorded, keeping
    /// whichever is later. The day only ever advances: a receipt clock that
    /// went backwards (an NTP step, a stale frame) must not be able to
    /// discard a day's record and start it again.
    fn roll_to(&mut self, day: NaiveDate) {
        match self.day {
            Some(current) if current >= day => return,
            _ => {}
        }
        self.day = Some(day);
        self.symbols.clear();
        self.since_secs = None;
        self.repeated_states = 0;
        self.unmatched_resumes = 0;
        self.rejected_timestamps = 0;
        self.out_of_order = 0;
        self.dropped_symbols = 0;
    }

    fn record(&mut self, symbol: &str, class: TradingStatus, at: DateTime<Utc>, code: &str, received: DateTime<Utc>) {
        // The receipt clock owns the calendar -- same 04:00 ET calendar as
        // the rest of the crate -- and it is rolled before anything is
        // counted, so a reject lands in the right day's tally.
        self.roll_to(market_day(received));
        // Nothing from an unbelievable timestamp reaches the record: not a
        // mark, not `sinceSecs`, not a transition count. A status whose own
        // market day is not the one being recorded is refused for the same
        // reason -- it would be filed under a day it did not happen on.
        if !plausible(at, received) || self.day != Some(market_day(at)) {
            self.rejected_timestamps += 1;
            return;
        }
        // Coverage begins at the first status of the day this process saw,
        // whatever that status was -- including ones that mark nothing.
        self.since_secs = Some(self.since_secs.map_or(at.timestamp(), |s| s.min(at.timestamp())));
        let kind = match class {
            TradingStatus::Halt | TradingStatus::Pause | TradingStatus::NonTradable => HaltMarkKind::Halted,
            TradingStatus::Resume => HaltMarkKind::Resumed,
            // Indications, imbalances, band updates, unknown codes: none
            // of them change whether the symbol is trading.
            _ => return,
        };
        let last = self.symbols.get(symbol).and_then(|log| log.marks.last());
        // The feed's timestamp is the only ordering there is, so a
        // transition that would sit before one already recorded for this
        // symbol is refused rather than inserted or appended out of order.
        // Either way it would be a lie: a chart drawing a resume before its
        // halt, or a record claiming an order the feed did not give.
        if last.is_some_and(|m| m.at_secs > at.timestamp()) {
            self.out_of_order += 1;
            return;
        }
        let last = last.map(|m| m.kind);
        // Transitions only. A halt is usually followed by a quotation-only
        // period before the resume, which is a second "not trading" code
        // for the same interruption; and a resume with no halt on record
        // (the server started mid-halt, or it is the ordinary open) marks
        // nothing a chart could show both ends of.
        match kind {
            HaltMarkKind::Halted if last == Some(HaltMarkKind::Halted) => {
                self.repeated_states += 1;
                return;
            }
            HaltMarkKind::Resumed if last != Some(HaltMarkKind::Halted) => {
                self.unmatched_resumes += 1;
                return;
            }
            _ => {}
        }
        if !self.symbols.contains_key(symbol) && self.symbols.len() >= MAX_SYMBOLS {
            self.dropped_symbols += 1;
            return;
        }
        let log = self.symbols.entry(symbol.to_string()).or_default();
        if log.marks.len() >= MAX_MARKS_PER_SYMBOL {
            log.marks.remove(0);
            // Disclosed in `history`: past this bound the record no longer
            // starts where `sinceSecs` says the coverage does.
            log.dropped = log.dropped.saturating_add(1);
        }
        log.marks.push(HaltMark { at_secs: at.timestamp(), kind, code: code.to_string() });
    }

    /// Rolls the day first: a read is as entitled to notice that the market
    /// day has turned as a status message is, and without this a server
    /// that saw halts yesterday and no status yet today answers with
    /// yesterday's marks.
    fn history(&mut self, symbol: &str, now: DateTime<Utc>) -> HaltHistory {
        self.roll_to(market_day(now));
        let log = self.symbols.get(symbol);
        HaltHistory {
            marks: log.map(|l| l.marks.clone()).unwrap_or_default(),
            since_secs: self.since_secs,
            complete: false,
            repeated_states: self.repeated_states,
            unmatched_resumes: self.unmatched_resumes,
            rejected_timestamps: self.rejected_timestamps,
            out_of_order: self.out_of_order,
            dropped_marks: log.map_or(0, |l| l.dropped),
            dropped_symbols: self.dropped_symbols,
        }
    }
}

fn log() -> &'static Mutex<Log> {
    static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
    LOG.get_or_init(Default::default)
}

pub const HALT_LOG_ENV: &str = "HALT_LOG_ENABLED";

/// Read once: flipping this needs a restart, same as every other
/// behaviour switch in the live loop.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(HALT_LOG_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")))
}

/// Call for every trading-status message received, with the moment the
/// frame carrying it was read off the socket (`live.rs` reads one
/// `Utc::now()` per batch). A no-op, without touching the global log at
/// all, unless `HALT_LOG_ENABLED` is set.
pub fn on_status(status: &Status, received: DateTime<Utc>) {
    if !enabled() {
        return;
    }
    let class = classify_status(status.tape.as_deref(), &status.status_code);
    log().lock().unwrap_or_else(|e| e.into_inner()).record(&status.symbol, class, status.timestamp, &status.status_code, received);
}

/// Today's halts and resumptions for `symbol` as this process saw them,
/// with the coverage that record has. Never a claim about the whole day,
/// and with the log off it is an empty record whose `sinceSecs` is null --
/// "this server kept no halt record", not "this symbol was not halted".
pub fn history(symbol: &str, now: DateTime<Utc>) -> HaltHistory {
    if !enabled() {
        return empty();
    }
    log().lock().unwrap_or_else(|e| e.into_inner()).history(symbol, now)
}

/// What a reader gets from a server that kept no record: nothing, and
/// `sinceSecs: null` to say so.
fn empty() -> HaltHistory {
    HaltHistory {
        marks: Vec::new(),
        since_secs: None,
        complete: false,
        repeated_states: 0,
        unmatched_resumes: 0,
        rejected_timestamps: 0,
        out_of_order: 0,
        dropped_marks: 0,
        dropped_symbols: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // 2026-10-06, regular hours.
    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 6, h, m, 0).unwrap()
    }

    fn kinds(log: &Log, symbol: &str) -> Vec<HaltMarkKind> {
        log.symbols.get(symbol).map(|l| l.marks.iter().map(|m| m.kind).collect()).unwrap_or_default()
    }

    /// A status received at the moment it says it happened, which is the
    /// ordinary case: the receipt guard is about the ones that are not.
    fn record(log: &mut Log, symbol: &str, class: TradingStatus, at: DateTime<Utc>, code: &str) {
        log.record(symbol, class, at, code, at);
    }

    #[test]
    fn a_halt_then_quote_only_then_resume_is_one_halt_and_one_resume() {
        let mut log = Log::default();
        record(&mut log, "AAA", TradingStatus::Pause, at(14, 0), "P");
        record(&mut log, "AAA", TradingStatus::NonTradable, at(14, 4), "Q");
        record(&mut log, "AAA", TradingStatus::Informational, at(14, 4), "I");
        record(&mut log, "AAA", TradingStatus::Resume, at(14, 5), "T");
        assert_eq!(kinds(&log, "AAA"), vec![HaltMarkKind::Halted, HaltMarkKind::Resumed]);
        assert_eq!(log.symbols["AAA"].marks[0], HaltMark { at_secs: at(14, 0).timestamp(), kind: HaltMarkKind::Halted, code: "P".into() });
        // The quotation-only code is disclosed as a repeat, not dropped silently.
        assert_eq!(log.repeated_states, 1);
    }

    #[test]
    fn a_resume_with_no_halt_on_record_marks_nothing_and_is_counted() {
        let mut log = Log::default();
        record(&mut log, "AAA", TradingStatus::Resume, at(13, 30), "T");
        record(&mut log, "AAA", TradingStatus::Unknown, at(13, 31), "?");
        assert!(kinds(&log, "AAA").is_empty());
        assert_eq!(log.unmatched_resumes, 1);
    }

    #[test]
    fn repeated_halts_are_each_kept_and_symbols_are_separate() {
        let mut log = Log::default();
        for minute in [0, 10] {
            record(&mut log, "AAA", TradingStatus::Halt, at(14, minute), "H");
            record(&mut log, "AAA", TradingStatus::Resume, at(14, minute + 5), "T");
        }
        record(&mut log, "BBB", TradingStatus::Halt, at(14, 20), "H");
        assert_eq!(kinds(&log, "AAA").len(), 4);
        assert_eq!(kinds(&log, "BBB"), vec![HaltMarkKind::Halted]);
    }

    #[test]
    fn a_new_market_day_starts_empty_on_the_same_04_et_calendar() {
        let mut log = Log::default();
        record(&mut log, "AAA", TradingStatus::Halt, at(14, 0), "H");
        // 2026-10-07 01:00Z is still 2026-10-06 ET (21:00), so the same
        // market day: the earlier mark survives.
        record(&mut log, "AAA", TradingStatus::Resume, Utc.with_ymd_and_hms(2026, 10, 7, 1, 0, 0).unwrap(), "T");
        assert_eq!(kinds(&log, "AAA").len(), 2);
        // 14:00Z the next day is past 04:00 ET: a new market day.
        record(&mut log, "BBB", TradingStatus::Halt, Utc.with_ymd_and_hms(2026, 10, 7, 14, 0, 0).unwrap(), "H");
        assert!(kinds(&log, "AAA").is_empty());
        assert_eq!(kinds(&log, "BBB"), vec![HaltMarkKind::Halted]);
    }

    #[test]
    fn an_old_day_frame_never_rolls_the_record_backwards() {
        let mut log = Log::default();
        record(&mut log, "AAA", TradingStatus::Halt, at(14, 0), "H");
        // A frame read today whose own timestamp claims yesterday. The
        // record stays on today and the frame is refused outright: rolling
        // backwards here would wipe today's marks, and the next real status
        // would then wipe them again.
        let yesterday = Utc.with_ymd_and_hms(2026, 10, 5, 14, 0, 0).unwrap();
        log.record("AAA", TradingStatus::Resume, yesterday, "T", at(14, 1));
        assert_eq!(log.day, Some(market_day(at(14, 0))));
        assert_eq!(kinds(&log, "AAA"), vec![HaltMarkKind::Halted]);
        assert_eq!(log.rejected_timestamps, 1);
        // And a receipt clock that steps backwards cannot do it either.
        log.record("AAA", TradingStatus::Resume, yesterday, "T", yesterday);
        assert_eq!(log.day, Some(market_day(at(14, 0))));
        assert_eq!(kinds(&log, "AAA"), vec![HaltMarkKind::Halted]);
        assert_eq!(log.rejected_timestamps, 2);
    }

    #[test]
    fn an_implausible_feed_timestamp_reaches_nothing_at_all() {
        let mut log = Log::default();
        let received = at(14, 0);
        // Far-future garbage, and a timestamp older than the receipt window.
        log.record("AAA", TradingStatus::Halt, at(23, 0), "H", received);
        log.record("AAA", TradingStatus::Halt, at(10, 0), "H", received);
        assert!(kinds(&log, "AAA").is_empty());
        assert_eq!(log.rejected_timestamps, 2);
        // Not even coverage: `sinceSecs` must not be moved by a frame the
        // log refused to believe.
        assert_eq!(log.since_secs, None);
        // The edges of the window are accepted.
        log.record("AAA", TradingStatus::Halt, received + chrono::Duration::seconds(MAX_AHEAD_OF_RECEIPT_SECS), "H", received);
        assert_eq!(kinds(&log, "AAA"), vec![HaltMarkKind::Halted]);
        log.record("BBB", TradingStatus::Halt, received - chrono::Duration::seconds(MAX_BEHIND_RECEIPT_SECS), "H", received);
        assert_eq!(kinds(&log, "BBB"), vec![HaltMarkKind::Halted]);
    }

    #[test]
    fn a_transition_that_would_land_before_one_already_held_is_refused() {
        let mut log = Log::default();
        record(&mut log, "AAA", TradingStatus::Halt, at(14, 10), "H");
        // A resume stamped before the halt it would follow. Appending it
        // would break "oldest first"; inserting it would claim an order the
        // feed never gave. It is dropped and disclosed.
        log.record("AAA", TradingStatus::Resume, at(14, 5), "T", at(14, 11));
        assert_eq!(kinds(&log, "AAA"), vec![HaltMarkKind::Halted]);
        assert_eq!(log.out_of_order, 1);
        // Not counted as a repeat or an unmatched resume either.
        assert_eq!(log.repeated_states, 0);
        assert_eq!(log.unmatched_resumes, 0);
        // The same second as the mark held is still accepted.
        record(&mut log, "AAA", TradingStatus::Resume, at(14, 10), "T");
        assert_eq!(kinds(&log, "AAA"), vec![HaltMarkKind::Halted, HaltMarkKind::Resumed]);
        let marks = &log.symbols["AAA"].marks;
        assert!(marks.windows(2).all(|w| w[0].at_secs <= w[1].at_secs));
    }

    #[test]
    fn history_never_claims_to_be_the_whole_day() {
        let mut log = Log::default();
        assert_eq!(log.history("AAA", at(14, 0)), empty());
        // Coverage starts at the first status seen, even one that marks
        // nothing, so a chart can say how far back the record reaches.
        record(&mut log, "AAA", TradingStatus::Informational, at(13, 45), "I");
        record(&mut log, "AAA", TradingStatus::Halt, at(14, 0), "H");
        let history = log.history("AAA", at(14, 1));
        assert_eq!(history.since_secs, Some(at(13, 45).timestamp()));
        assert!(!history.complete);
        assert_eq!(history.marks.len(), 1);
    }

    #[test]
    fn a_read_on_a_new_market_day_does_not_answer_with_yesterdays_halts() {
        let mut log = Log::default();
        record(&mut log, "AAA", TradingStatus::Halt, at(14, 0), "H");
        // Still the same market day late in the evening: the mark stands.
        assert_eq!(log.history("AAA", Utc.with_ymd_and_hms(2026, 10, 7, 1, 0, 0).unwrap()).marks.len(), 1);
        // Past 04:00 ET the next day, with no status having arrived yet.
        // Without rolling at read this answered with yesterday's halt and
        // yesterday's `sinceSecs`, which reads as "halted today".
        let today = log.history("AAA", Utc.with_ymd_and_hms(2026, 10, 7, 14, 0, 0).unwrap());
        assert_eq!(today, empty());
        // And the roll actually happened, rather than being hidden in the
        // answer: yesterday's record is released.
        assert!(log.symbols.is_empty());
        assert_eq!(log.day, Some(market_day(Utc.with_ymd_and_hms(2026, 10, 7, 14, 0, 0).unwrap())));
    }

    #[test]
    fn marks_serialize_in_the_shape_the_clients_read() {
        let mark = HaltMark { at_secs: 10, kind: HaltMarkKind::Resumed, code: "T".into() };
        assert_eq!(serde_json::to_string(&mark).unwrap(), r#"{"atSecs":10,"kind":"resumed","code":"T"}"#);
        let history = HaltHistory {
            marks: vec![mark],
            since_secs: Some(5),
            complete: false,
            repeated_states: 1,
            unmatched_resumes: 2,
            rejected_timestamps: 3,
            out_of_order: 4,
            dropped_marks: 5,
            dropped_symbols: 6,
        };
        assert_eq!(
            serde_json::to_string(&history).unwrap(),
            r#"{"marks":[{"atSecs":10,"kind":"resumed","code":"T"}],"sinceSecs":5,"complete":false,"repeatedStates":1,"unmatchedResumes":2,"rejectedTimestamps":3,"outOfOrder":4,"droppedMarks":5,"droppedSymbols":6}"#
        );
    }

    #[test]
    fn the_log_is_off_until_its_own_switch_is_set() {
        // HALT_LOG_ENABLED is unset under test, which is production's own
        // default: the live loop's status arm must not reach the global
        // log, and a reader gets an empty record that says it kept none.
        assert!(!enabled());
        on_status(
            &Status {
                symbol: "AAA".into(),
                status_code: "H".into(),
                timestamp: at(14, 0),
                status_message: None,
                reason_code: None,
                reason_message: None,
                tape: None,
            },
            at(14, 0),
        );
        assert_eq!(history("AAA", at(14, 0)), empty());
        // The global log was not even created, let alone written to: a
        // reader seeing `empty()` could otherwise mean "off" or "nothing
        // recorded yet", and only one of those is true here.
        assert!(log().lock().unwrap().day.is_none());
    }

    #[test]
    fn the_symbol_map_is_bounded_and_says_so() {
        let mut log = Log::default();
        for i in 0..=MAX_SYMBOLS {
            record(&mut log, &format!("S{i}"), TradingStatus::Halt, at(14, 0), "H");
        }
        assert_eq!(log.symbols.len(), MAX_SYMBOLS);
        assert_eq!(log.dropped_symbols, 1);
        // An already-tracked symbol is still recorded past the bound.
        record(&mut log, "S0", TradingStatus::Resume, at(14, 5), "T");
        assert_eq!(kinds(&log, "S0").len(), 2);
        // Every reader is told some symbol's record is missing entirely.
        assert_eq!(log.history("S0", at(14, 5)).dropped_symbols, 1);
    }

    #[test]
    fn the_per_symbol_bound_discloses_what_it_dropped() {
        let mut log = Log::default();
        // Alternating transitions, so each one is kept, past the bound.
        for i in 0..=(MAX_MARKS_PER_SYMBOL as u32) {
            let class = if i % 2 == 0 { TradingStatus::Halt } else { TradingStatus::Resume };
            record(&mut log, "AAA", class, at(14, 0) + chrono::Duration::seconds(i.into()), class_code(class));
        }
        let history = log.history("AAA", at(15, 0));
        assert_eq!(history.marks.len(), MAX_MARKS_PER_SYMBOL);
        // The oldest mark went, so the array no longer begins where
        // `sinceSecs` says the coverage does. Said out loud rather than
        // left for a chart to misread as "the day's first halt".
        assert_eq!(history.dropped_marks, 1);
        assert_eq!(history.since_secs, Some(at(14, 0).timestamp()));
        assert_eq!(history.marks[0].at_secs, at(14, 0).timestamp() + 1);
    }

    fn class_code(class: TradingStatus) -> &'static str {
        if class == TradingStatus::Halt {
            "H"
        } else {
            "T"
        }
    }
}
