//! Separate read-only subscriber and wire channel. Frozen ScanEvent, OI,
//! observer, push and auto-trader never consume experimental strategy outputs.
use market_data::ScanEvent;
use momentum_strategies::{Bar, Config, Event, Monitor};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const MAX_SYMBOLS: usize = 256;
const MAX_EVENTS: usize = 100;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(rename = "type")]
    event_type: &'static str,
    pub iac_enabled: bool,
    pub extreme_momentum_enabled: bool,
    pub healthy: bool,
    pub source_at_ms: Option<i64>,
    pub lagged_events: u64,
    pub evictions: u64,
    pub processing_warnings: u64,
    pub budget_breaches: u64,
    pub ignored_provisional: u64,
    pub recent_evicted_symbols: Vec<String>,
    pub active_symbols: usize,
    pub reason: String,
    pub coverage: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Message {
    Update(Event),
    Status(Status),
}

#[derive(Clone)]
pub struct Hub {
    pub output: broadcast::Sender<Message>,
    state: Arc<Mutex<State>>,
}
struct State {
    status: Status,
    history: VecDeque<Event>,
}

impl Hub {
    pub fn new(iac: bool, extreme: bool) -> Self {
        let (output, _) = broadcast::channel(128);
        Self {
            output,
            state: Arc::new(Mutex::new(State {
                status: Status {
                    event_type: "experimental_strategy_status",
                    iac_enabled: iac,
                    extreme_momentum_enabled: extreme,
                    healthy: true,
                    source_at_ms: None,
                    lagged_events: 0,
                    evictions: 0,
                    processing_warnings: 0,
                    budget_breaches: 0,
                    ignored_provisional: 0,
                    recent_evicted_symbols: Vec::new(),
                    active_symbols: 0,
                    reason: if iac || extreme {
                        "warming_up"
                    } else {
                        "disabled"
                    }
                    .into(),
                    coverage: "observed_subscribed_symbols",
                },
                history: VecDeque::new(),
            })),
        }
    }
    pub fn enabled(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.status.iac_enabled || s.status.extreme_momentum_enabled
    }
    pub fn snapshot(&self) -> Vec<Message> {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = vec![Message::Status(s.status.clone())];
        out.extend(s.history.iter().cloned().map(Message::Update));
        out
    }
    fn publish(&self, events: Vec<Event>) {
        for event in events {
            {
                let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
                s.history.push_back(event.clone());
                while s.history.len() > MAX_EVENTS {
                    s.history.pop_front();
                }
            }
            let _ = self.output.send(Message::Update(event));
        }
    }
    fn status(&self) {
        let status = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .status
            .clone();
        let _ = self.output.send(Message::Status(status));
    }
}

pub fn start(events: &broadcast::Sender<ScanEvent>) -> Hub {
    let iac = std::env::var("IAC_ENABLED").ok().as_deref() == Some("true");
    let extreme = std::env::var("EXTREME_MOMENTUM_ENABLED").ok().as_deref() == Some("true");
    let hub = Hub::new(iac, extreme);
    if !hub.enabled() {
        return hub;
    }
    let rx = events.subscribe();
    let output = hub.clone();
    tokio::spawn(async move {
        run(rx, output, iac, extreme).await;
    });
    hub
}

