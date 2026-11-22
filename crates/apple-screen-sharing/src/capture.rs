// SPDX-License-Identifier: MIT
use crate::encode::Rect;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Storage shared by the host display and its encoder.
pub trait FrameBuffer: Send + Sync + 'static {
    fn size(&self) -> (u32, u32);
}
pub struct Frame<B> {
    pub seq: u64,
    pub buffer: Arc<B>,
    pub damage: Vec<Rect>,
    pub presented: Option<Duration>,
    pub requested_at: Instant,
    pub ready_at: Instant,
}

pub fn monotonic_now() -> Duration {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}
