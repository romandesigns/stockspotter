//! Alpaca paper execution. The endpoint is fixed; this module cannot submit live orders.
use crate::journal::{ExitReason, JournalEntry};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufRead, Read, Write},
    path::Path,
};

const PAPER_BASE: &str = "https://paper-api.alpaca.markets";

#[derive(Clone)]
pub struct Broker {
    client: reqwest::Client,
    key: String,
    secret: String,
    #[cfg(test)]
    test_base: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    pub status: String,
    pub qty: String,
    pub filled_qty: String,
    pub filled_avg_price: Option<String>,
    pub filled_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

pub fn number(value: &str) -> Result<f64> {
    let n: f64 = value.parse().context("invalid broker numeric field")?;
    ensure!(n.is_finite() && n >= 0., "invalid broker numeric value");
    Ok(n)
}

pub fn shares(value: &str) -> Result<u64> {
    let n = number(value)?;
    ensure!(
        n.fract() == 0. && n < u64::MAX as f64,
        "expected whole-share paper order quantity"
    );
    Ok(n as u64)
}

impl Order {
    pub fn terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "filled" | "canceled" | "expired" | "rejected" | "replaced"
        )
    }
}

#[derive(Deserialize)]
pub struct Account {
    pub id: String,
    pub status: String,
    pub currency: String,
    pub trading_blocked: bool,
    pub account_blocked: bool,
    pub buying_power: String,
    pub cash: String,
}

