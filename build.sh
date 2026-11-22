#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
export CARGO_HOME="${CARGO_HOME:-$PWD/.local/cargo}"
toolchain="$PWD/.local/rustup/toolchains/stable-$(uname -m)-unknown-linux-gnu/bin"
if [[ -x "$toolchain/cargo" && -z "${RUSTUP_HOME:-}" ]]; then
    export PATH="$toolchain:$PATH"
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
cargo build --locked -p vvv -p cuda-scanout
cargo build --locked --release -p vvv-remote
