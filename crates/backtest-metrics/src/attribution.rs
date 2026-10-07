//! Discovery attribution (§16) — where in the pipeline did a symbol actually
//! get to, and can the evidence tell us at all?
//!
//! The question this answers is "was this symbol ever *able* to be found", not
//! "was it a good trade". It is the join that makes recall statements possible:
//! without it, a symbol absent from the detector output is indistinguishable
//! from a symbol the universe scan never returned, and those two mean opposite
//! things about the platform.
//!
//! # Three states, never two
//!
//! Every stage is `Option<bool>`:
//!
//! * `Some(true)` — positively observed at this stage.
//! * `Some(false)` — positively observed *absent*: the stage ran, and this
//!   symbol was not in it.
//! * `None` — no evidence either way.
//!
//! Collapsing the last two is the single most tempting error here, and it
//! manufactures recall: "not in the qualified list" and "we never saw the
//! qualified list" would both read as "did not qualify". §3 and §16 both
//! forbid it, so the type forbids it.
//!
//! # No fabrication
//!
//! Where the evidence cannot distinguish causes, the verdict is
//! [`Attribution::Unknown`] with the reason stated. It is never the nearest
//! plausible stage.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const ATTRIBUTION_SCHEMA_VERSION: u32 = 1;

/// The pipeline stages a symbol can be evidenced at, in causal order.
///
/// `Qualified` and `QuietSelected` are deliberately at the *same* depth: they
/// are two parallel outcomes of selection (the movers path and the quiet-watch
/// path), not a ranking of each other. Ordering them would invent a hierarchy
/// the platform does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Returned a usable snapshot from the universe scan.
    Visible,
    /// Reached the selection-input set (price / gap / volume / float known).
    SelectionInput,
    /// Selected by the movers path.
    Qualified,
    /// Selected by the quiet-watch path.
    QuietSelected,
    /// Produced at least one detector event.
    DetectorProduced,
    /// Appeared in at least one Opportunity Intelligence ranking window.
    OpportunityRanked,
}

impl Stage {
    /// Causal depth. Equal depth means "parallel", not "tied".
    pub fn depth(self) -> u8 {
        match self {
            Stage::Visible => 0,
            Stage::SelectionInput => 1,
            Stage::Qualified | Stage::QuietSelected => 2,
            Stage::DetectorProduced => 3,
            Stage::OpportunityRanked => 4,
        }
    }

    pub const ALL: [Stage; 6] = [
        Stage::Visible,
        Stage::SelectionInput,
        Stage::Qualified,
        Stage::QuietSelected,
        Stage::DetectorProduced,
        Stage::OpportunityRanked,
    ];
}

/// Why an attribution could not be made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// No stage has any evidence at all. The symbol may never have existed in
    /// this session, or capture may simply not have covered it.
    NoEvidence,
    /// Visibility itself is unevidenced, so no depth statement is meaningful.
    VisibilityUnknown,
    /// A deeper stage is evidenced while a shallower one is evidenced absent.
    ///
    /// This is a *capture* contradiction, not a market fact, and it is
    /// surfaced rather than resolved: silently trusting the deeper stage would
    /// hide an incomplete discovery log, and silently trusting the shallower
    /// one would erase real detector output.
    Inconsistent {
        /// The first stage at the contradicting depth. Where that depth has
        /// parallel alternatives, *all* of them were observed absent -- naming
        /// one of them is shorthand for "nothing at this depth".
        shallower: Stage,
        deeper: Stage,
    },
}

/// What the evidence supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Attribution {
    /// Furthest stage positively observed.
    Reached { stage: Stage },
    /// Requested from the universe scan and returned **without** a snapshot.
    ///
    /// Genuinely different from `Unknown`: the scan ran, this symbol was
    /// asked for, and no data came back. That is an observation about data
    /// availability, and it is the reason the reduced discovery log carries
    /// explicit `nosnap` rows instead of just omitting the symbol.
    NotVisible,
    /// Visible, and positively observed absent from every later stage.
    StoppedAtVisibility,
    Unknown { reason: UnknownReason },
}

/// Per-symbol, per-session stage evidence, assembled by the offline join.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolEvidence {
    pub symbol: String,
    pub session_date: String,
    /// Absent key == `None` == no evidence. Present-and-false == observed
    /// absent. See the module header on why these cannot be merged.
    #[serde(default)]
    pub stages: BTreeMap<String, bool>,
    /// Requested from the scan but returned without a snapshot.
    #[serde(default)]
    pub nosnap_events: u64,
    /// Detector events seen, by detector name. Retained rather than reduced to
    /// a boolean so "one stray event" and "sustained output" stay separable.
    #[serde(default)]
    pub detector_events: BTreeMap<String, u64>,
    /// Ranking windows this symbol appeared in.
    #[serde(default)]
    pub ranking_windows: u64,
}

impl SymbolEvidence {
    pub fn new(symbol: &str, session_date: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            session_date: session_date.to_string(),
            ..Default::default()
        }
    }

    fn key(stage: Stage) -> &'static str {
        match stage {
            Stage::Visible => "visible",
            Stage::SelectionInput => "selectionInput",
            Stage::Qualified => "qualified",
            Stage::QuietSelected => "quietSelected",
            Stage::DetectorProduced => "detectorProduced",
            Stage::OpportunityRanked => "opportunityRanked",
        }
    }

    pub fn observe(&mut self, stage: Stage, present: bool) -> &mut Self {
        // Once something is positively observed present, a later "absent"
        // observation from a different scan must not erase it -- a symbol can
        // legitimately leave a selection set it was previously in.
        let entry = self.stages.entry(Self::key(stage).to_string()).or_insert(present);
        *entry = *entry || present;
        self
    }

    pub fn get(&self, stage: Stage) -> Option<bool> {
        self.stages.get(Self::key(stage)).copied()
    }
}

