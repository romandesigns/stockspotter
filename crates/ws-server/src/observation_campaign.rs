//! Offline Step-4 campaign controller and the **outcome firewall**.
//!
//! The campaign is an append-only event log; its state is whatever replaying
//! the log produces, so a ledger cannot claim a state its events do not earn.
//!
//! The firewall is mechanical, not procedural: every code path that fetches,
//! loads or evaluates real outcome evidence takes an [`OutcomeAccess`], and the
//! only way to obtain one outside tests is [`Campaign::outcome_access`], which
//! refuses until the capture set is closed **and** an explicit authorization
//! event has been recorded. Qualification is decided from measurement evidence
//! alone (certificate, floors, discriminating windows, OI loss), so nothing
//! about outcomes can influence which sessions are in the set.
//!
//! Outcome-evidence completeness cannot be part of capture qualification
//! without looking at outcomes before closure; it is judged after unlock,
//! per session, by the censoring rule (a session over the LIMITED threshold
//! is reported, and the minimum-sessions rule makes the primary INDETERMINATE
//! if too few remain). That ordering is a freeze decision, recorded as such.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignRules {
    pub max_designated: usize,
    pub qualifying_target: usize,
    pub min_discriminating_windows: u64,
}

impl Default for CampaignRules {
    /// The provisionally accepted values (GPT, 2026-09-28): not frozen.
    fn default() -> Self {
        Self { max_designated: 20, qualifying_target: 10, min_discriminating_windows: 20 }
    }
}

/// Measurement-only qualification of one captured session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureQualification {
    /// Market session, `YYYY-MM-DD`.
    pub session: String,
    pub run_id: String,
    pub certificate_pass: bool,
    pub floors_satisfied: bool,
    pub discriminating_windows: u64,
    pub oi_zero_loss: bool,
}

