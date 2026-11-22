// Modified for vvv from WayShare. SPDX-License-Identifier: Apache-2.0
//! The High Performance media pipeline: capture, encode, packetize, SRTP, and RTCP handling.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use tracing::{debug, info, warn};

use crate::capture::FrameBuffer;

use crate::capture::monotonic_now;
use crate::encode::{Encoder, EncoderFactory, TileLayout};
use crate::hp::packetizer::{Packetizer, PacketizerConfig, rtp_timestamp};
use crate::hp::ratecontrol::{RateConfig, RateController, RateFeedback};
use crate::hp::rtcp::{self, RtcpEvent};
use crate::hp::srtp::{self, SrtpContext};
use crate::rfb::media::KEY_LEN;

mod ack_timing;
use ack_timing::SentTiles;
use std::time::SystemTime;

const FIRST_RTCP_TIMEOUT: Duration = Duration::from_secs(15);
const RTCP_SILENCE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RTCP_BATCH: usize = 32;
/// The viewer repeats FIR (one per tile, every ~250 ms) until a refresh decodes; one refresh
/// per window answers them all.
const FIR_DEDUPE: Duration = Duration::from_millis(500);
/// A 4K refresh is several MB sent back to back. Linux caps this at `net.core.wmem_max`.
const SEND_BUFFER: usize = 8 << 20;

pub(super) struct MediaParams<B: FrameBuffer> {
    pub(super) bind: SocketAddr,
    pub(super) external: Arc<dyn super::ExternalScreen<B>>,
    pub(super) peer: SocketAddr,
    pub(super) port: u16,
    pub(super) encoder: Arc<dyn EncoderFactory<B>>,
    /// hp.bitrate was configured: use it as the rate controller's starting point.
    pub(super) fixed_bitrate: Option<u64>,
    pub(super) ssrc: u32,
    pub(super) remote_ssrc: u32,
    pub(super) payload_type: u8,
    pub(super) stream_size: (u32, u32),
    pub(super) tiles: u8,
    pub(super) send_key: [u8; KEY_LEN],
    pub(super) recv_key: [u8; KEY_LEN],
}

/// Main-thread counters (capture, RTCP).
#[derive(Default)]
struct Stats {
    frames: u64,
    firs: u64,
    acks: u64,
    ack_ms: Vec<f64>,
    report_rtt_ms: Vec<f64>,
    last_ack_arrival: Option<SystemTime>,
    ack_bursts: usize,
    burst_gaps_ms: Vec<f64>,
    unmatched: u64,
    other_rtcp: u64,
    /// Latest receiver feedback (RCTL): bandwidth estimate and one-way relative delay.
    feedback: Option<RateFeedback>,
    last_refresh: Option<Instant>,
    /// Time from the capture being ready to `submit` returning (waiting for the encoder,
    /// diff and conversion).
    submit_ms: Vec<f64>,
    /// Compositor presentation to the captured frame reaching us.
    present_to_ready_ms: Vec<f64>,
    /// Captured frames replaced by a newer one while the encoder was busy.
    dropped: u64,
}

struct RtcpFeedback {
    rx: SrtpContext,
    last_received: Option<Instant>,
    received: u64,
    valid: u64,
    non_rtcp: u64,
    unprotect_failed: u64,
    malformed: u64,
    recv_errors: u64,
    last_recv_error: Option<std::io::ErrorKind>,
}

impl RtcpFeedback {
    fn new(key: &[u8; KEY_LEN]) -> Self {
        Self {
            rx: SrtpContext::new(key),
            last_received: None,
            received: 0,
            valid: 0,
            non_rtcp: 0,
            unprotect_failed: 0,
            malformed: 0,
            recv_errors: 0,
            last_recv_error: None,
        }
    }

