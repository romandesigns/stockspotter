//! Server-side push-notification delivery for a confirmed ignition
//! (2026-09-04, Roman: "I want to be notified on my phone even if my
//! phone is locked and I'm not looking at the screen or the chart
//! directly. I want to be able to turn this feature off on the phone if
//! I want to"). The client-side alert shipped earlier today
//! (useIgnitionAlerts.ts, both platforms) only fires while the app
//! process is alive and connected -- a locked phone with the app merely
//! backgrounded gets suspended by the OS within a short window
//! (especially iOS), so it can silently miss the exact kind of move it
//! exists to catch. A REAL push, sent by this server via Expo's push
//! service straight to Apple/Google's own push infrastructure, reaches
//! the device even fully backgrounded or closed -- the client only needs
//! to have registered its push token once.
//!
//! Deliberately server-side, not "the client polls and re-arms itself" --
//! this process already holds the live ScanEvent stream in-process (see
//! main.rs's push_handle, which reuses the exact "second independent
//! broadcast subscriber" shape signal_handle already established for the
//! live-efficiency collector).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;
use tracing::warn;

use crate::speed_alert::SpeedAlert;

/// Same real per-symbol quiet window as the client-side alert
/// (useIgnitionAlerts.ts) -- ignition's raw signal is far too frequent to
/// push on every confirmation (confirmed live: a single hot symbol fired
/// follow_through_confirmed multiple times within 90 seconds). Kept as
/// the same 15 minutes rather than a fresh number so "how often could I
/// get pinged about the same stock" means the same thing everywhere this
/// alert exists.
pub const IGNITION_PUSH_COOLDOWN: chrono::Duration = chrono::Duration::minutes(15);

const EXPO_PUSH_URL: &str = "https://exp.host/--/api/v2/push/send";

/// Registered device push tokens, persisted to a small shared JSON file
/// (the `../../data:/app/data` mount this container already has for
/// live_pending_signals.jsonl/auto_trader_journal.jsonl) so a restart
/// doesn't silently un-register every device. A `HashSet`, not a list --
/// a device re-registering (e.g. reinstalling, or just the app calling
/// register again defensively on every launch) is naturally idempotent.
#[derive(Clone)]
pub struct PushTokenStore {
    path: PathBuf,
    tokens: Arc<RwLock<HashSet<String>>>,
}

impl PushTokenStore {
    /// A missing file means no tokens registered yet, not an error --
    /// same convention every other shared-file store in this project
    /// already follows (journal::append, live_signals::read_pending).
    pub async fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let tokens = match tokio::fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str::<HashSet<String>>(&content).unwrap_or_default(),
            Err(_) => HashSet::new(),
        };
        Self { path, tokens: Arc::new(RwLock::new(tokens)) }
    }

    /// Returns true if this was a real new registration (the token
    /// wasn't already known) -- callers don't currently use this, but
    /// it's the honest return type for "did anything actually change".
    pub async fn register(&self, token: String) -> bool {
        let mut tokens = self.tokens.write().await;
        let inserted = tokens.insert(token);
        if inserted {
            self.persist(&tokens).await;
        }
        inserted
    }

    /// This is the real mechanism behind "turn this feature off on the
    /// phone" -- the app calls this when the toggle goes off, and this
    /// server simply stops including that token in any future send.
    pub async fn unregister(&self, token: &str) -> bool {
        let mut tokens = self.tokens.write().await;
        let removed = tokens.remove(token);
        if removed {
            self.persist(&tokens).await;
        }
        removed
    }

    pub async fn snapshot(&self) -> HashSet<String> {
        self.tokens.read().await.clone()
    }

    async fn persist(&self, tokens: &HashSet<String>) {
        if let Some(parent) = self.path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match serde_json::to_string(tokens) {
            Ok(json) => {
                if let Err(e) = tokio::fs::write(&self.path, json).await {
                    warn!(error = %e, path = %self.path.display(), "failed to persist push token store");
                }
            }
            Err(e) => warn!(error = %e, "failed to serialize push token store"),
        }
    }
}

