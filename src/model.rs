//! ## Messages and Conversations
//!
//! The [`Store`] holds everything Ferry knows about one phone: its messages and its
//! contacts. It is shared between the UI and the session worker, and cached on disk,
//! because an iPhone only exposes recent messages over MAP: the history builds up
//! locally, the way a car kit's does.
//!
//! MAP message handles are only meaningful during one session, so messages are
//! matched across sessions by their content: the same direction, conversation, text
//! and time.
//!
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::contacts::{conversation_key, format_address, Contact};

/// Whether a message was received or sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// Received from the other party.
    Incoming,
    /// Sent by the phone's owner.
    Outgoing,
}

/// The state of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// A received message.
    Received,
    /// Being handed to the phone.
    Sending,
    /// Accepted by the phone, or found in its Sent folder.
    Sent,
    /// The phone reported delivery to the recipient.
    Delivered,
    /// Sending failed, with the reason.
    Failed(String),
}

/// A text message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Identifies the message in the [`Store`].
    pub id: u64,
    /// The MAP handle of the message on the phone, during the current session.
    #[serde(skip)]
    pub handle: Option<u64>,
    /// Whether the message has been matched with a listing from the phone during the
    /// current session.
    #[serde(skip)]
    pub listed: bool,
    /// Whether the message was received or sent.
    pub direction: Direction,
    /// The other party: the sender of a received message, the recipient of a sent one.
    pub address: String,
    /// The other party's name as the phone reported it.
    pub name: Option<String>,
    /// The text.
    pub text: String,
    /// When the message was sent or received, in seconds since the Unix epoch.
    pub time: i64,
    /// Whether the message has been read.
    pub read: bool,
    /// Delivery state.
    pub status: Status,
}

impl Message {
    /// The key of the conversation the message belongs to.
    pub fn conversation(&self) -> String {
        conversation_key(&self.address)
    }
}

/// A message as listed and downloaded from the phone.
#[derive(Debug, Clone, PartialEq)]
pub struct Synced {
    /// The MAP handle.
    pub handle: u64,
    /// Whether the message was received or sent.
    pub direction: Direction,
    /// The other party.
    pub address: String,
    /// The other party's name, if the phone knows it.
    pub name: Option<String>,
    /// The text.
    pub text: String,
    /// When the message was sent or received, in seconds since the Unix epoch.
    pub time: i64,
    /// Whether the message has been read.
    pub read: bool,
}

/// A conversation, as listed in the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    /// The conversation key, see [`conversation_key`].
    pub key: String,
    /// The address to reply to.
    pub address: String,
    /// The contact name or formatted address.
    pub title: String,
    /// The text of the latest message.
    pub preview: String,
    /// The time of the latest message.
    pub time: i64,
    /// The number of unread received messages.
    pub unread: usize,
}

/// How far apart a locally sent message and its copy in the phone's Sent folder can
/// be timestamped, as the clocks of the phone and the computer can differ.
const SENT_MATCH_WINDOW: i64 = 15 * 60;

/// How far apart two downloads of the same message can be timestamped.
const SYNC_MATCH_WINDOW: i64 = 120;

/// The messages and contacts of one phone. See [the module documentation](self).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    /// Every message, in no particular order.
    pub messages: Vec<Message>,
    /// The phone's contacts, sorted by name.
    pub contacts: Vec<Contact>,
    next_id: u64,
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    revision: u64,
    #[serde(skip)]
    index: HashMap<String, usize>,
}

impl Store {
    /// An empty store that is never saved.
    pub fn in_memory() -> Store {
        Store::default()
    }