/// One attributed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributedSymbol {
    pub schema_version: u32,
    pub symbol: String,
    pub session_date: String,
    pub attribution: Attribution,
    /// Every stage's evidence, retained so the verdict is checkable rather
    /// than trusted.
    pub stages: BTreeMap<String, Option<bool>>,
    pub nosnap_events: u64,
    pub detector_events: BTreeMap<String, u64>,
    pub ranking_windows: u64,
}

/// Classifies one symbol's evidence.
pub fn attribute(evidence: &SymbolEvidence) -> AttributedSymbol {
    let stages: BTreeMap<String, Option<bool>> = Stage::ALL
        .iter()
        .map(|s| (SymbolEvidence::key(*s).to_string(), evidence.get(*s)))
        .collect();

    let attribution = classify(evidence);

    AttributedSymbol {
        schema_version: ATTRIBUTION_SCHEMA_VERSION,
        symbol: evidence.symbol.clone(),
        session_date: evidence.session_date.clone(),
        attribution,
        stages,
        nosnap_events: evidence.nosnap_events,
        detector_events: evidence.detector_events.clone(),
        ranking_windows: evidence.ranking_windows,
    }
}

fn classify(evidence: &SymbolEvidence) -> Attribution {
    let observed: Vec<Stage> =
        Stage::ALL.iter().copied().filter(|s| evidence.get(*s) == Some(true)).collect();

    // A deeper stage standing on an explicitly-absent shallower one is a
    // capture contradiction. Checked before anything else, because every
    // verdict below would otherwise be built on it.
    if let Some(deepest) = observed.iter().copied().max_by_key(|s| s.depth()) {
        // Checked per *depth*, not per stage, and a depth contradicts only
        // when EVERY alternative at it was observed absent.
        //
        // Per-stage was wrong and a real run caught it: a symbol that
        // qualified via the movers path and then produced detector events has
        // `quietSelected == Some(false)`, which is not a contradiction of
        // anything -- it never needed the quiet-watch path. Requiring every
        // parallel alternative to be present made ordinary symbols read as
        // capture failures.
        for depth in 0..deepest.depth() {
            let at_depth: Vec<Stage> =
                Stage::ALL.iter().copied().filter(|s| s.depth() == depth).collect();
            if at_depth.iter().all(|s| evidence.get(*s) == Some(false)) {
                return Attribution::Unknown {
                    reason: UnknownReason::Inconsistent {
                        shallower: at_depth[0],
                        deeper: deepest,
                    },
                };
            }
        }
        // "Visible and definitively went no further" is a different claim
        // from "visible, and we cannot say whether it went further". Only the
        // first supports a statement about the funnel narrowing.
        if deepest == Stage::Visible
            && Stage::ALL
                .iter()
                .copied()
                .filter(|s| s.depth() > 0)
                .all(|s| evidence.get(s) == Some(false))
        {
            return Attribution::StoppedAtVisibility;
        }
        return Attribution::Reached { stage: deepest };
    }

    // Nothing positively observed. Distinguish the three ways that happens.
    if evidence.get(Stage::Visible) == Some(false) {
        // The scan ran and returned nothing for this symbol. `nosnap_events`
        // is what makes this an observation rather than an inference.
        return if evidence.nosnap_events > 0 {
            Attribution::NotVisible
        } else {
            Attribution::Unknown { reason: UnknownReason::VisibilityUnknown }
        };
    }
    if evidence.stages.is_empty() {
        return Attribution::Unknown { reason: UnknownReason::NoEvidence };
    }
    Attribution::Unknown { reason: UnknownReason::VisibilityUnknown }
}

/// Attributes a whole session, in deterministic symbol order.
pub fn attribute_all(evidence: &[SymbolEvidence]) -> Vec<AttributedSymbol> {
    let mut out: Vec<AttributedSymbol> = evidence.iter().map(attribute).collect();
    out.sort_by(|a, b| {
        (a.session_date.as_str(), a.symbol.as_str())
            .cmp(&(b.session_date.as_str(), b.symbol.as_str()))
    });
    out
}

/// Count of each verdict, for a completeness statement about the join itself.
///
/// Deliberately the only aggregate in this module: it describes *coverage of
/// the evidence*, not performance of the platform.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionCoverage {
    pub symbols: usize,
    pub reached: BTreeMap<String, usize>,
    pub not_visible: usize,
    pub stopped_at_visibility: usize,
    pub unknown_no_evidence: usize,
    pub unknown_visibility: usize,
    pub unknown_inconsistent: usize,
}

pub fn coverage(attributed: &[AttributedSymbol]) -> AttributionCoverage {
    let mut c = AttributionCoverage { symbols: attributed.len(), ..Default::default() };
    for a in attributed {
        match &a.attribution {
            Attribution::Reached { stage } => {
                *c.reached.entry(format!("{stage:?}")).or_insert(0) += 1;
            }
            Attribution::NotVisible => c.not_visible += 1,
            Attribution::StoppedAtVisibility => c.stopped_at_visibility += 1,
            Attribution::Unknown { reason } => match reason {
                UnknownReason::NoEvidence => c.unknown_no_evidence += 1,
                UnknownReason::VisibilityUnknown => c.unknown_visibility += 1,
                UnknownReason::Inconsistent { .. } => c.unknown_inconsistent += 1,
            },
        }
    }
    c
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod tests;
