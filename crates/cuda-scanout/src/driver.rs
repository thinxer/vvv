// Linux CUDA driver ABI. Load lazily so bitmap-only receivers need no CUDA installation.
use anyhow::{Result, ensure};
use libloading::Library;
use std::sync::OnceLock;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcHandle(pub [u8; 64]);
#[repr(C)]
#[derive(Default)]
pub struct CopySide {
    pub x: usize,
    pub y: usize,
    pub kind: u32,
    pub host: usize,
    pub device: u64,
    pub array: usize,
    pub pitch: usize,
}
#[repr(C)]
pub struct Copy2D {
    pub src: CopySide,
    pub dst: CopySide,
    pub width: usize,
    pub height: usize,
}
macro_rules! driver {
    ($($field:ident : $symbol:literal ($($arg:ty),*)),+ $(,)?) => {
        pub struct Driver {
            _library: Library, context: usize,
            $(pub $field: unsafe extern "C" fn($($arg),*) -> i32,)+
        }
        impl Driver {
            fn load() -> Result<Self> {
                unsafe {
                    let lib = Library::new("libcuda.so.1")?;
                    let init = lib.get::<unsafe extern "C" fn(u32)->i32>(b"cuInit")?;
                    ok(init(0), "init")?;
                    let mut driver = Driver {
                        $($field: *lib.get($symbol.as_bytes())?,)+
                        context: 0, _library: lib,
                    };
                    let retain = driver._library.get::<unsafe extern "C" fn(*mut usize,i32)->i32>(b"cuDevicePrimaryCtxRetain")?;
                    ok(retain(&mut driver.context,0), "retain primary context")?;
                    Ok(driver)
                }
            }
        }
    }
}
driver! {
    push:"cuCtxPushCurrent_v2"(usize), pop:"cuCtxPopCurrent_v2"(*mut usize),
    alloc:"cuMemAlloc_v2"(*mut u64,usize), free:"cuMemFree_v2"(u64),
    zero:"cuMemsetD8_v2"(u64,u8,usize), sync:"cuCtxSynchronize"(),
    export:"cuIpcGetMemHandle"(*mut IpcHandle,u64), import:"cuIpcOpenMemHandle_v2"(*mut u64,IpcHandle,u32),
    close:"cuIpcCloseMemHandle"(u64), register:"cuGraphicsGLRegisterImage"(*mut usize,u32,u32,u32),
    map:"cuGraphicsMapResources"(u32,*mut usize,usize), unmap:"cuGraphicsUnmapResources"(u32,*mut usize,usize),
    array:"cuGraphicsSubResourceGetMappedArray"(*mut usize,usize,u32,u32), unregister:"cuGraphicsUnregisterResource"(usize),
    load_module:"cuModuleLoadData"(*mut usize,*const u8), unload_module:"cuModuleUnload"(usize),
    function:"cuModuleGetFunction"(*mut usize,usize,*const u8),
    launch:"cuLaunchKernel"(usize,u32,u32,u32,u32,u32,u32,u32,usize,*mut *mut std::ffi::c_void,*mut *mut std::ffi::c_void),
    copy:"cuMemcpy2D_v2"(*const Copy2D),
}
pub fn ok(code: i32, op: &str) -> Result<()> {
    ensure!(code == 0, "CUDA {op} returned {code}");
    Ok(())
}
pub fn driver() -> Result<&'static Driver> {
    static INSTANCE: OnceLock<Result<Driver, String>> = OnceLock::new();
    INSTANCE
        .get_or_init(|| Driver::load().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}
impl Driver {
    pub fn enter(&self) -> Result<Guard<'_>> {
        unsafe {
            ok((self.push)(self.context), "push context")?;
        }
        Ok(Guard(self))
    }
}
pub struct Guard<'a>(&'a Driver);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.0.pop)(&mut 0);
        }
    }
}
