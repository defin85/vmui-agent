use anyhow::Result;
use tracing_subscriber::EnvFilter;
use vmui_mcp_proxy::run_stdio_server;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    run_stdio_server().await
}
