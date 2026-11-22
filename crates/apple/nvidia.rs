//! Single, synchronous HEVC picture sequence. CUDA and NVENC are loaded only on use.
#[allow(
    unused_imports,
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    unsafe_op_in_unsafe_fn,
    unnecessary_transmutes
)]
mod sdk {
    include!("nvenc_bindings.rs");
}
use anyhow::{Context, Result, ensure};
use libloading::Library;
use sdk::*;
use std::{ffi::c_void, ptr};

type Handle = *mut c_void;
type Release = unsafe extern "C" fn(i32) -> i32;
#[derive(Default)]
struct Slot {
    output: Handle,
    surface: Option<cuda_scanout::Nv12Tile>,
    registered: Handle,
    mapped: Handle,
    pending: bool,
}

pub(super) struct Nvidia {
    _cuda: Library,
    _driver: Library,
    release: Release,
    device: i32,
    session: Handle,
    input: Handle,
    slots: [Slot; super::TILE_COUNT],
    api: NV_ENCODE_API_FUNCTION_LIST,
    params: NV_ENC_INITIALIZE_PARAMS,
    config: NV_ENC_CONFIG,
    pts: u64,
}

// These identifiers come from nvEncodeAPI.h. The SDK defines them as static C aggregates.
const CODEC: GUID = GUID {
    Data1: 0x790cdc88,
    Data2: 0x4522,
    Data3: 0x4d7b,
    Data4: [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 3],
};
const PRESET_P1: GUID = GUID {
    Data1: 0xfc0a8d3e,
    Data2: 0x45f8,
    Data3: 0x4cf8,
    Data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};
