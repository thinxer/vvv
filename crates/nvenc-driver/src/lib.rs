//! NVIDIA API 13 driver and CUDA primary-context lifetime shared by both encoders.
#[allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    unsafe_op_in_unsafe_fn,
    unnecessary_transmutes
)]
pub mod sys {
    include!(concat!(env!("OUT_DIR"), "/nvenc.rs"));
}

use anyhow::{Context, Result, ensure};
use libloading::Library;
use std::{ffi::c_void, ptr};
use sys::*;

pub type Handle = *mut c_void;
type ReleaseContext = unsafe extern "C" fn(i32) -> i32;

/// Owns one encoder session and the retained CUDA context it uses. The codec
/// owner releases its registered surfaces and buffers before dropping this.
pub struct Session {
    handle: Handle,
    api: NV_ENCODE_API_FUNCTION_LIST,
    device: i32,
    release: ReleaseContext,
    _cuda: Library,
    _driver: Library,
}

impl Session {
    pub fn open() -> Result<Self> {
        let cuda = unsafe { Library::new("libcuda.so.1")? };
        let driver = unsafe { Library::new("libnvidia-encode.so.1")? };
        let mut api = NV_ENCODE_API_FUNCTION_LIST {
            version: RUST_NV_ENCODE_API_FUNCTION_LIST_VER,
            ..Default::default()
        };
        let mut device = 0;
        let mut context = ptr::null_mut();
        let release;
        // All required symbols are resolved before retaining the context.
        unsafe {
            let create =
                driver
                    .get::<unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS>(
                        b"NvEncodeAPICreateInstance",
                    )?;
            ensure!(
                create(&mut api) == NV_ENC_SUCCESS,
                "NVENC API 13 unavailable"
            );
            api.nvEncDestroyEncoder
                .context("missing nvEncDestroyEncoder")?;
            let init = cuda.get::<unsafe extern "C" fn(u32) -> i32>(b"cuInit")?;
            let get = cuda.get::<unsafe extern "C" fn(*mut i32, i32) -> i32>(b"cuDeviceGet")?;
            let retain = cuda.get::<unsafe extern "C" fn(*mut Handle, i32) -> i32>(
                b"cuDevicePrimaryCtxRetain",
            )?;
            release = *cuda.get::<ReleaseContext>(b"cuDevicePrimaryCtxRelease_v2")?;
            ensure!(
                init(0) == 0 && get(&mut device, 0) == 0,
                "CUDA device 0 unavailable"
            );
            ensure!(
                retain(&mut context, device) == 0,
                "retain CUDA primary context"
            );
        }
        let mut session = Self {
            handle: ptr::null_mut(),
            api,
            device,
            release,
            _cuda: cuda,
            _driver: driver,
        };
        let mut params = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: RUST_NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            apiVersion: RUST_NVENCAPI_VERSION,
            deviceType: NV_ENC_DEVICE_TYPE_CUDA,
            device: context,
            ..Default::default()
        };
        let open = session
            .api
            .nvEncOpenEncodeSessionEx
            .context("missing nvEncOpenEncodeSessionEx")?;
        let mut handle = ptr::null_mut();
        let status = unsafe { open(&mut params, &mut handle) };
        ensure!(
            status == NV_ENC_SUCCESS,
            "nvEncOpenEncodeSessionEx returned {status}"
        );
        session.handle = handle;
        Ok(session)
    }

    pub fn handle(&self) -> Handle {
        self.handle
    }

    pub fn api(&self) -> &NV_ENCODE_API_FUNCTION_LIST {
        &self.api
    }

    /// # Safety
    /// A non-null buffer must be an unlocked, live input buffer owned by this session.
    pub unsafe fn destroy_input(&self, buffer: Handle) {
        if !buffer.is_null()
            && let Some(destroy) = self.api.nvEncDestroyInputBuffer
        {
            unsafe { destroy(self.handle, buffer) };
        }
    }

    /// # Safety
    /// A non-null buffer must be an unlocked, live bitstream buffer owned by this session.
    pub unsafe fn destroy_bitstream(&self, buffer: Handle) {
        if !buffer.is_null()
            && let Some(destroy) = self.api.nvEncDestroyBitstreamBuffer
        {
            unsafe { destroy(self.handle, buffer) };
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null()
                && let Some(destroy) = self.api.nvEncDestroyEncoder
            {
                destroy(self.handle);
            }
            (self.release)(self.device);
        }
    }
}
