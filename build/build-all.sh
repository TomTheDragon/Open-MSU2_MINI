#!/bin/sh
# Build the Linux and Windows executables into dist/ (needs Docker).
set -e
cd "$(dirname "$0")/.."
# always (re)build the image: cached and quick when build/Dockerfile is unchanged
docker build -q -t open-msu2-build build/
build/cargo-docker.sh build --release
build/build-windows.sh
rm -rf dist && mkdir -p dist
cp target/release/open-msu2-mini dist/open-msu2-mini-linux-x86_64
cp target/x86_64-pc-windows-gnu/release/open-msu2-mini.exe dist/open-msu2-mini-windows-x86_64.exe
ls -la dist
