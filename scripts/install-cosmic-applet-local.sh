#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${PROJECT_ROOT}"

APP_ID="io.rclone.sync-helper"
BIN_NAME="rclone_sync_helper"

XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
XDG_BIN_HOME="${XDG_BIN_HOME:-$HOME/.local/bin}"

DESKTOP_DST="${XDG_DATA_HOME}/applications/${APP_ID}.desktop"
ICON_DST="${XDG_DATA_HOME}/icons/hicolor/scalable/apps/${APP_ID}.svg"
# Some distros use appdata, others use metainfo; we install both.
APPDATA_DST="${XDG_DATA_HOME}/appdata/${APP_ID}.metainfo.xml"
METAINFO_DST="${XDG_DATA_HOME}/metainfo/${APP_ID}.metainfo.xml"

echo "Building release…"
cargo build --release

echo "Installing binary to ${XDG_BIN_HOME}/${BIN_NAME}…"
install -Dm0755 "target/release/${BIN_NAME}" "${XDG_BIN_HOME}/${BIN_NAME}"

echo "Installing COSMIC applet desktop entry to ${DESKTOP_DST}…"
install -Dm0644 "resources/app.desktop" "${DESKTOP_DST}"
# Ensure COSMIC panel can launch it even if ~/.local/bin isn't in PATH.
sed -i "s|^TryExec=.*$|TryExec=${XDG_BIN_HOME}/${BIN_NAME}|" "${DESKTOP_DST}"
sed -i "s|^Exec=.*$|Exec=${XDG_BIN_HOME}/${BIN_NAME}|" "${DESKTOP_DST}"

echo "Installing icon to ${ICON_DST}…"
install -Dm0644 "resources/icon.svg" "${ICON_DST}"

echo "Installing metainfo…"
install -Dm0644 "resources/app.metainfo.xml" "${APPDATA_DST}"
install -Dm0644 "resources/app.metainfo.xml" "${METAINFO_DST}"

echo
echo "Installed. Next steps:"
echo "1) Open COSMIC Settings → Panel and add “Rclone Sync Helper”."
echo
echo "Restarting COSMIC panel to reload applets…"
pkill cosmic-panel || true
echo "If it doesn't show up immediately, log out/in."

