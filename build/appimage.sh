#!/bin/sh
# Build dist/open-msu2-mini-VERSION-x86_64.AppImage from the Linux executable. Runs inside the
# open-msu2-build image (build/package.sh calls it), which has the libraries to bundle.
#
# Like the packages, the AppImage uses the system's GTK 3, PipeWire, ALSA, glibc and ffmpeg.
# It only brings libraries that are missing on some desktops: Ayatana AppIndicator (tray
# icon) with its helpers, and libSM / libICE.
set -e
BINARY=$1 VERSION=$2
APPDIR=target/appimage/AppDir
TOOL=target/tools/appimagetool-1.9.1-x86_64.AppImage
rm -rf "$APPDIR" && mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" target/tools
if [ ! -s "$TOOL" ]; then
    curl -sSfL -o "$TOOL" https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
    chmod +x "$TOOL"
fi

install -m 755 "$BINARY" "$APPDIR/usr/bin/open-msu2-mini"
for lib in libayatana-appindicator3.so.1 libayatana-indicator3.so.7 libayatana-ido3-0.4.so.0 \
    libdbusmenu-gtk3.so.4 libdbusmenu-glib.so.4 libSM.so.6 libICE.so.6; do
    cp -L "/lib/x86_64-linux-gnu/$lib" "$APPDIR/usr/lib/"
done
sed 's#@BIN@#open-msu2-mini#' packaging/linux/open-msu2-mini.desktop > "$APPDIR/open-msu2-mini.desktop"
cp assets/icon.svg "$APPDIR/open-msu2-mini.svg"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
# bundled libraries last: the system's own versions win when they exist
export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}$HERE/usr/lib"
exec "$HERE/usr/bin/open-msu2-mini" "$@"
RUN
chmod +x "$APPDIR/AppRun"

OUT="dist/open-msu2-mini-$VERSION-x86_64.AppImage"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT" >target/appimage/log.txt 2>&1 ||
    { cat target/appimage/log.txt; exit 1; }
echo "created package: $OUT"
