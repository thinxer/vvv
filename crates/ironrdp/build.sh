#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

export CARGO_HOME="$PWD/.local/cargo"
toolchain="$PWD/.local/rustup/toolchains/stable-$(uname -m)-unknown-linux-gnu/bin"
export PATH="$toolchain:$PATH"
export CARGO_TARGET_DIR="$PWD/target"
case "${1:-}" in
  ""|--direct-wayland|--smithay)
    exec cargo build --locked --manifest-path crates/ironrdp/Cargo.toml --bin wayland-ironrdp-receiver
    ;;
  --cuda)
    cargo build --locked --manifest-path crates/cuda-scanout/Cargo.toml
    exec cargo build --locked --manifest-path crates/ironrdp/Cargo.toml --bin wayland-ironrdp-receiver
    ;;
  *) echo "usage: $0 [--direct-wayland|--cuda]" >&2; exit 2 ;;
esac
