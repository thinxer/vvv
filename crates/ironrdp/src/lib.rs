pub mod input;
pub use vvv_session::{Viewer, ViewerSeat, Views};
mod gfx;
mod h264;
mod nvenc;
use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use gfx::{Graphics, Submission};
pub use h264::Encoder;
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::*;
use std::{
    ffi::c_void,
    net::SocketAddr,
    num::{NonZeroU16, NonZeroUsize},
    sync::{Arc, Mutex},
    thread,
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
};
use tokio_rustls::{TlsAcceptor, rustls};

type Callback = extern "C" fn(*mut c_void, u32, i32, i32);
#[repr(u32)]
pub enum Event {
    Reset,
    Key,
    Unicode,
    Move,
    Button,
    Wheel,
    Resize,
    Relative,
}

#[derive(Clone)]
struct Session {
    seat: Option<Arc<ViewerSeat>>,
    viewer: Arc<std::sync::OnceLock<Viewer>>,
    pointer: Arc<Mutex<(i32, i32)>>,
    target: Arc<Mutex<Option<EventHandler>>>,
}

impl Session {
    fn layout(&self, layout: cuda_scanout::display::DisplayLayout) -> bool {
        let target = self.target.lock().unwrap();
        let Some(handler) = target.as_ref() else {
            return false;
        };
        eprintln!("RDP display {layout:?}");
        if self.seat.is_some() {
            return self.viewer.get().is_some_and(|v| v.layout(layout));
        }
        let (w, h) = layout.backing;
        handler(Event::Resize, w.into(), h.into());
        true
    }

    fn activate(&self) {
        if let Some(seat) = &self.seat {
            let target = self.target.lock().unwrap();
            if let Some(handler) = target.as_ref() {
                self.viewer.get_or_init(|| {
                    let release = handler.clone();
                    let resize = handler.clone();
                    seat.join(
                        "RDP",
                        move || release(Event::Reset, 0, 0),
                        move |layout| resize(Event::Resize, layout.backing.0.into(), layout.backing.1.into()),
                    )
                });
            }
        }
    }
    fn emit(&self, event: Event, a: i32, b: i32) {
        self.dispatch(event, a, b);
    }
    fn dispatch(&self, event: Event, a: i32, b: i32) -> bool {
        let target = self.target.lock().unwrap();
        let Some(handler) = target.as_ref() else {
            return false;
        };
        if self.seat.is_none() {
            handler(event, a, b);
            return true;
        }
        let Some(viewer) = self.viewer.get() else {
            return false;
        };
        match event {
            Event::Move => {
                *self.pointer.lock().unwrap() = (a, b);
                viewer.input(|| handler(event, a, b)).is_some()
            }
            Event::Relative => {
                let mut pos = self.pointer.lock().unwrap();
                *pos = (
                    pos.0.saturating_add(a).max(0),
                    pos.1.saturating_add(b).max(0),
                );
                viewer.input(|| handler(event, a, b)).is_some()
            }
            Event::Button => {
                let mask = 1 << (a - 272);
                viewer
                    .pointer(
                        mask,
                        if b != 0 { mask } else { 0 },
                        *self.pointer.lock().unwrap(),
                        || handler(event, a, b),
                    )
                    .is_some()
            }
            _ => viewer.input(|| handler(event, a, b)).is_some(),
        }
    }
    fn button(&self, button: MouseButton, pressed: bool) {
        let code = match button {
            MouseButton::Left => 272,
            MouseButton::Right => 273,
            MouseButton::Middle => 274,
            MouseButton::X1 => 275,
            MouseButton::X2 => 276,
            _ => return,
        };
        self.emit(Event::Button, code, i32::from(pressed));
    }

    fn retire(&self) {
        if let Some(handler) = self.target.lock().unwrap().take() {
            if let Some(viewer) = self.viewer.get() {
                viewer.leave();
            } else if self.seat.is_none() {
                handler(Event::Reset, 0, 0);
            }
        }
    }
}

// A wheel detent is 120 RDP units and 15 pixels in VirtioInputSocket.
fn wheel_pixels(units: i32) -> i32 {
    let pixels = units / 8;
    if pixels == 0 { units.signum() } else { pixels }
}

