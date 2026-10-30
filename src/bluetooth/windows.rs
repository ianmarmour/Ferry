//! ## Windows: WinRT Bluetooth
//!
//! The `Windows.Devices.Bluetooth` APIs find the paired devices and the phone's RFCOMM
//! services (from its SDP records), and `Windows.Networking.Sockets` connects to them.
//! For notifications, an `RfcommServiceProvider` publishes the Message Notification
//! Server record and a `StreamSocketListener` accepts the phone's connections.
//!
//! WinRT operations are awaited by blocking a thread, so every call runs on tokio's
//! blocking pool, and sockets are bridged to tokio streams by [`super::bridge`].
//!
//! Windows' own Phone Link app talks MAP to iPhones too; if it is connected to the same
//! phone, quit it before using Ferry.
//!
use std::io;

use anyhow::{anyhow, Context as _};
use tokio::sync::mpsc;
use windows::core::HSTRING;
use windows::Devices::Bluetooth::Rfcomm::{
    RfcommDeviceService, RfcommServiceId, RfcommServiceProvider,
};
use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothDevice, BluetoothMajorClass};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Foundation::TypedEventHandler;
use windows::Networking::Sockets::{
    SocketProtectionLevel, StreamSocket, StreamSocketListener,
    StreamSocketListenerConnectionReceivedEventArgs,
};
use windows::Storage::Streams::{DataReader, DataWriter, IBuffer, InputStreamOptions};

use super::bridge::{self, Channel, Sender};
use super::{
    sdp, Backend, BoxFuture, Device, Link, Listener, Service, MNS_FEATURES, MNS_SERVICE_NAME,
};

/// The WinRT backend. See [the module documentation](self).
pub struct WinRt;

impl WinRt {
    /// Creates the backend.
    pub fn new() -> WinRt {
        WinRt
    }
}

fn format_address(address: u64) -> String {
    let bytes = address.to_be_bytes();
    bytes[2..]
        .iter()
        .map(|byte| format!("{:02X}", byte))
        .collect::<Vec<_>>()
        .join(":")
}

fn bytes(buffer: &IBuffer) -> windows::core::Result<Vec<u8>> {
    let reader = DataReader::FromBuffer(buffer)?;
    let mut bytes = vec![0; reader.UnconsumedBufferLength()? as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

fn buffer(bytes: &[u8]) -> windows::core::Result<IBuffer> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(bytes)?;
    writer.DetachBuffer()
}

fn paired_devices() -> windows::core::Result<Vec<Device>> {
    let selector = BluetoothDevice::GetDeviceSelectorFromPairingState(true)?;
    let found = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.join()?;

    let mut devices = Vec::new();
    for i in 0..found.Size()? {
        let information = found.GetAt(i)?;
        let id = information.Id()?;
        let Ok(device) = BluetoothDevice::FromIdAsync(&id).and_then(|operation| operation.join())
        else {
            continue;
        };
        let phone = device
            .ClassOfDevice()
            .and_then(|class| class.MajorClass())
            .is_ok_and(|class| class == BluetoothMajorClass::Phone);
        devices.push(Device {
            id: id.to_string(),
            name: information.Name()?.to_string(),
            address: format_address(device.BluetoothAddress()?),
            phone,
        });
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

/// The phone's service, preferring a MAS instance that carries SMS.
fn find_service(device: &Device, service: Service) -> anyhow::Result<RfcommDeviceService> {
    let bluetooth = BluetoothDevice::FromIdAsync(&HSTRING::from(&device.id))?
        .join()
        .context("the phone is no longer paired")?;
    let id = RfcommServiceId::FromShortId(u32::from(service.uuid16()))?;
    let result = bluetooth
        .GetRfcommServicesForIdWithCacheModeAsync(&id, BluetoothCacheMode::Uncached)?
        .join()
        .context("the phone is out of range or its Bluetooth is off")?;
    let services = result.Services()?;
    if services.Size()? == 0 {
        anyhow::bail!(
            "the phone doesn't offer this service ({:?}); check that Show Notifications and Sync Contacts are on for this computer in the iPhone's Bluetooth settings",
            result.Error()?
        );
    }

    let sms = |candidate: &RfcommDeviceService| -> bool {
        let types = candidate
            .GetSdpRawAttributesWithCacheModeAsync(BluetoothCacheMode::Cached)
            .and_then(|operation| operation.join())
            .and_then(|attributes| {
                attributes.Lookup(u32::from(map::sdp::ATTRIBUTE_SUPPORTED_MESSAGE_TYPES))
            })
            .and_then(|value| bytes(&value));
        // Bits 1 and 2 are SMS_GSM and SMS_CDMA.
        types
            .ok()
            .and_then(|value| sdp::read_uint(&value))
            .is_none_or(|types| types & 0b110 != 0)
    };

    let mut first = None;
    for i in 0..services.Size()? {
        let candidate = services.GetAt(i)?;
        if service != Service::MessageAccess || sms(&candidate) {
            return Ok(candidate);
        }
        first.get_or_insert(candidate);
    }
    first.ok_or_else(|| anyhow!("no usable service"))
}

/// Reads from a socket's input stream.
struct Input(DataReader);

impl Channel for Input {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let loaded = self.0.LoadAsync(buffer.len() as u32)?.join()? as usize;
        if loaded > 0 {
            self.0.ReadBytes(&mut buffer[..loaded])?;
        }
        Ok(loaded)
    }
}

/// Writes to a socket's output stream.
struct Output {
    writer: DataWriter,
    socket: StreamSocket,
}

impl Sender for Output {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.WriteBytes(data)?;
        self.writer.StoreAsync()?.join()?;
        self.writer.FlushAsync()?.join()?;
        Ok(())
    }

    fn close(&mut self) {
        let _ = self.socket.Close();
    }
}

fn link(runtime: tokio::runtime::Handle, socket: StreamSocket) -> windows::core::Result<Link> {
    let reader = DataReader::CreateDataReader(&socket.InputStream()?)?;
    reader.SetInputStreamOptions(InputStreamOptions::Partial)?;
    let writer = DataWriter::CreateDataWriter(&socket.OutputStream()?)?;
    let stream = bridge::bridge(runtime, Input(reader), Output { writer, socket });
    Ok(Link::stream(stream))
}

/// Stops advertising the notification server when its [`Listener`] is dropped.
struct Registration {
    provider: RfcommServiceProvider,
    listener: StreamSocketListener,
    token: i64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.provider.StopAdvertising();
        let _ = self.listener.RemoveConnectionReceived(self.token);
        let _ = self.listener.Close();
    }
}

