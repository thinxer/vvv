//! NVIDIA API 13 encoder. The driver owns the buffers; each session is used serially.
use anyhow::{Context, Result, ensure};
use std::{ptr, slice};

use vvv_nvenc_driver::{Handle, Session, sys::*};

const H264: GUID = GUID {
    Data1: 0x6bc82762,
    Data2: 0x4e63,
    Data3: 0x4ca4,
    Data4: [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
};
const HIGH: GUID = GUID {
    Data1: 0xe7cbc309,
    Data2: 0x4f7a,
    Data3: 0x4b89,
    Data4: [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
};
const P4: GUID = GUID {
    Data1: 0x90a7b826,
    Data2: 0xdf06,
    Data3: 0x4862,
    Data4: [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
};

pub struct Nvenc {
    input: Handle,
    output: Handle,
    locked: bool,
    visible: (usize, usize),
    coded: (u32, u32),
    pts: u64,
    driver: Session,
}
// The CUDA primary context and NVENC session support calls from different threads.
// Graphics holds the per-session mutex across every operation.
unsafe impl Send for Nvenc {}
macro_rules! call {
    ($enc:expr,$f:ident,$($arg:expr),+)=>{{
        let f=$enc.driver.api().$f.context(stringify!($f))?;
        let status=unsafe{f($($arg),+)};
        ensure!(status==NV_ENC_SUCCESS,"{} returned {}",stringify!($f),status);
    }};
}

impl Nvenc {
    pub fn new(width: u16, height: u16, avg_bitrate: std::num::NonZeroU32) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0,
            "NV12 requires even dimensions"
        );
        let mut enc = Self {
            driver: Session::open()?,
            input: ptr::null_mut(),
            output: ptr::null_mut(),
            locked: false,
            visible: (width.into(), height.into()),
            coded: (
                u32::from(width).next_multiple_of(16),
                u32::from(height).next_multiple_of(16),
            ),
            pts: 0,
        };
        enc.initialize(avg_bitrate.get())?;
        Ok(enc)
    }
    fn initialize(&mut self, avg_bitrate: u32) -> Result<()> {
        let (w, h) = self.coded;
        let mut preset = NV_ENC_PRESET_CONFIG {
            version: RUST_NV_ENC_PRESET_CONFIG_VER,
            ..Default::default()
        };
        preset.presetCfg.version = RUST_NV_ENC_CONFIG_VER;
        call!(
            self,
            nvEncGetEncodePresetConfigEx,
            self.driver.handle(),
            H264,
            P4,
            NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            &mut preset
        );
        let cfg = &mut preset.presetCfg;
        cfg.profileGUID = HIGH;
        cfg.gopLength = 600;
        cfg.frameIntervalP = 1;
        const FPS: u32 = 60;
        cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
        cfg.rcParams.averageBitRate = avg_bitrate;
        cfg.rcParams.vbvBufferSize = avg_bitrate.div_ceil(FPS);
        cfg.rcParams.set_enableLookahead(0);
        cfg.rcParams.set_zeroReorderDelay(1);
        let h264 = unsafe { &mut cfg.encodeCodecConfig.h264Config };
        h264.idrPeriod = cfg.gopLength;
        h264.chromaFormatIDC = 1;
        // Keep optional video signal metadata absent for Windows App.
        h264.h264VUIParameters.videoSignalTypePresentFlag = 0;
        h264.h264VUIParameters.colourDescriptionPresentFlag = 0;
        let mut init = NV_ENC_INITIALIZE_PARAMS {
            version: RUST_NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: H264,
            presetGUID: P4,
            tuningInfo: NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            encodeWidth: w,
            encodeHeight: h,
            darWidth: w,
            darHeight: h,
            frameRateNum: FPS,
            frameRateDen: 1,
            enablePTD: 1,
            encodeConfig: cfg,
            ..Default::default()
        };
        call!(
            self,
            nvEncInitializeEncoder,
            self.driver.handle(),
            &mut init
        );
        let mut input = NV_ENC_CREATE_INPUT_BUFFER {
            version: RUST_NV_ENC_CREATE_INPUT_BUFFER_VER,
            width: w,
            height: h,
            bufferFmt: NV_ENC_BUFFER_FORMAT_NV12,
            ..Default::default()
        };
        call!(
            self,
            nvEncCreateInputBuffer,
            self.driver.handle(),
            &mut input
        );
        self.input = input.inputBuffer;
        let mut output = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: RUST_NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };
        call!(
            self,
            nvEncCreateBitstreamBuffer,
            self.driver.handle(),
            &mut output
        );
        self.output = output.bitstreamBuffer;
        Ok(())
    }
    pub fn encode(&mut self, y: &[u8], uv: &[u8]) -> Result<&[u8]> {
        let (w, h) = self.visible;
        let (cw, ch) = self.coded;
        ensure!(
            y.len() >= w * h && uv.len() >= w * h / 2,
            "invalid NV12 planes"
        );
        if self.locked {
            call!(
                self,
                nvEncUnlockBitstream,
                self.driver.handle(),
                self.output
            );
            self.locked = false;
        }
        let mut lock = NV_ENC_LOCK_INPUT_BUFFER {
            version: RUST_NV_ENC_LOCK_INPUT_BUFFER_VER,
            inputBuffer: self.input,
            ..Default::default()
        };
        call!(self, nvEncLockInputBuffer, self.driver.handle(), &mut lock);
        let pitch = lock.pitch as usize;
        let valid = !lock.bufferDataPtr.is_null() && lock.pitch >= cw;
        if valid {
            // The driver's locked NV12 allocation has ch Y rows and ch/2 UV rows.
            let rows = unsafe {
                slice::from_raw_parts_mut(
                    lock.bufferDataPtr.cast::<u8>(),
                    pitch * ch as usize * 3 / 2,
                )
            };
            let (luma, chroma) = rows.split_at_mut(pitch * ch as usize);
            upload(luma, pitch, y, w, h, cw as usize, 1);
            upload(chroma, pitch, uv, w, h / 2, cw as usize, 2);
        }
        call!(
            self,
            nvEncUnlockInputBuffer,
            self.driver.handle(),
            self.input
        );
        ensure!(valid, "invalid NVENC input buffer");
        self.picture(self.input, NV_ENC_BUFFER_FORMAT_NV12, 0)?;
        let bits = self.lock_packet()?;
        self.bitstream(bits)
    }

    pub fn encode_gpu(&mut self, frame: &cuda_scanout::Frame) -> Result<&[u8]> {
        let (w, h) = self.coded;
        if self.locked {
            call!(
                self,
                nvEncUnlockBitstream,
                self.driver.handle(),
                self.output
            );
            self.locked = false;
        }
        let mut registration = NV_ENC_REGISTER_RESOURCE {
            version: RUST_NV_ENC_REGISTER_RESOURCE_VER,
            resourceType: NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
            width: w,
            height: h,
            pitch: frame.pitch as u32,
            resourceToRegister: frame.device_ptr() as Handle,
            bufferFormat: NV_ENC_BUFFER_FORMAT_ABGR,
            ..Default::default()
        };
        call!(
            self,
            nvEncRegisterResource,
            self.driver.handle(),
            &mut registration
        );
        let registered = registration.registeredResource;
        let mut map = NV_ENC_MAP_INPUT_RESOURCE {
            version: RUST_NV_ENC_MAP_INPUT_RESOURCE_VER,
            registeredResource: registered,
            ..Default::default()
        };
        let result = (|| -> Result<NV_ENC_LOCK_BITSTREAM> {
            call!(self, nvEncMapInputResource, self.driver.handle(), &mut map);
            let encoded = self
                .picture(map.mappedResource, map.mappedBufferFmt, frame.pitch as u32)
                .and_then(|_| self.lock_packet());
            call!(
                self,
                nvEncUnmapInputResource,
                self.driver.handle(),
                map.mappedResource
            );
            encoded
        })();
        call!(
            self,
            nvEncUnregisterResource,
            self.driver.handle(),
            registered
        );
        let bits = result?;
        self.bitstream(bits)
    }

    fn picture(&mut self, input: Handle, format: NV_ENC_BUFFER_FORMAT, pitch: u32) -> Result<()> {
        let (cw, ch) = self.coded;
        let mut pic = NV_ENC_PIC_PARAMS {
            version: RUST_NV_ENC_PIC_PARAMS_VER,
            inputWidth: cw,
            inputHeight: ch,
            inputBuffer: input,
            inputPitch: pitch,
            outputBitstream: self.output,
            bufferFmt: format,
            pictureStruct: NV_ENC_PIC_STRUCT_FRAME,
            inputTimeStamp: self.pts,
            ..Default::default()
        };
        self.pts += 1;
        call!(self, nvEncEncodePicture, self.driver.handle(), &mut pic);
        Ok(())
    }
    fn lock_packet(&mut self) -> Result<NV_ENC_LOCK_BITSTREAM> {
        let mut bits = NV_ENC_LOCK_BITSTREAM {
            version: RUST_NV_ENC_LOCK_BITSTREAM_VER,
            outputBitstream: self.output,
            ..Default::default()
        };
        call!(self, nvEncLockBitstream, self.driver.handle(), &mut bits);
        self.locked = true;
        ensure!(
            !bits.bitstreamBufferPtr.is_null() && bits.bitstreamSizeInBytes > 0,
            "empty NVENC bitstream"
        );
        Ok(bits)
    }
    fn bitstream(&self, bits: NV_ENC_LOCK_BITSTREAM) -> Result<&[u8]> {
        // Borrowing self keeps the bitstream locked until the caller is done.
        Ok(unsafe {
            slice::from_raw_parts(
                bits.bitstreamBufferPtr.cast(),
                bits.bitstreamSizeInBytes as usize,
            )
        })
    }
}

fn upload(
    dst: &mut [u8],
    pitch: usize,
    src: &[u8],
    w: usize,
    h: usize,
    coded: usize,
    pixel: usize,
) {
    for (y, out) in dst.chunks_exact_mut(pitch).enumerate() {
        let offset = y.min(h - 1) * w;
        let row = &src[offset..offset + w];
        out[..w].copy_from_slice(row);
        for pad in out[w..coded].chunks_exact_mut(pixel) {
            pad.copy_from_slice(&row[w - pixel..]);
        }
    }
}

impl Drop for Nvenc {
    fn drop(&mut self) {
        unsafe {
            if self.locked {
                if let Some(f) = self.driver.api().nvEncUnlockBitstream {
                    f(self.driver.handle(), self.output);
                }
            }
            self.driver.destroy_bitstream(self.output);
            self.driver.destroy_input(self.input);
        }
    }
}