    /// Loads the store saved at `path`, or starts an empty one there.
    pub fn open(path: impl Into<PathBuf>) -> Store {
        let path = path.into();
        let mut store = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Store>(&bytes).unwrap_or_else(|error| {
                log::warn!("discarding unreadable cache {}: {}", path.display(), error);
                Store::default()
            }),
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    log::warn!("cannot read cache {}: {}", path.display(), error);
                }
                Store::default()
            }
        };
        store.path = Some(path);
        store.reindex();
        store
    }

    /// Where the store is saved.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The bytes to save and where to save them, so the file can be written without
    /// holding a lock on the store.
    pub fn snapshot(&self) -> Option<(PathBuf, Vec<u8>)> {
        let path = self.path.clone()?;
        match serde_json::to_vec(self) {
            Ok(bytes) => Some((path, bytes)),
            Err(error) => {
                log::error!("cannot serialize the message cache: {}", error);
                None
            }
        }
    }

    /// Writes a [`snapshot`](Self::snapshot) atomically.
    pub fn write_snapshot(path: &Path, bytes: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, path)
    }

    /// Increases whenever the store changes, so views can cache what they derive.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision += 1;
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Replaces the contacts.
    pub fn set_contacts(&mut self, mut contacts: Vec<Contact>) {
        contacts.sort_by_key(|contact| contact.name.to_lowercase());
        self.contacts = contacts;
        self.reindex();
        self.changed();
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, contact) in self.contacts.iter().enumerate() {
            for address in contact.addresses() {
                self.index.entry(conversation_key(address)).or_insert(i);
            }
        }
    }

    /// The contact `address` belongs to.
    pub fn contact(&self, address: &str) -> Option<&Contact> {
        self.index
            .get(&conversation_key(address))
            .map(|&i| &self.contacts[i])
    }

    /// The name to show for `address`: the contact name, else the name the phone
    /// reported, else the formatted address.
    pub fn display_name(&self, address: &str, reported: Option<&str>) -> String {
        if let Some(contact) = self.contact(address) {
            return contact.name.clone();
        }
        match reported.map(str::trim) {
            Some(name)
                if !name.is_empty() && conversation_key(name) != conversation_key(address) =>
            {
                name.to_string()
            }
            _ => format_address(address),
        }
    }

    /// The message with handle `handle` in the current session.
    pub fn by_handle(&self, handle: u64) -> Option<&Message> {
        self.messages.iter().find(|m| m.handle == Some(handle))
    }

    /// The message `id`.
    pub fn get(&self, id: u64) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Message> {
        self.messages.iter_mut().find(|m| m.id == id)
    }

    /// Forgets the handles of the previous session.
    pub fn forget_handles(&mut self) {
        for message in &mut self.messages {
            message.handle = None;
            message.listed = false;
        }
    }

    /// Adds a message being sent and returns its id.
    pub fn add_outgoing(&mut self, address: &str, text: &str, time: i64) -> u64 {
        let id = self.next_id();
        self.messages.push(Message {
            id,
            handle: None,
            listed: false,
            direction: Direction::Outgoing,
            address: address.trim().to_string(),
            name: None,
            text: text.to_string(),
            time,
            read: true,
            status: Status::Sending,
        });
        self.changed();
        id
    }

    /// Records the handle the phone gave a pushed message.
    pub fn set_handle(&mut self, id: u64, handle: u64) {
        if let Some(message) = self.get_mut(id) {
            message.handle = Some(handle);
            self.changed();
        }
    }

    /// Changes the status of message `id`.
    pub fn set_status(&mut self, id: u64, status: Status) {
        if let Some(message) = self.get_mut(id) {
            if message.status != status {
                message.status = status;
                self.changed();
            }
        }
    }

    /// Changes the status of the message with handle `handle`.
    pub fn set_status_by_handle(&mut self, handle: u64, status: Status) {
        if let Some(id) = self.by_handle(handle).map(|m| m.id) {
            self.set_status(id, status);
        }
    }

    /// Changes the read flag of the message with handle `handle`.
    pub fn set_read_by_handle(&mut self, handle: u64, read: bool) {
        let message = self.messages.iter_mut().find(|m| m.handle == Some(handle));
        if let Some(message) = message.filter(|m| m.read != read) {
            message.read = read;
            self.changed();
        }
    }

    /// Merges a message downloaded from the phone and returns its id if it is new.
    ///
    /// A message already known from this session (same handle) or an earlier one (same
    /// content and time) is updated rather than added. A message from the phone's Sent
    /// folder that matches one sent from Ferry replaces the local copy's timestamp.
    pub fn merge(&mut self, synced: Synced) -> Option<u64> {
        if let Some(message) = self
            .messages
            .iter_mut()
            .find(|m| m.handle == Some(synced.handle))
        {
            if message.read != synced.read {
                message.read = synced.read;
                self.revision += 1;
            }
            return None;
        }

        let key = conversation_key(&synced.address);
        let same = |m: &Message, window: i64| {
            !m.listed
                && m.direction == synced.direction
                && m.text.trim() == synced.text.trim()
                && (m.time - synced.time).abs() <= window
                && m.conversation() == key
        };

        let local = self.messages.iter().position(|m| {
            m.direction == Direction::Outgoing
                && matches!(m.status, Status::Sending | Status::Sent | Status::Failed(_))
                && same(m, SENT_MATCH_WINDOW)
        });
        let earlier = local.or_else(|| {
            self.messages
                .iter()
                .position(|m| same(m, SYNC_MATCH_WINDOW))
        });

        if let Some(i) = earlier {
            let message = &mut self.messages[i];
            message.handle = Some(synced.handle);
            message.listed = true;
            message.time = synced.time;
            message.read = message.read || synced.read;
            if synced.name.is_some() {
                message.name = synced.name;
            }
            if matches!(message.status, Status::Sending | Status::Failed(_)) {
                message.status = Status::Sent;
            }
            self.changed();
            return None;
        }

        let id = self.next_id();
        self.messages.push(Message {
            id,
            handle: Some(synced.handle),
            listed: true,
            direction: synced.direction,
            address: synced.address,
            name: synced.name,
            text: synced.text,
            time: synced.time,
            read: synced.read || synced.direction == Direction::Outgoing,
            status: match synced.direction {
                Direction::Incoming => Status::Received,
                Direction::Outgoing => Status::Sent,
            },
        });
        self.changed();
        Some(id)
    }

    /// The conversations, most recent first.
    pub fn conversations(&self) -> Vec<Conversation> {
        let mut latest: HashMap<String, (&Message, usize)> = HashMap::new();

        for message in &self.messages {
            let unread = usize::from(message.direction == Direction::Incoming && !message.read);
            latest
                .entry(message.conversation())
                .and_modify(|(last, count)| {
                    *count += unread;
                    if (message.time, message.id) > (last.time, last.id) {
                        *last = message;
                    }
                })
                .or_insert((message, unread));
        }

        let mut conversations: Vec<Conversation> = latest
            .into_iter()
            .map(|(key, (last, unread))| {
                let name = self
                    .messages
                    .iter()
                    .filter(|m| m.conversation() == key)
                    .find_map(|m| m.name.as_deref());
                Conversation {
                    title: self.display_name(&last.address, name),
                    address: last.address.clone(),
                    preview: last.text.split_whitespace().collect::<Vec<_>>().join(" "),
                    time: last.time,
                    unread,
                    key,
                }
            })
            .collect();

        conversations.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| a.key.cmp(&b.key)));
        conversations
    }

    /// The messages of conversation `key`, oldest first.
    pub fn thread(&self, key: &str) -> Vec<Message> {
        let mut thread: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.conversation() == key)
            .cloned()
            .collect();
        thread.sort_by_key(|m| (m.time, m.id));
        thread
    }

    /// Marks the received messages of conversation `key` read, and returns the handles
    /// to mark read on the phone.
    pub fn mark_read(&mut self, key: &str) -> Vec<u64> {
        let mut handles = Vec::new();
        let mut changed = false;

        for message in &mut self.messages {
            if message.direction == Direction::Incoming
                && !message.read
                && message.conversation() == key
            {
                message.read = true;
                changed = true;
                handles.extend(message.handle);
            }
        }
        if changed {
            self.changed();
        }
        handles
    }

    /// The number of unread received messages in conversation `key`.
    pub fn unread_in(&self, key: &str) -> usize {
        self.messages
            .iter()
            .filter(|m| m.direction == Direction::Incoming && !m.read && m.conversation() == key)
            .count()
    }

    /// The number of unread received messages.
    pub fn unread(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| m.direction == Direction::Incoming && !m.read)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::Phone;

    fn synced(handle: u64, direction: Direction, address: &str, text: &str, time: i64) -> Synced {
        Synced {
            handle,
            direction,
            address: address.to_string(),
            name: None,
            text: text.to_string(),
            time,
            read: false,
        }
    }

    #[test]
    fn merging_the_same_handle_twice_updates_it() {
        let mut store = Store::in_memory();
        let id = store.merge(synced(1, Direction::Incoming, "+15555550001", "Hi", 100));
        assert!(id.is_some());

        let mut again = synced(1, Direction::Incoming, "+15555550001", "Hi", 100);
        again.read = true;
        assert_eq!(None, store.merge(again));
        assert_eq!(1, store.messages.len());
        assert!(store.messages[0].read);
    }

    #[test]
    fn messages_from_an_earlier_session_are_recognized() {
        let mut store = Store::in_memory();
        store.merge(synced(1, Direction::Incoming, "+15555550001", "Hi", 100));
        store.forget_handles();

        assert_eq!(
            None,
            store.merge(synced(7, Direction::Incoming, "5555550001", "Hi", 100))
        );
        assert_eq!(Some(7), store.messages[0].handle);
        assert!(store
            .merge(synced(8, Direction::Incoming, "5555550001", "Hi", 5000))
            .is_some());
    }

    #[test]
    fn sent_folder_copies_replace_local_messages() {
        let mut store = Store::in_memory();
        let id = store.add_outgoing("+1 555 000 1111", "On my way", 1_000);
        store.set_handle(id, 42);

        let copy = synced(99, Direction::Outgoing, "+15550001111", "On my way", 1_300);
        assert_eq!(None, store.merge(copy));
        let message = store.get(id).unwrap();
        assert_eq!(Some(99), message.handle);
        assert_eq!(1_300, message.time);
        assert_eq!(Status::Sent, message.status);

        // The same text sent again later is a new message.
        let again = synced(100, Direction::Outgoing, "+15550001111", "On my way", 1_400);
        assert!(store.merge(again).is_some());
    }

    #[test]
    fn conversations_group_and_count_unread() {
        let mut store = Store::in_memory();
        store.merge(synced(1, Direction::Incoming, "+15555550001", "First", 100));
        store.merge(synced(2, Direction::Incoming, "5555550001", "Second", 200));
        store.merge(synced(3, Direction::Incoming, "+15555550002", "Other", 150));
        store.add_outgoing("+15555550002", "Reply", 300);

        let conversations = store.conversations();
        assert_eq!(2, conversations.len());
        assert_eq!("Reply", conversations[0].preview);
        assert_eq!(1, conversations[0].unread);
        assert_eq!("Second", conversations[1].preview);
        assert_eq!(2, conversations[1].unread);

        let key = conversations[1].key.clone();
        assert_eq!(vec![1, 2], store.mark_read(&key));
        assert_eq!(1, store.unread());
        assert_eq!(2, store.thread(&key).len());
    }

    #[test]
    fn names_come_from_contacts_then_the_phone() {
        let mut store = Store::in_memory();
        let mut message = synced(1, Direction::Incoming, "+15551230001", "Hi", 100);
        message.name = Some("Ann (phone)".to_string());
        store.merge(message);
        assert_eq!("Ann (phone)", store.conversations()[0].title);

        store.set_contacts(vec![Contact {
            name: "Ann".to_string(),
            phones: vec![Phone {
                number: "(555) 123-0002".to_string(),
                label: "mobile".to_string(),
            }],
            emails: Vec::new(),
        }]);
        assert_eq!("Ann (phone)", store.conversations()[0].title);

        store.set_contacts(vec![Contact {
            name: "Ann".to_string(),
            phones: vec![Phone {
                number: "555-123-0001".to_string(),
                label: "mobile".to_string(),
            }],
            emails: Vec::new(),
        }]);
        assert_eq!("Ann", store.conversations()[0].title);
    }

    #[test]
    fn saves_and_loads() {
        let directory = std::env::temp_dir().join(format!("ferry-store-{}", std::process::id()));
        let path = directory.join("phone.json");

        let mut store = Store::open(&path);
        let id = store.add_outgoing("+15555550001", "Saved", 10);
        store.set_handle(id, 5);
        let (path, bytes) = store.snapshot().unwrap();
        Store::write_snapshot(&path, &bytes).unwrap();

        let loaded = Store::open(&path);
        assert_eq!(1, loaded.messages.len());
        assert_eq!("Saved", loaded.messages[0].text);
        assert_eq!(None, loaded.messages[0].handle);

        let mut loaded = loaded;
        assert_ne!(id, loaded.add_outgoing("+15555550001", "Next", 11));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
