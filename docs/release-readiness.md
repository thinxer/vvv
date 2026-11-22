# Source release readiness

Reviewed 22 November 2026. The development build serves a crosvm guest to Apple Screen Sharing High Performance (HEVC) and Windows App over RDP (AVC420). The combined receiver supports one Apple viewer and multiple RDP viewers, shared control with click-to-take-over, per-viewer aspect fit, backing-pixel resolution, and an observer cursor. Both video encoders default to 8 Mbps CBR. Apple audio currently sends silence. A streamer restart requires restarting the test guest to reconnect its virtio keyboard and tablet.

## First release scope

Prepare a source-only developer preview for the tested AArch64 Linux, KVM, crosvm, virglrenderer and NVIDIA configuration. The root manifest, one Cargo lockfile, Rust sources, GPU kernels, guest launch scripts, the in-tree Apple protocol library and vendored interface definitions belong in the source review. Local upstream checkouts, compiled driver libraries, Podman storage, session state, guest disks, root filesystems, verifiers, captures and logs stay outside the source snapshot.

## Upstream inputs

| Input | Revision or origin | Integration |
| --- | --- | --- |
| IronRDP | `e0727394c0c5ef172a13617594e1b2db90e006f7` | Cargo Git dependency. The audited checkout had no tracked changes. |
| WayShare | `257791a461fb1d30ab000a5ff6e5a64134216116` | Historical import origin. The modified Apple library is maintained directly under `crates/apple-screen-sharing`, with its license and third-party notices. |
| crosvm | `f4ef01fabd83f3c7ac8db34cace20d893a059347` | Apply the included CUDA presenter patch to this revision. Build crosvm with the GPU and virgl_renderer features. |
| virglrenderer | `29a4b623a8a15416fa12ab21a1982f75bb6b67e4` | The local source checkout had no tracked changes. The current launcher loads libraries from `runtime/lib`. The tested Venus configuration uses render-server-mode=thread and render-server-worker=thread, with vulkan=true and implicit-render-server=true on the crosvm GPU. |
| NVENC interface | `nv-codec-headers` tag `n13.0.19.0` per the encoder build notes | The vendored header carries NVIDIA’s copyright and permission notice. The host supplies the NVIDIA driver libraries. |
| Wayland metadata XML | `gpu_display/protocol/virtio-gpu-metadata-v1.xml` in the pinned crosvm tree | The vendored copy matches upstream byte for byte and contains its source notice. |

From the project root, with crosvm checked out at the revision above, check and apply its patch once:

```sh
git -C crosvm apply --check ../patches/crosvm-cuda-present.patch
git -C crosvm apply ../patches/crosvm-cuda-present.patch
```

## Before publication

- **License and provenance.** Original vvv code is licensed under MIT. Inventory direct and transitive notices for the artifacts being distributed, and review the WayShare modifications. The 160-byte AAC-ELD silence payload was extracted from a development stream. Document a reproducible generation procedure and its inputs, or replace the asset with one of established provenance.
- **Clean build.** Supply and test bootstrap instructions for workspace-local Rust, libclang and Podman storage. Cargo builds the combined receiver directly. Record the crosvm and virglrenderer build options, submodules and library installation layout.
- **Guest provisioning.** Document the boot requirements for integrator-supplied kernels, disks and optional initramfs. Provide a repeatable build recipe and checksums for the optional KDE test fixture separately. Test first boot, reconnection of input devices and display scaling from a new session.
- **Access model.** Apple supports an SRP verifier and an explicit development login. RDP uses development TLS and accepts clients without credentials. The launcher defaults to all IPv4 interfaces for RDP-only mode. A combined listener on a LAN address exposes RDP on that address even when Apple uses SRP. Decide which authentication and binding defaults are appropriate for a public developer preview.
- **Verification.** Build the reviewed source in a clean environment. Run Rust tests and GPU pixel tests, then exercise Apple HP and RDP with native clients, including resize, role handoff, cursor-only motion and input after restart. Audit the final file manifest and history for secrets and generated artifacts before creating a public remote or release.
