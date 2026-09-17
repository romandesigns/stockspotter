//! `alpha_qualify` — the Final Alpha Qualification System's command (§9).
//!
//! ```text
//! cargo run -p backtest-metrics --bin alpha_qualify -- \
//!     --session <session-dir> \
//!     --session-date 2026-09-18 \
//!     --expected-commit <sha> \
//!     --expected-oi-config oi-cfg-b4f21c8b311a1b99 \
//!     --output reports/qualification-2026-09-18
//! ```
//!
//! # What it will not do
//!
//! * **Never modifies a source artifact.** Inputs are opened read-only; a test
//!   digests every input before and after a run and requires them unchanged.
//! * **Never connects to production.** No network, no credentials, no reads of
//!   anything but the files it is pointed at.
//! * **Never places an order** and never touches strategy state.
//! * **Never tunes a model.** The contract it applies is frozen in source and
//!   hashed into the report; this binary has no code path that edits it.
//! * **Never overwrites a result.** The output directory must not exist.
//!
//! # Exit codes
//!
//! ```text
//! 0  the run completed -- read SESSION STATUS and OI V1 EVIDENCE STATUS
//! 1  the run could not start (bad arguments, missing artifacts, output exists)
//! ```
//!
//! A qualification that returns INVALID or DOES_NOT_QUALIFY is a *successful
//! run* and exits 0: the pipeline did its job. Conflating "the tool failed"
//! with "the evidence failed" would make a scripted operator unable to tell a
//! broken run from a real negative.
//!
//! # Printing the spec
//!
//! `--print-spec` writes the frozen contract and its hash to stdout and exits.
//! It reads nothing, so it can be used before a session to record exactly which
//! contract the upcoming run will be judged against.

use std::path::PathBuf;

use backtest_metrics::alpha::pipeline::{self, Request};
use backtest_metrics::alpha::spec::QualificationSpec;

fn usage() -> String {
    "alpha_qualify — offline Alpha qualification for one captured session\n\
     \n\
     USAGE:\n    \
       alpha_qualify --session <dir> --session-date <YYYY-MM-DD> --output <dir> [options]\n\
     \n\
     REQUIRED:\n    \
       --session <dir>            directory holding research/ and discovery-audit/\n    \
       --session-date <date>      the UTC session to evaluate, YYYY-MM-DD\n    \
       --output <dir>             report directory; must NOT already exist\n\
     \n\
     OPTIONS:\n    \
       --expected-commit <sha>    the deployed commit the session must carry\n    \
       --expected-oi-config <fp>  OI config fingerprint; defaults to the contract's\n    \
       --expected-spec-sha256 <h> contract hash recorded BEFORE the session opened\n    \
       --print-spec               print the frozen contract and its hash, then exit\n    \
       --help                     this text\n"
        .to_string()
}

struct Args {
    session: Option<PathBuf>,
    session_date: Option<String>,
    output: Option<PathBuf>,
    expected_commit: Option<String>,
    expected_oi_config: Option<String>,
    expected_spec_sha256: Option<String>,
    print_spec: bool,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        session: None,
        session_date: None,
        output: None,
        expected_commit: None,
        expected_oi_config: None,
        expected_spec_sha256: None,
        print_spec: false,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--session" => args.session = Some(PathBuf::from(value()?)),
            "--session-date" => args.session_date = Some(value()?),
            "--output" => args.output = Some(PathBuf::from(value()?)),
            "--expected-commit" => args.expected_commit = Some(value()?),
            "--expected-oi-config" => args.expected_oi_config = Some(value()?),
            "--expected-spec-sha256" => args.expected_spec_sha256 = Some(value()?),
            "--print-spec" => args.print_spec = true,
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = match parse() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("alpha_qualify: {error}\n\n{}", usage());
            std::process::exit(1);
        }
    };

    let spec = QualificationSpec::default();

    if args.print_spec {
        println!("{}", spec.canonical_json());
        eprintln!("\nversion : {}", spec.version);
        eprintln!("sha256  : {}", spec.sha256());
        return;
    }

    let (Some(session), Some(session_date), Some(output)) =
        (args.session, args.session_date, args.output)
    else {
        eprintln!("alpha_qualify: --session, --session-date and --output are required\n\n{}", usage());
        std::process::exit(1);
    };

    let request = Request {
        session_dir: session,
        session_date,
        expected_commit: args.expected_commit,
        expected_oi_config: args.expected_oi_config,
        expected_spec_sha256: args.expected_spec_sha256,
        output_dir: output,
        spec,
    };

    match pipeline::run(&request) {
        Ok(result) => {
            // The two decisions, on their own lines, so a script can read them
            // without parsing JSON.
            println!("SESSION STATUS: {}", result.session_status);
            println!("OI V1 EVIDENCE STATUS: {}", result.evidence_status);
            println!();
            println!("specification    : {} {}", result.spec_version, result.spec_sha256);
            println!(
                "capture commit   : {}",
                result.capture_commit.as_deref().unwrap_or("absent — provenance unprovable")
            );
            println!(
                "oi config        : {}",
                result.oi_config_fingerprint.as_deref().unwrap_or("absent")
            );
            println!("report           : {}", request.output_dir.join("FINAL-ALPHA-QUALIFICATION.md").display());

            if !result.completeness.blocking.is_empty() {
                println!("\nblocking:");
                for reason in &result.completeness.blocking {
                    println!("  - {reason}");
                }
            }
            if !result.completeness.missing.is_empty() {
                println!("\nmissing evidence:");
                for reason in &result.completeness.missing {
                    println!("  - {reason}");
                }
            }
            if !result.matrix.evidence_shortfalls.is_empty() {
                println!("\nminimum-evidence shortfalls:");
                for reason in &result.matrix.evidence_shortfalls {
                    println!("  - {reason}");
                }
            }
            if !result.matrix.failed.is_empty() {
                println!("\nfailed blocking criteria: {}", result.matrix.failed.join(", "));
            }
            // Exit 0 either way: the run succeeded. A negative verdict is a
            // result, not a tool failure, and a script must be able to tell
            // those apart.
        }
        Err(error) => {
            eprintln!("alpha_qualify: {error}");
            std::process::exit(1);
        }
    }
}
