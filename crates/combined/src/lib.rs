#[cfg(feature = "combined")]
mod combined;
mod cursor;
mod input;
mod resize;
mod virtio_input;

use anyhow::{Result, ensure};
use apple_screen_sharing::encode::Rect;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vvv_nvenc::capture::{DmabufBuffer, Frame};

use apple_screen_sharing::rfb::{self, AuthConfig, Config, srp::VerifierRecord};
use apple_screen_sharing::server::ExternalScreen;
use apple_screen_sharing::source;

#[derive(Parser)]
pub struct ServerConfig {
    /// New Unix socket for a separate crosvm --wayland-sock.
    #[arg(long)]
    pub socket: PathBuf,
    #[arg(long)]
    pub cuda_socket: Option<PathBuf>,
    /// Average NVENC video bitrate in bits per second per stream.
    #[arg(long, default_value_t = cuda_scanout::DEFAULT_AVG_BITRATE)]
    pub avg_bitrate: std::num::NonZeroU32,
    /// SRP verifier created with vvv-combined set-password.
    #[arg(long, conflicts_with = "dev_login")]
    pub srp_file: Option<PathBuf>,
    /// Development receiver: accept any viewer credentials using Apple DH login.
    #[arg(long)]
    pub dev_login: bool,
    #[arg(long, default_value = "127.0.0.1:5990")]
    pub listen: SocketAddr,
    #[arg(long, default_value_t = 5992)]
    pub media_port: u16,
    #[cfg(feature = "combined")]
    #[arg(long, default_value_t = 3389)]
    pub rdp_port: u16,
}

#[cfg(feature = "combined")]
type Badge = ((&'static str, usize), Arc<cuda_scanout::overlay::Stamp>);

struct Scanout {
    #[cfg(feature = "combined")]
    badge: Option<Badge>,
    seq: u64,
    captured: Duration,
    buffer: Arc<DmabufBuffer>,
}

#[cfg(feature = "combined")]
impl Scanout {
    fn badge_near(&self, pos: Option<(i32, i32)>) -> bool {
        self.badge.as_ref().is_some_and(|(_, s)| {
            !s.avoids(self.buffer.width as usize, self.buffer.height as usize, pos)
        })
    }
}

#[derive(Default)]
struct Screen {
    apple_layout: Mutex<Option<cuda_scanout::display::DisplayLayout>>,
    #[cfg(feature = "combined")]
    observer_motion: Arc<tokio::sync::Notify>,
    #[cfg(feature = "combined")]
    views: std::sync::OnceLock<tokio::sync::watch::Receiver<vvv_session::Views>>,
    #[cfg(feature = "combined")]
    seat: std::sync::OnceLock<Arc<vvv_session::ViewerSeat>>,
    #[cfg(feature = "combined")]
    rdp: std::sync::OnceLock<std::sync::Weak<vvv_rdp::Receiver>>,
    cursor: Mutex<Option<Arc<source::CursorImage>>>,
    pointer_position: Mutex<Option<(i32, i32)>>,
    displays: Mutex<resize::Handoff>,
    display_changed: Condvar,
    latest: Mutex<Option<Scanout>>,
    changed: Condvar,
    last_input: Mutex<Option<Instant>>,
}

#[cfg(feature = "combined")]
impl Drop for Screen {
    fn drop(&mut self) {
        self.observer_motion.notify_one();
    }
}

impl Screen {
    fn apple_display(&self, pixels: (u16, u16)) -> apple_screen_sharing::rfb::AppliedDisplay {
        let layout = *self.apple_layout.lock().unwrap();
        let points = layout
            .filter(|l| l.backing == pixels)
            .map_or(pixels, |l| l.logical);
        apple_screen_sharing::rfb::AppliedDisplay {
            pixels,
            points,
            scale: if points.0 == 0 {
                1.0
            } else {
                f64::from(pixels.0) / f64::from(points.0)
            },
        }
    }

    fn set_pointer(&self, pos: (i32, i32)) {
        if self.pointer_position.lock().unwrap().replace(pos) != Some(pos) {
            #[cfg(feature = "combined")]
            self.notify_observers();
        }
    }
    #[cfg(feature = "combined")]
    fn notify_observers(&self) {
        if self
            .views
            .get()
            .is_some_and(|v| v.borrow().status().is_some())
        {
            self.observer_motion.notify_one();
        }
    }

    fn commit(&self, id: u32, buffer: DmabufBuffer) {
        let mut handoff = self.displays.lock().unwrap();
        if !handoff.accepts(id, buffer.width, buffer.height) {
            return;
        }
        self.publish(buffer, apple_screen_sharing::capture::monotonic_now());
        if handoff.active != Some(id) {
            handoff.active = Some(id);
            handoff.pending = None;
            self.display_changed.notify_all();
        }
    }
    fn publish(&self, buffer: DmabufBuffer, captured: Duration) {
        let mut latest = self.latest.lock().unwrap();
        #[cfg(feature = "combined")]
        let (buffer, badge) = {
            let mut buffer = buffer;
            let desired = self.views.get().and_then(|r| r.borrow().status());
            let previous = latest.as_ref().and_then(|f| f.badge.as_ref());
            let badge = desired.map(|key| {
                let stamp = previous
                    .filter(|(old, _)| *old == key)
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| {
                        Arc::new(cuda_scanout::overlay::Stamp::status(key.0, key.1))
                    });
                let pointer = *self.pointer_position.lock().unwrap();
                let stamp = if stamp.avoids(buffer.width as usize, buffer.height as usize, pointer)
                {
                    stamp
                } else {
                    Arc::new(stamp.next_clear_corner(
                        buffer.width as usize,
                        buffer.height as usize,
                        pointer,
                    ))
                };
                (key, stamp)
            });
            if let Some((_, stamp)) = &badge
                && let Err(e) = buffer.stamp_status(stamp)
            {
                tracing::warn!("status composition failed: {e:#}");
                return;
            }
            if let Some(rdp) = self.rdp.get().and_then(|r| r.upgrade()) {
                combined::frame(&rdp, &buffer);
            }
            (buffer, badge)
        };
        let seq = latest.as_ref().map_or(1, |s| s.seq + 1);
        *latest = Some(Scanout {
            seq,
            captured,
            buffer: Arc::new(buffer),
            #[cfg(feature = "combined")]
            badge,
        });
        self.changed.notify_all();
    }
}

impl ExternalScreen<DmabufBuffer> for Screen {
    fn display(&self) -> apple_screen_sharing::rfb::AppliedDisplay {
        self.apple_display(self.size())
    }
    fn size(&self) -> (u16, u16) {
        self.latest
            .lock()
            .unwrap()
            .as_ref()
            .map_or((0, 0), |s| (s.buffer.width as u16, s.buffer.height as u16))
    }

    fn next_frame(&self, after: u64, timeout: Duration) -> Option<Frame> {
        let (latest, _) = self
            .changed
            .wait_timeout_while(self.latest.lock().unwrap(), timeout, |latest| {
                latest.as_ref().is_none_or(|s| s.seq == after)
            })
            .unwrap();
        let Scanout {
            seq,
            captured,
            buffer,
            ..
        } = latest.as_ref()?;
        // Keep the Mac's playout clock moving briefly after input on static desktops.
        // Real scanouts wake the condition immediately and always take priority.
        if *seq == after
            && (timeout < Duration::from_millis(16)
                || !self
                    .last_input
                    .lock()
                    .unwrap()
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(250)))
        {
            return None;
        }
        let now = Instant::now();
        Some(Frame {
            seq: *seq,
            buffer: buffer.clone(),
            damage: vec![Rect {
                x: 0,
                y: 0,
                width: buffer.width,
                height: buffer.height,
            }],
            // A new viewer samples the current desktop; repeats sample it again.
            presented: (after != 0 && *seq != after).then_some(*captured),
            requested_at: now,
            ready_at: now,
        })
    }
}

