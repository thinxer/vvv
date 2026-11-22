# Apple Screen Sharing library

This directory contains the parts of WayShare used by vvv for Apple Screen Sharing, maintained as ordinary workspace source. The import is derived from [WayShare commit 257791a](https://github.com/wesleygrimes/wayshare/tree/257791a461fb1d30ab000a5ff6e5a64134216116). That revision records provenance. Cargo reads this directory directly.

- `rfb/` implements the session and control wire protocol, authentication negotiation, standard framebuffer updates, and cursor messages.
- `hp/` implements High Performance media negotiation, packetization, SRTP and RTCP.
- `server/` and `server.rs` connect protocol sessions to the host-provided display and the media pipeline.
- `encode/` defines the HEVC strip layout, encoded outputs and encoder factory interface. The host implements `FrameBuffer`, `ExternalScreen` and `EncoderFactory` for its own buffer type. The server and encoder share an owned reference to each frame. A streaming encoder may send each strip as it finishes through `VideoEncoder::set_stream_output`.

All module paths above are relative to `src/`. Run `cargo test --locked -p apple-screen-sharing` from the workspace. Protocol fixtures are under `tests/fixtures`.

WayShare-derived code retains the upstream Apache-2.0 license. The import incorporates vvv changes to external capture, Retina display negotiation, cursor and input handling, viewer admission, media feedback, NVENC strip streaming, packet sizing, and dual-stack media. The vvv NVENC backend and WayShare-derived HEVC planning helpers live in the sibling `vvv-nvenc` crate. The upstream license and third-party notices are included alongside this file. See the repository’s [third-party notes](../../docs/third-party.md) for the silence reference asset.
