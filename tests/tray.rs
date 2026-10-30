//! The Linux tray icon against a fake StatusNotifierWatcher on a private session bus.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tidings::ui::tray::{Tray, Wake, Waker};
use zbus::interface;
use zbus::zvariant::OwnedValue;

struct Watcher(Arc<Mutex<Vec<String>>>);

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: String) {
        self.0.lock().unwrap().push(service);
    }

    fn register_status_notifier_host(&self, _service: String) {}

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[test]
fn registers_activates_and_shows_unread() {
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("dbus-daemon");
    let mut address = String::new();
    BufReader::new(daemon.stdout.as_mut().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim().to_string();
    // SAFETY: this test binary runs this one test, before any thread reads the
    // environment.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let items = Arc::new(Mutex::new(Vec::new()));
    let bus = runtime.block_on(async {
        zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .serve_at("/StatusNotifierWatcher", Watcher(items.clone()))
            .unwrap()
            .build()
            .await
            .unwrap()
    });

    let (waker, wakes) = Waker::new();
    let mut tray = Tray::start(runtime.handle(), waker).expect("a tray");
    let service = items.lock().unwrap().first().cloned().expect("registered");

    let pixmap = |bus: &zbus::Connection| {
        runtime.block_on(async {
            let reply = bus
                .call_method(
                    Some(service.as_str()),
                    "/StatusNotifierItem",
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &("org.kde.StatusNotifierItem", "IconPixmap"),
                )
                .await
                .unwrap();
            let value: OwnedValue = reply.body().deserialize().unwrap();
            let icons: Vec<(i32, i32, Vec<u8>)> = value.try_into().unwrap();
            icons
        })
    };
    let plain = pixmap(&bus);
    assert_eq!(
        vec![22, 32, 48, 64],
        plain.iter().map(|icon| icon.0).collect::<Vec<_>>()
    );
    assert!(plain
        .iter()
        .all(|(w, h, data)| data.len() == (w * h * 4) as usize));

    // A click opens the window.
    runtime.block_on(async {
        bus.call_method(
            Some(service.as_str()),
            "/StatusNotifierItem",
            Some("org.kde.StatusNotifierItem"),
            "Activate",
            &(0i32, 0i32),
        )
        .await
        .unwrap();
    });
    assert_eq!(Ok(Wake::Open), wakes.recv_timeout(Duration::from_secs(5)));

    // Unread messages add the badge.
    tray.set_unread(2);
    std::thread::sleep(Duration::from_millis(300));
    assert_ne!(plain, pixmap(&bus));

    drop(tray);
    let _ = daemon.kill();
    let _ = daemon.wait();
}