struct Display {
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
        if let Some(rdp) = self.screen.rdp.get().and_then(|r| r.upgrade()) {
            let (mx, my) = pointer.unwrap_or_default();
            rdp.cursor(w, h, w * 4, pixels, x, y, mx, my);
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

impl Display {
    fn start(
        path: PathBuf,
        screen: Arc<Screen>,
    ) -> Result<(Self, tokio::sync::oneshot::Receiver<String>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let sink = GuestCapture {
            hotspots: Default::default(),
            screen,
            stop: stop.clone(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("apple-capture".into())
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
            },
            rx,
        ))
    }
}
impl Drop for Display {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Protocol {
    Apple,
    #[cfg(feature = "combined")]
    Rdp,
    #[cfg(feature = "combined")]
    Both,
}

/// Serve the protocols enabled in this SDK build.
pub async fn serve(
    args: ServerConfig,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    #[cfg(feature = "combined")]
    let protocol = Protocol::Both;
    #[cfg(not(feature = "combined"))]
    let protocol = Protocol::Apple;
    serve_protocol(args, protocol, shutdown).await
}

/// Serve only the selected listeners on the shared guest.
pub async fn serve_protocol(
    args: ServerConfig,
    protocol: Protocol,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    #[cfg(feature = "combined")]
    let encoder = if protocol == Protocol::Rdp {
        vvv_rdp::Encoder::ClearCodec
    } else {
        vvv_rdp::Encoder::Nvenc(args.avg_bitrate)
    };
    serve_inner(
        args,
        protocol,
        #[cfg(feature = "combined")]
        encoder,
        shutdown,
    )
    .await
}

#[cfg(feature = "combined")]
pub async fn serve_with_rdp_encoder(
    args: ServerConfig,
    protocol: Protocol,
    encoder: vvv_rdp::Encoder,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    serve_inner(args, protocol, encoder, shutdown).await
}

async fn serve_inner(
    args: ServerConfig,
    protocol: Protocol,
    #[cfg(feature = "combined")] rdp_encoder: vvv_rdp::Encoder,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    #[cfg(feature = "combined")]
    let apple = protocol != Protocol::Rdp;
    #[cfg(not(feature = "combined"))]
    let apple = protocol == Protocol::Apple;
    let auth = if apple {
        ensure!(
            args.dev_login != args.srp_file.is_some(),
            "choose an SRP verifier or explicit development login"
        );
        ensure!(
            args.media_port > 0 && args.media_port <= 65533,
            "invalid media port"
        );
        Some(match &args.srp_file {
            Some(path) => AuthConfig {
                srp: Some(VerifierRecord::load(&path)?),
                dh: None,
            },
            None => {
                tracing::info!("development login enabled: accepting any viewer credentials");
                AuthConfig {
                    srp: None,
                    dh: Some(Arc::new(DevelopmentLogin)),
                }
            }
        })
    } else {
        None
    };
    let screen = Arc::new(Screen::default());
    let tablet = Arc::new(virtio_input::VirtioInput::bind(
        &args.socket.with_file_name("mouse.sock"),
    )?);
    let keyboard = Arc::new(virtio_input::VirtioInput::bind(
        &args.socket.with_file_name("keyboard.sock"),
    )?);
    let _cuda = args
        .cuda_socket
        .clone()
        .map(|path| CudaListener::start(path, screen.clone()))
        .transpose()?;
    let (_display, mut capture_end) = Display::start(args.socket.clone(), screen.clone())?;
    tracing::info!(socket = %args.socket.display(), "waiting for guest scanout");
    tokio::pin!(shutdown);
    while screen.size() == (0, 0) {
        tokio::select! { _ = &mut shutdown => return Ok(()),
            e = &mut capture_end => anyhow::bail!("capture ended: {e:?}"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
    }
    let (w, h) = screen.size();
    let display = Arc::new(resize::GuestDisplay::new(
        screen.clone(),
        args.socket.with_file_name("crosvm.sock"),
    ));
    #[cfg(feature = "combined")]
    let seat = Arc::new(vvv_session::ViewerSeat::default());
    #[cfg(feature = "combined")]
    combined::start_view_updates(&screen, &seat)?;
    #[cfg(feature = "combined")]
    let _rdp = if protocol != Protocol::Apple {
        Some(combined::start_rdp(
            &args,
            &screen,
            display.clone(),
            keyboard.clone(),
            tablet.clone(),
            seat.clone(),
            rdp_encoder,
        )?)
    } else {
        None
    };
    let Some(auth) = auth else {
        return tokio::select! { _ = shutdown => Ok(()), e = &mut capture_end => anyhow::bail!("capture ended: {e:?}") };
    };
    let mut cfg = Config::new("vvv", w, h, auth);
    #[cfg(feature = "combined")]
    let output: Arc<dyn ExternalScreen<DmabufBuffer>> =
        Arc::new(combined::AppleView::new(screen.clone()));
    #[cfg(not(feature = "combined"))]
    let output: Arc<dyn ExternalScreen<DmabufBuffer>> = screen.clone();
    vvv_apple::configure_video(
        &mut cfg,
        output,
        args.listen,
        args.media_port,
        args.avg_bitrate,
    )?;
    let cursors = screen.clone();
    let cursor_display = display.clone();
    cfg.cursor_source = Arc::new(move || {
        Box::new(cursor::GuestCursor::new(
            cursors.clone(),
            cursor_display.clone(),
        ))
    });
    let input_screen = screen.clone();
    let input_keyboard = keyboard.clone();
    let input_tablet = tablet.clone();
    cfg.input = Some(Arc::new(move |w, h| {
        Ok(Box::new(input::GuestInput::new(
            input_screen.clone(),
            input_keyboard.clone(),
            input_tablet.clone(),
            w,
            h,
        )))
    }));
    #[cfg(feature = "combined")]
    combined::configure_apple_seat(&mut cfg, display.clone(), seat);
    cfg.display = Some(display);
    tracing::info!(listen = %args.listen, w, h, media_port = args.media_port, "Apple NVENC receiver ready");
    tokio::select! { result = rfb::serve(args.listen, Arc::new(cfg), shutdown) => result,
    e = &mut capture_end => anyhow::bail!("capture ended: {e:?}"), }
}
struct DevelopmentLogin;

impl apple_screen_sharing::auth::Authenticator for DevelopmentLogin {
    fn verify(&self, _username: &str, _password: &[u8]) -> bool {
        true
    }
}
