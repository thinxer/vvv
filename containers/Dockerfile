FROM docker.io/debian:trixie-slim

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    clang \
    cmake \
    curl \
    g++ \
    gcc \
    git \
    libcap-dev \
    libclang-dev \
    libdrm-dev \
    libepoxy-dev \
    libgbm-dev \
    libvirglrenderer-dev \
    libvulkan-dev \
    libwayland-dev \
    make \
    meson \
    ninja-build \
    pkg-config \
    protobuf-compiler \
    python3 \
    python3-mako \
    python3-yaml \
    wayland-protocols \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/cache/cargo \
    CARGO_TARGET_DIR=/workspace/crosvm/target \
    PATH=/opt/rustup/toolchains/1.88.0-aarch64-unknown-linux-gnu/bin:/cache/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN mkdir -p /opt/rustup /cache/cargo \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --profile minimal --default-toolchain none \
    && rustup toolchain install 1.88.0 --profile minimal \
    && rustup default 1.88.0

WORKDIR /workspace
