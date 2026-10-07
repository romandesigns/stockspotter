#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args == ["--check"] || args == ["--run"],
        "use --check (read-only connection check) or --run (Alpaca paper orders)"
    );
    auto_trader::paper_runtime::run(args == ["--check"]).await
}
