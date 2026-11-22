use super::{ServerConfig, Screen, resize::GuestDisplay, virtio_input::VirtioInput};
use anyhow::Result;
use vvv_rdp::{
    Encoder, Event, Receiver,
    input::{Input, InputDevice},
};
use std::sync::{Arc, Mutex};
use vvv_session::{Viewer, ViewerSeat};
use wayshare::{capture::DmabufBuffer, rfb::Config, server_linux::ExternalScreen};

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

pub(super) fn frame(rdp: &Receiver, buffer: &DmabufBuffer) {
    if let Some(gpu) = buffer.cuda() {
        rdp.shared_gpu_frame(gpu.clone());
    } else {
        let _ = buffer.with_read(|g| {
            rdp.frame(
                buffer.width,
                buffer.height,
                g.stride as u32,
                bytes::Bytes::copy_from_slice(g.bytes()),
            )
        });
    }
}

fn start_view_updates(screen: &Arc<Screen>, seat: &Arc<ViewerSeat>) -> Result<()> {
    screen
        .seat
        .set(seat.clone())
        .map_err(|_| anyhow::anyhow!("duplicate viewer seat"))?;
    let mut updates = seat.subscribe();
    screen
        .views
        .set(updates.clone())
        .map_err(|_| anyhow::anyhow!("duplicate view subscription"))?;
    let weak = Arc::downgrade(screen);
    let motion = screen.observer_motion.clone();
    let cursor_seat = seat.clone();
    tokio::spawn(async move {
        let mut next_motion = tokio::time::Instant::now();
        loop {
            let roles_changed = tokio::select! {
                res=updates.changed()=>{if res.is_err(){break} true},
                _=motion.notified()=>{
                    tokio::time::sleep_until(next_motion).await;
                    next_motion = tokio::time::Instant::now() + std::time::Duration::from_micros(16_667);
                    false
                },
            };
            let Some(screen) = weak.upgrade() else { break };
            // A role or window change also needs a frame on an idle desktop.
            // Serialize with the producer before taking a snapshot of the last scanout.
            let _handoff = screen.displays.lock().unwrap();
            let mut latest = screen.latest.lock().unwrap();
            let Some(last) = latest.as_mut() else {
                continue;
            };
            let views = updates.borrow().clone();
            let desired = views.status();
            let position = *screen.pointer_position.lock().unwrap();
            let near = last.badge_near(position);
            let changed = {
                let shape = screen.cursor.lock().unwrap();
                let mut current = cursor_seat.observer_cursor.lock().unwrap();
                let previous = current.as_ref().map(|c| c.key());
                match (shape.as_ref(), position) {
                    (Some(s), Some(pos)) if desired.is_some() && s.serial != 0 => {
                        if let Some(c) = current.as_mut().filter(|c| c.serial == s.serial) {
                            c.position = pos;
                        } else {
                            *current = Some(cuda_scanout::overlay::ObserverCursor {
                                position: pos,
                                hotspot: s.hotspot,
                                width: s.width,
                                height: s.height,
                                pixels: s.pixels.clone().into(),
                                serial: s.serial,
                            });
                        }
                    }
                    _ => *current = None,
                }
                previous != current.as_ref().map(|c| c.key())
            };
            if !roles_changed && !near {
                if changed && views.apple_observing() {
                    last.seq += 1;
                    last.captured = wayshare::capture::monotonic_now();
                    screen.changed.notify_all();
                }
                if changed
                    && views.rdp_observing()
                    && let Some(rdp) = screen.rdp.get().and_then(|r| r.upgrade())
                {
                    rdp.refresh_views();
                }
                continue;
            }
            if !near && desired == last.badge.as_ref().map(|(key, _)| *key) {
                last.seq += 1;
                if let Some(rdp) = screen.rdp.get().and_then(|r| r.upgrade()) {
                    rdp.refresh_views();
                }
                screen.changed.notify_all();
            } else {
                let clean = last
                    .buffer
                    .clean_copy(last.badge.as_ref().map(|(_, stamp)| stamp.as_ref()));
                drop(latest);
                match clean {
                    Ok(buffer) => screen.publish(buffer, wayshare::capture::monotonic_now()),
                    Err(e) => tracing::warn!("refreshing viewer status: {e:#}"),
                }
            }
        }
    });
    Ok(())
}

