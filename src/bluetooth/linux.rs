//! ## Linux: BlueZ over D-Bus
//!
//! BlueZ does the SDP work for us through its profile API (`org.bluez.ProfileManager1`):
//!
//! * Ferry registers *client* profiles for the Message Access Server and the Phone Book
//!   Server. `Device1.ConnectProfile` then looks up the phone's SDP record, opens the
//!   RFCOMM channel (or the L2CAP channel of a GOEP 2.0 record) and hands the socket to
//!   Ferry's `Profile1.NewConnection`.
//! * For notifications, Ferry registers a *server* profile for the Message Notification
//!   Server. BlueZ publishes its SDP record, listens, and hands over the phone's
//!   connections the same way.
//!
//! Sockets are made non-blocking and driven by tokio.
//!
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{ready, Context, Poll};
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot, OnceCell};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

use super::{uuid128, Backend, BoxFuture, Device, Link, Listener, Service, MNS_SERVICE_NAME, PHONE_BOOK_CLIENT_UUID16};

const BLUEZ: &str = "org.bluez";
const DEVICE: &str = "org.bluez.Device1";
const ADAPTER: &str = "org.bluez.Adapter1";

const MAS_PROFILE: &str = "/org/ferry/profile/mas";
const PBAP_PROFILE: &str = "/org/ferry/profile/pbap";
const MNS_PROFILE: &str = "/org/ferry/profile/mns";

/// The BlueZ backend. See [the module documentation](self).
pub struct BlueZ {
    inner: OnceCell<Arc<Inner>>,
    address: Option<String>,
}

struct Inner {
    connection: Connection,
    /// Outgoing connections waiting for their socket, by service and device path.
    waiting: Mutex<HashMap<(String, String), oneshot::Sender<OwnedFd>>>,
    /// Where incoming notification connections go while listening.
    notifications: Mutex<Option<mpsc::UnboundedSender<Link>>>,
}

/// An `org.bluez.Profile1` object.
struct Profile {
    inner: std::sync::Weak<Inner>,
    service: Option<Service>,
}

#[zbus::interface(name = "org.bluez.Profile1")]
impl Profile {
    fn release(&self) {}

    fn new_connection(
        &self,
        device: OwnedObjectPath,
        fd: zbus::zvariant::OwnedFd,
        _properties: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let fd: OwnedFd = fd.into();
        let inner = self
            .inner
            .upgrade()
            .ok_or_else(|| zbus::fdo::Error::Failed("shutting down".to_string()))?;

        match self.service {
            Some(service) => {
                let key = (service.uuid(), device.to_string());
                let waiting = inner.waiting.lock().unwrap().remove(&key);
                match waiting {
                    Some(sender) => {
                        let _ = sender.send(fd);
                        Ok(())
                    }
                    None => Err(zbus::fdo::Error::Failed("no connection was requested".to_string())),
                }
            }
            None => {
                log::info!("notification connection from {}", device.as_str());
                let link = link(fd).map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
                let sender = inner.notifications.lock().unwrap().clone();
                match sender.map(|sender| sender.send(link)) {
                    Some(Ok(())) => Ok(()),
                    _ => Err(zbus::fdo::Error::Failed("not listening".to_string())),
                }
            }
        }
    }

    fn request_disconnection(&self, _device: OwnedObjectPath) {}

    fn cancel(&self) {}
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
            inner: OnceCell::new(),
            address: None,
        }
    }

    /// Uses the BlueZ reachable on the D-Bus at `address` instead of the system bus,
    /// for tests.
    pub fn on_bus(address: &str) -> BlueZ {
        BlueZ {
            inner: OnceCell::new(),
            address: Some(address.to_string()),
        }
    }

    async fn inner(&self) -> anyhow::Result<Arc<Inner>> {
        self.inner
            .get_or_try_init(|| async {
                let connection = match &self.address {
                    Some(address) => zbus::connection::Builder::address(address.as_str())?.build().await?,
                    None => Connection::system()
                        .await
                        .context("cannot reach the system D-Bus; is BlueZ installed?")?,
                };
                let inner = Arc::new(Inner {
                    connection,
                    waiting: Mutex::new(HashMap::new()),
                    notifications: Mutex::new(None),
                });

                let objects = inner.connection.object_server();
                for (path, service) in [
                    (MAS_PROFILE, Some(Service::MessageAccess)),
                    (PBAP_PROFILE, Some(Service::PhoneBook)),
                    (MNS_PROFILE, None),
                ] {
                    let profile = Profile {
                        inner: Arc::downgrade(&inner),
                        service,
                    };
                    objects.at(path, profile).await?;
                }

                register(
                    &inner.connection,
                    MAS_PROFILE,
                    &Service::MessageAccess.uuid(),
                    client_options("Ferry Message Access Client"),
                )
                .await?;
                register(
                    &inner.connection,
                    PBAP_PROFILE,
                    &uuid128(PHONE_BOOK_CLIENT_UUID16),
                    client_options("Ferry Phone Book Client"),
                )
                .await?;
                Ok(inner)
            })
            .await
            .cloned()
    }
}

