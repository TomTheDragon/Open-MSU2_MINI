#!/bin/sh
# Windows build (cross-compiled with cargo-zigbuild inside the open-msu2-build container).
# LDFLAGS adds the vswscanf shim to wxWidgets' own helper executables (wxrc).
set -e
cd "$(dirname "$0")/.."
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp \
    -v "$PWD":/src -v "$HOME/.cargo/registry":/opt/cargo/registry -w /src -e CARGO_TARGET_DIR=/src/target \
    "${MSU2_IMAGE:-open-msu2-build}" sh -c '
      export PATH=/src/build/win-shim:$PATH &&
      zig cc -target x86_64-windows-gnu -O2 -c build/win-shim/vswscanf.c -o /tmp/vswscanf.o &&
      LDFLAGS=/tmp/vswscanf.o cargo zigbuild --release --target x86_64-pc-windows-gnu'
