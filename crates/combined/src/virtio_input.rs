use anyhow::Result;
use apple_screen_sharing::input::pointer::{Axis, PtrAction};
use std::io::{ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct VirtioInput {
    listener: UnixListener,
    path: PathBuf,
    stream: Mutex<Option<UnixStream>>,
}

impl VirtioInput {
    pub fn bind(path: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
            stream: Mutex::new(None),
        })
    }

    pub fn key(&self, code: u32, down: bool) -> Result<()> {
        let mut packet = Vec::with_capacity(16);
        packet.extend(1u16.to_le_bytes());
        packet.extend((code as u16).to_le_bytes());
        packet.extend(i32::from(down).to_le_bytes());
        self.report(packet)
    }

    pub fn pointer(&self, actions: Vec<PtrAction>) -> Result<()> {
        let mut packet = Vec::new();
        let mut event = |kind: u16, code: u16, value: i32| {
            packet.extend(kind.to_le_bytes());
            packet.extend(code.to_le_bytes());
            packet.extend(value.to_le_bytes());
        };
        for action in actions {
            match action {
                PtrAction::Motion {
                    x,
                    y,
                    width,
                    height,
                } => {
                    let abs = |pos: u32, extent: u32| {
                        (pos as i64 * 32767 / i64::from(extent.saturating_sub(1).max(1))) as i32
                    };
                    event(3, 0, abs(x, width));
                    event(3, 1, abs(y, height));
                }
                PtrAction::Button { code, pressed } => event(1, code as u16, pressed.into()),
                PtrAction::AxisDiscrete { axis, steps, .. } => wheel(&mut event, axis, steps),
                PtrAction::Axis { axis, value } if value != 0.0 => {
                    let ticks = (value / 15.0).round() as i32;
                    wheel(
                        &mut event,
                        axis,
                        if ticks == 0 {
                            value.signum() as i32
                        } else {
                            ticks
                        },
                    );
                }
                _ => (),
            }
        }
        if packet.is_empty() {
            return Ok(());
        }
        self.report(packet)
    }

    pub(super) fn report(&self, mut packet: Vec<u8>) -> Result<()> {
        packet.extend([0; 8]); // SYN_REPORT ends one virtio input report.
        let mut stream = self.stream.lock().unwrap();
        match self.listener.accept() {
            Ok((new, _)) => *stream = Some(new),
            Err(e) if e.kind() == ErrorKind::WouldBlock => (),
            Err(e) => return Err(e.into()),
        }
        if let Some(client) = stream.as_mut() {
            if let Err(e) = client.write_all(&packet) {
                *stream = None;
                return Err(e.into());
            }
        }
        Ok(())
    }
}

fn wheel(emit: &mut impl FnMut(u16, u16, i32), axis: Axis, steps: i32) {
    match axis {
        Axis::Vertical => emit(2, 8, -steps),
        Axis::Horizontal => emit(2, 6, steps),
    }
}

impl Drop for VirtioInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
