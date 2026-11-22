use std::num::NonZeroU32;

pub const DEFAULT_AVG_BITRATE: NonZeroU32 = NonZeroU32::new(8_000_000).unwrap();

pub mod display;
mod driver;
mod nv12;
pub mod overlay;
mod presenter;
use anyhow::{Result, ensure};
use driver::*;
pub use nv12::Nv12Tile;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

// Each socket transaction transfers ownership of one immutable frame to the receiver.
// A producer may only write between receiving the CUDA handle and closing its mapping.
pub struct Frame {
    pub width: u16,
    pub height: u16,
    pub pitch: usize,
    ptr: u64,
}
impl Frame {
    pub fn device_ptr(&self) -> u64 {
        self.ptr
    }
    fn allocate(w: u32, h: u32) -> Result<Self> {
        Self::allocate_with_scratch(w, h, overlay::SHADOW_BYTES)
    }
    fn allocate_with_scratch(w: u32, h: u32, scratch: usize) -> Result<Self> {
        ensure!(
            (1..=8192).contains(&w) && (1..=8192).contains(&h),
            "invalid scanout size"
        );
        let pitch = (w as usize * 4).next_multiple_of(256);
        let bytes = pitch * (h as usize).next_multiple_of(16) + scratch.max(overlay::SHADOW_BYTES);
        let d = driver()?;
        let _ctx = d.enter()?;
        let mut ptr = 0;
        // Dedicated 2 MiB granularity prevents IPC from sharing adjacent allocations.
        unsafe {
            ok(
                (d.alloc)(&mut ptr, bytes.next_multiple_of(2 << 20)),
                "allocate",
            )?;
        }
        let frame = Self {
            width: w as u16,
            height: h as u16,
            pitch,
            ptr,
        };
        unsafe {
            ok(
                (d.zero)(ptr, 0, bytes.next_multiple_of(2 << 20)),
                "clear padding",
            )?;
            ok((d.sync)(), "clear completion")?;
        }
        Ok(frame)
    }
    pub fn bgra(&self) -> Result<Vec<u8>> {
        let row = self.width as usize * 4;
        let mut data = vec![0; row * self.height as usize];
        let d = driver()?;
        let _ctx = d.enter()?;
        let copy = Copy2D {
            src: CopySide {
                kind: 2,
                device: self.ptr,
                pitch: self.pitch,
                ..Default::default()
            },
            dst: CopySide {
                kind: 1,
                host: data.as_mut_ptr() as usize,
                pitch: row,
                ..Default::default()
            },
            width: row,
            height: self.height as usize,
        };
        unsafe {
            ok((d.copy)(&copy), "bitmap readback")?;
        }
        for px in data.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok(data)
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        if let Ok(d) = driver() {
            if let Ok(_ctx) = d.enter() {
                unsafe {
                    (d.free)(self.ptr);
                }
            }
        }
    }
}
pub fn receive(mut stream: UnixStream) -> Result<(u32, Frame)> {
    let timeout = Some(Duration::from_secs(3));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let mut req = [0; 16];
    stream.read_exact(&mut req)?;
    ensure!(&req[..4] == b"GPU1", "invalid CUDA request");
    let word = |i| u32::from_le_bytes(req[i..i + 4].try_into().unwrap());
    let frame = Frame::allocate(word(8), word(12))?;
    let d = driver()?;
    let _ctx = d.enter()?;
    let mut handle = IpcHandle([0; 64]);
    unsafe {
        ok((d.export)(&mut handle, frame.ptr), "export")?;
    }
    // Keep the allocation alive until the local producer finishes or closes the socket.
    stream.set_read_timeout(None)?;
    stream.write_all(&handle.0)?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    ensure!(ack == [1], "producer abandoned frame");
    Ok((word(4), frame))
}
