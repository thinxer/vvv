use super::Screen;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use socket2::{Domain, SockAddr, Socket, Type};
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::{Arc, Mutex};
use wayshare::display::apple::DisplayRequest;
use wayshare::rfb::{AppliedDisplay, DisplayControl, SessionId};
use wayshare::server_linux::ExternalScreen;

#[derive(Default)]
pub(super) struct Handoff {
    pub(super) active: Option<u32>,
    pub(super) pending: Option<(BTreeSet<u32>, u32, u32)>,
}

impl Handoff {
    pub(super) fn accepts(&self, id: u32, w: u32, h: u32) -> bool {
        self.active.is_none_or(|active| active == id)
            || self.pending.as_ref().is_some_and(|(old, width, height)| {
                !old.contains(&id) && (w, h) == (*width, *height)
            })
    }
}

// crosvm's Linux control Tube carries one serde JSON message per SOCK_SEQPACKET record.
fn gpu(path: &std::path::Path, command: Value) -> Result<Value> {
    let socket = Socket::new(Domain::UNIX, Type::from(libc::SOCK_SEQPACKET), None)?;
    let timeout = Some(std::time::Duration::from_secs(4));
    socket.set_read_timeout(timeout)?;
    socket.set_write_timeout(timeout)?;
    socket.connect(&SockAddr::unix(path)?)?;
    let request = serde_json::to_vec(&json!({"GpuCommand": command}))?;
    ensure!(
        socket.send(&request)? == request.len(),
        "short crosvm control write"
    );
    let mut reply = [0; 65536];
    let n = (&socket).read(&mut reply)?;
    ensure!(n < reply.len(), "truncated crosvm response");
    let mut result: Value = serde_json::from_slice(&reply[..n])?;
    result
        .get_mut("GpuResponse")
        .map(Value::take)
        .with_context(|| format!("crosvm rejected {command}: {result}"))
}

fn update(path: &std::path::Path, command: Value) -> Result<()> {
    let result = gpu(path, command)?;
    ensure!(
        result.as_str() == Some("DisplaysUpdated"),
        "GPU update failed: {result}"
    );
    Ok(())
}

fn ids(path: &std::path::Path) -> Result<BTreeSet<u32>> {
    let value = gpu(path, json!("ListDisplays"))?;
    value["DisplayList"]["displays"]
        .as_object()
        .context("missing crosvm displays")?
        .keys()
        .map(|id| Ok(id.parse()?))
        .collect()
}

fn remove(path: &std::path::Path, id: u32) -> Result<()> {
    update(path, json!({"RemoveDisplays":{"display_ids":[id]}}))
}

pub struct GuestDisplay {
    screen: Arc<Screen>,
    control: std::path::PathBuf,
    owner: Mutex<Option<SessionId>>,
    dpi: Mutex<u32>,
    #[cfg(feature = "combined")]
    pub(super) viewer: Mutex<std::sync::Weak<vvv_session::Viewer>>,
}

impl GuestDisplay {
    pub fn new(screen: Arc<Screen>, control: std::path::PathBuf) -> Self {
        Self {
            screen,
            control,
            owner: Mutex::new(None),
            dpi: Mutex::new(96),
            #[cfg(feature = "combined")]
            viewer: Default::default(),
        }
    }
    /// Skip cursor updates while a monitor handoff is in progress.
    pub(super) fn cursor_scale_eighths(&self) -> Option<u16> {
        self.dpi
            .try_lock()
            .ok()
            .map(|dpi| ((*dpi + 6) / 12).clamp(8, 32) as u16)
    }

    pub(super) fn apply_apple_layout(
        &self,
        layout: cuda_scanout::display::DisplayLayout,
    ) -> Result<()> {
        let (w, h) = layout.backing;
        let scale = f64::from(w) / f64::from(layout.logical.0);
        let dpi = (96.0 * scale.clamp(1.0, 4.0)).round() as u32;
        self.resize(w.into(), h.into(), dpi)
    }

    pub(super) fn resize(&self, w: u32, h: u32, dpi: u32) -> Result<()> {
        let mut active_dpi = self.dpi.lock().unwrap();
        if self.screen.size() == (w as u16, h as u16) && *active_dpi == dpi {
            return Ok(());
        }
        let path = &self.control;
        let before = ids(path)?;
        let old = {
            let mut state = self.screen.displays.lock().unwrap();
            let old = state.active.context("no active scanout")?;
            ensure!(before.contains(&old), "active scanout missing from crosvm");
            state.pending = Some((before.clone(), w, h));
            old
        };
        let added = update(
            path,
            json!({"AddDisplays":{"displays":[{
                "mode":{"windowed":[w,h]}, "dpi":[dpi,dpi], "refresh-rate":60, "hidden":false
            }]}}),
        );
        let current = {
            let state = self.screen.displays.lock().unwrap();
            let mut state = if added.is_ok() {
                self.screen
                    .display_changed
                    .wait_timeout_while(state, std::time::Duration::from_secs(4), |s| {
                        s.active == Some(old)
                    })
                    .unwrap()
                    .0
            } else {
                state
            };
            state.pending = None;
            state.active
        };
        added?;
        if current == Some(old) {
            let after = ids(path)?;
            let new: Vec<_> = after.difference(&before).copied().collect();
            if let [id] = new.as_slice() {
                remove(path, *id)?;
            }
            anyhow::bail!("guest did not present {w}x{h}");
        }
        ensure!(current.is_some(), "guest lost its scanout during resize");
        *active_dpi = dpi;
        remove(path, old)?;
        Ok(())
    }
}

impl DisplayControl for GuestDisplay {
    fn configure(&self, id: SessionId, req: &DisplayRequest) -> Result<Option<AppliedDisplay>> {
        let mut owner = self.owner.lock().unwrap();
        if owner.is_some_and(|s| s != id) {
            return Ok(None);
        }
        let mode = req
            .displays
            .first()
            .and_then(|d| d.preferred())
            .context("missing preferred mode")?;
        let (w, h) = (mode.pixel_width, mode.pixel_height);
        ensure!(
            (320..=7680).contains(&w) && (200..=4320).contains(&h),
            "unsupported display size {w}x{h}"
        );
        let (w, h) = (w.next_multiple_of(2), h.next_multiple_of(2));
        let pixels = (w as u16, h as u16);
        let points = match (
            u16::try_from(mode.point_width),
            u16::try_from(mode.point_height),
        ) {
            (Ok(x), Ok(y)) if x > 0 && y > 0 => (x, y),
            _ => pixels,
        };
        let layout = cuda_scanout::display::DisplayLayout {
            backing: pixels,
            logical: points,
        };
        #[cfg(feature = "combined")]
        if let Some(viewer) = self.viewer.lock().unwrap().upgrade() {
            viewer.layout(layout);
        }
        #[cfg(not(feature = "combined"))]
        self.apply_apple_layout(layout)?;
        *self.screen.apple_layout.lock().unwrap() = Some(layout);
        *owner = Some(id);
        let size = self.screen.size();
        #[cfg(feature = "combined")]
        let size = self
            .screen
            .views
            .get()
            .and_then(|r| r.borrow().apple_canvas(size))
            .unwrap_or(size);
        Ok(Some(self.screen.apple_display(size)))
    }
    fn restore(&self, id: SessionId) -> Result<()> {
        let mut owner = self.owner.lock().unwrap();
        if *owner == Some(id) {
            *owner = None;
            *self.screen.apple_layout.lock().unwrap() = None;
        }
        Ok(())
    }
}
