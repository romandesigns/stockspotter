//! Load test for the 50 ms chart publication cadence.
//!
//! Answers one infrastructure question: does moving the provisional chart
//! publication interval from 500 ms to 50 ms create a backpressure problem in
//! the fanout, and does it buy enough freshness to be worth it?
//!
//! Nothing here touches ranking, detection or any strategy parameter, and no
//! value is tuned against any trading outcome. It measures capacity only.
//!
//! # Load model, taken from the 2026-09-21 live session
//!
//! Measured over a 300 s WebSocket capture of production: 5,718 bar updates
//! across 691 symbol-streams, 19.1 msg/s in total. Classifying each stream by
//! whether the 500 ms throttle is what limits it:
//!
//! | class | streams | now |
//! |---|---|---|
//! | throttle-bound (>=1.6/s) | 4 | 7.1 msg/s |
//! | near-bound (0.8-1.6/s) | 4 | 5.5 msg/s |
//! | trade-bound (<0.8/s, sparse) | 683 | 6.5 msg/s |
//!
//! 98.8% of streams are trade-bound: they publish as often as trades arrive,
//! far below the throttle, so the cadence change cannot affect them. Only the
//! 8 throttle- and near-bound streams can go faster. Projected total at
//! 50 ms: 141.1 msg/s, i.e. 7.4x current, not the 10x a naive per-symbol
//! reading suggests.
//!
//! Tiers below follow that model, plus stress cases above it.
//!
//! Run: cargo run -p ws-server --release --example chart_cadence_load

use std::time::{Duration, Instant};

use market_data::ScanEvent;
use tokio::sync::broadcast;

/// Mirrors ws-server's own BROADCAST_CAPACITY so the test exercises the real bound.
const BROADCAST_CAPACITY: usize = 16_384;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Frame {
    event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sent_at: Option<String>,
    #[serde(flatten)]
    event: ScanEvent,
}

fn bar(symbol: &str, interval: u32) -> ScanEvent {
    ScanEvent::BarUpdate {
        coverage: market_data::events::Coverage::Complete,
        symbol: symbol.into(),
        timestamp: "2026-09-21T15:37:00Z".parse().unwrap(),
        open: 10.0,
        high: 11.25,
        low: 9.75,
        close: 10.5,
        volume: 123_456,
        interval_secs: interval,
        is_final: false,
    }
}

struct Tier {
    name: &'static str,
    msgs_per_sec: f64,
    clients: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("chart cadence load test -- capacity only, no strategy parameter involved");
    println!("broadcast capacity: {BROADCAST_CAPACITY} frames\n");

    // 1. What one frame costs to serialise. This is the fanout's per-client
    //    per-message cost, and it is paid once per client, not once per frame.
    let f = Frame { event_id: "1:1".into(), sent_at: Some("2026-09-21T15:37:00.123456Z".into()), event: bar("GRML", 60) };
    let one = serde_json::to_string(&f).unwrap();
    let iters = 200_000;
    let t = Instant::now();
    let mut bytes = 0usize;
    for i in 0..iters {
        let mut g = f.clone();
        g.event_id = format!("1:{i}");
        bytes += serde_json::to_string(&g).unwrap().len();
    }
    let el = t.elapsed();
    let us_per_frame = el.as_secs_f64() * 1e6 / iters as f64;
    println!("serialisation: {} bytes/frame, {:.2} us/frame, {:.0} frames/s single-threaded",
             one.len(), us_per_frame, iters as f64 / el.as_secs_f64());
    println!("  checksum {bytes} bytes serialised (keeps the loop from being optimised away)");
    println!("  (includes the clone the fanout already performs per client)");
    // CPU is DERIVED from the measured per-frame cost rather than sampled: a
    // 3-second run cannot separate fanout CPU from runtime startup, and the
    // per-frame figure is the quantity that actually scales with load.
    for (label, rate, clients) in [
        ("projected 50ms, 3 clients", 141.1_f64, 3.0_f64),
        ("2x projected, 8 clients", 282.2, 8.0),
        ("every stream at cap, 8 clients", 13_820.0, 8.0),
    ] {
        let cpu_ms_per_s = rate * clients * us_per_frame / 1000.0;
        println!("  derived fanout CPU, {label}: {:.2} ms/s = {:.3}% of one core",
                 cpu_ms_per_s, cpu_ms_per_s / 10.0);
    }
    // The ring is a reservation, not a steady state.
    println!("  broadcast ring worst case: {} frames x {} bytes = {:.1} MB payload",
             BROADCAST_CAPACITY, one.len(), (BROADCAST_CAPACITY * one.len()) as f64 / 1_048_576.0);
    println!();