fn advertise(
    runtime: tokio::runtime::Handle,
    incoming: mpsc::UnboundedSender<Link>,
) -> windows::core::Result<Registration> {
    let id = RfcommServiceId::FromShortId(u32::from(map::sdp::MESSAGE_NOTIFICATION_SERVER_UUID16))?;
    let provider = RfcommServiceProvider::CreateAsync(&id)?.join()?;
    let listener = StreamSocketListener::new()?;

    let token =
        listener.ConnectionReceived(&TypedEventHandler::new(
            move |_listener,
                  arguments: windows::core::Ref<
                StreamSocketListenerConnectionReceivedEventArgs,
            >| {
                if let Some(arguments) = arguments.as_ref() {
                    log::info!("notification connection received");
                    let link = link(runtime.clone(), arguments.Socket()?)?;
                    let _ = incoming.send(link);
                }
                Ok(())
            },
        ))?;

    listener
        .BindServiceNameWithProtectionLevelAsync(
            &provider.ServiceId()?.AsString()?,
            SocketProtectionLevel::BluetoothEncryptionAllowNullAuthentication,
        )?
        .join()?;

    // Windows adds the service class and protocol descriptor lists itself.
    let attributes = provider.SdpRawAttributes()?;
    let profile = sdp::sequence(&[sdp::sequence(&[
        sdp::uuid16(map::sdp::MESSAGE_ACCESS_PROFILE_UUID16),
        sdp::uint16(map::sdp::PROFILE_VERSION),
    ])]);
    attributes.Insert(0x0009, &buffer(&profile)?)?;
    attributes.Insert(0x0100, &buffer(&sdp::text(MNS_SERVICE_NAME))?)?;
    attributes.Insert(
        u32::from(map::sdp::ATTRIBUTE_MAP_SUPPORTED_FEATURES),
        &buffer(&sdp::uint32(MNS_FEATURES))?,
    )?;

    provider.StartAdvertisingWithRadioDiscoverability(&listener, false)?;
    Ok(Registration {
        provider,
        listener,
        token,
    })
}

/// Runs a blocking WinRT call on tokio's blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| anyhow!("Bluetooth task failed: {}", error))?
}

impl Backend for WinRt {
    fn name(&self) -> String {
        "Windows".to_string()
    }

    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>> {
        Box::pin(blocking(|| {
            paired_devices().context("cannot list Bluetooth devices; is Bluetooth on?")
        }))
    }

    fn connect<'a>(
        &'a self,
        device: &'a Device,
        service: Service,
    ) -> BoxFuture<'a, anyhow::Result<Link>> {
        let device = device.clone();
        Box::pin(async move {
            let runtime = tokio::runtime::Handle::current();
            blocking(move || {
                let service = find_service(&device, service)?;
                let socket = StreamSocket::new()?;
                socket
                    .ConnectWithProtectionLevelAsync(
                        &service.ConnectionHostName()?,
                        &service.ConnectionServiceName()?,
                        SocketProtectionLevel::BluetoothEncryptionAllowNullAuthentication,
                    )?
                    .join()
                    .context("the phone refused the connection")?;
                Ok(link(runtime, socket)?)
            })
            .await
        })
    }

    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>> {
        Box::pin(async move {
            let runtime = tokio::runtime::Handle::current();
            let (sender, incoming) = mpsc::unbounded_channel();
            let registration = blocking(move || {
                advertise(runtime, sender).context("cannot publish the notification service")
            })
            .await?;
            Ok(Listener::new(incoming, registration))
        })
    }
}
