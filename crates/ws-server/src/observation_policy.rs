//! Versioned, SHA-bound classification policies for outcome evidence:
//! trade conditions (per tape) and trading statuses (per tape family).
//!
//! Both are **data, not code**. The classification is an artifact whose RFC
//! 8785 canonical SHA-256 the preregistration binds, so the evaluator applies
//! exactly the table that was frozen and a later edit is visible as an
//! identity change rather than a silent behaviour change. Both were derived
//! from static metadata and documented semantics (Alpaca's condition-metadata
//! endpoint and its trading-status documentation), never from outcomes.
//!
//! Two properties are deliberate:
//!
//! * **Per tape.** The CTA (tapes A/B) and UTP (tape C) code spaces overlap
//!   with different meanings: `B` is *Average Price Trade* on CTA and *Bunched
//!   Trade* on UTP; `E` is *Automatic Execution* on CTA and a placeholder on
//!   UTP. A single canonical table would silently misclassify one of them.
//! * **Unknown is uncertain, never a guess.** An unlisted code, an unknown
//!   tape or an empty condition list is `Uncertain`, which the evaluator turns
//!   into censoring where it could matter, never into failure or success.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::prereg;

pub const CONDITION_POLICY_SCHEMA: &str = "trade-condition-policy-v2";
pub const STATUS_POLICY_SCHEMA: &str = "trading-status-policy-v1";

fn identity(v: &Value) -> Result<String, String> {
    let canonical = prereg::canonicalize(v).map_err(|e| e.to_string())?;
    Ok(prereg::sha256_hex(&canonical))
}

fn strings(v: &Value, what: &str) -> Result<Vec<String>, String> {
    v.as_array()
        .ok_or(format!("{what}: not an array"))?
        .iter()
        .map(|x| x.as_str().map(str::to_string).ok_or(format!("{what}: non-string entry")))
        .collect()
}

// ===========================================================================
// Trade conditions
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionClass {
    /// A market-priced execution: counts toward the target.
    Include,
    /// Not a market-priced execution at that instant: the print is ignored.
    Exclude,
    /// Semantics insufficiently determined: the print is uncertain.
    Censor,
}

/// What one print means for the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintVerdict {
    Counts,
    Ignored,
    Uncertain,
}

#[derive(Debug, Clone)]
pub struct ConditionPolicy {
    pub version: String,
    /// SHA-256 of the artifact's RFC 8785 canonical bytes.
    pub sha256: String,
    /// tape -> code -> class.
    pub tapes: BTreeMap<String, BTreeMap<String, ConditionClass>>,
}

impl ConditionPolicy {
    /// Loads and identifies a policy artifact. Every code of a tape is
    /// classified exactly once across `include` / `exclude` / `censor`.
    pub fn bind(bytes: &[u8]) -> Result<Self, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if v["schema"] != CONDITION_POLICY_SCHEMA {
            return Err(format!("schema is not {CONDITION_POLICY_SCHEMA}"));
        }
        let tapes_v = v["tapes"].as_object().ok_or("tapes missing")?;
        if tapes_v.is_empty() {
            return Err("no tape is classified".into());
        }
        let mut tapes = BTreeMap::new();
        for (tape, t) in tapes_v {
            let mut codes = BTreeMap::new();
            for (key, class) in
                [("include", ConditionClass::Include), ("exclude", ConditionClass::Exclude), ("censor", ConditionClass::Censor)]
            {
                for code in strings(&t[key], &format!("tapes.{tape}.{key}"))? {
                    if codes.insert(code.clone(), class).is_some() {
                        return Err(format!("tape {tape}: code {code:?} classified more than once"));
                    }
                }
            }
            tapes.insert(tape.clone(), codes);
        }
        Ok(Self { version: v["version"].as_str().unwrap_or_default().to_string(), sha256: identity(&v)?, tapes })
    }

    pub fn class_of(&self, tape: &str, code: &str) -> Option<ConditionClass> {
        self.tapes.get(tape)?.get(code).copied()
    }

    /// The combination rule, in order:
    ///
    /// 1. any `Exclude` code -> `Ignored`: the print is not a market-priced
    ///    execution whatever else it carries;
    /// 2. an unknown tape, an empty condition list, or any `Censor` or
    ///    unlisted code -> `Uncertain`;
    /// 3. otherwise (every code `Include`) -> `Counts`.
    pub fn classify(&self, tape: &str, conditions: &[String]) -> PrintVerdict {
        let Some(table) = self.tapes.get(tape) else {
            return PrintVerdict::Uncertain;
        };
        let classes: Vec<Option<ConditionClass>> = conditions.iter().map(|c| table.get(c).copied()).collect();
        if classes.contains(&Some(ConditionClass::Exclude)) {
            return PrintVerdict::Ignored;
        }
        if classes.is_empty() || classes.iter().any(|c| *c != Some(ConditionClass::Include)) {
            return PrintVerdict::Uncertain;
        }
        PrintVerdict::Counts
    }
}

