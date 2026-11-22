//! Shared guest scanout, presentation and observer updates.
mod capture;
use crate::resize;
use apple_screen_sharing::{encode::Rect, server::ExternalScreen, source};
pub(super) use capture::Capture;
use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use vvv_nvenc::capture::{DmabufBuffer, Frame};
#[cfg(feature = "combined")]
use {anyhow::Result, vvv_session::ViewerSeat};
#[cfg(feature = "combined")]
type Badge = ((&'static str, usize), Arc<cuda_scanout::overlay::Stamp>);

struct Scanout {
    #[cfg(feature = "combined")]
    badge: Option<Badge>,
    seq: u64,
    captured: Duration,
    buffer: Arc<DmabufBuffer>,
}

#[cfg(feature = "combined")]
impl Scanout {
    fn badge_near(&self, pos: Option<(i32, i32)>) -> bool {
        self.badge.as_ref().is_some_and(|(_, s)| {
            !s.avoids(self.buffer.width as usize, self.buffer.height as usize, pos)
        })
    }
}

#[derive(Default)]
pub(super) struct Screen {
    pub(super) apple_layout: Mutex<Option<cuda_scanout::display::DisplayLayout>>,
    #[cfg(feature = "combined")]
    observer_motion: Arc<tokio::sync::Notify>,
    #[cfg(feature = "combined")]
    pub(super) views: std::sync::OnceLock<tokio::sync::watch::Receiver<vvv_session::Views>>,
    #[cfg(feature = "combined")]
    pub(super) seat: std::sync::OnceLock<Arc<vvv_session::ViewerSeat>>,
    #[cfg(feature = "combined")]
    output: std::sync::OnceLock<std::sync::Weak<dyn DesktopOutput>>,
    pub(super) cursor: Mutex<Option<Arc<source::CursorImage>>>,
    pub(super) pointer_position: Mutex<Option<(i32, i32)>>,
    pub(super) displays: Mutex<resize::Handoff>,
    pub(super) display_changed: Condvar,
    latest: Mutex<Option<Scanout>>,
    changed: Condvar,
    pub(super) last_input: Mutex<Option<Instant>>,
}

#[cfg(feature = "combined")]
impl Drop for Screen {
    fn drop(&mut self) {
        self.observer_motion.notify_one();
    }
}

impl Screen {
    pub(super) fn apple_display(
        &self,
        pixels: (u16, u16),
    ) -> apple_screen_sharing::rfb::AppliedDisplay {
        let layout = *self.apple_layout.lock().unwrap();
        let points = layout
            .filter(|l| l.backing == pixels)
            .map_or(pixels, |l| l.logical);
        apple_screen_sharing::rfb::AppliedDisplay {
            pixels,
            points,
            scale: if points.0 == 0 {
                1.0
            } else {
                f64::from(pixels.0) / f64::from(points.0)
            },
        }
    }

    pub(super) fn set_pointer(&self, pos: (i32, i32)) {
        if self.pointer_position.lock().unwrap().replace(pos) != Some(pos) {
            #[cfg(feature = "combined")]
            self.notify_observers();
        }
    }
    #[cfg(feature = "combined")]
    fn notify_observers(&self) {
        if self
            .views
            .get()
            .is_some_and(|v| v.borrow().status().is_some())
        {
            self.observer_motion.notify_one();
        }
    }

    fn commit(&self, id: u32, buffer: DmabufBuffer) {
        let mut handoff = self.displays.lock().unwrap();
        if !handoff.accepts(id, buffer.width, buffer.height) {
            return;
        }
        self.publish(buffer, apple_screen_sharing::capture::monotonic_now());
        if handoff.active != Some(id) {
            handoff.active = Some(id);
            handoff.pending = None;
            self.display_changed.notify_all();
        }
    }
    pub(super) fn publish(&self, buffer: DmabufBuffer, captured: Duration) {
        let mut latest = self.latest.lock().unwrap();
        #[cfg(feature = "combined")]
        let (buffer, badge) = {
            let mut buffer = buffer;
            let desired = self.views.get().and_then(|r| r.borrow().status());
            let previous = latest.as_ref().and_then(|f| f.badge.as_ref());
            let badge = desired.map(|key| {
                let stamp = previous
                    .filter(|(old, _)| *old == key)
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| {
                        Arc::new(cuda_scanout::overlay::Stamp::status(key.0, key.1))
                    });
                let pointer = *self.pointer_position.lock().unwrap();
                let stamp = if stamp.avoids(buffer.width as usize, buffer.height as usize, pointer)
                {
                    stamp
                } else {
                    Arc::new(stamp.next_clear_corner(
                        buffer.width as usize,
                        buffer.height as usize,
                        pointer,
                    ))
                };
                (key, stamp)
            });
            if let Some((_, stamp)) = &badge
                && let Err(e) = buffer.stamp_status(stamp)
            {
                tracing::warn!("status composition failed: {e:#}");
                return;
            }
            if let Some(output) = self.output.get().and_then(|r| r.upgrade()) {
                output.publish_frame(&buffer);
            }
            (buffer, badge)
        };
        let seq = latest.as_ref().map_or(1, |s| s.seq + 1);
        *latest = Some(Scanout {
            seq,
            captured,
            buffer: Arc::new(buffer),
            #[cfg(feature = "combined")]
            badge,
        });
        self.changed.notify_all();
    }
}

impl ExternalScreen<DmabufBuffer> for Screen {
    fn display(&self) -> apple_screen_sharing::rfb::AppliedDisplay {
        self.apple_display(self.size())
    }
    fn size(&self) -> (u16, u16) {
        self.latest
            .lock()
            .unwrap()
            .as_ref()
            .map_or((0, 0), |s| (s.buffer.width as u16, s.buffer.height as u16))
    }

    fn next_frame(&self, after: u64, timeout: Duration) -> Option<Frame> {
        let (latest, _) = self
            .changed
            .wait_timeout_while(self.latest.lock().unwrap(), timeout, |latest| {
                latest.as_ref().is_none_or(|s| s.seq == after)
            })
            .unwrap();
        let Scanout {
            seq,
            captured,
            buffer,
            ..
        } = latest.as_ref()?;
        // Keep the Mac's playout clock moving briefly after input on static desktops.
        // Real scanouts wake the condition immediately and always take priority.
        if *seq == after
            && (timeout < Duration::from_millis(16)
                || !self
                    .last_input
                    .lock()
                    .unwrap()
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(250)))
        {
            return None;
        }
        let now = Instant::now();
        Some(Frame {
            seq: *seq,
            buffer: buffer.clone(),
            damage: vec![Rect {
                x: 0,
                y: 0,
                width: buffer.width,
                height: buffer.height,
            }],
            // A new viewer samples the current desktop; repeats sample it again.
            presented: (after != 0 && *seq != after).then_some(*captured),
            requested_at: now,
            ready_at: now,
        })
    }
}

