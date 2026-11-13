#!/bin/sh
# Builds Ferry.app from a release build of Ferry, on macOS:
#
#   macos/bundle.sh [binary] [output directory]
#
# The binary defaults to target/release/ferry, the output directory to
# target/release. The app is unsigned.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
binary=${1:-"$root/target/release/ferry"}
output=${2:-"$root/target/release"}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

app="$output/Ferry.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/ferry"
sed "s/@VERSION@/$version/g" "$root/macos/Info.plist" > "$app/Contents/Info.plist"

# The icon, from the rendered PNGs: each size, and its double for Retina.
iconset="$output/Ferry.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    cp "$root/assets/icons/ferry-$size.png" "$iconset/icon_${size}x${size}.png"
    double="$root/assets/icons/ferry-$((size * 2)).png"
    if [ -f "$double" ]; then
        cp "$double" "$iconset/icon_${size}x${size}@2x.png"
    fi
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Ferry.icns"
rm -rf "$iconset"

echo "$app"
