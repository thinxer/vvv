// Modified for vvv from WayShare. SPDX-License-Identifier: Apache-2.0
//! HEVC encoding of captured frames as four independent horizontal-strip streams (HP mode tiles).

pub const TILE_COUNT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Frame of `width x height` split into `TILE_COUNT` strips of `tile_height` coded rows each.
/// The last strip is padded below the frame when `height` doesn't divide evenly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileLayout {
    pub width: u32,
    pub height: u32,
    pub tile_height: u32,
}

impl TileLayout {
    pub fn new(width: u32, height: u32) -> Self {
        let tile_height = height.div_ceil(TILE_COUNT as u32).next_multiple_of(16);
        Self {
            width,
            height,
            tile_height,
        }
    }

    pub fn tile_y(&self, tile: usize) -> u32 {
        tile as u32 * self.tile_height
    }

    /// Rows of real frame content in `tile` (less than `tile_height` for a padded last strip).
    pub fn tile_rows(&self, tile: usize) -> u32 {
        self.height
            .saturating_sub(self.tile_y(tile))
            .min(self.tile_height)
    }

    /// Bitmask of tiles intersecting any of `rects`.
    pub fn tiles_touched(&self, rects: &[Rect]) -> u8 {
        let mut mask = 0;
        for r in rects.iter().filter(|r| r.width > 0 && r.height > 0) {
            let first = (r.y / self.tile_height) as usize;
            let last = ((r.y + r.height - 1) / self.tile_height) as usize;
            for t in first..=last.min(TILE_COUNT - 1) {
                mask |= 1 << t;
            }
        }
        mask
    }
}

#[derive(Clone, Debug)]
pub struct EncodedTile {
    pub index: u8,
    pub keyframe: bool,
    /// Annex B access unit (start-code prefixed NALs, SEI removed). IDRs carry VPS/SPS/PPS.
    pub data: Vec<u8>,
}

/// Where an `encode` call spent its time.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeTimings {
    /// Change detection.
    pub diff: std::time::Duration,
    /// Colour conversion / crop into encoder input.
    pub convert: std::time::Duration,
    /// Submitting pictures until the last coded picture is read back.
    pub encode: std::time::Duration,
}

/// Position of an incrementally delivered strip within one captured frame.
#[derive(Clone, Copy, Debug)]
pub struct StripPart {
    pub first: bool,
    pub last: bool,
    pub refresh: bool,
}

/// One captured frame's output. Tiles whose content didn't change are absent from `tiles`
/// unless a keyframe was requested.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub strip: Option<StripPart>,
    pub seq: u64,
    pub changed_tiles: [bool; TILE_COUNT],
    pub tiles: Vec<EncodedTile>,
    pub timings: EncodeTimings,
    /// The source frame's compositor presentation time (CLOCK_MONOTONIC).
    pub presented: Option<std::time::Duration>,
    /// Picture size (unpadded) the tiles were coded at.
    pub frame_size: (u16, u16),
}

pub trait VideoEncoder {
    /// Backend-specific input (e.g. a dmabuf capture frame for VAAPI).
    type Frame;

    fn layout(&self) -> TileLayout;
    /// Force an IDR on the next encode for one tile, or all tiles with `None` (FIR).
    fn request_keyframe(&mut self, tile: Option<usize>);
    /// Target total bitrate (RTCP rate control). Takes effect within a frame or two.
    fn set_bitrate(&mut self, bps: u64);
    /// Starts encoding `frame`. The frame's buffer is no longer needed when this returns.
    /// May block while the maximum number of frames is in flight.
    fn submit(&mut self, frame: &Self::Frame) -> anyhow::Result<()>;
    /// The next finished frame, in submission order. `timeout` `None` blocks; `Some(ZERO)` polls.
    fn receive(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> anyhow::Result<Option<EncodedFrame>>;
    /// Frames submitted and not yet received.
    fn pending(&self) -> usize;
    /// Whether `submit` would start right away instead of waiting for the encoder. Callers that
    /// care about latency hold only the newest frame until this is true (skipped frames lose
    /// nothing: change detection compares against the last submitted frame).
    fn can_submit(&mut self) -> bool;
    /// Whether to submit again even without a new capture (resubmit the last frame): a
    /// keyframe request is pending, or the stream is due a heartbeat picture.
    fn wants_frame(&self) -> bool;
    /// Streams coded frames to `tx` as they finish (possibly from another thread), instead of
    /// `receive`. Frames with nothing to code are not sent.
    fn set_output(&mut self, tx: std::sync::mpsc::Sender<anyhow::Result<EncodedFrame>>);

    /// Opt into ordered strip delivery when supported. Other encoders deliver complete frames.
    fn set_stream_output(&mut self, tx: std::sync::mpsc::Sender<anyhow::Result<EncodedFrame>>) {
        self.set_output(tx);
    }

    /// Synchronous `submit` + `receive`, for callers that keep nothing in flight.
    fn encode(&mut self, frame: &Self::Frame) -> anyhow::Result<EncodedFrame> {
        debug_assert_eq!(self.pending(), 0);
        self.submit(frame)?;
        loop {
            if let Some(f) = self.receive(None)? {
                return Ok(f);
            }
        }
    }
}

pub type Encoder<B> = Box<dyn VideoEncoder<Frame = crate::capture::Frame<B>> + Send>;

/// Opens one encoder for a negotiated HP frame layout. Implementations may emit strips
/// incrementally through VideoEncoder::set_stream_output.
pub trait EncoderFactory<B>: Send + Sync {
    fn open(&self, layout: TileLayout) -> anyhow::Result<Encoder<B>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_3456x2160() {
        let l = TileLayout::new(3456, 2160);
        assert_eq!(l.tile_height, 544);
        assert_eq!(l.tile_rows(3), 528);
        assert_eq!(l.tile_rows(0), 544);
    }

    #[test]
    fn layout_1080p() {
        let l = TileLayout::new(1920, 1080);
        assert_eq!(l.tile_height, 272);
        assert_eq!(l.tile_rows(3), 264);
    }

    #[test]
    fn damage_mask() {
        let l = TileLayout::new(3456, 2160);
        let r = |y, h| Rect {
            x: 0,
            y,
            width: 10,
            height: h,
        };
        assert_eq!(l.tiles_touched(&[]), 0);
        assert_eq!(l.tiles_touched(&[r(0, 1)]), 0b0001);
        assert_eq!(l.tiles_touched(&[r(543, 2)]), 0b0011);
        assert_eq!(l.tiles_touched(&[r(2000, 160)]), 0b1000);
        assert_eq!(l.tiles_touched(&[r(0, 2160)]), 0b1111);
    }
}
