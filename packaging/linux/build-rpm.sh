#!/usr/bin/env bash
# Builds Ferry's RPM, for Fedora and other RPM distributions, from a
# built ferry:
#
#   packaging/linux/build-rpm.sh target/release/ferry RELEASE OUTPUT.rpm
#
# RELEASE is the package's release number, such as the CI build's; the
# version is Cargo.toml's. Needs rpmbuild: the rpm package on Debian and
# Ubuntu, rpm-build on Fedora.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 FERRY_BINARY RELEASE OUTPUT.rpm" >&2
  exit 2
fi
binary=$1
release=$2
output=$3

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"$here/stage.sh" "$binary" "$work/usr"

rpmbuild --quiet -bb "$here/ferry.spec" \
  --define "_topdir $work/rpmbuild" \
  --define "ferry_source $work/usr" \
  --define "ferry_version $version" \
  --define "ferry_release $release"
cp "$work"/rpmbuild/RPMS/x86_64/ferry-*.rpm "$output"
echo "built $output"
