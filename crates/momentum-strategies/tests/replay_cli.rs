use momentum_strategies::Bar;
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn run(input: &str, args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_replay_strategies"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Invalid arguments may be rejected before stdin is read. Linux can
    // schedule that exit before this write; that is a legitimate rejection,
    // not a test panic. Never swallow other I/O failures or a successful exit.
    let early_rejection = match child.stdin.take().unwrap().write_all(input.as_bytes()) {
        Ok(()) => false,
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => true,
        Err(error) => panic!("replay fixture input failed: {error}"),
    };
    let output = child.wait_with_output().unwrap();
    if early_rejection {
        assert!(
            !output.status.success(),
            "closed stdin cannot establish successful replay"
        );
    }
    output
}
fn record(symbol: &str) -> serde_json::Value {
    let bar = Bar {
        market_open_ms: 1_800_000_000,
        received_at_ms: 1_800_060_000,
        decision_at_ms: 1_800_060_000,
        market_day: "2026-08-26".into(),
        open: 10.0,
        high: 10.2,
        low: 9.8,
        close: 10.0,
        volume: 100,
        is_final: true,
        interval_secs: 60,
    };
    serde_json::json!({"sessionId":format!("{symbol}/2026-08-26"),"symbol":symbol,"bar":bar})
}
#[test]
fn availability_is_explicit_and_self_describing() {
    let text = format!("{}\n", record("TEST"));
    assert!(!run(&text, &[]).status.success());
    let output = run(&text, &["--idealized-close-availability"]);
    assert!(output.status.success());
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["availability"], "idealized_completed_bar_close");
    assert_eq!(summary["contractVersion"], momentum_strategies::CONTRACT);
    assert_eq!(summary["configSha256"].as_str().unwrap().len(), 64);
}
#[test]
fn mismatched_availability_empty_malformed_and_partial_inputs_fail() {
    let mut altered = record("TEST");
    altered["bar"]["decisionAtMs"] = serde_json::json!(1_800_060_001);
    for text in [
        String::new(),
        "{}\n".into(),
        record("TEST").to_string(),
        format!("{altered}\n"),
    ] {
        assert!(!run(&text, &["--idealized-close-availability"])
            .status
            .success());
    }
}
#[test]
fn noncontiguous_sessions_and_symbol_day_mismatch_fail() {
    let a = record("AAA");
    let b = record("BBB");
    assert!(!run(
        &format!("{a}\n{b}\n{a}\n"),
        &["--idealized-close-availability"]
    )
    .status
    .success());
    let mut mismatch = record("TEST");
    mismatch["bar"]["marketDay"] = serde_json::json!("2026-08-27");
    assert!(!run(
        &format!("{mismatch}\n"),
        &["--idealized-close-availability"]
    )
    .status
    .success());
}
#[test]
fn distinct_sessions_have_distinct_complete_summaries() {
    let a = record("AAA");
    let b = record("BBB");
    let output = run(&format!("{a}\n{b}\n"), &["--idealized-close-availability"]);
    assert!(output.status.success());
    let lines = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<serde_json::Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0]["sessionId"], rows[1]["sessionId"]);
}

#[test]
fn rejected_arguments_may_close_stdin_before_the_fixture_finishes_writing() {
    // Larger than a pipe buffer: this exercises the early-close path rather
    // than depending on a scheduler race with a single tiny record.
    assert!(!run(&"x".repeat(256 * 1024), &[]).status.success());
}
