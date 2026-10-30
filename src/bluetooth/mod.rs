//! ## Bluetooth
//!
//! Ferry needs three things from the platform's Bluetooth stack, the same things a
//! car kit uses:
//!
//! * the list of paired devices;
//! * outgoing connections to the phone's Message Access Server (MAP MAS) and Phone
//!   Book Server (PBAP PSE), found through SDP; and
//! * a Message Notification Server (MAP MNS) record and listener, which the phone
//!   connects to in order to report new messages.
//!
//! Each platform implements [`Backend`]: BlueZ over D-Bus on Linux, WinRT on Windows
//! and IOBluetooth on macOS. [`demo`] simulates an iPhone, for trying Ferry without
//! one and for tests. Connections are handed over as tokio streams, ready for the
//! async clients of the `map` and `obex` crates.
//!
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

pub mod demo;

#[cfg(any(target_os = "macos", windows))]
mod bridge;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// A boxed future, so backends can be used as trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A bidirectional byte stream.
pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncStream for T {}

/// A connected Bluetooth channel.
pub struct Link {
    /// The channel.
    pub stream: Box<dyn AsyncStream>,
    /// The MTU of a packet based channel (L2CAP in Enhanced Retransmission Mode, GOEP
    /// 2.0), on which every OBEX packet must fit in one SDU and Single Response Mode is
    /// used. `None` for stream channels (RFCOMM, GOEP 1.1).
    pub packet_mtu: Option<u16>,
}

impl Link {
    /// A stream (RFCOMM) channel.
    pub fn stream(stream: impl AsyncStream + 'static) -> Link {
        Link {
            stream: Box::new(stream),
            packet_mtu: None,
        }
    }
}

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The platform's identifier for the device.
    pub id: String,
    /// The device name, such as `Jane's iPhone`.
    pub name: String,
    /// The Bluetooth address, `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// Whether the device looks like a phone.
    pub phone: bool,
}

/// The phone services Ferry connects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// The Message Access Server (MAP MAS, UUID 0x1132).
    MessageAccess,
    /// The Phone Book Server (PBAP PSE, UUID 0x112F).
    PhoneBook,
}

impl Service {
    /// The 16-bit service class UUID.
    pub fn uuid16(self) -> u16 {
        match self {
            Service::MessageAccess => map::sdp::MESSAGE_ACCESS_SERVER_UUID16,
            Service::PhoneBook => 0x112F,
        }
    }

    /// The 128-bit service class UUID.
    pub fn uuid(self) -> String {
        uuid128(self.uuid16())
    }
}

/// Expands a 16-bit Bluetooth UUID to its 128-bit string form.
pub fn uuid128(uuid16: u16) -> String {
    format!("0000{:04x}-0000-1000-8000-00805f9b34fb", uuid16)
}

/// The UUID of the PBAP client role (PCE).
pub const PHONE_BOOK_CLIENT_UUID16: u16 = 0x112E;

/// The `MapSupportedFeatures` Ferry advertises in its notification server record:
/// notification registration, notifications, browsing, uploading and delete
/// (`0x1F`, the MAP 1.0 feature set) plus the extended event reports of MAP 1.2,
/// which carry the sender and text of new messages.
pub const MNS_FEATURES: u32 = 0x1F | 1 << 6;

/// Service name of Ferry's notification server record.
pub const MNS_SERVICE_NAME: &str = "Ferry MAP MNS";

/// Incoming notification connections, until dropped.
pub struct Listener {
    incoming: mpsc::UnboundedReceiver<Link>,
    _registration: Box<dyn Send>,
}

impl Listener {
    /// Creates a listener; `registration` is dropped with it, which should unregister
    /// the service.
    pub fn new(
        incoming: mpsc::UnboundedReceiver<Link>,
        registration: impl Send + 'static,
    ) -> Listener {
        Listener {
            incoming,
            _registration: Box::new(registration),
        }
    }

    /// Waits for the phone to connect. `None` once the listener has stopped.
    pub async fn accept(&mut self) -> Option<Link> {
        self.incoming.recv().await
    }
}

/// A Bluetooth stack. See [the module documentation](self).
pub trait Backend: Send + Sync {
    /// A short description, shown in the UI.
    fn name(&self) -> String;

    /// The paired devices.
    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>>;

    /// Connects to `service` on `device`.
    fn connect<'a>(
        &'a self,
        device: &'a Device,
        service: Service,
    ) -> BoxFuture<'a, anyhow::Result<Link>>;

    /// Publishes the Message Notification Server and listens for the phone.
    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>>;
}

/// The Bluetooth stack of this platform.
pub fn native() -> anyhow::Result<Arc<dyn Backend>> {
    #[cfg(target_os = "linux")]
    return Ok(Arc::new(linux::BlueZ::new()));

    #[cfg(windows)]
    return Ok(Arc::new(windows::WinRt::new()));

    #[cfg(target_os = "macos")]
    return Ok(Arc::new(macos::IoBluetooth::new()));

    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    anyhow::bail!("Bluetooth is not supported on this platform")
}

/// Encodes SDP data elements (Bluetooth Core, Vol 3, Part B, section 3), for the
/// platforms that take raw service records.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) mod sdp {
    /// A 16-bit UUID.
    pub fn uuid16(uuid: u16) -> Vec<u8> {
        let mut out = vec![0x19];
        out.extend_from_slice(&uuid.to_be_bytes());
        out
    }

    /// A 16-bit unsigned integer.
    pub fn uint16(value: u16) -> Vec<u8> {
        let mut out = vec![0x09];
        out.extend_from_slice(&value.to_be_bytes());
        out
    }

    /// A 32-bit unsigned integer.
    pub fn uint32(value: u32) -> Vec<u8> {
        let mut out = vec![0x0A];
        out.extend_from_slice(&value.to_be_bytes());
        out
    }

    /// A text string.
    pub fn text(value: &str) -> Vec<u8> {
        let mut out = vec![0x25, value.len() as u8];
        out.extend_from_slice(value.as_bytes());
        out
    }

    /// A data element sequence.
    pub fn sequence(elements: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = elements.concat();
        let mut out = vec![0x35, body.len() as u8];
        out.extend(body);
        out
    }

    /// Reads an unsigned integer element, as found in an attribute value.
    pub fn read_uint(element: &[u8]) -> Option<u64> {
        let (&descriptor, value) = element.split_first()?;
        if descriptor >> 3 != 1 {
            return None;
        }
        let size = 1usize << (descriptor & 0x07);
        let bytes = value.get(..size)?;
        Some(
            bytes
                .iter()
                .fold(0u64, |acc, &byte| acc << 8 | u64::from(byte)),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn encodes_and_reads_elements() {
            assert_eq!(
                vec![0x35, 0x06, 0x19, 0x11, 0x34, 0x09, 0x01, 0x04],
                sequence(&[uuid16(0x1134), uint16(0x0104)])
            );
            assert_eq!(Some(0x1F), read_uint(&uint32(0x1F)));
            assert_eq!(Some(3), read_uint(&[0x08, 3]));
            assert_eq!(None, read_uint(&text("x")));
        }
    }
}
