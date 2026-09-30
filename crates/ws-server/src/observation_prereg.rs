//! Step-4 preregistration binding: `step4-preregistration-v1`.
//!
//! A preregistration is only worth anything if the thing that ran can be shown
//! to be the thing that was registered. This module makes that mechanical:
//!
//! * the artifact is JSON, identified by the **SHA-256 of its RFC 8785 (JCS)
//!   canonical bytes**, so formatting, key order and whitespace cannot change
//!   its identity while any change of content does;
//! * the observer refuses to start if the artifact's operational constants
//!   (freshness, budgets, queue, overhead limits, rotation) differ from the
//!   ones this build enforces -- a preregistration of an experiment the build
//!   would not run is rejected at startup, not discovered afterwards;
//! * `run_start` records the artifact's SHA-256 and the build commit, and a
//!   bound certificate (`assess_bound`) requires both to match.
//!
//! **Canonical subset.** JCS in full needs ECMAScript number formatting for
//! floats. This schema admits **integers only** -- floors are basis points,
//! durations are integer milliseconds or microseconds -- and ASCII keys only,
//! so canonicalisation is exact without a float formatter: objects with keys
//! sorted (byte order equals UTF-16 order for ASCII), no whitespace, strings
//! escaped exactly as JCS requires. A float anywhere is refused rather than
//! canonicalised approximately.

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{OverheadLimits, FRESHNESS_MAX_AGE_NANOS, PROTOCOL_VERSION};

pub const PREREG_SCHEMA: &str = "step4-preregistration-v1";

#[derive(Debug, PartialEq)]
pub enum PreregBindingError {
    Io(String),
    Parse(String),
    /// A number with a fractional part or exponent: not admitted.
    Float { path: String },
    NonAsciiKey { path: String },
    MissingField { path: String },
    WrongType { path: String, expected: &'static str },
    OutOfRange { path: String },
    SchemaMismatch { found: String },
    ProtocolMismatch { found: String },
    BadHex { path: String, len: usize },
    /// The artifact and this build disagree on an operational constant.
    ConstantMismatch { field: &'static str, preregistered: u64, implemented: u64 },
}

impl std::fmt::Display for PreregBindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PreregBindingError {}

/// A loaded, validated preregistration and its identity.
#[derive(Debug, Clone)]
pub struct BoundPreregistration {
    pub value: Value,
    pub canonical: Vec<u8>,
    pub sha256: String,
}

impl BoundPreregistration {
    pub fn implementation_sha(&self) -> &str {
        self.value["implementationSha"].as_str().unwrap_or_default()
    }
}

/// The constants this build enforces, as the preregistration must state them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImplementedConstants {
    pub capture_max_bytes: u64,
    pub capture_warn_permille: u64,
    pub rotate_bytes: u64,
    pub queue_records: u64,
    pub queue_bytes: u64,
    pub queue_warn_permille: u64,
    pub overhead: OverheadLimits,
}

/// RFC 8785 canonical bytes for the admitted subset.
pub fn canonicalize(value: &Value) -> Result<Vec<u8>, PreregBindingError> {
    let mut out = Vec::new();
    write_canonical(value, "$", &mut out)?;
    Ok(out)
}

fn write_canonical(value: &Value, path: &str, out: &mut Vec<u8>) -> Result<(), PreregBindingError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.extend_from_slice(i.to_string().as_bytes());
            } else if let Some(u) = n.as_u64() {
                out.extend_from_slice(u.to_string().as_bytes());
            } else {
                return Err(PreregBindingError::Float { path: path.to_string() });
            }
        }
        // serde_json's escaping is exactly JCS's for strings: the two-letter
        // escapes for \b \f \n \r \t, `\u00xx` lowercase for other controls,
        // `\"` and `\\`, and everything else -- '/' and non-ASCII included --
        // emitted literally.
        Value::String(s) => out.extend_from_slice(
            serde_json::to_string(s).map_err(|e| PreregBindingError::Parse(e.to_string()))?.as_bytes(),
        ),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, &format!("{path}[{i}]"), out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            for k in &keys {
                if !k.is_ascii() {
                    return Err(PreregBindingError::NonAsciiKey { path: format!("{path}.{k}") });
                }
            }
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(
                    serde_json::to_string(k).map_err(|e| PreregBindingError::Parse(e.to_string()))?.as_bytes(),
                );
                out.push(b':');
                write_canonical(&map[*k], &format!("{path}.{k}"), out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn field<'a>(v: &'a Value, path: &str) -> Result<&'a Value, PreregBindingError> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur.get(part).ok_or_else(|| PreregBindingError::MissingField { path: path.to_string() })?;
    }
    Ok(cur)
}

