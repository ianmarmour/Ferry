//! ## Linux: BlueZ
//!
//! Tidings talks to the phone over its own Bluetooth sockets, the way BlueZ's `obexd`
//! MAP client does, and uses BlueZ (over D-Bus) only for what needs the daemon:
//!
//! * listing the paired devices (`org.freedesktop.DBus.ObjectManager`); and
//! * publishing the SDP record of the Message Notification Server
//!   (`org.bluez.ProfileManager1`).
//!
//! It does not register BlueZ profiles for the MAP and PBAP services themselves: BlueZ
//! allows each service UUID to be registered once, and desktops usually run `obexd`,
//! which claims them. Instead:
//!
//! * the phone's services are found with an SDP query over L2CAP and reached over
//!   RFCOMM sockets, with an encrypted, authenticated link;
//! * notifications arrive on an RFCOMM socket Tidings listens on, advertised by an SDP
//!   record registered under a UUID of Tidings' own.
//!
//! Sockets are made non-blocking and driven by tokio. How they are opened is behind
//! [`Sockets`], so the backend can be tested without Bluetooth hardware.
//!
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, OnceCell};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

use super::{
    sdp, Backend, BoxFuture, Device, Link, Listener, Service, MNS_FEATURES, MNS_SERVICE_NAME,
};

const BLUEZ: &str = "org.bluez";
const DEVICE: &str = "org.bluez.Device1";
const ADAPTER: &str = "org.bluez.Adapter1";

/// The object of the notification server record's profile.
const MNS_PROFILE: &str = "/org/tidings/profile/mns";

/// The UUID the notification server record is registered under. BlueZ only allows one
/// registration per UUID, and `obexd` usually holds the MNS one; the record itself
/// still advertises the MNS service class.
pub const MNS_RECORD_UUID: &str = "1f7b5c2e-8d4a-4e61-9b0f-6a3c2d1e5f80";

/// Opens Bluetooth sockets. [`RawSockets`] is the real thing.
pub trait Sockets: Send + Sync + 'static {
    /// The SDP records of the services of class `uuid` on the device at `address`.
    fn search(&self, address: &str, uuid: u16) -> io::Result<Vec<sdp::Record>>;

    /// Connects to RFCOMM `channel` of the device at `address`.
    fn connect(&self, address: &str, channel: u8) -> io::Result<OwnedFd>;

    /// Listens on a free RFCOMM channel; returns the socket and the channel.
    fn listen(&self) -> io::Result<(OwnedFd, u8)>;
}

/// The BlueZ backend. See [the module documentation](self).
pub struct BlueZ {
    connection: OnceCell<Connection>,
    address: Option<String>,
    sockets: Arc<dyn Sockets>,
}

impl Default for BlueZ {
    fn default() -> BlueZ {
        BlueZ::new()
    }
}

impl BlueZ {
    /// Creates the backend; nothing happens until it is used.
    pub fn new() -> BlueZ {
        BlueZ {
            connection: OnceCell::new(),
            address: None,
            sockets: Arc::new(RawSockets),
        }
    }

    /// Uses the BlueZ reachable on the D-Bus at `address` and opens sockets with
    /// `sockets`, for tests.
    pub fn with(address: &str, sockets: Arc<dyn Sockets>) -> BlueZ {
        BlueZ {
            connection: OnceCell::new(),
            address: Some(address.to_string()),
            sockets,
        }
    }

    async fn connection(&self) -> anyhow::Result<Connection> {
        self.connection
            .get_or_try_init(|| async {
                let connection = match &self.address {
                    Some(address) => {
                        zbus::connection::Builder::address(address.as_str())?
                            .build()
                            .await?
                    }
                    None => Connection::system()
                        .await
                        .context("cannot reach the system D-Bus; is BlueZ installed?")?,
                };
                connection.object_server().at(MNS_PROFILE, Profile).await?;
                Ok(connection)
            })
            .await
            .cloned()
    }
}

/// The `org.bluez.Profile1` object of the notification server record. BlueZ does not
/// listen for this profile, so it never hands over connections.
struct Profile;

