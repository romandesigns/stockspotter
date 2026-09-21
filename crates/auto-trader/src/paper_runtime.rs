//! Paper-only runner. Broker polling continues even when the scanner disconnects.
use crate::{
    client::AutoTraderClient,
    config::Config,
    engine::Engine,
    journal::{ExitReason, JournalEntry},
    paper::*,
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use market_data::ScanEvent;
use std::{collections::HashMap, path::PathBuf, time::Duration as StdDuration};
use tracing::{info, warn};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    #[tokio::test]
    async fn lost_submission_response_is_recovered_after_restart_without_a_second_post() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let broker = Broker::mock(format!("http://{}", listener.local_addr().unwrap()));
        let at = Utc::now();
        let path = std::env::temp_dir().join(format!(
            "ss-paper-network-{}-{}.jsonl",
            std::process::id(),
            at.timestamp_nanos_opt().unwrap()
        ));
        let (mut store, mut state) = Store::open(&path).unwrap();
        state.trades.push(Trade {
            proposal: JournalEntry::Entered {
                symbol: "TEST".into(),
                strategy: backtest_metrics::Strategy::IgnitionDetector,
                entry_price: 10.,
                qty: 3,
                position_size_usd: 30.,
                target_price: 10.2,
                stop_price: 9.8,
                entered_at: at,
                momentum_overall: 0.9,
                momentum_volume_confirmation: 0.9,
                catalyst_tags: vec![],
            },
            buy: Intent {
                client_id: "durable-test".into(),
                symbol: "TEST".into(),
                side: "buy".into(),
                qty: 3,
                limit: Some("10.02".into()),
                created_at: at,
                attempted: false,
                local_canceled: false,
                attempted_at: None,
                last_received_at: None,
                order: None,
                first_fill_at: None,
            },
            sells: vec![],
            adjustments: vec![],
            exit_reason: None,
        });
        store.save(&state).unwrap();
        let persisted = path.clone();
        let server = std::thread::spawn(move || {
            for step in 0..3 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(StdDuration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let response=match step {
                    0=>{assert!(request.starts_with("GET /v2/clock "));serde_json::json!({"timestamp":at,"is_open":true,"next_close":at+Duration::hours(1)})},
                    1=>{
                        assert!(request.starts_with("POST /v2/orders "));
                        let body:serde_json::Value=serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["client_order_id"],"durable-test");assert_eq!(body["time_in_force"],"ioc");
                        assert_eq!(body["type"],"limit");assert_eq!(body["extended_hours"],false);
                        let disk=state_for_display(&std::fs::read_to_string(&persisted).unwrap()).unwrap();
                        assert!(disk.trades[0].buy.attempted);assert!(disk.trades[0].buy.attempted_at.is_some());
                        // Broker accepted it, but the response connection was lost.
                        continue;
                    },
                    _=>{
                        assert!(request.starts_with("GET /v2/orders:by_client_order_id?client_order_id=durable-test "));
                        serde_json::json!({"id":"broker-id","client_order_id":"durable-test","symbol":"TEST","side":"buy",
                            "status":"filled","qty":"3","filled_qty":"3","filled_avg_price":"10.01","filled_at":at,"updated_at":at})
                    }
                }.to_string();
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
            }
        });
        sync_intent(&broker, &mut store, &mut state, 0, None, true)
            .await
            .unwrap();
        assert!(state.trades[0].buy.attempted);
        assert!(state.trades[0].buy.order.is_none());
        drop(store);
        let (mut store, mut state) = Store::open(&path).unwrap();
        sync_intent(&broker, &mut store, &mut state, 0, None, true)
            .await
            .unwrap();
        assert_eq!(state.trades[0].remaining().unwrap(), 3);
        assert_eq!(state.trades.len(), 1);
        // Terminal reconciliation must not make another network request.
        sync_intent(&broker, &mut store, &mut state, 0, None, true)
            .await
            .unwrap();
        server.join().unwrap();
        drop(store);
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}

fn event_time(event: &ScanEvent) -> Option<DateTime<Utc>> {
    match event {
        ScanEvent::IgnitionEvent { timestamp, .. }
        | ScanEvent::ConsolidationEvent { timestamp, .. } => Some(*timestamp),
        _ => None,
    }
}

fn intent_mut(state: &mut State, trade: usize, sell: Option<usize>) -> &mut Intent {
    if let Some(i) = sell {
        &mut state.trades[trade].sells[i]
    } else {
        &mut state.trades[trade].buy
    }
}