impl RdpServerInputHandler for Session {
    fn keyboard(&mut self, event: KeyboardEvent) {
        use KeyboardEvent::*;
        match event {
            Pressed { code, extended } | Released { code, extended } => self.emit(
                Event::Key,
                i32::from(code) | (i32::from(extended) << 8),
                i32::from(matches!(event, Pressed { .. })),
            ),
            UnicodePressed(ch) => self.emit(Event::Unicode, ch.into(), 0),
            UnicodeReleased(_) => (),
            Synchronize(_) => self.emit(Event::Reset, 0, 0),
        }
    }
    fn mouse(&mut self, event: MouseEvent) {
        use MouseEvent::*;
        match event {
            Move { x, y } => self.emit(Event::Move, x.into(), y.into()),
            RelMove { x, y } => self.emit(Event::Relative, x, y),
            VerticalScroll { value } => self.emit(Event::Wheel, 0, wheel_pixels(-i32::from(value))),
            Scroll { x, y } => self.emit(
                Event::Wheel,
                wheel_pixels(x),
                wheel_pixels(y.saturating_neg()),
            ),
            HorizontalScroll { value } => {
                self.emit(Event::Wheel, wheel_pixels(i32::from(value)), 0)
            }
            Button {
                x,
                y,
                button,
                pressed,
            } => {
                self.emit(Event::Move, x.into(), y.into());
                self.button(button, pressed);
            }
            ButtonRel {
                x,
                y,
                button,
                pressed,
            } => {
                self.emit(Event::Relative, x, y);
                self.button(button, pressed);
            }
            _ => (),
        }
    }
}

#[derive(Clone)]
enum Frame {
    Bitmap(BitmapUpdate),
    Gpu(Arc<cuda_scanout::Frame>),
}
impl Frame {
    fn dimensions(&self) -> (u16, u16) {
        match self {
            Self::Bitmap(f) => (f.width.get(), f.height.get()),
            Self::Gpu(f) => (f.width, f.height),
        }
    }
    fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Gpu(a), Self::Gpu(b)) => Arc::ptr_eq(a, b),
            (Self::Bitmap(a), Self::Bitmap(b)) => {
                a.width == b.width
                    && a.height == b.height
                    && a.stride == b.stride
                    && a.data == b.data
            }
            _ => false,
        }
    }
    fn fitted(&self, (w, h): (u16, u16), cursor: Option<&cuda_scanout::overlay::ObserverCursor>) -> Result<Frame> {
        match self {
            Self::Gpu(f) => Ok(Self::Gpu(Arc::new(f.fitted_with_cursor(w, h, cursor)?))),
            Self::Bitmap(f) => {
                let pixels = cuda_scanout::overlay::fit_bitmap_with_cursor(
                    &f.data,
                    f.stride.get(),
                    f.width.get().into(),
                    f.height.get().into(),
                    w.into(),
                    h.into(),
                    cursor,
                );
                Ok(Self::Bitmap(BitmapUpdate {
                    x: 0,
                    y: 0,
                    width: w.try_into()?,
                    height: h.try_into()?,
                    stride: (usize::from(w) * 4).try_into()?,
                    data: pixels.into(),
                    format: PixelFormat::BgrX32,
                }))
            }
        }
    }
    fn bitmap(&self) -> Result<BitmapUpdate> {
        match self {
            Self::Bitmap(f) => Ok(f.clone()),
            Self::Gpu(f) => Ok(BitmapUpdate {
                x: 0,
                y: 0,
                width: NonZeroU16::new(f.width).unwrap(),
                height: NonZeroU16::new(f.height).unwrap(),
                stride: NonZeroUsize::new(usize::from(f.width) * 4).unwrap(),
                format: PixelFormat::BgrX32,
                data: f.bgra()?.into(),
            }),
        }
    }
}
type Frames = watch::Receiver<Option<Frame>>;
type Cursor = watch::Receiver<DisplayUpdate>;
fn frame_size(f: &Frame) -> DesktopSize {
    DesktopSize {
        width: f.dimensions().0,
        height: f.dimensions().1,
    }
}

struct Display {
    gfx: Graphics,
    frames: Frames,
    cursor: Cursor,
    session: Session,
    size: Arc<Mutex<DesktopSize>>,
    initial: bool,
    managed: bool,
    encoder: Encoder,
}
impl Display {
    fn guest_size(&self, width: u16, height: u16) -> DesktopSize {
        // At the u16 limit use the closest representable even dimension.
        let align = |value: u16| {
            if self.encoder != Encoder::ClearCodec {
                value.saturating_add(1) & !1
            } else {
                value
            }
        };
        DesktopSize {
            width: align(width),
            height: align(height),
        }
    }
}
struct Updates {
    session: Session,
    fitted: Option<(Frame, (u16, u16), Option<(u64, (i32, i32))>, Frame)>,
    observing: bool,
    gfx: Graphics,
    gfx_ready: bool,
    retry_frame: bool,
    startup_resize_deadline: Option<tokio::time::Instant>,
    last_gfx_frame: Option<Frame>,
    select_cursor: bool,
    frames: Frames,
    cursor: Cursor,
    size: Arc<Mutex<DesktopSize>>,
}

