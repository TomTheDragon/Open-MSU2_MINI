#!/bin/sh
# Install Open MSU2_MINI for the current user (no root needed).
#   packaging/linux/install.sh [BINARY]   install (default BINARY: dist/open-msu2-mini-linux-x86_64)
#   ... --autostart                       also start in the background after login
#   ... --udev                            also install a udev rule (sudo): device access for the
#                                         logged-in user, no ModemManager probing
#   ... --plugged                         open the window when the screen is plugged in (installs
#                                         a udev rule with sudo, and systemd user units)
#   packaging/linux/install.sh --uninstall
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$HOME/.local/bin/open-msu2-mini"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/open-msu2-mini.svg"
DESKTOP="$HOME/.local/share/applications/open-msu2-mini.desktop"
AUTOSTART="$HOME/.config/autostart/open-msu2-mini.desktop"
UDEV=/etc/udev/rules.d/70-open-msu2-mini.rules
UDEV_PLUGGED=/etc/udev/rules.d/71-open-msu2-mini-plugged.rules
UNITS="$HOME/.config/systemd/user"

SRC="$ROOT/dist/open-msu2-mini-linux-x86_64"
AUTO=0; RULE=0; PLUGGED=0
for a in "$@"; do
    case "$a" in
        --uninstall)
            pkill -x open-msu2-mini 2>/dev/null || true
            systemctl --user disable open-msu2-mini-plugged.service 2>/dev/null || true
            rm -f "$BIN" "$ICON" "$DESKTOP" "$AUTOSTART" \
                "$UNITS/open-msu2-mini-plugged.service" "$UNITS/open-msu2-mini-plugged.target"
            systemctl --user daemon-reload 2>/dev/null || true
            [ -f "$UDEV" ] && sudo rm -f "$UDEV" && sudo udevadm control --reload
            [ -f "$UDEV_PLUGGED" ] && sudo rm -f "$UDEV_PLUGGED" && sudo udevadm control --reload
            echo "Removed (settings in ~/.config/open-msu2-mini were kept)."
            exit 0 ;;
        --autostart) AUTO=1 ;;
        --udev) RULE=1 ;;
        --plugged) PLUGGED=1 ;;
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
if [ $PLUGGED = 1 ]; then
    mkdir -p "$UNITS"
    sed "s#@BIN@#$BIN#" "$ROOT/packaging/linux/open-msu2-mini-plugged.service" > "$UNITS/open-msu2-mini-plugged.service"
    install -m 644 "$ROOT/packaging/linux/open-msu2-mini-plugged.target" "$UNITS/"
    sudo install -m 644 "$ROOT/packaging/linux/71-open-msu2-mini-plugged.rules" "$UDEV_PLUGGED"
    sudo udevadm control --reload
    systemctl --user daemon-reload && systemctl --user enable open-msu2-mini-plugged.service
    echo "The window now opens when the screen is plugged in (switch it off in the window)."
fi

# missing system libraries / ffmpeg (package names: see "Install" in README.md)
MISSING=$(ldd "$BIN" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ')
[ -n "$MISSING" ] && echo "WARNING: missing libraries: $MISSING- install them as shown under \"Install\" in README.md"
command -v ffmpeg >/dev/null || echo "NOTE: install ffmpeg to play videos (see \"Install\" in README.md)"
command -v yt-dlp >/dev/null || echo "NOTE: install yt-dlp to play YouTube streams (see \"Install\" in README.md)"

# serial port access: the udev rule, or the group that owns serial ports (dialout; uucp on Arch)
if [ ! -f "$UDEV" ]; then
    GROUP=dialout
    getent group dialout >/dev/null || GROUP=uucp
    id -nG | grep -qw "$GROUP" ||
        echo "NOTE: for access to the device, run this again with --udev, or: sudo usermod -aG $GROUP $USER (then log in again)"
fi
echo "Installed. Start 'Open MSU2_MINI' from the app menu or run: open-msu2-mini --help"
