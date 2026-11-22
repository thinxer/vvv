# Third-party sources

The top-level MIT license covers original vvv contributions. Keep existing copyright and license notices with imported source, patch context and derived binaries. Cargo package license fields describe the first-party packages.

| Component | Where to find its terms |
| --- | --- |
| WayShare | The pinned export carries `LICENSE` (Apache-2.0) and `THIRD-PARTY-LICENSES`. The Apple build applies local patches to that export. |
| IronRDP | The pinned upstream repository carries `LICENSE-MIT` and `LICENSE-APACHE`. Check individual crates and their dependencies when distributing binaries. |
| crosvm and virglrenderer | Their source distributions contain `LICENSE` and `COPYING` respectively. The crosvm patch includes upstream context. |
| NVIDIA NVENC header | The copyright and permission notice is embedded in `vendor/nvenc/nvEncodeAPI.h`. NVIDIA supplies the host driver separately. |
| virtio GPU metadata protocol | The Chromium Authors’ notice is embedded in `vendor/protocol/virtio-gpu-metadata-v1.xml`. |
| AAC-ELD silence sample | `crates/apple/silent-48k-stereo.eld` is a 160-byte development reference asset. Its release provenance remains open. |
| Guest images and software | Integrators supply their own boot assets. The KDE example scripts reference external images with their own contents and terms. |

Review the full dependency inventory for each binary or container before distribution. The release checklist records the pinned revisions and remaining provenance work.
