#!/bin/sh
# Run cargo inside the open-msu2-build container (see build/Dockerfile).
# Usage: build/cargo-docker.sh build --release [--target x86_64-pc-windows-gnu ...]
cd "$(dirname "$0")/.." || exit 1
exec docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp \
    -v "$PWD":/src -v "$HOME/.cargo/registry":/opt/cargo/registry \
    -w /src -e CARGO_TARGET_DIR=/src/target "${MSU2_IMAGE:-open-msu2-build}" cargo "$@"
