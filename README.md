# Ferry
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![CI](https://github.com/ianmarmour/Ferry/workflows/CI/badge.svg)](https://github.com/ianmarmour/Ferry/actions/workflows/ci.yml)

A cross-platform desktop client for texting through your iPhone, over Bluetooth.

Ferry reads your messages and sends texts to any of your contacts, in a window styled after Messages on the Mac. Nothing is installed on the phone: Ferry talks to it the way a car does, with the Bluetooth **Message Access Profile** (MAP) for messages and the **Phone Book Access Profile** (PBAP) for contacts.

It runs on Linux (BlueZ over D-Bus), Windows (WinRT) and macOS (IOBluetooth), and is built on the [`map`](https://github.com/ianmarmour/map) and [`obex`](https://github.com/ianmarmour/obex) crates.

## How It Works

```text
+-----------------------------------------------------------------+
|  ui         egui window: conversations, bubbles, composer       |
+-----------------------------------------------------------------+
|  session    tokio worker driving the phone like a car kit       |
|  model      messages, conversations, contacts, on-disk cache    |
+--------------------+--------------------+-----------------------+
|  map::tokio        |  map::tokio        |  pbap (on obex)       |
|  MasClient         |  MnsServer         |  PullPhoneBook        |
|  browse, read,     |  new message and   |  contacts             |
|  send              |  delivery events   |                       |
+--------------------+--------------------+-----------------------+
|  bluetooth  BlueZ (D-Bus) | WinRT | IOBluetooth | demo phone    |
+-----------------------------------------------------------------+
```

When you connect, Ferry:

1. opens the phone's **Message Access Server** (MAP MAS) and goes to `telecom/msg`;
2. publishes its own **Message Notification Server** (MAP MNS) and registers for notifications, so the phone reports new messages and deliveries as they happen;
3. downloads your contacts over **PBAP** on a second channel;
4. lists the inbox and Sent folder and downloads the messages it hasn't seen, one at a time between your commands, so sending is never stuck behind a sync;
5. sends your texts by pushing them to the phone's outbox.

iPhones only expose recent messages over MAP, so Ferry keeps everything it has seen in a local cache (one file per phone) and the history builds up over time. Messages are matched across sessions by content, since MAP handles don't outlive a session. If the link drops, Ferry reconnects with a growing delay. Until the phone actually connects to the notification server, Ferry checks for new messages every 20 seconds.

New messages raise a native desktop notification: the freedesktop notification service over D-Bus on Linux, toasts on Windows and Notification Center on macOS.

## Getting Started

### 1. Set up the iPhone

1. Pair the iPhone with your computer in the system's Bluetooth settings.
2. On the iPhone, open **Settings › Bluetooth**, tap **ⓘ** next to your computer and turn on **Show Notifications** and **Sync Contacts**.

### 2. Build and run

```bash
cargo run --release
```

To try Ferry without a phone, run it against a simulated iPhone. Its contacts answer your messages, and new ones arrive now and then:

```bash
cargo run --release -- --demo
```

Set `RUST_LOG=ferry=debug` to see what's happening on the Bluetooth link.

### Platform notes

**Linux.** Needs BlueZ 5 (`bluetoothd`). Ferry opens its own Bluetooth sockets: it finds the phone's services with an SDP query and connects over RFCOMM, as BlueZ's `obexd` MAP client does, and uses BlueZ over D-Bus only to list paired devices and publish its notification server record. It doesn't register BlueZ profiles for MAP or PBAP, so it works alongside `obexd`, which most desktops run and which claims those profiles. Nothing is linked at build time: D-Bus is spoken in pure Rust (zbus), and X11, Wayland and OpenGL are loaded at runtime. `obexd` also publishes a notification server, and the phone may deliver to it instead of Ferry. If new messages only arrive at the 20 second check, stop it while using Ferry with `systemctl --user stop obex`, or turn it off for good with `systemctl --user mask obex`.

**Windows.** Windows 10 or later. The Phone Link app also talks MAP to iPhones; if it's connected to the same phone, quit it first. Notifications show up as coming from Windows PowerShell unless Ferry is installed with its own app identity.

**macOS.** Bluetooth access requires `NSBluetoothAlwaysUsageDescription`. Run Ferry from a terminal that has Bluetooth permission, or bundle it with [`cargo-bundle`](https://github.com/burtonageo/cargo-bundle), which picks up `macos/Info.plist`:

```bash
cargo bundle --release
```

## Development

```bash
cargo test
```

* `tests/session.rs` drives the session worker against the simulated iPhone, which runs the real MAP and PBAP servers from the `map` and `obex` crates over in-memory streams. It covers syncing, contacts, sending, delivery reports, notifications, read status and reconnection.
* `tests/bluez.rs` (Linux) runs the BlueZ backend against a fake BlueZ on a private D-Bus, with test sockets that answer SDP searches and lead to the simulated iPhone. It covers device listing, publishing the notification record while the MAP UUIDs are taken, and the whole session over the backend. It needs `dbus-daemon`. The kernel socket calls themselves need Bluetooth hardware.

The Windows and macOS backends are type-checked by CI on their platforms.

| Module | |
|--------|---|
| `src/bluetooth/` | the `Backend` trait, `linux.rs` (BlueZ), `windows.rs` (WinRT), `macos.rs` (IOBluetooth), `demo.rs` (simulated iPhone) |
| `src/session.rs` | the worker talking to the phone |
| `src/model.rs` | messages, conversations and the cache |
| `src/contacts.rs` | vCard parsing and address matching |
| `src/pbap.rs` | a minimal PBAP client on `obex` |
| `src/text.rs` | text extraction from MIME message bodies |
| `src/ui/` | the window |

## Limitations

* Conversations are one-to-one; group messages show up in the sender's conversation.
* Attachments aren't downloaded; MMS show their text.
* What an iPhone allows over MAP depends on its iOS version. Ferry reports what the phone refuses, for example if it won't accept messages to send.

## Minimum Supported Rust Version

Rust 1.95, the version egui 0.37 requires.
