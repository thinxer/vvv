#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GPU_ARGS="${CROSVM_GPU_ARGS:-backend=2d,displays=[[mode=windowed[1280,720],dpi=[96,96]]]}"
KERNEL="${CROSVM_KERNEL:-$ROOT/guest/vmlinux-stable.elf}"
DISK="${CROSVM_DISK:-$ROOT/guest/alpine-arm64.img}"
CPUS="${CROSVM_CPUS:-2}"
MEM="${CROSVM_MEM:-1024}"
CMDLINE="${CROSVM_CMDLINE:-console=ttyS0 root=/dev/vda rw}"
export LD_LIBRARY_PATH="$ROOT/runtime/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
CTRL_SOCK="${CROSVM_CTRL_SOCK:-$ROOT/guest/crosvm.sock}"
rm -f "$CTRL_SOCK"
MOUSE_ARGS=(--display-window-mouse)
if [ -n "${CROSVM_MOUSE_SOCK:-}" ]; then
  MOUSE_ARGS=(--input "custom[path=$CROSVM_MOUSE_SOCK,config-path=$ROOT/guest/virtio-tablet.json]")
fi
KEYBOARD_ARGS=(--display-window-keyboard)
if [ -n "${CROSVM_KEYBOARD_SOCK:-}" ]; then
  KEYBOARD_ARGS+=(--input "keyboard[path=$CROSVM_KEYBOARD_SOCK]")
fi
if [[ -n "${VVV_CUDA_PRESENTER:-}" ]]; then
    export LD_PRELOAD="$VVV_CUDA_PRESENTER${LD_PRELOAD:+:$LD_PRELOAD}"
fi
initrd_args=()
if [[ -n ${CROSVM_INITRD-"$ROOT/guest/initramfs-arm64.img"} ]]; then
    initrd_args+=(--initrd "${CROSVM_INITRD-"$ROOT/guest/initramfs-arm64.img"}")
fi
exec "$ROOT/crosvm/target/debug/crosvm" run \
  --disable-sandbox \
  --socket "$CTRL_SOCK" \
  --cpus "$CPUS" \
  --mem "$MEM" \
  --block "$DISK" \
  "${initrd_args[@]}" \
  --gpu "$GPU_ARGS" \
  --wayland-sock "${CROSVM_WAYLAND_SOCK:-$ROOT/guest/frames.sock}" \
  "${KEYBOARD_ARGS[@]}" \
  "${MOUSE_ARGS[@]}" \
  -p "$CMDLINE" \
  "$KERNEL"
