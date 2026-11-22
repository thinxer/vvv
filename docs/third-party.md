# Third-party sources

The top-level MIT license covers original vvv contributions. Keep existing copyright and license notices with imported source, patch context and derived binaries. Cargo package license fields describe the first-party packages.

| Component | Where to find its terms |
| --- | --- |
| WayShare | See the [in-tree source and provenance](../crates/apple-screen-sharing/README.md), [Apache-2.0 license](../crates/apple-screen-sharing/LICENSE), and [upstream third-party notices](../crates/apple-screen-sharing/THIRD-PARTY-LICENSES). |
| IronRDP | The pinned upstream repository carries `LICENSE-MIT` and `LICENSE-APACHE`. Check individual crates and their dependencies when distributing binaries. |
| crosvm and virglrenderer | Their source distributions contain `LICENSE` and `COPYING` respectively. The crosvm patch includes upstream context. |
| NVIDIA NVENC header | The copyright and permission notice is embedded in `vendor/nvenc/nvEncodeAPI.h`. NVIDIA supplies the host driver separately. |
| virtio GPU metadata protocol | The Chromium Authors’ notice is embedded in `vendor/protocol/virtio-gpu-metadata-v1.xml`. |
| AAC-ELD silence sample | `crates/apple-screen-sharing/src/audio/silent-48k-stereo.eld` is a 160-byte development reference asset. Its release provenance remains open. |
| Guest images and software | Integrators supply their own boot assets. The KDE example scripts reference external images with their own contents and terms. |

Review the full dependency inventory for each binary or container before distribution. The release checklist records the pinned revisions and remaining provenance work.
