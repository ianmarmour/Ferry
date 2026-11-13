#!/usr/bin/env bash
# Builds all of Ferry's Linux packages from a release build, as the Release
# workflow publishes them:
#
#   packaging/linux/package.sh target/release/ferry RELEASE DIRECTORY
#
# DIRECTORY gets ferry-linux-x86_64.tar.gz, .deb, .rpm and .AppImage. RELEASE
# is the packages' release number. Needs what build-deb.sh, build-rpm.sh and
# build-appimage.sh need, and curl, to download the AppImage runtime.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 FERRY_BINARY RELEASE DIRECTORY" >&2
  exit 2
fi
binary=$1
release=$2
output=$3
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
name=ferry-linux-x86_64

mkdir -p "$output"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The binary, to run from anywhere; it adds itself to the user's applications.
mkdir -p "$work/tarball/ferry"
cp "$binary" "$root/README.md" "$root/LICENSE" "$work/tarball/ferry/"
tar -czf "$output/$name.tar.gz" -C "$work/tarball" ferry
echo "built $output/$name.tar.gz"

"$here/build-deb.sh" "$binary" "$release" "$output/$name.deb"
"$here/build-rpm.sh" "$binary" "$release" "$output/$name.rpm"
"$here/fetch-appimage-runtime.sh" "$work/runtime-x86_64"
"$here/build-appimage.sh" "$binary" "$work/AppDir" "$work/runtime-x86_64" "$output/$name.AppImage"
