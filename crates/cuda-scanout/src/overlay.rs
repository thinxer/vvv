//! Shared geometry and pixels for the GPU and bitmap display paths.
use super::{Frame, driver::*};
use anyhow::Result;
use std::{ffi::c_void, sync::OnceLock};

pub const BUTTON_X: usize = 12;
pub const BUTTON_Y: usize = 12;
const HEIGHT: usize = 32;
pub(crate) const SHADOW_BYTES: usize = 512 * HEIGHT * 4;

#[derive(Clone, Copy)]
enum Corner {
    LowerLeft,
    UpperLeft,
    UpperRight,
    LowerRight,
}

impl Corner {
    fn clockwise(self) -> Self {
        match self {
            Self::LowerLeft => Self::UpperLeft,
            Self::UpperLeft => Self::UpperRight,
            Self::UpperRight => Self::LowerRight,
            Self::LowerRight => Self::LowerLeft,
        }
    }
}

#[derive(Clone)]
pub struct Stamp {
    corner: Corner,
    width: usize,
    pixels: std::sync::Arc<[u8]>,
}

impl Stamp {
    fn label(text: &str, background: [u8; 4]) -> Self {
        let width = 20 + text.len() * 12;
        let mut pixels = background.repeat(width * HEIGHT);
        for (i, c) in text.bytes().enumerate() {
            for (col, bits) in glyph(c).iter().enumerate() {
                for row in 0..7 {
                    if bits & (1 << row) == 0 {
                        continue;
                    }
                    for dy in 0..2 {
                        let j = ((9 + row * 2 + dy) * width + 10 + i * 12 + col * 2) * 4;
                        pixels[j..j + 8].fill(255);
                    }
                }
            }
        }
        Self {
            width,
            pixels: pixels.into(),
            corner: Corner::LowerLeft,
        }
    }
    pub fn status(protocol: &str, count: usize) -> Self {
        Self::label(
            &format!(
                "{} IN CONTROL | {} VIEWERS",
                protocol.to_ascii_uppercase(),
                count
            ),
            [38, 46, 58, 184],
        )
    }
    pub fn button() -> &'static Self {
        static BUTTON: OnceLock<Stamp> = OnceLock::new();
        BUTTON.get_or_init(|| Self::label("TAKE CONTROL", [25, 94, 177, 255]))
    }
    pub fn hit(x: i32, y: i32) -> bool {
        (BUTTON_X as i32..(BUTTON_X + Self::button().width) as i32).contains(&x)
            && (BUTTON_Y as i32..(BUTTON_Y + HEIGHT) as i32).contains(&y)
    }
    pub fn avoids(&self, w: usize, h: usize, pointer: Option<(i32, i32)>) -> bool {
        let Some((px, py)) = pointer else { return true };
        let (x, y, rw, rh) = self.rect(w, h, true);
        let margin = 20;
        !(px >= x as i32 - margin
            && px < (x + rw) as i32 + margin
            && py >= y as i32 - margin
            && py < (y + rh) as i32 + margin)
    }
    pub fn next_clear_corner(&self, w: usize, h: usize, pointer: Option<(i32, i32)>) -> Self {
        let mut next = self.clone();
        for _ in 0..3 {
            next.corner = next.corner.clockwise();
            if next.avoids(w, h, pointer) {
                return next;
            }
        }
        self.clone()
    }
    fn rect(&self, w: usize, h: usize, status: bool) -> (usize, usize, usize, usize) {
        let left = BUTTON_X.min(w);
        let top = BUTTON_Y.min(h);
        let right = w.saturating_sub(self.width + 12);
        let bottom = h.saturating_sub(HEIGHT + 12);
        let (x, y) = if status {
            match self.corner {
                Corner::LowerLeft => (left, bottom),
                Corner::UpperLeft => (left, top),
                Corner::UpperRight => (right, top),
                Corner::LowerRight => (right, bottom),
            }
        } else {
            (left, top)
        };
        (x, y, self.width.min(w - x), HEIGHT.min(h - y))
    }
    pub fn draw_bitmap(&self, data: &mut Vec<u8>, w: usize, h: usize, status: bool) {
        let (x, y, rw, rh) = self.rect(w, h, status);
        for row in 0..rh {
            let start = ((y + row) * w + x) * 4;
            if status {
                data.extend_from_within(start..start + rw * 4);
            }
            for (dst, src) in data[start..start + rw * 4]
                .chunks_exact_mut(4)
                .zip(self.pixels[row * self.width * 4..].chunks_exact(4))
            {
                let a = u32::from(src[3]);
                for k in 0..3 {
                    dst[k] = ((u32::from(src[2 - k]) * a + u32::from(dst[k]) * (255 - a) + 127)
                        / 255) as u8;
                }
                dst[3] = 255;
            }
        }
    }
    pub fn restore_bitmap(&self, data: &mut Vec<u8>, w: usize, h: usize) {
        let (x, y, rw, rh) = self.rect(w, h, true);
        for row in 0..rh {
            let start = w * h * 4 + row * rw * 4;
            data.copy_within(start..start + rw * 4, ((y + row) * w + x) * 4);
        }
        data.truncate(w * h * 4);
    }
}