fn client_options(name: &str) -> HashMap<&'static str, Value<'static>> {
    HashMap::from([
        ("Name", Value::from(name.to_string())),
        ("Role", Value::from("client")),
        ("AutoConnect", Value::from(false)),
    ])
}

async fn register(
    connection: &Connection,
    path: &str,
    uuid: &str,
    options: HashMap<&'static str, Value<'static>>,
) -> anyhow::Result<()> {
    connection
        .call_method(
            Some(BLUEZ),
            "/org/bluez",
            Some("org.bluez.ProfileManager1"),
            "RegisterProfile",
            &(ObjectPath::try_from(path)?, uuid, options),
        )
        .await
        .map_err(|error| explain(&error))
        .with_context(|| format!("cannot register the {} profile with BlueZ", uuid))?;
    Ok(())
}

async fn unregister(connection: &Connection, path: &str) {
    let Ok(path) = ObjectPath::try_from(path) else { return };
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
        log::debug!("cannot unregister profile: {}", error);
    }
}

/// Turns BlueZ's error names into something a person can act on.
fn explain(error: &zbus::Error) -> anyhow::Error {
    let text = error.to_string();
    let hint = if text.contains("profile-unavailable") || text.contains("NotAvailable") {
        "the phone doesn't offer this service; check that Show Notifications is on for this computer in the iPhone's Bluetooth settings"
    } else if text.contains("page-timeout") || text.contains("Host is down") || text.contains("No route") {
        "the phone is out of range, asleep with Bluetooth off, or not paired"
    } else if text.contains("NotReady") {
        "Bluetooth is turned off"
    } else if text.contains("AuthenticationFailed") || text.contains("Permission denied") {
        "pairing failed; remove the pairing and pair again"
    } else if text.contains("refused") || text.contains("Refused") {
        "the phone refused the connection"
    } else if text.contains("ServiceUnknown") {
        "BlueZ is not running"
    } else {
        return anyhow!("{}", text);
    };
    anyhow!("{} ({})", hint, text)
}

fn get<'a, T>(properties: &'a HashMap<String, OwnedValue>, name: &str) -> Option<T>
where
    T: TryFrom<&'a Value<'a>>,
    zbus::zvariant::Error: From<T::Error>,
{
    properties.get(name).and_then(|value| value.downcast_ref::<T>().ok())
}

