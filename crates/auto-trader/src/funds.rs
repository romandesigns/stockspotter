//! The universe's exchange-traded funds/notes (ws-server
//! `GET /universe/funds`), kept fresh for the Bullish Momentum trigger,
//! which leaves them out exactly as the web panel does (2026-10-16).
//!
//! Fetched on start, then every `REFRESH`; while the list is still empty
//! or a fetch fails, every `RETRY` instead. A fetch that returns an empty
//! list after a good one is ignored -- that is ws-server restarting before
//! its own first universe fetch, not every fund delisting at once. Until
//! the first good fetch the engine excludes nothing (fails open).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{info, warn};

const REFRESH: Duration = Duration::from_secs(30 * 60);
const RETRY: Duration = Duration::from_secs(60);

/// A slot the refresher fills with each new list; the run loop `take`s it
/// and hands it to the engine.
pub type FundSlot = Arc<Mutex<Option<HashSet<String>>>>;

pub fn spawn_refresher(http_base: String) -> FundSlot {
    let slot: FundSlot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    tokio::spawn(async move {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build();
        let Ok(client) = client else { return };
        let url = format!("{}/universe/funds", http_base.trim_end_matches('/'));
        let mut have_good = false;
        loop {
            let mut request = client.get(&url);
            if let Ok(token) = std::env::var("STOCKSPOTTER_API_TOKEN") {
                request = request.bearer_auth(token);
            }
            let fetched = async { request.send().await?.error_for_status()?.json::<Vec<String>>().await }.await;
            match fetched {
                Ok(list) if !list.is_empty() => {
                    info!(funds = list.len(), "fund list refreshed for the Bullish Momentum trigger");
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(list.into_iter().collect());
                    have_good = true;
                }
                Ok(_) => {}
                Err(error) => warn!(%error, %url, "fund list unavailable; keeping the last one"),
            }
            tokio::time::sleep(if have_good { REFRESH } else { RETRY }).await;
        }
    });
    slot
}

/// The newest list, if one arrived since the last call.
pub fn take(slot: &FundSlot) -> Option<HashSet<String>> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}
