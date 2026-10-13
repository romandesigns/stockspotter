//! Separate read-only subscriber and wire channel. Frozen ScanEvent, OI,
//! observer, push and auto-trader never consume experimental strategy outputs.
use market_data::ScanEvent;
use momentum_strategies::{Bar, Config, Event, Monitor};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

// Capacity amendment: docs/IAC-EXTREME-COVERAGE-2026-10-13.md.
// Stable admission avoids resetting every history when the working set exceeds 256.
const MAX_SYMBOLS: usize = 16_384;
const MAX_EVENTS: usize = 100;
const SOURCE_IDLE_MS: i64 = 120_000;

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
    pub symbol_limit: usize,
    pub iac_ready_symbols: usize,
    pub extreme_ready_symbols: usize,
    pub capacity_dropped_updates: u64,
    pub recent_capacity_dropped_symbols: Vec<String>,
    pub idle_reclaims: u64,
    pub idle_resets: u64,
    pub accepted_bar_updates: u64,
    pub warmup_bar_updates: u64,
    pub duplicate_bar_updates: u64,
    pub revision_resets: u64,
    pub missing_minute_resets: u64,
    pub invalid_bar_updates: u64,
    pub output_lagged_events: u64,
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
                    symbol_limit: MAX_SYMBOLS,
                    iac_ready_symbols: 0,
                    extreme_ready_symbols: 0,
                    capacity_dropped_updates: 0,
                    recent_capacity_dropped_symbols: Vec::new(),
                    idle_reclaims: 0,
                    idle_resets: 0,
                    accepted_bar_updates: 0,
                    warmup_bar_updates: 0,
                    duplicate_bar_updates: 0,
                    revision_resets: 0,
                    missing_minute_resets: 0,
                    invalid_bar_updates: 0,
                    output_lagged_events: 0,
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
    pub fn record_output_lag(&self, missed: u64) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.status.output_lagged_events = s.status.output_lagged_events.saturating_add(missed);
    }
}

struct Entry {
    monitor: Monitor,
    contiguous: usize,
    day: Option<String>,
    last_receipt_ms: i64,
    idle_reset: bool,
}

/// Same admission and bar-processing path in production and deterministic load tests.
/// No fabricated historical receipts enter the live runner.
struct Collector {
    entries: HashMap<String, Entry>,
    run_id: String,
    generation: u64,
    limit: usize,
    enabled: [bool; 2],
    ready_at: [usize; 2],
    reclaim_after_ms: i64,
}

impl Collector {
    fn new(run_id: String, limit: usize, iac: bool, extreme: bool) -> Self {
        let config = Config::default();
        Self {
            entries: HashMap::new(), run_id, generation: 0, limit,
            enabled: [iac, extreme],
            ready_at: [config.baseline_bars + config.compression_bars,
                config.baseline_bars + config.extreme_bars - 1],
            // Retain cooldown state beyond the source-idle reset. Receipt time is
            // no earlier than candidate market time, so this is conservative.
            reclaim_after_ms: config.cooldown_ms.max(SOURCE_IDLE_MS),
        }
    }

    fn ready(&self, entry: &Entry) -> [bool; 2] {
        [self.enabled[0] && entry.contiguous >= self.ready_at[0],
            self.enabled[1] && entry.contiguous >= self.ready_at[1]]
    }

