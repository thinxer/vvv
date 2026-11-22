//! One EGFX surface and flow-control window per RDP connection.
use crate::h264::{Encoder, H264};
use anyhow::{Context, Result};
use ironrdp_core::{Encode, EncodeResult, WriteCursor};
use ironrdp_egfx::pdu::{CapabilitiesAdvertisePdu, CapabilitySet};
use ironrdp_egfx::server::MixedTilePayload;
use ironrdp_egfx::server::{GraphicsPipelineHandler, GraphicsPipelineServer};
use ironrdp_graphics::clearcodec::ClearCodecEncoder;
use ironrdp_graphics::diff::{Rect, find_different_rects_sub};
use ironrdp_pdu::geometry::ExclusiveRectangle;
use ironrdp_server::*;
use ironrdp_svc::{ChannelFlags, SvcEncode, SvcMessage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// The server's event queue is unbounded. Keep at most one frame queued behind
// the writer even when a client suspends frame acknowledgements.
struct LastFragment {
    bytes: Vec<u8>,
    queued: Arc<AtomicBool>,
    wake: Arc<Notify>,
}
impl Encode for LastFragment {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        self.bytes.encode(dst)
    }
    fn name(&self) -> &'static str {
        "EGFX frame tail"
    }
    fn size(&self) -> usize {
        self.bytes.len()
    }
}
impl SvcEncode for LastFragment {}
impl Drop for LastFragment {
    fn drop(&mut self) {
        self.queued.store(false, Ordering::Release);
        self.wake.notify_one();
    }
}
use tokio::sync::{Notify, mpsc};

struct Handler(Arc<Notify>, Vec<CapabilitySet>);
impl GraphicsPipelineHandler for Handler {
    fn capabilities_advertise(&mut self, caps: &CapabilitiesAdvertisePdu) {
        // Preserve client restrictions, in particular the negative AVC_DISABLED
        // flag which would disappear if intersected with a default zero bit.
        self.1 = caps
            .0
            .iter()
            .filter_map(|raw| raw.parsed().ok().flatten())
            .collect();
    }
    fn preferred_capabilities(&self) -> Vec<CapabilitySet> {
        self.1.clone()
    }
    fn on_ready(&mut self, caps: &CapabilitySet) {
        eprintln!("IronRDP EGFX negotiated {caps:?}");
        self.0.notify_one();
    }
    fn on_close(&mut self) {
        self.0.notify_one();
    }
    fn on_frame_ack(&mut self, _: u32, _: u32, _: u32) {
        self.0.notify_one();
    }
    fn max_frames_in_flight(&self) -> u32 {
        1
    }
}

#[derive(Clone)]
pub struct Graphics {
    pub wake: Arc<Notify>,
    pipeline: GfxServerHandle,
    events: Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>,
    started: Instant,
    queued: Arc<AtomicBool>,
    codec: Arc<Mutex<ClearCodecEncoder>>,
    encoder: Encoder,
    video: Arc<Mutex<Option<H264>>>,
}

pub enum Submission {
    Bitmap,
    Sent,
    Busy,
}

// Encode only pixels which differ from the last submitted frame. Keep all
// rectangles from a capture inside a single EGFX StartFrame/EndFrame pair.
fn encode_damage(
    frame: &BitmapUpdate,
    previous: Option<&BitmapUpdate>,
    codec: &mut ClearCodecEncoder,
) -> Vec<MixedTilePayload> {
    let (w, h) = (
        usize::from(frame.width.get()),
        usize::from(frame.height.get()),
    );
    let stride = frame.stride.get();
    let rects = match previous.filter(|p| p.width == frame.width && p.height == frame.height) {
        Some(p) => find_different_rects_sub::<4>(
            &p.data,
            p.stride.get(),
            w,
            h,
            &frame.data,
            stride,
            w,
            h,
            0,
            0,
        ),
        None => vec![Rect::new(0, 0, w, h)],
    };
    let mut tiles = Vec::new();
    let mut packed = Vec::new();
    for rect in rects {
        // Bound scratch memory and the work per ClearCodec bitmap on a full repaint.
        for y in (rect.y..rect.y + rect.height).step_by(256) {
            let bottom = (y + 256).min(rect.y + rect.height);
            for x in (rect.x..rect.x + rect.width).step_by(256) {
                let right = (x + 256).min(rect.x + rect.width);
                packed.clear();
                for row in y..bottom {
                    packed.extend_from_slice(
                        &frame.data[row * stride + x * 4..row * stride + right * 4],
                    );
                }
                tiles.push(MixedTilePayload::ClearCodec {
                    destination: ExclusiveRectangle {
                        left: x as u16,
                        top: y as u16,
                        right: right as u16,
                        bottom: bottom as u16,
                    },
                    bitmap_data: codec.encode(&packed, (right - x) as u16, (bottom - y) as u16),
                });
            }
        }
    }
    tiles
}

impl Graphics {
    pub fn new(encoder: Encoder) -> Self {
        let wake = Arc::new(Notify::new());
        let server = GraphicsPipelineServer::new(Box::new(Handler(wake.clone(), Vec::new())));
        Self {
            wake,
            pipeline: Arc::new(Mutex::new(server)),
            events: Default::default(),
            started: Instant::now(),
            queued: Default::default(),
            codec: Default::default(),
            encoder,
            video: Default::default(),
        }
    }

