#!/usr/bin/env bash
# Downloads the AppImage runtime, type 2, for x86-64, that build-appimage.sh
# packs Ferry after, and checks it is the release it names:
#
#   packaging/linux/fetch-appimage-runtime.sh OUTPUT
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 OUTPUT" >&2
  exit 2
fi
output=$1
release=20251108
sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

curl -fsSL --retry 3 -o "$output" \
  "https://github.com/AppImage/type2-runtime/releases/download/$release/runtime-x86_64"
echo "$sha256  $output" | sha256sum -c --quiet