    fn on_bar(&mut self, symbol: String, bar: Bar, hub: &Hub) {
        let now = bar.received_at_ms;
        if !self.entries.contains_key(&symbol) {
            // Validate before capacity accounting. Bad symbols cannot displace
            // a valid monitor or make capacity telemetry look like real demand.
            let Ok(monitor) = create_monitor(symbol.clone(), &self.run_id,
                &mut self.generation, self.enabled[0], self.enabled[1]) else {return;};
            if self.entries.len() >= self.limit {
                let mut s = hub.state.lock().unwrap_or_else(|p| p.into_inner());
                s.status.capacity_dropped_updates += 1;
                if !s.status.recent_capacity_dropped_symbols.contains(&symbol) {
                    s.status.recent_capacity_dropped_symbols.push(symbol);
                    if s.status.recent_capacity_dropped_symbols.len() > 100 {
                        s.status.recent_capacity_dropped_symbols.remove(0);
                    }
                }
                return;
            }
            self.entries.insert(symbol.clone(), Entry { monitor, contiguous: 0,
                day: None, last_receipt_ms: now, idle_reset: false });
        }
        let before_ready = self.ready(&self.entries[&symbol]);
        let entry = self.entries.get_mut(&symbol).unwrap();
        let before = entry.monitor.counters.clone();
        let day = bar.market_day.clone();
        let updates = entry.monitor.on_bar(bar);
        let after = &entry.monitor.counters;
        if after.gaps > before.gaps || after.revisions > before.revisions
            || after.invalid_inputs > before.invalid_inputs
            || (after.accepted_bars > before.accepted_bars && entry.day.as_ref().is_some_and(|d| d != &day)) {
            entry.contiguous = 0;
        }
        if after.accepted_bars > before.accepted_bars {
            entry.contiguous = (entry.contiguous + 1).min(64);
            entry.day = Some(day);
            entry.idle_reset = false;
        }
        entry.last_receipt_ms = now;
        // Absorb deltas immediately: removing an idle entry never erases totals.
        {
            let mut s = hub.state.lock().unwrap_or_else(|p| p.into_inner());
            s.status.accepted_bar_updates += after.accepted_bars - before.accepted_bars;
            s.status.warmup_bar_updates += after.warmup_bars - before.warmup_bars;
            s.status.duplicate_bar_updates += after.duplicates - before.duplicates;
            s.status.revision_resets += after.revisions - before.revisions;
            s.status.missing_minute_resets += after.gaps - before.gaps;
            s.status.invalid_bar_updates += after.invalid_inputs - before.invalid_inputs;
            s.status.source_at_ms = Some(now);
        }
        let after_ready = self.ready(&self.entries[&symbol]);
        let mut s = hub.state.lock().unwrap_or_else(|p| p.into_inner());
        s.status.iac_ready_symbols = s.status.iac_ready_symbols + usize::from(after_ready[0]) - usize::from(before_ready[0]);
        s.status.extreme_ready_symbols = s.status.extreme_ready_symbols + usize::from(after_ready[1]) - usize::from(before_ready[1]);
        s.status.active_symbols = self.entries.len();
        if s.status.healthy {
            s.status.reason = if s.status.iac_ready_symbols + s.status.extreme_ready_symbols == 0 {
                "warming_up"
            } else { "observing" }.into();
        }
        drop(s);
        hub.publish(updates);
    }

    fn sweep(&mut self, now: i64, hub: &Hub) {
        let mut idle_resets = 0;
        let mut ready = [0, 0];
        self.entries.retain(|_, entry| {
            if !entry.idle_reset && entry.monitor.last_available_ms().is_some_and(|t| now - t > SOURCE_IDLE_MS) {
                hub.publish(entry.monitor.on_gap(now, "source_idle"));
                entry.contiguous = 0;
                entry.idle_reset = true;
                idle_resets += 1;
            }
            now - entry.last_receipt_ms <= self.reclaim_after_ms
        });
        for entry in self.entries.values() {
            let r = self.ready(entry);
            ready[0] += usize::from(r[0]); ready[1] += usize::from(r[1]);
        }
        let mut s = hub.state.lock().unwrap_or_else(|p| p.into_inner());
        s.status.idle_resets += idle_resets;
        s.status.idle_reclaims += (s.status.active_symbols - self.entries.len()) as u64;
        s.status.active_symbols = self.entries.len();
        s.status.iac_ready_symbols = ready[0]; s.status.extreme_ready_symbols = ready[1];
        if s.status.healthy && ready == [0, 0] {s.status.reason = "warming_up".into();}
    }

