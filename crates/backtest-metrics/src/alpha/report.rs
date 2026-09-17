//! `FINAL-ALPHA-QUALIFICATION.md` (§36, and the contract review's decision 4).
//!
//! # Two conclusions, both prominent
//!
//! The report states **OI layer qualification** and **end-to-end scanner
//! coverage** separately, near the top, always. The failure this prevents runs
//! both ways: strong conditional OI performance must not hide poor scanner
//! coverage, and poor upstream detection must not make OI appear to fail a task
//! it never had the chance to perform.
//!
//! # What this report is not
//!
//! It contains no strategy recommendation. It answers whether V1 satisfied a
//! contract that was frozen and hashed before the session, and nothing else.
//! Promotion is a separate milestone, and `QUALIFIES` means only that V1 has
//! earned consideration for it.

use crate::alpha::ladder::Classification;
use crate::alpha::matrix::{EvidenceStatus, Outcome};
use crate::alpha::pipeline::{Qualification, Request};
use crate::alpha::spec::Dimension;
use crate::completeness::Verdict;

fn pct(value: Option<f64>) -> String {
    value.map_or("—".to_string(), |v| format!("{:.1}%", v * 100.0))
}

fn num(value: Option<f64>) -> String {
    value.map_or("—".to_string(), |v| format!("{v:.3}"))
}

fn interval(estimate: &crate::alpha::stats::Estimate) -> String {
    match (estimate.lower, estimate.upper) {
        (Some(low), Some(high)) => format!("{low:.3} – {high:.3}"),
        _ => "not estimable".to_string(),
    }
}

fn mark(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Pass => "PASS",
        Outcome::Fail => "**FAIL**",
        Outcome::Insufficient => "INSUFFICIENT",
    }
}