/// An ambiguous submission is looked up, never blindly submitted again.
async fn sync_intent(
    broker: &Broker,
    store: &mut Store,
    state: &mut State,
    trade: usize,
    sell: Option<usize>,
    can_submit: bool,
) -> Result<()> {
    let intent = intent_mut(state, trade, sell).clone();
    if intent.done() {
        return Ok(());
    }
    if intent.attempted {
        match broker.lookup(&intent.client_id).await? {
            Some(order) => {
                if intent.order.as_ref() != Some(&order) {
                    intent_mut(state, trade, sell).update(order, Utc::now())?;
                    store.save(state)?;
                }
            }
            None => {
                warn!(symbol=%intent.symbol,client_order_id=%intent.client_id,"submission outcome remains unknown; lookup only, no duplicate order")
            }
        }
        return Ok(());
    }
    if !can_submit {
        return Ok(());
    }
    let clock = broker.clock().await?;
    if !clock.is_open {
        return Ok(());
    }
    if intent.side == "buy" && !entry_window(&clock, intent.created_at) {
        intent_mut(state, trade, sell).local_canceled = true;
        store.save(state)?;
        return Ok(());
    }
    if intent.side == "buy" && Utc::now() - intent.created_at > Duration::seconds(2) {
        intent_mut(state, trade, sell).local_canceled = true;
        store.save(state)?;
        return Ok(());
    }
    intent_mut(state, trade, sell).attempted = true;
    intent_mut(state, trade, sell).attempted_at = Some(Utc::now());
    store.save(state)?;
    // Durably record the attempt BEFORE the request, including ambiguous failures.
    match broker.submit(&intent).await {
        Ok(order) => {
            intent_mut(state, trade, sell).update(order, Utc::now())?;
            store.save(state)?;
        }
        Err(error) => {
            warn!(%error,client_order_id=%intent.client_id,"paper submission failed or response lost; reconcile by client ID")
        }
    }
    Ok(())
}

async fn reconcile(broker: &Broker, store: &mut Store, state: &mut State) -> Result<()> {
    for i in 0..state.trades.len() {
        sync_intent(broker, store, state, i, None, false).await?;
        for j in 0..state.trades[i].sells.len() {
            sync_intent(broker, store, state, i, Some(j), false).await?;
        }
    }
    Ok(())
}

async fn exit_trade(
    broker: &Broker,
    store: &mut Store,
    state: &mut State,
    i: usize,
    reason: ExitReason,
) -> Result<()> {
    let t = &state.trades[i];
    if !t.buy.done() || t.remaining()? == 0 {
        return Ok(());
    }
    if let Some(j) = t.sells.iter().position(|o| !o.done()) {
        sync_intent(broker, store, state, i, Some(j), true).await?;
        return Ok(());
    }
    ensure!(
        !t.sells
            .last()
            .and_then(|o| o.order.as_ref())
            .is_some_and(|o| o.status == "rejected"),
        "paper exit was rejected; inspect broker reason before retrying"
    );
    // Only sell this runner's confirmed remaining quantity. Never use close-all.
    let positions = broker.positions().await?;
    ensure!(
        !broker
            .open_orders()
            .await?
            .iter()
            .any(|o| o.symbol == t.buy.symbol),
        "an open broker order still owns this symbol; reconcile before another exit"
    );
    let remaining = t.remaining()?;
    let actual = positions
        .iter()
        .find(|p| p.symbol == t.buy.symbol)
        .context("broker position missing before exit")?;
    ensure!(
        actual.side == "long" && shares(&actual.qty)? == remaining,
        "broker position differs before exit; reconcile first"
    );
    let symbol = t.buy.symbol.clone();
    let client_id = format!("{}-s{}", t.buy.client_id, t.sells.len());
    let j = state.trades[i].sells.len();
    state.trades[i].exit_reason = Some(reason);
    state.trades[i].sells.push(Intent {
        client_id,
        symbol,
        side: "sell".into(),
        qty: remaining,
        limit: None,
        created_at: Utc::now(),
        attempted: false,
        local_canceled: false,
        attempted_at: None,
        last_received_at: None,
        order: None,
        first_fill_at: None,
    });
    store.save(state)?;
    sync_intent(broker, store, state, i, Some(j), true).await
}