    fn clear(&mut self, now: i64, reason: &str, hub: &Hub) {
        for entry in self.entries.values_mut() {hub.publish(entry.monitor.on_gap(now, reason));}
        self.entries.clear();
        let mut s = hub.state.lock().unwrap_or_else(|p| p.into_inner());
        s.status.active_symbols = 0; s.status.iac_ready_symbols = 0; s.status.extreme_ready_symbols = 0;
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
    let mut collector = Collector::new(run_id, MAX_SYMBOLS, iac, extreme);
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut consecutive_budget_breaches = 0u32;
    loop {
        tokio::select! {
            _=timer.tick()=>{
                let now=chrono::Utc::now().timestamp_millis();
                collector.sweep(now, &hub);
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
                    let bar=Bar {market_open_ms:timestamp.timestamp_millis(),received_at_ms:now,
                        decision_at_ms:chrono::Utc::now().timestamp_millis().max(now),
                        market_day:market_data::market_day(timestamp).to_string(),open,high,low,close,volume,is_final:true,interval_secs:60};
                    collector.on_bar(symbol, bar, &hub);
                    let elapsed=started.elapsed();
                    if elapsed>std::time::Duration::from_millis(25) {
                        consecutive_budget_breaches+=1;
                        hub.state.lock().unwrap_or_else(|p| p.into_inner()).status.budget_breaches+=1;
                    } else {consecutive_budget_breaches=0;}
                    if consecutive_budget_breaches>=3 {
                        // Shed this entire new subscriber; frozen consumers continue unchanged.
                        collector.clear(now, "processing_budget_exceeded", &hub);
                        let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                        s.status.healthy=false;s.status.active_symbols=0;s.status.reason="processing_budget_exceeded".into();
                        drop(s);hub.status();break;
                    }
                    {let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                        if elapsed>std::time::Duration::from_millis(5) {s.status.processing_warnings+=1;}
                    }
                }
                Ok(_)=>{},
                Err(broadcast::error::RecvError::Lagged(n))=>{
                    let now=chrono::Utc::now().timestamp_millis();
                    collector.clear(now, "upstream_feed_gap", &hub);
                    {let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());
                        s.status.lagged_events+=n;s.status.healthy=false;s.status.active_symbols=0;
                        s.status.reason="upstream_feed_gap".into();}
                    hub.status();
                }
                Err(broadcast::error::RecvError::Closed)=>{
                    let now=chrono::Utc::now().timestamp_millis();
                    collector.clear(now, "source_closed", &hub);
                    {let mut s=hub.state.lock().unwrap_or_else(|p| p.into_inner());s.status.healthy=false;s.status.active_symbols=0;s.status.reason="source_closed".into();}
                    hub.status();break;
                }
            }
        }
    }
}