    fn accept(&mut self, mut pkt: Vec<u8>, now: Instant) -> Vec<RtcpEvent> {
        self.received += 1;
        if !srtp::is_rtcp(&pkt) {
            self.non_rtcp += 1;
            return Vec::new();
        }
        if let Err(e) = self.rx.unprotect_rtcp(&mut pkt) {
            self.unprotect_failed += 1;
            if self.unprotect_failed == 1 {
                warn!(
                    "SRTCP unprotect failed: {e:#}; first occurrence, cumulative count in HP stats"
                );
            }
            return Vec::new();
        }
        let events = match rtcp::parse(&pkt) {
            Ok(events) => events,
            Err(e) => {
                self.malformed += 1;
                if self.malformed == 1 {
                    warn!(
                        "RTCP parse failed: {e:#}; first occurrence, cumulative count in HP stats"
                    );
                }
                return Vec::new();
            }
        };
        if self.last_received.is_none() {
            info!("first RTCP from viewer");
        }
        self.last_received = Some(now);
        self.valid += 1;
        events
    }

    fn drain(&mut self, sock: &UdpSocket) -> Vec<(RtcpEvent, Option<SystemTime>)> {
        let mut events = Vec::new();
        let mut buf = [0u8; 2048];
        // A continuously readable socket must leave time for capture, timeout and stop checks.
        for _ in 0..MAX_RTCP_BATCH {
            match ack_timing::receive(sock, &mut buf) {
                Ok((n, arrival)) => events.extend(
                    self.accept(buf[..n].to_vec(), Instant::now())
                        .into_iter()
                        .map(|ev| (ev, arrival)),
                ),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.recv_errors += 1;
                    self.last_recv_error = Some(e.kind());
                    break;
                }
            }
        }
        events
    }

    fn check_timeout(&self, start: Instant, now: Instant) -> Result<()> {
        let (since, timeout, phase) = match self.last_received {
            Some(t) => (t, RTCP_SILENCE_TIMEOUT, "after feedback"),
            None => (start, FIRST_RTCP_TIMEOUT, "initial feedback"),
        };
        ensure!(
            now.duration_since(since) <= timeout,
            "no valid video RTCP for {timeout:?} ({phase}); HP stream totals: received={} valid={} non_rtcp={} unprotect_failed={} malformed={} recv_errors={} last_recv_error={:?}",
            self.received,
            self.valid,
            self.non_rtcp,
            self.unprotect_failed,
            self.malformed,
            self.recv_errors,
            self.last_recv_error,
        );
        Ok(())
    }
}

/// Counters kept by the send thread.
#[derive(Default)]
struct SendStats {
    frames_window: u64,
    bytes_window: u64,
    send_errors: u64,
    present_to_first_ms: Vec<f64>,
    present_to_sent_ms: Vec<f64>,
    diff_ms: Vec<f64>,
    convert_ms: Vec<f64>,
    encode_ms: Vec<f64>,
    send_ms: Vec<f64>,
    failed: Option<String>,
}

fn pct(v: &mut [f64], q: f64) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(((v.len() as f64 - 1.0) * q) as usize)
        .map(|x| (x * 10.0).round() / 10.0)
}

struct Sender {
    sock: UdpSocket,
    pk: Packetizer,
    tx: SrtpContext,
    ssrc: u32,
    remote_ssrc: u32,
    tiles: usize,
    start: Instant,
    ts_offset: u32,
    frame_ts: u32,
    frame_send_time: Duration,
    packets: u32,
    octets: u32,
    last_sr: Instant,
    stats: Arc<Mutex<SendStats>>,
    send_errors_logged: u32,
    sent: Arc<Mutex<SentTiles>>,
}

impl Sender {
    /// Packetizes, encrypts and sends coded frames as the encoder finishes them, so the next
    /// frame's capture and submission never wait on the network path.
    fn run(mut self, frames: std::sync::mpsc::Receiver<Result<crate::encode::EncodedFrame>>) {
        loop {
            match frames.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(out)) => self.send_frame(out),
                Ok(Err(e)) => {
                    self.stats.lock().unwrap().failed = Some(format!("{e:#}"));
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if self.last_sr.elapsed() >= Duration::from_secs(1) {
                let ntp = rtcp::ntp_now();
                let mut sr = rtcp::sender_report(
                    self.ssrc,
                    self.remote_ssrc,
                    ntp,
                    rtp_timestamp(self.start.elapsed(), self.ts_offset),
                    self.packets,
                    self.octets,
                );
                if self.tx.protect_rtcp(&mut sr).is_ok() {
                    let at = SystemTime::now();
                    if self.sock.send(&sr).is_ok() {
                        self.sent.lock().unwrap().remember_report(
                            self.ssrc,
                            (ntp >> 16) as u32,
                            at,
                        );
                    }
                }
                self.last_sr = Instant::now();
            }
        }
    }

