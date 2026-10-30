#!/bin/sh
# Install Open MSU2_MINI for the current user (no root needed).
#   packaging/linux/install.sh [BINARY]   install (default BINARY: dist/open-msu2-mini-linux-x86_64)
#   ... --autostart                       also start in the background after login
#   ... --udev                            also stop ModemManager from probing the device (sudo)
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
id -nG | grep -qw dialout || echo "NOTE: add yourself to the 'dialout' group: sudo usermod -aG dialout $USER (then log in again)"
echo "Installed. Start 'Open MSU2_MINI' from the app menu or run: open-msu2-mini --help"
