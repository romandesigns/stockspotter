//! Build-time identity: which commit produced this binary.
//!
//! # Why this exists
//!
//! The Stage A deployment established that the live completeness surface
//! omitted `commit` entirely, because `option_env!("STOCKSPOTTER_COMMIT")` was
//! never populated at build time. That is not cosmetic. The qualification
//! pipeline verifies provenance against an expected capture commit, and
//! `completeness::check` treats absent provenance as **INDETERMINATE** —
//! correctly, since a report that cannot say which build produced it cannot
//! prove anything. As deployed, the pipeline would have refused the very
//! session it exists to evaluate.
//!
//! # Why compiled in, and not read at runtime
//!
//! `ops/vps/.deployed-commit` already exists and is trivially readable. It is
//! the wrong source, and deliberately not used here: it is a **mutable runtime
//! file** describing what the deploy script last recorded, not what this
//! binary was built from. The two can disagree — a half-finished deploy, a
//! hand-edited file, a container running an older image against a newer
//! checkout — and in exactly those cases an honest provenance stamp is the
//! thing that catches it. Reading it back would turn the check into a
//! tautology.
//!
//! `option_env!` resolves at compile time, so the value is baked into the
//! binary and cannot be changed by anything the running process can reach.
//!
//! # Cache invalidation
//!
//! `build.rs` emits `cargo:rerun-if-env-changed=STOCKSPOTTER_COMMIT`. Without
//! it, cargo would happily reuse a cached object file compiled with a previous
//! commit's stamp, and the binary would confidently report the wrong identity —
//! which is worse than reporting none.

/// A well-formed 40-character lowercase git object id.
fn looks_like_a_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The commit this binary was built from, or `None`.
///
/// `None` in two cases, and they are deliberately not distinguished to the
/// caller because both mean the same thing downstream — *this build cannot
/// prove its identity*, so any session it captures is INDETERMINATE rather
/// than VALID:
///
/// * the stamp was never set (an ordinary `cargo run`, or a CI build);
/// * the stamp is set but malformed.
///
/// A malformed stamp reads as **absent** rather than being passed through. The
/// failure directions are not symmetric: an absent stamp makes a session
/// unprovable, while a wrong-looking stamp passed through could silently
/// mismatch a correct expectation and be read as a *different build having run*
/// — a much more misleading answer.
pub fn build_commit() -> Option<&'static str> {
    let raw = option_env!("STOCKSPOTTER_COMMIT")?;
    let trimmed = raw.trim();
    if looks_like_a_sha(trimmed) {
        Some(trimmed)
    } else {
        None
    }
}

/// Whether this binary can prove which commit produced it.
pub fn is_stamped() -> bool {
    build_commit().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_object_id_is_accepted() {
        assert!(looks_like_a_sha("af986b84cd3745f077b48fef912610990b7db725"));
        assert!(looks_like_a_sha("79c21e16c3fac00f36d52a20828ff65f56657acd"));
        assert!(looks_like_a_sha("0000000000000000000000000000000000000000"));
    }

    #[test]
    fn anything_that_is_not_an_object_id_is_rejected() {
        for bad in [
            "",
            "not-a-sha",
            "af986b8",                                    // abbreviated
            "af986b84cd3745f077b48fef912610990b7db72",    // 39
            "af986b84cd3745f077b48fef912610990b7db7255",  // 41
            "AF986B84CD3745F077B48FEF912610990B7DB725",   // uppercase
            "af986b84cd3745f077b48fef912610990b7db72g",   // non-hex
            "$STOCKSPOTTER_COMMIT",                       // an unexpanded variable
            "af986b84cd3745f077b48fef912610990b7db725\n", // trimmed by the caller
        ] {
            assert!(!looks_like_a_sha(bad), "{bad:?} must not pass as an object id");
        }
    }

    /// An unexpanded shell variable is the realistic failure: a build arg that
    /// was declared but never given a value arrives as the literal text. Read
    /// as a commit it would mismatch every expectation and be reported as *a
    /// different build having run*, which is a far more misleading answer than
    /// "this build cannot prove its identity".
    #[test]
    fn a_malformed_stamp_reads_as_absent_not_as_a_wrong_identity() {
        assert!(!looks_like_a_sha("${STOCKSPOTTER_COMMIT}"));
        assert!(!looks_like_a_sha("unknown"));
        assert!(!looks_like_a_sha("HEAD"));
    }

    /// End-to-end: when the stamp is supplied at *compile* time it must reach
    /// `build_commit`, and `build.rs`'s `rerun-if-env-changed` must have made
    /// cargo recompile rather than reuse an object built without it.
    ///
    /// Skipped when the variable is absent at compile time, which is the normal
    /// case for a local test run; the deployment path is exercised by building
    /// with it set (see `ops/vps/deploy.sh`).
    #[test]
    fn a_stamp_supplied_at_build_time_reaches_the_surface() {
        let compiled_in = option_env!("STOCKSPOTTER_COMMIT");
        match compiled_in {
            None | Some("") => {
                assert_eq!(build_commit(), None, "no stamp means no identity");
            }
            Some(raw) => {
                assert_eq!(
                    build_commit(),
                    Some(raw.trim()).filter(|v| looks_like_a_sha(v)),
                    "a compile-time stamp must reach build_commit unchanged"
                );
                println!("compiled-in stamp: {raw}");
            }
        }
    }

    /// In this test binary the stamp is normally unset, and that must be a
    /// clean `None` rather than a panic or an empty string.
    #[test]
    fn an_unstamped_build_reports_no_commit_rather_than_a_bad_one() {
        match build_commit() {
            None => assert!(!is_stamped()),
            Some(sha) => {
                // If CI ever does stamp the test build, the value must still be
                // well-formed -- never a half-substituted placeholder.
                assert!(looks_like_a_sha(sha), "a stamped build must carry a real object id: {sha}");
                assert!(is_stamped());
            }
        }
    }
}