#[cfg(feature = "combined")]
pub(super) trait DesktopOutput: Send + Sync {
    fn publish_frame(&self, buffer: &DmabufBuffer);
    fn publish_cursor(
        &self,
        size: (u32, u32),
        pixels: &[u8],
        hotspot: (i32, i32),
        position: (i32, i32),
    );
    fn refresh_views(&self);
}

#[cfg(feature = "combined")]
impl Screen {
    pub(super) fn attach_output(&self, output: Arc<dyn DesktopOutput>) -> Result<()> {
        // Serialize subscription with publication so an old snapshot cannot replace a new frame.
        let _handoff = self.displays.lock().unwrap();
        self.output
            .set(Arc::downgrade(&output))
            .map_err(|_| anyhow::anyhow!("duplicate desktop output"))?;
        if let Some(latest) = self.latest.lock().unwrap().as_ref() {
            output.publish_frame(&latest.buffer);
        }
        Ok(())
    }
}
#[cfg(feature = "combined")]
pub(super) fn start_view_updates(screen: &Arc<Screen>, seat: &Arc<ViewerSeat>) -> Result<()> {
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
                    last.captured = apple_screen_sharing::capture::monotonic_now();
                    screen.changed.notify_all();
                }
                if changed
                    && views.rdp_observing()
                    && let Some(output) = screen.output.get().and_then(|r| r.upgrade())
                {
                    output.refresh_views();
                }
                continue;
            }
            if !near && desired == last.badge.as_ref().map(|(key, _)| *key) {
                last.seq += 1;
                if let Some(output) = screen.output.get().and_then(|r| r.upgrade()) {
                    output.refresh_views();
                }
                screen.changed.notify_all();
            } else {
                let clean = last
                    .buffer
                    .clean_copy(last.badge.as_ref().map(|(_, stamp)| stamp.as_ref()));
                drop(latest);
                match clean {
                    Ok(buffer) => {
                        screen.publish(buffer, apple_screen_sharing::capture::monotonic_now())
                    }
                    Err(e) => tracing::warn!("refreshing viewer status: {e:#}"),
                }
            }
        }
    });
    Ok(())
}
