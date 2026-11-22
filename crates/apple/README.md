# Apple Screen Sharing receiver

This receiver sends the guest's active crosvm scanout through WayShare using NVIDIA NVENC HEVC 4:2:0 video and silent AAC-ELD audio. The example guest starts at 1280×720. With Dynamic Resolution enabled in Screen Sharing, the primary viewer sizes the guest to the requested backing pixels, rounding each dimension up to an even number for HEVC. The active Apple viewer also sets the virtual monitor DPI. The optional KDE example applies that scale with `kscreen-doctor` on display changes, so a 2× Mac window gets 200% desktop scaling. When RDP takes control, it restores the guest to 100%. The server reports logical points and backing pixels separately, including when the Mac is observing on an aspect-fitted canvas. Keyboard input, absolute pointer motion, buttons and scrolling reach the guest through separate crosvm virtio keyboard and tablet sockets, using the same device definitions as the RDP receiver.

From the workspace root, build the receiver:

```sh
crates/apple/build.sh
```

Replace the example address with a trusted LAN address reachable from your Mac. In a second terminal, boot the dedicated guest:

```sh
crates/apple/run.sh serve-dev --listen 192.0.2.10:5990
```

```sh
crates/apple/guest.sh
```

Open `vnc://192.0.2.10:5990` in Screen Sharing on the Mac, select **High Performance**, and enter any username and password. Anyone who can reach this development listener can control its guest. For a password-checked session, run `crates/apple/run.sh set-password --user vvv` and start the listener with `serve`. TCP 5990 carries session control. UDP 5992 carries video and its feedback. UDP 5994 carries audio and its feedback. The listener starts accepting viewers after the guest publishes its first frame.

The launcher copies the base KDE disk on first use. Its disk, sockets and verifier live under `.cache/apple-build/session`. Set `APPLE_STATE_DIR` consistently for both commands to use another private session directory. To stop the trial, shut down the guest and interrupt the foreground receiver.

The Rust receiver uses the workspace `wayland-capture` crate for owned BGRX scanouts, sends resize requests directly over the crosvm control socket, and encodes HEVC through the NVIDIA driver. Audio is a 48 kHz stereo AAC-ELD silence track. Each 10 ms packet carries the same 160-byte reference access unit with a fresh RTP sequence number, timestamp and authentication tag.

The build pins WayShare to `257791a461fb1d30ab000a5ff6e5a64134216116` and selects its external capture profile. Podman runs Cargo with the workspace Rust toolchain and generates driver bindings from the vendored NVENC API 13.0 header. The runtime container mounts the Rust executable, session directory, GPU 0, and three host driver libraries (`libcuda.so.1`, `libnvidia-encode.so.1`, and `libnvcuvid.so.1`). The guest launcher runs the workspace crosvm binary on the host.

When launched with `vvv serve --protocol apple --session mac --cuda` and `vvv guest --session mac --cuda`, the guest presents supported RGBA textures through CUDA IPC. A device kernel converts each HEVC tile to BT.709 NV12 and extends the last tile to its coded height. NVENC consumes that surface directly. This mode also mounts the host NVIDIA PTX JIT compiler. Standard framebuffer requests can read back the GPU frame, and unsupported guest scanouts use the Wayland pixel path. Cursor images continue to arrive over Wayland.
