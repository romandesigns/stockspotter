//! Live discovery policy. Replay's historical seed calculation stays unchanged.
//! Missing provider bars are unknown, never invented zero-volume sessions.
use std::collections::HashMap;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use crate::rest::DailySeed;

pub const BATCH_SYMBOLS: usize = 100;
pub const MAX_PAGES: usize = 4;
const ATTEMPTS_PER_DAY: u32 = 12;
// Includes initial fetches and retries. Each batch has at most four HTTP pages.
const FIRST_BATCHES_PER_HOUR: usize = 240;
const RETRY_BATCHES_PER_HOUR: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub struct BaselineRecord {
    pub market_day: NaiveDate,
    pub expected_session: Option<NaiveDate>,
    pub last_bar_date: Option<NaiveDate>,
    pub window_start: Option<NaiveDate>,
    pub bars_used: usize,
    pub fetched_at: DateTime<Utc>,
    pub feed: String,
    pub adjustment: &'static str,
    pub input_hash: String,
    pub status: String,
    pub reason: Option<String>,
    pub seed: Option<DailySeed>,
    pub attempts: u32,
    #[serde(skip)]
    pub next_attempt: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct DailyBaselineCache {
    day: Option<NaiveDate>,
    pub records: HashMap<String, BaselineRecord>,
    first_batches: Vec<DateTime<Utc>>,
    retry_batches: Vec<DateTime<Utc>>,
}

/// Independent regular-exchange schedule; exceptional closures must fail closed
/// when observed snapshot dates disagree. Never infer a missing bar is zero.
pub fn expected_session(today: NaiveDate) -> Option<NaiveDate> {
    (1..=7).filter_map(|n| today.checked_sub_signed(Duration::days(n)))
        .find(|d| halt_detector::calendar::regular_close_minutes(*d).is_some())
}

impl DailyBaselineCache {
    pub fn roll(&mut self, day: NaiveDate) {
        if self.day != Some(day) {
            self.day = Some(day);
            self.records.clear();
            self.first_batches.clear();
            self.retry_batches.clear();
        }
    }

    pub fn due(&mut self, mut symbols: Vec<String>, now: DateTime<Utc>, expected: Option<NaiveDate>) -> Vec<String> {
        self.first_batches.retain(|t| *t > now - Duration::hours(1));
        self.retry_batches.retain(|t| *t > now - Duration::hours(1));
        symbols.sort();
        let mut out: Vec<_> = if self.first_batches.len() < FIRST_BATCHES_PER_HOUR {
            symbols.iter().filter(|s| !self.records.contains_key(*s)).take(BATCH_SYMBOLS).cloned().collect()
        } else { vec![] };
        let first_count=out.len();
        if self.retry_batches.len() < RETRY_BATCHES_PER_HOUR {
            let mut retries: Vec<_> = symbols.into_iter().filter(|s| self.records.get(s).is_some_and(|r|
                (r.status != "complete" || r.expected_session != expected) && r.attempts < ATTEMPTS_PER_DAY && now >= r.next_attempt)).collect();
            retries.sort_by_key(|s| (self.records[s].next_attempt,s.clone()));
            out.extend(retries.into_iter().take(BATCH_SYMBOLS-first_count));
        }
        if first_count>0 {self.first_batches.push(now);}
        if out.len()>first_count {self.retry_batches.push(now);}
        out
    }

    pub fn record(&mut self, symbol: String, mut record: BaselineRecord) {
        record.attempts = self.records.get(&symbol).map_or(1, |r| r.attempts + 1);
        self.store_record(symbol, record);
    }

    /// Finalize an already reserved attempt without spending it twice.
    pub fn finish(&mut self, symbol: String, mut record: BaselineRecord) {
        record.attempts = self.records.get(&symbol).map_or(1, |r| r.attempts);
        self.store_record(symbol, record);
    }

    fn store_record(&mut self, symbol: String, mut record: BaselineRecord) {
        let minutes = match record.attempts { 1 => 5, 2 => 10, 3 => 20, 4 => 40, _ => 60 };
        record.next_attempt = record.fetched_at + if record.status == "fetch_failed" && record.attempts == 1 {Duration::seconds(30)} else {Duration::minutes(minutes)};
        self.records.insert(symbol, record);
    }

    pub fn complete(&self, expected: Option<NaiveDate>) -> HashMap<String, DailySeed> {
        self.records.iter().filter_map(|(s,r)|
            (r.status == "complete" && expected.is_some() && r.expected_session == expected)
                .then(|| r.seed.map(|seed| (s.clone(), seed))).flatten()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(s: &str) -> DateTime<Utc> { s.parse().unwrap() }
    fn record(now: DateTime<Utc>, status: &str) -> BaselineRecord {
        BaselineRecord { market_day:now.date_naive(), expected_session:None,
            last_bar_date:None,window_start:None,bars_used:0,fetched_at:now,feed:"sip".into(),
            adjustment:"raw",input_hash:String::new(),status:status.into(),reason:None,seed:None,
            attempts:0,next_attempt:now }
    }
    #[test]
    fn holidays_weekends_and_dst_use_completed_sessions() {
        assert_eq!(expected_session(NaiveDate::from_ymd_opt(2026,9,8).unwrap()),NaiveDate::from_ymd_opt(2026,9,4));
        assert_eq!(expected_session(NaiveDate::from_ymd_opt(2026,10,12).unwrap()),NaiveDate::from_ymd_opt(2026,10,9));
        assert_eq!(expected_session(NaiveDate::from_ymd_opt(2026,11,2).unwrap()),NaiveDate::from_ymd_opt(2026,10,30));
    }
    #[test]
    fn retries_are_bounded_and_success_is_frozen_until_rollover() {
        let now=at("2026-10-09T04:00:00Z"); let mut c=DailyBaselineCache::default(); c.roll(now.date_naive());
        let names=vec!["X".into()];
        assert_eq!(c.due(names.clone(),now,None),names);
        c.record("X".into(),record(now,"stale_latest"));
        assert!(c.due(names.clone(),now+Duration::minutes(4),None).is_empty());
        assert_eq!(c.due(names.clone(),now+Duration::minutes(5),None),names);
        for n in 2..=12 { c.record("X".into(),record(now+Duration::hours(n as i64),"no_bars")); }
        assert!(c.due(names.clone(),now+Duration::hours(20),None).is_empty());
        c.roll(now.date_naive().succ_opt().unwrap());
        assert_eq!(c.due(names.clone(),now+Duration::days(1),None),names);
        c.record("X".into(),record(now+Duration::days(1),"complete"));
        assert!(c.due(names,now+Duration::days(1)+Duration::hours(2),None).is_empty());
    }
    #[test]
    fn hourly_cap_includes_first_fetches_and_all_pages() {
        let now=at("2026-10-09T04:00:00Z"); let mut c=DailyBaselineCache::default();
        for _ in 0..240 { assert_eq!(c.due((0..500).map(|i|format!("S{i}")).collect(),now,None).len(),100); }
        assert!(c.due(vec!["NEW".into()],now,None).is_empty());
        assert_eq!(c.due(vec!["NEW".into()],now+Duration::hours(1),None).len(),1);
    }
    #[test]
    fn new_symbols_do_not_starve_due_retries_and_changed_expectation_recovers() {
        let now=at("2026-10-09T04:00:00Z"); let mut c=DailyBaselineCache::default();
        c.record("OLD".into(),record(now-Duration::hours(1),"stale_latest"));
        let due=c.due(vec!["NEW".into(),"OLD".into()],now,None);
        assert_eq!(due,vec!["NEW","OLD"]);
        let mut c=DailyBaselineCache::default();
        for i in 0..300 {
            let time=now+Duration::seconds(15*i);
            let name=format!("G{i}");
            assert!(c.due(vec![name.clone()],time,None).contains(&name));
            c.record(name,record(time,"complete"));
        }
        c.record("CHANGED".into(),record(now,"complete"));
        assert!(c.due(vec!["CHANGED".into()],now+Duration::hours(3),Some(now.date_naive())).contains(&"CHANGED".into()));
    }

    #[test]
    fn interrupted_fetches_still_spend_the_per_symbol_attempt_cap() {
        let now=at("2026-10-09T04:00:00Z");let mut c=DailyBaselineCache::default();
        for i in 0..12 {
            let time=now+Duration::hours(i);
            assert_eq!(c.due(vec!["X".into()],time,None),vec!["X"]);
            // The HTTP future is aborted after reservation; no finish occurs.
            c.record("X".into(),record(time,"fetch_pending"));
        }
        assert_eq!(c.records["X"].attempts,12);
        assert!(c.due(vec!["X".into()],now+Duration::hours(20),None).is_empty());
        c.finish("X".into(),record(now+Duration::hours(20),"fetch_failed"));
        assert_eq!(c.records["X"].attempts,12);
    }

}
