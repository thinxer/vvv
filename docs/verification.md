# Build and verify the receiver

On a Linux checkout with rootless Podman, run:

```sh
tools/verify/run
```

The initial run downloads a Rust toolchain image, libclang and the locked Cargo dependencies. Container storage, the Cargo cache and build output stay inside the checkout. The check builds the launcher, CUDA presenter and combined receiver, runs the hardware-independent workspace tests, checks the Apple-only remote SDK, and builds an external Apple protocol consumer. GitHub Actions runs this command on pushes and pull requests. The full receiver build requires libclang. The Apple protocol consumer has no NVENC, bindgen or RDP dependency.

`./build.sh` also works with a native Rust toolchain and libclang. It selects the existing workspace toolchain when available. The host GPU checks below require native binaries compatible with the host system libraries.

## Dedicated NVIDIA guest smoke test

Use an unused session and a **dedicated writable guest disk** on a host with the patched crosvm, virglrenderer, CUDA presenter and NVIDIA driver. The probe takes control, clicks the viewer control button and resizes the desktop. The display must have visible non-black content. Keep production listeners on their usual ports.

Run these in three terminals from the workspace root:

```sh
./build.sh
./target/debug/vvv serve --session smoke --protocol both --cuda --dev-login -- \
  --listen 127.0.0.1:45990 --media-port 45992 --rdp-port 43989 > .local/smoke.log 2>&1
```

```sh
./target/debug/vvv guest --session smoke --cuda --kernel /path/to/kernel \
  --disk /path/to/dedicated.img --initrd /path/to/initrd
```

```sh
tools/smoke/run .local/smoke.log
```

The KDE test fixture can be booted with `vvv guest --session smoke --cuda --example kde`. Wait for the receiver log to report both listeners ready before running the probe. The test decodes four Apple HEVC strips, keeps Apple connected, paints the RDP observer view, clicks Take Control, and receives an RDP frame after a portrait resize. The receiver log confirms the primary changed and the new H.264 surface uses NVENC. All client dependencies (FreeRDP, iShareScreen, PyAV and crypto libraries) live in the test image. After testing, shut down the dedicated guest and interrupt the receiver.
