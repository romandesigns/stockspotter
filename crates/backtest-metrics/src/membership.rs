//! Opportunity ↔ episode membership (Correction 2).
//!
//! # Why an identity join would have been wrong
//!
//! `OpportunityId` and `EpisodeId` share a shape — `symbol:sessionDate:sequence`
//! — and the V1 report treated that as licence to join them. It is not. The two
//! lifecycles differ on purpose, in exactly one rule:
//!
//! | Event | `EpisodeTracker` | `OpportunityIntelligence` |
//! |---|---|---|
//! | `FollowThroughRejected` | **closes** the episode | **absorbs** it, opportunity continues |
//!
//! So one opportunity can span several episodes, and the two sequence counters
//! advance at different rates the moment any invalidation occurs. After one
//! fragmented move, `AAA:2026-09-14:1` as an opportunity and as an episode
//! denote different things. Joining on the key would silently attach the wrong
//! outcome to the wrong score — and would do so *more* often precisely for the
//! fragmented, invalidation-heavy moves this whole layer exists to study.
//!
//! # What is used instead
//!
//! Causal fields only: symbol, session date, and the two open/close intervals.
//! No outcome, no forward price, nothing observed after the episode opened
//! participates in deciding membership — so membership cannot be contaminated
//! by what happened next.
//!
//! # Ambiguity is a result, not a rounding error
//!
//! Where the intervals cannot settle membership the answer is
//! [`AmbiguityReason`], never a best guess. An episode that outlives its
//! candidate opportunity, or that opens at an instant two opportunities both
//! claim, is reported and left unassigned.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::episode::OpportunityEpisode;
use crate::opportunity::Opportunity;

pub const MEMBERSHIP_SCHEMA_VERSION: u32 = 1;
pub const MEMBERSHIP_RULES_VERSION: &str = "membership-v1-temporal-containment";

/// Why an episode could not be attributed to a single opportunity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AmbiguityReason {
    /// The episode's opening instant falls inside more than one opportunity
    /// window. The engine keeps at most one opportunity open per symbol, so
    /// this normally means two windows touch at a boundary instant, or the
    /// inputs came from different runs.
    MultipleCandidates { candidates: Vec<String> },
    /// The episode was still open after its candidate opportunity's window
    /// ended, so part of its life — and therefore part of whatever outcome is
    /// measured from it — belongs to something else.
    EpisodeOutlivesOpportunity {
        opportunity_id: String,
        episode_closed_at: DateTime<Utc>,
        window_end: DateTime<Utc>,
    },
}

/// Why an episode matched no opportunity at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum UnassignedReason {
    /// No opportunity exists for this symbol and session.
    NoOpportunityForSymbolSession,
    /// Opportunities exist, but the episode opened outside every window.
    OutsideEveryWindow,
}

/// How completely an opportunity's episode membership could be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipStatus {
    /// Every episode that pointed at this opportunity resolved cleanly.
    Resolved,
    /// At least one episode in this window could not be attributed.
    PartiallyAmbiguous,
    /// No episode resolved to this opportunity.
    NoEpisodes,
}

