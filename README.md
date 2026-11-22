# vvv

vvv is a Rust integration demo for running Vulkan-capable guests under crosvm/KVM and serving their desktops from NVIDIA Linux hosts. Guest Vulkan uses virtio-gpu, Mesa Venus and virglrenderer. On the remote-display path, the host presenter transfers supported scanouts through CUDA IPC to NVENC for Apple Screen Sharing High Performance (HEVC) and RDP (H.264). The two paths can be used independently.

The tested platform is AArch64 Linux with NVIDIA GB10. An isolated guest enumerated **Virtio-GPU Venus (NVIDIA GB10)**, submitted a Vulkan buffer fill to that device, and verified all 4,096 bytes on readback. Window-system presentation and other guest/driver combinations need separate validation.

## Bring your own guest

Build the receivers with `./build.sh`. The current build uses a workspace-local Rust toolchain and Cargo caches, Podman, libclang, and the pinned WayShare overlay. VM launch also requires a compatible crosvm binary, the CUDA presenter patch, and virglrenderer built with Venus and a render server. The tested configuration uses the threaded render server and worker. See [source release readiness](docs/release-readiness.md) for pins and clean-machine prerequisites.

Start the receiver first, then boot a guest in a second terminal:

```sh
./target/debug/vvv serve --protocol both --session demo --cuda --dev-login
./target/debug/vvv guest --session demo --cuda \
  --kernel /path/to/kernel --disk /path/to/writable-session.img --initrd /path/to/initramfs
```

Supply a dedicated writable disk for this VM. The custom launcher opens it directly. Omit `--initrd` for a kernel that can boot the disk directly. The demo launches crosvm with its process sandbox disabled. The current launcher defaults to a Linux guest with root on `/dev/vda`; set `CROSVM_CMDLINE` for a different kernel command line. The guest needs virtio GPU, input and block support, and a Venus Vulkan driver. Run `vulkaninfo --summary` inside the guest and check the reported device and driver. Plasma-specific provisioning and DPI automation live in the optional [KDE example](examples/kde/README.md).

## Integrate as Rust components

| Cargo package | Integration point |
| --- | --- |
| `vvv` | `GuestConfig::command` constructs a process command for a caller-supplied kernel, disk, optional initramfs and session directory. The application owns the child and the asset lifecycle. |
| `vvv-remote` (`crates/combined`) | `serve(ServerConfig, shutdown)` embeds the remote desktop service. The caller supplies a multithreaded Tokio runtime and shutdown future. The default feature enables both protocols. Disable default features for Apple alone. |
| `vvv-rdp` (`crates/ironrdp`) | `start_on`, `Receiver` and the input callback embed an RDP endpoint directly. Supply captured frames, cursor updates and guest input handling. |
| `vvv-session` | `ViewerSeat` coordinates control, resize preferences, observer views and ownership changes across protocol adapters. |
| `wayland-capture` | `CaptureSink` receives guest frames and cursor planes from the crosvm Wayland socket. |
| `cuda-scanout` | Rust frame and CUDA IPC APIs plus the presenter shared library for the crosvm GPU hook. |

Use local Cargo path dependencies. These demo APIs may change. Build the workspace once before depending on `vvv-remote` so its pinned WayShare source and generated bindings are present. The receiver CLI and SDK use the same server implementation. A runnable [embedding example](crates/combined/examples/embed.rs) starts the service with caller-managed shutdown.

## Remote desktop behavior

One Apple viewer and multiple RDP viewers can share a guest. The primary controls guest input and resolution. Observers see an aspect-fitted desktop and the primary’s cursor, and can click **Take control**. Guest DPI requests reach the virtual display; applying those requests to desktop UI is guest policy.

Both NVENC encoders default to 8 Mbps CBR per stream. Set `--avg-bitrate BPS` on `vvv serve` to override it. Supported GPU surfaces go directly to the encoder after device-side conversion. Software and legacy bitmap paths can use CPU copies. Apple audio currently carries silence.

The combined receiver defaults to loopback. To test over a trusted LAN, pass `-- --listen HOST_IP:5990`. Apple uses TCP 5990 for control and UDP 5992/5994 for media. RDP defaults to TCP 3389. Apple accepts arbitrary credentials only with `--dev-login`. Configure an SRP verifier with `vvv set-password --session NAME -- --user USER` for password verification. RDP accepts development connections without credentials even when Apple uses SRP. Standalone RDP binds all IPv4 interfaces.

Protocol details are in the [RDP notes](crates/ironrdp/README.md) and [Apple notes](crates/apple/README.md).

## License

Original vvv code is available under the [MIT License](LICENSE). Upstream components, vendored files, and upstream portions of patches retain their respective terms. See the [third-party source notes](docs/third-party.md).