#[zbus::interface(name = "org.bluez.Profile1")]
impl Profile {
    fn release(&self) {}

    fn new_connection(
        &self,
        _device: OwnedObjectPath,
        _fd: zbus::zvariant::OwnedFd,
        _properties: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Tidings accepts connections itself".to_string(),
        ))
    }

    fn request_disconnection(&self, _device: OwnedObjectPath) {}
}

/// Turns D-Bus errors from BlueZ into something a person can act on.
fn explain(error: &zbus::Error) -> anyhow::Error {
    let text = error.to_string();
    if text.contains("ServiceUnknown") || text.contains("org.bluez was not provided") {
        anyhow!("BlueZ is not running; start it with `systemctl start bluetooth`")
    } else {
        anyhow!("{}", text)
    }
}

/// Turns socket errors into something a person can act on.
fn explain_socket(error: io::Error, what: &str) -> anyhow::Error {
    let hint = match error.raw_os_error() {
        Some(libc::EHOSTDOWN | libc::ETIMEDOUT | libc::EHOSTUNREACH) => {
            "the phone is out of range, or its Bluetooth is off"
        }
        Some(libc::ECONNREFUSED) => "the phone refused the connection",
        Some(libc::EACCES | libc::EPERM) => {
            "the phone did not accept the pairing; remove it on both sides and pair again"
        }
        Some(libc::EAFNOSUPPORT | libc::EPROTONOSUPPORT) => {
            "this system has no Bluetooth support in the kernel"
        }
        Some(libc::EBUSY) => "the phone is busy with another connection",
        _ => return anyhow!("{}: {}", what, error),
    };
    anyhow!("{} ({}: {})", hint, what, error)
}

fn get<'a, T>(properties: &'a HashMap<String, OwnedValue>, name: &str) -> Option<T>
where
    T: TryFrom<&'a Value<'a>>,
    zbus::zvariant::Error: From<T::Error>,
{
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<T>().ok())
}

/// Picks the record to connect to: for MAP, a MAS instance that carries SMS.
fn choose(records: &[sdp::Record], service: Service) -> Option<u8> {
    let sms = |record: &sdp::Record| {
        record
            .uint(map::sdp::ATTRIBUTE_SUPPORTED_MESSAGE_TYPES)
            // Bits 1 and 2 are SMS_GSM and SMS_CDMA.
            .is_none_or(|types| types & 0b110 != 0)
    };
    records
        .iter()
        .filter(|record| service != Service::MessageAccess || sms(record))
        .chain(records)
        .find_map(sdp::Record::rfcomm_channel)
}

/// The Message Notification Server record, in BlueZ's XML format.
fn mns_record(channel: u8) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" ?>
<record>
  <attribute id="0x0001">
    <sequence><uuid value="0x{class:04x}"/></sequence>
  </attribute>
  <attribute id="0x0004">
    <sequence>
      <sequence><uuid value="0x0100"/></sequence>
      <sequence><uuid value="0x0003"/><uint8 value="0x{channel:02x}"/></sequence>
      <sequence><uuid value="0x0008"/></sequence>
    </sequence>
  </attribute>
  <attribute id="0x0005">
    <sequence><uuid value="0x1002"/></sequence>
  </attribute>
  <attribute id="0x0009">
    <sequence>
      <sequence><uuid value="0x{profile:04x}"/><uint16 value="0x{version:04x}"/></sequence>
    </sequence>
  </attribute>
  <attribute id="0x0100">
    <text value="{name}"/>
  </attribute>
  <attribute id="0x{features_id:04x}">
    <uint32 value="0x{features:08x}"/>
  </attribute>
</record>"#,
        class = map::sdp::MESSAGE_NOTIFICATION_SERVER_UUID16,
        profile = map::sdp::MESSAGE_ACCESS_PROFILE_UUID16,
        version = map::sdp::PROFILE_VERSION,
        name = MNS_SERVICE_NAME,
        features_id = map::sdp::ATTRIBUTE_MAP_SUPPORTED_FEATURES,
        features = MNS_FEATURES,
    )
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| anyhow!("Bluetooth task failed: {}", error))?
}

