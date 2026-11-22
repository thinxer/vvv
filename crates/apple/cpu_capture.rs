//! Owned crosvm scanouts used by the external-only Apple build.
use crate::encode::Rect;
use anyhow::{Result, ensure};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub struct DmabufBuffer {
    pub width: u32,
    pub height: u32,
    storage: Storage,
}
enum Storage {
    Cpu(Vec<u8>),
    Gpu(Arc<cuda_scanout::Frame>),
}
pub struct ReadGuard<'a> {
    data: &'a [u8],
    pub stride: usize,
}
impl ReadGuard<'_> {
    pub fn bytes(&self) -> &[u8] {
        self.data
    }
}
impl DmabufBuffer {
    pub fn from_bgrx(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        ensure!(
            width > 0
                && height > 0
                && (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|n| n.checked_mul(4))
                    == Some(pixels.len()),
            "invalid BGRX frame"
        );
        Ok(Self {
            width,
            height,
            storage: Storage::Cpu(pixels),
        })
    }
    pub fn from_cuda(frame: cuda_scanout::Frame) -> Self {
        Self {
            width: frame.width.into(),
            height: frame.height.into(),
            storage: Storage::Gpu(Arc::new(frame)),
        }
    }
    pub fn stamp_status(&mut self, stamp: &cuda_scanout::overlay::Stamp) -> Result<()> {
        match &mut self.storage {
            Storage::Cpu(data) => {
                stamp.draw_bitmap(data, self.width as usize, self.height as usize, true)
            }
            Storage::Gpu(f) => Arc::get_mut(f)
                .ok_or_else(|| anyhow::anyhow!("scanout already published"))?
                .stamp(stamp, true)?,
        }
        Ok(())
    }
    pub fn clean_copy(&self, status: Option<&cuda_scanout::overlay::Stamp>) -> Result<Self> {
        match &self.storage {
            Storage::Gpu(f) => Ok(Self::from_cuda(f.clean_copy(status)?)),
            Storage::Cpu(data) => {
                let mut copy = data.clone();
                if let Some(stamp) = status {
                    stamp.restore_bitmap(&mut copy, self.width as usize, self.height as usize);
                }
                Self::from_bgrx(self.width, self.height, copy)
            }
        }
    }
    pub fn fitted(
        &self,
        w: u16,
        h: u16,
        cursor: Option<&cuda_scanout::overlay::ObserverCursor>,
    ) -> Result<Self> {
        match &self.storage {
            Storage::Gpu(f) => Ok(Self::from_cuda(f.fitted_with_cursor(w, h, cursor)?)),
            Storage::Cpu(data) => Self::from_bgrx(
                w.into(),
                h.into(),
                cuda_scanout::overlay::fit_bitmap_with_cursor(
                    data,
                    self.width as usize * 4,
                    self.width as usize,
                    self.height as usize,
                    w.into(),
                    h.into(),
                    cursor,
                ),
            ),
        }
    }
    pub fn cuda(&self) -> Option<&Arc<cuda_scanout::Frame>> {
        match &self.storage {
            Storage::Gpu(f) => Some(f),
            Storage::Cpu(_) => None,
        }
    }
    pub fn with_read<T>(&self, read: impl FnOnce(&ReadGuard<'_>) -> T) -> Result<T> {
        let readback;
        let pixels = match &self.storage {
            Storage::Cpu(data) => &data[..self.width as usize * self.height as usize * 4],
            Storage::Gpu(frame) => {
                readback = frame.bgra()?;
                &readback
            }
        };
        Ok(read(&ReadGuard {
            data: pixels,
            stride: self.width as usize * 4,
        }))
    }
}
#[derive(PartialEq, Eq)]
pub enum Memory {
    System,
    Gpu,
}
pub struct Frame {
    pub seq: u64,
    pub buffer: Arc<DmabufBuffer>,
    pub damage: Vec<Rect>,
    pub presented: Option<Duration>,
    pub requested_at: Instant,
    pub ready_at: Instant,
}

pub fn monotonic_now() -> Duration {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}
