#[cfg(feature = "combined")]
mod view;
use crate::{Screen, ServerConfig, cursor, input, resize::GuestDisplay, virtio_input::VirtioInput};
use anyhow::{Result, ensure};
use apple_screen_sharing::{
    rfb::{AuthConfig, Config, srp::VerifierRecord},
    server::ExternalScreen,
};
use std::sync::Arc;
use vvv_nvenc::capture::DmabufBuffer;

pub(super) fn auth(args: &ServerConfig) -> Result<AuthConfig> {
    ensure!(
        args.dev_login != args.srp_file.is_some(),
        "choose an SRP verifier or explicit development login"
    );
    ensure!(
        args.media_port > 0 && args.media_port <= 65533,
        "invalid media port"
    );
    Ok(match &args.srp_file {
        Some(path) => AuthConfig {
            srp: Some(VerifierRecord::load(&path)?),
            dh: None,
        },
        None => {
            tracing::info!("development login enabled: accepting any viewer credentials");
            AuthConfig {
                srp: None,
                dh: Some(Arc::new(DevelopmentLogin)),
            }
        }
    })
}

pub(super) fn configure(
    args: &ServerConfig,
    auth: AuthConfig,
    screen: Arc<Screen>,
    display: Arc<GuestDisplay>,
    keyboard: Arc<VirtioInput>,
    tablet: Arc<VirtioInput>,
    #[cfg(feature = "combined")] seat: Arc<vvv_session::ViewerSeat>,
) -> Result<Config> {
    let (w, h) = screen.size();
    let mut cfg = Config::new("vvv", w, h, auth);
    #[cfg(feature = "combined")]
    let output: Arc<dyn ExternalScreen<DmabufBuffer>> =
        Arc::new(view::AppleView::new(screen.clone()));
    #[cfg(not(feature = "combined"))]
    let output: Arc<dyn ExternalScreen<DmabufBuffer>> = screen.clone();
    vvv_apple::configure_video(
        &mut cfg,
        output,
        args.listen,
        args.media_port,
        args.avg_bitrate,
    )?;
    let cursors = screen.clone();
    let cursor_display = display.clone();
    cfg.cursor_source = Arc::new(move || {
        Box::new(cursor::GuestCursor::new(
            cursors.clone(),
            cursor_display.clone(),
        ))
    });
    let input_screen = screen.clone();
    let input_keyboard = keyboard.clone();
    let input_tablet = tablet.clone();
    cfg.input = Some(Arc::new(move |w, h| {
        Ok(Box::new(input::GuestInput::new(
            input_screen.clone(),
            input_keyboard.clone(),
            input_tablet.clone(),
            w,
            h,
        )))
    }));
    #[cfg(feature = "combined")]
    view::configure_apple_seat(&mut cfg, display.clone(), seat);
    cfg.display = Some(display);
    Ok(cfg)
}
struct DevelopmentLogin;

impl apple_screen_sharing::auth::Authenticator for DevelopmentLogin {
    fn verify(&self, _username: &str, _password: &[u8]) -> bool {
        true
    }
}
