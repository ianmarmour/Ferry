//! The BlueZ backend against a fake BlueZ on a private D-Bus.
//!
//! The fake implements the parts of BlueZ Tidings uses: the object manager with an
//! adapter and a paired phone, and `ProfileManager1`, where Tidings publishes its
//! notification server record. Sockets come from [`TestSockets`], which answers SDP
//! searches and connects "RFCOMM channels" to the simulated iPhone over Unix sockets.
//! The phone connects back to Tidings' listening socket for notifications.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Child, Command as Process, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tidings::bluetooth::demo::DemoPhone;
use tidings::bluetooth::linux::{BlueZ, Sockets, MNS_RECORD_UUID};
use tidings::bluetooth::{sdp, Backend, Link, Service};
use tidings::model::{Status, Store};
use tidings::session::{Command, Event, Outbox, Session, State, Timing};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{interface, Connection};

const PHONE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
const ADDRESS: &str = "00:11:22:33:44:55";
const MAS_CHANNEL: u8 = 2;
const PSE_CHANNEL: u8 = 19;
const MNS_CHANNEL: u8 = 9;

/// A private bus, stopped when dropped.
struct Bus {
    daemon: Child,
    address: String,
}

impl Bus {
    fn start() -> Bus {
        let mut daemon = Process::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon");
        let mut address = String::new();
        BufReader::new(daemon.stdout.as_mut().unwrap())
            .read_line(&mut address)
            .unwrap();
        Bus {
            daemon,
            address: address.trim().to_string(),
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
    }
}

/// Connects `link` (the phone's side) to a new socket and returns Tidings' end.
fn socket_for(runtime: &tokio::runtime::Handle, mut link: Link) -> OwnedFd {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();
    runtime.spawn(async move {
        let mut theirs = UnixStream::from_std(theirs).unwrap();
        let _ = tokio::io::copy_bidirectional(&mut theirs, &mut link.stream).await;
    });
    ours.into()
}

/// Sockets leading to the simulated iPhone.
struct TestSockets {
    phone: DemoPhone,
    runtime: tokio::runtime::Handle,
    directory: PathBuf,
    connections: Mutex<Vec<u8>>,
}

impl Sockets for TestSockets {
    fn search(&self, address: &str, uuid: u16) -> io::Result<Vec<sdp::Record>> {
        assert_eq!(ADDRESS, address);
        let lists = match uuid {
            0x1132 => sdp::sequence(&[
                sdp::rfcomm_service(0x1132, 7, Some(0x01)),
                sdp::rfcomm_service(0x1132, MAS_CHANNEL, Some(0x0E)),
            ]),
            0x112F => sdp::sequence(&[sdp::rfcomm_service(0x112F, PSE_CHANNEL, None)]),
            _ => sdp::sequence(&[]),
        };
        Ok(sdp::records(&lists).unwrap())
    }

    fn connect(&self, address: &str, channel: u8) -> io::Result<OwnedFd> {
        assert_eq!(ADDRESS, address);
        self.connections.lock().unwrap().push(channel);
        let service = match channel {
            MAS_CHANNEL => Service::MessageAccess,
            PSE_CHANNEL => Service::PhoneBook,
            _ => return Err(io::Error::from_raw_os_error(libc::ECONNREFUSED)),
        };
        let phone = self.phone.clone();
        let link = self.runtime.block_on(async move {
            let devices = phone.devices().await.unwrap();
            phone.connect(&devices[0], service).await.unwrap()
        });
        Ok(socket_for(&self.runtime, link))
    }

    fn listen(&self) -> io::Result<(OwnedFd, u8)> {
        let path = self.directory.join(format!("mns-{}", rand_suffix()));
        let listener = std::os::unix::net::UnixListener::bind(&path)?;

        // The phone connects to the listening socket whenever it has news.
        let phone = self.phone.clone();
        let mut notifications = self.runtime.block_on(phone.listen()).unwrap();
        self.runtime.spawn(async move {
            while let Some(mut link) = notifications.accept().await {
                let mut socket = UnixStream::connect(&path).await.unwrap();
                tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut link.stream).await;
                });
            }
        });
        Ok((listener.into(), MNS_CHANNEL))
    }
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[derive(Clone, Debug)]
struct Registered {
    path: OwnedObjectPath,
    uuid: String,
    role: String,
    record: String,
}

#[derive(Clone, Default)]
struct Profiles(Arc<Mutex<Vec<Registered>>>);

struct ProfileManager(Profiles);

#[interface(name = "org.bluez.ProfileManager1")]
impl ProfileManager {
    async fn register_profile(
        &self,
        path: OwnedObjectPath,
        uuid: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let text = |name: &str| {
            options
                .get(name)
                .and_then(|value| value.downcast_ref::<&str>().ok())
                .unwrap_or("")
                .to_string()
        };
        let mut profiles = self.0 .0.lock().unwrap();
        // Like BlueZ, refuse a UUID that is already registered, as obexd's would be.
        if profiles.iter().any(|profile| profile.uuid == uuid) || uuid.starts_with("00001132") {
            return Err(zbus::fdo::Error::Failed(
                "UUID already registered".to_string(),
            ));
        }
        profiles.push(Registered {
            path,
            uuid,
            role: text("Role"),
            record: text("ServiceRecord"),
        });
        Ok(())
    }

