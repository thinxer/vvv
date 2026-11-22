use crate::{
    Screen, ServerConfig, desktop::DesktopOutput, resize::GuestDisplay, virtio_input::VirtioInput,
};
use anyhow::Result;
use apple_screen_sharing::server::ExternalScreen;
use std::sync::{Arc, Mutex};
use vvv_nvenc::capture::DmabufBuffer;
use vvv_rdp::{
    Encoder, Event, Receiver,
    input::{Input, InputDevice},
};
use vvv_session::ViewerSeat;
struct Device(Arc<VirtioInput>);
impl InputDevice for Device {
    fn send(&mut self, events: &[(u16, u16, i32)]) {
        let mut bytes = Vec::with_capacity(8 * (events.len() + 1));
        for &(kind, code, value) in events {
            bytes.extend(kind.to_le_bytes());
            bytes.extend(code.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        if let Err(e) = self.0.report(bytes) {
            tracing::warn!("virtio report: {e}");
        }
    }
}

impl DesktopOutput for Receiver {
    fn publish_frame(&self, buffer: &DmabufBuffer) {
        if let Some(gpu) = buffer.cuda() {
            self.shared_gpu_frame(gpu.clone());
        } else {
            let _ = buffer.with_read(|g| {
                self.frame(
                    buffer.width,
                    buffer.height,
                    g.stride as u32,
                    bytes::Bytes::copy_from_slice(g.bytes()),
                )
            });
        }
    }

    fn publish_cursor(
        &self,
        (w, h): (u32, u32),
        pixels: &[u8],
        (x, y): (i32, i32),
        (mx, my): (i32, i32),
    ) {
        self.cursor(w, h, w * 4, pixels, x, y, mx, my);
    }
    fn refresh_views(&self) {
        Receiver::refresh_views(self);
    }
}
pub(super) fn start(
    args: &ServerConfig,
    screen: &Arc<Screen>,
    display: Arc<GuestDisplay>,
    keyboard: Arc<VirtioInput>,
    tablet: Arc<VirtioInput>,
    seat: Arc<ViewerSeat>,
    encoder: Encoder,
) -> Result<Arc<Receiver>> {
    let desktop = screen.clone();
    let input = Mutex::new(Input::from_devices(
        Box::new(Device(keyboard)),
        Box::new(Device(tablet)),
    ));
    let mut addr = args.listen;
    addr.set_port(args.rdp_port);
    let receiver = Arc::new(vvv_rdp::start_on(
        addr,
        Arc::new(move |event, a, b| {
            if matches!(event, Event::Resize) {
                if (320..=7680).contains(&a) && (200..=4320).contains(&b) {
                    if let Err(e) = display.resize(a as u32, b as u32, 96) {
                        tracing::warn!("RDP resize: {e:#}");
                    }
                }
            } else {
                let mut input = input.lock().unwrap();
                let (w, h) = desktop.size();
                input.size = (w.into(), h.into());
                if let Some(pos) = *desktop.pointer_position.lock().unwrap() {
                    input.position = pos;
                }
                let moved = matches!(event, Event::Move | Event::Relative);
                input.event(event, a, b);
                if moved {
                    desktop.set_pointer(input.position);
                }
            }
        }),
        true,
        encoder,
        Some(seat),
    )?);
    screen.attach_output(receiver.clone())?;
    tracing::info!(%addr,"shared desktop RDP listener ready");
    Ok(receiver)
}
