use anyhow::Result;
use clap::{Parser, Subcommand};
use vvv_remote::{Protocol, ServerConfig};

#[derive(Parser)]
#[command(subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, value_enum, default_value = "both")]
    protocol: Protocol,
    /// Enable AVC420 for an RDP-only listener (both protocols already default to AVC420).
    #[arg(long, alias = "h264")]
    nvenc: bool,
    #[command(flatten)]
    server: Option<ServerConfig>,
}

#[derive(Subcommand)]
enum Command {
    /// Create an Apple Screen Sharing SRP verifier.
    SetPassword(vvv_apple::set_password::PasswordArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::SetPassword(args)) = cli.command {
        return vvv_apple::set_password::set_password(args);
    }
    let config = cli
        .server
        .ok_or_else(|| anyhow::anyhow!("missing server options; supply --socket PATH"))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let encoder = if cli.protocol == Protocol::Rdp && !cli.nvenc {
        vvv_rdp::Encoder::ClearCodec
    } else {
        vvv_rdp::Encoder::Nvenc(config.avg_bitrate)
    };
    vvv_remote::serve_with_rdp_encoder(config, cli.protocol, encoder, async {
        tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = term.recv() => () }
    })
    .await
}
