// SPDX-License-Identifier: Apache-2.0
//! NVIDIA HEVC strip encoder for the Apple Screen Sharing media interface.
pub mod capture;
mod convert;
pub mod nal;
mod nvenc;
pub mod plan;
mod tiles;

use apple_screen_sharing::encode::*;
pub use nvenc::NvencEncoder;

/// VUI colour description. The matrix is always BT.709 (the GPU conversion uses it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorTags {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub full_range: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primaries {
    Bt709,
    DisplayP3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Bt709,
    /// IEC 61966-2-1, what Hyprland outputs and Apple tags.
    Srgb,
}

impl Default for ColorTags {
    fn default() -> Self {
        Self {
            primaries: Primaries::Bt709,
            transfer: Transfer::Srgb,
            full_range: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma {
    Yuv420,
    /// x265 only (Main 4:4:4 8-bit).
    Yuv444,
}

pub struct EncoderConfig {
    pub bitrate_bps: Option<u64>,
    pub fps: u32,
    pub chroma: Chroma,
    pub color: ColorTags,
}
impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            bitrate_bps: Some(8_000_000),
            fps: 60,
            chroma: Chroma::Yuv420,
            color: ColorTags::default(),
        }
    }
}
impl apple_screen_sharing::encode::EncoderFactory<capture::DmabufBuffer> for EncoderConfig {
    fn open(&self, layout: TileLayout) -> anyhow::Result<Encoder<capture::DmabufBuffer>> {
        Ok(Box::new(NvencEncoder::new(
            layout.width,
            layout.height,
            self,
        )?))
    }
}
impl EncoderConfig {
    pub fn check(&self) -> anyhow::Result<()> {
        NvencEncoder::new(1280, 720, self).map(drop)
    }
}