// Five columns per glyph, low bit at the top.
fn glyph(c: u8) -> [u8; 5] {
    match c {
        b'A' => [126, 17, 17, 17, 126],
        b'C' => [62, 65, 65, 65, 34],
        b'D' => [127, 65, 65, 34, 28],
        b'E' => [127, 73, 73, 73, 65],
        b'I' => [0, 65, 127, 65, 0],
        b'K' => [127, 8, 20, 34, 65],
        b'L' => [127, 64, 64, 64, 64],
        b'N' => [127, 2, 4, 8, 127],
        b'O' => [62, 65, 65, 65, 62],
        b'P' => [127, 9, 9, 9, 6],
        b'R' => [127, 9, 25, 41, 70],
        b'S' => [38, 73, 73, 73, 50],
        b'T' => [1, 1, 127, 1, 1],
        b'V' => [31, 32, 64, 32, 31],
        b'W' => [63, 64, 56, 64, 63],
        b'|' => [0, 0, 127, 0, 0],
        b'0' => [62, 81, 73, 69, 62],
        b'1' => [0, 66, 127, 64, 0],
        b'2' => [66, 97, 81, 73, 70],
        b'3' => [33, 65, 69, 75, 49],
        b'4' => [24, 20, 18, 127, 16],
        b'5' => [39, 69, 69, 69, 57],
        b'6' => [60, 74, 73, 73, 48],
        b'7' => [1, 113, 9, 5, 3],
        b'8' => [54, 73, 73, 73, 54],
        b'9' => [6, 73, 73, 41, 30],
        _ => [0; 5],
    }
}

fn viewport(sw: usize, sh: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let (fw, fh) = if sw * h > sh * w {
        (w, (sh * w / sw).max(1))
    } else {
        ((sw * h / sh).max(1), h)
    };
    ((w - fw) / 2, (h - fh) / 2, fw, fh)
}

/// Guest backing-pixel position, hotspot and straight-alpha ARGB bitmap.
#[derive(Clone)]
pub struct ObserverCursor {
    pub position: (i32, i32),
    pub hotspot: (u16, u16),
    pub width: u16,
    pub height: u16,
    pub pixels: std::sync::Arc<[u32]>,
    pub serial: u64,
}

impl ObserverCursor {
    pub fn key(&self) -> (u64, (i32, i32)) {
        (self.serial, self.position)
    }
    fn valid(&self) -> bool {
        self.pixels.len() == usize::from(self.width) * usize::from(self.height)
    }
}

