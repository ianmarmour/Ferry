# Releasing

Releases are built and published by the
[Release workflow](../.github/workflows/release.yml).

1. Set the new version in `Cargo.toml` and run `cargo build`, which updates
   `Cargo.lock`. Merge that into `main`.
2. Tag the commit and push the tag:

   ```bash
   git tag v0.3.1
   git push origin v0.3.1
   ```

The workflow builds Ferry on Linux, Windows and macOS and, once all three
builds succeed, publishes a GitHub release for the tag with these files, which
the README's download links point to:

| File | Contents |
|---|---|
| `ferry-linux-x86_64.AppImage` | Ferry as an AppImage, which runs without installing |
| `ferry-linux-x86_64.rpm` | The RPM, for Fedora and other RPM distributions |
| `ferry-linux-x86_64.deb` | The Debian package, for Debian, Ubuntu and their derivatives |
| `ferry-linux-x86_64.tar.gz` | The `ferry` binary |
| `ferry-windows-x64.zip` | `ferry.exe` |
| `ferry-macos-app.zip` | `Ferry.app` for Apple silicon, unsigned |

The Linux files hold the same binary, built on Ubuntu 22.04 so that it runs
with glibc 2.35 or newer; [`packaging/linux`](../packaging/linux) has the
scripts that package it. CI builds the Linux packages for every pull request
too, installs them on Ubuntu and Fedora, and keeps them as the run's
artifacts for two weeks.

Starting the workflow by hand from the Actions tab builds the same files
without publishing them; they're kept as the run's artifacts.
