//! Reusable device-local NV12 surface for one synchronous HEVC tile encoder.
use crate::{Frame, driver::*};
use anyhow::{Result, ensure};
use std::{ffi::c_void, ptr};

pub struct Nv12Tile {
    ptr: u64,
    module: usize,
    kernel: usize,
    width: u32,
    height: u32,
    pitch: u32,
}

impl Nv12Tile {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        ensure!(
            width > 0 && width <= 8192 && height > 0 && height <= 8192 && (width | height) % 2 == 0,
            "invalid tile size"
        );
        let d = driver()?;
        let _ctx = d.enter()?;
        let mut tile = Self {
            ptr: 0,
            module: 0,
            kernel: 0,
            width,
            height,
            pitch: width.next_multiple_of(256),
        };
        let ptx = concat!(include_str!("nv12.ptx"), "\0");
        unsafe {
            ok(
                (d.load_module)(&mut tile.module, ptx.as_ptr()),
                "load NV12 kernel",
            )?;
            ok(
                (d.function)(&mut tile.kernel, tile.module, c"to_nv12".as_ptr().cast()),
                "find NV12 kernel",
            )?;
            ok(
                (d.alloc)(&mut tile.ptr, tile.pitch as usize * height as usize * 3 / 2),
                "allocate NV12 tile",
            )?;
        }
        Ok(tile)
    }
    pub fn device_ptr(&self) -> u64 {
        self.ptr
    }
    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    pub fn convert(&mut self, frame: &Frame, top: u32, full_range: bool) -> Result<()> {
        ensure!(
            self.width == u32::from(frame.width) && top < u32::from(frame.height) && top % 2 == 0,
            "invalid tile origin"
        );
        let rows = (u32::from(frame.height) - top).min(self.height);
        ensure!(rows % 2 == 0, "invalid NV12 tile height");
        let src = frame.ptr + u64::from(top) * frame.pitch as u64;
        let stride = frame.pitch as u32;
        let full = u32::from(full_range);
        let arg = |p: *const c_void| p.cast_mut();
        let mut args = [
            arg((&raw const src).cast()),
            arg((&raw const stride).cast()),
            arg((&raw const self.ptr).cast()),
            arg((&raw const self.pitch).cast()),
            arg((&raw const self.width).cast()),
            arg((&raw const self.height).cast()),
            arg((&raw const rows).cast()),
            arg((&raw const full).cast()),
        ];
        let d = driver()?;
        let _ctx = d.enter()?;
        unsafe {
            ok(
                (d.launch)(
                    self.kernel,
                    self.width.div_ceil(32),
                    self.height.div_ceil(16),
                    1,
                    16,
                    8,
                    1,
                    0,
                    0,
                    args.as_mut_ptr(),
                    ptr::null_mut(),
                ),
                "convert RGBA to NV12",
            )?;
            ok((d.sync)(), "finish NV12 tile")
        }
    }
}
impl Drop for Nv12Tile {
    fn drop(&mut self) {
        if let Ok(d) = driver()
            && let Ok(_ctx) = d.enter()
        {
            unsafe {
                if self.ptr != 0 {
                    (d.free)(self.ptr);
                }
                if self.module != 0 {
                    (d.unload_module)(self.module);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an NVIDIA GPU and driver"]
    fn offset_and_padded_nv12_tile() -> Result<()> {
        let d = driver()?;
        let _ctx = d.enter()?;
        let frame = Frame::allocate(2, 4)?;
        let mut tile = Nv12Tile::new(2, 4)?;
        // Two rows of green above the tile, then a red row and a blue row.
        let rgba = [
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [255, 0, 0, 255],
            [0, 0, 255, 255],
        ]
        .into_iter()
        .flat_map(|pixel: [u8; 4]| pixel.repeat(2))
        .collect::<Vec<_>>();
        unsafe {
            ok(
                (d.copy)(&Copy2D {
                    src: CopySide {
                        kind: 1,
                        host: rgba.as_ptr() as usize,
                        pitch: 8,
                        ..Default::default()
                    },
                    dst: CopySide {
                        kind: 2,
                        device: frame.ptr,
                        pitch: frame.pitch,
                        ..Default::default()
                    },
                    width: 8,
                    height: 4,
                }),
                "upload color fixture",
            )?;
        }
        for (full, red, blue, u, v) in [(true, 54, 18, 177, 186), (false, 63, 32, 171, 179)] {
            tile.convert(&frame, 2, full)?;
            let mut actual = [0u8; 12];
            unsafe {
                ok(
                    (d.copy)(&Copy2D {
                        src: CopySide {
                            kind: 2,
                            device: tile.ptr,
                            pitch: tile.pitch as usize,
                            ..Default::default()
                        },
                        dst: CopySide {
                            kind: 1,
                            host: actual.as_mut_ptr() as usize,
                            pitch: 2,
                            ..Default::default()
                        },
                        width: 2,
                        height: 6,
                    }),
                    "read test tile",
                )?;
            }
            assert_eq!(
                actual,
                [red, red, blue, blue, blue, blue, blue, blue, u, v, u, v]
            );
        }
        Ok(())
    }
}