async fn latest_ask(cfg: &market_data::AlpacaConfig, symbol: &str) -> Result<f64> {
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = reqwest::Url::parse(&cfg.data_base)?;
    ensure!(
        url.scheme() == "https" && url.host_str() == Some("data.alpaca.markets"),
        "paper quote source must be Alpaca market data"
    );
    let value: serde_json::Value = client
        .get(format!(
            "{}/v2/stocks/{symbol}/quotes/latest",
            cfg.data_base.trim_end_matches('/')
        ))
        .query(&[("feed", cfg.feed.as_str())])
        .header("APCA-API-KEY-ID", &cfg.api_key)
        .header("APCA-API-SECRET-KEY", &cfg.api_secret)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let quote = &value["quote"];
    let at: DateTime<Utc> = quote["t"]
        .as_str()
        .context("quote timestamp missing")?
        .parse()?;
    let age = Utc::now() - at;
    let ask = quote["ap"].as_f64().context("ask missing")?;
    let bid = quote["bp"].as_f64().context("bid missing")?;
    ensure!(
        age >= Duration::seconds(-1) && age <= Duration::seconds(1),
        "entry quote is stale"
    );
    ensure!(
        ask.is_finite()
            && bid.is_finite()
            && bid > 0.
            && ask >= bid
            && quote["as"].as_u64().is_some_and(|s| s > 0)
            && quote["bs"].as_u64().is_some_and(|s| s > 0),
        "entry quote is invalid"
    );
    Ok(ask)
}

async fn process_event(
    event: &ScanEvent,
    engine: &mut Engine,
    broker: &Broker,
    store: &mut Store,
    state: &mut State,
    cfg: &Config,
    market: &market_data::AlpacaConfig,
    context_at: &mut HashMap<String, DateTime<Utc>>,
    allow_entry: bool,
) -> Result<()> {
    if let ScanEvent::MomentumUpdate {
        symbol, timestamp, ..
    } = event
    {
        if context_at
            .get(symbol)
            .is_some_and(|previous| timestamp < previous)
        {
            return Ok(());
        }
        context_at.insert(symbol.clone(), *timestamp);
    }
    let proposals = engine.broker_proposals(event, &state.history()?);
    for proposal in proposals {
        match &proposal {
            JournalEntry::StopAdjusted { symbol, .. } => {
                if let Some(t) = state
                    .trades
                    .iter_mut()
                    .rev()
                    .find(|t| t.buy.symbol == *symbol && t.remaining().is_ok_and(|q| q > 0))
                {
                    t.adjustments.push(proposal.clone());
                    store.save(state)?;
                }
            }
            JournalEntry::Exited {
                symbol,
                exit_reason,
                ..
            } => {
                if let Some(i) = state
                    .trades
                    .iter()
                    .rposition(|t| t.buy.symbol == *symbol && t.remaining().is_ok_and(|q| q > 0))
                {
                    state.trades[i].exit_reason = Some(*exit_reason);
                    store.save(state)?;
                    if broker.clock().await?.is_open {
                        exit_trade(broker, store, state, i, *exit_reason).await?;
                    }
                }
            }
            JournalEntry::Entered {
                symbol,
                entered_at,
                position_size_usd,
                ..
            } if allow_entry => {
                let now = Utc::now();
                if now - *entered_at > Duration::seconds(2)
                    || *entered_at - now > Duration::seconds(1)
                {
                    continue;
                }
                if context_at.get(symbol).is_none_or(|at| {
                    now - *at > Duration::seconds(90) || *at > now + Duration::seconds(1)
                }) {
                    info!(%symbol, "paper entry skipped: fresh momentum context unavailable");
                    continue;
                }
                if state.active_count()? >= cfg.max_concurrent_positions {
                    continue;
                }
                if state.trades.iter().any(|t| {
                    t.buy.symbol == *symbol && t.buy.created_at.date_naive() == now.date_naive()
                }) {
                    continue;
                }
                reconcile(broker, store, state).await?;
                let account = broker.account().await?;
                ensure!(account.id == state.account_id, "paper account changed");
                state.verify_positions(&broker.positions().await?)?;
                let own: Vec<_> = state
                    .trades
                    .iter()
                    .flat_map(|t| std::iter::once(&t.buy).chain(t.sells.iter()))
                    .map(|o| o.client_id.as_str())
                    .collect();
                ensure!(
                    broker
                        .open_orders()
                        .await?
                        .iter()
                        .all(|o| own.contains(&o.client_order_id.as_str())),
                    "unmanaged orders exist; entries paused"
                );
                let reserved: f64 = state
                    .trades
                    .iter()
                    .filter(|t| !t.buy.done())
                    .map(|t| {
                        t.buy
                            .limit
                            .as_deref()
                            .map(number)
                            .transpose()
                            .map(|p| p.unwrap_or(0.) * t.buy.qty as f64)
                    })
                    .collect::<Result<Vec<_>>>()?
                    .iter()
                    .sum();
                let budget = position_size_usd
                    .min(cfg.position_size_usd)
                    .min(number(&account.cash)? - reserved)
                    .min(number(&account.buying_power)? - reserved);
                let (limit, qty) = entry_limit(latest_ask(market, symbol).await?, budget)?;
                let clock = broker.clock().await?;
                if !entry_window(&clock, *entered_at) {
                    continue;
                }
                let id = format!(
                    "ss-paper-{}-{symbol}",
                    entered_at
                        .timestamp_nanos_opt()
                        .context("invalid signal timestamp")?
                );
                state.trades.push(Trade {
                    proposal: proposal.clone(),
                    buy: Intent {
                        client_id: id,
                        symbol: symbol.clone(),
                        side: "buy".into(),
                        qty,
                        limit: Some(limit),
                        created_at: *entered_at,
                        attempted: false,
                        local_canceled: false,
                        attempted_at: None,
                        last_received_at: None,
                        order: None,
                        first_fill_at: None,
                    },
                    sells: vec![],
                    adjustments: vec![],
                    exit_reason: None,
                });
                store.save(state)?;
                let i = state.trades.len() - 1;
                sync_intent(broker, store, state, i, None, true).await?;
            }
            JournalEntry::Skipped { symbol, reason, .. } => {
                info!(%symbol,?reason,"paper entry skipped by strategy rules")
            }
            _ => {}
        }
    }
    Ok(())
}