fn int(v: &Value, path: &str) -> Result<u64, PreregBindingError> {
    field(v, path)?
        .as_u64()
        .ok_or(PreregBindingError::WrongType { path: path.to_string(), expected: "non-negative integer" })
}

fn text<'a>(v: &'a Value, path: &str) -> Result<&'a str, PreregBindingError> {
    let s = field(v, path)?
        .as_str()
        .ok_or(PreregBindingError::WrongType { path: path.to_string(), expected: "string" })?;
    if s.trim().is_empty() {
        return Err(PreregBindingError::WrongType { path: path.to_string(), expected: "non-empty string" });
    }
    Ok(s)
}

fn object(v: &Value, path: &str) -> Result<(), PreregBindingError> {
    if field(v, path)?.is_object() {
        Ok(())
    } else {
        Err(PreregBindingError::WrongType { path: path.to_string(), expected: "object" })
    }
}

fn hex(v: &Value, path: &str, len: usize) -> Result<(), PreregBindingError> {
    let s = text(v, path)?;
    if s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(())
    } else {
        Err(PreregBindingError::BadHex { path: path.to_string(), len })
    }
}

/// Structural validation: every bound decision present and well-typed.
pub fn validate(v: &Value) -> Result<(), PreregBindingError> {
    if !v.is_object() {
        return Err(PreregBindingError::WrongType { path: "$".into(), expected: "object" });
    }
    let schema = text(v, "schema")?;
    if schema != PREREG_SCHEMA {
        return Err(PreregBindingError::SchemaMismatch { found: schema.to_string() });
    }
    let protocol = text(v, "protocolVersion")?;
    if protocol != PROTOCOL_VERSION {
        return Err(PreregBindingError::ProtocolMismatch { found: protocol.to_string() });
    }
    hex(v, "gateSha256", 64)?;
    hex(v, "implementationSha", 40)?;
    hex(v, "outcome.tradeConditionTableSha256", 64)?;
    hex(v, "status.policySha256", 64)?;
    text(v, "freezeStatus")?;
    for p in ["population", "membership", "rates.primary", "rates.coverage", "rates.freshness",
              "certificate.semantics", "certificate.invalidWindows", "selection.contract",
              "outcome.contract", "outcome.fetchContract", "outcome.source", "status.contract",
              "status.interruptionRule", "censoring.contract",
              "inference.contract", "export.contract"] {
        text(v, p)?;
    }
    for p in ["selection", "outcome", "inference"] {
        object(v, p)?;
    }
    for p in ["floorsBasisPoints.eligibility", "floorsBasisPoints.provenanceEstablishment"] {
        if int(v, p)? > 10_000 {
            return Err(PreregBindingError::OutOfRange { path: p.to_string() });
        }
    }
    for p in ["informativeness.discriminatingWindowsPerSession", "informativeness.qualifyingSessions",
              "informativeness.calendarCapSessions", "freshness.primaryMaxAgeMs",
              "freshness.sensitivityMaxAgeMs", "capture.maxBytes", "capture.warnPermille",
              "capture.rotateBytes", "queue.records", "queue.bytes", "queue.warnPermille",
              "overhead.warnWindowMicros", "overhead.stopWindowMicros", "overhead.warnDutyPpm",
              "overhead.stopDutyPpm", "overhead.dutyWindowSeconds", "selection.budget",
              "selection.minDiscriminatingPool", "outcome.targetBasisPoints", "outcome.horizonSeconds",
              "censoring.limitedThresholdPermille", "inference.minimumSessions",
              "inference.calendarCapSessions", "inference.alphaBasisPoints"] {
        int(v, p)?;
    }
    if int(v, "inference.minimumSessions")? != int(v, "informativeness.qualifyingSessions")?
        || int(v, "inference.calendarCapSessions")? != int(v, "informativeness.calendarCapSessions")?
    {
        return Err(PreregBindingError::OutOfRange { path: "inference.minimumSessions".into() });
    }
    for p in ["capture.warnPermille", "queue.warnPermille"] {
        if int(v, p)? > 1_000 {
            return Err(PreregBindingError::OutOfRange { path: p.to_string() });
        }
    }
    if int(v, "informativeness.qualifyingSessions")? > int(v, "informativeness.calendarCapSessions")? {
        return Err(PreregBindingError::OutOfRange { path: "informativeness.qualifyingSessions".into() });
    }
    Ok(())
}

