use anyhow::Result;
use tracing_subscriber::EnvFilter;
use vmui_agent::run_daemon;
use vmui_core::AgentConfig;
use vmui_platform_windows::WindowsBackend;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    run_daemon(AgentConfig::default(), WindowsBackend::new()).await
}