#[derive(Deserialize)]
pub struct Clock {
    pub timestamp: DateTime<Utc>,
    pub is_open: bool,
    pub next_close: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct Position {
    pub symbol: String,
    pub qty: String,
    pub side: String,
}

impl Broker {
    pub fn from_env() -> Result<Self> {
        let configured = std::env::var("ALPACA_TRADING_BASE").unwrap_or_else(|_| PAPER_BASE.into());
        ensure!(
            configured.trim_end_matches('/') == PAPER_BASE,
            "paper trader requires the Alpaca paper endpoint"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            key: std::env::var("ALPACA_API_KEY").context("missing ALPACA_API_KEY")?,
            secret: std::env::var("ALPACA_API_SECRET").context("missing ALPACA_API_SECRET")?,
            #[cfg(test)]
            test_base: None,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let base = PAPER_BASE;
        #[cfg(test)]
        let base = self.test_base.as_deref().unwrap_or(base);
        self.client
            .request(method, format!("{base}{path}"))
            .header("APCA-API-KEY-ID", &self.key)
            .header("APCA-API-SECRET-KEY", &self.secret)
    }
    #[cfg(test)]
    pub(crate) fn mock(base: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap(),
            key: "test-key".into(),
            secret: "test-secret".into(),
            test_base: Some(base),
        }
    }
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .request(reqwest::Method::GET, path)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn account(&self) -> Result<Account> {
        let a: Account = self.get("/v2/account").await?;
        ensure!(
            a.status == "ACTIVE" && a.currency == "USD" && !a.account_blocked && !a.trading_blocked,
            "paper account is not active for USD trading"
        );
        Ok(a)
    }
    pub async fn clock(&self) -> Result<Clock> {
        self.get("/v2/clock").await
    }
    pub async fn positions(&self) -> Result<Vec<Position>> {
        self.get("/v2/positions").await
    }
    pub async fn open_orders(&self) -> Result<Vec<Order>> {
        self.get("/v2/orders?status=open&limit=500").await
    }
    pub async fn lookup(&self, client_id: &str) -> Result<Option<Order>> {
        let r = self
            .request(reqwest::Method::GET, "/v2/orders:by_client_order_id")
            .query(&[("client_order_id", client_id)])
            .send()
            .await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(r.error_for_status()?.json().await?))
    }
    pub async fn submit(&self, intent: &Intent) -> Result<Order> {
        let mut body = serde_json::json!({"symbol":intent.symbol,"qty":intent.qty.to_string(),
            "side":intent.side,"client_order_id":intent.client_id,"extended_hours":false,
            "type":if intent.limit.is_some() {"limit"} else {"market"},
            "time_in_force":if intent.limit.is_some() {"ioc"} else {"day"}});
        if let Some(limit) = &intent.limit {
            body["limit_price"] = serde_json::json!(limit);
        }
        Ok(self
            .request(reqwest::Method::POST, "/v2/orders")
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Intent {
    pub client_id: String,
    pub symbol: String,
    pub side: String,
    pub qty: u64,
    pub limit: Option<String>,
    pub created_at: DateTime<Utc>,
    pub attempted: bool,
    pub local_canceled: bool,
    #[serde(default)]
    pub attempted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_received_at: Option<DateTime<Utc>>,
    pub order: Option<Order>,
    pub first_fill_at: Option<DateTime<Utc>>,
}

impl Intent {
    pub fn done(&self) -> bool {
        self.local_canceled || self.order.as_ref().is_some_and(Order::terminal)
    }
    pub fn filled(&self) -> Result<u64> {
        self.order.as_ref().map_or(Ok(0), |o| shares(&o.filled_qty))
    }
    pub fn value(&self) -> Result<f64> {
        let qty = self.filled()?;
        if qty == 0 {
            return Ok(0.);
        }
        let price = number(
            self.order
                .as_ref()
                .and_then(|o| o.filled_avg_price.as_deref())
                .context("fill missing price")?,
        )?;
        ensure!(price > 0., "fill price must be positive");
        Ok(price * qty as f64)
    }
    pub fn update(&mut self, order: Order, received_at: DateTime<Utc>) -> Result<()> {
        ensure!(
            order.client_order_id == self.client_id
                && order.symbol == self.symbol
                && order.side == self.side,
            "broker order identity differs from durable intent"
        );
        ensure!(
            shares(&order.qty)? == self.qty,
            "broker order size differs from intent"
        );
        let filled = shares(&order.filled_qty)?;
        ensure!(
            filled >= self.filled()? && filled <= self.qty,
            "broker fill quantity regressed or exceeded order"
        );
        if filled > 0 {
            ensure!(
                order
                    .filled_avg_price
                    .as_deref()
                    .map(number)
                    .transpose()?
                    .is_some_and(|p| p > 0.),
                "fill has no valid price"
            );
            if self.first_fill_at.is_none() {
                self.first_fill_at =
                    Some(order.filled_at.or(order.updated_at).unwrap_or(received_at));
            }
        }
        self.order = Some(order);
        self.last_received_at = Some(received_at);
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub proposal: JournalEntry,
    pub buy: Intent,
    pub sells: Vec<Intent>,
    pub adjustments: Vec<JournalEntry>,
    pub exit_reason: Option<ExitReason>,
}

impl Trade {
    pub fn remaining(&self) -> Result<u64> {
        let sold = self
            .sells
            .iter()
            .map(Intent::filled)
            .collect::<Result<Vec<_>>>()?
            .iter()
            .sum::<u64>();
        self.buy
            .filled()?
            .checked_sub(sold)
            .context("paper ledger would contain a short position")
    }
    pub fn active(&self) -> Result<bool> {
        Ok(!self.buy.done() || self.remaining()? > 0 || self.sells.iter().any(|o| !o.done()))
    }
    pub fn history(&self) -> Result<Vec<JournalEntry>> {
        let qty = self.buy.filled()?;
        if qty == 0 {
            return Ok(vec![]);
        }
        let mut entry = self.proposal.clone();
        let price = self.buy.value()? / qty as f64;
        let remaining = self.remaining()?;
        let entered_at = self
            .buy
            .first_fill_at
            .context("missing first fill timestamp")?;
        if let JournalEntry::Entered {
            entry_price,
            qty: entry_qty,
            target_price,
            stop_price,
            entered_at: at,
            strategy,
            position_size_usd,
            ..
        } = &mut entry
        {
            let brackets = backtest_metrics::OutcomeThresholds::for_strategy(*strategy);
            *entry_price = price;
            *entry_qty = if remaining > 0 { remaining } else { qty };
            *target_price = price * (1. + brackets.target_pct / 100.);
            *stop_price = price * (1. - brackets.stop_pct / 100.);
            *at = entered_at;
            *position_size_usd = price * *entry_qty as f64;
        } else {
            bail!("paper trade proposal is not an entry");
        }
        let mut out = vec![entry];
        out.extend(self.adjustments.clone());
        if remaining == 0 && self.buy.done() {
            let proceeds = self
                .sells
                .iter()
                .map(Intent::value)
                .collect::<Result<Vec<_>>>()?
                .iter()
                .sum::<f64>();
            let pnl = proceeds - self.buy.value()?;
            let at = self
                .sells
                .iter()
                .filter_map(|o| o.order.as_ref().and_then(|o| o.filled_at.or(o.updated_at)))
                .max()
                .context("exit lacks timestamp")?;
            out.push(JournalEntry::Exited {
                symbol: self.buy.symbol.clone(),
                exit_price: proceeds / qty as f64,
                exit_reason: self.exit_reason.unwrap_or(ExitReason::Timeout),
                pnl_usd: pnl,
                pnl_pct: 100. * pnl / self.buy.value()?,
                assumed_cost_pct: 0.,
                qty,
                entered_at,
                exited_at: at,
            });
        }
        Ok(out)
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub account_id: String,
    pub trades: Vec<Trade>,
}

impl State {
    pub fn history(&self) -> Result<Vec<JournalEntry>> {
        let mut out = Vec::new();
        for t in &self.trades {
            out.extend(t.history()?);
        }
        Ok(out)
    }
    pub fn active_count(&self) -> Result<usize> {
        Ok(self
            .trades
            .iter()
            .map(Trade::active)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|x| *x)
            .count())
    }
    pub fn expected_positions(&self) -> Result<HashMap<String, u64>> {
        let mut expected = HashMap::new();
        for t in &self.trades {
            let qty = t.remaining()?;
            if qty > 0 {
                *expected.entry(t.buy.symbol.clone()).or_insert(0) += qty;
            }
        }
        Ok(expected)
    }
    pub fn verify_positions(&self, positions: &[Position]) -> Result<()> {
        let expected = self.expected_positions()?;
        let mut actual = HashMap::new();
        for p in positions {
            ensure!(
                p.side == "long",
                "paper account contains an unmanaged short position"
            );
            actual.insert(p.symbol.clone(), shares(&p.qty)?);
        }
        ensure!(
            expected == actual,
            "paper account positions differ from this runner's ledger; entries paused"
        );
        Ok(())
    }
}

/// Exclusive process lock plus a durable snapshot write before every broker mutation.
///
/// ## On-disk format (unchanged since the ledger was introduced)
///
/// JSON Lines, one complete [`State`] per line; the LAST line is the
/// state. Nothing ever reads an earlier line for its content -- recovery,
/// `state_for_display` and the ws-server status endpoint all take the last
/// complete snapshot.
///
/// ## Why the ledger used to grow quadratically (fixed 2026-09-25)
///
/// `save` used to APPEND a full snapshot per call. Every snapshot carries
/// every trade since inception, and a single trade costs roughly ten saves
/// (intent, attempted, submit response, each changed lookup, stop
/// adjustments, exit reason, each sell's own intent/attempt/fill). So file
/// size was ~ saves x trades: on the VPS, 4,031 snapshots over 405 trades
/// had reached 1.39 GB (latest line 693 KB) and was adding >100 MB a day,
/// and `open` read the whole file into one `String` and parsed every line.
///
/// `save` now REPLACES the file with the one snapshot that was always the
/// only one that mattered: write `<ledger>.tmp`, fsync it, rename it over
/// the ledger, fsync the directory. The format is identical -- a file
/// with one snapshot is a legal old-format ledger -- so an older binary
/// (rollback) and the ws-server read it without change.
///
/// Crash safety is strictly better than the append it replaces. With
/// append, a crash mid-write left a torn last line and `open` refused to
/// start until a human reconciled it. With rename, the ledger path always
/// names a complete file: either the previous snapshot or the new one.
/// Falling back to the previous snapshot is safe for the same reason the
/// runtime saves BEFORE every broker mutation: if the write did not
/// complete, `save` did not return, so the mutation it guards was never
/// sent. (A save that records a broker RESPONSE and is lost is recovered
/// by the next client-order-id lookup, exactly as a lost HTTP response is.)
///
/// ## Existing multi-snapshot ledgers
///
/// `open` still accepts them exactly as before (every non-blank line must
/// parse, the file must end in a newline, last line wins), but streams
/// them line by line instead of holding the whole file in memory. If more
/// than one snapshot is present, the old file is first preserved under
/// `<ledger>.archived-<unix-seconds>` by HARD LINK -- no copy, no extra
/// disk, and no instant at which the ledger path is missing -- and then
/// the recovered state is written back as a single snapshot. Nothing is
/// deleted; removing the archive is an operator decision. The suffix is
/// deliberately not `.jsonl` so `python/export_session.py`'s capture glob
/// does not start exporting 1.4 GB of superseded snapshots.
pub struct Store {
    path: std::path::PathBuf,
    _owner_lock: File,
}
#[derive(Debug)]
pub struct PersistenceError(pub String);
impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "paper ledger persistence failed: {}", self.0)
    }
}
impl std::error::Error for PersistenceError {}

fn sibling(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Makes a completed rename or link durable. POSIX only persists a
/// directory entry once the directory itself is fsynced; Windows (tests
/// only -- production is Linux) cannot open a directory as a `File`.
fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn write_snapshot(path: &Path, state: &State) -> Result<()> {
    let tmp = sibling(path, ".tmp");
    let mut out = std::io::BufWriter::new(File::create(&tmp)?);
    serde_json::to_writer(&mut out, state)?;
    out.write_all(b"\n")?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    sync_parent(path)?;
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<(Self, State)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let owner_lock = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path.with_extension("lock"))?;
        owner_lock
            .try_lock()
            .context("another paper runner owns this ledger")?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        // The same validation the original whole-file read applied, one
        // line at a time: every non-blank line must be a valid State (the
        // last one wins), then the file must end in a newline.
        let mut reader = std::io::BufReader::new(file);
        let mut line = Vec::new();
        let mut state = State::default();
        let mut snapshots = 0usize;
        let mut ends_with_newline = true;
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            ends_with_newline = line.ends_with(b"\n");
            let text = std::str::from_utf8(&line)
                .context("invalid paper ledger; reconcile before resuming")?;
            if text.trim().is_empty() {
                continue;
            }
            state = serde_json::from_str(text)
                .context("invalid paper ledger; reconcile before resuming")?;
            snapshots += 1;
        }
        drop(reader);
        ensure!(
            ends_with_newline,
            "incomplete paper ledger write; reconcile before resuming"
        );
        if snapshots > 1 {
            let archive = sibling(path, &format!(".archived-{}", Utc::now().timestamp()));
            std::fs::hard_link(path, &archive).with_context(|| {
                format!(
                    "could not preserve the multi-snapshot paper ledger as {} before compacting it",
                    archive.display()
                )
            })?;
            sync_parent(&archive)?;
            write_snapshot(path, &state).context("compacting the paper ledger")?;
        }
        Ok((
            Self {
                path: path.to_path_buf(),
                _owner_lock: owner_lock,
            },
            state,
        ))
    }
    pub fn save(&mut self, state: &State) -> Result<()> {
        write_snapshot(&self.path, state).map_err(|e| PersistenceError(e.to_string()).into())
    }
}

