//! The BlueZ backend against a fake BlueZ on a private D-Bus.
//!
//! The fake implements the parts of BlueZ Ferry uses: the object manager with an
//! adapter and a paired phone, `ProfileManager1`, and `Device1.ConnectProfile`, which
//! hands Ferry's `Profile1` object one end of a Unix socket pair, as BlueZ does with
//! RFCOMM sockets. The other end leads to the simulated iPhone, which connects back
//! for notifications through the registered notification server profile.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command as Process, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferry::bluetooth::demo::DemoPhone;
use ferry::bluetooth::linux::BlueZ;
use ferry::bluetooth::{uuid128, Backend, Link, Service};
use ferry::model::{Status, Store};
use ferry::session::{Command, Event, Outbox, Session, State, Timing};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection};

const PHONE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
const MNS: u16 = 0x1133;

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

#[derive(Clone, Debug)]
struct Registered {
    owner: String,
    path: OwnedObjectPath,
    uuid: String,
    role: String,
}

#[derive(Clone)]
struct Fake {
    phone: DemoPhone,
    profiles: Arc<Mutex<Vec<Registered>>>,
}

/// Connects `link` (the phone's side) to a new socket and returns Ferry's end.
fn socket_for(mut link: Link) -> std::os::fd::OwnedFd {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();
    let mut theirs = UnixStream::from_std(theirs).unwrap();
    tokio::spawn(async move {
        let _ = tokio::io::copy_bidirectional(&mut theirs, &mut link.stream).await;
    });
    ours.into()
}

async fn new_connection(
    connection: &Connection,
    profile: &Registered,
    fd: std::os::fd::OwnedFd,
) -> zbus::fdo::Result<()> {
    connection
        .call_method(
            Some(profile.owner.as_str()),
            profile.path.as_str(),
            Some("org.bluez.Profile1"),
            "NewConnection",
            &(
                OwnedObjectPath::try_from(PHONE).unwrap(),
                zbus::zvariant::OwnedFd::from(fd),
                HashMap::<String, Value>::new(),
            ),
        )
        .await
        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
    Ok(())
}

struct ProfileManager(Fake);

#[interface(name = "org.bluez.ProfileManager1")]
impl ProfileManager {
    async fn register_profile(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &Connection,
        path: OwnedObjectPath,
        uuid: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let role = options
            .get("Role")
            .and_then(|role| role.downcast_ref::<&str>().ok())
            .unwrap_or("")
            .to_string();
        let profile = Registered {
            owner: header.sender().unwrap().to_string(),
            path,
            uuid: uuid.clone(),
            role,
        };
        self.0.profiles.lock().unwrap().push(profile.clone());

        // The phone connects to the notification server whenever it has news.
        if uuid == uuid128(MNS) {
            let mut listener = self.0.phone.listen().await.unwrap();
            let connection = connection.clone();
            tokio::spawn(async move {
                while let Some(link) = listener.accept().await {
                    new_connection(&connection, &profile, socket_for(link))
                        .await
                        .unwrap();
                }
            });
        }
        Ok(())
    }

    async fn unregister_profile(&self, path: OwnedObjectPath) {
        self.0
            .profiles
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

struct Device(Fake);

#[interface(name = "org.bluez.Device1")]
impl Device {
    async fn connect_profile(
        &self,
        #[zbus(connection)] connection: &Connection,
        uuid: String,
    ) -> zbus::fdo::Result<()> {
        // BlueZ connects the client profile whose remote service is `uuid`.
        let (service, registered_uuid) = match uuid.as_str() {
            u if u == Service::MessageAccess.uuid() => (Service::MessageAccess, uuid.clone()),
            u if u == Service::PhoneBook.uuid() => (Service::PhoneBook, uuid128(0x112E)),
            _ => {
                return Err(zbus::fdo::Error::Failed(
                    "br-connection-profile-unavailable".to_string(),
                ))
            }
        };
        let profile = self
            .0
            .profiles
            .lock()
            .unwrap()
            .iter()
            .find(|profile| profile.uuid == registered_uuid && profile.role == "client")
            .cloned()
            .ok_or_else(|| zbus::fdo::Error::Failed("no profile".to_string()))?;

        let devices = self.0.phone.devices().await.unwrap();
        let link = self.0.phone.connect(&devices[0], service).await.unwrap();
        new_connection(connection, &profile, socket_for(link)).await
    }

    async fn disconnect_profile(&self, _uuid: String) {}

    #[zbus(property)]
    fn address(&self) -> String {
        "00:11:22:33:44:55".to_string()
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

async fn fake_bluez(address: &str, fake: Fake) -> Connection {
    let connection = zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/", zbus::fdo::ObjectManager)
        .unwrap()
        .serve_at("/org/bluez", ProfileManager(fake.clone()))
        .unwrap()
        .serve_at("/org/bluez/hci0", Adapter)
        .unwrap()
        .serve_at(PHONE, Device(fake))
        .unwrap()
        .build()
        .await
        .unwrap();
    connection
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
    let fake = Fake {
        phone: DemoPhone::new().with_reply_delay(Duration::from_millis(200)),
        profiles: Arc::new(Mutex::new(Vec::new())),
    };
    let _bluez = fake_bluez(&bus.address, fake.clone()).await;

    let backend = Arc::new(BlueZ::on_bus(&bus.address));
    let devices = backend.devices().await.unwrap();
    assert_eq!(1, devices.len());
    assert_eq!("Test iPhone", devices[0].name);
    assert_eq!(PHONE, devices[0].id);
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
    assert_eq!(14, store.lock().unwrap().messages.len());

    // Contacts arrive over a second channel.
    let contacts = store.clone();
    if contacts.lock().unwrap().contacts.is_empty() {
        until(&mut events, "contacts", |_| {
            !contacts.lock().unwrap().contacts.is_empty()
        })
        .await;
    }

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

    // Disconnecting unregisters the notification server.
    session.send(Command::Disconnect);
    until(&mut events, "disconnection", |event| {
        matches!(event, Event::State(State::Disconnected))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let roles: Vec<String> = fake
        .profiles
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.role.clone())
        .collect();
    assert_eq!(vec!["client", "client"], roles);
}
