//! ## macOS: IOBluetooth
//!
//! `IOBluetoothDevice` lists the paired devices, runs the SDP query and opens the RFCOMM
//! channel of the phone's service. For notifications, an SDP record for the Message
//! Notification Server is published with `IOBluetoothSDPServiceRecord`, and incoming
//! RFCOMM channels on its channel are accepted.
//!
//! IOBluetooth delivers its callbacks on the run loop of the thread that made the
//! request, so every call is made on the main thread, whose run loop the window keeps
//! running, through the main dispatch queue. Channel delegates forward events to
//! [`super::bridge`], which turns them into tokio streams.
//!
//! When bundled, the app needs `NSBluetoothAlwaysUsageDescription` in its Info.plist;
//! see `macos/Info.plist`.
//!
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as channel;
use std::time::Duration;

use anyhow::{anyhow, bail};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, Message};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSObjectProtocol, NSString};
use objc2_io_bluetooth::{
    IOBluetoothDevice, IOBluetoothRFCOMMChannel, IOBluetoothRFCOMMChannelDelegate,
    IOBluetoothSDPServiceRecord, IOBluetoothSDPUUID, IOBluetoothUserNotification,
    IOBluetoothUserNotificationChannelDirection,
};
use tokio::sync::{mpsc, oneshot};

use super::bridge::{self, Channel, Sender};
use super::{Backend, BoxFuture, Device, Link, Listener, Service, MNS_FEATURES, MNS_SERVICE_NAME};

/// `kBluetoothDeviceClassMajorPhone`.
const MAJOR_CLASS_PHONE: u32 = 0x02;

/// What a channel delegate reports.
enum ChannelEvent {
    /// The SDP query finished, with its status.
    Searched(i32),
    /// The channel opened, or failed to.
    Opened(i32),
    /// Data arrived.
    Data(Vec<u8>),
    /// The channel closed.
    Closed,
}

struct DelegateIvars {
    events: channel::Sender<ChannelEvent>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `ChannelDelegate` does not
    // implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "TidingsChannelDelegate"]
    #[ivars = DelegateIvars]
    struct ChannelDelegate;

    impl ChannelDelegate {
        #[unsafe(method(sdpQueryComplete:status:))]
        fn sdp_query_complete(&self, _device: *mut AnyObject, status: i32) {
            let _ = self.ivars().events.send(ChannelEvent::Searched(status));
        }
    }

    unsafe impl NSObjectProtocol for ChannelDelegate {}

    unsafe impl IOBluetoothRFCOMMChannelDelegate for ChannelDelegate {
        #[unsafe(method(rfcommChannelData:data:length:))]
        fn rfcomm_channel_data(&self, _channel: *mut IOBluetoothRFCOMMChannel, data: *mut c_void, length: usize) {
            if data.is_null() {
                return;
            }
            // SAFETY: IOBluetooth passes `length` readable bytes at `data`.
            let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
            let _ = self.ivars().events.send(ChannelEvent::Data(bytes.to_vec()));
        }

        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        fn rfcomm_channel_open_complete(&self, _channel: *mut IOBluetoothRFCOMMChannel, status: i32) {
            let _ = self.ivars().events.send(ChannelEvent::Opened(status));
        }

        #[unsafe(method(rfcommChannelClosed:))]
        fn rfcomm_channel_closed(&self, _channel: *mut IOBluetoothRFCOMMChannel) {
            let _ = self.ivars().events.send(ChannelEvent::Closed);
        }
    }
);

impl ChannelDelegate {
    fn new(events: channel::Sender<ChannelEvent>) -> Retained<ChannelDelegate> {
        let this = ChannelDelegate::alloc().set_ivars(DelegateIvars { events });
        // SAFETY: `init` of NSObject.
        unsafe { msg_send![super(this), init] }
    }
}

struct ObserverIvars {
    incoming: mpsc::UnboundedSender<Link>,
    runtime: tokio::runtime::Handle,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `ChannelObserver` does not
    // implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "TidingsChannelObserver"]
    #[ivars = ObserverIvars]
    struct ChannelObserver;

