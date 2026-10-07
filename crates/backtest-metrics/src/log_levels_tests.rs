//! Section 12 fixtures.
//!
//! The central case is `ansi_coloured_error_is_classified`, which is the exact
//! line shape that defeated `\bERROR` and produced a published "0 ERROR" for a
//! log containing ten of them. Everything else guards the edges around it.

use super::*;

/// The three level shapes `tracing`'s ANSI formatter actually produces.
///
/// Note the padding: `INFO` and `WARN` are right-aligned to five characters
/// *inside* the escape, so they carry a leading space that `ERROR` does not.
/// That asymmetry is the whole defect.
fn tracing_line(level: &str) -> String {
    let (colour, padded) = match level {
        "ERROR" => ("31", "ERROR"),
        "WARN" => ("33", " WARN"),
        "INFO" => ("32", " INFO"),
        "DEBUG" => ("34", "DEBUG"),
        _ => ("37", "TRACE"),
    };
    format!(
        "2026-09-16T19:50:01.123456Z \u{1b}[{colour}m{padded}\u{1b}[0m \
         \u{1b}[2mmarket_data::discovery_audit\u{1b}[0m\u{1b}[2m:\u{1b}[0m \
         discovery audit queue full or writer stopped lost_records=4096"
    )
}

#[test]
fn ansi_coloured_error_is_classified() {
    let line = tracing_line("ERROR");
    assert_eq!(level_of(&line), Some(Level::Error));

    // The regression itself, asserted directly: the published gate used
    // `\bERROR` against exactly this text and matched nothing, because the
    // character before `E` is the `m` closing the escape and `m` is a word
    // character. Kept as a test so the reason survives the fix.
    let boundary_would_match = line
        .match_indices("ERROR")
        .any(|(i, _)| i == 0 || !line.as_bytes()[i - 1].is_ascii_alphanumeric());
    assert!(
        !boundary_would_match,
        "this fixture must reproduce the no-word-boundary condition, or it proves nothing"
    );
}

#[test]
fn ansi_coloured_info_and_warn_are_classified() {
    assert_eq!(level_of(&tracing_line("INFO")), Some(Level::Info));
    assert_eq!(level_of(&tracing_line("WARN")), Some(Level::Warn));
    assert_eq!(level_of(&tracing_line("DEBUG")), Some(Level::Debug));
    assert_eq!(level_of(&tracing_line("TRACE")), Some(Level::Trace));
}

#[test]
fn uncoloured_lines_are_classified_identically() {
    for level in ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"] {
        let coloured = tracing_line(level);
        let plain = strip_ansi(&coloured);
        assert_eq!(
            level_of(&coloured),
            level_of(&plain),
            "colour must not change the classification of {level}"
        );
    }
}

#[test]
fn stripping_leaves_the_message_intact() {
    let plain = strip_ansi(&tracing_line("ERROR"));
    assert!(!plain.contains('\u{1b}'), "no escape bytes may survive");
    assert!(!plain.contains("[31m"), "no escape bodies may survive either");
    assert!(plain.contains("lost_records=4096"), "the payload must be untouched");
    assert!(plain.contains("market_data::discovery_audit"));
}

#[test]
fn a_level_word_inside_a_message_is_not_a_level() {
    // A whitespace-delimited token is the rule, so these must not match.
    assert_eq!(level_of("2026-09-16  INFO ws: recovered from an error state"), Some(Level::Info));
    assert_eq!(level_of("2026-09-16  INFO ws: ERRORS=0"), Some(Level::Info));
    assert_eq!(level_of("no level here at all"), None);
    assert_eq!(level_of(""), None);
}

#[test]
fn the_first_level_token_wins() {
    // A WARN line whose message quotes the word ERROR is a WARN.
    let line = format!("{} and the word ERROR appears later", tracing_line("WARN"));
    assert_eq!(level_of(&line), Some(Level::Warn));
}

#[test]
fn counting_reproduces_the_september_16_shape() {
    // Ten ERROR, nineteen WARN, the rest INFO -- the real distribution the gate
    // had to establish, at a scale a test can hold.
    let mut lines: Vec<String> = Vec::new();
    for _ in 0..10 {
        lines.push(tracing_line("ERROR"));
    }
    for _ in 0..19 {
        lines.push(tracing_line("WARN"));
    }
    for _ in 0..971 {
        lines.push(tracing_line("INFO"));
    }
    lines.push("  Compiling something (not a tracing line)".to_string());

    let counts = count_levels(lines.iter().map(|s| s.as_str()));
    assert_eq!(counts.error, 10);
    assert_eq!(counts.warn, 19);
    assert_eq!(counts.info, 971);
    assert_eq!(counts.unclassified, 1, "a non-tracing line must be counted, not silently ignored");
}

#[test]
fn malformed_escapes_do_not_panic_or_swallow_text() {
    for line in [
        "\u{1b}",
        "\u{1b}[",
        "\u{1b}[31",
        "\u{1b}[31mERROR",
        "plain\u{1b}[0m",
        "\u{1b}]8;;http://example\u{7}ERROR",
    ] {
        let _ = strip_ansi(line);
        let _ = level_of(line);
    }
    assert_eq!(level_of("\u{1b}[31mERROR"), Some(Level::Error));
}

#[test]
fn multibyte_text_survives_stripping() {
    let line = "\u{1b}[31mERROR\u{1b}[0m symbol=ÑÁÜ message=café — em dash";
    let plain = strip_ansi(line);
    assert_eq!(level_of(line), Some(Level::Error));
    assert!(plain.contains("café"));
    assert!(plain.contains("ÑÁÜ"));
    assert!(plain.contains("—"));
}
