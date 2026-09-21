// Extracted verbatim closure from baseline 143cb8a live.rs, with its original state.
use std::{collections::HashMap, time::{Duration, Instant}};
use chrono::{DateTime,Utc};
use market_data::{Trade,ScanEvent};
use tokio::sync::broadcast;
const LIVE_BAR_BROADCAST_INTERVAL: Duration = Duration::from_millis(500);
pub struct LiveBar { bucket_start:DateTime<Utc>,open:f64,high:f64,low:f64,close:f64,volume:u64,last_broadcast:Instant }
fn floor_to_interval(t:DateTime<Utc>,interval_secs:i64)->DateTime<Utc>{
 let secs=t.timestamp();let floored=secs-secs.rem_euclid(interval_secs);DateTime::from_timestamp(floored,0).unwrap_or(t)
}
pub fn on_trade(trade:&Trade,live_bars:&mut HashMap<String,LiveBar>,sub_minute_bars:&mut HashMap<String,LiveBar>,events:&broadcast::Sender<ScanEvent>) {
                            let update_live_bar = |bars: &mut HashMap<String, LiveBar>, interval_secs: i64| {
                                let bucket_start = floor_to_interval(trade.timestamp, interval_secs);
                                let state = bars.entry(trade.symbol.clone()).or_insert_with(|| LiveBar {
                                    bucket_start,
                                    open: trade.price,
                                    high: trade.price,
                                    low: trade.price,
                                    close: trade.price,
                                    volume: 0,
                                    // Backdated so the very first trade of a
                                    // newly-tracked symbol broadcasts
                                    // immediately instead of waiting out a
                                    // full throttle interval first.
                                    last_broadcast: Instant::now() - LIVE_BAR_BROADCAST_INTERVAL,
                                });
                                if state.bucket_start != bucket_start {
                                    // A new bucket started -- for the 60s
                                    // map, Alpaca's own official Bar for the
                                    // just-finished minute arrives separately
                                    // (handled above) and is authoritative;
                                    // this just starts tracking the new one
                                    // live. The 30s map never gets that
                                    // correction (SUB_MINUTE_BUCKET_SECS's
                                    // own doc comment).
                                    *state = LiveBar {
                                        bucket_start,
                                        open: trade.price,
                                        high: trade.price,
                                        low: trade.price,
                                        close: trade.price,
                                        volume: 0,
                                        last_broadcast: state.last_broadcast,
                                    };
                                }
                                state.high = state.high.max(trade.price);
                                state.low = state.low.min(trade.price);
                                state.close = trade.price;
                                state.volume += trade.size;

                                if state.last_broadcast.elapsed() >= LIVE_BAR_BROADCAST_INTERVAL {
                                    state.last_broadcast = Instant::now();
                                    let _ = events.send(ScanEvent::BarUpdate {
                                        // The extracted BASELINE aggregator, kept for
                                        // comparison. It has no coverage concept at all,
                                        // which is precisely what the new contract adds,
                                        // so Unknown is the honest value here.
                                        coverage: market_data::events::Coverage::Unknown,
                                        symbol: trade.symbol.clone(),
                                        timestamp: state.bucket_start,
                                        open: state.open,
                                        high: state.high,
                                        low: state.low,
                                        close: state.close,
                                        volume: state.volume,
                                        interval_secs: interval_secs as u32,
                                        is_final: false,
                                    });
                                }
                            };

 update_live_bar(live_bars,60); update_live_bar(sub_minute_bars,30);
}

#[cfg(test)]
mod audit_characterization {
    use super::*;
    fn t(at:&str,price:f64,size:u64)->Trade {
        Trade {symbol:"AUDIT".into(),timestamp:at.parse().unwrap(),price,size,conditions:vec![]}
    }
    #[test]
    fn baseline_discards_unpublished_rollover_tail() {
        let (tx,mut rx)=broadcast::channel(32);let mut a=HashMap::new();let mut b=HashMap::new();
        on_trade(&t("2026-09-18T13:29:59.900Z",10.,2),&mut a,&mut b,&tx);
        on_trade(&t("2026-09-18T13:29:59.950Z",15.,3),&mut a,&mut b,&tx);
        on_trade(&t("2026-09-18T13:30:00Z",20.,4),&mut a,&mut b,&tx);
        let mut thirty=Vec::new();while let Ok(e)=rx.try_recv(){if let ScanEvent::BarUpdate{interval_secs:30,high,volume,..}=e{thirty.push((high,volume));}}
        assert_eq!(thirty,vec![(10.,2)]); // Oracle for previous bucket is high=15,volume=5.
    }
    #[test]
    fn baseline_late_trade_resets_newer_bucket_and_loses_volume() {
        let (tx,_rx)=broadcast::channel(32);let mut a=HashMap::new();let mut b=HashMap::new();
        for trade in [t("2026-09-18T13:30:00Z",20.,4),t("2026-09-18T13:29:59Z",8.,1),t("2026-09-18T13:30:01Z",21.,5)]{
            on_trade(&trade,&mut a,&mut b,&tx);
        }
        assert_eq!((b["AUDIT"].open,b["AUDIT"].volume),(21.,5)); // Correct: open=20,volume=9.
    }
}
