#!/bin/bash
set -euo pipefail
base=/workspace/.cache/apple-build
src="$base/pure/wayshare"
export CARGO_HOME="$base/cargo" CARGO_TARGET_DIR="$base/rust-target"
export PATH="/workspace/.local/rustup/toolchains/stable-$(uname -m)-unknown-linux-gnu/bin:$PATH"
bindgen streamer/apple/nvenc-wrapper.h --allowlist-type NV_ENCODE_API_FUNCTION_LIST \
    --allowlist-type NV_ENC_PIC_FLAGS --allowlist-var 'RUST_.*|NV_ENC_PIC_FLAG_.*' --no-prepend-enum-name --with-derive-default \
    --no-layout-tests --no-doc-comments --output "$src/src/encode/nvenc/nvenc_bindings.rs" \
    -- -Istreamer/third_party/nvenc
cd "$src"
cargo build --locked --release --no-default-features --features apple-guest --bin vvv-apple --bin apple-password -j 4
