//! Linux virtio input reports. crosvm connects as a client on each Unix stream.
use crate::Event;
use anyhow::{Context, Result};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    os::unix::{
        fs::FileTypeExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

struct Device {
    listener: UnixListener,
    client: Option<UnixStream>,
    path: PathBuf,
}
impl Device {
    fn bind(path: &Path) -> Result<Self> {
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
            && UnixStream::connect(path)
                .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionRefused)
        {
            fs::remove_file(path)?;
        }
        let listener =
            UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            client: None,
            path: path.into(),
        })
    }
}
pub trait InputDevice: Send {
    fn send(&mut self, events: &[(u16, u16, i32)]);
    fn key(&mut self, code: u16, down: bool) {
        self.send(&[(1, code, i32::from(down))]);
    }
}
impl InputDevice for Device {
    fn send(&mut self, events: &[(u16, u16, i32)]) {
        while let Ok((client, _)) = self.listener.accept() {
            self.client = Some(client);
        }
        if let Some(client) = self.client.as_mut() {
            let mut data = Vec::with_capacity(8 * (events.len() + 1));
            for &(kind, code, value) in events {
                data.extend(kind.to_le_bytes());
                data.extend(code.to_le_bytes());
                data.extend(value.to_le_bytes());
            }
            data.extend([0; 8]); // SYN_REPORT
            if let Err(e) = client.write_all(&data) {
                eprintln!("virtio input: {e}");
                self.client = None;
            }
        }
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub struct Input {
    keyboard: Box<dyn InputDevice>,
    tablet: Box<dyn InputDevice>,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u16>,
    pub position: (i32, i32),
    pub size: (u32, u32),
}
impl Input {
    pub fn new(keyboard: &Path, tablet: &Path) -> Result<Self> {
        Ok(Self::from_devices(
            Box::new(Device::bind(keyboard)?),
            Box::new(Device::bind(tablet)?),
        ))
    }
    pub fn from_devices(keyboard: Box<dyn InputDevice>, tablet: Box<dyn InputDevice>) -> Self {
        Self {
            keyboard,
            tablet,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            position: (0, 0),
            size: (1408, 1008),
        }
    }
    fn motion(&mut self, x: i32, y: i32) {
        let axis = |pos: i32, extent: u32| {
            let max = extent.saturating_sub(1).max(1) as i32;
            (i64::from(pos.clamp(0, max)) * 32767 / i64::from(max)) as i32
        };
        self.position = (x, y);
        self.tablet
            .send(&[(3, 0, axis(x, self.size.0)), (3, 1, axis(y, self.size.1))]);
    }
    pub fn event(&mut self, event: Event, a: i32, b: i32) {
        match event {
            Event::Reset => {
                for k in std::mem::take(&mut self.keys) {
                    self.keyboard.key(k, false);
                }
                for k in std::mem::take(&mut self.buttons) {
                    self.tablet.key(k, false);
                }
            }
            Event::Key => {
                if let Some(key) = scancode(a) {
                    if transition(&mut self.keys, key, b != 0) {
                        self.keyboard.key(key, b != 0);
                    }
                }
            }
            Event::Unicode => {
                if let Some((key, shift)) = ascii_key(a) {
                    if self.keys.contains(&key) {
                        return;
                    }
                    let shift = shift && !self.keys.contains(&42) && !self.keys.contains(&54);
                    if shift {
                        self.keyboard.key(42, true);
                    }
                    self.keyboard.key(key, true);
                    self.keyboard.key(key, false);
                    if shift {
                        self.keyboard.key(42, false);
                    }
                }
            }
            Event::Move => self.motion(a, b),
            Event::Relative => self.motion(
                self.position.0.saturating_add(a).max(0),
                self.position.1.saturating_add(b).max(0),
            ),
            Event::Button => {
                let k = a as u16;
                if transition(&mut self.buttons, k, b != 0) {
                    self.tablet.key(k, b != 0);
                }
            }
            Event::Wheel => {
                for (code, value) in [(6, a), (8, b.saturating_neg())] {
                    if value != 0 {
                        let ticks = value / 15;
                        self.tablet.send(&[(
                            2,
                            code,
                            if ticks == 0 { value.signum() } else { ticks },
                        )]);
                    }
                }
            }
            Event::Resize => {}
        }
    }
}
fn transition(held: &mut BTreeSet<u16>, key: u16, down: bool) -> bool {
    if down {
        held.insert(key)
    } else {
        held.remove(&key)
    }
}
// XT set 1 matches evdev for the base keys. E0-prefixed keys are remapped.
fn scancode(code: i32) -> Option<u16> {
    Some(match code {
        1..=0x58 => code as u16,
        0x11c => 96,
        0x11d => 97,
        0x135 => 98,
        0x137 => 99,
        0x138 => 100,
        0x147 => 102,
        0x148 => 103,
        0x149 => 104,
        0x14b => 105,
        0x14d => 106,
        0x14f => 107,
        0x150 => 108,
        0x151 => 109,
        0x152 => 110,
        0x153 => 111,
        0x15b => 125,
        0x15c => 126,
        0x15d => 127,
        _ => return None,
    })
}

fn ascii_key(ch: i32) -> Option<(u16, bool)> {
    let ch = u8::try_from(ch).ok()?;
    for (base, plain, shifted) in [
        (2, &b"1234567890-="[..], &b"!@#$%^&*()_+"[..]),
        (16, &b"qwertyuiop[]"[..], &b"QWERTYUIOP{}"[..]),
        (30, &b"asdfghjkl;'\x60"[..], &b"ASDFGHJKL:\"~"[..]),
        (44, &b"zxcvbnm,./"[..], &b"ZXCVBNM<>?"[..]),
        (43, &b"\\"[..], &b"|"[..]),
    ] {
        for (row, shift) in [(plain, false), (shifted, true)] {
            if let Some(i) = row.iter().position(|&key| key == ch) {
                return Some((base + i as u16, shift));
            }
        }
    }
    Some((
        match ch {
            b' ' => 57,
            b'\r' | b'\n' => 28,
            b'\t' => 15,
            8 => 14,
            _ => return None,
        },
        false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn reset_releases_only_held_keys_and_buttons() -> Result<()> {
        let base = std::env::temp_dir().join(format!("iron-input-{}", std::process::id()));
        let k = base.with_extension("keys");
        let m = base.with_extension("mouse");
        let mut input = Input::new(&k, &m)?;
        let mut keys = UnixStream::connect(k)?;
        let mut mouse = UnixStream::connect(m)?;
        input.event(Event::Key, 0x11d, 1);
        input.event(Event::Key, 0x11d, 1);
        input.event(Event::Button, 272, 1);
        input.event(Event::Reset, 0, 0);
        for (stream, code) in [(&mut keys, 97u16), (&mut mouse, 272u16)] {
            stream.set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
            let mut reports = [0u8; 32];
            stream.read_exact(&mut reports)?;
            for (report, down) in reports.chunks_exact(16).zip([1i32, 0]) {
                assert_eq!(report[..2], 1u16.to_le_bytes());
                assert_eq!(report[2..4], code.to_le_bytes());
                assert_eq!(report[4..8], down.to_le_bytes());
                assert_eq!(report[8..], [0; 8]);
            }
        }
        Ok(())
    }
}
