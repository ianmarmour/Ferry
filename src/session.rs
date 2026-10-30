//! ## Session
//!
//! The session worker talks to the phone the way a car kit does, on a tokio runtime,
//! while the UI only reads the shared [`Store`] and sends [`Command`]s:
//!
//! 1. it connects to the phone's Message Access Server (MAP MAS) and opens
//!    `telecom/msg`;
//! 2. it publishes a Message Notification Server and registers for notifications, so
//!    the phone reports new messages and delivery as they happen;
//! 3. it downloads the phone's contacts over PBAP, on a second channel;
//! 4. it lists the inbox and Sent folder and downloads every message it has not seen,
//!    one at a time, between commands, so sending is never stuck behind a sync;
//! 5. it pushes messages to the outbox when asked.
//!
//! When the link drops, the worker reconnects with a growing delay until told to
//! disconnect. Without notifications it polls the inbox instead.
//!
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{FixedOffset, Local, NaiveDate, TimeZone};
use map::bmessage::{BMessage, BMessageBody, VCard};
use map::event_report::{EventReport, EventType};
use map::mas::{GetMessageParams, MessageStatus, MessagesListingParams, PushMessageParams};
use map::messages_listing::{Direction as ListingDirection, MessageEntry};
use map::tokio::{MasClient, MnsServer};
use map::types::{MessageHandle, MessageType, Timestamp, UtcOffset};
use obex::response::ResponseCode;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

use crate::bluetooth::{AsyncStream, Backend, Device, Service};
use crate::model::{Direction, Status, Store, Synced};
use crate::pbap::PbapClient;
use crate::text::message_text;

/// What the UI asks the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// List the paired devices.
    RefreshDevices,
    /// Connect to a phone, and stay connected until [`Command::Disconnect`].
    Connect(Device),
    /// Disconnect, and stop reconnecting.
    Disconnect,
    /// Check the phone for new messages now.
    Sync,
    /// Send message `id`, already added to the store with [`Store::add_outgoing`].
    Send {
        /// The message in the store.
        id: u64,
        /// The recipient's phone number or e-mail address.
        address: String,
        /// The text.
        text: String,
    },
    /// Mark messages read on the phone, by handle.
    MarkRead(Vec<u64>),
    /// Download the contacts again.
    ReloadContacts,
}

/// The connection to the phone.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Not connected, and not trying to.
    Disconnected,
    /// Connecting.
    Connecting(Device),
    /// Connected.
    Connected {
        /// The phone.
        device: Device,
        /// Whether messages are being downloaded.
        syncing: bool,
        /// Whether the phone reports new messages as they arrive.
        notifications: bool,
    },
    /// The connection failed or was lost; trying again soon.
    Retrying {
        /// The phone.
        device: Device,
        /// What went wrong.
        error: String,
    },
}

impl State {
    /// The phone being used, if any.
    pub fn device(&self) -> Option<&Device> {
        match self {
            State::Disconnected => None,
            State::Connecting(device)
            | State::Connected { device, .. }
            | State::Retrying { device, .. } => Some(device),
        }
    }
}

/// What the session tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The paired devices, or why they could not be listed.
    Devices(Result<Vec<Device>, String>),
    /// The connection changed.
    State(State),
    /// The store changed.
    Changed,
    /// New messages were received, by id; worth a notification.
    Received(Vec<u64>),
    /// Something worth telling the user that does not affect the connection.
    Notice(String),
}

/// Timings of the session, adjustable for tests.
#[derive(Debug, Clone)]
pub struct Timing {
    /// How long a single MAP operation may take.
    pub operation: Duration,
    /// How often to check for new messages without notifications.
    pub poll: Duration,
    /// How often to check for new messages with notifications, as a safety net.
    pub poll_with_notifications: Duration,
    /// Delays between reconnection attempts.
    pub retry: Vec<Duration>,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            operation: Duration::from_secs(45),
            poll: Duration::from_secs(20),
            poll_with_notifications: Duration::from_secs(120),
            retry: [3, 5, 10, 20, 30].map(Duration::from_secs).to_vec(),
        }
    }
}

/// How many messages to list per folder when connecting.
const INITIAL_LISTING: u16 = 200;

/// How many messages to list when checking for new ones.
const RECENT_LISTING: u16 = 25;

/// The longest message preview to request in listings.
const SUBJECT_LENGTH: u8 = 255;

