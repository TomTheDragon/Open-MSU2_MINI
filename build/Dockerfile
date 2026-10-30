# Build environment for the release executables (Linux x86_64 + Windows x86_64).
# Ubuntu 24.04 (glibc 2.39): the oldest base whose PipeWire headers work with pipewire-rs.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils git build-essential cmake ninja-build pkg-config \
      clang libclang-dev libgtk-3-dev libpipewire-0.3-dev libudev-dev libxkbcommon-dev \
      libgl1-mesa-dev libglu1-mesa-dev libexpat1-dev libpng-dev libdbus-1-dev libssl-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/opt/zig:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.0 \
    && rustup target add x86_64-pc-windows-gnu && rustup component add rustfmt clippy
RUN curl -sSL https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz | tar -xJ -C /opt \
    && mv /opt/zig-x86_64-linux-0.15.2 /opt/zig
RUN cargo install --locked cargo-zigbuild && chmod -R a+rwX /opt/cargo /opt/rustup