    impl ChannelObserver {
        #[unsafe(method(channelOpened:channel:))]
        fn channel_opened(&self, _notification: *mut AnyObject, channel: *mut IOBluetoothRFCOMMChannel) {
            // SAFETY: IOBluetooth passes a valid channel, or null.
            let Some(channel) = (unsafe { Retained::retain(channel) }) else { return };
            log::info!("notification channel opened");
            let link = adopt(channel, self.ivars().runtime.clone());
            let _ = self.ivars().incoming.send(link);
        }
    }

    unsafe impl NSObjectProtocol for ChannelObserver {}
);

impl ChannelObserver {
    fn new(
        incoming: mpsc::UnboundedSender<Link>,
        runtime: tokio::runtime::Handle,
    ) -> Retained<ChannelObserver> {
        let this = ChannelObserver::alloc().set_ivars(ObserverIvars { incoming, runtime });
        // SAFETY: `init` of NSObject.
        unsafe { msg_send![super(this), init] }
    }
}

/// A published notification server record.
struct Server {
    record: Retained<IOBluetoothSDPServiceRecord>,
    notification: Retained<IOBluetoothUserNotification>,
    _observer: Retained<ChannelObserver>,
}

/// An open channel and its delegate.
type OpenChannel = (
    Retained<IOBluetoothRFCOMMChannel>,
    Retained<ChannelDelegate>,
);

// Objects that only the main thread touches, by id.
thread_local! {
    static SEARCHES: RefCell<HashMap<u64, Retained<ChannelDelegate>>> = RefCell::new(HashMap::new());
    static CHANNELS: RefCell<HashMap<u64, OpenChannel>> = RefCell::new(HashMap::new());
    static SERVERS: RefCell<HashMap<u64, Server>> = RefCell::new(HashMap::new());
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Runs `work` on the main thread.
async fn on_main<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> anyhow::Result<T> {
    let (sender, result) = oneshot::channel();
    DispatchQueue::main().exec_async(move || {
        let _ = sender.send(work());
    });
    result
        .await
        .map_err(|_| anyhow!("the main thread is not running"))
}

/// Waits for the next event of a channel without blocking the runtime.
async fn next_event(
    events: channel::Receiver<ChannelEvent>,
    timeout: Duration,
) -> (channel::Receiver<ChannelEvent>, Option<ChannelEvent>) {
    tokio::task::spawn_blocking(move || {
        let event = events.recv_timeout(timeout).ok();
        (events, event)
    })
    .await
    .unwrap_or_else(|_| {
        let (_, events) = channel::channel();
        (events, None)
    })
}

fn any<T: Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an `AnyObject`.
    unsafe { Retained::cast_unchecked(object) }
}

fn find_device(address: &str) -> anyhow::Result<Retained<IOBluetoothDevice>> {
    // SAFETY: plain IOBluetooth call on the main thread.
    unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&NSString::from_str(address))) }
        .ok_or_else(|| anyhow!("the phone is no longer paired"))
}

fn paired_devices() -> Vec<Device> {
    // SAFETY: plain IOBluetooth calls on the main thread.
    unsafe {
        let Some(paired) = IOBluetoothDevice::pairedDevices() else {
            return Vec::new();
        };
        let mut devices: Vec<Device> = paired
            .to_vec()
            .into_iter()
            .filter_map(|object| object.downcast::<IOBluetoothDevice>().ok())
            .filter_map(|device| {
                let id = device.addressString()?.to_string();
                let name = device
                    .nameOrAddress()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| id.clone());
                Some(Device {
                    address: id.replace('-', ":").to_uppercase(),
                    id,
                    name,
                    phone: device.deviceClassMajor() == MAJOR_CLASS_PHONE,
                })
            })
            .collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        devices
    }
}