fn create_monitor(symbol:String,run_id:&str,generation:&mut u64,iac:bool,extreme:bool)->Result<Monitor,&'static str> {
    *generation=generation.checked_add(1).ok_or("monitor generation exhausted")?;
    // Recreating an evicted symbol must not reuse its former event IDs.
    Monitor::new(symbol,format!("{run_id}:{generation}"),Config::default(),iac,extreme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use momentum_strategies::Kind;

    fn fixture_bar(i: i64, extreme: bool) -> Bar {
        let at = (30_000 + i) * 60_000;
        let (open, high, low, close, volume) = if extreme {
            if i < 20 { (10.0, 10.02, 9.98, 10.0, 1000) }
            else { let o = 10.0 + (i - 20) as f64 * 0.2; (o, o + 0.2, o - 0.01, o + 0.18, 4000) }
        } else if i < 20 { (10.0, 10.2, 9.8, 10.0, 1000) }
        else if i < 25 { (10.0, 10.05, 9.95, 10.0, 400) }
        else { (10.0, 10.5, 9.99, 10.45, 2000) };
        Bar {market_open_ms: at, received_at_ms: at + 60_000, decision_at_ms: at + 60_000,
            market_day: "2026-08-26".into(), open, high, low, close, volume, is_final: true, interval_secs: 60}
    }

    #[test]
    fn mixed_symbol_cycles_preserve_end_of_cycle_detection_for_both_strategies() {
        let hub = Hub::new(true, true);
        let mut c = Collector::new("load".into(), MAX_SYMBOLS, true, true);
        let mut candidates = std::collections::HashSet::new();
        let mut rx = hub.output.subscribe();
        for minute in 0..26 {
            for symbol in 0..512 {
                c.on_bar(format!("S{symbol}"), fixture_bar(minute, symbol % 2 == 1), &hub);
                while let Ok(Message::Update(e)) = rx.try_recv() {
                    if e.kind == Kind::Candidate {candidates.insert((e.symbol, e.strategy == momentum_strategies::Strategy::Iac));}
                }
            }
        }
        assert_eq!(candidates.len(), 512);
        assert!(candidates.contains(&("S510".into(), true)));
        assert!(candidates.contains(&("S511".into(), false)));
        let s = hub.state.lock().unwrap();
        assert_eq!(s.status.evictions, 0);
        assert_eq!(s.status.capacity_dropped_updates, 0);
        assert_eq!(s.status.iac_ready_symbols, 512);
        assert_eq!(s.status.extreme_ready_symbols, 512);
    }

    #[test]
    fn old_256_lru_negative_control_never_warms_a_512_symbol_cycle() {
        let mut entries = HashMap::new(); let mut order = VecDeque::new();
        let mut generation = 0; let mut candidates = 0;
        for minute in 0..26 {
            for symbol in 0..512 {
                let symbol = format!("S{symbol}");
                if !entries.contains_key(&symbol) {
                    if entries.len() == 256 {entries.remove(&order.pop_front().unwrap());}
                    entries.insert(symbol.clone(), create_monitor(symbol.clone(), "old", &mut generation, true, true).unwrap());
                }
                order.retain(|s| s != &symbol); order.push_back(symbol.clone());
                candidates += entries.get_mut(&symbol).unwrap().on_bar(fixture_bar(minute, false))
                    .iter().filter(|e| e.kind == Kind::Candidate).count();
            }
        }
        assert_eq!(candidates, 0);
        assert!(entries.values().all(|m| m.counters.accepted_bars == 1));
    }

    #[test]
    fn overflow_preserves_incumbents_and_rejections_are_bounded_and_sticky() {
        let hub = Hub::new(true, true);
        let mut c = Collector::new("overflow".into(), 2, true, true);
        for minute in 0..26 {
            for symbol in ["FIRST", "LAST"] {c.on_bar(symbol.into(), fixture_bar(minute, false), &hub);}
            c.on_bar("NEW".into(), fixture_bar(minute, false), &hub);
        }
        assert_eq!(c.entries.len(), 2);
        assert_eq!(c.entries["LAST"].monitor.counters.candidates, 1);
        for symbol in 0..150 {c.on_bar(format!("NEW{symbol}"), fixture_bar(25, false), &hub);}
        let s = hub.state.lock().unwrap();
        assert_eq!(s.status.capacity_dropped_updates, 176);
        assert_eq!(s.status.recent_capacity_dropped_symbols.len(), 100);
        assert_eq!(s.status.evictions, 0);
        assert_eq!(s.status.iac_ready_symbols, 2);
    }

    #[test]
    fn readiness_resets_on_revision_invalid_gap_and_day_change_but_not_duplicates() {
        for reset in ["revision", "invalid", "gap", "day"] {
            let hub = Hub::new(true, true); let mut c = Collector::new("ready".into(), 2, true, true);
            for i in 0..25 {c.on_bar("TEST".into(), fixture_bar(i, false), &hub);}
            let mut duplicate = fixture_bar(24, false); duplicate.received_at_ms += 10; duplicate.decision_at_ms += 10;
            c.on_bar("TEST".into(), duplicate, &hub);
            assert_eq!(c.entries["TEST"].contiguous, 25);
            let mut bad = fixture_bar(if reset == "gap" {26} else {25}, false);
            if reset == "revision" {bad.market_open_ms -= 60_000; bad.volume += 1;}
            if reset == "invalid" {bad.close = f64::NAN;}
            if reset == "day" {bad.market_day = "2026-08-27".into();}
            c.on_bar("TEST".into(), bad, &hub);
            assert!(c.entries["TEST"].contiguous <= 1, "{reset}");
            let s = hub.state.lock().unwrap();
            assert_eq!(s.status.iac_ready_symbols, 0); assert_eq!(s.status.extreme_ready_symbols, 0);
            assert_eq!(s.status.duplicate_bar_updates, 1);
        }
    }

    #[test]
    fn idle_invalidates_once_and_reclaim_preserves_cooldown_and_process_totals() {
        let hub = Hub::new(true, true); let mut c = Collector::new("idle".into(), 2, true, true);
        for i in 0..26 {c.on_bar("TEST".into(), fixture_bar(i, false), &hub);}
        let receipt = fixture_bar(25, false).received_at_ms;
        c.sweep(receipt + SOURCE_IDLE_MS, &hub);
        assert_eq!(c.entries["TEST"].contiguous, 26);
        c.sweep(receipt + SOURCE_IDLE_MS + 1, &hub);
        assert_eq!(c.entries["TEST"].contiguous, 0);
        assert_eq!(c.entries.len(), 1);
        assert!(hub.snapshot().iter().any(|m| matches!(m, Message::Update(e) if e.kind == Kind::Invalidated && e.reason == "source_idle")));
        c.sweep(receipt + 300_000, &hub);
        assert_eq!(hub.state.lock().unwrap().status.idle_resets, 1);
        // Another valid anchor-shaped sequence inside the original cooldown
        // cannot create a second lifecycle, even though idle reset cleared history.
        for i in 26..31 {c.on_bar("TEST".into(), fixture_bar(i, false), &hub);}
        assert_eq!(c.entries["TEST"].monitor.counters.candidates, 1);
        let last = fixture_bar(30, false).received_at_ms;
        c.sweep(last + Config::default().cooldown_ms, &hub); assert_eq!(c.entries.len(), 1);
        c.sweep(last + Config::default().cooldown_ms + 1, &hub); assert!(c.entries.is_empty());
        let s = hub.state.lock().unwrap(); assert_eq!(s.status.accepted_bar_updates, 31);
        assert_eq!(s.status.idle_reclaims, 1); assert_eq!(s.status.evictions, 0);
    }

    #[test]
    fn collector_events_equal_direct_monitor_and_clear_invalidates_active_observations() {
        let hub = Hub::new(true, true); let mut c = Collector::new("same".into(), 2, true, true);
        let mut generation = 0;
        let mut direct = create_monitor("TEST".into(), "same", &mut generation, true, true).unwrap();
        let mut rx = hub.output.subscribe();
        for i in 0..29 {
            let b = fixture_bar(i, false); let expected = direct.on_bar(b.clone());
            c.on_bar("TEST".into(), b, &hub);
            let mut actual = Vec::new(); while let Ok(Message::Update(e)) = rx.try_recv() {actual.push(e);}
            assert_eq!(actual, expected);
        }
        c.clear(fixture_bar(29, false).received_at_ms, "upstream_feed_gap", &hub);
        assert!(hub.snapshot().iter().any(|m| matches!(m, Message::Update(e) if e.kind == Kind::Invalidated && e.reason == "upstream_feed_gap")));
        assert_eq!(hub.state.lock().unwrap().status.iac_ready_symbols, 0);
        assert!(c.entries.is_empty());
    }

    #[test]
    #[ignore = "release-mode full-capacity qualification; run explicitly before deployment"]
    fn full_capacity_load_qualification() {
        let hub = Hub::new(true, true); let mut c = Collector::new("full".into(), MAX_SYMBOLS, true, true);
        let started = std::time::Instant::now(); let mut timings = Vec::new();
        for minute in 0..64 {
            for symbol in 0..MAX_SYMBOLS {
                let at = std::time::Instant::now();
                c.on_bar(format!("S{symbol}"), fixture_bar(minute, symbol % 2 == 1), &hub);
                timings.push(at.elapsed().as_micros());
            }
        }
        c.on_bar("OVERFLOW".into(), fixture_bar(63, false), &hub);
        assert_eq!(c.entries.len(), MAX_SYMBOLS);
        assert_eq!(hub.state.lock().unwrap().status.iac_ready_symbols, MAX_SYMBOLS);
        assert_eq!(hub.state.lock().unwrap().status.capacity_dropped_updates, 1);
        timings.sort_unstable(); let p99 = timings[timings.len() * 99 / 100];
        assert!(p99 < 5_000, "p99 exceeded 5ms: {p99}us");
        let sweep = std::time::Instant::now(); c.sweep(fixture_bar(63, false).received_at_ms + 120_001, &hub);
        let sweep_ms = sweep.elapsed().as_millis();
        let rss_kib = std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines()
            .find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|n| n.parse::<u64>().ok()));
        if let Some(rss) = rss_kib {assert!(rss < 512 * 1024, "research harness RSS {rss}KiB exceeds512MiB");}
        assert!(sweep_ms < 250, "full-capacity idle sweep {sweep_ms}ms exceeded qualification budget");
        println!("capacity={MAX_SYMBOLS} bars={} p99_us={p99} max_us={} elapsed_ms={} sweep_ms={sweep_ms} rss_hwm_kib={rss_kib:?}", timings.len(), timings.last().unwrap(), started.elapsed().as_millis());
    }
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

    #[test]
    fn recreated_symbol_does_not_reuse_a_prior_candidate_identity() {
        let mut generation=0;
        let mut first=create_monitor("TEST".into(),"run",&mut generation,true,false).unwrap();
        let mut recreated=create_monitor("TEST".into(),"run",&mut generation,true,false).unwrap();
        let mut first_event=None;let mut recreated_event=None;
        for i in 0..26 {
            let at=(30_000+i)*60_000;
            let (high,low,close,volume)=if i<20 {(10.2,9.8,10.0,1000)} else if i<25 {(10.05,9.95,10.0,400)} else {(10.5,9.99,10.45,2000)};
            let bar=Bar {market_open_ms:at,received_at_ms:at+60_000,decision_at_ms:at+60_000,
                market_day:"2026-08-26".into(),open:10.0,high,low,close,volume,is_final:true,interval_secs:60};
            first_event=first.on_bar(bar.clone()).pop().or(first_event);
            recreated_event=recreated.on_bar(bar).pop().or(recreated_event);
        }
        let a=first_event.unwrap();let b=recreated_event.unwrap();
        assert_eq!(a.evidence,b.evidence);assert_eq!(a.anchor_price,b.anchor_price);
        assert_ne!(a.event_id,b.event_id);assert_ne!(a.lifecycle_id,b.lifecycle_id);
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