/// Renders the whole report.
pub fn render(request: &Request, q: &Qualification) -> String {
    let mut out = String::new();
    let line = |out: &mut String, text: &str| {
        out.push_str(text);
        out.push('\n');
    };

    // --- the two statuses, first, before anything else --------------------
    line(&mut out, "# Final Alpha Qualification");
    line(&mut out, "");
    line(&mut out, &format!("**SESSION STATUS:** {}", q.session_status));
    line(&mut out, "");
    line(&mut out, &format!("**OI V1 EVIDENCE STATUS:** {}", q.evidence_status));
    line(&mut out, "");

    match q.evidence_status {
        EvidenceStatus::NotEvaluated => line(
            &mut out,
            "> The session did not pass its completeness gate, so **no Alpha evaluation was \
             performed**. Nothing below is a statement about Opportunity Intelligence V1; it is a \
             statement about the capture.",
        ),
        EvidenceStatus::InsufficientEvidence => line(
            &mut out,
            "> The session is analysable, but does not carry enough evidence to decide the \
             pre-registered contract. **This is not a finding that V1 failed.**",
        ),
        EvidenceStatus::DoesNotQualify => line(
            &mut out,
            "> The session is analysable and carried sufficient evidence, and one or more \
             pre-registered blocking criteria were not met.",
        ),
        EvidenceStatus::Qualifies => line(
            &mut out,
            "> Every pre-registered blocking criterion was met. **`QUALIFIES` means V1 has earned \
             consideration for productization — not that it is promoted.** This report authorizes \
             no Auto-Trader consumption, no automatic orders from OI rank, and no change to any \
             production detector gate.",
        ),
    }
    line(&mut out, "");

    // --- the two interpretations (decision 4) ------------------------------
    line(&mut out, "## The two conclusions, separately");
    line(&mut out, "");
    let ladder = q.evaluation.as_ref().map(|e| &e.ladder);
    let coverage = ladder.and_then(|l| l.detection_coverage());
    let retention = ladder.and_then(|l| l.oi_conditional_retention());
    line(&mut out, "| Question | Answer |");
    line(&mut out, "|---|---|");
    line(
        &mut out,
        &format!(
            "| **OI layer qualification** — given what Stockspotter detected, did V1 prioritize \
             usefully, early, and without unacceptable risk degradation? | **{}** |",
            q.evidence_status
        ),
    );
    line(
        &mut out,
        &format!(
            "| **End-to-end scanner coverage** — how much of the independent meaningful-opportunity \
             population did Stockspotter detect at all? | **{}** detection coverage |",
            pct(coverage)
        ),
    );
    line(&mut out, "");
    line(
        &mut out,
        "These are different subjects. Detection coverage characterises the scanner *beneath* OI \
         and carries no pre-registered pass threshold, because none is derivable from development \
         data. OI conditional retention grades the intelligence layer on the population detection \
         actually reached.",
    );
    line(&mut out, "");
    line(&mut out, &format!("- Independent detection coverage: **{}**", pct(coverage)));
    line(&mut out, &format!("- OI conditional retention: **{}**", pct(retention)));
    line(
        &mut out,
        &format!(
            "- Causally established stage: **{}** of the reference population",
            pct(ladder.and_then(|l| l.attribution_rate()))
        ),
    );
    line(&mut out, "");

    // --- 1-6 identity and provenance --------------------------------------
    line(&mut out, "## 1–6. Session identity, provenance and completeness");
    line(&mut out, "");
    line(&mut out, "| | |");
    line(&mut out, "|---|---|");
    line(&mut out, &format!("| Session date | {} |", q.session_date));
    line(&mut out, &format!("| Session directory | `{}` |", request.session_dir.display()));
    line(
        &mut out,
        &format!(
            "| Capture commit | `{}` |",
            q.capture_commit.as_deref().unwrap_or("absent — provenance unprovable")
        ),
    );
    line(
        &mut out,
        &format!(
            "| OI config fingerprint | `{}` |",
            q.oi_config_fingerprint.as_deref().unwrap_or("absent")
        ),
    );
    line(&mut out, &format!("| Qualification spec | `{}` |", q.spec_version));
    line(&mut out, &format!("| Specification SHA-256 | `{}` |", q.spec_sha256));
    line(&mut out, &format!("| Reference label | `{}` |", request.spec.reference_label_version));
    line(&mut out, &format!("| Completeness verdict | **{}** |", q.session_status));
    line(&mut out, "");

    if !q.completeness.blocking.is_empty() {
        line(&mut out, "### Blocking conditions");
        line(&mut out, "");
        for reason in &q.completeness.blocking {
            line(&mut out, &format!("- {reason}"));
        }
        line(&mut out, "");
    }
    if !q.completeness.missing.is_empty() {
        line(&mut out, "### Missing evidence");
        line(&mut out, "");
        for reason in &q.completeness.missing {
            line(&mut out, &format!("- {reason}"));
        }
        line(&mut out, "");
    }

    line(&mut out, "### Artifact integrity");
    line(&mut out, "");
    line(&mut out, "| Artifact | Records | Bytes | Malformed | Truncated |");
    line(&mut out, "|---|---|---|---|---|");
    for artifact in &q.integrity.artifacts {
        line(
            &mut out,
            &format!(
                "| `{}` | {} | {} | {} | {} |",
                artifact.path,
                artifact.records,
                artifact.bytes,
                artifact.malformed_records,
                if artifact.truncated { "**yes**" } else { "no" }
            ),
        );
    }
    line(&mut out, "");

    let Some(evaluation) = &q.evaluation else {
        line(
            &mut out,
            "## Alpha evaluation — not performed\n\nThe pipeline stopped at the completeness gate. \
             No predictive, ranking or effectiveness claim was computed.",
        );
        line(&mut out, "");
        line(&mut out, "---");
        line(&mut out, "");
        line(&mut out, &format!("SESSION STATUS: {}", q.session_status));
        line(&mut out, &format!("OI V1 EVIDENCE STATUS: {}", q.evidence_status));
        return out;
    };

    // --- 7. population ------------------------------------------------------
    let p = &evaluation.population;
    line(&mut out, "## 7. Opportunity and reference population");
    line(&mut out, "");
    line(&mut out, "| | |");
    line(&mut out, "|---|---|");
    line(&mut out, &format!("| Raw ranking rows read | {} |", p.raw_snapshot_rows));
    line(
        &mut out,
        &format!("| **Distinct opportunities** (the analytical unit) | **{}** |", p.distinct_opportunities),
    );
    line(&mut out, &format!("| Distinct symbols (the cluster unit) | {} |", p.distinct_symbols));
    line(&mut out, &format!("| Ranking windows | {} |", p.ranking_windows));
    line(&mut out, &format!("| Reference opportunities, eligible | {} |", p.eligible_reference_opportunities));
    line(
        &mut out,
        &format!("| Positive reference opportunities (+2%) | {} |", p.positive_reference_opportunities),
    );
    line(&mut out, "");
    line(
        &mut out,
        "Repeated ranking snapshots of one opportunity are not independent observations and are \
         collapsed to a single unit entered at its first window. The gap between the first two \
         rows is the inflation that would otherwise have been treated as sample size.",
    );
    line(&mut out, "");

    // --- 8. recall ----------------------------------------------------------
    line(&mut out, "## 8. Recall — three distinct questions");
    line(&mut out, "");
    let l = &evaluation.ladder;
    line(&mut out, "| Stage reached | Count |");
    line(&mut out, "|---|---|");
    line(&mut out, &format!("| A — never visible | {} |", l.never_visible));
    line(&mut out, &format!("| B — visible, detector missed | {} |", l.visible_not_detected));
    line(&mut out, &format!("| C — detected, no OI opportunity | {} |", l.detected_no_opportunity));
    line(&mut out, &format!("| D — OI opportunity, ranked poorly | {} |", l.ranked_poorly));
    line(&mut out, &format!("| E — ranked highly, too late | {} |", l.ranked_late));
    line(&mut out, &format!("| F — ranked highly and early | {} |", l.ranked_early));
    line(&mut out, &format!("| UNKNOWN | {} |", l.unknown));
    line(&mut out, &format!("| **Total** | **{}** |", l.total));
    line(&mut out, "");
    line(
        &mut out,
        "A and B are scanner outcomes; C, D and E are Opportunity Intelligence outcomes. UNKNOWN \
         is a real category and is never redistributed.",
    );
    line(&mut out, "");

    // --- 9-13 the criteria --------------------------------------------------
    let section = |out: &mut String, title: &str, dimension: Dimension| {
        out.push_str(&format!("## {title}\n\n"));
        let Some(row) = q.matrix.row(dimension) else {
            out.push_str("Not evaluated.\n\n");
            return;
        };
        out.push_str("| Criterion | Candidate | Control | Comparison | 95% interval | Units | Clusters | Criterion | Result |\n");
        out.push_str("|---|---|---|---|---|---|---|---|---|\n");
        for criterion in &row.criteria {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                criterion.criterion_id,
                num(criterion.candidate_value),
                num(criterion.control_value),
                num(criterion.comparison),
                interval(&criterion.estimate),
                criterion.estimate.n_units,
                criterion.estimate.n_clusters,
                criterion.criterion,
                mark(criterion.outcome),
            ));
        }
        out.push('\n');
    };

    section(&mut out, "9. EarlyQuality", Dimension::EarlyQuality);
    section(&mut out, "10. ContinuationConfidence", Dimension::ContinuationQuality);
    section(&mut out, "11. Ranking quality", Dimension::RankingQuality);
    section(&mut out, "12. Earliness", Dimension::Earliness);
    section(&mut out, "13. MFE / MAE / risk", Dimension::RiskExcursion);

    // --- 14-17 segmentation --------------------------------------------------
    line(&mut out, "## 14–17. Segmentation");
    line(&mut out, "");
    if evaluation.segments.is_empty() {
        line(&mut out, "No segment carried enough evidence to estimate.");
    } else {
        line(&mut out, "| Dimension | Segment | Comparison | 95% interval | Units | Reverses |");
        line(&mut out, "|---|---|---|---|---|---|");
        for cell in &evaluation.segments {
            line(
                &mut out,
                &format!(
                    "| {} | {} | {} | {} | {} | {} |",
                    cell.dimension,
                    cell.segment,
                    num(cell.estimate.point),
                    interval(&cell.estimate),
                    cell.estimate.n_units,
                    if cell.reverses { "**yes**" } else { "no" }
                ),
            );
        }
    }
    line(&mut out, "");
    line(
        &mut out,
        "Segmentation is diagnostic and cannot change the verdict. A cell marked *reverses* is a \
         reason for human review, not a failure.",
    );
    line(&mut out, "");

    // --- secondary direction (decision 2) ------------------------------------
    line(&mut out, "## 18. Direction at the secondary targets");
    line(&mut out, "");
    line(
        &mut out,
        "Descriptive, and structurally unable to change the verdict. It distinguishes a model \
         identifying short continuation from one whose information also points toward larger \
         runner-type outcomes.",
    );
    line(&mut out, "");
    if evaluation.secondary_direction.is_empty() {
        line(&mut out, "Not estimable on this session.");
    } else {
        line(&mut out, "| Surface | Target | Horizon | Candidate | Control | Ratio | 95% interval |");
        line(&mut out, "|---|---|---|---|---|---|---|");
        for surface in &evaluation.secondary_direction {
            line(
                &mut out,
                &format!(
                    "| {} | +{:.0}% | {}s | {} | {} | {} | {} |",
                    surface.surface,
                    surface.target_pct,
                    surface.horizon_secs,
                    num(surface.candidate),
                    num(surface.control),
                    num(surface.estimate.point),
                    interval(&surface.estimate),
                ),
            );
        }
    }
    line(&mut out, "");

    // --- 19. the matrix -----------------------------------------------------
    line(&mut out, "## 19. Qualification matrix");
    line(&mut out, "");
    line(&mut out, "| Dimension | Blocking | Result |");
    line(&mut out, "|---|---|---|");
    for row in &q.matrix.rows {
        line(
            &mut out,
            &format!(
                "| {:?} | {} | {} |",
                row.dimension,
                if row.blocking { "yes" } else { "diagnostic" },
                mark(row.outcome)
            ),
        );
    }
    line(&mut out, "");
    if !q.matrix.failed.is_empty() {
        line(&mut out, &format!("**Failed blocking criteria:** {}", q.matrix.failed.join(", ")));
        line(&mut out, "");
    }
    if !q.matrix.undecided.is_empty() {
        line(&mut out, &format!("**Undecided blocking criteria:** {}", q.matrix.undecided.join(", ")));
        line(&mut out, "");
    }

    // --- 20. limitations ----------------------------------------------------
    line(&mut out, "## 20. Evidence limitations");
    line(&mut out, "");
    if !q.matrix.evidence_shortfalls.is_empty() {
        for shortfall in &q.matrix.evidence_shortfalls {
            line(&mut out, &format!("- **Minimum evidence:** {shortfall}"));
        }
    }
    for note in &q.matrix.notes {
        line(&mut out, &format!("- {note}"));
    }
    for note in &evaluation.notes {
        line(&mut out, &format!("- {note}"));
    }
    let censored: usize = q
        .matrix
        .rows
        .iter()
        .flat_map(|r| r.criteria.iter())
        .map(|c| c.estimate.n_censored)
        .max()
        .unwrap_or(0);
    if censored > 0 {
        line(
            &mut out,
            &format!(
                "- Up to {censored} opportunities were censored in at least one comparison — \
                 excluded from both cohorts and never counted as failures."
            ),
        );
    }
    line(
        &mut out,
        "- This is a single session. A pre-registered contract satisfied once is evidence, not \
         proof of generalisation.",
    );
    line(&mut out, "");

    // --- 21. final ----------------------------------------------------------
    line(&mut out, "## 21. Final evidence status");
    line(&mut out, "");
    line(&mut out, &format!("**SESSION STATUS: {}**", q.session_status));
    line(&mut out, "");
    line(&mut out, &format!("**OI V1 EVIDENCE STATUS: {}**", q.evidence_status));
    line(&mut out, "");
    line(
        &mut out,
        "No strategy recommendation is made or implied. This report answers whether V1 satisfied \
         a contract frozen and hashed before the session, and nothing else.",
    );
    out
}