#[derive(Serialize)]
struct ExpoPushMessage<'a> {
    to: &'a str,
    title: String,
    body: String,
    sound: &'static str,
    #[serde(rename = "channelId")]
    channel_id: &'static str,
    data: ExpoPushData<'a>,
}

#[derive(Serialize)]
struct ExpoPushData<'a> {
    symbol: &'a str,
}

/// Sends one real ignition-confirmed push to every currently-registered
/// device, in a single batched request (Expo's own push API accepts an
/// array body for exactly this). Best-effort: a bad response is logged,
/// not treated as fatal -- same "one bad send doesn't take down the
/// caller" idiom this project already uses for its other outbound calls
/// (request_assessment, the /qualify fire-and-forget notify). Doesn't yet
/// prune tokens off a DeviceNotRegistered receipt -- that needs Expo's
/// separate receipts API, a real, scoped follow-up not attempted here to
/// keep this addition bounded; a stale token just fails silently
/// server-side (and the app re-registers its current token on every
/// launch anyway, so this self-heals in practice).
pub async fn send_ignition_push(client: &reqwest::Client, tokens: &HashSet<String>, symbol: &str, price: f64) {
    if tokens.is_empty() {
        return;
    }
    let price_str = if price < 1.0 { format!("{price:.4}") } else { format!("{price:.2}") };
    let body: Vec<ExpoPushMessage> = tokens
        .iter()
        .map(|token| ExpoPushMessage {
            to: token,
            title: format!("{symbol} ignition confirmed"),
            // Detector language, not a promise: ignition is indistinguishable
            // from random minutes (consolidation index R1), so the alert
            // must not read as a trade call.
            body: format!("Detector follow-through at ${price_str}. Unproven signal, not advice."),
            sound: "default",
            channel_id: "ignition-alerts",
            data: ExpoPushData { symbol },
        })
        .collect();

    match client.post(EXPO_PUSH_URL).json(&body).send().await {
        Ok(resp) if !resp.status().is_success() => {
            warn!(status = %resp.status(), symbol, recipients = tokens.len(), "Expo push send returned a non-success status");
        }
        Err(e) => warn!(error = %e, symbol, "Expo push send request failed"),
        Ok(_) => {}
    }
}

/// Title and body of a speed-alert push, split out so the wording is
/// testable without a network call. Detector language, same as the
/// ignition push: the thresholds behind it are unmeasured starting values
/// (speed_alert.rs), so it must not read as a trade call. Parts whose
/// input is unknown are left out rather than shown as a placeholder.
pub fn speed_push_text(alert: &SpeedAlert) -> (String, String) {
    let mut title = format!("{} ▲ speed {}%", alert.symbol, alert.pct);
    if alert.filling_fast {
        title.push_str(" · filling fast");
    }
    let price = alert.price;
    let price_str = if price < 1.0 { format!("{price:.4}") } else { format!("{price:.2}") };
    let mut body = match alert.move_pct.filter(|m| m.is_finite()) {
        Some(m) => format!("{m:+.1}% in 5 min at ${price_str}"),
        None => format!("At ${price_str}"),
    };
    if let Some(rv) = alert.relative_volume.filter(|v| v.is_finite()) {
        body.push_str(&format!(" · rel vol {rv:.1}x"));
    }
    body.push_str(". Unproven signal, not advice.");
    (title, body)
}

