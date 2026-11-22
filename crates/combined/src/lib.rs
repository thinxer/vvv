//! Remote desktop service lifecycle and listener selection.
mod apple;
mod cursor;
mod desktop;
mod input;
#[cfg(feature = "combined")]
mod rdp;
mod resize;
mod virtio_input;
use anyhow::Result;
use apple_screen_sharing::{rfb, server::ExternalScreen};
use clap::Parser;
use desktop::Screen;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
#[derive(Parser)]
pub struct ServerConfig {
    /// New Unix socket for a separate crosvm --wayland-sock.
    #[arg(long)]
    pub socket: PathBuf,
    #[arg(long)]
    pub cuda_socket: Option<PathBuf>,
    /// Average NVENC video bitrate in bits per second per stream.
    #[arg(long, default_value_t = cuda_scanout::DEFAULT_AVG_BITRATE)]
    pub avg_bitrate: std::num::NonZeroU32,
    /// SRP verifier created with vvv-combined set-password.
    #[arg(long, conflicts_with = "dev_login")]
    pub srp_file: Option<PathBuf>,
    /// Development receiver: accept any viewer credentials using Apple DH login.
    #[arg(long)]
    pub dev_login: bool,
    #[arg(long, default_value = "127.0.0.1:5990")]
    pub listen: SocketAddr,
    #[arg(long, default_value_t = 5992)]
    pub media_port: u16,
    #[cfg(feature = "combined")]
    #[arg(long, default_value_t = 3389)]
    pub rdp_port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Protocol {
    Apple,
    #[cfg(feature = "combined")]
    Rdp,
    #[cfg(feature = "combined")]
    Both,
}

/// Serve the protocols enabled in this SDK build.
pub async fn serve(
    args: ServerConfig,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    #[cfg(feature = "combined")]
    let protocol = Protocol::Both;
    #[cfg(not(feature = "combined"))]
    let protocol = Protocol::Apple;
    serve_protocol(args, protocol, shutdown).await
}

/// Serve only the selected listeners on the shared guest.
pub async fn serve_protocol(
    args: ServerConfig,
    protocol: Protocol,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    #[cfg(feature = "combined")]
    let encoder = if protocol == Protocol::Rdp {
        vvv_rdp::Encoder::ClearCodec
    } else {
        vvv_rdp::Encoder::Nvenc(args.avg_bitrate)
    };
    serve_inner(
        args,
        protocol,
        #[cfg(feature = "combined")]
        encoder,
        shutdown,
    )
    .await
}

#[cfg(feature = "combined")]
pub async fn serve_with_rdp_encoder(
    args: ServerConfig,
    protocol: Protocol,
    encoder: vvv_rdp::Encoder,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    serve_inner(args, protocol, encoder, shutdown).await
}

async fn serve_inner(
    args: ServerConfig,
    protocol: Protocol,
    #[cfg(feature = "combined")] rdp_encoder: vvv_rdp::Encoder,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    #[cfg(feature = "combined")]
    let apple = protocol != Protocol::Rdp;
    #[cfg(not(feature = "combined"))]
    let apple = protocol == Protocol::Apple;
    let auth = apple.then(|| apple::auth(&args)).transpose()?;
    let screen = Arc::new(Screen::default());
    let tablet = Arc::new(virtio_input::VirtioInput::bind(
        &args.socket.with_file_name("mouse.sock"),
    )?);
    let keyboard = Arc::new(virtio_input::VirtioInput::bind(
        &args.socket.with_file_name("keyboard.sock"),
    )?);
    let (_capture, mut capture_end) = desktop::Capture::start(
        args.socket.clone(),
        args.cuda_socket.clone(),
        screen.clone(),
    )?;
    tracing::info!(socket = %args.socket.display(), "waiting for guest scanout");
    tokio::pin!(shutdown);
    while screen.size() == (0, 0) {
        tokio::select! { _ = &mut shutdown => return Ok(()),
            e = &mut capture_end => anyhow::bail!("capture ended: {e:?}"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
    }
    let (w, h) = screen.size();
    let display = Arc::new(resize::GuestDisplay::new(
        screen.clone(),
        args.socket.with_file_name("crosvm.sock"),
    ));
    #[cfg(feature = "combined")]
    let seat = Arc::new(vvv_session::ViewerSeat::default());
    #[cfg(feature = "combined")]
    desktop::start_view_updates(&screen, &seat)?;
    #[cfg(feature = "combined")]
    let _rdp = if protocol != Protocol::Apple {
        Some(rdp::start(
            &args,
            &screen,
            display.clone(),
            keyboard.clone(),
            tablet.clone(),
            seat.clone(),
            rdp_encoder,
        )?)
    } else {
        None
    };
    let Some(auth) = auth else {
        return tokio::select! { _ = shutdown => Ok(()), e = &mut capture_end => anyhow::bail!("capture ended: {e:?}") };
    };
    let cfg = apple::configure(
        &args,
        auth,
        screen,
        display,
        keyboard,
        tablet,
        #[cfg(feature = "combined")]
        seat,
    )?;
    tracing::info!(listen = %args.listen, w, h, media_port = args.media_port, "Apple NVENC receiver ready");
    tokio::select! { result = rfb::serve(args.listen, Arc::new(cfg), shutdown) => result,
    e = &mut capture_end => anyhow::bail!("capture ended: {e:?}"), }
}