impl Backend for BlueZ {
    fn name(&self) -> String {
        "BlueZ".to_string()
    }

    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>> {
        Box::pin(async move {
            let connection = self.connection().await?;
            let manager = zbus::fdo::ObjectManagerProxy::builder(&connection)
                .destination(BLUEZ)?
                .path("/")?
                .build()
                .await?;
            let objects = manager
                .get_managed_objects()
                .await
                .map_err(|error| explain(&error.into()))?;

            let adapters: Vec<bool> = objects
                .values()
                .filter_map(|interfaces| interfaces.get(ADAPTER))
                .map(|adapter| get::<bool>(adapter, "Powered").unwrap_or(false))
                .collect();
            if adapters.is_empty() {
                anyhow::bail!("no Bluetooth adapter found");
            }
            if !adapters.contains(&true) {
                anyhow::bail!("Bluetooth is turned off");
            }

            let mas = Service::MessageAccess.uuid();
            let mut devices: Vec<Device> = objects
                .iter()
                .filter_map(|(path, interfaces)| {
                    let properties = interfaces.get(DEVICE)?;
                    if !get::<bool>(properties, "Paired").unwrap_or(false) {
                        return None;
                    }
                    let address: String = get::<&str>(properties, "Address")?.to_string();
                    let name = get::<&str>(properties, "Alias")
                        .or_else(|| get::<&str>(properties, "Name"))
                        .unwrap_or(&address)
                        .to_string();
                    let icon = get::<&str>(properties, "Icon").unwrap_or_default();
                    let offers_map = properties
                        .get("UUIDs")
                        .and_then(|uuids| uuids.downcast_ref::<zbus::zvariant::Array>().ok())
                        .is_some_and(|uuids| {
                            uuids.iter().any(|uuid| {
                                uuid.downcast_ref::<&str>()
                                    .is_ok_and(|uuid| uuid.eq_ignore_ascii_case(&mas))
                            })
                        });

                    Some(Device {
                        id: path.to_string(),
                        name,
                        address,
                        phone: icon == "phone" || offers_map,
                    })
                })
                .collect();
            devices.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(devices)
        })
    }

    fn connect<'a>(
        &'a self,
        device: &'a Device,
        service: Service,
    ) -> BoxFuture<'a, anyhow::Result<Link>> {
        Box::pin(async move {
            let sockets = self.sockets.clone();
            let address = device.address.clone();
            let fd = blocking(move || {
                let records = sockets
                    .search(&address, service.uuid16())
                    .map_err(|error| explain_socket(error, "service search"))?;
                let channel = choose(&records, service).ok_or_else(|| {
                    anyhow!("the phone doesn't offer this service over RFCOMM; check that Show Notifications and Sync Contacts are on for this computer in the iPhone's Bluetooth settings")
                })?;
                log::info!("connecting to {:?} on RFCOMM channel {}", service, channel);
                sockets
                    .connect(&address, channel)
                    .map_err(|error| explain_socket(error, "RFCOMM connection"))
            })
            .await?;
            Ok(link(fd)?)
        })
    }

    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>> {
        Box::pin(async move {
            let connection = self.connection().await?;
            let sockets = self.sockets.clone();
            let (socket, channel) = blocking(move || {
                sockets
                    .listen()
                    .map_err(|error| explain_socket(error, "RFCOMM listener"))
            })
            .await?;
            set_nonblocking(socket.as_raw_fd())?;
            // SAFETY: the descriptor is owned by `socket`, which the `AsyncFd` owns
            // until dropped, and is never closed or replaced in the meantime.
            let socket = unsafe { AsyncFd::register(socket) }?;

            let options = HashMap::from([
                ("Name", Value::from(MNS_SERVICE_NAME)),
                ("Role", Value::from("server")),
                ("ServiceRecord", Value::from(mns_record(channel))),
                ("RequireAuthentication", Value::from(true)),
                ("RequireAuthorization", Value::from(false)),
                ("AutoConnect", Value::from(false)),
            ]);
            connection
                .call_method(
                    Some(BLUEZ),
                    "/org/bluez",
                    Some("org.bluez.ProfileManager1"),
                    "RegisterProfile",
                    &(ObjectPath::try_from(MNS_PROFILE)?, MNS_RECORD_UUID, options),
                )
                .await
                .map_err(|error| explain(&error))
                .context("cannot publish the notification service")?;
            log::info!(
                "notification service published on RFCOMM channel {}",
                channel
            );

            let (sender, incoming) = mpsc::unbounded_channel();
            let accepting = tokio::spawn(accept(socket, sender));
            Ok(Listener::new(
                incoming,
                Registration {
                    connection,
                    accepting,
                },
            ))
        })
    }
}

