// Keep a pool fd alive for its buffers, including after wl_shm_pool.destroy.
// Positional reads give us an owned snapshot and report truncation as an I/O error.
use super::Capture;
use std::{
    fs::File,
    io,
    os::unix::fs::FileExt,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, Resource, WEnum,
    protocol::{wl_buffer, wl_shm, wl_shm_pool},
};

pub struct Pool {
    file: Arc<File>,
    size: AtomicI32,
}
pub struct Buffer {
    file: Arc<File>,
    offset: u64,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}
impl Buffer {
    pub fn read_pixels(&self) -> io::Result<Vec<u8>> {
        let row = self.width * 4;
        let mut pixels = vec![0; row * self.height];
        if row == self.stride {
            self.file.read_exact_at(&mut pixels, self.offset)?;
        } else {
            for (y, dst) in pixels.chunks_exact_mut(row).enumerate() {
                self.file
                    .read_exact_at(dst, self.offset + (y * self.stride) as u64)?;
            }
        }
        Ok(pixels)
    }
}
impl Dispatch<wl_shm::WlShm, ()> for Capture {
    fn request(
        _: &mut Self,
        _: &Client,
        res: &wl_shm::WlShm,
        req: wl_shm::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm::Request::CreatePool { id, fd, size } = req {
            if size <= 0 {
                res.post_error(wl_shm::Error::InvalidFd, "invalid SHM pool size");
                return;
            }
            init.init(
                id,
                Pool {
                    file: Arc::new(File::from(fd)),
                    size: AtomicI32::new(size),
                },
            );
        }
    }
}
impl Dispatch<wl_shm_pool::WlShmPool, Pool> for Capture {
    fn request(
        _: &mut Self,
        _: &Client,
        res: &wl_shm_pool::WlShmPool,
        req: wl_shm_pool::Request,
        pool: &Pool,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let size = pool.size.load(Ordering::Relaxed);
        match req {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                if !matches!(
                    format,
                    WEnum::Value(wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)
                ) {
                    res.post_error(wl_shm::Error::InvalidFormat, "capture requires 32-bit RGB");
                    return;
                }
                if !(1..=8192).contains(&width)
                    || !(1..=8192).contains(&height)
                    || offset < 0
                    || stride < width * 4
                    || i64::from(offset)
                        + i64::from(stride) * i64::from(height - 1)
                        + i64::from(width) * 4
                        > i64::from(size)
                {
                    res.post_error(
                        wl_shm::Error::InvalidStride,
                        "buffer exceeds capture or pool bounds",
                    );
                    return;
                }
                init.init(
                    id,
                    Buffer {
                        file: pool.file.clone(),
                        offset: offset as u64,
                        width: width as usize,
                        height: height as usize,
                        stride: stride as usize,
                    },
                );
            }
            wl_shm_pool::Request::Resize { size: new } if new > size => {
                pool.size.store(new, Ordering::Relaxed)
            }
            wl_shm_pool::Request::Resize { .. } => {
                res.post_error(wl_shm::Error::InvalidFd, "pool must grow")
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, Buffer> for Capture {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Request,
        _: &Buffer,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[test]
    fn padded_buffer_at_offset_and_short_pool() -> io::Result<()> {
        let path =
            std::env::temp_dir().join(format!("wayland-capture-pool-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::fs::remove_file(path)?;
        file.write_all_at(b"_abcdPAD!efgh", 0)?;
        let buffer = Buffer {
            file: Arc::new(file),
            offset: 1,
            width: 1,
            height: 2,
            stride: 8,
        };
        assert_eq!(buffer.read_pixels()?, b"abcdefgh");
        buffer.file.set_len(12)?;
        assert_eq!(
            buffer.read_pixels().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        Ok(())
    }
}