    // 2. Fanout behaviour at each modelled rate. Measures what the broadcast
    //    channel and its receivers actually do, including lag, which is the
    //    mechanism that would surface as stream_lagged in production.
    let tiers = [
        Tier { name: "A current, today's cohort", msgs_per_sec: 19.1, clients: 3 },
        Tier { name: "C projected at 50ms", msgs_per_sec: 141.1, clients: 3 },
        Tier { name: "D 2x projected (stress)", msgs_per_sec: 282.2, clients: 3 },
        Tier { name: "D 2x projected, 8 clients", msgs_per_sec: 282.2, clients: 8 },
        Tier { name: "E every stream at cap", msgs_per_sec: 13_820.0, clients: 3 },
        Tier { name: "E' 2x E (absurd ceiling)", msgs_per_sec: 27_640.0, clients: 8 },
    ];

    println!("{:<28} {:>9} {:>7} {:>10} {:>10} {:>9} {:>9}",
             "tier", "msg/s", "clients", "sent", "delivered", "lagged", "MB/s");
    for tier in tiers {
        let (tx, _) = broadcast::channel::<Frame>(BROADCAST_CAPACITY);
        let mut handles = Vec::new();
        for _ in 0..tier.clients {
            let mut rx = tx.subscribe();
            handles.push(tokio::spawn(async move {
                let mut got = 0u64;
                let mut lagged = 0u64;
                let mut bytes = 0u64;
                loop {
                    match rx.recv().await {
                        Ok(frame) => {
                            // The real per-client cost: serialise for this socket.
                            bytes += serde_json::to_string(&frame).unwrap().len() as u64;
                            got += 1;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => lagged += n,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                (got, lagged, bytes)
            }));
        }

        let secs = 3.0;
        let total = (tier.msgs_per_sec * secs) as u64;
        // Paced emission, so this measures steady-state behaviour rather than
        // how fast a tight loop can fill a channel.
        let interval = Duration::from_secs_f64(1.0 / tier.msgs_per_sec);
        let start = Instant::now();
        let mut sent = 0u64;
        for i in 0..total {
            let mut g = f.clone();
            g.event_id = format!("1:{i}");
            let _ = tx.send(g);
            sent += 1;
            let due = start + interval.mul_f64(i as f64 + 1.0);
            let now = Instant::now();
            if due > now {
                tokio::time::sleep(due - now).await;
            }
        }
        let wall = start.elapsed().as_secs_f64();
        drop(tx);
        let mut got = 0u64;
        let mut lagged = 0u64;
        let mut bytes = 0u64;
        for h in handles {
            let (g, l, b) = h.await.unwrap();
            got += g;
            lagged += l;
            bytes += b;
        }
        println!("{:<28} {:>9.1} {:>7} {:>10} {:>10} {:>9} {:>9.2}",
                 tier.name, sent as f64 / wall, tier.clients, sent, got, lagged,
                 bytes as f64 / wall / 1_048_576.0);
    }

    println!("\nlagged > 0 means a receiver fell behind the {BROADCAST_CAPACITY}-frame ring and");
    println!("the server would emit stream_lagged. That is the backpressure signal.");
}