pub async fn run(check_only: bool) -> Result<()> {
    let broker = Broker::from_env()?;
    let account = broker.account().await?;
    let clock = broker.clock().await?;
    let positions = broker.positions().await?;
    let orders = broker.open_orders().await?;
    info!(
        market_open = clock.is_open,
        positions = positions.len(),
        open_orders = orders.len(),
        "Alpaca PAPER account connection verified"
    );
    if check_only {
        return Ok(());
    }
    let cfg = Config::from_env();
    ensure!(
        cfg.position_size_usd.is_finite()
            && cfg.position_size_usd > 0.
            && cfg.position_size_usd <= 500.,
        "initial paper integration allows at most $500 per entry"
    );
    ensure!(
        cfg.max_concurrent_positions > 0 && cfg.max_concurrent_positions <= 4,
        "initial paper integration allows at most four positions"
    );
    let market = market_data::AlpacaConfig::from_env()?;
    let path = PathBuf::from(
        std::env::var("AUTO_TRADER_PAPER_LEDGER_PATH")
            .unwrap_or_else(|_| "data/alpaca_paper_ledger.jsonl".into()),
    );
    let (mut store, mut state) = Store::open(&path)?;
    if state.account_id.is_empty() {
        state.account_id = account.id.clone();
        store.save(&state)?;
    }
    ensure!(
        state.account_id == account.id,
        "ledger belongs to a different paper account"
    );
    reconcile(&broker, &mut store, &mut state).await?;
    state.verify_positions(&broker.positions().await?)?;
    let mut engine = Engine::new(cfg.clone());
    if let Ok(text) = std::fs::read_to_string("data/auto_trader_strategy_config.json") {
        let policy: backtest_metrics::StrategyConfigFile =
            serde_json::from_str(&text).context("invalid initial strategy policy")?;
        engine.set_enabled_strategies(&policy.decisions(), Utc::now());
    }
    let mut context_at = HashMap::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(512);
    let url = cfg.ws_url.clone();
    tokio::spawn(async move {
        loop {
            match AutoTraderClient::connect(&url).await {
                Ok(mut client) => loop {
                    match client.next_event().await {
                        Ok(Some(event)) => {
                            if sender.send(event).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            warn!(%error,"paper scanner disconnected; broker polling continues");
                            break;
                        }
                    }
                },
                Err(error) => {
                    warn!(%error,"paper scanner connection unavailable; broker polling continues")
                }
            }
            tokio::time::sleep(StdDuration::from_secs(5)).await;
        }
    });
    let mut tick = tokio::time::interval(StdDuration::from_secs(5));
    let mut last_bar: HashMap<String, DateTime<Utc>> = HashMap::new();
    loop {
        tokio::select! {
            event=receiver.recv()=>{
                let event=event.context("scanner receiver closed")?;
                if event_time(&event).is_some_and(|at|Utc::now()-at>Duration::seconds(2)) {continue;}
                if let Err(error)=process_event(&event,&mut engine,&broker,&mut store,&mut state,&cfg,&market,&mut context_at,true).await {
                    if error.downcast_ref::<PersistenceError>().is_some() {return Err(error);}
                    warn!(%error,"paper event could not complete; durable orders will reconcile");
                }
            }
            _=tick.tick()=>{
                if let Err(error)=reconcile(&broker,&mut store,&mut state).await {
                    if error.downcast_ref::<PersistenceError>().is_some() {return Err(error);}
                    warn!(%error,"paper reconciliation unavailable");continue;
                }
                let clock=match broker.clock().await {Ok(c)=>c,Err(e)=>{warn!(error=%e,"paper clock unavailable");continue;}};
                // Existing review policy is honored; no profitability claim is inferred from enablement.
                if let Ok(text)=std::fs::read_to_string("data/auto_trader_strategy_config.json") {
                    match serde_json::from_str::<backtest_metrics::StrategyConfigFile>(&text) {
                        Ok(config)=>{engine.set_enabled_strategies(&config.decisions(),Utc::now());}
                        Err(error)=>warn!(%error,"invalid strategy config; keeping last policy"),
                    }
                }
                if !clock.is_open {continue;}
                for i in 0..state.trades.len() {
                    if !state.trades[i].active()? {continue;}
                    // Recover an intent persisted before its attempted flag, but never replay an old buy.
                    if let Err(error)=sync_intent(&broker,&mut store,&mut state,i,None,true).await {
                        if error.downcast_ref::<PersistenceError>().is_some() {return Err(error);}
                        warn!(%error,"paper intent reconciliation will retry");continue;
                    }
                    let t=&state.trades[i];
                    if t.remaining()?==0 {continue;}
                    let holding_since=t.buy.first_fill_at.context("missing fill time")?;
                    let max_hold=match &t.proposal {JournalEntry::Entered{strategy,..}=>backtest_metrics::OutcomeThresholds::for_strategy(*strategy).lookforward_bars as i64,_=>0};
                    let must_exit=clock.next_close-clock.timestamp<=Duration::minutes(1) || clock.timestamp-holding_since>=Duration::minutes(max_hold);
                    if must_exit || t.exit_reason.is_some() {
                        let reason=t.exit_reason.unwrap_or(ExitReason::Timeout);
                        if let Err(error)=exit_trade(&broker,&mut store,&mut state,i,reason).await {
                            if error.downcast_ref::<PersistenceError>().is_some() {return Err(error);}
                            warn!(%error,"paper exit pending reconciliation");
                        }
                        continue;
                    }
                    let symbol=t.buy.symbol.clone();
                    let since=last_bar.get(&symbol).copied().unwrap_or(holding_since-Duration::minutes(1));
                    if clock.timestamp-since<Duration::seconds(60) {continue;}
                    let fetched=tokio::time::timeout(StdDuration::from_secs(5),market_data::fetch_recent_minute_bars(&market,&symbol,&since.to_rfc3339(),&clock.timestamp.to_rfc3339())).await;
                    match fetched {
                        Ok(Ok(mut bars))=>{
                            bars.sort_by_key(|b|b.timestamp);
                            for b in bars {
                                let completed=b.timestamp+Duration::minutes(1);
                                if completed<=holding_since || completed>clock.timestamp || last_bar.get(&symbol).is_some_and(|at|completed<=*at) {continue;}
                                last_bar.insert(symbol.clone(),completed);
                                let event=ScanEvent::BarUpdate{ coverage: market_data::events::Coverage::Unknown,symbol:b.symbol,timestamp:b.timestamp,open:b.open,high:b.high,low:b.low,close:b.close,volume:b.volume,interval_secs:60,is_final:true};
                                if let Err(error)=process_event(&event,&mut engine,&broker,&mut store,&mut state,&cfg,&market,&mut context_at,false).await {
                                    if error.downcast_ref::<PersistenceError>().is_some() {return Err(error);}
                                    warn!(%error,"paper recovery bar could not complete");
                                }
                            }
                        }
                        _=>warn!(%symbol,"paper management bar fetch unavailable; timer exits remain active"),
                    }
                }
            }
        }
    }
}
