<p align="center">
  <img src="assets/ferry-banner.svg" width="760" alt="Ferry: your iPhone's messages, on your computer">
</p>

<p align="center">
  <a href="https://github.com/ianmarmour/Ferry/actions/workflows/ci.yml"><img src="https://github.com/ianmarmour/Ferry/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-0A7E86" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/rust-1.95%2B-0A7E86?logo=rust" alt="Rust 1.95+">
  <a href="https://github.com/ianmarmour/Ferry/releases/latest"><img src="https://img.shields.io/badge/platforms-Linux%20%7C%20Windows%20%7C%20macOS-0A7E86" alt="Platforms: Linux, Windows, macOS"></a>
</p>

**Text from your computer through your iPhone.**

<p align="center">
  <img src="docs/screenshots/conversation-light.png" width="800" alt="A conversation in Ferry, with a photo">
</p>

Ferry brings your iPhone's messages to your Linux, Windows or Mac desktop
over Bluetooth. Read your conversations, reply, and start new ones with anyone
in your contacts, in a window styled after Messages on the Mac.

## Highlights

- **No Applications Required** - Ferry uses the Bluetooth profiles car kits
  use: the Message Access Profile for messages and the Phone Book Access
  Profile for contacts. Pair once, turn on two switches on the iPhone, and
  you're set.
- **Stays Out of the Way** - New messages raise desktop notifications, and
  closing the window keeps Ferry connected in the system tray.
- **Multi Platform** - One app for Linux, Windows and macOS.

## Documentation

- **[Setup](docs/setup.md)**: pair your iPhone, install Ferry, and what to
  know on each platform.
- **[Troubleshooting](docs/troubleshooting.md)**: fixes for common problems,
  how to collect logs, and known limitations.
- **[How It Works](docs/architecture.md)**: how Ferry talks to the phone,
  and how the code fits together.
- **[Releasing](docs/releasing.md)**: how a release is built and published.

## Contributing

Bug reports and pull requests are welcome. To build Ferry yourself, install
Rust 1.95 or newer and run:

```bash
cargo run --release
```

`cargo run --release -- --demo` runs it against the simulated iPhone, and
`cargo test` runs the tests, including end-to-end sessions with the simulated
iPhone. CI checks formatting, Clippy and the tests on Linux, Windows and macOS,
and builds the Linux packages and installs them on Ubuntu and Fedora.

## License

Ferry is licensed under the [MIT License](LICENSE).