// ===========================================================================
// Trading statuses
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// Regulatory or exchange halt: normal execution unavailable.
    Halt,
    /// Volatility (LULD) trading pause: normal execution unavailable.
    Pause,
    /// Not halted by name but not tradable either (e.g. a quotation-only
    /// period before trading resumes).
    NonTradable,
    /// Normal trading resumes.
    Resume,
    /// No effect on execution availability.
    Informational,
}

impl StatusClass {
    pub fn interrupts(self) -> bool {
        matches!(self, Self::Halt | Self::Pause | Self::NonTradable)
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "HALT" => Self::Halt,
            "PAUSE" => Self::Pause,
            "NON_TRADABLE" => Self::NonTradable,
            "RESUME" => Self::Resume,
            "INFORMATIONAL" => Self::Informational,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct StatusPolicy {
    pub version: String,
    pub sha256: String,
    /// tape -> family name.
    pub family_of_tape: BTreeMap<String, String>,
    /// family -> status code -> class.
    pub families: BTreeMap<String, BTreeMap<String, StatusClass>>,
}

impl StatusPolicy {
    /// Loads and identifies a status policy. Every tape belongs to exactly
    /// one family; every code carries exactly one known class.
    pub fn bind(bytes: &[u8]) -> Result<Self, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if v["schema"] != STATUS_POLICY_SCHEMA {
            return Err(format!("schema is not {STATUS_POLICY_SCHEMA}"));
        }
        let fam_v = v["families"].as_object().ok_or("families missing")?;
        let mut family_of_tape = BTreeMap::new();
        let mut families = BTreeMap::new();
        for (name, f) in fam_v {
            for tape in strings(&f["tapes"], &format!("families.{name}.tapes"))? {
                if let Some(other) = family_of_tape.insert(tape.clone(), name.clone()) {
                    return Err(format!("tape {tape} is in both {other} and {name}"));
                }
            }
            let codes_v = f["codes"].as_object().ok_or(format!("families.{name}.codes missing"))?;
            let mut codes = BTreeMap::new();
            for (code, entry) in codes_v {
                let class = entry["class"]
                    .as_str()
                    .and_then(StatusClass::parse)
                    .ok_or(format!("families.{name}.codes.{code}: unknown class"))?;
                codes.insert(code.clone(), class);
            }
            families.insert(name.clone(), codes);
        }
        Ok(Self {
            version: v["version"].as_str().unwrap_or_default().to_string(),
            sha256: identity(&v)?,
            family_of_tape,
            families,
        })
    }

    /// `None` = UNKNOWN: an unknown tape or an unlisted code.
    pub fn classify(&self, tape: Option<&str>, code: &str) -> Option<StatusClass> {
        let family = self.family_of_tape.get(tape?)?;
        self.families.get(family)?.get(code).copied()
    }

    pub fn tapes(&self) -> BTreeSet<&str> {
        self.family_of_tape.keys().map(String::as_str).collect()
    }
}
