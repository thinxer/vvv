#!/bin/bash
# Boot one named session against its receiver sockets.
set -euo pipefail
cd "$(dirname "$0")/.."
state="${1:?usage: start-session.sh SESSION_DIR}"
mkdir -p -m 700 "$state"
state=$(realpath "$state")
exec 9>"$state/guest.lock"
flock -n 9 || { echo "Guest already running in $state" >&2; exit 1; }
[[ -S "$state/frames.sock" ]] || { echo "Start the receiver first" >&2; exit 1; }
[[ ! -S "$state/crosvm.sock" ]] || { echo "Guest control socket already exists" >&2; exit 1; }
if [[ -n ${VVV_GUEST_DISK:-} || -n ${VVV_GUEST_KERNEL:-} ]]; then
    export CROSVM_DISK="${VVV_GUEST_DISK:?a writable guest disk is required}"
    export CROSVM_KERNEL="${VVV_GUEST_KERNEL:?a guest kernel is required}"
    export CROSVM_INITRD="${VVV_GUEST_INITRD:-}"
else
    source examples/kde/prepare.sh
fi
export CROSVM_CTRL_SOCK="$state/crosvm.sock"
export CROSVM_WAYLAND_SOCK="$state/frames.sock" CROSVM_MOUSE_SOCK="$state/mouse.sock" CROSVM_KEYBOARD_SOCK="$state/keyboard.sock"
export CROSVM_CPUS=4 CROSVM_MEM=4096
export CROSVM_GPU_ARGS='backend=virglrenderer,vulkan=true,implicit-render-server=true,displays=[[mode=windowed[1280,720],dpi=[96,96]]]'
if [[ ${2:-} == --cuda ]]; then
    [[ -S "$state/cuda.sock" ]] || { echo "Start the receiver with --cuda first" >&2; exit 1; }
    export VVV_CUDA_SOCKET="$state/cuda.sock"
    export VVV_CUDA_PRESENTER="$PWD/target/debug/libcuda_scanout.so"
    [[ -r "$VVV_CUDA_PRESENTER" ]] || { echo "Build the CUDA presenter first" >&2; exit 1; }
fi
if [[ ! -w /dev/kvm ]]; then
    exec sg kvm -c 'exec guest/run-crosvm.sh'
fi
exec guest/run-crosvm.sh