pub fn fit_bitmap(src: &[u8], stride: usize, sw: usize, sh: usize, w: usize, h: usize) -> Vec<u8> {
    fit_bitmap_with_cursor(src, stride, sw, sh, w, h, None)
}
pub fn fit_bitmap_with_cursor(
    src: &[u8],
    stride: usize,
    sw: usize,
    sh: usize,
    w: usize,
    h: usize,
    cursor: Option<&ObserverCursor>,
) -> Vec<u8> {
    let cursor = cursor.filter(|c| c.valid());
    let mut dst = [24, 19, 16, 255].repeat(w * h);
    let (left, top, fw, fh) = viewport(sw, sh, w, h);
    for y in 0..fh {
        for x in 0..fw {
            let from = y * sh / fh * stride + x * sw / fw * 4;
            let to = ((top + y) * w + left + x) * 4;
            dst[to..to + 4].copy_from_slice(&src[from..from + 4]);
            if let Some(c) = cursor {
                let cx = (x * sw / fw) as i64 - i64::from(c.position.0) + i64::from(c.hotspot.0);
                let cy = (y * sh / fh) as i64 - i64::from(c.position.1) + i64::from(c.hotspot.1);
                if (0..i64::from(c.width)).contains(&cx) && (0..i64::from(c.height)).contains(&cy) {
                    let rgba = c.pixels[cy as usize * usize::from(c.width) + cx as usize];
                    let a = rgba >> 24;
                    for k in 0..3 {
                        let fg = (rgba >> (8 * k)) & 255;
                        dst[to + k] =
                            ((fg * a + u32::from(dst[to + k]) * (255 - a) + 127) / 255) as u8;
                    }
                }
            }
        }
    }
    Stamp::button().draw_bitmap(&mut dst, w, h, false);
    dst
}

