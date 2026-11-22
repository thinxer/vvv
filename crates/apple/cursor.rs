use super::Screen;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use wayshare::source::{CursorImage, CursorSource};

pub struct GuestCursor {
    screen: Arc<Screen>,
    display: Arc<super::resize::GuestDisplay>,
    last: Option<(u64, u16)>,
    current: Option<Arc<CursorImage>>,
}

impl GuestCursor {
    pub fn new(screen: Arc<Screen>, display: Arc<super::resize::GuestDisplay>) -> Self {
        Self {
            screen,
            display,
            last: None,
            current: None,
        }
    }
}

impl CursorSource for GuestCursor {
    fn refresh(&mut self) {
        #[cfg(feature = "combined")]
        if self
            .screen
            .views
            .get()
            .is_some_and(|v| v.borrow().apple_observing())
        {
            if self.last.take().is_some() || self.current.is_none() {
                self.current = wayshare::source::TestCursor::default()
                    .image()
                    .cloned()
                    .map(Arc::new);
            }
            return;
        }
        let Some(scale) = self.display.cursor_scale_eighths() else {
            return;
        };
        let Some(source) = self.screen.cursor.lock().unwrap().clone() else {
            return;
        };
        let key = (source.serial, scale);
        if self.last != Some(key) {
            self.current = Some(if scale == 8 {
                source
            } else {
                Arc::new(logical_cursor(&source, scale))
            });
            self.last = Some(key);
        }
    }
    fn image(&self) -> Option<&CursorImage> {
        self.current.as_deref()
    }
    fn position(&self) -> Option<(u16, u16)> {
        None
    }
}

// Apple draws a local cursor. Its bitmap and hotspot use logical units even
// when the desktop video uses backing pixels. KDE's cursor plane uses pixels.
fn logical_cursor(source: &CursorImage, scale: u16) -> CursorImage {
    let logical = |v| (u32::from(v) * 8 + u32::from(scale / 2)) / u32::from(scale);
    let width = logical(source.width).max(1) as u16;
    let height = logical(source.height).max(1) as u16;
    let hotspot = (
        logical(source.hotspot.0).min(u32::from(width - 1)) as u16,
        logical(source.hotspot.1).min(u32::from(height - 1)) as u16,
    );
    let mut pixels = Vec::with_capacity(usize::from(width) * usize::from(height));
    // Average the covered area in premultiplied alpha so thin shapes and
    // translucent cursor outlines survive reduction.
    let coverage = |index: u16, src: u16, dst: u16| {
        let dst = u32::from(dst);
        let start = u32::from(index) * u32::from(src);
        let end = start + u32::from(src);
        (start / dst..end.div_ceil(dst)).map(move |i| {
            (
                i as usize,
                u64::from(end.min((i + 1) * dst) - start.max(i * dst)),
            )
        })
    };
    let area = u64::from(source.width) * u64::from(source.height);
    for y in 0..height {
        for x in 0..width {
            let mut alpha = 0;
            let mut colors = [0u64; 3];
            for (sy, wy) in coverage(y, source.height, height) {
                for (sx, wx) in coverage(x, source.width, width) {
                    let pixel = source.pixels[sy * usize::from(source.width) + sx];
                    let weight = u64::from(pixel >> 24) * wx * wy;
                    alpha += weight;
                    for (i, sum) in colors.iter_mut().enumerate() {
                        *sum += u64::from((pixel >> (i * 8)) & 255) * weight;
                    }
                }
            }
            let mut pixel = (((alpha + area / 2) / area) as u32) << 24;
            if pixel != 0 {
                for (i, sum) in colors.into_iter().enumerate() {
                    pixel |= (((sum + alpha / 2) / alpha) as u32) << (i * 8);
                }
            }
            pixels.push(pixel);
        }
    }
    let mut hash = DefaultHasher::new();
    (source.serial, scale).hash(&mut hash);
    CursorImage {
        serial: hash.finish(),
        width,
        height,
        hotspot,
        pixels,
    }
}