const MAIN: GUID = GUID {
    Data1: 0xb514c39a,
    Data2: 0xb55b,
    Data3: 0x40fa,
    Data4: [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
};

macro_rules! call {
    ($s:ident, $name:ident, $($arg:expr),*) => {{
        let f = $s.api.$name.context(concat!("missing NVIDIA function ", stringify!($name)))?;
        let status = unsafe { f($($arg),*) };
        ensure!(status == NV_ENC_SUCCESS, "{} returned NVIDIA status {}", stringify!($name),status);
    }}
}

impl Nvidia {
    pub(super) fn open(layout: super::TileLayout, cfg: &super::EncoderConfig) -> Result<Self> {
        let cuda = unsafe { Library::new("libcuda.so.1") }?;
        let driver = unsafe { Library::new("libnvidia-encode.so.1") }?;
        let mut api = NV_ENCODE_API_FUNCTION_LIST {
            version: RUST_NV_ENCODE_API_FUNCTION_LIST_VER,
            ..Default::default()
        };
        let mut device = 0;
        let mut context = ptr::null_mut();
        // Libraries stay loaded until all resources allocated by their functions are released.
        let release;
        unsafe {
            let init = cuda.get::<unsafe extern "C" fn(u32) -> i32>(b"cuInit\0")?;
            let get = cuda.get::<unsafe extern "C" fn(*mut i32, i32) -> i32>(b"cuDeviceGet\0")?;
            let retain = cuda.get::<unsafe extern "C" fn(*mut Handle, i32) -> i32>(
                b"cuDevicePrimaryCtxRetain\0",
            )?;
            release = *cuda.get::<Release>(b"cuDevicePrimaryCtxRelease_v2\0")?;
            let create = driver
                .get::<unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> u32>(
                    b"NvEncodeAPICreateInstance\0",
                )?;
            ensure!(create(&mut api) == 0, "initializing NVENC API 13");
            ensure!(
                init(0) == 0 && get(&mut device, 0) == 0 && retain(&mut context, device) == 0,
                "initializing CUDA device 0"
            );
        }
        let mut s = Self {
            _cuda: cuda,
            _driver: driver,
            release,
            device,
            api,
            session: ptr::null_mut(),
            input: ptr::null_mut(),
            slots: Default::default(),
            config: Default::default(),
            params: Default::default(),
            pts: 0,
        };
        let mut open = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: RUST_NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: NV_ENC_DEVICE_TYPE_CUDA,
            device: context,
            apiVersion: RUST_NVENCAPI_VERSION,
            ..Default::default()
        };
        call!(s, nvEncOpenEncodeSessionEx, &mut open, &mut s.session);
        tracing::info!(preset = "P1", bitrate_bps = ?cfg.bitrate_bps, "Apple HEVC encoder configuration");
        let mut preset = NV_ENC_PRESET_CONFIG {
            version: RUST_NV_ENC_PRESET_CONFIG_VER,
            ..Default::default()
        };
        preset.presetCfg.version = RUST_NV_ENC_CONFIG_VER;
        call!(
            s,
            nvEncGetEncodePresetConfigEx,
            s.session,
            CODEC,
            PRESET_P1,
            NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            &mut preset
        );
        let mut c = preset.presetCfg;
        c.profileGUID = MAIN;
        c.gopLength = u32::MAX;
        c.frameIntervalP = 1;
        c.rcParams.set_enableLookahead(0);
        c.rcParams.set_zeroReorderDelay(1);
        let bitrate = u32::try_from(cfg.bitrate_bps.context("missing average bitrate")?)?;
        let picture_rate = cfg.fps * super::TILE_COUNT as u32;
        ensure!(picture_rate > 0 && bitrate > 0, "invalid encoder rate");
        c.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
        c.rcParams.averageBitRate = bitrate;
        c.rcParams.vbvBufferSize = bitrate.div_ceil(picture_rate);
        let hevc = unsafe { &mut c.encodeCodecConfig.hevcConfig };
        hevc.idrPeriod = u32::MAX;
        hevc.maxNumRefFramesInDPB = 4;
        hevc.numRefL0 = NV_ENC_NUM_REF_FRAMES_4;
        hevc.set_repeatSPSPPS(1);
        hevc.set_chromaFormatIDC(1);
        let vui = &mut hevc.hevcVUIParameters;
        vui.videoSignalTypePresentFlag = 1;
        vui.videoFullRangeFlag = cfg.color.full_range.into();
        vui.colourDescriptionPresentFlag = 1;
        vui.colourPrimaries = match cfg.color.primaries {
            super::Primaries::Bt709 => 1,
            super::Primaries::DisplayP3 => 12,
        };
        vui.transferCharacteristics = match cfg.color.transfer {
            super::Transfer::Bt709 => 1,
            super::Transfer::Srgb => 13,
        };
        vui.colourMatrix = NV_ENC_VUI_MATRIX_COEFFS_BT709;
        s.config = c;
        s.params = NV_ENC_INITIALIZE_PARAMS {
            version: RUST_NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: CODEC,
            presetGUID: PRESET_P1,
            tuningInfo: NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            encodeWidth: layout.width,
            encodeHeight: layout.tile_height,
            darWidth: layout.width,
            darHeight: layout.tile_height,
            frameRateNum: picture_rate,
            frameRateDen: 1,
            enablePTD: 1,
            ..Default::default()
        };
        // Keep the borrowed config pointer local; the owning Rust object can move after open.
        let mut params = s.params;
        params.encodeConfig = &mut s.config;
        call!(s, nvEncInitializeEncoder, s.session, &mut params);
        let mut input = NV_ENC_CREATE_INPUT_BUFFER {
            version: RUST_NV_ENC_CREATE_INPUT_BUFFER_VER,
            width: layout.width,
            height: layout.tile_height,
            bufferFmt: NV_ENC_BUFFER_FORMAT_NV12,
            ..Default::default()
        };
        call!(s, nvEncCreateInputBuffer, s.session, &mut input);
        s.input = input.inputBuffer;
        let mut output = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: RUST_NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };
        for slot in &mut s.slots {
            call!(s, nvEncCreateBitstreamBuffer, s.session, &mut output);
            slot.output = output.bitstreamBuffer;
        }
        Ok(s)
    }

    pub(super) fn rate(&mut self, bitrate: u32) -> Result<()> {
        let mut config = self.config;
        ensure!(bitrate > 0, "invalid average bitrate");
        config.rcParams.vbvBufferSize = bitrate.div_ceil(self.params.frameRateNum);
        config.rcParams.averageBitRate = bitrate;
        let mut p = NV_ENC_RECONFIGURE_PARAMS {
            version: RUST_NV_ENC_RECONFIGURE_PARAMS_VER,
            reInitEncodeParams: self.params,
            ..Default::default()
        };
        p.reInitEncodeParams.encodeConfig = &mut config;
        call!(self, nvEncReconfigureEncoder, self.session, &mut p);
        self.config = config;
        Ok(())
    }

    pub(super) fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        rows: usize,
        kind: super::PicKind,
    ) -> Result<Vec<u8>> {
        let w = self.params.encodeWidth as usize;
        let h = self.params.encodeHeight as usize;
        ensure!(
            rows >= 2
                && rows <= h
                && rows % 2 == 0
                && y.len() >= rows * w
                && u.len() >= rows * w / 4
                && v.len() >= rows * w / 4,
            "invalid NV12 tile planes"
        );
        let mut lock = NV_ENC_LOCK_INPUT_BUFFER {
            version: RUST_NV_ENC_LOCK_INPUT_BUFFER_VER,
            inputBuffer: self.input,
            ..Default::default()
        };
        call!(self, nvEncLockInputBuffer, self.session, &mut lock);
        let pitch = lock.pitch as usize;
        if lock.bufferDataPtr.is_null() || pitch < w {
            call!(self, nvEncUnlockInputBuffer, self.session, self.input);
            anyhow::bail!("invalid NVENC input surface");
        }
        // Each NV12 allocation has h luma rows and h/2 UV rows at the driver-supplied pitch.
        // Write visible bytes only; padding can be uninitialized.
        for i in 0..h {
            let off = i.min(rows - 1) * w;
            unsafe {
                ptr::copy_nonoverlapping(
                    y[off..].as_ptr(),
                    lock.bufferDataPtr.cast::<u8>().add(i * pitch),
                    w,
                );
            }
        }
        for i in 0..h / 2 {
            let off = i.min(rows / 2 - 1) * (w / 2);
            let dst = unsafe { lock.bufferDataPtr.cast::<u8>().add((h + i) * pitch) };
            for x in 0..w / 2 {
                unsafe {
                    dst.add(2 * x).write(u[off + x]);
                    dst.add(2 * x + 1).write(v[off + x]);
                }
            }
        }
        call!(self, nvEncUnlockInputBuffer, self.session, self.input);
        self.submit_picture(0, self.input, 0, kind)?;
        self.finish(0)
    }

    pub(super) fn submit_tile(
        &mut self,
        frame: &cuda_scanout::Frame,
        pic: &super::TilePic,
        y: u32,
        full_range: bool,
    ) -> Result<()> {
        let slot = &mut self.slots[pic.tile];
        ensure!(
            !slot.pending && slot.mapped.is_null(),
            "NVENC tile still in use"
        );
        if slot.surface.is_none() {
            let surface =
                cuda_scanout::Nv12Tile::new(self.params.encodeWidth, self.params.encodeHeight)?;
            let mut registration = NV_ENC_REGISTER_RESOURCE {
                version: RUST_NV_ENC_REGISTER_RESOURCE_VER,
                resourceType: NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
                width: self.params.encodeWidth,
                height: self.params.encodeHeight,
                pitch: surface.pitch(),
                resourceToRegister: surface.device_ptr() as Handle,
                bufferFormat: NV_ENC_BUFFER_FORMAT_NV12,
                ..Default::default()
            };
            call!(self, nvEncRegisterResource, self.session, &mut registration);
            slot.registered = registration.registeredResource;
            slot.surface = Some(surface);
            if pic.tile == 0 {
                tracing::info!(
                    width = self.params.encodeWidth,
                    tile_height = self.params.encodeHeight,
                    "Apple HEVC using pipelined CUDA NV12 tiles"
                );
            }
        }
        let surface = slot.surface.as_mut().unwrap();
        surface.convert(frame, y, full_range)?;
        let pitch = surface.pitch();
        let mut map = NV_ENC_MAP_INPUT_RESOURCE {
            version: RUST_NV_ENC_MAP_INPUT_RESOURCE_VER,
            registeredResource: slot.registered,
            ..Default::default()
        };
        call!(self, nvEncMapInputResource, self.session, &mut map);
        slot.mapped = map.mappedResource;
        self.submit_picture(pic.tile, map.mappedResource, pitch, pic.kind)
    }

    fn submit_picture(
        &mut self,
        index: usize,
        input: Handle,
        pitch: u32,
        kind: super::PicKind,
    ) -> Result<()> {
        let mut pic = NV_ENC_PIC_PARAMS {
            version: RUST_NV_ENC_PIC_PARAMS_VER,
            inputWidth: self.params.encodeWidth,
            inputHeight: self.params.encodeHeight,
            inputBuffer: input,
            inputPitch: pitch,
            outputBitstream: self.slots[index].output,
            bufferFmt: NV_ENC_BUFFER_FORMAT_NV12,
            pictureStruct: NV_ENC_PIC_STRUCT_FRAME,
            encodePicFlags: match kind {
                super::PicKind::Idr => NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS,
                super::PicKind::Intra => NV_ENC_PIC_FLAG_FORCEINTRA,
                super::PicKind::Inter => 0,
            },
            inputTimeStamp: self.pts,
            ..Default::default()
        };
        self.pts += 1;
        call!(self, nvEncEncodePicture, self.session, &mut pic);
        self.slots[index].pending = true;
        Ok(())
    }

    pub(super) fn finish(&mut self, index: usize) -> Result<Vec<u8>> {
        let slot = &mut self.slots[index];
        ensure!(slot.pending, "NVENC tile has no pending output");
        let mut bits = NV_ENC_LOCK_BITSTREAM {
            version: RUST_NV_ENC_LOCK_BITSTREAM_VER,
            outputBitstream: slot.output,
            ..Default::default()
        };
        call!(self, nvEncLockBitstream, self.session, &mut bits);
        let data = if bits.bitstreamBufferPtr.is_null() || bits.bitstreamSizeInBytes == 0 {
            None
        } else {
            Some(super::nal::strip_sei(unsafe {
                std::slice::from_raw_parts(
                    bits.bitstreamBufferPtr.cast(),
                    bits.bitstreamSizeInBytes as usize,
                )
            }))
        };
        call!(self, nvEncUnlockBitstream, self.session, slot.output);
        slot.pending = false;
        if !slot.mapped.is_null() {
            call!(self, nvEncUnmapInputResource, self.session, slot.mapped);
            slot.mapped = ptr::null_mut();
        }
        data.context("empty NVENC bitstream")
    }
}
impl Drop for Nvidia {
    fn drop(&mut self) {
        // A failure midway through a batch can leave earlier submissions outstanding.
        for index in 0..self.slots.len() {
            if self.slots[index].pending {
                let _ = self.finish(index);
            }
        }
        unsafe {
            for slot in &mut self.slots {
                if !slot.mapped.is_null() {
                    if let Some(f) = self.api.nvEncUnmapInputResource {
                        f(self.session, slot.mapped);
                    }
                }
                if !slot.registered.is_null() {
                    if let Some(f) = self.api.nvEncUnregisterResource {
                        f(self.session, slot.registered);
                    }
                }
                if !slot.output.is_null() {
                    if let Some(f) = self.api.nvEncDestroyBitstreamBuffer {
                        f(self.session, slot.output);
                    }
                }
            }
            if !self.input.is_null() {
                if let Some(f) = self.api.nvEncDestroyInputBuffer {
                    f(self.session, self.input);
                }
            }
            if !self.session.is_null() {
                if let Some(f) = self.api.nvEncDestroyEncoder {
                    f(self.session);
                }
            }
            (self.release)(self.device);
        }
    }
}
