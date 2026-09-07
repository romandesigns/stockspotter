//! Alpaca paper execution. The endpoint is fixed; this module cannot submit live orders.
use crate::journal::{ExitReason, JournalEntry};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
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

#[derive(Default, Serialize, Deserialize)]
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

/// Exclusive process lock plus append/fsync before every broker mutation.
pub struct Store {
    file: File,
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
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let mut state = State::default();
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            state = serde_json::from_str(line)
                .context("invalid paper ledger; reconcile before resuming")?;
        }
        ensure!(
            content.is_empty() || content.ends_with('\n'),
            "incomplete paper ledger write; reconcile before resuming"
        );
        Ok((
            Self {
                file,
                _owner_lock: owner_lock,
            },
            state,
        ))
    }
    pub fn save(&mut self, state: &State) -> Result<()> {
        let result = (|| -> Result<()> {
            serde_json::to_writer(&mut self.file, state)?;
            self.file.write_all(b"\n")?;
            self.file.sync_all()?;
            Ok(())
        })();
        result.map_err(|e| PersistenceError(e.to_string()).into())
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
}
