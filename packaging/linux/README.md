# Linux packages

Scripts that package a release build of Ferry for Linux. The
[Release workflow](../../.github/workflows/release.yml) runs `package.sh`
for every release, and [CI](../../.github/workflows/ci.yml) for every pull
request, installing the packages on Ubuntu and Fedora.

```bash
cargo build --release
packaging/linux/package.sh target/release/ferry 1 dist
```

`package.sh` builds all four into `dist`, the `1` being the packages' release
number. Each has a script of its own too:

| Package | Build |
|---|---|
| Debian package | `packaging/linux/build-deb.sh target/release/ferry 1 ferry.deb` |
| RPM | `packaging/linux/build-rpm.sh target/release/ferry 1 ferry.rpm` |
| AppImage | `packaging/linux/build-appimage.sh target/release/ferry AppDir runtime-x86_64 ferry.AppImage` |

The packages install the same files, which `stage.sh` lays out: `ferry` in
`/usr/bin`, its desktop entry, its icons in the hicolor theme, and its
AppStream metadata, which software centers show. Ferry itself installs the
same desktop entry, built into it from
[`io.github.ianmarmour.Ferry.desktop`](io.github.ianmarmour.Ferry.desktop),
for the user when it runs from the AppImage or the tarball.

Ferry links only the C library. The windowing library loads the display
server's, graphics drivers' and keyboard libraries as Ferry starts, so the
packages name them as dependencies by hand: in [`ferry.spec`](ferry.spec)
and [`deb/control`](deb/control). The AppImage takes them from the system it
runs on, as AppImages do, so it holds Ferry alone.

## Requirements

- **Debian packages:** `dpkg-deb` and `objdump`, which Debian and Ubuntu
  have.
- **RPMs:** `rpmbuild`, from `rpm-build` on Fedora or `rpm` on Debian and
  Ubuntu.
- **AppImages:** `mksquashfs`, from `squashfs-tools`, and the
  [AppImage runtime](https://github.com/AppImage/type2-runtime/releases) for
  x86-64, which `fetch-appimage-runtime.sh` downloads and checks.
