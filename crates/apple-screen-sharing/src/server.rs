// Modified for vvv from WayShare. SPDX-License-Identifier: Apache-2.0
//! Linux glue between the rfb server and the capture, encode and hp modules.

mod media;

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::capture::FrameBuffer;
use crate::encode::EncoderFactory;
use crate::hp::negotiation::{self, AnswerParams, Offer};
use crate::rfb::SessionId;
use crate::rfb::media::{HpBackend, MediaAnswer, MediaPorts, MediaStreamConfig, OutputResize};
use crate::rfb::session::canonical_ip;
/// Guest display and frame source supplied by the host application.
pub trait ExternalScreen<B: FrameBuffer>: Send + Sync {
    fn size(&self) -> (u16, u16);
    fn display(&self) -> crate::rfb::AppliedDisplay {
        let pixels = self.size();
        crate::rfb::AppliedDisplay {
            pixels,
            points: pixels,
            scale: 1.0,
        }
    }
    fn next_frame(
        &self,
        after: u64,
        timeout: std::time::Duration,
    ) -> Option<crate::capture::Frame<B>>;
}

pub struct HpServer<B: FrameBuffer> {
    media_bind: SocketAddr,
    external: Arc<dyn ExternalScreen<B>>,
    port: u16,
    encoder: Arc<dyn EncoderFactory<B>>,
    fixed_bitrate: Option<u64>,
    audio: bool,
    swap_keys: bool,
    media: Mutex<Option<Media>>,
}

struct Media {
    session: SessionId,
    audio: Option<crate::audio::sender::AudioSender>,
    peer: IpAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    resized_to: Arc<Mutex<Option<OutputResize>>>,
}