    fn send_frame(&mut self, out: crate::encode::EncodedFrame) {
        let t0 = Instant::now();
        let mut don = self.pk.next_don();
        let first = out.strip.is_none_or(|part| part.first);
        let last = out.strip.is_none_or(|part| part.last);
        if first {
            let elapsed = self.start.elapsed();
            let age = out.presented.map_or(Duration::ZERO, |capture| {
                monotonic_now().saturating_sub(capture)
            });
            self.frame_ts = rtp_timestamp(elapsed.saturating_sub(age), self.ts_offset);
            self.frame_send_time = Duration::ZERO;
        }
        let mut first_sent = None;
        let (mut bytes, mut errors) = (0u64, 0u64);
        let emit = |mut pkt: Vec<u8>| {
            if self.tx.protect_rtp(&mut pkt).is_err() {
                errors += 1;
                return;
            }
            self.packets = self.packets.wrapping_add(1);
            self.octets = self.octets.wrapping_add(pkt.len() as u32);
            bytes += pkt.len() as u64;
            // The marker is the last packet of this tile. Record only successful sends.
            let marker = pkt[1] & 0x80 != 0;
            let mut ledger = marker.then(|| self.sent.lock().unwrap());
            let sent_at = ledger.as_ref().map(|_| SystemTime::now());
            let result = self.sock.send(&pkt);
            if first && first_sent.is_none() && result.is_ok() {
                first_sent = Some(monotonic_now());
            }
            if let Some(tracker) = ledger.as_mut() {
                if result.is_ok() {
                    tracker.sent(don, sent_at.unwrap());
                }
                don = don.wrapping_add(1);
            }
            if let Err(e) = result {
                errors += 1;
                if errors == 1 && self.send_errors_logged < 10 {
                    self.send_errors_logged += 1;
                    warn!(
                        len = pkt.len(),
                        frame_bytes = out.tiles.iter().map(|t| t.data.len()).sum::<usize>(),
                        "HP video send failed: {e}"
                    );
                }
            }
        };
        if let Some(part) = out.strip {
            let t = &out.tiles[0];
            self.pk.packetize_strip(
                t.index.into(),
                &t.data,
                self.frame_ts,
                part.refresh,
                part.last,
                emit,
            );
        } else {
            let mut tiles = vec![None; self.tiles];
            for t in &out.tiles {
                if let Some(slot) = tiles.get_mut(t.index as usize) {
                    *slot = Some(t.data.as_slice());
                }
            }
            self.pk.packetize_each(&tiles, self.frame_ts, emit);
        }
        self.frame_send_time += t0.elapsed();
        let now = monotonic_now();
        let mut st = self.stats.lock().unwrap();
        st.bytes_window += bytes;
        st.send_errors += errors;
        if let (Some(capture), Some(sent)) = (out.presented, first_sent) {
            st.present_to_first_ms
                .push(sent.saturating_sub(capture).as_secs_f64() * 1e3);
        }
        if !last {
            return;
        }
        st.frames_window += 1;
        if let Some(p) = out.presented.filter(|&p| now > p) {
            st.present_to_sent_ms.push((now - p).as_secs_f64() * 1e3);
        }
        st.diff_ms.push(out.timings.diff.as_secs_f64() * 1e3);
        st.convert_ms.push(out.timings.convert.as_secs_f64() * 1e3);
        st.encode_ms.push(out.timings.encode.as_secs_f64() * 1e3);
        st.send_ms.push(self.frame_send_time.as_secs_f64() * 1e3);
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum MediaEnd {
    Stopped,
    /// No frame at the new size has been sent to the viewer.
    OutputResized {
        width: u32,
        height: u32,
        first_frame_mismatch: bool,
    },
}

/// Query the connected socket so tunnel routes use their actual MTU.
fn video_payload_limit(sock: &UdpSocket, peer: SocketAddr) -> usize {
    use std::os::fd::AsRawFd;
    let v4 = peer.ip().to_canonical().is_ipv4();
    let (level, option) = if sock.local_addr().is_ok_and(|a| a.is_ipv6()) {
        (libc::IPPROTO_IPV6, libc::IPV6_MTU)
    } else {
        (libc::IPPROTO_IP, libc::IP_MTU)
    };
    let mut mtu: libc::c_int = 0;
    let mut len = std::mem::size_of_val(&mtu) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            level,
            option,
            (&mut mtu as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    let path_mtu = if result == 0 && mtu >= 128 {
        mtu as usize
    } else {
        1280
    };
    // The packetizer adds five FU bytes after splitting the NAL into chunks.
    let overhead = (if v4 { 20 } else { 40 }) + 8 + 20 + srtp::TAG_LEN + 5;
    (path_mtu - overhead).min(crate::hp::packetizer::DEFAULT_MTU_PAYLOAD)
}

pub(super) fn run_media<B: FrameBuffer>(p: MediaParams<B>, stop: &AtomicBool) -> Result<MediaEnd> {
    let sock = UdpSocket::bind(p.bind).with_context(|| format!("binding UDP {}", p.port))?;
    sock.connect(p.peer)?;
    sock.set_nonblocking(true)?;
    let local = sock.local_addr()?;
    let peer = sock.peer_addr()?;
    let sref = socket2::SockRef::from(&sock);
    if let Err(e) = sref.set_send_buffer_size(SEND_BUFFER) {
        warn!("SO_SNDBUF {SEND_BUFFER}: {e}");
    }
    info!(
        local_bind = %local,
        peer = %peer,
        port = p.port,
        sndbuf = sref.send_buffer_size().ok(),
        "HP media socket ready"
    );

    let mut cap = Source {
        screen: p.external.clone(),
        last: 0,
    };
    let Some(first) = first_frame(&mut cap, stop)? else {
        return Ok(MediaEnd::Stopped);
    };
    let (w, h) = first.buffer.size();
    if (w, h) != p.stream_size {
        return Ok(MediaEnd::OutputResized {
            width: w,
            height: h,
            first_frame_mismatch: true,
        });
    }
    let layout = TileLayout::new(w, h);
    let mut enc = p.encoder.open(layout)?;
    ensure!(
        enc.layout() == layout,
        "encoder returned a different HEVC strip layout"
    );
    let mut rate_cfg = RateConfig::default();
    if let Some(bps) = p.fixed_bitrate {
        rate_cfg.start_bps = bps.min(u32::MAX as u64) as u32;
        rate_cfg.min_bps = rate_cfg.start_bps;
        rate_cfg.max_bps = rate_cfg.start_bps;
    }
    let mut rc = RateController::new(rate_cfg);
    enc.set_bitrate(rc.target_bps() as u64);
    ensure!(
        u32::from(p.tiles) == crate::encode::TILE_COUNT as u32,
        "answer has {} tiles, encoder produces {}",
        p.tiles,
        crate::encode::TILE_COUNT
    );
    let mut rx = RtcpFeedback::new(&p.recv_key);
    info!(width = w, height = h, layout = ?enc.layout(), "HP encoder ready");

    let start = Instant::now();
    let send_stats = Arc::new(Mutex::new(SendStats::default()));
    let (frame_tx, frame_rx) = std::sync::mpsc::channel();
    enc.set_stream_output(frame_tx.clone());
    ack_timing::enable(&sock)?;
    let sent = Arc::new(Mutex::new(SentTiles::new()));
    let mut packet_config =
        PacketizerConfig::new(p.ssrc, p.payload_type, p.tiles, (w as u16, h as u16));
    packet_config.mtu_payload = video_payload_limit(&sock, peer);
    info!(
        payload_limit = packet_config.mtu_payload,
        "HP video packet sizing"
    );
    let sender = Sender {
        sent: sent.clone(),
        sock: sock.try_clone()?,
        pk: Packetizer::new(packet_config),
        tx: SrtpContext::new(&p.send_key),
        ssrc: p.ssrc,
        remote_ssrc: p.remote_ssrc,
        tiles: p.tiles as usize,
        start,
        ts_offset: rand::random(),
        frame_ts: 0,
        frame_send_time: Duration::ZERO,
        packets: 0,
        octets: 0,
        last_sr: Instant::now(),
        stats: send_stats.clone(),
        send_errors_logged: 0,
    };
    let send_thread = std::thread::Builder::new()
        .name("hp-send".into())
        .spawn(move || sender.run(frame_rx))?;

    let r = media_loop(
        stop,
        &sock,
        &mut cap,
        &mut enc,
        &mut rc,
        &mut rx,
        first,
        start,
        &send_stats,
        &sent,
    );
    // Dropping the encoder and the last sender closes the frame channel and ends the send thread.
    drop(frame_tx);
    drop(enc);
    let _ = send_thread.join();
    r
}

struct Source<B: FrameBuffer> {
    screen: Arc<dyn super::ExternalScreen<B>>,
    last: u64,
}

impl<B: FrameBuffer> Source<B> {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<crate::capture::Frame<B>>> {
        let frame = self.screen.next_frame(self.last, timeout);
        if let Some(f) = &frame {
            self.last = f.seq;
        }
        Ok(frame)
    }
}

/// `None` if `stop` was set first.
fn first_frame<B: FrameBuffer>(
    cap: &mut Source<B>,
    stop: &AtomicBool,
) -> Result<Option<crate::capture::Frame<B>>> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if let Some(f) = cap.next_frame(Duration::from_millis(500))? {
            return Ok(Some(f));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn media_loop<B: FrameBuffer>(
    stop: &AtomicBool,
    sock: &UdpSocket,
    cap: &mut Source<B>,
    enc: &mut Encoder<B>,
    rc: &mut RateController,
    rx: &mut RtcpFeedback,
    first: crate::capture::Frame<B>,
    start: Instant,
    send_stats: &Mutex<SendStats>,
    sent: &Mutex<SentTiles>,
) -> Result<MediaEnd> {
    let mut stats = Stats::default();
    let mut last_log = Instant::now();
    let mut last_forced_idr: Option<Instant> = None;
    // Latest-frame policy: while the encoder is busy only the newest capture is kept, so a
    // frame is never encoded after a newer one exists. Nothing is lost by dropping frames:
    // change detection compares against the last submitted frame.
    let mut held = Some(first);
    // A static screen produces no captures, but keyframe requests and heartbeats still need a
    // source, so the last submitted frame is kept for resubmission.
    let mut last: Option<crate::capture::Frame<B>> = None;

    while !stop.load(Ordering::Relaxed) {
        if held.is_none() && enc.can_submit() && enc.wants_frame() {
            held = last.as_ref().map(repeat_frame);
        }
        if held.is_some() && enc.can_submit() {
            let frame = held.take().unwrap();
            let l = enc.layout();
            let size = frame.buffer.size();
            if size != (l.width, l.height) {
                // The viewer decodes a new size in-band but keeps its old window, so it would
                // stretch the picture. End here and let the session renegotiate at the new size.
                info!(from = ?(l.width, l.height), to = ?size, "shared output changed size");
                return Ok(MediaEnd::OutputResized {
                    width: size.0,
                    height: size.1,
                    first_frame_mismatch: false,
                });
            }
            if rx.last_received.is_none()
                && last_forced_idr.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            {
                enc.request_keyframe(None);
                last_forced_idr = Some(Instant::now());
            }
            enc.submit(&frame)?;
            stats
                .submit_ms
                .push(frame.ready_at.elapsed().as_secs_f64() * 1e3);
            stats.frames += 1;
            last = Some(frame);
        }
        if let Some(e) = send_stats.lock().unwrap().failed.take() {
            anyhow::bail!("encode failed: {e}");
        }

        drain_rtcp(sock, rx, enc, rc, start, &mut stats, sent);
        rx.check_timeout(start, Instant::now())?;

        if last_log.elapsed() >= Duration::from_secs(5) {
            let secs = last_log.elapsed().as_secs_f64();
            let mut st = std::mem::take(&mut *send_stats.lock().unwrap());
            info!(
                fps = format_args!("{:.1}", st.frames_window as f64 / secs),
                mbps = format_args!("{:.1}", st.bytes_window as f64 * 8.0 / secs / 1e6),
                frames = stats.frames,
                rtcp = rx.valid,
                udp_received = rx.received,
                non_rtcp = rx.non_rtcp,
                srtcp_unprotect_failed = rx.unprotect_failed,
                rtcp_malformed = rx.malformed,
                recv_errors = rx.recv_errors,
                last_recv_error = ?rx.last_recv_error,
                don_acks = stats.acks,
                ack_samples = stats.ack_ms.len(),
                ack_unmatched = stats.unmatched,
                rr_samples = stats.report_rtt_ms.len(),
                rr_rtt_p50_ms = ?pct(&mut stats.report_rtt_ms, 0.5),
                rr_rtt_p95_ms = ?pct(&mut stats.report_rtt_ms, 0.95),
                ack_bursts = stats.ack_bursts,
                ack_burst_gap_p50_ms = ?pct(&mut stats.burst_gaps_ms, 0.5),
                send_to_ack_p50_ms = ?pct(&mut stats.ack_ms, 0.5),
                send_to_ack_p95_ms = ?pct(&mut stats.ack_ms, 0.95),
                firs = stats.firs,
                target_mbps = format_args!("{:.1}", rc.target_bps() as f64 / 1e6),
                bwe_mbps = ?stats.feedback.as_ref().map(|f| f.bwe_bps as f64 / 1e6),
                owrd_ms = ?stats.feedback.as_ref().map(|f| (f.owrd * 1000.0).round()),
                other_rtcp = stats.other_rtcp,
                send_errors = st.send_errors,
                present_to_first_p50_ms = ?pct(&mut st.present_to_first_ms, 0.5),
                present_to_first_p95_ms = ?pct(&mut st.present_to_first_ms, 0.95),
                present_to_sent_p50_ms = ?pct(&mut st.present_to_sent_ms, 0.5),
                present_to_sent_p95_ms = ?pct(&mut st.present_to_sent_ms, 0.95),
                ready_to_submitted_p50_ms = ?pct(&mut stats.submit_ms, 0.5),
                ready_to_submitted_p95_ms = ?pct(&mut stats.submit_ms, 0.95),
                present_to_ready_p50_ms = ?pct(&mut stats.present_to_ready_ms, 0.5),
                present_to_ready_p95_ms = ?pct(&mut stats.present_to_ready_ms, 0.95),
                dropped = stats.dropped,
                diff_p50_ms = ?pct(&mut st.diff_ms, 0.5),
                convert_p50_ms = ?pct(&mut st.convert_ms, 0.5),
                encode_p50_ms = ?pct(&mut st.encode_ms, 0.5),
                encode_p95_ms = ?pct(&mut st.encode_ms, 0.95),
                send_p50_ms = ?pct(&mut st.send_ms, 0.5),
                "HP stats"
            );
            stats.ack_ms.clear();
            stats.unmatched = 0;
            stats.report_rtt_ms.clear();
            stats.ack_bursts = 0;
            stats.burst_gaps_ms.clear();
            stats.submit_ms.clear();
            stats.present_to_ready_ms.clear();
            last_log = Instant::now();
        }

        // Poll quickly while a frame waits for the encoder.
        let wait = if held.is_some() { 2 } else { 20 };
        if let Some(f) = cap.next_frame(Duration::from_millis(wait))? {
            if let Some(p) = f.presented {
                let ready = monotonic_now().saturating_sub(f.ready_at.elapsed());
                stats
                    .present_to_ready_ms
                    .push(ready.saturating_sub(p).as_secs_f64() * 1e3);
            }
            if held.replace(f).is_some() {
                stats.dropped += 1;
            }
        }
    }
    info!(frames = stats.frames, "HP media stopping");
    Ok(MediaEnd::Stopped)
}

fn repeat_frame<B>(f: &crate::capture::Frame<B>) -> crate::capture::Frame<B> {
    let now = Instant::now();
    crate::capture::Frame {
        seq: f.seq,
        buffer: f.buffer.clone(),
        damage: Vec::new(),
        presented: None,
        requested_at: now,
        ready_at: now,
    }
}

fn drain_rtcp<B: FrameBuffer>(
    sock: &UdpSocket,
    rx: &mut RtcpFeedback,
    enc: &mut Encoder<B>,
    rc: &mut RateController,
    start: Instant,
    stats: &mut Stats,
    sent: &Mutex<SentTiles>,
) {
    for (ev, arrival) in rx.drain(sock) {
        if let Some(bps) = rc.on_event(&ev, start.elapsed()) {
            debug!(bps, "rate control: new target");
            enc.set_bitrate(bps as u64);
        }
        match ev {
            RtcpEvent::KeyframeRequest { .. } => {
                stats.firs += 1;
                if stats.last_refresh.is_none_or(|t| t.elapsed() >= FIR_DEDUPE) {
                    info!("viewer requested a keyframe");
                    enc.request_keyframe(None);
                    stats.last_refresh = Some(Instant::now());
                }
            }
            RtcpEvent::DonAck { don, .. } => {
                stats.acks += 1;
                if let Some(ms) = arrival.and_then(|t| sent.lock().unwrap().ack(don as u16, t)) {
                    stats.ack_ms.push(ms);
                    if let Some(at) = arrival {
                        let gap = stats
                            .last_ack_arrival
                            .and_then(|prev| at.duration_since(prev).ok());
                        // A burst starts after at least 5 ms without a matched ACK.
                        if gap.is_none_or(|d| d >= Duration::from_millis(5)) {
                            stats.ack_bursts += 1;
                            if let Some(d) = gap {
                                stats.burst_gaps_ms.push(d.as_secs_f64() * 1000.0);
                            }
                        }
                        stats.last_ack_arrival = Some(at);
                    }
                } else {
                    stats.unmatched += 1;
                }
            }
            RtcpEvent::RateControl { ref data, .. } => {
                if let Some(fb) = RateFeedback::parse(data) {
                    stats.feedback = Some(fb);
                }
            }
            RtcpEvent::ReceiverReport { blocks, .. } | RtcpEvent::SenderReport { blocks, .. } => {
                if let Some(at) = arrival {
                    let ledger = sent.lock().unwrap();
                    for block in blocks {
                        if let Some(ms) = ledger.report_rtt(
                            block.ssrc,
                            block.last_sr,
                            block.delay_since_last_sr,
                            at,
                        ) {
                            stats.report_rtt_ms.push(ms);
                        }
                    }
                }
            }
            RtcpEvent::Sdes => {}
            other => {
                stats.other_rtcp += 1;
                if stats.other_rtcp <= 20 {
                    info!(?other, "unhandled RTCP");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected_report(tx: &mut SrtpContext, malformed: bool) -> Vec<u8> {
        let mut pkt = vec![0x80, rtcp::PT_RR, 0, 1, 0, 0, 0, 7];
        if malformed {
            pkt[3] = 10;
        }
        tx.protect_rtcp(&mut pkt).unwrap();
        pkt
    }

    #[test]
    fn rtcp_feedback_deadlines_and_recovery() {
        let key = [7; KEY_LEN];
        let mut tx = SrtpContext::new(&key);
        let mut rx = RtcpFeedback::new(&key);
        let start = Instant::now();
        let tick = Duration::from_millis(1);
        assert!(rx.check_timeout(start, start + FIRST_RTCP_TIMEOUT).is_ok());
        assert!(
            rx.check_timeout(start, start + FIRST_RTCP_TIMEOUT + tick)
                .is_err()
        );

        let first = start + Duration::from_secs(2);
        assert_eq!(
            rx.accept(protected_report(&mut tx, false), first),
            [RtcpEvent::ReceiverReport {
                ssrc: 7,
                blocks: Vec::new()
            }]
        );
        assert!(
            rx.check_timeout(start, first + RTCP_SILENCE_TIMEOUT)
                .is_ok()
        );
        assert!(
            rx.check_timeout(start, first + RTCP_SILENCE_TIMEOUT + tick)
                .is_err()
        );

        let resumed = first + RTCP_SILENCE_TIMEOUT - tick;
        rx.accept(protected_report(&mut tx, false), resumed);
        assert!(
            rx.check_timeout(start, first + RTCP_SILENCE_TIMEOUT + tick)
                .is_ok()
        );
        assert_eq!(rx.valid, 2);
    }

    #[test]
    fn rtcp_feedback_rejected_packets_do_not_postpone_fallback() {
        let key = [7; KEY_LEN];
        let start = Instant::now();
        for established in [false, true] {
            let mut tx = SrtpContext::new(&key);
            let mut rx = RtcpFeedback::new(&key);
            let first = start + Duration::from_secs(1);
            if established {
                rx.accept(protected_report(&mut tx, false), first);
            }
            let late = start + FIRST_RTCP_TIMEOUT + Duration::from_secs(1);
            assert!(rx.accept(vec![0; 24], late).is_empty());
            let mut corrupt = protected_report(&mut tx, false);
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(rx.accept(corrupt, late).is_empty());
            assert!(rx.accept(protected_report(&mut tx, true), late).is_empty());
            assert_eq!(rx.last_received, established.then_some(first));
            assert_eq!((rx.non_rtcp, rx.unprotect_failed, rx.malformed), (1, 1, 1));
            assert_eq!(rx.valid, u64::from(established));
            assert_eq!(rx.received, 3 + u64::from(established));
            assert!(rx.check_timeout(start, late).is_err());
        }
    }

    #[test]
    fn rtcp_feedback_unknown_packet_type_still_counts_as_liveness() {
        let key = [7; KEY_LEN];
        let mut tx = SrtpContext::new(&key);
        let mut rx = RtcpFeedback::new(&key);
        let start = Instant::now();
        let now = start + FIRST_RTCP_TIMEOUT;
        let mut pkt = vec![0x80, 210, 0, 1, 0, 0, 0, 7];
        tx.protect_rtcp(&mut pkt).unwrap();
        assert!(matches!(
            rx.accept(pkt, now).as_slice(),
            [RtcpEvent::Other { pt: 210, .. }]
        ));
        assert_eq!(rx.last_received, Some(now));
        assert!(rx.check_timeout(start, now + RTCP_SILENCE_TIMEOUT).is_ok());
    }

    #[test]
    fn rtcp_feedback_compound_packet_must_parse_in_full() {
        let key = [7; KEY_LEN];
        let mut tx = SrtpContext::new(&key);
        let mut rx = RtcpFeedback::new(&key);
        let start = Instant::now();
        let mut compound = vec![0x80, rtcp::PT_RR, 0, 1, 0, 0, 0, 7];
        compound.extend_from_slice(&[0x81, rtcp::PT_SDES, 0, 2, 0, 0, 0, 7, 1, 1, b'x', 0]);
        let mut valid = compound.clone();
        tx.protect_rtcp(&mut valid).unwrap();
        assert_eq!(
            rx.accept(valid, start),
            [
                RtcpEvent::ReceiverReport {
                    ssrc: 7,
                    blocks: Vec::new()
                },
                RtcpEvent::Sdes
            ]
        );

        compound[11] = 10;
        tx.protect_rtcp(&mut compound).unwrap();
        let late = start + RTCP_SILENCE_TIMEOUT + Duration::from_millis(1);
        assert!(rx.accept(compound, late).is_empty());
        assert_eq!(rx.last_received, Some(start));
        assert_eq!((rx.received, rx.valid, rx.malformed), (2, 1, 1));
        assert!(rx.check_timeout(start, late).is_err());
    }

    #[test]
    fn rtcp_feedback_drains_only_one_batch() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let viewer = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(viewer.local_addr().unwrap()).unwrap();
        viewer.connect(sock.local_addr().unwrap()).unwrap();
        sock.set_nonblocking(true).unwrap();
        let key = [7; KEY_LEN];
        let mut tx = SrtpContext::new(&key);
        let mut rx = RtcpFeedback::new(&key);
        for _ in 0..=MAX_RTCP_BATCH {
            viewer.send(&protected_report(&mut tx, false)).unwrap();
        }
        assert_eq!(rx.drain(&sock).len(), MAX_RTCP_BATCH);
        assert_eq!(rx.received, MAX_RTCP_BATCH as u64);
        assert_eq!(rx.drain(&sock).len(), 1);
        assert!(rx.drain(&sock).is_empty());
        assert_eq!(rx.valid, (MAX_RTCP_BATCH + 1) as u64);
        assert_eq!(rx.recv_errors, 0);
    }
}
