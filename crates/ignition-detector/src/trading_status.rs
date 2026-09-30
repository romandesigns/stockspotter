//! The one authoritative interpretation of SIP trading-status messages.
//!
//! Alpaca forwards the consolidated tapes' own status codes, and the two
//! plans use **different code spaces**:
//!
//! | Plan (tapes)       | Halt / pause                                  | Resume | Other |
//! |--------------------|-----------------------------------------------|--------|-------|
//! | CTA (A, B)         | `2` Trading Halt (LULD pause = `2`, reason `M`) | `3`  | `5` `6` indications, `7` `8` `9` `A` `C` `D` imbalances, `E` SSR, `F` LULD band info |
//! | UTP (C, O)         | `H` Trading Halt, `P` Volatility Trading Pause | `T`   | `Q` Quotation Resumption: quoting only, still not tradable |
//!
//! Source: Alpaca real-time stock data documentation, "Trading Status"
//! section, snapshotted 2026-09-28T21:54:57Z (SHA-256
//! `5a1857f5d0cd4652079dadd45df0a674b370ea725e8ddf2c9f1ccb4ade211f83`,
//! preserved with the Step-4 static-metadata evidence). The classification
//! is static metadata only -- never inferred from trade gaps or from what
//! stocks did afterwards.
//!
//! Every status decision in this crate goes through [`classify_status`];
//! nothing else compares raw status codes.

/// What a status message means for whether normal trading is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingStatus {
    /// Regulatory or exchange halt.
    Halt,
    /// Volatility (LULD) trading pause.
    Pause,
    /// Not tradable yet (quotation-only period before trading resumes).
    NonTradable,
    /// Normal trading resumes.
    Resume,
    /// No effect on trading availability (indications, imbalances, SSR,
    /// LULD band information).
    Informational,
    /// A code this table does not define, or one from the other plan's code
    /// space. Never treated as a resume (or as a halt).
    Unknown,
}

impl TradingStatus {
    /// Normal execution is unavailable.
    pub fn interrupts(self) -> bool {
        matches!(self, Self::Halt | Self::Pause | Self::NonTradable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    Cta,
    Utp,
}

fn plan_of_tape(tape: &str) -> Option<Plan> {
    match tape {
        "A" | "B" => Some(Plan::Cta),
        "C" | "O" => Some(Plan::Utp),
        _ => None,
    }
}

fn cta(code: &str) -> Option<TradingStatus> {
    use TradingStatus::*;
    Some(match code {
        "2" => Halt,
        "3" => Resume,
        "5" | "6" | "7" | "8" | "9" | "A" | "C" | "D" | "E" | "F" => Informational,
        _ => return None,
    })
}

fn utp(code: &str) -> Option<TradingStatus> {
    use TradingStatus::*;
    Some(match code {
        "H" => Halt,
        "P" => Pause,
        "Q" => NonTradable,
        "T" => Resume,
        _ => return None,
    })
}

/// Classifies one status message by `(tape, status code)`.
///
/// * Known tape: the code is read in that plan's code space only. A code from
///   the other plan (e.g. `H` on tape A) is [`TradingStatus::Unknown`].
/// * Tape absent or unrecognised: the two plans' code spaces are disjoint, so
///   a code that belongs to exactly one plan is read in that plan. This keeps
///   a message without `z` from losing a halt the code alone makes
///   unambiguous (today's `H` handling works without a tape, and must keep
///   working); anything else is `Unknown`.
pub fn classify_status(tape: Option<&str>, code: &str) -> TradingStatus {
    let classified = match tape.and_then(plan_of_tape) {
        Some(Plan::Cta) => cta(code),
        Some(Plan::Utp) => utp(code),
        None => match (cta(code), utp(code)) {
            (Some(c), None) => Some(c),
            (None, Some(u)) => Some(u),
            _ => None,
        },
    };
    classified.unwrap_or(TradingStatus::Unknown)
}

#[cfg(test)]
mod tests {
    use super::TradingStatus::*;
    use super::*;

    #[test]
    fn cta_codes_are_read_in_the_cta_space() {
        for tape in ["A", "B"] {
            assert_eq!(classify_status(Some(tape), "2"), Halt);
            assert_eq!(classify_status(Some(tape), "3"), Resume);
            for info in ["5", "6", "7", "8", "9", "A", "C", "D", "E", "F"] {
                assert_eq!(classify_status(Some(tape), info), Informational, "{info}");
            }
            for foreign in ["H", "P", "Q", "T"] {
                assert_eq!(classify_status(Some(tape), foreign), Unknown, "tape {tape} code {foreign}");
            }
        }
    }

    #[test]
    fn utp_codes_are_read_in_the_utp_space() {
        for tape in ["C", "O"] {
            assert_eq!(classify_status(Some(tape), "H"), Halt);
            assert_eq!(classify_status(Some(tape), "P"), Pause);
            assert_eq!(classify_status(Some(tape), "Q"), NonTradable);
            assert_eq!(classify_status(Some(tape), "T"), Resume);
            for foreign in ["2", "3", "5", "E", "F"] {
                assert_eq!(classify_status(Some(tape), foreign), Unknown, "tape {tape} code {foreign}");
            }
        }
    }

    #[test]
    fn a_missing_or_unknown_tape_falls_back_only_where_the_code_is_unambiguous() {
        for tape in [None, Some(""), Some("Z")] {
            assert_eq!(classify_status(tape, "H"), Halt);
            assert_eq!(classify_status(tape, "2"), Halt);
            assert_eq!(classify_status(tape, "T"), Resume);
            assert_eq!(classify_status(tape, "3"), Resume);
            assert_eq!(classify_status(tape, "X9"), Unknown);
            assert_eq!(classify_status(tape, ""), Unknown);
        }
    }

    #[test]
    fn only_halts_pauses_and_quote_only_periods_interrupt() {
        assert!(Halt.interrupts() && Pause.interrupts() && NonTradable.interrupts());
        assert!(!Resume.interrupts() && !Informational.interrupts() && !Unknown.interrupts());
    }
}