/// Sends events to the UI and wakes it up.
#[derive(Clone)]
pub struct Outbox {
    events: mpsc::UnboundedSender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Outbox {
    /// Creates an outbox calling `wake` after every event.
    pub fn new(
        events: mpsc::UnboundedSender<Event>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Outbox {
        Outbox {
            events,
            wake: Arc::new(wake),
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        (self.wake)();
    }
}

/// The UI's handle on the session worker.
#[derive(Clone)]
pub struct Session {
    commands: mpsc::UnboundedSender<Command>,
}

impl Session {
    /// Starts the worker on the current tokio runtime.
    ///
    /// Messages are cached in `cache_dir`, one file per phone; without it they are kept
    /// in memory only.
    pub fn start(
        backend: Arc<dyn Backend>,
        store: Arc<Mutex<Store>>,
        cache_dir: Option<PathBuf>,
        timing: Timing,
        outbox: Outbox,
    ) -> Session {
        let (commands, receiver) = mpsc::unbounded_channel();
        let worker = Worker {
            backend,
            store,
            cache_dir,
            timing,
            outbox,
            commands: receiver,
            pending: VecDeque::new(),
        };
        tokio::spawn(worker.run());
        Session { commands }
    }

    /// Sends a command to the worker.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

/// The cache file of `device` in `cache_dir`.
pub fn cache_path(cache_dir: &std::path::Path, device: &Device) -> PathBuf {
    let name: String = device
        .address
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    cache_dir.join(format!("{}.json", name))
}

/// Why a connection ended.
enum Exit {
    /// The user disconnected.
    Disconnect,
    /// The user picked another phone.
    Switch(Device),
    /// The command channel closed: the application is quitting.
    Quit,
    /// The link failed.
    Lost(String),
}

/// What happened while waiting between connections.
enum Wait {
    Elapsed,
    Connect(Device),
    Disconnect,
    Quit,
}

/// A failed MAP operation.
#[derive(Debug)]
enum Failure {
    /// The phone refused the request; the connection is fine.
    Refused(map::Error),
    /// The connection is unusable.
    Link(String),
}

impl From<map::Error> for Failure {
    fn from(error: map::Error) -> Failure {
        if error.response_code().is_some() {
            Failure::Refused(error)
        } else {
            Failure::Link(error.to_string())
        }
    }
}

/// A unit of synchronization work, small enough not to delay commands.
#[derive(Debug, Clone)]
enum Work {
    /// List a folder, queueing the messages not downloaded yet. `expect` is a handle
    /// reported by a notification, downloaded even if the listing lacks it.
    List {
        folder: String,
        count: u16,
        expect: Option<u64>,
    },
    /// Download a message.
    Fetch {
        folder: String,
        entry: Box<MessageEntry>,
    },
}

/// Aborts a task when dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The state of an open connection.
struct Connection {
    device: Device,
    mas: MasClient<Box<dyn AsyncStream>>,
    /// The message folders to synchronize, as the phone names them.
    folders: Vec<String>,
    work: VecDeque<Work>,
    /// Whether the phone accepted the notification registration.
    registered: bool,
    /// Whether the phone connected to deliver notifications. Some desktops run a
    /// second notification server (BlueZ's obexd) the phone may pick instead, so
    /// registering alone does not prove notifications will arrive.
    notifications: bool,
    /// Whether new messages are announced; off during the first synchronization.
    announce: bool,
    contacts: Option<AbortOnDrop>,
}

impl Connection {
    /// Queues a listing of every folder, unless one is queued already.
    fn list_all(&mut self, count: u16) {
        for folder in self.folders.clone() {
            let queued = self
                .work
                .iter()
                .any(|work| matches!(work, Work::List { folder: queued, expect: None, .. } if *queued == folder));
            if !queued {
                self.work.push_back(Work::List {
                    folder,
                    count,
                    expect: None,
                });
            }
        }
    }
}

/// The result of opening a connection.
enum Opening {
    Opened(MasClient<Box<dyn AsyncStream>>),
    Failed(String),
    Interrupted(Exit),
}

struct Worker {
    backend: Arc<dyn Backend>,
    store: Arc<Mutex<Store>>,
    cache_dir: Option<PathBuf>,
    timing: Timing,
    outbox: Outbox,
    commands: mpsc::UnboundedReceiver<Command>,
    /// Commands received while connecting, handled once connected.
    pending: VecDeque<Command>,
}

impl Worker {
    async fn run(mut self) {
        let mut target: Option<Device> = None;
        let mut attempt = 0;

        loop {
            let Some(device) = target.clone() else {
                self.outbox.emit(Event::State(State::Disconnected));
                match self.wait(None).await {
                    Wait::Connect(device) => target = Some(device),
                    Wait::Quit => return,
                    Wait::Elapsed | Wait::Disconnect => {}
                }
                continue;
            };

            self.outbox
                .emit(Event::State(State::Connecting(device.clone())));
            let exit = match self.open(&device).await {
                Opening::Opened(mas) => {
                    attempt = 0;
                    self.connected(mas, &device).await
                }
                Opening::Failed(error) => Exit::Lost(error),
                Opening::Interrupted(exit) => exit,
            };

            match exit {
                Exit::Disconnect => target = None,
                Exit::Switch(other) => {
                    attempt = 0;
                    target = Some(other);
                }
                Exit::Quit => return,
                Exit::Lost(error) => {
                    log::warn!("connection to {} failed: {}", device.name, error);
                    self.fail_sending().await;
                    self.outbox.emit(Event::State(State::Retrying {
                        device: device.clone(),
                        error,
                    }));

                    let delay = self.timing.retry[attempt.min(self.timing.retry.len() - 1)];
                    attempt += 1;
                    match self.wait(Some(delay)).await {
                        Wait::Elapsed => {}
                        Wait::Connect(other) => {
                            attempt = 0;
                            target = Some(other);
                        }
                        Wait::Disconnect => target = None,
                        Wait::Quit => return,
                    }
                }
            }
        }
    }

    async fn next_command(&mut self) -> Option<Command> {
        match self.pending.pop_front() {
            Some(command) => Some(command),
            None => self.commands.recv().await,
        }
    }

    fn with_store<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        f(&mut self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    async fn save(&self) {
        save_store(&self.store).await;
    }

    /// Runs a MAP operation with a time limit.
    async fn op<T>(
        &self,
        operation: impl std::future::Future<Output = map::Result<T>>,
    ) -> Result<T, Failure> {
        match timeout(self.timing.operation, operation).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(Failure::Link("The phone stopped answering".to_string())),
        }
    }

    /// Handles commands while not connected, until `delay` elapses or a connection
    /// command arrives.
    async fn wait(&mut self, delay: Option<Duration>) -> Wait {
        let deadline = delay.map(|delay| Instant::now() + delay);
        loop {
            let command = match deadline {
                Some(deadline) => {
                    match tokio::time::timeout_at(deadline, self.next_command()).await {
                        Ok(command) => command,
                        Err(_) => return Wait::Elapsed,
                    }
                }
                None => self.next_command().await,
            };

            match command {
                None => return Wait::Quit,
                Some(Command::Connect(device)) => return Wait::Connect(device),
                Some(Command::Disconnect) => return Wait::Disconnect,
                Some(Command::RefreshDevices) => self.list_devices().await,
                Some(Command::Send { id, .. }) => {
                    self.with_store(|store| {
                        store.set_status(
                            id,
                            Status::Failed("Not connected to your phone".to_string()),
                        )
                    });
                    self.outbox.emit(Event::Changed);
                    self.save().await;
                }
                // Syncing while waiting to reconnect means retrying now.
                Some(Command::Sync) if deadline.is_some() => return Wait::Elapsed,
                Some(Command::Sync | Command::MarkRead(_) | Command::ReloadContacts) => {}
            }
        }
    }

    async fn list_devices(&self) {
        let devices = self
            .backend
            .devices()
            .await
            .map_err(|error| format!("{:#}", error));
        self.outbox.emit(Event::Devices(devices));
    }

    /// Switches the store to the cache of `device`.
    async fn open_store(&self, device: &Device) {
        if let Some(cache_dir) = &self.cache_dir {
            let path = cache_path(cache_dir, device);
            let current = self.with_store(|store| store.path().map(std::path::Path::to_path_buf));
            if current.as_deref() != Some(path.as_path()) {
                let loaded = tokio::task::spawn_blocking(move || Store::open(path))
                    .await
                    .unwrap_or_else(|_| Store::in_memory());
                self.with_store(|store| *store = loaded);
            }
        }
        self.with_store(Store::forget_handles);
        self.outbox.emit(Event::Changed);
    }

    /// Connects to the phone's Message Access Server.
    async fn open(&mut self, device: &Device) -> Opening {
        self.open_store(device).await;

        let backend = self.backend.clone();
        let target = device.clone();
        let opening = timeout(self.timing.operation, async move {
            let link = backend
                .connect(&target, Service::MessageAccess)
                .await
                .map_err(|error| format!("{:#}", error))?;

            let mut mas = MasClient::new(link.stream);
            if let Some(mtu) = link.packet_mtu {
                mas = mas
                    .with_max_packet_length(mtu)
                    .with_single_response_mode(true);
            }
            mas.connect().await.map_err(|error| describe(&error))?;
            mas.set_folder_path("telecom/msg")
                .await
                .map_err(|error| describe(&error))?;
            Ok::<_, String>(mas)
        });
        tokio::pin!(opening);

        loop {
            tokio::select! {
                result = &mut opening => {
                    return match result {
                        Ok(Ok(mas)) => Opening::Opened(mas),
                        Ok(Err(error)) => Opening::Failed(error),
                        Err(_) => Opening::Failed("The phone did not answer".to_string()),
                    };
                }
                command = self.commands.recv() => match command {
                    None => return Opening::Interrupted(Exit::Quit),
                    Some(Command::Disconnect) => return Opening::Interrupted(Exit::Disconnect),
                    Some(Command::Connect(other)) if other != *device => {
                        return Opening::Interrupted(Exit::Switch(other));
                    }
                    Some(Command::Connect(_)) => {}
                    Some(Command::RefreshDevices) => self.list_devices().await,
                    Some(other) => self.pending.push_back(other),
                },
            }
        }
    }

    /// Runs a connection until it ends.
    async fn connected(&mut self, mas: MasClient<Box<dyn AsyncStream>>, device: &Device) -> Exit {
        let mut conn = Connection {
            device: device.clone(),
            mas,
            folders: Vec::new(),
            work: VecDeque::new(),
            registered: false,
            notifications: false,
            announce: false,
            contacts: None,
        };

        // The phone delivers reports to a server of ours, published before registering.
        let (reports_sender, mut reports) = mpsc::unbounded_channel();
        let (arrived, mut arrivals) = mpsc::unbounded_channel();
        let _notifications = match self.backend.listen().await {
            Ok(listener) => Some(AbortOnDrop(tokio::spawn(serve_notifications(
                listener,
                reports_sender,
                arrived,
            )))),
            Err(error) => {
                log::warn!("cannot listen for notifications: {:#}", error);
                None
            }
        };
        if _notifications.is_some() {
            match self.op(conn.mas.set_notification_registration(true)).await {
                Ok(()) => conn.registered = true,
                Err(Failure::Refused(error)) => {
                    log::warn!("the phone refused notifications: {}", error);
                }
                Err(Failure::Link(error)) => return Exit::Lost(error),
            }
        }

        conn.contacts = Some(self.spawn_contacts(device));

        conn.folders = match self.folders(&mut conn).await {
            Ok(folders) => folders,
            Err(error) => return Exit::Lost(error),
        };
        conn.list_all(INITIAL_LISTING);

        let mut syncing = true;
        self.emit_connected(&conn, syncing);

        // Poll often until the phone connects to deliver notifications.
        let period = self.timing.poll;
        let mut poll = tokio::time::interval_at(Instant::now() + period, period);

        loop {
            tokio::select! {
                biased;
                command = self.next_command() => {
                    let Some(command) = command else {
                        self.close(&mut conn).await;
                        return Exit::Quit;
                    };
                    match self.command(&mut conn, command).await {
                        Ok(None) => {}
                        Ok(Some(exit)) => return exit,
                        Err(error) => return Exit::Lost(error),
                    }
                }
                Some(report) = reports.recv() => self.report(&mut conn, report),
                Some(()) = arrivals.recv() => {
                    if !conn.notifications {
                        conn.notifications = true;
                        let period = self.timing.poll_with_notifications;
                        poll = tokio::time::interval_at(Instant::now() + period, period);
                        self.emit_connected(&conn, syncing);
                    }
                }
                _ = poll.tick() => conn.list_all(RECENT_LISTING),
                _ = std::future::ready(()), if !conn.work.is_empty() => {
                    let work = conn.work.pop_front().expect("not empty");
                    if let Err(error) = self.work(&mut conn, work).await {
                        return Exit::Lost(error);
                    }
                }
            }

            if conn.work.is_empty() == syncing {
                syncing = !syncing;
                if !syncing {
                    conn.announce = true;
                    self.save().await;
                }
                self.emit_connected(&conn, syncing);
            }
        }
    }

    fn emit_connected(&self, conn: &Connection, syncing: bool) {
        self.outbox.emit(Event::State(State::Connected {
            device: conn.device.clone(),
            syncing,
            notifications: conn.notifications,
        }));
    }

    /// Ends the session politely.
    async fn close(&self, conn: &mut Connection) {
        let polite = Duration::from_secs(3);
        if conn.registered {
            let _ = timeout(polite, conn.mas.set_notification_registration(false)).await;
        }
        let _ = timeout(polite, conn.mas.disconnect()).await;
        self.save().await;
    }

    /// The folders of `telecom/msg` to synchronize.
    async fn folders(&self, conn: &mut Connection) -> Result<Vec<String>, String> {
        let defaults = vec!["inbox".to_string(), "sent".to_string()];
        let listing = match self
            .op(conn.mas.get_folder_listing(&Default::default()))
            .await
        {
            Ok(listing) => listing,
            Err(Failure::Refused(_)) => return Ok(defaults),
            Err(Failure::Link(error)) => return Err(error),
        };

        let folders: Vec<String> = ["inbox", "sent"]
            .iter()
            .filter_map(|wanted| {
                listing
                    .names()
                    .into_iter()
                    .find(|name| name.eq_ignore_ascii_case(wanted))
                    .map(str::to_string)
            })
            .collect();
        Ok(if folders.is_empty() {
            defaults
        } else {
            folders
        })
    }

    /// Handles a command; returns how the connection ends, if it does.
    async fn command(
        &mut self,
        conn: &mut Connection,
        command: Command,
    ) -> Result<Option<Exit>, String> {
        match command {
            Command::Connect(other) if other == conn.device => {}
            Command::Connect(other) => {
                self.close(conn).await;
                return Ok(Some(Exit::Switch(other)));
            }
            Command::Disconnect => {
                self.close(conn).await;
                return Ok(Some(Exit::Disconnect));
            }
            Command::RefreshDevices => self.list_devices().await,
            Command::Sync => conn.list_all(RECENT_LISTING),
            Command::Send { id, address, text } => self.send(conn, id, &address, &text).await?,
            Command::MarkRead(handles) => {
                for handle in handles {
                    let status = MessageStatus::Read(true);
                    match self
                        .op(conn.mas.set_message_status(MessageHandle(handle), status))
                        .await
                    {
                        Ok(()) | Err(Failure::Refused(_)) => {}
                        Err(Failure::Link(error)) => return Err(error),
                    }
                }
            }
            Command::ReloadContacts => conn.contacts = Some(self.spawn_contacts(&conn.device)),
        }
        Ok(None)
    }

    /// Pushes a message to the outbox, which makes the phone send it.
    async fn send(
        &self,
        conn: &mut Connection,
        id: u64,
        address: &str,
        text: &str,
    ) -> Result<(), String> {
        let recipient = if address.contains('@') {
            VCard::email(address.trim())
        } else {
            VCard::telephone(address.trim())
        };
        let message = BMessage::new(MessageType::SmsGsm, BMessageBody::from_text(text))
            .with_recipient(recipient);
        let params = PushMessageParams {
            retry: Some(true),
            ..PushMessageParams::default()
        };

        self.with_store(|store| store.set_status(id, Status::Sending));
        self.outbox.emit(Event::Changed);

        let result = self
            .op(conn.mas.push_message("outbox", &message, &params))
            .await;
        let outcome = match result {
            Ok(handle) => {
                self.with_store(|store| {
                    store.set_handle(id, handle.0);
                    store.set_status(id, Status::Sent);
                });
                Ok(())
            }
            Err(Failure::Refused(error)) => {
                log::warn!("the phone refused a message: {}", error);
                self.with_store(|store| store.set_status(id, Status::Failed(refusal(&error))));
                Ok(())
            }
            Err(Failure::Link(error)) => {
                self.with_store(|store| {
                    store.set_status(
                        id,
                        Status::Failed("The connection to your phone was lost".to_string()),
                    )
                });
                Err(error)
            }
        };

        self.outbox.emit(Event::Changed);
        self.save().await;
        outcome
    }

    /// Acts on an event report from the phone.
    fn report(&self, conn: &mut Connection, report: EventReport) {
        log::debug!(
            "event {} {:?} in {:?}",
            report.event_type,
            report.handle,
            report.folder
        );
        let handle = report.handle.map(|handle| handle.0);
        let current = handle.and_then(|handle| {
            self.with_store(|store| store.by_handle(handle).map(|m| m.status.clone()))
        });

        let status = match (&report.event_type, &current) {
            (EventType::NewMessage, None) => {
                let reported = report
                    .folder
                    .as_deref()
                    .and_then(|path| path.rsplit('/').next())
                    .unwrap_or("inbox");
                let folder = conn
                    .folders
                    .iter()
                    .find(|folder| folder.eq_ignore_ascii_case(reported))
                    .cloned()
                    .unwrap_or_else(|| reported.to_lowercase());
                conn.work.push_front(Work::List {
                    folder,
                    count: RECENT_LISTING,
                    expect: handle,
                });
                return;
            }
            (EventType::SendingSuccess, Some(Status::Sending | Status::Failed(_))) => Status::Sent,
            (EventType::DeliverySuccess, Some(_)) => Status::Delivered,
            (EventType::SendingFailure | EventType::DeliveryFailure, Some(_)) => {
                Status::Failed("Not Delivered".to_string())
            }
            (EventType::ReadStatusChanged, Some(_)) if report.read_status == Some(true) => {
                if let Some(handle) = handle {
                    self.with_store(|store| store.set_read_by_handle(handle, true));
                    self.outbox.emit(Event::Changed);
                }
                return;
            }
            _ => return,
        };

        if let Some(handle) = handle {
            self.with_store(|store| store.set_status_by_handle(handle, status));
            self.outbox.emit(Event::Changed);
        }
    }

    /// Performs one unit of synchronization.
    async fn work(&self, conn: &mut Connection, work: Work) -> Result<(), String> {
        match work {
            Work::List {
                folder,
                count,
                expect,
            } => {
                let params = MessagesListingParams {
                    max_list_count: Some(count),
                    subject_length: Some(SUBJECT_LENGTH),
                    ..MessagesListingParams::default()
                };
                let entries = match self
                    .op(conn.mas.get_messages_listing(&folder, &params))
                    .await
                {
                    Ok(response) => response.listing.unwrap_or_default().messages,
                    Err(Failure::Refused(error)) => {
                        log::info!("listing {} refused: {}", folder, error);
                        Vec::new()
                    }
                    Err(Failure::Link(error)) => return Err(error),
                };

                let mut expected = expect;
                self.with_store(|store| {
                    for entry in entries {
                        let handle = entry.handle.0;
                        if expected == Some(handle) {
                            expected = None;
                        }
                        if store.by_handle(handle).is_some() {
                            // A message read on the phone is read here too; not the other
                            // way around, as phones do not always keep the status set.
                            if entry.read == Some(true) {
                                store.set_read_by_handle(handle, true);
                            }
                            continue;
                        }
                        let queued = conn
                            .work
                            .iter()
                            .any(|work| matches!(work, Work::Fetch { entry: queued, .. } if queued.handle.0 == handle));
                        if !queued {
                            conn.work.push_back(Work::Fetch {
                                folder: folder.clone(),
                                entry: Box::new(entry),
                            });
                        }
                    }
                });

                // A notified message missing from the listing is downloaded anyway.
                if let Some(handle) = expected {
                    conn.work.push_front(Work::Fetch {
                        folder,
                        entry: Box::new(MessageEntry::new(MessageHandle(handle))),
                    });
                }
                self.outbox.emit(Event::Changed);
            }
            Work::Fetch { folder, entry } => {
                if self.with_store(|store| store.by_handle(entry.handle.0).is_some()) {
                    return Ok(());
                }
                let message = match self
                    .op(conn
                        .mas
                        .get_message(entry.handle, &GetMessageParams::default()))
                    .await
                {
                    Ok(message) => Some(message),
                    Err(Failure::Refused(error)) => {
                        log::info!("message {} refused: {}", entry.handle, error);
                        None
                    }
                    Err(Failure::Link(error)) => return Err(error),
                };
                if message.is_none() && entry.subject.is_none() {
                    return Ok(());
                }

                let synced = synced(&folder, &entry, message.as_ref());
                let announce =
                    conn.announce && synced.direction == Direction::Incoming && !synced.read;
                let added = self.with_store(|store| store.merge(synced));
                self.outbox.emit(Event::Changed);
                if let (Some(id), true) = (added, announce) {
                    self.outbox.emit(Event::Received(vec![id]));
                }
            }
        }
        Ok(())
    }

    /// Marks messages that were being sent when the connection dropped as failed.
    async fn fail_sending(&self) {
        let queued: Vec<u64> = self
            .pending
            .iter()
            .filter_map(|command| match command {
                Command::Send { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        let failed: Vec<u64> = self.with_store(|store| {
            store
                .messages
                .iter()
                .filter(|m| m.status == Status::Sending && !queued.contains(&m.id))
                .map(|m| m.id)
                .collect()
        });
        if failed.is_empty() {
            return;
        }
        self.with_store(|store| {
            for id in failed {
                store.set_status(
                    id,
                    Status::Failed("The connection to your phone was lost".to_string()),
                );
            }
        });
        self.outbox.emit(Event::Changed);
        self.save().await;
    }

    /// Downloads the contacts in the background.
    fn spawn_contacts(&self, device: &Device) -> AbortOnDrop {
        let backend = self.backend.clone();
        let store = self.store.clone();
        let outbox = self.outbox.clone();
        let device = device.clone();

        AbortOnDrop(tokio::spawn(async move {
            let downloading = timeout(Duration::from_secs(180), download_contacts(backend, device));
            let error = match downloading.await {
                Ok(Ok(contacts)) if contacts.is_empty() => {
                    "the phone shared no contacts".to_string()
                }
                Ok(Ok(contacts)) => {
                    log::info!("downloaded {} contacts", contacts.len());
                    store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .set_contacts(contacts);
                    save_store(&store).await;
                    outbox.emit(Event::Changed);
                    return;
                }
                Ok(Err(error)) => format!("{:#}", error),
                Err(_) => "the phone stopped answering".to_string(),
            };
            log::warn!("cannot download contacts: {}", error);
            outbox.emit(Event::Notice(format!(
                "Couldn't load contacts ({}). On your iPhone, turn on Sync Contacts for this computer in Settings › Bluetooth.",
                error
            )));
        }))
    }
}

async fn download_contacts(
    backend: Arc<dyn Backend>,
    device: Device,
) -> anyhow::Result<Vec<crate::contacts::Contact>> {
    let link = backend.connect(&device, Service::PhoneBook).await?;
    let mut pbap = PbapClient::new(link.stream, link.packet_mtu);
    pbap.connect().await?;
    let contacts = pbap.contacts().await?;
    let _ = pbap.disconnect().await;
    Ok(contacts)
}

/// Serves the phone's notification connections, forwarding the reports.
async fn serve_notifications(
    mut listener: crate::bluetooth::Listener,
    reports: mpsc::UnboundedSender<EventReport>,
    arrived: mpsc::UnboundedSender<()>,
) {
    let mut servers = Vec::new();
    while let Some(link) = listener.accept().await {
        log::info!("the phone connected for notifications");
        let _ = arrived.send(());
        let reports = reports.clone();
        servers.push(AbortOnDrop(tokio::spawn(async move {
            let handler = move |_instance: u8, report: EventReport| {
                let _ = reports.send(report);
                async {}
            };
            let mut server = MnsServer::new(link.stream, handler);
            if let Some(mtu) = link.packet_mtu {
                server = server
                    .with_max_packet_length(mtu)
                    .with_single_response_mode(true);
            }
            if let Err(error) = server.run().await {
                log::info!("notification connection closed: {}", error);
            }
        })));
    }
}

async fn save_store(store: &Arc<Mutex<Store>>) {
    let snapshot = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .snapshot();
    if let Some((path, bytes)) = snapshot {
        match tokio::task::spawn_blocking(move || Store::write_snapshot(&path, &bytes)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log::warn!("cannot save messages: {}", error),
            Err(error) => log::warn!("cannot save messages: {}", error),
        }
    }
}

/// Explains a failed MAP operation.
fn describe(error: &map::Error) -> String {
    match error.response_code() {
        Some(ResponseCode::Forbidden | ResponseCode::Unauthorized) => {
            "The phone did not allow access to its messages. On your iPhone, turn on Show Notifications for this computer in Settings › Bluetooth.".to_string()
        }
        Some(code) => format!("The phone refused the request ({:?})", code),
        None => error.to_string(),
    }
}

/// Explains why the phone refused to send a message.
fn refusal(error: &map::Error) -> String {
    match error.response_code() {
        Some(
            ResponseCode::NotImplemented | ResponseCode::BadRequest | ResponseCode::NotAcceptable,
        ) => "Your phone does not accept messages over Bluetooth".to_string(),
        Some(ResponseCode::Forbidden | ResponseCode::Unauthorized) => {
            "Your phone did not allow sending. Check Show Notifications in its Bluetooth settings"
                .to_string()
        }
        Some(ResponseCode::ServiceUnavailable) => {
            "Your phone could not send the message right now".to_string()
        }
        _ => "Not Delivered".to_string(),
    }
}

/// Converts a listing entry and its downloaded message.
fn synced(folder: &str, entry: &MessageEntry, message: Option<&BMessage>) -> Synced {
    let direction = match entry.direction {
        Some(ListingDirection::Incoming) => Direction::Incoming,
        Some(_) => Direction::Outgoing,
        None if folder.eq_ignore_ascii_case("inbox") => Direction::Incoming,
        None => Direction::Outgoing,
    };

    let present = |value: &Option<String>| value.clone().filter(|value| !value.trim().is_empty());
    let card_address = |card: &VCard| card.telephones.first().or(card.emails.first()).cloned();
    let card_name = |card: &VCard| {
        present(&card.formatted_name).or_else(|| {
            card.name
                .as_deref()
                .map(structured_name)
                .filter(|name| !name.is_empty())
        })
    };
    let card = match direction {
        Direction::Incoming => message.and_then(|message| message.originator.as_ref()),
        Direction::Outgoing => message.and_then(|message| message.recipients().first()),
    };

    let (address, name) = match direction {
        Direction::Incoming => (
            present(&entry.sender_addressing),
            present(&entry.sender_name),
        ),
        Direction::Outgoing => (
            entry.recipients().into_iter().next(),
            present(&entry.recipient_name),
        ),
    };
    let address = address.or_else(|| card.and_then(card_address));
    let name = name.or_else(|| card.and_then(card_name));

    let text = message
        .map(message_text)
        .or_else(|| entry.subject.clone())
        .unwrap_or_default();
    let read = entry
        .read
        .or(message.map(|message| message.read))
        .unwrap_or(false);

    Synced {
        handle: entry.handle.0,
        direction,
        address: address.unwrap_or_else(|| "Unknown".to_string()),
        name,
        text,
        time: entry
            .datetime
            .as_deref()
            .and_then(unix_time)
            .unwrap_or_else(|| Local::now().timestamp()),
        read: read || direction == Direction::Outgoing,
    }
}

/// `Doe;John;;;` to `John Doe`.
fn structured_name(name: &str) -> String {
    let parts: Vec<&str> = name.split(';').map(str::trim).collect();
    [3, 1, 2, 0, 4]
        .iter()
        .filter_map(|&i| parts.get(i).copied())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Converts a MAP timestamp to seconds since the Unix epoch. Timestamps without a
/// time zone are in the phone's local time, assumed to be the computer's.
pub fn unix_time(datetime: &str) -> Option<i64> {
    let time: Timestamp = datetime.parse().ok()?;
    let naive = NaiveDate::from_ymd_opt(time.year.into(), time.month.into(), time.day.into())?
        .and_hms_opt(
            time.hour.into(),
            time.minute.into(),
            time.second.min(59).into(),
        )?;

    match time.offset {
        UtcOffset::Local => Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|t| t.timestamp()),
        UtcOffset::Utc => Some(naive.and_utc().timestamp()),
        UtcOffset::Minutes(minutes) => FixedOffset::east_opt(i32::from(minutes) * 60)?
            .from_local_datetime(&naive)
            .single()
            .map(|t| t.timestamp()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_timestamps() {
        assert_eq!(Some(951_825_600), unix_time("20000229T120000Z"));
        assert_eq!(Some(951_825_600), unix_time("20000229T080000-0400"));
        assert!(unix_time("20240301T101500").is_some());
        assert_eq!(None, unix_time("yesterday"));
    }

    #[test]
    fn converts_listing_entries() {
        let entry = MessageEntry {
            datetime: Some("20240301T101500Z".to_string()),
            sender_addressing: Some("+15550001".to_string()),
            read: Some(false),
            ..MessageEntry::new(MessageHandle(5))
        };
        let message = BMessage::new(MessageType::SmsGsm, BMessageBody::from_text("Hi"))
            .with_originator(VCard::telephone("+15550001").with_name("Doe;Jane"));

        let synced = synced("inbox", &entry, Some(&message));
        assert_eq!(Direction::Incoming, synced.direction);
        assert_eq!("+15550001", synced.address);
        assert_eq!(Some("Jane Doe".to_string()), synced.name);
        assert_eq!("Hi", synced.text);
        assert!(!synced.read);

        let sent = MessageEntry {
            recipient_addressing: Some("+15550002".to_string()),
            subject: Some("Preview".to_string()),
            ..MessageEntry::new(MessageHandle(6))
        };
        let synced = super::synced("sent", &sent, None);
        assert_eq!(Direction::Outgoing, synced.direction);
        assert_eq!("+15550002", synced.address);
        assert_eq!("Preview", synced.text);
        assert!(synced.read);
    }
}
