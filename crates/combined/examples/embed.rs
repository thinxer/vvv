//! Run with an unused receiver socket and an existing Apple SRP verifier.
use std::path::PathBuf;
use vvv_remote::{ServerConfig, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let socket = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("missing frames socket path"))?,
    );
    let verifier = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("missing SRP verifier path"))?,
    );
    let config = ServerConfig {
        socket,
        cuda_socket: None,
        avg_bitrate: cuda_scanout::DEFAULT_AVG_BITRATE,
        srp_file: Some(verifier),
        dev_login: false,
        listen: "127.0.0.1:5990".parse()?,
        media_port: 5992,
        #[cfg(feature = "combined")]
        rdp_port: 3389,
    };
    serve(config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