/// Accepts the phone's notification connections.
async fn accept(socket: AsyncFd<OwnedFd>, incoming: mpsc::UnboundedSender<Link>) {
    loop {
        let accepted = socket
            .async_io(tokio::io::Interest::READABLE, |socket| {
                // SAFETY: plain accept on a listening socket we own.
                let fd = unsafe {
                    libc::accept4(
                        socket.as_raw_fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        libc::SOCK_CLOEXEC,
                    )
                };
                if fd < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    // SAFETY: `accept4` returned a new descriptor we now own.
                    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
                }
            })
            .await;

        match accepted.and_then(link) {
            Ok(link) => {
                log::info!("notification connection accepted");
                if incoming.send(link).is_err() {
                    return;
                }
            }
            Err(error) => {
                log::warn!("cannot accept a notification connection: {}", error);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Stops listening and removes the record when its [`Listener`] is dropped.
struct Registration {
    connection: Connection,
    accepting: tokio::task::JoinHandle<()>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.accepting.abort();
        let connection = self.connection.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let Ok(path) = ObjectPath::try_from(MNS_PROFILE) else {
                    return;
                };
                let result = connection
                    .call_method(
                        Some(BLUEZ),
                        "/org/bluez",
                        Some("org.bluez.ProfileManager1"),
                        "UnregisterProfile",
                        &(path,),
                    )
                    .await;
                if let Err(error) = result {
                    log::debug!("cannot unregister the notification service: {}", error);
                }
            });
        }
    }
}

const BTPROTO_L2CAP: libc::c_int = 0;
const BTPROTO_RFCOMM: libc::c_int = 3;
const SOL_BLUETOOTH: libc::c_int = 274;
const BT_SECURITY: libc::c_int = 4;
const BT_SECURITY_MEDIUM: u8 = 2;
const SDP_PSM: u16 = 1;

#[repr(C)]
struct SockaddrL2 {
    family: libc::sa_family_t,
    psm: u16,
    bdaddr: [u8; 6],
    cid: u16,
    bdaddr_type: u8,
}

#[repr(C)]
struct SockaddrRc {
    family: libc::sa_family_t,
    bdaddr: [u8; 6],
    channel: u8,
}

#[repr(C)]
struct BtSecurity {
    level: u8,
    key_size: u8,
}

/// `AA:BB:CC:DD:EE:FF` to the little endian `bdaddr_t` of the kernel.
fn bdaddr(address: &str) -> io::Result<[u8; 6]> {
    let bytes: Vec<u8> = address
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<_, _>>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad Bluetooth address"))?;
    let mut bdaddr: [u8; 6] = bytes
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad Bluetooth address"))?;
    bdaddr.reverse();
    Ok(bdaddr)
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn bluetooth_socket(kind: libc::c_int, protocol: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: creating a socket has no preconditions.
    let fd =
        check(unsafe { libc::socket(libc::AF_BLUETOOTH, kind | libc::SOCK_CLOEXEC, protocol) })?;
    // SAFETY: `socket` returned a new descriptor we now own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn set_option<T>(fd: RawFd, level: libc::c_int, name: libc::c_int, value: &T) -> io::Result<()> {
    // SAFETY: `value` is valid for reads of its size.
    check(unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (value as *const T).cast(),
            std::mem::size_of::<T>() as libc::socklen_t,
        )
    })
    .map(drop)
}

/// Requires an encrypted link with an authenticated (paired) device.
fn require_pairing(fd: RawFd) -> io::Result<()> {
    let security = BtSecurity {
        level: BT_SECURITY_MEDIUM,
        key_size: 0,
    };
    set_option(fd, SOL_BLUETOOTH, BT_SECURITY, &security)
}