/// Monitoring may read the prior complete snapshot during an in-progress append.
/// Recovery uses Store::open's stricter validation instead.
pub fn state_for_display(content: &str) -> Result<State> {
    let Some(end) = content.rfind('\n') else {
        ensure!(content.is_empty(), "no complete paper snapshot available");
        return Ok(State::default());
    };
    let line = content[..end]
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty());
    line.map_or(Ok(State::default()), |line| Ok(serde_json::from_str(line)?))
}

/// `state_for_display(&read_to_string(path)?)` without reading the file.
///
/// The ws-server's `/auto-trader/status` used to do exactly that per
/// request, which against the 1.39 GB multi-snapshot ledger meant 1.5 s
/// warm and >20 s cold. This reads backwards from the end in doubling
/// chunks until it holds the last complete, non-blank line, so the cost is
/// one snapshot however long the file is -- and it stays correct against
/// a legacy ledger that has not been compacted yet (an older trader still
/// running, or ws-server deployed first).
///
/// Same answer as `state_for_display` for the same bytes: a trailing
/// partial line is ignored, blank lines are skipped, an empty file is the
/// default state, and a non-empty file with no newline at all is an error.
/// A missing file is also the default state -- the ws-server's
/// long-standing "nothing captured yet" convention. More lenient in one
/// respect only: bytes before the last snapshot are never looked at, so
/// they cannot fail the read.
pub fn state_for_display_from_file(path: &Path) -> Result<State> {
    tail_snapshot(path, 64 * 1024)
}