fn device(ptr: u64, pitch: usize) -> CopySide {
    CopySide {
        kind: 2,
        device: ptr,
        pitch,
        ..Default::default()
    }
}
fn copy(src: CopySide, dst: CopySide, width: usize, height: usize) -> Result<()> {
    if width == 0 || height == 0 {
        return Ok(());
    }
    unsafe {
        ok(
            (driver()?.copy)(&Copy2D {
                src,
                dst,
                width,
                height,
            }),
            "presentation copy",
        )
    }
}
fn kernels() -> Result<&'static (usize, usize)> {
    static PAIR: OnceLock<Result<(usize, usize), String>> = OnceLock::new();
    PAIR.get_or_init(|| {
        (|| -> Result<_> {
            let d = driver()?;
            let mut module = 0;
            let (mut fit, mut blend) = (0, 0);
            unsafe {
                ok(
                    (d.load_module)(&mut module, concat!(include_str!("fit.ptx"), "\0").as_ptr()),
                    "load presentation kernels",
                )?;
                ok(
                    (d.function)(&mut fit, module, c"fit".as_ptr().cast()),
                    "find fit kernel",
                )?;
                ok(
                    (d.function)(&mut blend, module, c"blend_label".as_ptr().cast()),
                    "find blend kernel",
                )?;
            }
            Ok((fit, blend))
        })()
        .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!("{e}"))
}
impl Frame {
    fn shadow(&self) -> u64 {
        self.ptr + (self.pitch * (self.height as usize).next_multiple_of(16)) as u64
    }
    pub fn stamp(&mut self, image: &Stamp, status: bool) -> Result<()> {
        let d = driver()?;
        let _ctx = d.enter()?;
        let (x, y, w, h) = image.rect(self.width.into(), self.height.into(), status);
        let dest = self.ptr + (y * self.pitch + x * 4) as u64;
        if status {
            anyhow::ensure!(
                w * h * 4 <= SHADOW_BYTES,
                "status label exceeds saved region"
            );
            copy(
                device(dest, self.pitch),
                device(self.shadow(), w * 4),
                w * 4,
                h,
            )?;
        }
        copy(
            CopySide {
                kind: 1,
                host: image.pixels.as_ptr() as usize,
                pitch: image.width * 4,
                ..Default::default()
            },
            device(dest, self.pitch),
            w * 4,
            h,
        )?;
        if status && w > 0 && h > 0 {
            let mut dims = [(self.pitch / 4) as i32, w as i32, h as i32];
            let shadow = self.shadow();
            let mut args = [
                (&raw const dest).cast_mut().cast(),
                (&raw mut dims[0]).cast(),
                (&raw const shadow).cast_mut().cast(),
                (&raw mut dims[1]).cast(),
                (&raw mut dims[2]).cast(),
            ];
            unsafe {
                ok(
                    (d.launch)(
                        kernels()?.1,
                        (w as u32).div_ceil(16),
                        (h as u32).div_ceil(16),
                        1,
                        16,
                        16,
                        1,
                        0,
                        0,
                        args.as_mut_ptr(),
                        std::ptr::null_mut(),
                    ),
                    "blend status label",
                )?;
            }
        }
        unsafe { ok((d.sync)(), "finish presentation label") }
    }
    pub fn clean_copy(&self, status: Option<&Stamp>) -> Result<Frame> {
        let d = driver()?;
        let _ctx = d.enter()?;
        let dst = Frame::allocate(self.width.into(), self.height.into())?;
        copy(
            device(self.ptr, self.pitch),
            device(dst.ptr, dst.pitch),
            self.width as usize * 4,
            self.height.into(),
        )?;
        if let Some(image) = status {
            let (x, y, w, h) = image.rect(self.width.into(), self.height.into(), true);
            copy(
                device(self.shadow(), w * 4),
                device(dst.ptr + (y * dst.pitch + x * 4) as u64, dst.pitch),
                w * 4,
                h,
            )?;
        }
        unsafe { ok((d.sync)(), "finish presentation copy")? }
        Ok(dst)
    }
    pub fn fitted(&self, w: u16, h: u16) -> Result<Frame> {
        self.fitted_with_cursor(w, h, None)
    }
    pub fn fitted_with_cursor(
        &self,
        w: u16,
        h: u16,
        cursor: Option<&ObserverCursor>,
    ) -> Result<Frame> {
        let d = driver()?;
        let _ctx = d.enter()?;
        let cursor = cursor.filter(|c| c.valid() && !c.pixels.is_empty());
        let bytes = cursor.map_or(0, |c| c.pixels.len() * 4);
        let mut dst = Frame::allocate_with_scratch(w.into(), h.into(), bytes)?;
        let mut shape = 0u64;
        let mut rect = [0i32; 4];
        if let Some(c) = cursor {
            shape = dst.shadow();
            let pitch = usize::from(c.width) * 4;
            copy(
                CopySide {
                    kind: 1,
                    host: c.pixels.as_ptr() as usize,
                    pitch,
                    ..Default::default()
                },
                device(shape, pitch),
                pitch,
                c.height.into(),
            )?;
            rect = [
                c.position.0.saturating_sub(c.hotspot.0.into()),
                c.position.1.saturating_sub(c.hotspot.1.into()),
                c.width.into(),
                c.height.into(),
            ];
        }
        let kernel = kernels()?.0;
        let (x, y, fw, fh) = viewport(self.width.into(), self.height.into(), w.into(), h.into());
        let mut dims = [
            self.pitch as i32 / 4,
            self.width.into(),
            self.height.into(),
            dst.pitch as i32 / 4,
            w.into(),
            h.into(),
            x as i32,
            y as i32,
            fw as i32,
            fh as i32,
        ];
        let mut args: Vec<*mut c_void> = dims.iter_mut().map(|v| (v as *mut i32).cast()).collect();
        args.insert(0, (&raw const self.ptr).cast_mut().cast());
        args.insert(4, (&raw const dst.ptr).cast_mut().cast());
        args.push((&raw const shape).cast_mut().cast());
        args.extend(rect.iter_mut().map(|v| (v as *mut i32).cast::<c_void>()));
        unsafe {
            ok(
                (d.launch)(
                    kernel,
                    u32::from(w).div_ceil(16),
                    u32::from(h).div_ceil(16),
                    1,
                    16,
                    16,
                    1,
                    0,
                    0,
                    args.as_mut_ptr(),
                    std::ptr::null_mut(),
                ),
                "fit view",
            )?;
        }
        dst.stamp(Stamp::button(), false)?;
        Ok(dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observer_marker() -> ObserverCursor {
        ObserverCursor {
            position: (12, 24),
            hotspot: (2, 2),
            width: 4,
            height: 4,
            serial: 10,
            pixels: vec![0x80ff0000; 16].into(),
        }
    }

    #[test]
    fn observer_cursor_follows_fit_and_clips_at_guest_edge() {
        let src = [0, 0, 0, 255].repeat(640 * 360);
        let mut cursor = observer_marker();
        let fitted =
            |c: &ObserverCursor| fit_bitmap_with_cursor(&src, 2560, 640, 360, 320, 480, Some(c));
        let pixel = |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            frame[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3]
                .try_into()
                .unwrap()
        };
        let first = fitted(&cursor);
        assert_eq!(pixel(&first, 5, 161), [0, 0, 128]);
        cursor.position = (0, 0);
        let second = fitted(&cursor);
        assert_eq!(pixel(&second, 5, 161), [0, 0, 0]);
        assert_eq!(pixel(&second, 0, 150), [0, 0, 128]);
        assert_eq!(pixel(&second, 0, 149), [24, 19, 16]);
    }

    #[test]
    fn overlapping_corners_skip_past_the_pointer() {
        let status = Stamp::status("Apple", 2);
        let upper_left = status.next_clear_corner(320, 240, Some((40, 210)));
        assert_eq!(upper_left.rect(320, 240, true).1, 12);
        let pointer = Some((40, 16));
        assert!(!upper_left.avoids(320, 240, pointer));
        let next = upper_left.next_clear_corner(320, 240, pointer);
        assert!(matches!(next.corner, Corner::LowerRight));
        assert!(next.avoids(320, 240, pointer));
    }

    #[test]
    fn wide_guest_fits_portrait_and_button_is_in_view_coordinates() {
        let src = [30, 130, 230, 255].repeat(640 * 360);
        let dst = fit_bitmap(&src, 640 * 4, 640, 360, 320, 480);
        let pixel = |x: usize, y: usize| &dst[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4];
        assert_eq!(pixel(150, 149), [24, 19, 16, 255]);
        assert_eq!(pixel(150, 150), [30, 130, 230, 255]);
        assert_eq!(pixel(150, 329), [30, 130, 230, 255]);
        assert_eq!(pixel(150, 330), [24, 19, 16, 255]);
        assert_eq!(pixel(12, 12), [177, 94, 25, 255]);
        assert!(Stamp::hit(12, 12));
        assert!(!Stamp::hit(12, 44));
    }

    #[test]
    #[ignore = "requires CUDA"]
    fn device_badge_restore_and_fit_match_bitmap() -> Result<()> {
        let (w, h) = (640, 360);
        let mut frame = Frame::allocate(w, h)?;
        let original = frame.bgra()?;
        let status = Stamp::status("Apple", 2);
        frame.stamp(&status, true)?;
        let mut marked = original.clone();
        status.draw_bitmap(&mut marked, w as usize, h as usize, true);
        assert_eq!(frame.bgra()?, marked[..original.len()]);
        let fitted = frame
            .fitted_with_cursor(320, 480, Some(&observer_marker()))?
            .bgra()?;
        let at = |x: usize, y: usize| &fitted[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3];
        assert_eq!(at(12, 12), [177, 94, 25]);
        assert_eq!(at(5, 161), [0, 0, 128]);
        assert_eq!(at(160, 140), [24, 19, 16]);
        assert_eq!(at(160, 220), [0, 0, 0]);
        assert_eq!(frame.clean_copy(Some(&status))?.bgra()?, original);
        Ok(())
    }
}