    async fn unregister_profile(&self, path: OwnedObjectPath) {
        self.0
             .0
            .lock()
            .unwrap()
            .retain(|profile| profile.path != path);
    }
}

struct Adapter;

#[interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
}

struct Device;

#[interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn address(&self) -> String {
        ADDRESS.to_string()
    }

    #[zbus(property)]
    fn alias(&self) -> String {
        "Test iPhone".to_string()
    }

    #[zbus(property)]
    fn paired(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn icon(&self) -> String {
        "phone".to_string()
    }

    #[zbus(property, name = "UUIDs")]
    fn uuids(&self) -> Vec<String> {
        vec![Service::MessageAccess.uuid(), Service::PhoneBook.uuid()]
    }
}

async fn fake_bluez(address: &str, profiles: Profiles) -> Connection {
    zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/", zbus::fdo::ObjectManager)
        .unwrap()
        .serve_at("/org/bluez", ProfileManager(profiles))
        .unwrap()
        .serve_at("/org/bluez/hci0", Adapter)
        .unwrap()
        .serve_at(PHONE, Device)
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn until(
    events: &mut mpsc::UnboundedReceiver<Event>,
    what: &str,
    mut predicate: impl FnMut(&Event) -> bool,
) -> Event {
    let waiting = async {
        loop {
            let event = events.recv().await.expect("session running");
            if let Event::Notice(notice) = &event {
                panic!("{}", notice);
            }
            if predicate(&event) {
                return event;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {}", what))
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_contacts_and_notifications_through_bluez() {
    let bus = Bus::start();
    let profiles = Profiles::default();
    let _bluez = fake_bluez(&bus.address, profiles.clone()).await;

    let directory = std::env::temp_dir().join(format!("tidings-bluez-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let sockets = Arc::new(TestSockets {
        phone: DemoPhone::new().with_reply_delay(Duration::from_millis(200)),
        runtime: tokio::runtime::Handle::current(),
        directory: directory.clone(),
        connections: Mutex::new(Vec::new()),
    });

    let backend = Arc::new(BlueZ::with(&bus.address, sockets.clone()));
    let devices = backend.devices().await.unwrap();
    assert_eq!(1, devices.len());
    assert_eq!("Test iPhone", devices[0].name);
    assert_eq!(ADDRESS, devices[0].address);
    assert!(devices[0].phone);

    let store = Arc::new(Mutex::new(Store::in_memory()));
    let (sender, mut events) = mpsc::unbounded_channel();
    let session = Session::start(
        backend,
        store.clone(),
        None,
        Timing::default(),
        Outbox::new(sender, || {}),
    );

    session.send(Command::Connect(devices[0].clone()));
    until(&mut events, "the first sync", |event| {
        matches!(
            event,
            Event::State(State::Connected {
                syncing: false,
                notifications: true,
                ..
            })
        )
    })
    .await;
    assert_eq!(17, store.lock().unwrap().messages.len());

    // The notification record is published under Tidings' own UUID, with its channel.
    {
        let profiles = profiles.0.lock().unwrap();
        assert_eq!(1, profiles.len());
        assert_eq!(MNS_RECORD_UUID, profiles[0].uuid);
        assert_eq!("server", profiles[0].role);
        assert!(profiles[0].record.contains(r#"<uuid value="0x1133"/>"#));
        assert!(profiles[0].record.contains(r#"<uint8 value="0x09"/>"#));
    }

    // Contacts arrive over a second channel.
    let contacts = store.clone();
    if contacts.lock().unwrap().contacts.is_empty() {
        until(&mut events, "contacts", |_| {
            !contacts.lock().unwrap().contacts.is_empty()
        })
        .await;
    }
    let mut channels = sockets.connections.lock().unwrap().clone();
    channels.sort();
    assert_eq!(vec![MAS_CHANNEL, PSE_CHANNEL], channels);

    // Sending, then the reply through the notification server.
    let id = store
        .lock()
        .unwrap()
        .add_outgoing("+14155550104", "Over BlueZ", 0);
    session.send(Command::Send {
        id,
        address: "+14155550104".to_string(),
        text: "Over BlueZ".to_string(),
    });
    let Event::Received(ids) = until(&mut events, "the reply", |event| {
        matches!(event, Event::Received(_))
    })
    .await
    else {
        unreachable!()
    };
    {
        let store = store.lock().unwrap();
        assert_eq!("Sounds good!", store.get(ids[0]).unwrap().text);
        assert_eq!(Status::Delivered, store.get(id).unwrap().status);
    }

    // Disconnecting removes the notification record.
    session.send(Command::Disconnect);
    until(&mut events, "disconnection", |event| {
        matches!(event, Event::State(State::Disconnected))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(profiles.0.lock().unwrap().is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