fn tail_snapshot(path: &Path, first_chunk: u64) -> Result<State> {
    use std::io::{Seek, SeekFrom};
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(e.into()),
    };
    let parse = |line: &[u8]| -> Result<Option<State>> {
        let text = std::str::from_utf8(line)?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(text)?))
    };
    // `window` always holds exactly the file's bytes [start, len).
    let len = file.metadata()?.len();
    let mut start = len;
    let mut window: Vec<u8> = Vec::new();
    let mut chunk = first_chunk.max(1);
    loop {
        if let Some(end) = window.iter().rposition(|b| *b == b'\n') {
            let complete = &window[..end];
            let mut line_end = complete.len();
            loop {
                match complete[..line_end].iter().rposition(|b| *b == b'\n') {
                    Some(newline) => {
                        if let Some(state) = parse(&complete[newline + 1..line_end])? {
                            return Ok(state);
                        }
                        line_end = newline;
                    }
                    // A line's start is only certain at the file's start;
                    // anywhere else it may continue before `start`.
                    None if start == 0 => {
                        return Ok(parse(&complete[..line_end])?.unwrap_or_default());
                    }
                    None => break,
                }
            }
        } else if start == 0 {
            ensure!(window.is_empty(), "no complete paper snapshot available");
            return Ok(State::default());
        }
        let read = chunk.min(start);
        start -= read;
        file.seek(SeekFrom::Start(start))?;
        let mut before = vec![0; read as usize];
        file.read_exact(&mut before)?;
        before.extend_from_slice(&window);
        window = before;
        chunk = chunk.saturating_mul(2);
    }
}