async fn run(mut input: broadcast::Receiver<ScanEvent>, hub: Hub, iac: bool, extreme: bool) {
    let run_id = format!(
        "experimental:{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let mut monitors: HashMap<String, Monitor> = HashMap::new();
    let mut order = VecDeque::new();
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut consecutive_budget_breaches = 0u32;
    loop {
        tokio::select! {
            _=timer.tick()=>{
                let now=chrono::Utc::now().timestamp_millis();
                for monitor in monitors.values_mut() {
                    if monitor.last_available_ms().is_some_and(|t| now-t>120_000) {
                        hub.publish(monitor.on_gap(now,"source_idle"));
                    }
                }
                hub.status();
            }
            received=input.recv()=>match received {
                Ok(ScanEvent::BarUpdate {symbol,timestamp,open,high,low,close,volume,is_final,interval_secs})=>{
                    let now=chrono::Utc::now().timestamp_millis();
                    let started=std::time::Instant::now();
                    if !is_final || interval_secs!=60 {
                        hub.state.lock().unwrap_or_else(|p| p.into_inner()).status.ignored_provisional+=1;
                        continue;
                    }
                    if !monitors.contains_key(&symbol) {
                        if monitors.len()>=MAX_SYMBOLS {
                            if let Some(old)=order.pop_front() {
                                if let Some(mut monitor)=monitors.remove(&old) {hub.publish(monitor.on_gap(now,"capacity_eviction"));}
                                let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                                s.status.evictions+=1;s.status.recent_evicted_symbols.push(old);
                                if s.status.recent_evicted_symbols.len()>100 {s.status.recent_evicted_symbols.remove(0);}
                            }
                        }
                        let Ok(monitor)=Monitor::new(symbol.clone(),run_id.clone(),Config::default(),iac,extreme) else {continue;};
                        order.push_back(symbol.clone());monitors.insert(symbol.clone(),monitor);
                    }
                    // LRU access, not insertion FIFO. Eviction history is explicitly bounded.
                    order.retain(|key| key!=&symbol);order.push_back(symbol.clone());
                    let bar=Bar {market_open_ms:timestamp.timestamp_millis(),received_at_ms:now,
                        decision_at_ms:chrono::Utc::now().timestamp_millis().max(now),
                        market_day:market_data::market_day(timestamp).to_string(),open,high,low,close,volume,is_final:true,interval_secs:60};
                    let updates=monitors.get_mut(&symbol).unwrap().on_bar(bar);
                    let elapsed=started.elapsed();
                    hub.publish(updates);
                    if elapsed>std::time::Duration::from_millis(25) {
                        consecutive_budget_breaches+=1;
                        hub.state.lock().unwrap_or_else(|p| p.into_inner()).status.budget_breaches+=1;
                    } else {consecutive_budget_breaches=0;}
                    if consecutive_budget_breaches>=3 {
                        // Shed this entire new subscriber; frozen consumers continue unchanged.
                        for m in monitors.values_mut() {hub.publish(m.on_gap(now,"processing_budget_exceeded"));}
                        let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                        s.status.healthy=false;s.status.active_symbols=0;s.status.reason="processing_budget_exceeded".into();
                        drop(s);hub.status();break;
                    }
                    {let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                        s.status.source_at_ms=Some(now);s.status.active_symbols=monitors.len();
                        if elapsed>std::time::Duration::from_millis(5) {s.status.processing_warnings+=1;}
                        if s.status.healthy {s.status.reason="observing".into();}}
                }
                Ok(_)=>{},
                Err(broadcast::error::RecvError::Lagged(n))=>{
                    let now=chrono::Utc::now().timestamp_millis();
                    for m in monitors.values_mut() {hub.publish(m.on_gap(now,"upstream_feed_gap"));}
                    monitors.clear();order.clear();
                    {let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                        s.status.lagged_events+=n;s.status.healthy=false;s.status.active_symbols=0;
                        s.status.reason="upstream_feed_gap".into();}
                    hub.status();
                }
                Err(broadcast::error::RecvError::Closed)=>{
                    let now=chrono::Utc::now().timestamp_millis();
                    for m in monitors.values_mut(){hub.publish(m.on_gap(now,"source_closed"));}
                    {let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());s.status.healthy=false;s.status.active_symbols=0;s.status.reason="source_closed".into();}
                    hub.status();break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_has_no_detection_or_frozen_output() {
        let hub = Hub::new(false, false);
        assert!(!hub.enabled());
        assert_eq!(hub.snapshot().len(), 1);
        let json = serde_json::to_value(&hub.snapshot()[0]).unwrap();
        assert_eq!(json["type"], "experimental_strategy_status");
        assert_eq!(json["reason"], "disabled");
    }
    #[test]
    fn flags_are_independent() {
        assert!(Hub::new(true, false).enabled());
        assert!(Hub::new(false, true).enabled());
    }

    #[tokio::test]
    async fn lag_is_explicit_and_does_not_publish_to_original_channel() {
        let (source, _) = broadcast::channel(1);
        let rx = source.subscribe();
        let hub = Hub::new(true, true);
        let mut output = hub.output.subscribe();
        for remaining in [1, 2] {
            source
                .send(ScanEvent::FunnelHealth {
                    timestamp: chrono::Utc::now(),
                    float_budget_remaining: remaining,
                    float_budget: 10,
                    starved_candidates: 0,
                    api_key_missing: false,
                })
                .unwrap();
        }
        let task = tokio::spawn(run(rx, hub.clone(), true, true));
        let status = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(Message::Status(s)) = output.recv().await {
                    if s.lagged_events > 0 {
                        break s;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(!status.healthy);
        assert_eq!(status.reason, "upstream_feed_gap");
        assert_eq!(status.active_symbols, 0);
        let mut original = source.subscribe();
        source
            .send(ScanEvent::FunnelHealth {
                timestamp: chrono::Utc::now(),
                float_budget_remaining: 3,
                float_budget: 10,
                starved_candidates: 0,
                api_key_missing: false,
            })
            .unwrap();
        assert!(matches!(
            original.recv().await.unwrap(),
            ScanEvent::FunnelHealth {
                float_budget_remaining: 3,
                ..
            }
        ));
        task.abort();
    }
}