/// Takes over an open channel and bridges it to a tokio stream.
fn adopt(channel: Retained<IOBluetoothRFCOMMChannel>, runtime: tokio::runtime::Handle) -> Link {
    let id = next_id();
    let (events, received) = channel::channel();
    let delegate = ChannelDelegate::new(events);
    let target: &AnyObject = &delegate;
    // SAFETY: plain IOBluetooth calls on the main thread.
    let mtu = unsafe {
        channel.setDelegate(Some(target));
        channel.getMTU()
    };
    CHANNELS.with(|channels| channels.borrow_mut().insert(id, (channel, delegate)));
    Link::stream(bridge::bridge(
        runtime,
        Incoming {
            events: received,
            pending: Vec::new(),
        },
        Outgoing { id, mtu },
    ))
}

fn close_channel(id: u64) {
    if let Some((channel, _delegate)) = CHANNELS.with(|channels| channels.borrow_mut().remove(&id))
    {
        // SAFETY: plain IOBluetooth calls on the main thread.
        unsafe {
            channel.setDelegate(None);
            channel.closeChannel();
        }
    }
}

/// Data received on a channel.
struct Incoming {
    events: channel::Receiver<ChannelEvent>,
    pending: Vec<u8>,
}

impl Channel for Incoming {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() {
            match self.events.recv() {
                Ok(ChannelEvent::Data(data)) => self.pending = data,
                Ok(ChannelEvent::Closed) | Err(_) => return Ok(0),
                Ok(_) => {}
            }
        }
        let count = buffer.len().min(self.pending.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

/// Writes to a channel from the main thread.
struct Outgoing {
    id: u64,
    mtu: u16,
}

impl Sender for Outgoing {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let (id, mtu) = (self.id, usize::from(self.mtu.max(1)));
        let mut result = Ok(());
        DispatchQueue::main().exec_sync(|| {
            result = CHANNELS.with(|channels| {
                let channels = channels.borrow();
                let (channel, _) = channels.get(&id).ok_or(io::ErrorKind::NotConnected)?;
                for chunk in data.chunks(mtu) {
                    // SAFETY: `chunk` is valid for reads of its length, which fits the MTU.
                    let status = unsafe {
                        channel
                            .writeSync_length(chunk.as_ptr().cast_mut().cast(), chunk.len() as u16)
                    };
                    if status != 0 {
                        return Err(io::Error::other(format!(
                            "RFCOMM write failed ({:#x})",
                            status
                        )));
                    }
                }
                Ok(())
            });
        });
        result
    }

    fn close(&mut self) {
        let id = self.id;
        DispatchQueue::main().exec_async(move || close_channel(id));
    }
}

/// An SDP data element for a fixed size unsigned integer.
fn number(value: u32, size: u32) -> Retained<AnyObject> {
    let keys = [
        &*NSString::from_str("DataElementType"),
        &*NSString::from_str("DataElementSize"),
        &*NSString::from_str("DataElementValue"),
    ];
    let values = [
        any(NSNumber::new_u32(1)),
        any(NSNumber::new_u32(size)),
        any(NSNumber::new_u32(value)),
    ];
    any(NSDictionary::<NSString, AnyObject>::from_retained_objects(
        &keys, &values,
    ))
}

fn uuid(value: u16) -> Retained<AnyObject> {
    any(NSData::with_bytes(&value.to_be_bytes()))
}

fn list(items: Vec<Retained<AnyObject>>) -> Retained<AnyObject> {
    any(NSArray::from_retained_slice(&items))
}

/// The Message Notification Server record, in the dictionary format of
/// `publishedServiceRecordWithDictionary:`. The RFCOMM channel is assigned when
/// publishing.
fn mns_record() -> Retained<NSDictionary> {
    let entries: Vec<(&str, Retained<AnyObject>)> = vec![
        (
            "0001 - ServiceClassIDList",
            list(vec![uuid(map::sdp::MESSAGE_NOTIFICATION_SERVER_UUID16)]),
        ),
        (
            "0004 - ProtocolDescriptorList",
            list(vec![
                list(vec![uuid(0x0100)]),
                list(vec![uuid(0x0003), number(0x10, 1)]),
                list(vec![uuid(0x0008)]),
            ]),
        ),
        ("0005 - BrowseGroupList*", list(vec![uuid(0x1002)])),
        (
            "0009 - BluetoothProfileDescriptorList",
            list(vec![list(vec![
                uuid(map::sdp::MESSAGE_ACCESS_PROFILE_UUID16),
                number(u32::from(map::sdp::PROFILE_VERSION), 2),
            ])]),
        ),
        (
            "0100 - ServiceName*",
            any(NSString::from_str(MNS_SERVICE_NAME)),
        ),
        ("0317 - MapSupportedFeatures", number(MNS_FEATURES, 4)),
    ];

    let keys: Vec<Retained<NSString>> = entries
        .iter()
        .map(|(key, _)| NSString::from_str(key))
        .collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let values: Vec<Retained<AnyObject>> = entries.into_iter().map(|(_, value)| value).collect();
    let dictionary = NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &values);
    // SAFETY: an NSDictionary with NSString keys is an NSDictionary.
    unsafe { Retained::cast_unchecked(dictionary) }
}