pub fn entry_limit(ask: f64, budget: f64) -> Result<(String, u64)> {
    ensure!(
        ask.is_finite() && ask > 0. && budget.is_finite() && budget > 0.,
        "invalid entry quote/budget"
    );
    let scale = if ask * 1.001 >= 1. { 100. } else { 10000. };
    let price = (ask * 1.001 * scale).ceil() / scale;
    let qty = (budget / price).floor() as u64;
    ensure!(qty > 0, "one share exceeds the paper entry budget");
    Ok((
        if scale == 100. {
            format!("{price:.2}")
        } else {
            format!("{price:.4}")
        },
        qty,
    ))
}

pub fn entry_window(clock: &Clock, at: DateTime<Utc>) -> bool {
    clock.is_open
        && clock.next_close - clock.timestamp > Duration::minutes(1)
        && clock.timestamp - at >= Duration::seconds(-1)
        && clock.timestamp - at <= Duration::seconds(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limit_rounding_keeps_total_within_budget() {
        for ask in [0.25, 0.9999, 1., 4.123, 272.05] {
            let (price, qty) = entry_limit(ask, 500.).unwrap();
            assert!(number(&price).unwrap() * qty as f64 <= 500.);
            assert!(number(&price).unwrap() >= ask);
        }
        assert!(entry_limit(f64::NAN, 500.).is_err());
    }
    #[test]
    fn closed_market_and_old_signals_cannot_queue_for_tomorrow() {
        let now = Utc::now();
        let mut clock = Clock {
            timestamp: now,
            is_open: false,
            next_close: now + Duration::hours(1),
        };
        assert!(!entry_window(&clock, now));
        clock.is_open = true;
        assert!(entry_window(&clock, now));
        assert!(!entry_window(&clock, now - Duration::seconds(3)));
        clock.next_close = now + Duration::seconds(30);
        assert!(!entry_window(&clock, now));
    }
    #[test]
    fn fractional_negative_and_nonfinite_quantities_fail_closed() {
        assert_eq!(shares("3.0").unwrap(), 3);
        for value in ["0.5", "-1", "NaN", "inf"] {
            assert!(shares(value).is_err());
        }
    }

    fn order(qty: u64, filled: u64, status: &str, side: &str, id: &str) -> Order {
        Order {
            id: id.into(),
            client_order_id: id.into(),
            symbol: "TEST".into(),
            side: side.into(),
            status: status.into(),
            qty: qty.to_string(),
            filled_qty: filled.to_string(),
            filled_avg_price: if filled > 0 { Some("10".into()) } else { None },
            filled_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    }
    fn intent(side: &str, id: &str, qty: u64) -> Intent {
        Intent {
            client_id: id.into(),
            symbol: "TEST".into(),
            side: side.into(),
            qty,
            limit: None,
            created_at: Utc::now(),
            attempted: true,
            local_canceled: false,
            attempted_at: Some(Utc::now()),
            last_received_at: None,
            order: None,
            first_fill_at: None,
        }
    }
    fn trade() -> Trade {
        Trade {
            proposal: JournalEntry::Entered {
                symbol: "TEST".into(),
                strategy: backtest_metrics::Strategy::IgnitionDetector,
                entry_price: 9.,
                qty: 10,
                position_size_usd: 90.,
                target_price: 9.18,
                stop_price: 8.82,
                entered_at: Utc::now(),
                momentum_overall: 0.9,
                momentum_volume_confirmation: 0.9,
                catalyst_tags: vec![],
            },
            buy: intent("buy", "buy-id", 10),
            sells: vec![],
            adjustments: vec![],
            exit_reason: None,
        }
    }
    #[test]
    fn accepted_orders_are_not_positions_and_partial_fills_use_broker_price() {
        let mut t = trade();
        t.buy
            .update(order(10, 0, "accepted", "buy", "buy-id"), Utc::now())
            .unwrap();
        assert!(t.history().unwrap().is_empty());
        assert!(t.active().unwrap());
        t.buy
            .update(
                order(10, 3, "partially_filled", "buy", "buy-id"),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(t.remaining().unwrap(), 3);
        assert!(
            matches!(&t.history().unwrap()[0],JournalEntry::Entered{entry_price,qty,target_price,..}
            if *entry_price==10. && *qty==3 && (*target_price-10.2).abs()<1e-9)
        );
        t.buy
            .update(order(10, 3, "canceled", "buy", "buy-id"), Utc::now())
            .unwrap();
        assert!(t.buy.done());
        assert!(t.active().unwrap());
    }
    #[test]
    fn partial_sales_never_mark_remaining_shares_closed() {
        let mut t = trade();
        t.buy
            .update(order(10, 10, "filled", "buy", "buy-id"), Utc::now())
            .unwrap();
        let mut sell = intent("sell", "sell-id", 10);
        sell.update(order(10, 4, "canceled", "sell", "sell-id"), Utc::now())
            .unwrap();
        t.sells.push(sell);
        assert_eq!(t.remaining().unwrap(), 6);
        assert_eq!(t.history().unwrap().len(), 1);
        assert!(matches!(
            t.history().unwrap()[0],
            JournalEntry::Entered { qty: 6, .. }
        ));
        let mut sell = intent("sell", "sell-2", 6);
        sell.update(order(6, 6, "filled", "sell", "sell-2"), Utc::now())
            .unwrap();
        t.sells.push(sell);
        assert_eq!(t.remaining().unwrap(), 0);
        assert!(!t.active().unwrap());
        assert!(
            matches!(t.history().unwrap()[1],JournalEntry::Exited{qty:10,pnl_usd,..} if pnl_usd==0.)
        );
    }
    #[test]
    fn mismatched_or_regressing_fills_are_rejected() {
        let mut i = intent("buy", "buy-id", 10);
        i.update(
            order(10, 5, "partially_filled", "buy", "buy-id"),
            Utc::now(),
        )
        .unwrap();
        assert!(i
            .update(
                order(10, 4, "partially_filled", "buy", "buy-id"),
                Utc::now()
            )
            .is_err());
        assert!(i
            .update(
                order(10, 6, "partially_filled", "buy", "wrong-id"),
                Utc::now()
            )
            .is_err());
        assert_eq!(i.filled().unwrap(), 5);
    }
    #[test]
    fn durable_attempt_and_partial_fill_survive_restart_and_prevent_second_owner() {
        let path = std::env::temp_dir().join(format!(
            "ss-paper-test-{}-{}.jsonl",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let (mut store, mut state) = Store::open(&path).unwrap();
        let mut t = trade();
        t.buy
            .update(
                order(10, 3, "partially_filled", "buy", "buy-id"),
                Utc::now(),
            )
            .unwrap();
        state.trades.push(t);
        store.save(&state).unwrap();
        assert!(Store::open(&path).is_err());
        drop(store);
        let (store, recovered) = Store::open(&path).unwrap();
        assert!(recovered.trades[0].buy.attempted);
        assert_eq!(recovered.trades[0].remaining().unwrap(), 3);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            state_for_display(&content).unwrap().trades[0]
                .remaining()
                .unwrap(),
            3
        );
        drop(store);
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn unmanaged_positions_block_entries() {
        let state = State::default();
        assert!(state.verify_positions(&[]).is_ok());
        assert!(state
            .verify_positions(&[Position {
                symbol: "OTHER".into(),
                qty: "1".into(),
                side: "long".into()
            }])
            .is_err());
    }

    // ---- ledger persistence (2026-09-25: replace-not-append) ----

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ss-paper-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn json(state: &State) -> serde_json::Value {
        serde_json::to_value(state).unwrap()
    }
    /// Three snapshots of one growing history, as the old append-per-save
    /// `Store::save` would have left them on disk.
    fn legacy_snapshots() -> Vec<State> {
        let mut state = State {
            account_id: "acct".into(),
            trades: vec![],
        };
        let mut out = vec![];
        let mut t = trade();
        state.trades.push(t.clone());
        out.push(state.clone());
        t.buy
            .update(order(10, 10, "filled", "buy", "buy-id"), Utc::now())
            .unwrap();
        state.trades[0] = t.clone();
        out.push(state.clone());
        let mut stuck = trade();
        stuck.buy.client_id = "stuck-id".into();
        state.trades.push(stuck);
        out.push(state);
        out
    }
    fn legacy_bytes(snapshots: &[State]) -> String {
        let mut content = String::new();
        for (i, s) in snapshots.iter().enumerate() {
            content.push_str(&serde_json::to_string(s).unwrap());
            content.push('\n');
            if i == 0 {
                // Blank lines were always tolerated; keep proving it.
                content.push_str("  \n");
            }
        }
        content
    }
    fn archives(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".archived-"))
            .collect()
    }

    #[test]
    fn legacy_multi_snapshot_ledger_loads_unchanged_and_is_compacted_without_deleting_it() {
        let dir = scratch_dir("legacy");
        let path = dir.join("ledger.jsonl");
        let snapshots = legacy_snapshots();
        let original = legacy_bytes(&snapshots);
        std::fs::write(&path, &original).unwrap();
        let last = json(snapshots.last().unwrap());

        let (store, state) = Store::open(&path).unwrap();
        // Same reconstructed state the old whole-file loader produced.
        assert_eq!(json(&state), last);
        drop(store);
        // The superseded history is preserved byte-for-byte, not deleted...
        let archived = archives(&dir);
        assert_eq!(archived.len(), 1);
        assert_eq!(std::fs::read_to_string(&archived[0]).unwrap(), original);
        assert!(!archived[0].to_string_lossy().ends_with(".jsonl"));
        // ...and the live ledger is now the single snapshot that mattered,
        // still in the old format, so the old parser reads the same state.
        let live = std::fs::read_to_string(&path).unwrap();
        assert_eq!(live.lines().count(), 1);
        assert!(live.ends_with('\n'));
        assert_eq!(json(&state_for_display(&live).unwrap()), last);
        // Reopening is a no-op: one snapshot, nothing more to archive.
        let (_store, again) = Store::open(&path).unwrap();
        assert_eq!(json(&again), last);
        assert_eq!(archives(&dir).len(), 1);
        drop(_store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_replaces_the_snapshot_instead_of_appending_history() {
        let dir = scratch_dir("replace");
        let path = dir.join("ledger.jsonl");
        let (mut store, _) = Store::open(&path).unwrap();
        let snapshots = legacy_snapshots();
        for s in &snapshots {
            store.save(s).unwrap();
        }
        let live = std::fs::read_to_string(&path).unwrap();
        // One line, sized by the current state -- not by how many saves ran.
        assert_eq!(
            live,
            format!(
                "{}\n",
                serde_json::to_string(snapshots.last().unwrap()).unwrap()
            )
        );
        assert!(!sibling(&path, ".tmp").exists());
        assert!(archives(&dir).is_empty());
        drop(store);
        let (_store, recovered) = Store::open(&path).unwrap();
        assert_eq!(json(&recovered), json(snapshots.last().unwrap()));
        drop(_store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_leftover_tmp_from_a_crashed_save_never_replaces_the_ledger() {
        // A crash between writing `.tmp` and the rename leaves the previous
        // complete snapshot in place: open must use it and ignore the tmp.
        let dir = scratch_dir("tmp");
        let path = dir.join("ledger.jsonl");
        let snapshots = legacy_snapshots();
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&snapshots[0]).unwrap()),
        )
        .unwrap();
        std::fs::write(sibling(&path, ".tmp"), "{\"account_id\":\"acct\",\"tra").unwrap();
        let (mut store, state) = Store::open(&path).unwrap();
        assert_eq!(json(&state), json(&snapshots[0]));
        store.save(&snapshots[1]).unwrap();
        drop(store);
        let (_store, state) = Store::open(&path).unwrap();
        assert_eq!(json(&state), json(&snapshots[1]));
        drop(_store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_still_refuses_torn_or_invalid_legacy_ledgers() {
        let dir = scratch_dir("refuse");
        let good = serde_json::to_string(&legacy_snapshots()[0]).unwrap();
        for (name, content) in [
            ("torn", format!("{good}\n{}", &good[..good.len() / 2])),
            ("no-newline", good.clone()),
            ("bad-middle", format!("{good}\nnot json\n{good}\n")),
        ] {
            let path = dir.join(format!("{name}.jsonl"));
            std::fs::write(&path, &content).unwrap();
            assert!(Store::open(&path).is_err(), "{name} must not open");
            // Refusal is read-only: nothing archived, nothing rewritten.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        }
        assert!(archives(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tail_read_matches_whole_file_display_parse_for_every_shape() {
        let dir = scratch_dir("tail");
        let s = legacy_snapshots();
        let a = serde_json::to_string(&s[0]).unwrap();
        let b = serde_json::to_string(&s[2]).unwrap();
        let cases = [
            String::new(),
            "\n".into(),
            "\n \n\n".into(),
            a.clone(),
            format!("{a}\n"),
            format!("{a}\n{b}\n"),
            format!("{a}\n{b}\n{}", &a[..10]),
            format!("{a}\n{b}\n\n  \n"),
            format!("{a}\r\n{b}\r\n"),
            format!("\n{b}\n"),
            format!("{a}\nnot json\n"),
        ];
        for (i, content) in cases.iter().enumerate() {
            let path = dir.join(format!("case-{i}.jsonl"));
            std::fs::write(&path, content).unwrap();
            let expected = state_for_display(content).map(|s| json(&s));
            // Tiny chunks force lines to straddle every chunk boundary.
            for chunk in [1, 2, 7, 64, 64 * 1024] {
                let got = tail_snapshot(&path, chunk).map(|s| json(&s));
                match (&expected, &got) {
                    (Ok(e), Ok(g)) => assert_eq!(e, g, "case {i} chunk {chunk}"),
                    (Err(_), Err(_)) => {}
                    _ => panic!("case {i} chunk {chunk}: {expected:?} vs {got:?}"),
                }
            }
        }
        assert_eq!(
            json(&state_for_display_from_file(&dir.join("missing.jsonl")).unwrap()),
            json(&State::default())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tail_read_never_touches_history_before_the_last_snapshot() {
        // Proves the read is bounded: a prefix the whole-file read cannot
        // even decode (invalid UTF-8) does not affect the tail read.
        let dir = scratch_dir("bounded");
        let path = dir.join("ledger.jsonl");
        let last = &legacy_snapshots()[2];
        let mut bytes = vec![0xff, 0xfe, b'\n'];
        bytes.extend(std::iter::repeat_n(b'x', 300_000));
        bytes.push(b'\n');
        bytes.extend(serde_json::to_vec(last).unwrap());
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        assert!(std::fs::read_to_string(&path).is_err());
        assert_eq!(
            json(&state_for_display_from_file(&path).unwrap()),
            json(last)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
