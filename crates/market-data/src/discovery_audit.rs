//! Optional prospective evidence. Never block market dispatch on disk I/O.
//! Each process writes a separate, daily JSONL file; gaps invalidate recall claims.
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, OnceLock,
    },
};

struct Recorder {
    tx: SyncSender<Message>,
    lost: Arc<AtomicU64>,
}
enum Message {
    Record(Value),
    Flush(std::sync::mpsc::Sender<Result<(), String>>),
}
static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();

fn recorder() -> Option<&'static Recorder> {
    RECORDER
        .get_or_init(|| {
            let dir = std::env::var_os("DISCOVERY_AUDIT_DIR").filter(|v| !v.is_empty())?;
            let dir = std::path::PathBuf::from(dir);
            if let Err(error) = std::fs::create_dir_all(&dir) {
                tracing::error!(%error, "discovery audit unavailable: cannot create directory");
                return None;
            }
            let run = format!("{}-{}", std::process::id(), Utc::now().timestamp_micros());
            let (tx, rx) = sync_channel::<Message>(32);
            let lost = Arc::new(AtomicU64::new(0));
            let writer_lost = lost.clone();
            std::thread::spawn(move || {
                let mut day = String::new();
                let mut file: Option<std::fs::File> = None;
                let mut bytes = 0usize;
                for message in rx {
                    let record = match message {
                        Message::Record(record) => record,
                        Message::Flush(reply) => {
                            let result = file
                                .as_ref()
                                .map_or(Ok(()), |f| f.sync_all())
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                            continue;
                        }
                    };
                    let result = (|| -> anyhow::Result<()> {
                        let today =
                            record["recorded_at"].as_str().unwrap_or_default()[..10].to_string();
                        if today != day {
                            file = Some(
                                std::fs::OpenOptions::new()
                                    .create_new(true)
                                    .write(true)
                                    .open(dir.join(format!("{today}-{run}.jsonl")))?,
                            );
                            day = today;
                            bytes = 0;
                        }
                        let mut encoded = serde_json::to_vec(&record)?;
                        encoded.push(b'\n');
                        anyhow::ensure!(
                            bytes + encoded.len() <= 8usize * 1024 * 1024 * 1024,
                            "8 GiB daily audit cap reached; remaining records will be lost"
                        );
                        file.as_mut().unwrap().write_all(&encoded)?;
                        bytes += encoded.len();
                        Ok(())
                    })();
                    if let Err(error) = result {
                        let n = writer_lost.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_power_of_two() {
                            tracing::error!(%error, lost_records=n, "discovery audit has gaps");
                        }
                    }
                }
            });
            Some(Recorder { tx, lost })
        })
        .as_ref()
}

pub fn enabled() -> bool {
    recorder().is_some()
}

pub fn emit(kind: &str, data: Value) {
    if let Some(r) = recorder() {
        let record = json!({"schema":1,"recorded_at":Utc::now(),"kind":kind,
            "lost_records":r.lost.load(Ordering::Relaxed),"data":data});
        if r.tx.try_send(Message::Record(record)).is_err() {
            let n = r.lost.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                tracing::error!(
                    lost_records = n,
                    "discovery audit queue full or writer stopped"
                );
            }
        }
    }
}

/// Blocking durability barrier for one-shot tools, never called from tick dispatch.
pub fn flush() -> anyhow::Result<()> {
    let r =
        recorder().ok_or_else(|| anyhow::anyhow!("DISCOVERY_AUDIT_DIR is unset or unavailable"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    r.tx.send(Message::Flush(tx))
        .map_err(|_| anyhow::anyhow!("audit writer stopped"))?;
    rx.recv_timeout(std::time::Duration::from_secs(30))?
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        r.lost.load(Ordering::Relaxed) == 0,
        "audit records were lost"
    );
    Ok(())
}

/// One first observed print per symbol per heartbeat interval, not every tick.
/// Presence proves receipt only at that instant; absence never proves no subscription.
#[derive(Default)]
pub struct Receipts(pub HashMap<String, Value>);
impl Receipts {
    pub fn trade(&mut self, trade: &crate::Trade, monitored: bool) {
        if enabled() && !self.0.contains_key(&trade.symbol) {
            self.0.insert(
                trade.symbol.clone(),
                json!({"symbol":trade.symbol,
                "market_at":trade.timestamp,"received_at":Utc::now(),
                "price":trade.price,"ignition_monitored":monitored}),
            );
        }
    }
    pub fn take(&mut self) -> HashMap<String, Value> {
        std::mem::take(&mut self.0)
    }
}