fn publish(
    id: u64,
    incoming: mpsc::UnboundedSender<Link>,
    runtime: tokio::runtime::Handle,
) -> anyhow::Result<()> {
    // SAFETY: plain IOBluetooth calls on the main thread.
    unsafe {
        let record =
            IOBluetoothSDPServiceRecord::publishedServiceRecordWithDictionary(Some(&mns_record()))
                .ok_or_else(|| {
                    anyhow!("cannot publish the notification service; is Bluetooth on?")
                })?;
        let mut channel_id = 0;
        if record.getRFCOMMChannelID(&mut channel_id) != 0 {
            record.removeServiceRecord();
            bail!("the notification service has no RFCOMM channel");
        }

        let observer = ChannelObserver::new(incoming, runtime);
        let target: &AnyObject = &observer;
        let notification = IOBluetoothRFCOMMChannel::registerForChannelOpenNotifications_selector_withChannelID_direction(
            Some(target),
            Some(sel!(channelOpened:channel:)),
            channel_id,
            IOBluetoothUserNotificationChannelDirection::Incoming,
        )
        .ok_or_else(|| {
            record.removeServiceRecord();
            anyhow!("cannot listen for the phone's notifications")
        })?;

        log::info!(
            "notification service published on RFCOMM channel {}",
            channel_id
        );
        SERVERS.with(|servers| {
            servers.borrow_mut().insert(
                id,
                Server {
                    record,
                    notification,
                    _observer: observer,
                },
            )
        });
    }
    Ok(())
}

/// Removes the notification server record when its [`Listener`] is dropped.
struct Registration(u64);

impl Drop for Registration {
    fn drop(&mut self) {
        let id = self.0;
        DispatchQueue::main().exec_async(move || {
            if let Some(server) = SERVERS.with(|servers| servers.borrow_mut().remove(&id)) {
                // SAFETY: plain IOBluetooth calls on the main thread.
                unsafe {
                    server.notification.unregister();
                    server.record.removeServiceRecord();
                }
            }
        });
    }
}

/// The IOBluetooth backend. See [the module documentation](self).
pub struct IoBluetooth;

impl IoBluetooth {
    /// Creates the backend.
    pub fn new() -> IoBluetooth {
        IoBluetooth
    }
}

impl Backend for IoBluetooth {
    fn name(&self) -> String {
        "IOBluetooth".to_string()
    }

    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>> {
        Box::pin(on_main(paired_devices))
    }

