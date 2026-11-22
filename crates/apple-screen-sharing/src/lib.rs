// Modified for vvv from WayShare. SPDX-License-Identifier: Apache-2.0
//! Apple Screen Sharing server and High Performance media support derived from WayShare.
pub mod audio;
pub mod auth;
pub mod capture;
pub mod clipboard;
pub mod display;
pub mod encode;
pub mod hp;
pub mod input;
pub mod rfb;
pub mod server;
pub mod setup;
pub mod source;

pub use encode::{EncoderFactory, VideoEncoder};
pub use rfb::{Config, serve};
pub use server::{ExternalScreen, HpServer};
