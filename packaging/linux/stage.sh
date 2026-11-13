#!/usr/bin/env bash
# Lays out Ferry as it installs, under a prefix such as /usr:
#
#   packaging/linux/stage.sh target/release/ferry DIRECTORY
#
# DIRECTORY gets bin/ferry and share/, with the desktop entry, the icons, the
# AppStream metadata, the license and the README, for the RPM, the Debian
# package and the AppImage.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 FERRY_BINARY DIRECTORY" >&2
  exit 2
fi
binary=$1
stage=$2

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
id=io.github.ianmarmour.Ferry
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

install -Dm0755 "$binary" "$stage/bin/ferry"
install -Dm0644 "$here/$id.desktop" "$stage/share/applications/$id.desktop"
# The metadata names the release: Cargo.toml's version, built today.
mkdir -p "$stage/share/metainfo"
sed -e "s/@VERSION@/$version/" -e "s/@DATE@/$(date -u +%F)/" "$here/$id.metainfo.xml" \
  > "$stage/share/metainfo/$id.metainfo.xml"
chmod 0644 "$stage/share/metainfo/$id.metainfo.xml"
# Each size of the icon, and the drawing they were rendered from.
for icon in "$root"/assets/icons/ferry-*.png; do
  size=${icon##*-}
  size=${size%.png}
  install -Dm0644 "$icon" "$stage/share/icons/hicolor/${size}x${size}/apps/$id.png"
done
install -Dm0644 "$root/assets/ferry.svg" "$stage/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm0644 "$root/LICENSE" "$stage/share/licenses/ferry/LICENSE"
install -Dm0644 "$root/README.md" "$stage/share/doc/ferry/README.md"