fn set_timeouts(fd: RawFd, timeout: Duration) -> io::Result<()> {
    let time = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: 0,
    };
    set_option(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO, &time)?;
    set_option(fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO, &time)
}

fn connect_to<T>(fd: RawFd, address: &T) -> io::Result<()> {
    // SAFETY: `address` is a valid socket address of its size.
    check(unsafe {
        libc::connect(
            fd,
            (address as *const T).cast(),
            std::mem::size_of::<T>() as libc::socklen_t,
        )
    })
    .map(drop)
}

/// The kernel's Bluetooth sockets.
pub struct RawSockets;

impl Sockets for RawSockets {
    fn search(&self, address: &str, uuid: u16) -> io::Result<Vec<sdp::Record>> {
        let socket = bluetooth_socket(libc::SOCK_SEQPACKET, BTPROTO_L2CAP)?;
        let fd = socket.as_raw_fd();
        set_timeouts(fd, Duration::from_secs(15))?;
        connect_to(
            fd,
            &SockaddrL2 {
                family: libc::AF_BLUETOOTH as libc::sa_family_t,
                psm: SDP_PSM.to_le(),
                bdaddr: bdaddr(address)?,
                cid: 0,
                bdaddr_type: 0,
            },
        )?;

        let mut lists = Vec::new();
        let mut continuation = Vec::new();
        let mut buffer = vec![0; 65536];
        for transaction in 1..=64u16 {
            let request = sdp::search_request(transaction, uuid, &continuation);
            // SAFETY: `request` is valid for reads of its length.
            check(unsafe {
                libc::send(fd, request.as_ptr().cast(), request.len(), 0) as libc::c_int
            })?;
            // SAFETY: `buffer` is valid for writes of its length.
            let received = unsafe { libc::recv(fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
            let received = check(received as libc::c_int)? as usize;

            let (part, next) = sdp::search_response(&buffer[..received])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            lists.extend_from_slice(part);
            if next.is_empty() {
                return sdp::records(&lists).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "malformed SDP records")
                });
            }
            continuation = next.to_vec();
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "endless SDP response",
        ))
    }

    fn connect(&self, address: &str, channel: u8) -> io::Result<OwnedFd> {
        let socket = bluetooth_socket(libc::SOCK_STREAM, BTPROTO_RFCOMM)?;
        require_pairing(socket.as_raw_fd())?;
        connect_to(
            socket.as_raw_fd(),
            &SockaddrRc {
                family: libc::AF_BLUETOOTH as libc::sa_family_t,
                bdaddr: bdaddr(address)?,
                channel,
            },
        )?;
        Ok(socket)
    }

    fn listen(&self) -> io::Result<(OwnedFd, u8)> {
        let socket = bluetooth_socket(libc::SOCK_STREAM, BTPROTO_RFCOMM)?;
        let fd = socket.as_raw_fd();
        require_pairing(fd)?;

        let mut address = SockaddrRc {
            family: libc::AF_BLUETOOTH as libc::sa_family_t,
            bdaddr: [0; 6],
            channel: 0,
        };
        // SAFETY: `address` is a valid socket address of its size.
        check(unsafe {
            libc::bind(
                fd,
                (&address as *const SockaddrRc).cast(),
                std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
            )
        })?;
        // Listening on channel 0 makes the kernel pick a free channel.
        // SAFETY: plain listen on a socket we own.
        check(unsafe { libc::listen(fd, 4) })?;

        let mut length = std::mem::size_of::<SockaddrRc>() as libc::socklen_t;
        // SAFETY: `address` and `length` are valid for writes of the sizes given.
        check(unsafe {
            libc::getsockname(fd, (&mut address as *mut SockaddrRc).cast(), &mut length)
        })?;
        Ok((socket, address.channel))
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is a valid, open descriptor.
    unsafe {
        let flags = check(libc::fcntl(fd, libc::F_GETFL))?;
        check(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
    }
    Ok(())
}

/// Wraps a connected socket, finding out whether it is RFCOMM or L2CAP.
fn link(fd: OwnedFd) -> io::Result<Link> {
    let raw = fd.as_raw_fd();
    set_nonblocking(raw)?;

    let packet_mtu = (socket_type(raw) == Some(libc::SOCK_SEQPACKET)).then(|| l2cap_mtu(raw));
    // SAFETY: the descriptor is owned by `fd`, which the `AsyncFd` owns until dropped,
    // and is never closed or replaced in the meantime.
    let fd = unsafe { AsyncFd::register(fd) }?;
    Ok(Link {
        stream: Box::new(FdStream(fd)),
        packet_mtu,
    })
}

fn socket_type(fd: RawFd) -> Option<i32> {
    let mut value: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `value` and `length` are valid for writes of the sizes given.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut value as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    (result == 0).then_some(value)
}

/// The usable MTU of an L2CAP socket: the smaller of the send and receive MTUs.
fn l2cap_mtu(fd: RawFd) -> u16 {
    const BT_SNDMTU: libc::c_int = 12;
    const BT_RCVMTU: libc::c_int = 13;

    let option = |name| {
        let mut value: u16 = 0;
        let mut length = std::mem::size_of::<u16>() as libc::socklen_t;
        // SAFETY: `value` and `length` are valid for writes of the sizes given.
        let result = unsafe {
            libc::getsockopt(
                fd,
                SOL_BLUETOOTH,
                name,
                (&mut value as *mut u16).cast(),
                &mut length,
            )
        };
        (result == 0 && value > 0).then_some(value)
    };
    match (option(BT_SNDMTU), option(BT_RCVMTU)) {
        (Some(send), Some(receive)) => send.min(receive),
        (Some(mtu), None) | (None, Some(mtu)) => mtu,
        (None, None) => 672,
    }
}

/// A non-blocking socket driven by tokio.
struct FdStream(AsyncFd<OwnedFd>);

impl AsyncRead for FdStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            let result = guard.try_io(|fd| {
                // SAFETY: `unfilled` is valid for writes of its length.
                let read = unsafe {
                    libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len())
                };
                if read < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(read as usize)
                }
            });
            match result {
                Ok(Ok(read)) => {
                    buf.advance(read);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for FdStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            let result = guard.try_io(|fd| {
                // SAFETY: `data` is valid for reads of its length.
                let written =
                    unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
                if written < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(written as usize)
                }
            });
            match result {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn streams_over_sockets() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let a = link(OwnedFd::from(a)).unwrap();
        let b = link(OwnedFd::from(b)).unwrap();
        assert_eq!(None, a.packet_mtu);

        let (mut a, mut b) = (a.stream, b.stream);
        a.write_all(b"hello").await.unwrap();
        let mut buffer = [0; 5];
        b.read_exact(&mut buffer).await.unwrap();
        assert_eq!(b"hello", &buffer);

        drop(a);
        assert_eq!(0, b.read(&mut buffer).await.unwrap());
    }

    #[test]
    fn addresses_are_little_endian() {
        assert_eq!(
            [0xFC, 0x96, 0xB5, 0x66, 0xFA, 0x44],
            bdaddr("44:FA:66:B5:96:FC").unwrap()
        );
        assert!(bdaddr("44:FA:66").is_err());
    }

    #[test]
    fn prefers_mas_instances_with_sms() {
        let lists = sdp::sequence(&[
            sdp::rfcomm_service(0x1132, 3, Some(0x01)),
            sdp::rfcomm_service(0x1132, 4, Some(0x0E)),
        ]);
        let records = sdp::records(&lists).unwrap();
        assert_eq!(Some(4), choose(&records, Service::MessageAccess));
        assert_eq!(Some(3), choose(&records, Service::PhoneBook));
        assert_eq!(Some(3), choose(&records[..1], Service::MessageAccess));
    }

    #[test]
    fn notification_record_has_the_channel() {
        let record = mns_record(0x17);
        assert!(record.contains(r#"<uuid value="0x1133"/>"#));
        assert!(record.contains(r#"<uint8 value="0x17"/>"#));
        assert!(record.contains(r#"<attribute id="0x0317">"#));
    }
}
