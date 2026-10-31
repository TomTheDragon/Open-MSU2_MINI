#!/bin/sh
# Install Open MSU2_MINI for the current user (no root needed).
#   packaging/linux/install.sh [BINARY]   install (default BINARY: dist/open-msu2-mini-linux-x86_64)
#   ... --autostart                       also start in the background after login
#   ... --udev                            also install a udev rule (sudo): device access for the
#                                         logged-in user, no ModemManager probing
#   packaging/linux/install.sh --uninstall
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$HOME/.local/bin/open-msu2-mini"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/open-msu2-mini.svg"
DESKTOP="$HOME/.local/share/applications/open-msu2-mini.desktop"
AUTOSTART="$HOME/.config/autostart/open-msu2-mini.desktop"
UDEV=/etc/udev/rules.d/70-open-msu2-mini.rules

SRC="$ROOT/dist/open-msu2-mini-linux-x86_64"
AUTO=0; RULE=0
for a in "$@"; do
    case "$a" in
        --uninstall)
            pkill -x open-msu2-mini 2>/dev/null || true
            rm -f "$BIN" "$ICON" "$DESKTOP" "$AUTOSTART"
            [ -f "$UDEV" ] && sudo rm -f "$UDEV" && sudo udevadm control --reload
            echo "Removed (settings in ~/.config/open-msu2-mini were kept)."
            exit 0 ;;
        --autostart) AUTO=1 ;;
        --udev) RULE=1 ;;
        *) SRC="$a" ;;
    esac
done
[ -x "$SRC" ] || { echo "Executable not found: $SRC (build it with build/build-all.sh)"; exit 1; }

mkdir -p "$(dirname "$BIN")" "$(dirname "$ICON")" "$(dirname "$DESKTOP")"
install -m 755 "$SRC" "$BIN"
install -m 644 "$ROOT/assets/icon.svg" "$ICON"
sed "s#@BIN@#$BIN#" "$ROOT/packaging/linux/open-msu2-mini.desktop" > "$DESKTOP"

if [ $AUTO = 1 ]; then
    mkdir -p "$(dirname "$AUTOSTART")"
    sed "s#@BIN@ gui#@BIN@ run#; s#@BIN@#$BIN#; s#^Name=.*#Name=Open MSU2_MINI (background)#" \
        "$ROOT/packaging/linux/open-msu2-mini.desktop" > "$AUTOSTART"
    echo "Autostart enabled."
fi
if [ $RULE = 1 ]; then
    sudo install -m 644 "$ROOT/packaging/linux/70-open-msu2-mini.rules" "$UDEV"
    sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=tty
    echo "udev rule installed."
fi

# missing system libraries / ffmpeg (package names: see "Install" in README.md)
MISSING=$(ldd "$BIN" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ')
[ -n "$MISSING" ] && echo "WARNING: missing libraries: $MISSING- install them as shown under \"Install\" in README.md"
command -v ffmpeg >/dev/null || echo "NOTE: install ffmpeg to play videos (see \"Install\" in README.md)"

# serial port access: the udev rule, or the group that owns serial ports (dialout; uucp on Arch)
if [ ! -f "$UDEV" ]; then
    GROUP=dialout
    getent group dialout >/dev/null || GROUP=uucp
    id -nG | grep -qw "$GROUP" ||
        echo "NOTE: for access to the device, run this again with --udev, or: sudo usermod -aG $GROUP $USER (then log in again)"
fi
echo "Installed. Start 'Open MSU2_MINI' from the app menu or run: open-msu2-mini --help"