/// Refuses a preregistration whose operational constants differ from this
/// build's. Freshness is checked against the compiled protocol constant.
pub fn check_constants(v: &Value, c: &ImplementedConstants) -> Result<(), PreregBindingError> {
    let pairs: [(&'static str, &str, u64); 17] = [
        ("selection.budget", "selection.budget", super::analysis::SelectionConfig::default().budget as u64),
        ("selection.minDiscriminatingPool", "selection.minDiscriminatingPool", super::analysis::SelectionConfig::default().min_pool as u64),
        ("outcome.targetBasisPoints", "outcome.targetBasisPoints", super::analysis::PRIMARY_TARGET_BP as u64),
        ("outcome.horizonSeconds", "outcome.horizonSeconds", super::analysis::HORIZON_SECS as u64),
        ("freshness.primaryMaxAgeMs", "freshness.primaryMaxAgeMs", (FRESHNESS_MAX_AGE_NANOS / 1_000_000) as u64),
        ("capture.maxBytes", "capture.maxBytes", c.capture_max_bytes),
        ("capture.warnPermille", "capture.warnPermille", c.capture_warn_permille),
        ("capture.rotateBytes", "capture.rotateBytes", c.rotate_bytes),
        ("queue.records", "queue.records", c.queue_records),
        ("queue.bytes", "queue.bytes", c.queue_bytes),
        ("queue.warnPermille", "queue.warnPermille", c.queue_warn_permille),
        ("overhead.warnWindowMicros", "overhead.warnWindowMicros", c.overhead.warn_window_micros),
        ("overhead.stopWindowMicros", "overhead.stopWindowMicros", c.overhead.stop_window_micros),
        ("overhead.warnDutyPpm", "overhead.warnDutyPpm", c.overhead.warn_duty_ppm),
        ("overhead.stopDutyPpm", "overhead.stopDutyPpm", c.overhead.stop_duty_ppm),
        ("overhead.dutyWindowSeconds", "overhead.dutyWindowSeconds", c.overhead.duty_window_secs),
        ("freshness.sensitivityMaxAgeMs", "freshness.sensitivityMaxAgeMs", super::SENSITIVITY_MAX_AGE_MS as u64),
    ];
    for (name, path, implemented) in pairs {
        let preregistered = int(v, path)?;
        if preregistered != implemented {
            return Err(PreregBindingError::ConstantMismatch { field: name, preregistered, implemented });
        }
    }
    // Campaign rules: the offline controller enforces exactly what is registered.
    let rules = super::campaign::CampaignRules::default();
    for (name, path, implemented) in [
        ("informativeness.discriminatingWindowsPerSession", "informativeness.discriminatingWindowsPerSession", rules.min_discriminating_windows),
        ("informativeness.qualifyingSessions", "informativeness.qualifyingSessions", rules.qualifying_target as u64),
        ("informativeness.calendarCapSessions", "informativeness.calendarCapSessions", rules.max_designated as u64),
    ] {
        let preregistered = int(v, path)?;
        if preregistered != implemented {
            return Err(PreregBindingError::ConstantMismatch { field: name, preregistered, implemented });
        }
    }
    // The outcome adapter is identified by its contract string.
    if text(v, "outcome.fetchContract")? != super::outcome::FETCH_CONTRACT {
        return Err(PreregBindingError::WrongType { path: "outcome.fetchContract".into(), expected: "this build's fetch contract" });
    }
    Ok(())
}

/// Parses, validates and identifies a preregistration from bytes.
pub fn bind(bytes: &[u8]) -> Result<BoundPreregistration, PreregBindingError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| PreregBindingError::Parse(e.to_string()))?;
    validate(&value)?;
    let canonical = canonicalize(&value)?;
    let sha256 = sha256_hex(&canonical);
    Ok(BoundPreregistration { value, canonical, sha256 })
}

pub fn load(path: &std::path::Path) -> Result<BoundPreregistration, PreregBindingError> {
    let bytes = std::fs::read(path).map_err(|e| PreregBindingError::Io(format!("{}: {e}", path.display())))?;
    bind(&bytes)
}
