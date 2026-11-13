# Setup

Ferry needs a computer with Bluetooth and an iPhone. Nothing is installed on
the iPhone.

## 1. Pair your iPhone

1. On the iPhone, open **Settings › Bluetooth** and leave that screen open:
   iPhones can only be found while it's showing.
2. On the computer, open the system's Bluetooth settings, choose the iPhone
   and confirm that both screens show the same code.

## 2. Allow messages and contacts

On the iPhone, open **Settings › Bluetooth**, tap **ⓘ** next to your computer,
and turn on **Show Notifications** and **Sync Contacts**. The iPhone may also
ask the first time Ferry connects.

## 3. Install and run Ferry

Download Ferry for your platform from the
[latest release](https://github.com/ianmarmour/Ferry/releases/latest):

- **Linux**: on Fedora and other RPM distributions, install the RPM:

  ```bash
  sudo dnf install ./ferry-linux-x86_64.rpm
  ```

  On Debian, Ubuntu and their derivatives, install the Debian package:

  ```bash
  sudo apt install ./ferry-linux-x86_64.deb
  ```

  Either adds Ferry to your applications. On other distributions, make
  `ferry-linux-x86_64.AppImage` executable and run it, or extract
  `ferry-linux-x86_64.tar.gz` and run `ferry`; the first time, Ferry adds
  itself, with its icon, to your applications.
- **Windows**: extract `ferry-windows-x64.zip` and run `ferry.exe`.
- **macOS**: extract `ferry-macos-app.zip` and open `Ferry.app`. The app
  isn't signed, so the first time, right-click it and choose **Open**.

Choose your iPhone in the list of paired devices. Ferry remembers it and
connects to it on startup from then on. To try Ferry without a phone, run
`ferry --demo`, which connects to a simulated iPhone.

Closing the window keeps Ferry running in the system tray, connected and
showing notifications. The tray icon opens it again, shows a red dot while
there are unread messages, and quits Ferry. Ctrl+Q (⌘Q on a Mac) quits from
the window.

## Platform notes

### Linux

Ferry needs BlueZ 5 (`bluetoothd`), which every major distribution runs. It
opens its own Bluetooth sockets and uses BlueZ only to list paired devices and
to publish its notification service, so it works alongside BlueZ's `obexd`.

- **The tray icon** is a StatusNotifierItem. KDE, Xfce and Cinnamon show it.
  GNOME needs the AppIndicator extension: on Fedora,
  `sudo dnf install gnome-shell-extension-appindicator`, then enable
  *AppIndicator and KStatusNotifierItem Support* in Extensions and log in
  again. Without a tray, closing the window quits Ferry.
- **The icon in the dock and window list** comes from a desktop entry, which
  is how Wayland finds an application's icon. The RPM and the Debian package
  install one for every user. Run from the AppImage or the tarball, Ferry
  installs one for your user when it starts; running it with `--install`
  installs it again, for example after moving the AppImage or the binary,
  and `--uninstall` removes it. Once Ferry is installed from a package, it
  removes the one it installed for your user before.
- **Data** is kept in `~/.local/share/ferry`, settings in
  `~/.config/ferry`.

### Windows

Windows 10 or later. Microsoft's Phone Link app also connects to iPhones over
the same profile; if it's connected to your iPhone, quit it before using
Ferry. Notifications appear as coming from Windows PowerShell, as Ferry
isn't installed with an app identity of its own.

### macOS

macOS 11 or later. The first time it connects, macOS asks whether Ferry may
use Bluetooth. To build the app yourself, run `macos/bundle.sh` after
`cargo build --release`.