#[async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        if std::mem::take(&mut self.select_cursor) {
            return Ok(Some(DisplayUpdate::CachedPointer(0)));
        }
        loop {
            tokio::select! {
                _ = async {
                    if let Some(deadline) = self.startup_resize_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    self.startup_resize_deadline = None;
                    self.frames.mark_changed();
                }
                changed = self.cursor.changed() => {
                    if changed.is_err() { return Ok(None); }
                    let shape = self.cursor.borrow_and_update().clone();
                    if self.session.viewer.get().is_some_and(Viewer::observing) {
                        return Ok(Some(DisplayUpdate::DefaultPointer));
                    }
                    self.select_cursor = matches!(shape, DisplayUpdate::ColorPointer(_));
                    return Ok(Some(shape))
                }
                _ = self.gfx.wake.notified() => {
                    let ready = self.gfx.ready();
                    if ready { self.startup_resize_deadline = None; }
                    if ready != self.gfx_ready { self.last_gfx_frame = None; }
                    if ready != self.gfx_ready || self.retry_frame {
                        self.frames.mark_changed();
                    }
                    self.gfx_ready = ready;
                }
                changed = self.frames.changed() => {
                    if changed.is_err() { return Ok(None); }
                    let latest = self.frames.borrow_and_update().clone();
                    if let Some(mut frame) = latest {
                        let observing = self.session.viewer.get().is_some_and(Viewer::observing);
                        if self.observing != observing {
                            self.observing = observing;
                            self.cursor.mark_changed();
                        }
                        if let Some(canvas) = self.session.viewer.get().and_then(|v|v.canvas(frame.dimensions())) {
                            let cursor = self.session.seat.as_ref().filter(|_| observing)
                                .and_then(|s| s.observer_cursor.lock().unwrap().clone());
                            let key = cursor.as_ref().map(|c| c.key());
                            if let Some((src,size,previous,cached))=&self.fitted
                                && *size==canvas && *previous==key && src.unchanged(&frame) { frame=cached.clone(); }
                            else {
                                let composed=frame.fitted(canvas, cursor.as_ref()).map_err(|e| ServerError::custom("fit observer frame",std::io::Error::other(e)))?;
                                self.fitted=Some((frame,canvas,key,composed.clone()));
                                frame=composed;
                            }
                        } else { self.fitted=None; }
                        if self.last_gfx_frame.as_ref().is_some_and(|last|last.unchanged(&frame)) {
                            self.retry_frame = false;
                            continue;
                        }
                        match self.gfx.submit(&frame, self.last_gfx_frame.as_ref()).map_err(|e| ServerError::custom("submit frame", std::io::Error::other(e)))? {
                            Submission::Bitmap => {
                                // EGFX resets its own output and surface. Only legacy
                                // bitmap clients need a core RDP reactivation on resize.
                                let actual = frame_size(&frame);
                                let mut size = self.size.lock().unwrap();
                                if (size.width, size.height) != (actual.width, actual.height) {
                                    // A core reactivation during DRDYNVC startup can prevent
                                    // both graphics and display-control channels from opening.
                                    if self.startup_resize_deadline.is_some() { continue; }
                                    *size = actual;
                                    return Ok(Some(DisplayUpdate::Resize(actual)));
                                }
                                return Ok(Some(DisplayUpdate::Bitmap(frame.bitmap().map_err(|e|ServerError::custom("bitmap readback",std::io::Error::other(e)))?)));
                            }
                            Submission::Sent => { self.retry_frame = false; self.last_gfx_frame = Some(frame); },
                            Submission::Busy => self.retry_frame = true,
                        }

                    }
                }
            }
        }
    }
}

