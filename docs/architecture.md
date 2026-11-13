# How It Works

Ferry talks to the iPhone the way a car kit does, over two Bluetooth
profiles that run on OBEX:

- the **Message Access Profile** (MAP), to list, download and send messages,
  and to be told about new ones; and
- the **Phone Book Access Profile** (PBAP), to download contacts and their
  photos.

OBEX and MAP come from the [`obex-protocol`](https://crates.io/crates/obex-protocol)
and [`obex-protocol-map`](https://crates.io/crates/obex-protocol-map) crates.
Ferry has a small PBAP client of its own, in `src/pbap.rs`.

```text
+-----------------------------------------------------------------+
|  ui         egui window, tray icon, notifications                |
+-----------------------------------------------------------------+
|  session    tokio worker driving the phone like a car kit        |
|  model      messages, conversations, contacts, on-disk cache     |
+--------------------+--------------------+-----------------------+
|  MAP client        |  MAP notification  |  PBAP client          |
|  browse, read,     |  server: new       |  contacts and         |
|  send              |  messages, delivery|  their photos         |
+--------------------+--------------------+-----------------------+
|  bluetooth  BlueZ | WinRT | IOBluetooth | simulated iPhone        |
+-----------------------------------------------------------------+
```

## A session

When it connects, Ferry:

1. opens the phone's Message Access Server and goes to `telecom/msg`;
2. publishes its own Message Notification Server and registers for
   notifications, so the phone reports new messages and deliveries as they
   happen;
3. downloads the contacts over PBAP on a second channel, first without photos,
   which is quick, then with them;
4. lists the inbox and Sent folder and downloads the messages it hasn't seen,
   one at a time between commands, so sending is never stuck behind a sync;
5. sends messages by pushing them to the phone's outbox.

iPhones only share recent messages, so Ferry keeps everything it has seen in
a cache, one file per phone, and the history builds up over time. MAP message
handles don't outlive a session, so messages are matched across sessions by
their content. When the link drops, Ferry reconnects with a growing delay.
Until the phone connects for notifications, it checks for new messages every
20 seconds, and every two minutes after that.

## Bluetooth on each platform

Each platform implements the `Backend` trait in `src/bluetooth`: list the
paired devices, connect to a service of the phone, and listen for the phone's
notification connections. Connections are handed over as tokio streams.

- **Linux** (`linux.rs`): Ferry finds the phone's services with its own SDP
  query and connects RFCOMM sockets, like BlueZ's `obexd` MAP client. It uses
  BlueZ over D-Bus only to list devices and to publish its notification
  service record, under a UUID of its own: BlueZ allows each profile UUID to
  be registered once, and `obexd` usually holds MAP's.
- **Windows** (`windows.rs`): WinRT finds the phone's RFCOMM services and
  connects stream sockets; an `RfcommServiceProvider` publishes the
  notification service.
- **macOS** (`macos.rs`): IOBluetooth runs the SDP query, opens the RFCOMM
  channels and publishes the notification service, on the main thread.
- **Demo** (`demo.rs`): a simulated iPhone running real MAP and PBAP servers
  over in-memory streams, used by `ferry --demo` and the tests.

## The code

| Path | What it is |
|---|---|
| `src/bluetooth/` | The `Backend` trait, its platform implementations and the simulated iPhone |
| `src/session.rs` | The worker that talks to the phone |
| `src/model.rs` | Messages, conversations and the cache |
| `src/contacts.rs` | vCard parsing and matching addresses to contacts |
| `src/pbap.rs` | The PBAP client |
| `src/text.rs` | Text and attachments of MIME messages |
| `src/ui/` | The window, tray icon, app icon and photos |
| `src/notify.rs` | Desktop notifications |
| `src/desktop.rs` | The Linux desktop entry, from `packaging/linux`, installed for the user when no package installed it |
| `src/migrate.rs` | Moving data back from Tidings, a name Ferry briefly had |

## Tests

- `tests/session.rs` drives the session worker against the simulated iPhone:
  syncing, contacts and photos, sending, delivery reports, notifications, read
  status and reconnecting.
- `tests/bluez.rs` (Linux) runs the BlueZ backend against a fake BlueZ on a
  private D-Bus, with sockets leading to the simulated iPhone.
- `tests/tray.rs` (Linux) registers the tray icon with a fake
  StatusNotifierWatcher and clicks it.

The Linux tests need `dbus-daemon`. The kernel's Bluetooth sockets, WinRT and
IOBluetooth need real hardware, so CI builds and lints those backends but
can't exercise them.
