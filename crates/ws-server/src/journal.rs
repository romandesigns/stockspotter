//! Trade journal (2026-10-15): `GET /journal` and `POST /journal`.
//!
//! What the trader did with a shortlisted candidate, next to what its card
//! said at the time (packages/shared-types/src/journal.ts is the client
//! side). Server-side so the desktop app, the web app and the phone share
//! one journal.
//!
//! Storage is an append-only JSONL file, like every other journal in this
//! project: a POST appends the entry as sent, and a GET folds the file so
//! the last line per `id` wins. Nothing is ever rewritten or deleted, so a
//! bad client cannot destroy history, and the file can be audited line by
//! line. Writes are serialized by one process-wide lock.

use std::collections::HashMap;
use std::sync::OnceLock;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::warn;

const PATH_ENV: &str = "TRADE_JOURNAL_PATH";
const DEFAULT_PATH: &str = "data/trade_journal.jsonl";
/// Most recent entries returned by a GET.
const MAX_RETURNED: usize = 500;
const MAX_NOTES_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    pub gap_pct: f64,
    pub relative_volume: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tradability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    pub price: f64,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub symbol: String,
    pub created_at: String,
    pub updated_at: String,
    pub entry: f64,
    pub stop: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<f64>,
    pub shares: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<Exit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn positive(n: f64) -> bool {
    n.is_finite() && n > 0.0
}

fn timestamp(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

/// Why an entry cannot be stored, or `None` if it can.
pub fn invalid(e: &Entry) -> Option<&'static str> {
    if e.id.is_empty() || e.id.len() > 64 || !e.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Some("id must be 1-64 letters, digits, '-' or '_'");
    }
    if e.symbol.is_empty() || e.symbol.len() > 12 || !e.symbol.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.') {
        return Some("symbol must be an upper-case ticker");
    }
    if !timestamp(&e.created_at) || !timestamp(&e.updated_at) {
        return Some("createdAt and updatedAt must be RFC 3339 timestamps");
    }
    if !positive(e.entry) || !positive(e.stop) || e.stop >= e.entry {
        return Some("entry and stop must be positive, with the stop below the entry");
    }
    if e.shares == 0 {
        return Some("shares must be at least 1");
    }
    if e.target.is_some_and(|t| !positive(t)) {
        return Some("target must be positive");
    }
    if let Some(x) = &e.exit {
        if !positive(x.price) || !timestamp(&x.at) {
            return Some("exit needs a positive price and an RFC 3339 time");
        }
    }
    if e.notes.as_ref().is_some_and(|n| n.chars().count() > MAX_NOTES_CHARS) {
        return Some("notes are limited to 2000 characters");
    }
    None
}

/// Last version of each entry, newest first, from the file's lines.
/// Unparseable lines are skipped (and logged) rather than failing the read.
pub fn fold(text: &str) -> Vec<Entry> {
    let mut latest: HashMap<String, (usize, Entry)> = HashMap::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(line) {
            Ok(e) => {
                latest.insert(e.id.clone(), (line_no, e));
            }
            Err(err) => warn!(line = line_no + 1, error = %err, "journal: skipped an unreadable line"),
        }
    }
    let mut out: Vec<(usize, Entry)> = latest.into_values().collect();
    out.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at).then(b.0.cmp(&a.0)));
    out.into_iter().take(MAX_RETURNED).map(|(_, e)| e).collect()
}

fn path() -> String {
    std::env::var(PATH_ENV).unwrap_or_else(|_| DEFAULT_PATH.into())
}

fn write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub async fn get_journal() -> impl IntoResponse {
    match tokio::fs::read_to_string(path()).await {
        Ok(text) => Json(fold(&text)).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Json(Vec::<Entry>::new()).into_response(),
        Err(e) => {
            warn!(error = %e, "journal: read failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "journal unavailable").into_response()
        }
    }
}

pub async fn post_journal(Json(entry): Json<Entry>) -> impl IntoResponse {
    if let Some(why) = invalid(&entry) {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    let line = match serde_json::to_string(&entry) {
        Ok(l) => l + "\n",
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "journal entry could not be encoded").into_response(),
    };
    let _guard = write_lock().lock().await;
    let path = path();
    if let Some(dir) = std::path::Path::new(&path).parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let result = async {
        let mut f = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await?;
        f.write_all(line.as_bytes()).await?;
        f.flush().await
    }
    .await;
    match result {
        Ok(()) => Json(entry).into_response(),
        Err(e) => {
            warn!(error = %e, %path, "journal: append failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "journal entry was not saved").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, created: &str) -> Entry {
        Entry {
            id: id.into(),
            symbol: "ABCD".into(),
            created_at: created.into(),
            updated_at: created.into(),
            entry: 4.0,
            stop: 3.8,
            target: Some(4.4),
            shares: 500,
            snapshot: None,
            exit: None,
            notes: None,
        }
    }

    #[test]
    fn a_valid_entry_passes_and_bad_ones_say_why() {
        assert_eq!(invalid(&entry("a-1", "2026-10-15T13:40:00Z")), None);
        let mut e = entry("a 1", "2026-10-15T13:40:00Z");
        assert!(invalid(&e).unwrap().contains("id"));
        e = entry("a1", "yesterday");
        assert!(invalid(&e).unwrap().contains("RFC 3339"));
        e = entry("a1", "2026-10-15T13:40:00Z");
        e.stop = 4.0;
        assert!(invalid(&e).unwrap().contains("stop below"));
        e = entry("a1", "2026-10-15T13:40:00Z");
        e.symbol = "abcd".into();
        assert!(invalid(&e).unwrap().contains("ticker"));
        e = entry("a1", "2026-10-15T13:40:00Z");
        e.notes = Some("x".repeat(MAX_NOTES_CHARS + 1));
        assert!(invalid(&e).unwrap().contains("2000"));
    }

    #[test]
    fn the_last_line_per_id_wins_and_newest_trades_come_first() {
        let first = entry("a", "2026-10-15T13:40:00Z");
        let mut closed = first.clone();
        closed.exit = Some(Exit { price: 4.4, at: "2026-10-15T14:00:00Z".into() });
        let later = entry("b", "2026-10-15T15:00:00Z");
        let text = [&first, &later, &closed].iter().map(|e| serde_json::to_string(e).unwrap()).collect::<Vec<_>>().join("\n") + "\nnot json\n";
        let folded = fold(&text);
        assert_eq!(folded.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
        assert_eq!(folded[1].exit.as_ref().unwrap().price, 4.4);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let json = r#"{"id":"a","symbol":"ABCD","createdAt":"2026-10-15T13:40:00Z","updatedAt":"2026-10-15T13:40:00Z","entry":4,"stop":3.8,"shares":1,"admin":true}"#;
        assert!(serde_json::from_str::<Entry>(json).is_err());
    }

    #[test]
    fn camel_case_matches_the_client_type() {
        let json = serde_json::to_value(entry("a", "2026-10-15T13:40:00Z")).unwrap();
        for key in ["id", "symbol", "createdAt", "updatedAt", "entry", "stop", "target", "shares"] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
    }
}