    fn connect<'a>(
        &'a self,
        device: &'a Device,
        service: Service,
    ) -> BoxFuture<'a, anyhow::Result<Link>> {
        let address = device.id.clone();
        Box::pin(async move {
            let runtime = tokio::runtime::Handle::current();
            let id = next_id();
            let (events, received) = channel::channel();

            // Refresh the phone's services; cached records are used if this fails.
            let search = address.clone();
            on_main(move || -> anyhow::Result<()> {
                let device = find_device(&search)?;
                let delegate = ChannelDelegate::new(events);
                let target: &AnyObject = &delegate;
                // SAFETY: plain IOBluetooth call on the main thread.
                let status = unsafe { device.performSDPQuery(Some(target)) };
                if status != 0 {
                    let _ = delegate.ivars().events.send(ChannelEvent::Searched(status));
                }
                SEARCHES.with(|searches| searches.borrow_mut().insert(id, delegate));
                Ok(())
            })
            .await??;

            let (received, searched) = next_event(received, Duration::from_secs(20)).await;
            if let Some(ChannelEvent::Searched(status)) = searched {
                if status != 0 {
                    log::info!("SDP query failed ({:#x}); using cached records", status);
                }
            } else {
                on_main(move || SEARCHES.with(|searches| searches.borrow_mut().remove(&id)))
                    .await?;
                bail!("the phone is out of range or its Bluetooth is off");
            }

            let uuid16 = service.uuid16();
            on_main(move || -> anyhow::Result<()> {
                let delegate = SEARCHES
                    .with(|searches| searches.borrow_mut().remove(&id))
                    .ok_or_else(|| anyhow!("connection cancelled"))?;
                let device = find_device(&address)?;
                // SAFETY: plain IOBluetooth calls on the main thread.
                unsafe {
                    let uuid = IOBluetoothSDPUUID::uuid16(uuid16).ok_or_else(|| anyhow!("bad UUID"))?;
                    let record = device.getServiceRecordForUUID(Some(&uuid)).ok_or_else(|| {
                        anyhow!("the phone doesn't offer this service; check that Show Notifications and Sync Contacts are on for this computer in the iPhone's Bluetooth settings")
                    })?;
                    let mut channel_id = 0;
                    if record.getRFCOMMChannelID(&mut channel_id) != 0 {
                        bail!("the phone's service is not reachable over RFCOMM");
                    }

                    let mut channel = None;
                    let target: &AnyObject = &delegate;
                    let status = device.openRFCOMMChannelAsync_withChannelID_delegate(Some(&mut channel), channel_id, Some(target));
                    let channel = channel.filter(|_| status == 0).ok_or_else(|| {
                        anyhow!("cannot open the RFCOMM channel ({:#x})", status)
                    })?;
                    CHANNELS.with(|channels| channels.borrow_mut().insert(id, (channel, delegate)));
                }
                Ok(())
            })
            .await??;

            let (received, opened) = next_event(received, Duration::from_secs(20)).await;
            match opened {
                Some(ChannelEvent::Opened(0)) => {}
                other => {
                    on_main(move || close_channel(id)).await?;
                    match other {
                        Some(ChannelEvent::Opened(status)) => {
                            bail!("the phone refused the connection ({:#x})", status)
                        }
                        _ => bail!("the phone did not answer"),
                    }
                }
            }

            let mtu = on_main(move || {
                CHANNELS.with(|channels| {
                    // SAFETY: plain IOBluetooth call on the main thread.
                    channels
                        .borrow()
                        .get(&id)
                        .map(|(channel, _)| unsafe { channel.getMTU() })
                })
            })
            .await?
            .ok_or_else(|| anyhow!("the channel closed"))?;

            Ok(Link::stream(bridge::bridge(
                runtime,
                Incoming {
                    events: received,
                    pending: Vec::new(),
                },
                Outgoing { id, mtu },
            )))
        })
    }

    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>> {
        Box::pin(async move {
            let runtime = tokio::runtime::Handle::current();
            let id = next_id();
            let (sender, incoming) = mpsc::unbounded_channel();
            on_main(move || publish(id, sender, runtime)).await??;
            Ok(Listener::new(incoming, Registration(id)))
        })
    }
}