pub(super) fn start(
    cfg: &mut Config,
    args: &ServerConfig,
    screen: &Arc<Screen>,
    display: Arc<GuestDisplay>,
    keyboard: Arc<VirtioInput>,
    tablet: Arc<VirtioInput>,
) -> Result<Arc<Receiver>> {
    let seat = Arc::new(ViewerSeat::default());
    start_view_updates(screen, &seat)?;
    let apple_seat = seat.clone();
    let make_input = cfg.input.take().expect("guest input initialized");
    let apple_display = display.clone();
    cfg.viewer_slot = Some(Default::default());
    cfg.input = Some(Arc::new(move |w, h| {
        let sink = Arc::new(Mutex::new(make_input(w, h)?));
        let reset = sink.clone();
        let resize = apple_display.clone();
        let viewer = Arc::new(apple_seat.join(
            "Apple",
            move || {
                if let Err(e) = reset.lock().unwrap().release_all() {
                    tracing::warn!("releasing Apple input: {e:#}");
                }
            },
            move |layout| {
                if let Err(e) = resize.apply_apple_layout(layout) {
                    tracing::warn!("Apple resize: {e:#}");
                }
            },
        ));
        *apple_display.viewer.lock().unwrap() = Arc::downgrade(&viewer);
        Ok(Box::new(AppleInput { viewer, sink }))
    }));
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
        Encoder::Nvenc(args.avg_bitrate),
        Some(seat),
    )?);
    // Serialize subscription with publishing so an older snapshot cannot replace a new one.
    let _handoff = screen.displays.lock().unwrap();
    screen
        .rdp
        .set(Arc::downgrade(&receiver))
        .map_err(|_| anyhow::anyhow!("duplicate RDP receiver"))?;
    if let Some(latest) = screen.latest.lock().unwrap().as_ref() {
        frame(&receiver, &latest.buffer);
    }
    tracing::info!(%addr,"shared desktop RDP listener ready");
    Ok(receiver)
}

use wayshare::input::{InputSink, ScrollEvent};

struct AppleInput {
    viewer: Arc<Viewer>,
    sink: Arc<Mutex<Box<dyn InputSink>>>,
}

impl InputSink for AppleInput {
    fn key(&mut self, down: bool, sym: u32, code: Option<u16>) -> Result<()> {
        self.viewer
            .input(|| self.sink.lock().unwrap().key(down, sym, code))
            .unwrap_or(Ok(()))
    }
    fn pointer(&mut self, x: i32, y: i32, buttons: u8) -> Result<()> {
        let send = || {
            self.viewer
                .pointer(7, buttons & 7, (x, y), || {
                    self.sink.lock().unwrap().pointer(x, y, buttons)
                })
                .unwrap_or(Ok(()))
        };
        // Taking control can synchronously hotplug the guest display. Let Tokio
        // keep accepting CUDA scanouts while crosvm waits for their completion.
        if buttons & 1 != 0 && cuda_scanout::overlay::Stamp::hit(x, y) {
            tokio::task::block_in_place(send)
        } else {
            send()
        }
    }
    fn scroll(&mut self, event: &ScrollEvent) -> Result<()> {
        self.viewer
            .input(|| self.sink.lock().unwrap().scroll(event))
            .unwrap_or(Ok(()))
    }
    fn set_extent(&mut self, w: u32, h: u32) {
        self.sink.lock().unwrap().set_extent(w, h);
    }
    fn set_input_source(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        self.viewer
            .input(|| self.sink.lock().unwrap().release_all())
            .unwrap_or(Ok(()))
    }
}

