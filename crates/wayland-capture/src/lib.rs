mod metadata;
use rustix::event::{PollFd, PollFlags, poll};
mod protocol;
mod shm;
use std::os::{fd::AsFd, unix::net::UnixStream};
use std::{
    collections::HashMap,
    error::Error,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use wayland_server::{
    Display, ListeningSocket, Resource,
    backend::{ClientData, ObjectId},
    protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
};

#[derive(Default)]
struct Guest;
impl ClientData for Guest {}

struct Capture {
    surfaces: HashMap<ObjectId, protocol::Surface>,
    serial: u32,
    scanouts: HashMap<ObjectId, Option<u32>>,
    written: HashMap<ObjectId, Instant>,
    cursor: Vec<u8>,
    output: Output,
    start: Instant,
}

fn write_image(path: &Path, header: String, pixels: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(header.as_bytes())?;
    file.write_all(pixels)?;
    fs::rename(tmp, path)
}

impl Capture {
    fn capture(&mut self, surface: &WlSurface, buffer: &WlBuffer) -> io::Result<()> {
        let scanout = self.scanouts.get(&surface.id()).copied().flatten();
        if scanout.is_none()
            && self
                .surfaces
                .get(&surface.id())
                .and_then(|s| s.parent.as_ref())
                .is_none()
        {
            return Ok(());
        }
        if matches!(self.output, Output::Snapshots(_))
            && scanout.is_some()
            && self
                .written
                .get(&surface.id())
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return Ok(());
        }
        let buffer = buffer
            .data::<shm::Buffer>()
            .ok_or_else(|| io::Error::other("expected SHM buffer"))?;
        let (width, height, stride) = (buffer.width, buffer.height, buffer.stride);
        let pixels = buffer.read_pixels()?;
        let (x, y) = self.surfaces[&surface.id()].position;
        let output = match &mut self.output {
            Output::Snapshots(path) => path,
            Output::Stream(sink) => {
                if let Some(id) = scanout {
                    sink.frame(id, width as u32, height as u32, pixels);
                } else {
                    sink.cursor(width as u32, height as u32, &pixels, x, y);
                }
                return Ok(());
            }
        };
        if let Some(id) = scanout {
            let mut rgb = Vec::with_capacity(width * height * 3);
            for p in pixels.chunks_exact(4) {
                rgb.extend_from_slice(&[p[2], p[1], p[0]]);
            }
            let name = format!("display-{id}-{width}x{height}.ppm");
            write_image(
                &output.join(&name),
                format!("P6\n{width} {height}\n255\n"),
                &rgb,
            )?;
            if self.written.insert(surface.id(), Instant::now()).is_none() {
                println!("scanout {id} {width}x{height} stride={stride}");
            }
        } else if pixels != self.cursor {
            let mut rgba = pixels.clone();
            for p in rgba.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            write_image(
                &output.join("cursor.pam"),
                format!(
                    "P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
                ),
                &rgba,
            )?;
            println!("cursor changed {width}x{height} position={},{}", x, y);
            self.cursor = pixels;
        }
        Ok(())
    }
}

enum Output {
    Snapshots(PathBuf),
    Stream(Box<dyn CaptureSink>),
}

/// Buffers use tightly packed four-byte BGRX (desktop) or BGRA (cursor) pixels.
pub trait CaptureSink {
    fn frame(&mut self, display: u32, width: u32, height: u32, pixels: Vec<u8>);
    fn cursor(&mut self, width: u32, height: u32, pixels: &[u8], x: i32, y: i32);
    fn stopped(&self) -> bool;
}
pub fn run(socket: PathBuf, sink: impl CaptureSink + 'static) -> Result<(), Box<dyn Error>> {
    let listener = ListeningSocket::bind_absolute(std::path::absolute(socket)?)?;
    serve(
        &listener,
        || listener.accept(),
        Output::Stream(Box::new(sink)),
    )
}

pub fn snapshots(socket: PathBuf, output: PathBuf) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&output)?;
    let listener = ListeningSocket::bind_absolute(std::path::absolute(socket)?)?;
    serve(&listener, || listener.accept(), Output::Snapshots(output))
}

fn serve(
    listener: &impl AsFd,
    accept: impl Fn() -> io::Result<Option<UnixStream>>,
    output: Output,
) -> Result<(), Box<dyn Error>> {
    let mut display = Display::<Capture>::new()?;
    let dh = display.handle();
    protocol::register(&dh);
    metadata::register(&dh);
    let mut state = Capture {
        surfaces: HashMap::new(),
        serial: 0,
        scanouts: HashMap::new(),
        written: HashMap::new(),
        cursor: Vec::new(),
        output,
        start: Instant::now(),
    };
    loop {
        let mut fds = [
            PollFd::new(listener, PollFlags::IN),
            PollFd::new(&display, PollFlags::IN),
        ];
        let timeout = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        let timeout = match &state.output {
            Output::Stream(sink) if sink.stopped() => return Ok(()),
            Output::Stream(_) => Some(&timeout),
            Output::Snapshots(_) => None,
        };
        if let Err(err) = poll(&mut fds, timeout) {
            if err == rustix::io::Errno::INTR {
                continue;
            }
            return Err(err.into());
        }
        while let Some(stream) = accept()? {
            display.handle().insert_client(stream, Arc::new(Guest))?;
        }
        display.dispatch_clients(&mut state)?;
        display.flush_clients()?;
    }
}
