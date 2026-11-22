use crate::{Screen, resize::GuestDisplay};
use anyhow::Result;
use apple_screen_sharing::{rfb::Config, server::ExternalScreen};
use std::sync::{Arc, Mutex};
use vvv_nvenc::capture::DmabufBuffer;
use vvv_session::{Viewer, ViewerSeat};
pub(super) fn configure_apple_seat(
    cfg: &mut Config,
    display: Arc<GuestDisplay>,
    seat: Arc<ViewerSeat>,
) {
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
}

use apple_screen_sharing::input::{InputSink, ScrollEvent};

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
impl ExternalScreen<DmabufBuffer> for AppleView {
    fn display(&self) -> apple_screen_sharing::rfb::AppliedDisplay {
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
    ) -> Option<vvv_nvenc::capture::Frame> {
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
        frame.damage = vec![apple_screen_sharing::encode::Rect {
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
    use anyhow::Result;
    use apple_screen_sharing::source::CursorImage;
    use std::time::Duration;

    #[tokio::test]
    async fn idle_observer_cursor_moves_and_clears_on_handoff() -> Result<()> {
        let screen = Arc::new(Screen::default());
        let seat = Arc::new(ViewerSeat::default());
        crate::desktop::start_view_updates(&screen, &seat)?;
        let size = (320, 200);
        screen.publish(
            DmabufBuffer::from_bgrx(size.0, size.1, vec![0; 320 * 200 * 4])?,
            apple_screen_sharing::capture::monotonic_now(),
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
