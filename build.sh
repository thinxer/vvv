#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
export CARGO_HOME="$PWD/.local/cargo"
export PATH="$PWD/.local/rustup/toolchains/stable-$(uname -m)-unknown-linux-gnu/bin:$PATH"
export CARGO_TARGET_DIR="$PWD/target"
cargo build --locked -p vvv -p cuda-scanout
cargo build --locked --release -p vvv-remote
