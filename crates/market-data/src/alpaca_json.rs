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