impl CaptureQualification {
    pub fn qualifies(&self, rules: &CampaignRules) -> bool {
        self.certificate_pass
            && self.floors_satisfied
            && self.oi_zero_loss
            && self.discriminating_windows >= rules.min_discriminating_windows
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CampaignEvent {
    Designated { session: String },
    CaptureRecorded { qualification: CaptureQualification },
    OutcomeFetchAuthorized { authorized_by: String, reference: String, at: DateTime<Utc> },
    OutcomeEvidenceVerified { session: String, archive_manifest_sha256: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CampaignState {
    Collecting,
    CaptureSetClosed,
    OutcomeFetchAuthorized,
    AnalysisReady,
    MeasurementInsufficient,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignError {
    /// The event is not permitted in the current state.
    NotPermitted { state: CampaignState, event: &'static str },
    AlreadyDesignated(String),
    DesignationCapReached,
    /// A designated session's capture has not been recorded yet.
    PendingCapture(String),
    NotDesignated(String),
    AlreadyRecorded(String),
    NotQualifying(String),
    EmptyAuthorization,
    BadManifestSha,
    /// Outcomes are locked in this state.
    OutcomesLocked(CampaignState),
}

/// Proof that outcome evidence may be touched, for exactly the closed set of
/// qualifying sessions. Not constructible outside this module (tests aside).
#[derive(Debug, Clone)]
pub struct OutcomeAccess {
    sessions: BTreeSet<String>,
    _sealed: (),
}

impl OutcomeAccess {
    pub fn allows(&self, session: &str) -> bool {
        self.sessions.contains(session)
    }

    pub fn sessions(&self) -> &BTreeSet<String> {
        &self.sessions
    }

    /// Synthetic-fixture tests only.
    #[cfg(test)]
    pub(crate) fn synthetic_for_tests(sessions: &[&str]) -> Self {
        Self { sessions: sessions.iter().map(|s| s.to_string()).collect(), _sealed: () }
    }
}

#[derive(Debug, Clone)]
pub struct Campaign {
    rules: CampaignRules,
    events: Vec<CampaignEvent>,
    state: CampaignState,
    designated: Vec<String>,
    recorded: BTreeMap<String, CaptureQualification>,
    qualifying: Vec<String>,
    verified: BTreeSet<String>,
}

impl Campaign {
    pub fn new(rules: CampaignRules) -> Self {
        Self {
            rules,
            events: Vec::new(),
            state: CampaignState::Collecting,
            designated: Vec::new(),
            recorded: BTreeMap::new(),
            qualifying: Vec::new(),
            verified: BTreeSet::new(),
        }
    }

    /// Rebuilds a campaign from its ledger; any event that would not have
    /// been accepted live is refused here too.
    pub fn replay(rules: CampaignRules, events: &[CampaignEvent]) -> Result<Self, CampaignError> {
        let mut c = Self::new(rules);
        for e in events {
            c.apply(e.clone())?;
        }
        Ok(c)
    }

    pub fn state(&self) -> CampaignState {
        self.state
    }

    pub fn events(&self) -> &[CampaignEvent] {
        &self.events
    }

    pub fn designated(&self) -> &[String] {
        &self.designated
    }

    pub fn qualifying_sessions(&self) -> &[String] {
        &self.qualifying
    }

    fn refuse(&self, event: &'static str) -> CampaignError {
        CampaignError::NotPermitted { state: self.state, event }
    }

    pub fn apply(&mut self, event: CampaignEvent) -> Result<(), CampaignError> {
        match &event {
            CampaignEvent::Designated { session } => {
                if self.state != CampaignState::Collecting {
                    return Err(self.refuse("designated"));
                }
                if self.designated.contains(session) {
                    return Err(CampaignError::AlreadyDesignated(session.clone()));
                }
                if let Some(p) = self.designated.iter().find(|s| !self.recorded.contains_key(*s)) {
                    return Err(CampaignError::PendingCapture(p.clone()));
                }
                if self.designated.len() >= self.rules.max_designated {
                    return Err(CampaignError::DesignationCapReached);
                }
                self.designated.push(session.clone());
            }
            CampaignEvent::CaptureRecorded { qualification: q } => {
                if self.state != CampaignState::Collecting {
                    return Err(self.refuse("captureRecorded"));
                }
                if !self.designated.contains(&q.session) {
                    return Err(CampaignError::NotDesignated(q.session.clone()));
                }
                if self.recorded.contains_key(&q.session) {
                    return Err(CampaignError::AlreadyRecorded(q.session.clone()));
                }
                self.recorded.insert(q.session.clone(), q.clone());
                if q.qualifies(&self.rules) {
                    self.qualifying.push(q.session.clone());
                }
                if self.qualifying.len() >= self.rules.qualifying_target {
                    self.state = CampaignState::CaptureSetClosed;
                } else if self.recorded.len() >= self.rules.max_designated {
                    self.state = CampaignState::MeasurementInsufficient;
                }
            }
            CampaignEvent::OutcomeFetchAuthorized { authorized_by, reference, .. } => {
                if self.state != CampaignState::CaptureSetClosed {
                    return Err(self.refuse("outcomeFetchAuthorized"));
                }
                if authorized_by.trim().is_empty() || reference.trim().is_empty() {
                    return Err(CampaignError::EmptyAuthorization);
                }
                self.state = CampaignState::OutcomeFetchAuthorized;
            }
            CampaignEvent::OutcomeEvidenceVerified { session, archive_manifest_sha256 } => {
                if self.state != CampaignState::OutcomeFetchAuthorized {
                    return Err(self.refuse("outcomeEvidenceVerified"));
                }
                if !self.qualifying.contains(session) {
                    return Err(CampaignError::NotQualifying(session.clone()));
                }
                if archive_manifest_sha256.len() != 64 || !archive_manifest_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(CampaignError::BadManifestSha);
                }
                self.verified.insert(session.clone());
                if self.qualifying.iter().all(|s| self.verified.contains(s)) {
                    self.state = CampaignState::AnalysisReady;
                }
            }
        }
        self.events.push(event);
        Ok(())
    }

    /// The only production source of [`OutcomeAccess`].
    pub fn outcome_access(&self) -> Result<OutcomeAccess, CampaignError> {
        match self.state {
            CampaignState::OutcomeFetchAuthorized | CampaignState::AnalysisReady => {
                Ok(OutcomeAccess { sessions: self.qualifying.iter().cloned().collect(), _sealed: () })
            }
            other => Err(CampaignError::OutcomesLocked(other)),
        }
    }
}
