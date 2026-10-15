//! Small serde helpers for a real Alpaca API quirk: bars endpoints
//! return `"bars": null` (not `[]`/an empty object) for a window with
//! zero bars -- confirmed live 2026-09-01 via a direct curl during a
//! quiet post-close period (`{"bars":null,"next_page_token":null,
//! "symbol":"SWVL"}` for a real, genuinely-tracked symbol with zero
//! trades in the requested window). Plain `Vec<T>`/`HashMap<String,
//! Vec<T>>` fields reject an explicit `null` outright ("invalid type:
//! null, expected a sequence"), which turned a real, benign "no data
//! right now" case into a hard 502 for the whole request -- including
//! blocking the Super Chart's live backfill for any symbol going through
//! a quiet stretch, found while verifying an unrelated feature (catalyst
//! badges' click-through-to-chart). Use via `#[serde(default,
//! deserialize_with = "...")]` on any field that comes straight off an
//! Alpaca bars response.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

pub fn null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

/// Same issue, one level deeper: a multi-symbol response's per-symbol
/// entry can itself be `null` (that one symbol had zero bars in the
/// window) even when the outer map is present.
pub fn null_values_as_empty_vecs<'de, D, T>(deserializer: D) -> Result<HashMap<String, Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let raw: HashMap<String, Option<Vec<T>>> = HashMap::deserialize(deserializer)?;
    Ok(raw.into_iter().map(|(k, v)| (k, v.unwrap_or_default())).collect())
}

/// Anything that is not a `u64`, treated as absent instead of as a parse
/// failure.
///
/// For a field that is *observational only*. A malformed value in one of
/// those must never reject the record that carries it: the trade it is
/// attached to is real market data on the detector's input path, and
/// dropping it to protect a piece of metadata nothing classifies on would
/// trade a signal for a label. A negative, fractional, string, object or
/// array id therefore reads as "no id", which is exactly what it is --
/// and no value is coerced or rounded into one, so a genuine id is either
/// preserved exactly or reported as absent.
///
/// Do NOT use this on a field anything depends on. There, a malformed
/// value should fail loudly.
pub fn lenient_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MaybeU64 {
        // Tried first: an exact u64 round-trips with no precision loss,
        // which an f64 hop through would not for ids near u64::MAX.
        Exact(u64),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<MaybeU64>::deserialize(deserializer)? {
        Some(MaybeU64::Exact(value)) => Some(value),
        Some(MaybeU64::Other(_)) | None => None,
    })
}
