//! crosvm framebuffer adapter for standard RFB and NVIDIA High Performance video.
pub mod set_password;
use apple_screen_sharing::{
    rfb::Config,
    server::{ExternalScreen, HpServer},
    source::{self, FrameSource},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use vvv_nvenc::{EncoderConfig, capture::DmabufBuffer};

/// Attach standard framebuffer reads and pipelined High Performance media to an Apple session.
pub fn configure_video(
    cfg: &mut Config,
    output: Arc<dyn ExternalScreen<DmabufBuffer>>,
    listen: SocketAddr,
    media_port: u16,
    bitrate: std::num::NonZeroU32,
) -> anyhow::Result<()> {
    let encoder = EncoderConfig {
        bitrate_bps: Some(u64::from(bitrate.get())),
        ..Default::default()
    };
    encoder.check()?;
    let standard = output.clone();
    cfg.frame_source = Arc::new(move || {
        let frame = standard
            .next_frame(0, Duration::ZERO)
            .ok_or_else(|| anyhow::anyhow!("guest scanout unavailable"))?;
        Ok(Box::new(Standard {
            screen: standard.clone(),
            seq: frame.seq,
            buffer: frame.buffer,
            pixels: Default::default(),
        }))
    });
    cfg.hp = Some(Arc::new(HpServer::new(
        output,
        listen,
        media_port,
        Arc::new(encoder),
        Some(u64::from(bitrate.get())),
        true,
        false,
    )));
    Ok(())
}
struct Standard {
    screen: Arc<dyn ExternalScreen<DmabufBuffer>>,
    seq: u64,
    buffer: Arc<DmabufBuffer>,
    pixels: std::cell::OnceCell<source::Frame>,
}

impl FrameSource for Standard {
    fn size(&self) -> (u16, u16) {
        (self.buffer.width as u16, self.buffer.height as u16)
    }
    fn frame(&self) -> &source::Frame {
        // The HP handshake also constructs a standard source. Read back pixels only when
        // the viewer actually requests a standard framebuffer update.
        self.pixels.get_or_init(|| {
            let (width, height) = self.size();
            let pixels = self
                .buffer
                .with_read(|guard| {
                    guard
                        .bytes()
                        .chunks_exact(4)
                        .map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0]))
                        .collect()
                })
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "standard framebuffer read failed");
                    vec![0; width as usize * height as usize]
                });
            source::Frame {
                width,
                height,
                pixels,
            }
        })
    }
    fn advance(&mut self) -> bool {
        let Some(frame) = self.screen.next_frame(self.seq, Duration::ZERO) else {
            return false;
        };
        self.buffer = frame.buffer;
        self.seq = frame.seq;
        self.pixels.take();
        true
    }
}
