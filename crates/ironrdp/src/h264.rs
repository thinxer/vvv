use crate::nvenc::Nvenc;
use anyhow::{Result, ensure};
use ironrdp_server::BitmapUpdate;
use yuv::{
    YuvBiPlanarImageMut, YuvChromaSubsampling, YuvConversionMode, YuvRange, YuvStandardMatrix,
    bgra_to_yuv_nv12,
};

#[derive(Clone, Copy, PartialEq)]
pub enum Encoder {
    ClearCodec,
    Nvenc(std::num::NonZeroU32),
}

pub struct H264 {
    encoder: Nvenc,
    nv12: YuvBiPlanarImageMut<'static, u8>,
}

impl H264 {
    pub fn new(width: u16, height: u16, avg_bitrate: std::num::NonZeroU32) -> Option<Self> {
        let encoder = Nvenc::new(width, height, avg_bitrate)
            .inspect_err(|e| eprintln!("NVENC: {e}"))
            .ok()?;
        let nv12 =
            YuvBiPlanarImageMut::alloc(width.into(), height.into(), YuvChromaSubsampling::Yuv420);
        Some(Self { encoder, nv12 })
    }

    pub fn encode_gpu(&mut self, frame: &cuda_scanout::Frame) -> Result<&[u8]> {
        self.encoder.encode_gpu(frame)
    }

    pub fn encode(&mut self, frame: &BitmapUpdate) -> Result<&[u8]> {
        ensure!(
            self.nv12.width == u32::from(frame.width.get())
                && self.nv12.height == u32::from(frame.height.get()),
            "NVENC surface size changed"
        );
        bgra_to_yuv_nv12(
            &mut self.nv12,
            &frame.data,
            frame.stride.get().try_into()?,
            YuvRange::Limited,
            YuvStandardMatrix::Bt601,
            YuvConversionMode::Balanced,
        )?;
        self.encoder
            .encode(self.nv12.y_plane.borrow(), self.nv12.uv_plane.borrow())
    }
}
