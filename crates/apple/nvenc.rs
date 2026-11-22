use super::convert::{Coeffs, Planes, bgrx_to_yuv};
use super::plan::{PicKind, TilePic};
use super::tiles::{Backend, Tiles};
use super::{
    Chroma, EncodedFrame, EncodedTile, EncoderConfig, Primaries, TILE_COUNT, TileLayout, Transfer,
    VideoEncoder, nal,
};
use crate::capture::Frame;
use anyhow::{Result, ensure};
use std::collections::VecDeque;
mod nvidia;
use nvidia::Nvidia;
use std::time::{Duration, Instant};

struct Session {
    handle: Nvidia,
    layout: TileLayout,
    full_range: bool,
    coeffs: Coeffs,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    done: VecDeque<EncodedFrame>,
    stream: Option<std::sync::mpsc::Sender<Result<EncodedFrame>>>,
}

// The session owns the CUDA primary context reference. NVENC calls are serialized by &mut self.
unsafe impl Send for NvencEncoder {}

pub struct NvencEncoder {
    tiles: Tiles<Session>,
}

impl Session {
    fn new(layout: TileLayout, cfg: &EncoderConfig) -> Result<Self> {
        ensure!(
            cfg.chroma == Chroma::Yuv420
                && layout.width % 2 == 0
                && layout.height % 2 == 0
                && layout.tile_rows(TILE_COUNT - 1) >= 2,
            "NVENC requires even HEVC 4:2:0 tiles"
        );
        let handle = Nvidia::open(layout, cfg)?;
        let pixels = layout.width as usize * layout.height as usize;
        Ok(Self {
            handle,
            layout,
            full_range: cfg.color.full_range,
            coeffs: Coeffs::bt709(cfg.color.full_range),
            y: vec![0; pixels],
            u: vec![0; pixels / 4],
            v: vec![0; pixels / 4],
            done: VecDeque::new(),
            stream: None,
        })
    }
}

pub fn check() -> Result<()> {
    Session::new(TileLayout::new(1280, 720), &EncoderConfig::default()).map(drop)
}

impl NvencEncoder {
    pub fn new(w: u32, h: u32, cfg: &EncoderConfig) -> Result<Self> {
        let layout = TileLayout::new(w, h);
        let session = Session::new(layout, cfg)?;
        Ok(Self {
            tiles: Tiles::new(session, layout, cfg, false, Duration::from_secs(1))?,
        })
    }
}

impl VideoEncoder for NvencEncoder {
    type Frame = Frame;

    fn layout(&self) -> TileLayout {
        self.tiles.layout()
    }

    fn request_keyframe(&mut self, tile: Option<usize>) {
        self.tiles.request_keyframe(tile);
    }

    fn set_bitrate(&mut self, bps: u64) {
        self.tiles.set_bitrate(bps);
    }

    fn submit(&mut self, frame: &Frame) -> Result<()> {
        self.tiles.submit(frame)
    }

    fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<EncodedFrame>> {
        self.tiles.receive(timeout)
    }

    fn pending(&self) -> usize {
        self.tiles.pending()
    }

    fn can_submit(&mut self) -> bool {
        self.tiles.can_submit()
    }

    fn wants_frame(&self) -> bool {
        self.tiles.wants_frame()
    }

    fn set_stream_output(&mut self, tx: std::sync::mpsc::Sender<Result<EncodedFrame>>) {
        self.tiles.backend.stream = Some(tx.clone());
        self.tiles.set_output(tx);
    }

    fn set_output(&mut self, tx: std::sync::mpsc::Sender<Result<EncodedFrame>>) {
        self.tiles.set_output(tx);
    }
}

impl Backend for Session {
    fn submit(&mut self, frame: &Frame, pics: &[TilePic], mut meta: EncodedFrame) -> Result<()> {
        let start = Instant::now();
        let width = self.layout.width as usize;
        let gpu = frame.buffer.cuda();
        if gpu.is_none() {
            frame.buffer.with_read(|pixels| {
                bgrx_to_yuv(
                    pixels.bytes(),
                    pixels.stride,
                    width,
                    self.layout.height as usize,
                    Chroma::Yuv420,
                    &self.coeffs,
                    Planes {
                        y: &mut self.y,
                        u: &mut self.u,
                        v: &mut self.v,
                        y_stride: width,
                        c_stride: width / 2,
                    },
                )
            })?;
        }
        meta.timings.convert = start.elapsed();
        let start = Instant::now();
        // Queue only this frame. NVENC sees one HEVC sequence in planner order,
        // and each strip owns its input and output until its bitstream is read.
        if let Some(frame) = gpu {
            for pic in pics {
                self.handle.submit_tile(
                    frame,
                    pic,
                    self.layout.tile_y(pic.tile),
                    self.full_range,
                )?;
            }
        }
        // A selective intra refresh can first reveal an IRAP in a later strip.
        // Keep that uncommon batch together so its frame-wide refresh flag is exact.
        let stream = self.stream.as_ref().filter(|_| {
            gpu.is_some()
                && (pics.first().is_some_and(|p| p.kind == PicKind::Idr)
                    || pics.iter().all(|p| p.kind == PicKind::Inter))
        });
        let mut refresh = false;
        for (i, pic) in pics.iter().enumerate() {
            let data = if gpu.is_some() {
                self.handle.finish(pic.tile)?
            } else {
                let offset = self.layout.tile_y(pic.tile) as usize * width;
                self.handle.encode(
                    &self.y[offset..],
                    &self.u[offset / 4..],
                    &self.v[offset / 4..],
                    self.layout.tile_rows(pic.tile) as usize,
                    pic.kind,
                )?
            };
            let tile = EncodedTile {
                index: pic.tile as u8,
                keyframe: pic.kind != PicKind::Inter,
                data,
            };
            if let Some(tx) = stream {
                if i == 0 {
                    refresh = nal::split_annexb(&tile.data)
                        .any(|n| (16..=23).contains(&nal::nal_type(n)));
                }
                let mut part = meta.clone();
                part.strip = Some(super::StripPart {
                    first: i == 0,
                    last: i + 1 == pics.len(),
                    refresh,
                });
                part.tiles.push(tile);
                part.timings.encode = start.elapsed();
                tx.send(Ok(part))
                    .map_err(|_| anyhow::anyhow!("HP sender stopped"))?;
            } else {
                meta.tiles.push(tile);
            }
        }
        meta.timings.encode = start.elapsed();
        if stream.is_none() {
            self.done.push_back(meta);
        }
        Ok(())
    }
    fn receive(&mut self, _: Option<Duration>) -> Result<Option<EncodedFrame>> {
        Ok(self.done.pop_front())
    }
    fn set_bitrate(&mut self, bps: u64) -> bool {
        if let Err(e) = self.handle.rate(bps.min(u32::MAX as u64) as u32) {
            tracing::warn!(bps, "NVENC bitrate change failed: {e:#}");
        }
        false
    }
}
