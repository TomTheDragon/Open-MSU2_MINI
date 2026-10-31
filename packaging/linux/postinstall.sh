#!/bin/sh
# Apply the udev rule (device access for the logged-in user) without replugging.
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --subsystem-match=tty 2>/dev/null || true
fi
true
