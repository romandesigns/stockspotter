//! Offline loopback fixture. No .env, production APIs, detectors or trading tasks.
#![allow(dead_code)]
#[path = "../src/access.rs"]
mod access;
#[path = "../../../tools/chart-audit/legacy.rs"]
mod legacy;
#[path = "../src/protocol.rs"]
mod protocol;
#[path = "../src/server.rs"]
mod server;
use market_data::{chart_bars::ChartBars, AlpacaConfig, AlpacaMessage, AlpacaStream, ScanEvent};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::broadcast;
const BROADCAST_CAPACITY: usize = 16_384;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Known fixture token, scoped to this process and loopback listeners only.
    std::env::set_var(
        "STOCKSPOTTER_API_TOKEN",
        "chart-audit-loopback-fixture-only",
    );
    let (before, _) = broadcast::channel::<ScanEvent>(BROADCAST_CAPACITY);
    let (after, _) = broadcast::channel::<ScanEvent>(BROADCAST_CAPACITY);
    for (port, tx) in [(19872, before.clone()), (19873, after.clone())] {
        tokio::spawn(async move {
            server::run(
                &format!("127.0.0.1:{port}"),
                tx,
                Arc::new(access::AuthLimiter::new()),
            )
            .await
            .unwrap();
        });
    }
    let receipts = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let http_receipts = receipts.clone();
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/ingress",
            axum::routing::get(move || {
                let records = http_receipts.clone();
                async move { axum::Json(records.lock().unwrap().clone()) }
            }),
        );
        axum::serve(
            tokio::net::TcpListener::bind("127.0.0.1:19874")
                .await
                .unwrap(),
            app,
        )
        .await
        .unwrap();
    });
    let cfg = AlpacaConfig {
        api_key: "fixture".into(),
        api_secret: "fixture".into(),
        feed: "fixture".into(),
        market_ws: "ws://127.0.0.1:19871/source".into(),
        data_base: "http://127.0.0.1:19871".into(),
        trading_base: "http://127.0.0.1:19871".into(),
        fmp_api_key: None,
    };
    let mut stream = AlpacaStream::connect(&cfg, &["AUDIT".into()]).await?;
    let mut old60 = HashMap::new();
    let mut old30 = HashMap::new();
    let mut new60: HashMap<String, ChartBars> = HashMap::new();
    let mut new30: HashMap<String, ChartBars> = HashMap::new();
    let mut tick = tokio::time::interval(market_data::chart_bars::FLUSH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    println!("fixture ready: baseline 19872; candidate 19873");
    loop {
        tokio::select! {
            _ = tick.tick() => {
                for (bars, secs) in [(&mut new60,60),(&mut new30,30)] {
                    for (symbol,state) in bars.iter_mut() {
                        for event in state.flush(symbol,secs,Instant::now(),false) { let _ = after.send(event); }
                    }
                }
            }
            batch = stream.next_batch() => {
                let Some(batch) = batch? else { break; };
                let ingress = chrono::Utc::now().timestamp_nanos_opt().unwrap() as f64 / 1e6;
                for message in batch {
                    if let AlpacaMessage::Trade(trade) = message {
                        legacy::on_trade(&trade,&mut old60,&mut old30,&before);
                        for (bars,secs) in [(&mut new60,60),(&mut new30,30)] {
                            for event in bars.entry(trade.symbol.clone()).or_default().on_trade(&trade,secs,Instant::now()) { let _ = after.send(event); }
                        }
                        receipts.lock().unwrap().push(serde_json::json!({"symbol":trade.symbol,"seq":((trade.price-100.)*10000.).round(),"ingress":ingress}));
                    }
                }
            }
        }
    }
    Ok(())
}
