#!/usr/bin/env bash
# Builds Ferry as an AppImage: one file that runs without installing on most
# Linux systems.
#
#   packaging/linux/build-appimage.sh target/release/ferry APPDIR [RUNTIME OUTPUT.AppImage]
#
# APPDIR gets the AppImage's contents, which its AppRun runs as they are.
# Given the AppImage runtime, type 2, the script packs them after it into
# OUTPUT; fetch-appimage-runtime.sh downloads it. Ferry links only the C
# library, and loads the display server's, graphics drivers' and keyboard
# libraries as it starts, which AppImages take from the computer they run on,
# so the AppImage holds Ferry alone, and runs where Ferry's binary does: with
# the C library of the system it was built on or a newer one. Needs
# mksquashfs, from squashfs-tools.
set -euo pipefail

if [ "$#" -ne 2 ] && [ "$#" -ne 4 ]; then
  echo "usage: $0 FERRY_BINARY APPDIR [RUNTIME OUTPUT.AppImage]" >&2
  exit 2
fi
binary=$1
appdir=$2
here=$(cd "$(dirname "$0")" && pwd)
id=io.github.ianmarmour.Ferry

rm -rf "$appdir"
"$here/stage.sh" "$binary" "$appdir/usr"
install -m0755 "$here/AppRun" "$appdir/AppRun"
# The desktop entry and icon the AppImage shows, as file managers and
# AppImage launchers find them.
install -m0644 "$appdir/usr/share/applications/$id.desktop" "$appdir/$id.desktop"
install -m0644 "$appdir/usr/share/icons/hicolor/256x256/apps/$id.png" "$appdir/$id.png"
ln -sf "$id.png" "$appdir/.DirIcon"

if [ "$#" -eq 4 ]; then
  runtime=$3
  output=$4
  image=$(mktemp)
  trap 'rm -f "$image"' EXIT
  mksquashfs "$appdir" "$image" -root-owned -noappend -comp zstd -quiet -no-progress
  cat "$runtime" "$image" > "$output"
  chmod 0755 "$output"
  echo "built $output"
fi