impl Backend for BlueZ {
    fn name(&self) -> String {
        "BlueZ".to_string()
    }

    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>> {
        Box::pin(async move {
            let inner = self.inner().await?;
            let manager = zbus::fdo::ObjectManagerProxy::builder(&inner.connection)
                .destination(BLUEZ)?
                .path("/")?
                .build()
                .await?;
            let objects = manager.get_managed_objects().await.map_err(|error| explain(&error.into()))?;

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
                            uuids
                                .iter()
                                .any(|uuid| uuid.downcast_ref::<&str>().is_ok_and(|uuid| uuid.eq_ignore_ascii_case(&mas)))
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

    fn connect<'a>(&'a self, device: &'a Device, service: Service) -> BoxFuture<'a, anyhow::Result<Link>> {
        Box::pin(async move {
            let inner = self.inner().await?;
            let key = (service.uuid(), device.id.clone());
            let (sender, socket) = oneshot::channel();
            inner.waiting.lock().unwrap().insert(key.clone(), sender);

            let result = connect_profile(&inner.connection, &device.id, &service.uuid()).await;
            if let Err(error) = result {
                inner.waiting.lock().unwrap().remove(&key);
                return Err(error);
            }

            let fd = tokio::time::timeout(Duration::from_secs(10), socket)
                .await
                .map_err(|_| anyhow!("BlueZ connected but handed over no socket"))?
                .map_err(|_| anyhow!("BlueZ connected but handed over no socket"))?;
            Ok(link(fd)?)
        })
    }

    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>> {
        Box::pin(async move {
            let inner = self.inner().await?;
            let (sender, incoming) = mpsc::unbounded_channel();
            *inner.notifications.lock().unwrap() = Some(sender);

            let options = HashMap::from([
                ("Name", Value::from(MNS_SERVICE_NAME)),
                ("Role", Value::from("server")),
                // Any free RFCOMM channel and L2CAP PSM.
                ("Channel", Value::from(0u16)),
                ("PSM", Value::from(0u16)),
                ("RequireAuthentication", Value::from(true)),
                ("RequireAuthorization", Value::from(false)),
                ("AutoConnect", Value::from(false)),
            ]);
            let uuid = uuid128(map::sdp::MESSAGE_NOTIFICATION_SERVER_UUID16);
            register(&inner.connection, MNS_PROFILE, &uuid, options).await?;
            Ok(Listener::new(incoming, Registration(inner)))
        })
    }
}

/// Connects a profile, clearing a stale connection BlueZ still holds if needed.
async fn connect_profile(connection: &Connection, device: &str, uuid: &str) -> anyhow::Result<()> {
    let call = |method: &'static str| async move {
        connection
            .call_method(Some(BLUEZ), device, Some(DEVICE), method, &(uuid,))
            .await
    };

    match call("ConnectProfile").await {
        Ok(_) => Ok(()),
        Err(error) if error.to_string().contains("AlreadyConnected") || error.to_string().contains("InProgress") => {
            log::info!("dropping a stale {} connection", uuid);
            let _ = call("DisconnectProfile").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            call("ConnectProfile").await.map(drop).map_err(|error| explain(&error))
        }
        Err(error) => Err(explain(&error)),
    }
}

/// Unregisters the notification server when its [`Listener`] is dropped.
struct Registration(Arc<Inner>);

impl Drop for Registration {
    fn drop(&mut self) {
        *self.0.notifications.lock().unwrap() = None;
        let inner = self.0.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { unregister(&inner.connection, MNS_PROFILE).await });
        }
    }
}

/// Wraps a socket from BlueZ, finding out whether it is RFCOMM or L2CAP.
fn link(fd: OwnedFd) -> io::Result<Link> {
    let raw = fd.as_raw_fd();
    // SAFETY: `raw` is a valid, open descriptor owned by `fd`.
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        if flags < 0 || libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }

    let packet_mtu = (socket_type(raw) == Some(libc::SOCK_SEQPACKET)).then(|| l2cap_mtu(raw));
    // SAFETY: the descriptor is owned by `fd`, which the `AsyncFd` owns until dropped,
    // and is never closed or replaced in the meantime.
    let fd = unsafe { AsyncFd::register(fd) }?;
    Ok(Link {
        stream: Box::new(FdStream(fd)),
        packet_mtu,
    })
}

fn socket_type(fd: i32) -> Option<i32> {
    let mut value: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `value` and `length` are valid for writes of the sizes given.
    let result = unsafe {
        libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_TYPE, (&mut value as *mut libc::c_int).cast(), &mut length)
    };
    (result == 0).then_some(value)
}

/// The usable MTU of an L2CAP socket: the smaller of the send and receive MTUs.
fn l2cap_mtu(fd: i32) -> u16 {
    const SOL_BLUETOOTH: libc::c_int = 274;
    const BT_SNDMTU: libc::c_int = 12;
    const BT_RCVMTU: libc::c_int = 13;

    let option = |name| {
        let mut value: u16 = 0;
        let mut length = std::mem::size_of::<u16>() as libc::socklen_t;
        // SAFETY: `value` and `length` are valid for writes of the sizes given.
        let result =
            unsafe { libc::getsockopt(fd, SOL_BLUETOOTH, name, (&mut value as *mut u16).cast(), &mut length) };
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
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            let result = guard.try_io(|fd| {
                // SAFETY: `unfilled` is valid for writes of its length.
                let read = unsafe { libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len()) };
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
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            let result = guard.try_io(|fd| {
                // SAFETY: `data` is valid for reads of its length.
                let written = unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
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
    async fn streams_and_packets_over_sockets() {
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

        let (a, _b) = std::os::unix::net::UnixDatagram::pair().unwrap();
        assert_eq!(Some(libc::SOCK_DGRAM), socket_type(a.as_raw_fd()));
    }
}