/// The explicit `opportunityId -> episodeIds[]` relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityMembership {
    pub opportunity_id: String,
    pub symbol: String,
    pub session_date: String,
    pub opened_at: DateTime<Utc>,
    /// `closed_at` where the opportunity closed; otherwise `last_seen_at`.
    pub window_end: DateTime<Utc>,
    /// True when `window_end` is `last_seen_at` rather than a real close, so
    /// the tail of the window is unknown and later episodes may be missing
    /// from `member_episode_ids`. Under-assignment, never mis-assignment.
    pub opportunity_open: bool,
    /// Resolved members, sorted. This is the relationship Correction 2 asks
    /// to be persisted or derivable.
    pub member_episode_ids: Vec<String>,
    /// Episodes inside this window that could not be attributed, sorted.
    pub ambiguous_episode_ids: Vec<String>,
    /// The opportunity's own count of episodes it believes it spans, carried
    /// alongside the resolved count so the two can be compared without
    /// re-deriving either.
    pub episode_fragments_claimed: u32,
    pub status: MembershipStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbiguousEpisode {
    pub episode_id: String,
    pub symbol: String,
    pub session_date: String,
    pub opened_at: DateTime<Utc>,
    #[serde(flatten)]
    pub ambiguity: AmbiguityReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnassignedEpisode {
    pub episode_id: String,
    pub symbol: String,
    pub session_date: String,
    pub opened_at: DateTime<Utc>,
    #[serde(flatten)]
    pub unassigned: UnassignedReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MembershipReport {
    pub schema_version: u32,
    pub rules_version: String,
    pub mappings: Vec<OpportunityMembership>,
    pub ambiguous_episodes: Vec<AmbiguousEpisode>,
    pub unassigned_episodes: Vec<UnassignedEpisode>,
}

impl MembershipReport {
    /// Resolved members for one opportunity key.
    pub fn members_of(&self, opportunity_id: &str) -> &[String] {
        self.mappings
            .iter()
            .find(|m| m.opportunity_id == opportunity_id)
            .map(|m| m.member_episode_ids.as_slice())
            .unwrap_or(&[])
    }

    /// The opportunity a given episode resolved to, if any.
    pub fn opportunity_of(&self, episode_id: &str) -> Option<&str> {
        self.mappings
            .iter()
            .find(|m| m.member_episode_ids.iter().any(|e| e == episode_id))
            .map(|m| m.opportunity_id.as_str())
    }
}

/// The end of an opportunity's window, and whether it is a real close.
fn window_end(op: &Opportunity) -> (DateTime<Utc>, bool) {
    match op.closed_at {
        Some(closed) => (closed, false),
        // Still open: `last_seen_at` is a real observation and a sound lower
        // bound on the end, but the tail is unknown. Nothing after it is
        // claimed.
        None => (op.last_seen_at, true),
    }
}

/// Builds the membership mapping.
///
/// Deterministic: inputs are keyed and sorted before use, so neither the order
/// the opportunities arrive in nor the order the episodes do can change the
/// output.
pub fn map_memberships(
    opportunities: &[Opportunity],
    episodes: &[OpportunityEpisode],
) -> MembershipReport {
    // Grouped by (session_date, symbol) — the two fields that must match
    // before any temporal reasoning is even meaningful.
    let mut groups: BTreeMap<(String, String), Vec<&Opportunity>> = BTreeMap::new();
    for op in opportunities {
        groups
            .entry((op.session_date.clone(), op.symbol.clone()))
            .or_default()
            .push(op);
    }
    for ops in groups.values_mut() {
        ops.sort_by(|a, b| (a.opened_at, a.id.sequence).cmp(&(b.opened_at, b.id.sequence)));
    }

    let mut members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ambiguous_per_op: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ambiguous_episodes: Vec<AmbiguousEpisode> = Vec::new();
    let mut unassigned_episodes: Vec<UnassignedEpisode> = Vec::new();

    let mut ordered: Vec<&OpportunityEpisode> = episodes.iter().collect();
    ordered.sort_by(|a, b| {
        (&a.id.session_date, &a.id.symbol, a.opened_at, a.id.sequence).cmp(&(
            &b.id.session_date,
            &b.id.symbol,
            b.opened_at,
            b.id.sequence,
        ))
    });

    for ep in ordered {
        let key = (ep.id.session_date.clone(), ep.id.symbol.clone());
        let ep_key = ep.id.as_key();

        // Rules 1 and 2: a different symbol or a different session can never
        // join. Not a near miss — not applicable.
        let Some(candidates_all) = groups.get(&key) else {
            unassigned_episodes.push(UnassignedEpisode {
                episode_id: ep_key,
                symbol: ep.id.symbol.clone(),
                session_date: ep.id.session_date.clone(),
                opened_at: ep.opened_at,
                unassigned: UnassignedReason::NoOpportunityForSymbolSession,
            });
            continue;
        };

        // Rule 3: membership is decided by where the episode *opened*. An
        // episode that opened before the opportunity existed cannot be part of
        // it, whatever else overlaps.
        let candidates: Vec<&&Opportunity> = candidates_all
            .iter()
            .filter(|op| {
                let (end, _) = window_end(op);
                ep.opened_at >= op.opened_at && ep.opened_at <= end
            })
            .collect();

        match candidates.len() {
            0 => unassigned_episodes.push(UnassignedEpisode {
                episode_id: ep_key,
                symbol: ep.id.symbol.clone(),
                session_date: ep.id.session_date.clone(),
                opened_at: ep.opened_at,
                unassigned: UnassignedReason::OutsideEveryWindow,
            }),
            1 => {
                let op = candidates[0];
                let (end, _) = window_end(op);
                let op_key = op.id.as_key();
                match ep.closed_at {
                    // Straddles the boundary: part of this episode's life, and
                    // therefore part of any outcome measured from it, happened
                    // after this opportunity ended. Not silently assigned.
                    Some(closed) if closed > end => {
                        ambiguous_per_op.entry(op_key.clone()).or_default().push(ep_key.clone());
                        ambiguous_episodes.push(AmbiguousEpisode {
                            episode_id: ep_key,
                            symbol: ep.id.symbol.clone(),
                            session_date: ep.id.session_date.clone(),
                            opened_at: ep.opened_at,
                            ambiguity: AmbiguityReason::EpisodeOutlivesOpportunity {
                                opportunity_id: op_key,
                                episode_closed_at: closed,
                                window_end: end,
                            },
                        });
                    }
                    _ => members.entry(op_key).or_default().push(ep_key),
                }
            }
            _ => {
                let names: Vec<String> = candidates.iter().map(|op| op.id.as_key()).collect();
                for name in &names {
                    ambiguous_per_op.entry(name.clone()).or_default().push(ep_key.clone());
                }
                ambiguous_episodes.push(AmbiguousEpisode {
                    episode_id: ep_key,
                    symbol: ep.id.symbol.clone(),
                    session_date: ep.id.session_date.clone(),
                    opened_at: ep.opened_at,
                    ambiguity: AmbiguityReason::MultipleCandidates { candidates: names },
                });
            }
        }
    }

    let mut mappings: Vec<OpportunityMembership> = opportunities
        .iter()
        .map(|op| {
            let key = op.id.as_key();
            let (end, still_open) = window_end(op);
            let mut member_episode_ids = members.get(&key).cloned().unwrap_or_default();
            member_episode_ids.sort();
            let mut ambiguous_episode_ids =
                ambiguous_per_op.get(&key).cloned().unwrap_or_default();
            ambiguous_episode_ids.sort();
            ambiguous_episode_ids.dedup();
            let status = if !ambiguous_episode_ids.is_empty() {
                MembershipStatus::PartiallyAmbiguous
            } else if member_episode_ids.is_empty() {
                MembershipStatus::NoEpisodes
            } else {
                MembershipStatus::Resolved
            };
            OpportunityMembership {
                opportunity_id: key,
                symbol: op.symbol.clone(),
                session_date: op.session_date.clone(),
                opened_at: op.opened_at,
                window_end: end,
                opportunity_open: still_open,
                member_episode_ids,
                ambiguous_episode_ids,
                episode_fragments_claimed: op.episode_fragments,
                status,
            }
        })
        .collect();

    mappings.sort_by(|a, b| {
        (&a.session_date, &a.symbol, &a.opportunity_id).cmp(&(
            &b.session_date,
            &b.symbol,
            &b.opportunity_id,
        ))
    });
    ambiguous_episodes.sort_by(|a, b| a.episode_id.cmp(&b.episode_id));
    unassigned_episodes.sort_by(|a, b| a.episode_id.cmp(&b.episode_id));

    MembershipReport {
        schema_version: MEMBERSHIP_SCHEMA_VERSION,
        rules_version: MEMBERSHIP_RULES_VERSION.to_string(),
        mappings,
        ambiguous_episodes,
        unassigned_episodes,
    }
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
