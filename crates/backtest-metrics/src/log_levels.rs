//! Classifying a `tracing` log line by level, correctly, when the line may be
//! ANSI-coloured.
//!
//! # The defect this exists to prevent recurring
//!
//! The September-16 completeness gate reported **0 ERROR lines** across
//! 6,270,975 `ws` log lines. The true figure was **10**, and they were a third
//! independent capture failure — discovery's own queue saturating.
//!
//! The mistake was in the query, not the data. `tracing`'s default formatter
//! right-pads level names to a fixed width, and it does so *inside* the ANSI
//! escape:
//!
//! ```text
//!   INFO  ->  \x1b[32m INFO\x1b[0m     (leading space, inside the escape)
//!   WARN  ->  \x1b[33m WARN\x1b[0m
//!   ERROR ->  \x1b[31mERROR\x1b[0m     (no padding: five characters already)
//! ```
//!
//! A filter of `\bERROR` matches the first two cases' neighbours fine, but for
//! `ERROR` the preceding character is the `m` that closes the escape. `m` is a
//! word character and so is `E`, so there is no word boundary between them and
//! the pattern matches nothing at all. It fails silently, and it fails only on
//! the level anyone actually cares about.
//!
//! # The rule
//!
//! **Structured counters are authoritative.** This module is for the cases
//! where a log line is genuinely the only evidence available — reading a
//! historical capture, or a subsystem whose counters are not exposed. It is
//! not a health mechanism, and nothing in the completeness checker consults it.
//!
//! Correctness here comes from stripping the escapes *first* and then matching
//! a trimmed token, rather than from a cleverer regex over coloured text.

/// Log levels `tracing` emits, in severity order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Removes ANSI SGR and other CSI escape sequences.
///
/// Deliberately a small state machine rather than a regex: the whole point of
/// this module is that pattern-matching *around* escapes is what went wrong,
/// and a dependency-free strip is easier to be sure about than a pattern that
/// has to anticipate every sequence a terminal writer might emit.
pub fn strip_ansi(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            // CSI: ESC [ ... final byte in 0x40..=0x7e
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1; // consume the final byte
            } else {
                // Two-character escape, or a stray ESC at end of line.
                i += 1;
            }
            continue;
        }
        // Safe: we only ever skip over ASCII escape bytes, so `i` always lands
        // on a UTF-8 boundary.
        let start = i;
        while i < bytes.len() && bytes[i] != 0x1b {
            i += 1;
        }
        out.push_str(&line[start..i]);
    }
    out
}

/// The level of one formatted log line, or `None` when it carries none.
///
/// Matches a whitespace-delimited token, so padding cannot hide a level and a
/// level cannot be found inside an unrelated word — `"error"` in a message body
/// is not a level, and neither is `RECOVERED`.
pub fn level_of(line: &str) -> Option<Level> {
    let plain = strip_ansi(line);
    for token in plain.split_whitespace() {
        let candidate = match token {
            "TRACE" => Level::Trace,
            "DEBUG" => Level::Debug,
            "INFO" => Level::Info,
            "WARN" => Level::Warn,
            "ERROR" => Level::Error,
            _ => continue,
        };
        return Some(candidate);
    }
    None
}

/// Counts of each level across a set of lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelCounts {
    pub trace: u64,
    pub debug: u64,
    pub info: u64,
    pub warn: u64,
    pub error: u64,
    /// Lines carrying no recognisable level — continuation lines, backtraces,
    /// output from a subprocess. Counted rather than ignored, so a reader can
    /// tell "no errors" from "this is not a `tracing` log at all".
    pub unclassified: u64,
}

pub fn count_levels<'a>(lines: impl IntoIterator<Item = &'a str>) -> LevelCounts {
    let mut counts = LevelCounts::default();
    for line in lines {
        match level_of(line) {
            Some(Level::Trace) => counts.trace += 1,
            Some(Level::Debug) => counts.debug += 1,
            Some(Level::Info) => counts.info += 1,
            Some(Level::Warn) => counts.warn += 1,
            Some(Level::Error) => counts.error += 1,
            None => counts.unclassified += 1,
        }
    }
    counts
}

#[cfg(test)]
#[path = "log_levels_tests.rs"]
mod tests;