/// Classification counts as a one-line summary, for the runner section.
pub fn classification_line(classification: Classification, count: usize, total: usize) -> String {
    let share = if total == 0 { 0.0 } else { count as f64 / total as f64 * 100.0 };
    format!("{} — {count} ({share:.1}%)", classification.letter())
}

/// Whether a rendered report satisfies the contract's reporting obligations.
///
/// Checked rather than trusted: a reporting duty that can be dropped while
/// writing is not a duty. The two-interpretation requirement is the one most
/// likely to go missing, because it is the one that can make a good result look
/// worse.
pub fn missing_requirements(request: &Request, rendered: &str) -> Vec<String> {
    let mut missing = Vec::new();
    for requirement in &request.spec.reporting {
        let present = match requirement.id.as_str() {
            "R-two-interpretations" => {
                rendered.contains("OI layer qualification")
                    && rendered.contains("End-to-end scanner coverage")
            }
            "R-detection-coverage" => rendered.contains("detection coverage"),
            "R-runner-stage-ladder" => {
                rendered.contains("never visible") && rendered.contains("UNKNOWN")
            }
            "R-secondary-direction" => rendered.contains("secondary targets"),
            _ => true,
        };
        if !present {
            missing.push(requirement.id.clone());
        }
    }
    let _ = Verdict::Valid;
    missing
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
