#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
base="$PWD/.cache/apple-build"
repo="$base/wayshare"
rev=257791a461fb1d30ab000a5ff6e5a64134216116
export CONTAINERS_STORAGE_CONF="$PWD/.podman/storage.conf"
mkdir -p "$base"
exec 9>"$base/rust-build.lock"
flock 9
if [[ ! -d "$repo/.git" ]]; then
    git init -q "$repo"
fi
if ! git -C "$repo" cat-file -e "$rev^{commit}" 2>/dev/null; then
    git -C "$repo" fetch --depth=1 https://github.com/wesleygrimes/wayshare.git "$rev"
fi
# Apply the Apple overlay to pristine pinned sources, independent of the RDP checkout.
src="$base/pure/wayshare"
mkdir -p "$src"
git -C "$repo" archive "$rev" | tar -x -C "$src"
for patch in external display-info nvenc video-clock ack feedback pure cursor cuda admission backing dual-stack strip-send path-mtu fixed-target; do
    (cd "$src" && GIT_CEILING_DIRECTORIES="$base/pure" git apply "$OLDPWD/streamer/apple/wayshare-$patch.patch")
done
mkdir -p "$src/src/server_linux/media" "$src/src/encode/nvenc" "$src/src/bin/vvv-apple"
cp streamer/apple/ack_timing.rs "$src/src/server_linux/media/"
cp streamer/apple/nvenc.rs "$src/src/encode/"
cp streamer/apple/nvidia.rs "$src/src/encode/nvenc/"
cp streamer/apple/silent-48k-stereo.eld "$src/src/audio/"
cp streamer/apple/cpu_capture.rs "$src/src/"
cp streamer/apple/{main,cli,service,input,virtio_input,resize,cursor}.rs "$src/src/bin/vvv-apple/"
cp streamer/apple/set_password.rs "$src/src/bin/apple-password.rs"
podman build --target build -t localhost/vvv-apple-rust-build -f streamer/apple/Containerfile streamer/apple
podman run --rm --userns keep-id -v "$PWD:/workspace" -w /workspace \
    localhost/vvv-apple-rust-build bash streamer/apple/build-rust.sh
podman build --target runtime -t localhost/vvv-apple-rust -f streamer/apple/Containerfile streamer/apple
