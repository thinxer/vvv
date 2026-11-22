use crate::driver::*;
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

#[repr(C)]
#[derive(Default)]
struct ResourceInfo {
    handle: u32,
    format: u32,
    width: u32,
    height: u32,
    depth: u32,
    flags: u32,
    texture: u32,
    stride: u32,
    fourcc: u32,
    fd: i32,
}

fn texture(resource: u32, w: u32, h: u32) -> Result<u32> {
    type Query = unsafe extern "C" fn(i32, *mut ResourceInfo) -> i32;
    static QUERY: OnceLock<Option<Query>> = OnceLock::new();
    let query = QUERY
        .get_or_init(|| unsafe {
            libloading::os::unix::Library::this()
                .get::<Query>(b"virgl_renderer_resource_get_info")
                .ok()
                .map(|s| *s)
        })
        .context("virgl unavailable")?;
    let mut info = ResourceInfo::default();
    unsafe {
        ensure!(
            query(resource as i32, &mut info) == 0,
            "virgl texture query"
        );
    }
    ensure!(
        info.width == w
            && info.height == h
            && info.texture != 0
            && info.fourcc == u32::from_le_bytes(*b"AB24"),
        "unsupported scanout format or size"
    );
    Ok(info.texture)
}
fn blit(tex: u32, ptr: u64, w: u32, h: u32) -> Result<()> {
    let d = driver()?;
    let mut resource = 0;
    unsafe {
        ok(
            (d.register)(&mut resource, tex, 0x0de1, 1),
            "register GL texture",
        )?;
        let result = (|| {
            ok((d.map)(1, &mut resource, 0), "map texture")?;
            let result = (|| {
                let mut array = 0;
                ok((d.array)(&mut array, resource, 0, 0), "texture array")?;
                let width = w as usize * 4;
                let copy = Copy2D {
                    src: CopySide {
                        kind: 3,
                        array,
                        ..Default::default()
                    },
                    dst: CopySide {
                        kind: 2,
                        device: ptr,
                        pitch: width.next_multiple_of(256),
                        ..Default::default()
                    },
                    width,
                    height: h as usize,
                };
                ok((d.copy)(&copy), "scanout blit")
            })();
            let unmap = ok((d.unmap)(1, &mut resource, 0), "unmap texture");
            result.and(unmap)
        })();
        let unregister = ok((d.unregister)(resource), "unregister texture");
        result.and(unregister)?;
        ok((d.sync)(), "blit completion")
    }
}
fn present(id: u32, resource: u32, w: u32, h: u32) -> Result<()> {
    static SOCKET: OnceLock<Option<PathBuf>> = OnceLock::new();
    let path = SOCKET
        .get_or_init(|| std::env::var_os("VVV_CUDA_SOCKET").map(PathBuf::from))
        .as_ref()
        .context("no CUDA socket")?;
    let tex = texture(resource, w, h)?;
    let mut socket = UnixStream::connect(path)?;
    let timeout = Some(Duration::from_secs(3));
    socket.set_read_timeout(timeout)?;
    socket.set_write_timeout(timeout)?;
    let mut request = Vec::from(*b"GPU1");
    for word in [id, w, h] {
        request.extend_from_slice(&word.to_le_bytes());
    }
    socket.write_all(&request)?;
    let mut handle = IpcHandle([0; 64]);
    socket.read_exact(&mut handle.0)?;
    let d = driver()?;
    let _ctx = d.enter()?;
    let mut ptr = 0;
    unsafe {
        ok((d.import)(&mut ptr, handle, 1), "open frame")?;
    }
    let result = blit(tex, ptr, w, h);
    let close = unsafe { ok((d.close)(ptr), "release frame") };
    result.and(close)?;
    socket.write_all(&[1])?;
    Ok(())
}
#[unsafe(no_mangle)]
pub extern "C" fn vvv_cuda_present_v1(id: u32, resource: u32, w: u32, h: u32) -> i32 {
    match present(id, resource, w, h) {
        Ok(()) => 1,
        Err(e) => {
            static ERRORS: AtomicU64 = AtomicU64::new(0);
            let n = ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                eprintln!("CUDA scanout fallback ({n}): {e:#}");
            }
            0
        }
    }
}
