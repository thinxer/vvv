use super::server::{ServerConfig, serve};
use anyhow::Result;
use clap::Parser;

#[tokio::main]
pub async fn run() -> Result<()> {
    let args = ServerConfig::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let shutdown = async {
        tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = term.recv() => () }
    };
    serve(args, shutdown).await
}
