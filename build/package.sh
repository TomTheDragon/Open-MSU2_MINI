#!/bin/sh
# Build Linux packages (.deb, .rpm, Arch .pkg.tar.zst, AppImage) into dist/ from the Linux executable.
# Usage: build/package.sh [BINARY]   (default: dist/open-msu2-mini-linux-x86_64; needs Docker)
set -e
cd "$(dirname "$0")/.."
BINARY=${1:-dist/open-msu2-mini-linux-x86_64}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
STAGE=target/package
mkdir -p "$STAGE" dist
# fixed paths, as used in packaging/linux/nfpm.yaml
cp "$BINARY" "$STAGE/open-msu2-mini"
sed 's#@BIN@#/usr/bin/open-msu2-mini#' packaging/linux/open-msu2-mini.desktop > "$STAGE/open-msu2-mini.desktop"
for p in deb rpm archlinux; do
    docker run --rm -u "$(id -u):$(id -g)" -v "$PWD":/src -w /src \
        -e VERSION="$VERSION" \
        goreleaser/nfpm:v2.48.0 package --config packaging/linux/nfpm.yaml --packager "$p" --target dist/
done
# AppImage: built in CI; locally only with MSU2_APPIMAGE=1, because it downloads and runs
# appimagetool. Runs in the build image (which has the libraries to bundle), or directly on
# an Ubuntu 24.04 host (CI).
if [ -z "$CI" ] && [ -z "$MSU2_APPIMAGE" ]; then
    echo "AppImage skipped (built in CI; set MSU2_APPIMAGE=1 to build it here)"
elif docker image inspect "${MSU2_IMAGE:-open-msu2-build}" >/dev/null 2>&1; then
    docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD":/src -w /src "${MSU2_IMAGE:-open-msu2-build}" \
        build/appimage.sh "$BINARY" "$VERSION"
else
    build/appimage.sh "$BINARY" "$VERSION"
fi
ls -la dist/*.deb dist/*.rpm dist/*.pkg.tar.zst dist/*.AppImage 2>/dev/null || true
