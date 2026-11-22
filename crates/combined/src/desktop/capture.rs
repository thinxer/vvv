use super::Screen;
use crate::cursor;
use anyhow::Result;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use vvv_nvenc::capture::DmabufBuffer;
pub(crate) struct Capture {
    _cuda: Option<CudaListener>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct GuestCapture {
    hotspots: std::collections::HashMap<u64, (u16, u16)>,
    screen: Arc<Screen>,
    stop: Arc<AtomicBool>,
}

impl wayland_capture::CaptureSink for GuestCapture {
    fn frame(&mut self, id: u32, w: u32, h: u32, pixels: Vec<u8>) {
        if let Ok(buffer) = DmabufBuffer::from_bgrx(w, h, pixels) {
            self.screen.commit(id, buffer);
        }
    }
    fn cursor(&mut self, w: u32, h: u32, pixels: &[u8], x: i32, y: i32) {
        let pointer = *self.screen.pointer_position.lock().unwrap();
        #[cfg(feature = "combined")]
        if let Some(output) = self.screen.output.get().and_then(|r| r.upgrade()) {
            output.publish_cursor((w, h), pixels, (x, y), pointer.unwrap_or_default());
        }
        let image = cursor::image(w, h, pixels, (x, y), pointer, &mut self.hotspots);
        let mut current = self.screen.cursor.lock().unwrap();
        if current.as_ref().map(|c| c.serial) != image.as_ref().map(|c| c.serial) {
            *current = image.map(Arc::new);
            drop(current);
            #[cfg(feature = "combined")]
            self.screen.notify_observers();
        }
    }
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

impl Capture {
    pub(crate) fn start(
        path: PathBuf,
        cuda_socket: Option<PathBuf>,
        screen: Arc<Screen>,
    ) -> Result<(Self, tokio::sync::oneshot::Receiver<String>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let cuda = cuda_socket
            .map(|p| CudaListener::start(p, screen.clone()))
            .transpose()?;
        let sink = GuestCapture {
            hotspots: Default::default(),
            screen,
            stop: stop.clone(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("guest-capture".into())
            .spawn(move || {
                let result = wayland_capture::run(path, sink);
                let _ = tx.send(
                    result
                        .err()
                        .map_or_else(|| "capture stopped".into(), |e| e.to_string()),
                );
            })?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
                _cuda: cuda,
            },
            rx,
        ))
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct CudaListener {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}
impl CudaListener {
    fn start(path: PathBuf, screen: Arc<Screen>) -> Result<Self> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        use std::os::unix::net::UnixStream;
        if std::fs::symlink_metadata(&path).is_ok_and(|s| s.file_type().is_socket())
            && UnixStream::connect(&path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            std::fs::remove_file(&path)?;
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        // crosvm may wait for this socket while a viewer callback waits for a
        // display hotplug. Scanout delivery must progress independently of that runtime.
        std::thread::Builder::new()
            .name("cuda-scanout".into())
            .spawn(move || {
                while let Ok((socket, _)) = listener.accept() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    match cuda_scanout::receive(socket) {
                        Ok((id, frame)) => screen.commit(id, DmabufBuffer::from_cuda(frame)),
                        Err(e) => tracing::warn!("CUDA scanout: {e:#}"),
                    }
                }
            })?;
        Ok(Self { path, stop })
    }
}
impl Drop for CudaListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::os::unix::net::UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}