impl Media {
    fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Stop audio before waiting for video teardown: the viewer may have
        // already closed its audio socket.
        if let Some(audio) = self.audio.as_mut() {
            audio.stop();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl<B: FrameBuffer> HpServer<B> {
    /// Start HP media on the supplied display. Enabling audio sends the silent AAC-ELD track.
    pub fn new(
        external: Arc<dyn ExternalScreen<B>>,
        media_bind: SocketAddr,
        port: u16,
        encoder: Arc<dyn EncoderFactory<B>>,
        fixed_bitrate: Option<u64>,
        audio: bool,
        swap_keys: bool,
    ) -> Self {
        Self {
            external,
            media_bind,
            port,
            encoder,
            fixed_bitrate,
            audio,
            swap_keys,
            media: Mutex::new(None),
        }
    }
}

impl<B: FrameBuffer> HpBackend for HpServer<B> {
    fn answer_sent(&self, session: SessionId) {
        let slot = self.media.lock().unwrap();
        if let Some(m) = slot.as_ref()
            && m.session == session
            && let Some(audio) = m.audio.as_ref()
        {
            audio.activate();
        }
    }

    fn available(&self, peer: IpAddr) -> bool {
        match &*self.media.lock().unwrap() {
            Some(m) => !m.running() || m.peer == peer,
            None => true,
        }
    }

    fn active(&self, session: SessionId) -> bool {
        self.media
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|m| m.session == session && m.running())
    }

    fn take_output_resize(&self, session: SessionId) -> Option<OutputResize> {
        let slot = self.media.lock().unwrap();
        let m = slot
            .as_ref()
            .filter(|m| m.session == session && !m.running())?;
        m.resized_to.lock().unwrap().take()
    }

    fn stop(&self, session: SessionId) {
        let mut slot = self.media.lock().unwrap();
        if slot.as_ref().is_some_and(|m| m.session == session) {
            slot.take();
            info!(session, "HP media stopped for session end");
        }
    }

    fn ports(&self) -> MediaPorts {
        MediaPorts {
            flags: 0,
            audio_port: self.port + 2,
            audio_flags: 1,
            video1_port: self.port,
            video1_flags: 1,
            video2_port: 0,
            video2_flags: 0,
        }
    }

    fn configure(
        &self,
        session: SessionId,
        peer: SocketAddr,
        config: MediaStreamConfig,
    ) -> Result<Option<MediaAnswer>> {
        let display = self.external.display();
        let stream_size = (u32::from(display.pixels.0), u32::from(display.pixels.1));
        let video = Offer::parse(&config.video1.offer).context("parsing video offer")?;
        let audio = Offer::parse(&config.audio.offer).context("parsing audio offer")?;
        let v = negotiation::answer(
            &video,
            &AnswerParams {
                ssrc: rand::random::<u32>() & !0xf,
                local: None,
                stream_size,
            },
        )?;
        let a = negotiation::answer(
            &audio,
            &AnswerParams {
                ssrc: rand::random(),
                local: None,
                stream_size,
            },
        )?;
        info!(
            video_mode = video.mode,
            audio_mode = audio.mode,
            ssrc = format_args!("{:#x}", v.ssrc),
            remote_ssrc = format_args!("{:#x}", v.remote_ssrc),
            pt = v.payload_type,
            tiles = v.tiles,
            ?stream_size,
            "HP answers built"
        );
        let (send_key, recv_key) = if self.swap_keys {
            (
                config.video1.key_viewer_to_server,
                config.video1.key_server_to_viewer,
            )
        } else {
            (
                config.video1.key_server_to_viewer,
                config.video1.key_viewer_to_server,
            )
        };
        let peer_ip = canonical_ip(peer.ip());
        let mut video_peer = peer;
        video_peer.set_port(self.port);
        let mut bind = self.media_bind;
        if bind.ip().is_unspecified() && bind.is_ipv4() != video_peer.is_ipv4() {
            bind = if video_peer.is_ipv4() {
                (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
            } else {
                (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
            };
        }
        bind.set_port(self.port);
        let params = media::MediaParams {
            bind,
            external: self.external.clone(),
            peer: video_peer,
            port: self.port,
            encoder: self.encoder.clone(),
            fixed_bitrate: self.fixed_bitrate,
            ssrc: v.ssrc,
            remote_ssrc: v.remote_ssrc,
            payload_type: v.payload_type,
            stream_size,
            tiles: v.tiles as u8,
            send_key,
            recv_key,
        };

        let mut slot = self.media.lock().unwrap();
        if let Some(m) = slot.as_ref()
            && m.running()
            && m.peer != peer_ip
        {
            anyhow::bail!("HP media is in use by another viewer ({})", m.peer);
        }

        slot.take();
        let audio = if self.audio {
            let (a_send, a_recv) = if self.swap_keys {
                (
                    config.audio.key_viewer_to_server,
                    config.audio.key_server_to_viewer,
                )
            } else {
                (
                    config.audio.key_server_to_viewer,
                    config.audio.key_viewer_to_server,
                )
            };
            let params = crate::audio::sender::AudioParams {
                peer: {
                    let mut addr = peer;
                    addr.set_port(self.ports().audio_port);
                    addr
                },
                local_port: self.port + 2,
                ssrc: a.ssrc,
                remote_ssrc: a.remote_ssrc,
                payload_type: a.payload_type,
                send_key: a_send,
                recv_key: a_recv,
                encoder: crate::audio::fdk::EncoderConfig::default(),
                device: None,
            };
            let sender = crate::audio::sender::AudioSender::start_silent(params, bind);
            match sender {
                Ok(sender) => {
                    info!(ssrc = format_args!("{:#x}", a.ssrc), "HP audio prepared");
                    Some(sender)
                }
                Err(e) => {
                    warn!("HP audio unavailable, continuing with video only: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let stop = Arc::new(AtomicBool::new(false));
        let resized_to = Arc::new(Mutex::new(None));
        let thread_stop = stop.clone();
        let thread_resized_to = resized_to.clone();
        let thread = std::thread::Builder::new()
            .name("hp-media".into())
            .spawn(move || match media::run_media(params, &thread_stop) {
                Ok(media::MediaEnd::Stopped) => {}
                Ok(media::MediaEnd::OutputResized {
                    width,
                    height,
                    first_frame_mismatch,
                }) => {
                    let px = |v: u32| v.min(u16::MAX as u32) as u16;
                    *thread_resized_to.lock().unwrap() = Some(OutputResize {
                        pixels: (px(width), px(height)),
                        first_frame_mismatch,
                    });
                }
                Err(e) => warn!("HP media stopped: {e:#}"),
            })?;
        *slot = Some(Media {
            session,
            audio,
            peer: peer_ip,
            stop,
            thread: Some(thread),
            resized_to,
        });

        Ok(Some(MediaAnswer {
            display: Some(display),
            flags: 1,
            audio: a.bytes,
            video1: v.bytes,
            video2: Vec::new(),
        }))
    }
}
