//! One read-only census to validate the audit path; never places orders.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    dotenvy::dotenv().ok();
    let cfg = market_data::AlpacaConfig::from_env()?;
    let result = market_data::universe::capture_discovery_snapshots(&cfg).await;
    // Drain even on a failed scan so its incomplete evidence survives exit.
    let flushed = market_data::discovery_audit::flush();
    let count = result?;
    flushed?;
    tracing::info!(
        snapshots = count,
        "read-only snapshot audit complete; no scanner coverage measured"
    );
    Ok(())
}