pub struct AppleView {
    screen: Arc<Screen>,
    cache: Mutex<Option<(u64, (u16, u16), Arc<DmabufBuffer>)>>,
}
impl AppleView {
    pub fn new(screen: Arc<Screen>) -> Self {
        Self {
            screen,
            cache: Default::default(),
        }
    }
    fn canvas(&self, guest: (u16, u16)) -> Option<(u16, u16)> {
        self.screen.views.get()?.borrow().apple_canvas(guest)
    }
}
impl ExternalScreen for AppleView {
    fn display(&self) -> wayshare::rfb::AppliedDisplay {
        self.screen.apple_display(self.size())
    }
    fn size(&self) -> (u16, u16) {
        let guest = self.screen.size();
        self.canvas(guest).unwrap_or(guest)
    }
    fn next_frame(
        &self,
        after: u64,
        timeout: std::time::Duration,
    ) -> Option<wayshare::capture::Frame> {
        let mut frame = self.screen.next_frame(after, timeout)?;
        let mut cache = self.cache.lock().unwrap();
        let Some(canvas) = self.canvas((frame.buffer.width as u16, frame.buffer.height as u16))
        else {
            *cache = None;
            return Some(frame);
        };
        if let Some((seq, size, buffer)) = cache.as_ref()
            && (*seq, *size) == (frame.seq, canvas)
        {
            frame.buffer = buffer.clone();
        } else {
            let cursor = self
                .screen
                .views
                .get()
                .filter(|v| v.borrow().apple_observing())
                .and_then(|_| self.screen.seat.get())
                .and_then(|seat| seat.observer_cursor.lock().unwrap().clone());
            let fitted = match frame.buffer.fitted(canvas.0, canvas.1, cursor.as_ref()) {
                Ok(b) => Arc::new(b),
                Err(e) => {
                    tracing::warn!("fitting Apple view: {e:#}");
                    return None;
                }
            };
            *cache = Some((frame.seq, canvas, fitted.clone()));
            frame.buffer = fitted;
        }
        frame.damage = vec![wayshare::encode::Rect {
            x: 0,
            y: 0,
            width: canvas.0.into(),
            height: canvas.1.into(),
        }];
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wayshare::source::CursorImage;

    #[tokio::test]
    async fn idle_observer_cursor_moves_and_clears_on_handoff() -> Result<()> {
        let screen = Arc::new(Screen::default());
        let seat = Arc::new(ViewerSeat::default());
        start_view_updates(&screen, &seat)?;
        let size = (320, 200);
        screen.publish(
            DmabufBuffer::from_bgrx(size.0, size.1, vec![0; 320 * 200 * 4])?,
            wayshare::capture::monotonic_now(),
        );
        let rdp = seat.join("RDP", || {}, |_| {});
        let apple = seat.join("Apple", || {}, |_| {});
        let view = AppleView::new(screen.clone());
        screen.cursor.lock().unwrap().replace(Arc::new(CursorImage {
            serial: 17,
            width: 1,
            height: 1,
            hotspot: (0, 0),
            pixels: vec![0xff123456],
        }));
        let mut after = 0;
        for (old, new) in [(100usize, 120usize), (120, 180)] {
            screen.set_pointer((new as i32, 100));
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    if let Some(f) = view.next_frame(after, Duration::ZERO) {
                        after = f.seq;
                        if f.buffer
                            .with_read(|g| {
                                let i = 100 * g.stride;
                                g.bytes()[i + new * 4..i + new * 4 + 3] == [0x56, 0x34, 0x12]
                                    && g.bytes()[i + old * 4..i + old * 4 + 3] == [0; 3]
                            })
                            .unwrap()
                        {
                            break;
                        }
                    }
                }
            })
            .await?;
        }
        apple.pointer(1, 1, (20, 20), || {});
        assert!(rdp.observing());
        let f = view.next_frame(0, Duration::ZERO).unwrap();
        f.buffer
            .with_read(|g| assert_eq!(&g.bytes()[100 * g.stride + 180 * 4..][..3], &[0; 3]))?;
        Ok(())
    }
}