    pub(crate) fn submit(
        &self,
        frame: &crate::Frame,
        previous: Option<&crate::Frame>,
    ) -> Result<Submission> {
        let mut pipe = self.pipeline.lock().unwrap();
        let Some(channel) = pipe.channel_id().filter(|_| pipe.is_ready()) else {
            return Ok(Submission::Bitmap);
        };
        if self.queued.load(Ordering::Acquire) {
            return Ok(Submission::Busy);
        }
        let (w, h) = frame.dimensions();
        let existing = pipe.surface_ids().next();
        let mut video = self.video.lock().unwrap();
        let surface = match existing {
            Some(id) if pipe.output_dimensions() == (w, h) => id,
            _ => {
                pipe.resize_with_monitors(
                    w,
                    h,
                    vec![ironrdp_pdu::gcc::Monitor {
                        left: 0,
                        top: 0,
                        right: i32::from(w) - 1,
                        bottom: i32::from(h) - 1,
                        flags: ironrdp_pdu::gcc::MonitorFlags::PRIMARY,
                    }],
                );
                let id = pipe
                    .create_surface(w, h)
                    .context("EGFX surface creation failed")?;
                anyhow::ensure!(
                    pipe.map_surface_to_output(id, 0, 0),
                    "EGFX surface mapping failed"
                );
                *video = None;
                let avc420 = matches!(
                    pipe.negotiated_capabilities(),
                    Some(CapabilitySet::V8_1 { .. } | CapabilitySet::V10_7 { .. })
                ) && pipe.supports_avc420();
                if let Encoder::Nvenc(avg_bitrate) = self.encoder
                    && avc420 && w % 2 == 0 && h % 2 == 0 {
                    *video = H264::new(w, h, avg_bitrate);
                    if video.is_none() {
                        eprintln!("IronRDP NVENC initialization failed; using ClearCodec");
                    }
                }
                let mode = if video.is_some() {
                    "AVC420 NVENC"
                } else {
                    "ClearCodec"
                };
                eprintln!("IronRDP EGFX {mode} surface {w}x{h}");
                id
            }
        };
        if pipe.should_backpressure() {
            return Ok(Submission::Busy);
        }
        let mut baseline = previous.filter(|_| existing == Some(surface));
        let timestamp = self.started.elapsed().as_millis() as u32;
        if let Some(encoder) = video.as_mut() {
            match match frame {
                crate::Frame::Gpu(f) => encoder.encode_gpu(f),
                crate::Frame::Bitmap(f) => encoder.encode(f),
            } {
                Ok(packet) => {
                    if matches!(frame, crate::Frame::Gpu(_)) && existing != Some(surface) {
                        eprintln!("IronRDP submitted CUDA IPC AVC420 {w}x{h}");
                    }
                    let mut region = ironrdp_egfx::pdu::Avc420Region::full_frame(w, h, 24);
                    region.quality = 80;
                    pipe.send_avc420_frame(surface, packet, &[region], timestamp)
                        .context("EGFX H.264 frame rejected")?;
                }
                Err(error) => {
                    eprintln!("IronRDP {error}; repainting with ClearCodec");
                    *video = None;
                    baseline = None;
                }
            }
        }
        if video.is_none() {
            let bitmap = frame.bitmap()?;
            let baseline = baseline.and_then(|p| match p {
                crate::Frame::Bitmap(f) => Some(f),
                _ => None,
            });
            let tiles = encode_damage(&bitmap, baseline, &mut self.codec.lock().unwrap());
            if tiles.is_empty() {
                return Ok(Submission::Sent);
            }
            pipe.send_mixed_frame(surface, tiles, timestamp)
                .context("EGFX frame rejected")?;
        }
        let mut messages = ironrdp_dvc::encode_dvc_messages(
            channel,
            pipe.drain_output(),
            ChannelFlags::SHOW_PROTOCOL,
        )?;
        let bytes = messages
            .pop()
            .context("missing EGFX end marker")?
            .encode_unframed_pdu()?;
        self.queued.store(true, Ordering::Release);
        messages.push(
            SvcMessage::from(LastFragment {
                bytes,
                queued: self.queued.clone(),
                wake: self.wake.clone(),
            })
            .with_flags(ChannelFlags::SHOW_PROTOCOL),
        );
        self.events
            .lock()
            .unwrap()
            .as_ref()
            .context("missing server event sender")?
            .send(ServerEvent::Egfx(EgfxServerMessage::SendMessages {
                messages,
            }))
            .map_err(|_| anyhow::anyhow!("EGFX receiver stopped"))?;
        Ok(Submission::Sent)
    }

    pub fn ready(&self) -> bool {
        self.pipeline.lock().unwrap().is_ready()
    }
}

impl ServerEventSender for Graphics {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        *self.events.lock().unwrap() = Some(sender);
    }
}
impl GfxServerFactory for Graphics {
    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        Box::new(Handler(self.wake.clone(), Vec::new()))
    }
    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        Some((
            GfxDvcBridge::new(self.pipeline.clone()),
            self.pipeline.clone(),
        ))
    }
}