#[async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        *self.size.lock().unwrap()
    }
    async fn request_initial_size(&mut self, size: DesktopSize) -> DesktopSize {
        self.session.activate();
        if self.initial && self.managed {
            self.initial = false;
            let client_size = size;
            let size = self.guest_size(size.width, size.height);
            if !self
                .session
                .layout(cuda_scanout::display::DisplayLayout::from_rdp(
                    (size.width, size.height),
                    None,
                ))
            {
                // The acceptor honored the client's dimensions. Track that wire geometry
                // until EGFX resets the output or bitmap delivery reactivates at guest size.
                *self.size.lock().unwrap() = client_size;
                return client_size;
            }
            *self.size.lock().unwrap() = size;
            // Allow crosvm's hotplug/debounce to deliver the requested frame. On timeout
            // the update stream renegotiates the actual guest size before sending pixels.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.frames.wait_for(|frame| {
                    frame
                        .as_ref()
                        .is_some_and(|f| f.dimensions() == (size.width, size.height))
                }),
            )
            .await;
        }
        self.size().await
    }
    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        self.session.activate();
        let mut frames = self.frames.clone();
        let mut cursor = self.cursor.clone();
        frames.mark_changed();
        cursor.mark_changed();
        Ok(Box::new(Updates {
            session: self.session.clone(),
            fitted: None,
            observing: false,
            gfx: self.gfx.clone(),
            gfx_ready: self.gfx.ready(),
            retry_frame: false,
            startup_resize_deadline: self
                .session
                .seat
                .as_ref()
                .filter(|_| !self.gfx.ready())
                .map(|_| tokio::time::Instant::now() + std::time::Duration::from_secs(5)),
            last_gfx_frame: None,
            select_cursor: false,
            frames,
            cursor,
            size: self.size.clone(),
        }))
    }
    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        if let Some(primary) = layout.monitors().iter().find(|m| m.is_primary()) {
            let (w, h) = primary.dimensions();
            if let (Ok(w), Ok(h)) = (u16::try_from(w), u16::try_from(h)) {
                let size = self.guest_size(w, h);
                self.session
                    .layout(cuda_scanout::display::DisplayLayout::from_rdp(
                        (size.width, size.height),
                        primary.device_scale_factor().and(primary.desktop_scale_factor()),
                    ));
            }
        }
    }
}

pub type EventHandler = Arc<dyn Fn(Event, i32, i32) + Send + Sync>;
pub struct Receiver {
    frames: watch::Sender<Option<Frame>>,
    cursor: watch::Sender<DisplayUpdate>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

async fn serve(
    listener: std::net::TcpListener,
    tls: TlsAcceptor,
    frame_rx: Frames,
    cursor_rx: Cursor,
    handler: EventHandler,
    managed: bool,
    encoder: Encoder,
    mut stopped: oneshot::Receiver<()>,
    seat: Option<Arc<ViewerSeat>>,
) -> Result<()> {
    let addr = listener.local_addr()?;
    let listener = TcpListener::from_std(listener)?;
    let mut peers = tokio::task::JoinSet::new();
    loop {
        let (stream, peer) = tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => accepted?,
            Some(_) = peers.join_next(), if !peers.is_empty() => continue,
        };
        if seat.is_none() {
            peers.abort_all();
            while peers.join_next().await.is_some() {}
        }
        let (frame_rx, cursor_rx, tls, handler, seat) = (
            frame_rx.clone(),
            cursor_rx.clone(),
            tls.clone(),
            handler.clone(),
            seat.clone(),
        );
        peers.spawn_local(async move {
        if let Err(error) = stream.set_nodelay(true) {
            eprintln!("IronRDP peer {peer}: {error}");
            return;
        }
        eprintln!("IronRDP peer {peer} connected");
        let session = Session {
            seat,
            viewer: Default::default(),
            pointer: Default::default(),
            target: Arc::new(Mutex::new(Some(handler.clone()))),
        };
        struct Retire(Session);
        impl Drop for Retire { fn drop(&mut self) { self.0.retire(); } }
        let _retire = Retire(session.clone());
        let size = frame_rx
            .borrow()
            .as_ref()
            .map(frame_size)
            .unwrap_or(DesktopSize {
                width: 1280,
                height: 720,
            });
        let gfx = Graphics::new(encoder);
        let display = Display {
            gfx: gfx.clone(),
            frames: frame_rx.clone(),
            cursor: cursor_rx.clone(),
            session: session.clone(),
            size: Arc::new(Mutex::new(size)),
            initial: true,
            managed,
            encoder,
        };
        let mut server = RdpServer::builder()
            .with_addr(addr)
            .with_tls(tls.clone())
            .with_input_handler(session.clone())
            .with_display_handler(display)
            .with_gfx_factory(Some(Box::new(gfx)))
            .with_honor_client_desktop_size(managed.then_some(DesktopSize {
                width: 8192,
                height: 8192,
            }))
            .build();
        tokio::select! {
            _ = async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if session.seat.is_none() || session.viewer.get().is_some() {
                    std::future::pending::<()>().await;
                }
            } => (),
            res = server.run_connection(stream) => if let Err(error) = res { eprintln!("IronRDP peer {peer}: {error:#}"); },
        }
        });
    }
    peers.abort_all();
    while peers.join_next().await.is_some() {}
    Ok(())
}

