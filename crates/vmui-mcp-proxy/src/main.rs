use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    info!("starting vmui-mcp-proxy scaffold; transport bridge is not implemented yet");

    tokio::signal::ctrl_c().await?;
    info!("shutdown requested");

    Ok(())
}
