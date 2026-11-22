use vvv_rdp::input;
mod resize;
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use vvv_rdp::{Encoder, Event, Receiver};
use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use wayland_capture::CaptureSink;

struct Desktop {
    rdp: Arc<Receiver>,
    input: Arc<Mutex<input::Input>>,
    displays: resize::Displays,
    managed: bool,
    stopped: Arc<AtomicBool>,
    frames: u64,
    output: PathBuf,
}
impl CaptureSink for Desktop {
    fn frame(&mut self, id: u32, w: u32, h: u32, pixels: Vec<u8>) {
        self.frames += 1;
        if self.frames == 1 || self.frames == 60 {
            if let Err(e) = snapshot(
                &self.output.join(format!("frame-{}.ppm", self.frames)),
                w,
                h,
                &pixels,
            ) {
                eprintln!("capture snapshot: {e}");
            }
        }
        if self.managed && !self.displays.accept(id, (w, h)) {
            return;
        }
        self.input.lock().unwrap().size = (w, h);
        self.rdp.frame(w, h, w * 4, Bytes::from(pixels));
    }
    fn cursor(&mut self, w: u32, h: u32, pixels: &[u8], x: i32, y: i32) {
        let (mx, my) = self.input.lock().unwrap().position;
        self.rdp.cursor(w, h, w * 4, pixels, x, y, mx, my);
    }
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}
fn snapshot(path: &Path, w: u32, h: u32, pixels: &[u8]) -> Result<()> {
    let mut out = BufWriter::new(fs::File::create(path)?);
    write!(out, "P6\n{w} {h}\n255\n")?;
    for pixel in pixels.chunks_exact(4) {
        out.write_all(&[pixel[2], pixel[1], pixel[0]])?;
    }
    out.flush()?;
    Ok(())
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let socket=PathBuf::from(args.next().context("usage: wayland-ironrdp-receiver SOCKET CAPTURE_DIR --mouse-sock PATH --keyboard-sock PATH [--nvenc] [--avg-bitrate BPS] [--rdp-port PORT] [--crosvm-sock PATH] [--crosvm-bin PATH]")?);
    let output = PathBuf::from(args.next().context("missing capture directory")?);
    let mut port = 3389;
    let mut cuda_socket = None;
    let mut nvenc = false;
    let mut avg_bitrate = cuda_scanout::DEFAULT_AVG_BITRATE;
    let (mut keyboard, mut tablet, mut ctrl) = (None, None, None);
    let mut bin = PathBuf::from("crosvm/target/debug/crosvm");
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("missing value for {flag}"))
        };
        match flag.as_str() {
            "--cuda-socket" => cuda_socket = Some(PathBuf::from(value()?)),
            "--rdp" => {}
            "--nvenc" | "--h264" => nvenc = true,
            "--avg-bitrate" => avg_bitrate = value()?.parse().context("invalid --avg-bitrate")?,
            "--rdp-port" => port = value()?.parse()?,
            "--mouse-sock" => tablet = Some(PathBuf::from(value()?)),
            "--keyboard-sock" => keyboard = Some(PathBuf::from(value()?)),
            "--crosvm-sock" => ctrl = Some(PathBuf::from(value()?)),
            "--crosvm-bin" => bin = value()?.into(),
            _ => bail!("unknown option {flag}"),
        }
    }
    fs::create_dir_all(&output)?;
    let input = Arc::new(Mutex::new(input::Input::new(
        &keyboard.context("missing --keyboard-sock")?,
        &tablet.context("missing --mouse-sock")?,
    )?));
    let displays = resize::Displays::default();
    let managed = ctrl.is_some();
    let (resize, worker) = ctrl.map(|socket| displays.worker(bin, socket)).unzip();
    let events = input.clone();
    let rdp = vvv_rdp::start(
        port,
        Arc::new(move |event, a, b| {
            if matches!(event, Event::Resize) {
                if let Some(tx) = &resize {
                    let _ = tx.send((a as u32, b as u32));
                }
            } else {
                events.lock().unwrap().event(event, a, b);
            }
        }),
        managed,
        if nvenc { Encoder::Nvenc(avg_bitrate) } else { Encoder::ClearCodec },
    )?;
    let rdp = Arc::new(rdp);
    if let Some(path) = cuda_socket {
        use std::os::unix::{fs::FileTypeExt, net::UnixStream};
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_socket())
            && UnixStream::connect(&path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let rdp = Arc::downgrade(&rdp);
        let input = input.clone();
        let displays = displays.clone();
        std::thread::Builder::new()
            .name("cuda-scanout".into())
            .spawn(move || {
                let mut last = None;
                for stream in listener.incoming() {
                    match stream
                        .map_err(anyhow::Error::from)
                        .and_then(cuda_scanout::receive)
                    {
                        Ok((id, f)) => {
                            let size = (u32::from(f.width), u32::from(f.height));
                            if managed && !displays.accept(id, size) {
                                continue;
                            }
                            if last != Some((id, size)) {
                                eprintln!("CUDA IPC scanout {id} {}x{}", size.0, size.1);
                                last = Some((id, size));
                            }
                            input.lock().unwrap().size = size;
                            let Some(receiver) = rdp.upgrade() else { break };
                            receiver.gpu_frame(f);
                        }
                        Err(e) => eprintln!("CUDA IPC frame: {e:#}"),
                    }
                }
            })?;
    }
    let stopped = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, stopped.clone())?;
    }
    println!("Rust IronRDP listening on port {port}");
    let result = wayland_capture::run(
        socket,
        Desktop {
            rdp,
            input,
            displays,
            managed,
            stopped,
            frames: 0,
            output,
        },
    );
    if let Some(thread) = worker {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("resize worker panicked"))?;
    }
    result.map_err(|e| anyhow::anyhow!("{e}"))
}