pub fn image(
    w: u32,
    h: u32,
    bytes: &[u8],
    origin: (i32, i32),
    pointer: Option<(i32, i32)>,
    hotspots: &mut HashMap<u64, (u16, u16)>,
) -> Option<CursorImage> {
    let (w, h) = (u16::try_from(w).ok()?, u16::try_from(h).ok()?);
    if w == 0 || h == 0 || bytes.len() != usize::from(w).checked_mul(h.into())?.checked_mul(4)? {
        return None;
    }
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    for (i, pixel) in bytes.chunks_exact(4).enumerate() {
        if pixel[3] != 0 {
            let (x, y) = ((i % usize::from(w)) as u16, (i / usize::from(w)) as u16);
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
        }
    }
    // A transparent 1x1 image also replaces the previous visible cursor on the Mac.
    if left > right {
        return Some(CursorImage {
            serial: 0,
            width: 1,
            height: 1,
            hotspot: (0, 0),
            pixels: vec![0],
        });
    }
    // crosvm gives the cursor plane origin. KDE can supply logical coordinates on HiDPI.
    let observed = pointer.and_then(|(x, y)| {
        [1, 2].into_iter().find_map(|scale| {
            let hx = i64::from(x) - scale * i64::from(origin.0);
            let hy = i64::from(y) - scale * i64::from(origin.1);
            ((i64::from(left)..=right.into()).contains(&hx)
                && (i64::from(top)..=bottom.into()).contains(&hy))
            .then(|| (hx as u16 - left, hy as u16 - top))
        })
    });
    // Input and scanout arrive asynchronously. Keep the first observed hotspot for each
    // plane image so a delayed motion event cannot shift an already displayed pointer.
    let mut plane = DefaultHasher::new();
    (w, h, bytes).hash(&mut plane);
    let key = plane.finish();
    let hotspot = hotspots
        .get(&key)
        .copied()
        .or_else(|| {
            let point = observed?;
            if hotspots.len() >= 256 {
                hotspots.clear();
            }
            hotspots.insert(key, point);
            Some(point)
        })
        .unwrap_or_else(|| {
            if left >= 16 && top >= 16 {
                ((right - left) / 2, (bottom - top) / 2)
            } else {
                (0, 0)
            }
        });
    let width = right - left + 1;
    let height = bottom - top + 1;
    let mut pixels = Vec::with_capacity(usize::from(width) * usize::from(height));
    for y in top..=bottom {
        let start = (usize::from(y) * usize::from(w) + usize::from(left)) * 4;
        for p in bytes[start..start + usize::from(width) * 4].chunks_exact(4) {
            let a = u32::from(p[3]);
            let straight = |c: u8| {
                (u32::from(c) * 255 + a / 2)
                    .checked_div(a)
                    .unwrap_or(0)
                    .min(255)
            };
            pixels.push(a << 24 | straight(p[2]) << 16 | straight(p[1]) << 8 | straight(p[0]));
        }
    }
    let mut hash = DefaultHasher::new();
    (width, height, hotspot, &pixels).hash(&mut hash);
    Some(CursorImage {
        serial: hash.finish(),
        width,
        height,
        hotspot,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retina_cursor_scales_shape_hotspot_and_alpha_together() {
        let source = CursorImage {
            serial: 17,
            width: 4,
            height: 2,
            hotspot: (3, 1),
            pixels: vec![
                0xffff0000, 0, 0xff00ff00, 0xff00ff00, 0xffff0000, 0, 0xff00ff00, 0xff00ff00,
            ],
        };
        let cursor = logical_cursor(&source, 16);
        assert_eq!(
            (cursor.width, cursor.height, cursor.hotspot),
            (2, 1, (1, 0))
        );
        assert_eq!(cursor.pixels, [0x80ff0000, 0xff00ff00]);
        assert_ne!(source.serial, cursor.serial);
    }
}
