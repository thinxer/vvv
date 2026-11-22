#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
base="$PWD/.cache/apple-build"
export CONTAINERS_STORAGE_CONF="$PWD/.podman/storage.conf"
state="${APPLE_STATE_DIR:-$base/session}"
mkdir -p -m 700 "$state"
state=$(realpath "$state")
case "${1:-}" in
    set-password) shift; cmd=(apple-password --srp-file /state/verifier "$@");;
    serve-dev) shift; cmd=(vvv-apple --socket /state/frames.sock --dev-login "$@");;
    serve) shift; cmd=(vvv-apple --socket /state/frames.sock --srp-file /state/verifier "$@");;
    *) echo 'Usage: run.sh set-password --user NAME | serve[-dev] [--listen IPv4:PORT] [--media-port PORT]' >&2; exit 2;;
esac
tty_args=(-i)
if [[ -t 0 && -t 1 ]]; then tty_args+=(-t); fi
gpu_args=()
if [[ ${cmd[0]} == vvv-apple ]]; then
    gpu_args+=(--device /dev/nvidia0 --device /dev/nvidiactl --device /dev/nvidia-uvm)
    libs=(libcuda.so.1 libnvidia-encode.so.1 libnvcuvid.so.1)
    if [[ " ${cmd[*]} " == *" --cuda-socket "* ]]; then
        libs+=(libnvidia-ptxjitcompiler.so.1)
    fi
    for lib in "${libs[@]}"; do
        path=$(ldconfig -p | awk -v lib="$lib" '$1 == lib && !found {print $NF; found=1}')
        [[ -f "$path" ]] || { echo "Missing NVIDIA driver library $lib" >&2; exit 1; }
        gpu_args+=(-v "$(realpath "$path"):/nvidia/$lib:ro")
    done
fi
exec podman run "${gpu_args[@]}" --rm "${tty_args[@]}" --userns keep-id --network "${APPLE_NETWORK:-host}" \
    -v "$base/rust-target/release/${cmd[0]}:/bin-apple/${cmd[0]}:ro" \
    -v "$state:/state" -e LD_LIBRARY_PATH=/nvidia localhost/vvv-apple-rust "/bin-apple/${cmd[@]}"