/// Sends one speed-alert push to every device registered on the SPEED
/// token store (a separate opt-in from ignition pushes, see main.rs's
/// SPEED_PUSH_TOKENS_PATH). Same batched, best-effort delivery as
/// send_ignition_push, on its own Android channel ("speed-alerts") so the
/// phone can give it its own sound/importance settings.
pub async fn send_speed_push(client: &reqwest::Client, tokens: &HashSet<String>, alert: &SpeedAlert) {
    if tokens.is_empty() {
        return;
    }
    let (title, body_text) = speed_push_text(alert);
    let symbol = alert.symbol.as_str();
    let body: Vec<ExpoPushMessage> = tokens
        .iter()
        .map(|token| ExpoPushMessage {
            to: token,
            title: title.clone(),
            body: body_text.clone(),
            sound: "default",
            channel_id: "speed-alerts",
            data: ExpoPushData { symbol },
        })
        .collect();

    match client.post(EXPO_PUSH_URL).json(&body).send().await {
        Ok(resp) if !resp.status().is_success() => {
            warn!(status = %resp.status(), symbol, recipients = tokens.len(), "Expo speed push send returned a non-success status");
        }
        Err(e) => warn!(error = %e, symbol, "Expo speed push send request failed"),
        Ok(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speed_alert(price: f64, move_pct: Option<f64>, relative_volume: Option<f64>, filling_fast: bool) -> SpeedAlert {
        SpeedAlert { symbol: "ABCD".into(), at: chrono::Utc::now(), price, pct: 72, move_pct, relative_volume, filling_fast }
    }

    #[test]
    fn speed_push_text_with_every_part_known() {
        let (title, body) = speed_push_text(&speed_alert(0.531, Some(19.6), Some(5.2), true));
        assert_eq!(title, "ABCD ▲ speed 72% · filling fast");
        assert_eq!(body, "+19.6% in 5 min at $0.5310 · rel vol 5.2x. Unproven signal, not advice.");
    }

    #[test]
    fn speed_push_text_omits_unknown_parts_and_uses_cents_above_a_dollar() {
        let (title, body) = speed_push_text(&speed_alert(12.346, None, None, false));
        assert_eq!(title, "ABCD ▲ speed 72%");
        assert_eq!(body, "At $12.35. Unproven signal, not advice.");
    }

    #[tokio::test]
    async fn sending_a_speed_push_to_an_empty_token_set_is_a_safe_no_op() {
        let client = reqwest::Client::new();
        send_speed_push(&client, &HashSet::new(), &speed_alert(1.0, None, None, false)).await;
    }

    #[tokio::test]
    async fn a_missing_file_loads_as_an_empty_store_not_an_error() {
        let store = PushTokenStore::load("/tmp/definitely-does-not-exist-stockspotter-push-tokens.json").await;
        assert!(store.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn register_then_unregister_round_trips_through_the_real_file() {
        let path = std::env::temp_dir().join(format!("push_tokens_test_{}.json", std::process::id()));
        let store = PushTokenStore::load(&path).await;

        assert!(store.register("ExponentPushToken[abc]".to_string()).await);
        // Registering the same token again is a no-op, not a duplicate.
        assert!(!store.register("ExponentPushToken[abc]".to_string()).await);
        assert_eq!(store.snapshot().await.len(), 1);

        // A fresh load from disk sees the real persisted write, not just
        // this instance's in-memory copy.
        let reloaded = PushTokenStore::load(&path).await;
        assert_eq!(reloaded.snapshot().await.len(), 1);

        assert!(store.unregister("ExponentPushToken[abc]").await);
        assert!(store.snapshot().await.is_empty());
        // This is the real mechanism behind "turn this feature off" --
        // confirm it actually clears the persisted file too, not just
        // this process's memory.
        let reloaded_after_off = PushTokenStore::load(&path).await;
        assert!(reloaded_after_off.snapshot().await.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn sending_to_an_empty_token_set_is_a_safe_no_op() {
        // No network call should even be attempted -- if this test hangs
        // or errors, that's the regression (an empty-recipients POST to
        // Expo would be a wasted, always-failing call).
        let client = reqwest::Client::new();
        send_ignition_push(&client, &HashSet::new(), "TEST", 1.23).await;
    }
}