pub fn start(
    port: u16,
    handler: EventHandler,
    managed: bool,
    encoder: Encoder,
) -> Result<Receiver> {
    start_on(
        SocketAddr::from(([0, 0, 0, 0], port)),
        handler,
        managed,
        encoder,
        None,
    )
}

pub fn start_on(
    addr: SocketAddr,
    handler: EventHandler,
    managed: bool,
    encoder: Encoder,
    seat: Option<Arc<ViewerSeat>>,
) -> Result<Receiver> {
    let listener = std::net::TcpListener::bind(addr).context("bind RDP listener")?;
    listener.set_nonblocking(true)?;
    let cert = rcgen::generate_simple_self_signed(vec!["vvv-streamer".into()])?;
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert.cert.der().clone()], key.into())?;
    let tls = TlsAcceptor::from(Arc::new(tls));
    let (frames, frame_rx) = watch::channel::<Option<Frame>>(None);
    let (cursor, cursor_rx) = watch::channel(DisplayUpdate::DefaultPointer);
    let (stop, stopped) = oneshot::channel();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let worker = thread::Builder::new()
        .name("ironrdp".into())
        .spawn(move || {
            let result = tokio::task::LocalSet::new().block_on(
                &rt,
                serve(
                    listener, tls, frame_rx, cursor_rx, handler, managed, encoder, stopped, seat,
                ),
            );
            if let Err(error) = result {
                eprintln!("IronRDP listener: {error:#}");
            }
        })?;
    Ok(Receiver {
        frames,
        cursor,
        stop: Some(stop),
        worker: Some(worker),
    })
}

