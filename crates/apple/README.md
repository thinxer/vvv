# Apple Screen Sharing receiver

This receiver sends the guest's active crosvm scanout through WayShare using NVIDIA NVENC HEVC 4:2:0 video and silent AAC-ELD audio. The example guest starts at 1280×720. With Dynamic Resolution enabled in Screen Sharing, the primary viewer sizes the guest to the requested backing pixels, rounding each dimension up to an even number for HEVC. The active Apple viewer also sets the virtual monitor DPI. The optional KDE example applies that scale with `kscreen-doctor` on display changes, so a 2× Mac window gets 200% desktop scaling. When RDP takes control, it restores the guest to 100%. The server reports logical points and backing pixels separately, including when the Mac is observing on an aspect-fitted canvas. Keyboard input, absolute pointer motion, buttons and scrolling reach the guest through separate crosvm virtio keyboard and tablet sockets, using the same device definitions as the RDP receiver.

Build with `./build.sh`. Start the receiver and guest in separate terminals. Replace the example listener address with a trusted LAN address.

```sh
./target/debug/vvv serve --protocol apple --session mac --cuda --dev-login -- --listen 192.0.2.10:5990
./target/debug/vvv guest --session mac --cuda --example kde
```

Open `vnc://192.0.2.10:5990` in Screen Sharing and select **High Performance**. For password verification, run `vvv set-password --session mac -- --user vvv` and omit `--dev-login` when serving. TCP 5990 carries session control. UDP 5992 and 5994 carry video and audio. Session files live under `.local/sessions/mac`.

The Rust receiver uses the workspace `wayland-capture` crate for owned BGRX scanouts, sends resize requests directly over the crosvm control socket, and encodes HEVC through the NVIDIA driver. Audio is a 48 kHz stereo AAC-ELD silence track. Each 10 ms packet carries the same 160-byte reference access unit with a fresh RTP sequence number, timestamp and authentication tag.

The protocol and transport live in `apple-screen-sharing`. The `vvv-nvenc` crate supplies the HEVC strip encoder. Both video encoders use `vvv-nvenc-driver` for the generated NVIDIA bindings and driver session lifetime. The `vvv-combined` process runs natively and loads the host driver libraries.

When launched with `vvv serve --protocol apple --session mac --cuda` and `vvv guest --session mac --cuda`, the guest presents supported RGBA textures through CUDA IPC. A device kernel converts each HEVC tile to BT.709 NV12 and extends the last tile to its coded height. NVENC consumes that surface directly. Standard framebuffer requests can read back the GPU frame, and unsupported guest scanouts use the Wayland pixel path. Cursor images continue to arrive over Wayland.
