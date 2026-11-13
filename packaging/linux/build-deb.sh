#!/usr/bin/env bash
# Builds Ferry's Debian package, for Debian, Ubuntu and their derivatives,
# from a built ferry, with the same files as its RPM:
#
#   packaging/linux/build-deb.sh target/release/ferry RELEASE OUTPUT.deb
#
# RELEASE is the package's release number, such as the CI build's; the
# version is Cargo.toml's. The package depends on the packages that have the
# libraries the program links, as rpmbuild finds an RPM's, and on the oldest
# C library its symbols allow; deb/control beside this script adds those the
# windowing library loads as it starts. Needs dpkg-deb and objdump, as Debian
# and Ubuntu have them.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 FERRY_BINARY RELEASE OUTPUT.deb" >&2
  exit 2
fi
binary=$1
release=$2
output=$3

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

# Prints the packages the program needs, comma-separated, failing on a
# library it does not know the package of.
depends() {
  # Weak references, which Rust's standard library makes to newer C library
  # functions it uses only where they exist, do not count.
  local glibc
  glibc=$(objdump -T "$1" | awk '$2 != "w" && /\*UND\*/' | grep -o 'GLIBC_[0-9.]*' \
    | sed 's/^GLIBC_//' | sort -Vu | tail -n 1)
  local needs=("libc6${glibc:+ (>= $glibc)}")
  local library
  for library in $(objdump -p "$1" | awk '$1 == "NEEDED" { print $2 }'); do
    case "$library" in
      libc.so.6 | libm.so.6 | libdl.so.2 | libpthread.so.0 | librt.so.1 | ld-linux-x86-64.so.2) ;;
      libgcc_s.so.1) needs+=("libgcc-s1") ;;
      *)
        echo "$0: $1 links $library: add the Debian package that has it" >&2
        return 1
        ;;
    esac
  done
  local IFS=,
  local list="${needs[*]}"
  echo "${list//,/, }"
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/ferry
"$here/stage.sh" "$binary" "$stage/usr"
# Debian keeps a package's license as its copyright file.
mv "$stage/usr/share/licenses/ferry/LICENSE" "$stage/usr/share/doc/ferry/copyright"
rm -r "$stage/usr/share/licenses"

mkdir -p "$stage/DEBIAN"
(cd "$stage" && find . -type f ! -path './DEBIAN/*' -printf '%P\0' | sort -z | xargs -0 md5sum) \
  > "$stage/DEBIAN/md5sums"
size=$(du -sk --exclude=DEBIAN "$stage" | cut -f1)
dependencies=$(depends "$binary")
sed -e "s/@VERSION@/$version-$release/" -e "s/@INSTALLED_SIZE@/$size/" -e "s/@DEPENDS@/$dependencies/" \
  "$here/deb/control" > "$stage/DEBIAN/control"

# xz, which every dpkg reads, rather than Ubuntu's zstd, which older
# Debian's does not.
dpkg-deb --root-owner-group -Zxz --build "$stage" "$output" >/dev/null
echo "built $output"