#[unsafe(no_mangle)]
extern "C" fn iron_start(
    port: u16,
    ctx: *mut c_void,
    callback: Callback,
    managed: bool,
    encoder: u8,
) -> *mut Receiver {
    let encoder = match encoder {
        0 => Encoder::ClearCodec,
        1 => Encoder::Nvenc(cuda_scanout::DEFAULT_AVG_BITRATE),
        _ => return std::ptr::null_mut(),
    };
    let address = ctx as usize;
    match start(
        port,
        Arc::new(move |event, a, b| callback(address as *mut c_void, event as u32, a, b)),
        managed,
        encoder,
    ) {
        Ok(server) => Box::into_raw(Box::new(server)),
        Err(error) => {
            eprintln!("IronRDP startup: {error:#}");
            std::ptr::null_mut()
        }
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn iron_stop(server: *mut Receiver) {
    drop(unsafe { Box::from_raw(server) });
}
impl Drop for Receiver {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn iron_frame(
    server: *const Receiver,
    w: u32,
    h: u32,
    stride: u32,
    data: *const u8,
) {
    if w == 0 || h == 0 || w > 8192 || h > 8192 || stride < w * 4 || data.is_null() {
        return;
    }
    let len = stride as usize * (h as usize - 1) + w as usize * 4;
    let bytes = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    unsafe { &*server }.frame(w, h, stride, bytes);
}
impl Receiver {
    pub fn gpu_frame(&self, frame: cuda_scanout::Frame) {
        self.shared_gpu_frame(Arc::new(frame));
    }
    pub fn shared_gpu_frame(&self, frame: Arc<cuda_scanout::Frame>) {
        self.frames.send_replace(Some(Frame::Gpu(frame)));
    }
    pub fn frame(&self, w: u32, h: u32, stride: u32, bytes: Bytes) {
        if w == 0
            || h == 0
            || w > 8192
            || h > 8192
            || stride < w * 4
            || bytes.len() < stride as usize * (h as usize - 1) + w as usize * 4
        {
            return;
        }
        self.frames.send_replace(Some(Frame::Bitmap(BitmapUpdate {
            x: 0,
            y: 0,
            width: NonZeroU16::new(w as u16).unwrap(),
            height: NonZeroU16::new(h as u16).unwrap(),
            format: PixelFormat::BgrX32,
            data: bytes,
            stride: NonZeroUsize::new(stride as usize).unwrap(),
        })));
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn iron_cursor(
    server: *const Receiver,
    w: u32,
    h: u32,
    stride: u32,
    data: *const u8,
    origin_x: i32,
    origin_y: i32,
    mouse_x: i32,
    mouse_y: i32,
) {
    let bytes = if w == 0 || h == 0 || data.is_null() {
        &[][..]
    } else {
        if w > 384 || h > 384 || stride < w * 4 {
            return;
        }
        unsafe {
            std::slice::from_raw_parts(data, stride as usize * (h as usize - 1) + w as usize * 4)
        }
    };
    unsafe { &*server }.cursor(w, h, stride, bytes, origin_x, origin_y, mouse_x, mouse_y);
}
impl Receiver {
    pub fn refresh_views(&self) {
        self.frames.send_modify(|_| {});
    }
    pub fn cursor(
        &self,
        w: u32,
        h: u32,
        stride: u32,
        bytes: &[u8],
        origin_x: i32,
        origin_y: i32,
        mouse_x: i32,
        mouse_y: i32,
    ) {
        let tx = &self.cursor;
        if w == 0 || h == 0 || bytes.is_empty() {
            tx.send_replace(DisplayUpdate::HidePointer);
            return;
        }
        if w > 384
            || h > 384
            || stride < w * 4
            || bytes.len() < stride as usize * (h as usize - 1) + w as usize * 4
        {
            return;
        }
        let mut left = w;
        let mut top = h;
        let (mut right, mut bottom) = (0, 0);
        for y in 0..h {
            for x in 0..w {
                if bytes[y as usize * stride as usize + x as usize * 4 + 3] >= 64 {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x);
                    bottom = bottom.max(y);
                }
            }
        }
        if left > right {
            tx.send_replace(DisplayUpdate::HidePointer);
            return;
        }
        // The color-pointer capability is always negotiated. Fit the visible glyph
        // into a standard pointer, discarding transparent padding on KDE's cursor plane.
        let width = right - left + 1;
        let height = bottom - top + 1;
        let extent = width.max(height).max(32);
        let cw = (width * 32).div_ceil(extent);
        let ch = (height * 32).div_ceil(extent);
        let mut xor_mask = vec![0; 32 * 32 * 3];
        let mut and_mask = vec![0xff; 32 * 4];
        for y in 0..ch {
            for x in 0..cw {
                let sx = left + x * width / cw;
                let sy = top + y * height / ch;
                let src = sy as usize * stride as usize + sx as usize * 4;
                if bytes[src + 3] < 64 {
                    continue;
                }
                let pos = ((31 - y) * 32 + x) as usize;
                xor_mask[pos * 3..pos * 3 + 3].copy_from_slice(&bytes[src..src + 3]);
                and_mask[pos / 8] &= !(0x80 >> (pos % 8));
            }
        }
        // KDE may report the cursor origin in logical pixels on a HiDPI output.
        let hotspot = [1_i64, 2]
            .into_iter()
            .map(|scale| {
                (
                    i64::from(mouse_x) - scale * i64::from(origin_x),
                    i64::from(mouse_y) - scale * i64::from(origin_y),
                )
            })
            .find(|&(x, y)| {
                (i64::from(left)..=i64::from(right)).contains(&x)
                    && (i64::from(top)..=i64::from(bottom)).contains(&y)
            });
        let (hx, hy) = hotspot.unwrap_or_else(|| {
            if left >= 16 && top >= 16 {
                (i64::from((left + right) / 2), i64::from((top + bottom) / 2))
            } else {
                (i64::from(left), i64::from(top))
            }
        });
        let shape = ColorPointer {
            cache_index: 0,
            width: 32,
            height: 32,
            hot_x: ((hx as u32 - left) * cw / width) as u16,
            hot_y: ((hy as u32 - top) * ch / height) as u16,
            and_mask,
            xor_mask,
        };
        tx.send_if_modified(|previous| {
            if matches!(previous,DisplayUpdate::ColorPointer(old)
            if old.xor_mask==shape.xor_mask && old.and_mask==shape.and_mask)
            {
                return false;
            }
            *previous = DisplayUpdate::ColorPointer(shape);
            true
        });
    }
}
