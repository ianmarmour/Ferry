//! ## Demo Phone
//!
//! A simulated iPhone, for trying Ferry without a phone (`ferry --demo`) and for the
//! end to end tests. It runs real MAP and PBAP servers from the `map` and `obex`
//! crates over in-memory streams, so everything above the Bluetooth stack works as
//! it does with a phone: browsing, downloading, sending, contacts and notifications.
//! Contacts answer the messages they are sent, and new messages arrive now and then.
//!
use std::sync::{Arc, Mutex};
use std::time::Duration;

use map::bmessage::{BMessage, BMessageBody, VCard};
use map::event_report::{EventReport, EventType};
use map::folder_listing::{Folder, FolderListing};
use map::mas::{
    GetMessageParams, MessageStatus, MessagesListingParams, MessagesListingResponse,
    PushMessageParams, SetFolder,
};
use map::messages_listing::MessageEntry;
use map::tokio::{MasServer, MessageStore, MnsClient};
use map::types::{MessageHandle, MessageType};
use obex::header::Header;
use obex::packet::{Request, Response};
use obex::response::ResponseCode;
use obex::tlv::ApplicationParameters;
use obex::tokio::{Handler, Object, Server};
use tokio::sync::mpsc;

use super::{Backend, BoxFuture, Device, Link, Listener, Service};
use crate::pbap;

const FOLDERS: [&str; 6] = [
    "telecom",
    "telecom/msg",
    "telecom/msg/inbox",
    "telecom/msg/outbox",
    "telecom/msg/sent",
    "telecom/msg/deleted",
];

const OWNER: &str = "+14155550100";

/// The people in the demo phone's address book.
const PEOPLE: [(&str, &str); 6] = [
    ("Jane Appleseed", "+14155550101"),
    ("Mom", "+14155550102"),
    ("Alex Rivera", "+14155550103"),
    ("Sam Chen", "+14155550104"),
    ("Priya Patel", "+14155550105"),
    ("Book Club", "+14155550106"),
];

/// Chatter for the demo, by person: (minutes ago, sent by the owner, text).
const HISTORY: [(usize, i64, bool, &str); 14] = [
    (0, 60 * 26, false, "Are we still on for Saturday?"),
    (0, 60 * 25, true, "Yes! 10am at the farmers market"),
    (0, 12, false, "Perfect, I'll bring coffee ☕️"),
    (1, 60 * 50, false, "Call me when you get a chance"),
    (1, 60 * 49, true, "Will do, in a meeting until 4"),
    (1, 35, false, "Don't forget grandma's birthday on Sunday 🎂"),
    (2, 60 * 24 * 3, true, "Did you push the fix?"),
    (2, 60 * 24 * 3 - 5, false, "Yep, it's in main now"),
    (3, 60 * 24 * 8, false, "Thanks for dinner last night!"),
    (3, 60 * 24 * 8 - 20, true, "Anytime 😊"),
    (4, 90, false, "Running 10 minutes late, sorry!"),
    (4, 85, true, "No worries"),
    (4, 3, false, "Here now, grabbing a table by the window"),
    (5, 60 * 24 * 2, false, "Next book: Project Hail Mary"),
];

/// What iPhones send in place of a photo.
const PHOTO_PLACEHOLDER: &str = "Attachment: 1 Photo";

/// Draws a little sunset over the sea, as the demo's photo.
fn sunset() -> Vec<u8> {
    let (width, height) = (320u32, 220u32);
    let horizon = 150.0;
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| {
        let t = t.clamp(0.0, 1.0);
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32, y as f32);
            let sun = ((fx - 160.0).powi(2) + (fy - 138.0).powi(2)).sqrt();
            let color = if fy < horizon {
                let sky = mix([43.0, 16.0, 85.0], [255.0, 126.0, 95.0], fy / horizon);
                let sky = mix(sky, [254.0, 180.0, 123.0], (fy - 100.0) / 50.0);
                if sun < 34.0 {
                    [255.0, 214.0, 107.0]
                } else {
                    mix([255.0, 214.0, 107.0], sky, (sun - 34.0) / 30.0)
                }
            } else {
                let depth = (fy - horizon) / (height as f32 - horizon);
                let sea = mix([45.0, 70.0, 130.0], [14.0, 35.0, 71.0], depth);
                let glint = (fx - 160.0).abs() < 40.0 - depth * 30.0 && (y / 4) % 2 == 0;
                if glint {
                    mix(sea, [255.0, 190.0, 120.0], 0.6)
                } else {
                    sea
                }
            };
            pixels.extend(color.map(|channel| channel as u8));
            pixels.push(255);
        }
    }

    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("in memory");
    writer.write_image_data(&pixels).expect("in memory");
    drop(writer);
    png
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (i, chunk) in data.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push_str("\r\n");
        }
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
        for position in 0..4 {
            if position <= chunk.len() {
                out.push(ALPHABET[(value >> (18 - 6 * position) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The MMS content of a photo message, with the picture if `attachment`.
fn photo_message(caption: &str, png: &[u8], attachment: bool) -> String {
    let mut body = format!(
        "Content-Type: multipart/related; boundary=\"ferry\"\r\n\r\n\
         --ferry\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
        caption
    );
    if attachment {
        body.push_str(
            "--ferry\r\nContent-Type: image/png; name=\"sunset.png\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n",
        );
        body.push_str(&base64(png));
        body.push_str("\r\n");
    }
    body.push_str("--ferry--\r\n");
    body
}

const REPLIES: [&str; 6] = [
    "Sounds good!",
    "Haha 😂",
    "On my way",
    "Can't talk right now, text you later",
    "👍",
    "Really? Tell me more",
];

struct Stored {
    folder: String,
    entry: MessageEntry,
    message: BMessage,
    /// The caption and PNG of a photo message, sent as MMS.
    photo: Option<(String, Vec<u8>)>,
}

struct State {
    messages: Vec<Stored>,
    next_handle: u64,
    /// The MNS server of the connected client, from [`Backend::listen`].
    listener: Option<mpsc::UnboundedSender<Link>>,
    /// Reports to deliver to the client while notifications are registered.
    reports: Option<mpsc::UnboundedSender<EventReport>>,
    replies: usize,
}

impl State {
    fn add(
        &mut self,
        folder: &str,
        person: usize,
        outgoing: bool,
        minutes_ago: i64,
        text: &str,
        read: bool,
    ) -> MessageHandle {
        let handle = MessageHandle(self.next_handle);
        self.next_handle += 1;

        let (name, number) = PEOPLE[person];
        let datetime = (chrono::Local::now() - chrono::Duration::minutes(minutes_ago))
            .format("%Y%m%dT%H%M%S")
            .to_string();
        let (sender, recipient) = if outgoing {
            (OWNER, number)
        } else {
            (number, OWNER)
        };

        let entry = MessageEntry {
            subject: Some(text.chars().take(40).collect()),
            datetime: Some(datetime),
            sender_name: (!outgoing).then(|| name.to_string()),
            sender_addressing: Some(sender.to_string()),
            recipient_name: outgoing.then(|| name.to_string()),
            recipient_addressing: Some(recipient.to_string()),
            message_type: Some(MessageType::SmsGsm),
            size: Some(text.len() as u64),
            read: Some(read),
            sent: Some(outgoing),
            ..MessageEntry::new(handle)
        };
        let mut message = BMessage::new(MessageType::SmsGsm, BMessageBody::from_text(text))
            .with_originator(VCard::telephone(sender))
            .with_recipient(VCard::telephone(recipient));
        message.read = read;
        message.folder = folder.to_uppercase();

        self.messages.push(Stored {
            folder: folder.to_string(),
            entry,
            message,
            photo: None,
        });
        handle
    }

    fn report(&self, report: EventReport) {
        if let Some(reports) = &self.reports {
            let _ = reports.send(report);
        }
    }

    /// A message arrives from `person`.
    fn receive(&mut self, person: usize, text: &str) {
        let handle = self.add("telecom/msg/inbox", person, false, 0, text, false);
        self.report(EventReport::message(
            EventType::NewMessage,
            handle,
            "TELECOM/MSG/INBOX",
            MessageType::SmsGsm,
        ));
    }
}

/// The simulated phone. See [the module documentation](self).
#[derive(Clone)]
pub struct DemoPhone {
    state: Arc<Mutex<State>>,
    reply_delay: Duration,
}

impl Default for DemoPhone {
    fn default() -> DemoPhone {
        DemoPhone::new()
    }
}

impl DemoPhone {
    /// A phone with some message history whose contacts answer after a few seconds.
    pub fn new() -> DemoPhone {
        let mut state = State {
            messages: Vec::new(),
            next_handle: 0x0400_0000_0000_0001,
            listener: None,
            reports: None,
            replies: 0,
        };
        for (person, minutes_ago, outgoing, text) in HISTORY {
            let folder = if outgoing {
                "telecom/msg/sent"
            } else {
                "telecom/msg/inbox"
            };
            state.add(
                folder,
                person,
                outgoing,
                minutes_ago,
                text,
                outgoing || minutes_ago > 30,
            );
        }

        // Photos: one sent as MMS with the picture, and the placeholders iPhones
        // send in place of pictures they don't transfer.
        state.add(
            "telecom/msg/inbox",
            4,
            false,
            2,
            "Sunset from the ferry 🌅",
            false,
        );
        let stored = state.messages.last_mut().expect("just added");
        let png = sunset();
        stored.entry.message_type = Some(MessageType::Mms);
        stored.entry.attachment_size = Some(png.len() as u64);
        stored.message.message_type = MessageType::Mms;
        stored.photo = Some(("Sunset from the ferry 🌅".to_string(), png));
        state.add(
            "telecom/msg/inbox",
            2,
            false,
            60 * 24 * 3 - 7,
            PHOTO_PLACEHOLDER,
            true,
        );
        state.add(
            "telecom/msg/sent",
            3,
            true,
            60 * 24 * 8 - 25,
            PHOTO_PLACEHOLDER,
            true,
        );

        DemoPhone {
            state: Arc::new(Mutex::new(state)),
            reply_delay: Duration::from_secs(4),
        }
    }

    /// Sets how long contacts take to answer.
    pub fn with_reply_delay(mut self, delay: Duration) -> DemoPhone {
        self.reply_delay = delay;
        self
    }

    /// Simulates a message from `number` arriving.
    pub fn receive(&self, number: &str, text: &str) {
        let person = PEOPLE
            .iter()
            .position(|(_, n)| *n == number)
            .expect("one of the demo people");
        self.state.lock().unwrap().receive(person, text);
    }

    /// Every message in folder `folder` (e.g. `sent`), oldest first.
    pub fn folder(&self, folder: &str) -> Vec<String> {
        let path = format!("telecom/msg/{}", folder);
        self.state
            .lock()
            .unwrap()
            .messages
            .iter()
            .filter(|stored| stored.folder == path)
            .map(|stored| stored.message.text())
            .collect()
    }

    /// Whether a client has registered for notifications.
    pub fn notifications_registered(&self) -> bool {
        self.state.lock().unwrap().reports.is_some()
    }

    /// Sends a message from a demo contact every `period`, until the phone is dropped.
    pub fn chat_every(&self, period: Duration) {
        let state = Arc::downgrade(&self.state);
        tokio::spawn(async move {
            let mut turn = 0;
            loop {
                tokio::time::sleep(period).await;
                let Some(state) = state.upgrade() else { break };
                let text = [
                    "Hey! Got a minute?",
                    "Look outside, the sunset is amazing 🌅",
                    "Lunch tomorrow?",
                ][turn % 3];
                state.lock().unwrap().receive([0, 2, 4][turn % 3], text);
                turn += 1;
            }
        });
    }
}

/// The MAS of the demo phone, one per connection.
struct Mas {
    state: Arc<Mutex<State>>,
    path: Vec<String>,
    reply_delay: Duration,
}

impl Mas {
    fn resolve(&self, child: &str) -> Result<String, ResponseCode> {
        let mut path = self.path.clone();
        if !child.is_empty() {
            path.push(child.to_string());
        }
        let path = path.join("/");
        if path.is_empty() || FOLDERS.contains(&path.as_str()) {
            Ok(path)
        } else {
            Err(ResponseCode::NotFound)
        }
    }
}

impl MessageStore for Mas {
    async fn set_folder(&mut self, step: SetFolder) -> Result<(), ResponseCode> {
        match step {
            SetFolder::Root => self.path.clear(),
            SetFolder::Parent => {
                self.path.pop().ok_or(ResponseCode::NotFound)?;
            }
            SetFolder::Child(name) => {
                self.resolve(&name)?;
                self.path.push(name);
            }
            SetFolder::Sibling(name) => {
                self.path.pop().ok_or(ResponseCode::NotFound)?;
                self.resolve(&name)?;
                self.path.push(name);
            }
        }
        Ok(())
    }

    async fn get_folder_listing(&mut self) -> Result<FolderListing, ResponseCode> {
        let current = self.path.join("/");
        Ok(FolderListing::new(FOLDERS.iter().filter_map(|folder| {
            let (parent, name) = folder.rsplit_once('/').unwrap_or(("", folder));
            (parent == current).then(|| Folder::new(name))
        })))
    }

    async fn get_messages_listing(
        &mut self,
        folder: &str,
        params: &MessagesListingParams,
    ) -> Result<MessagesListingResponse, ResponseCode> {
        let folder = self.resolve(folder)?;
        let state = self.state.lock().unwrap();
        let entries = state
            .messages
            .iter()
            .filter(|stored| stored.folder == folder)
            .map(|stored| stored.entry.clone());
        Ok(params.apply(entries))
    }

    async fn get_message(
        &mut self,
        handle: MessageHandle,
        params: &GetMessageParams,
    ) -> Result<BMessage, ResponseCode> {
        let state = self.state.lock().unwrap();
        let stored = state
            .messages
            .iter()
            .find(|stored| stored.entry.handle == handle)
            .ok_or(ResponseCode::NotFound)?;
        let mut message = stored.message.clone();
        if let Some((caption, png)) = &stored.photo {
            *message.body_mut() =
                BMessageBody::from_text(photo_message(caption, png, params.attachment));
        }
        Ok(message)
    }

    async fn set_message_status(
        &mut self,
        handle: MessageHandle,
        status: MessageStatus,
    ) -> Result<(), ResponseCode> {
        let mut state = self.state.lock().unwrap();
        let stored = state
            .messages
            .iter_mut()
            .find(|stored| stored.entry.handle == handle)
            .ok_or(ResponseCode::NotFound)?;
        if let MessageStatus::Read(read) = status {
            stored.entry.read = Some(read);
            stored.message.read = read;
        }
        Ok(())
    }

    async fn push_message(
        &mut self,
        folder: &str,
        message: BMessage,
        _params: &PushMessageParams,
    ) -> Result<MessageHandle, ResponseCode> {
        if self.resolve(folder)? != "telecom/msg/outbox" {
            return Err(ResponseCode::BadRequest);
        }
        let recipient = message
            .recipients()
            .first()
            .and_then(|card| card.telephones.first().or(card.emails.first()))
            .cloned()
            .ok_or(ResponseCode::BadRequest)?;
        let person = PEOPLE.iter().position(|(_, number)| {
            crate::contacts::conversation_key(number)
                == crate::contacts::conversation_key(&recipient)
        });

        let mut state = self.state.lock().unwrap();
        let handle = match person {
            Some(person) => state.add("telecom/msg/sent", person, true, 0, &message.text(), true),
            None => return Err(ResponseCode::NotFound),
        };

        let reply = REPLIES[state.replies % REPLIES.len()];
        state.replies += 1;
        drop(state);

        let shared = self.state.clone();
        let delay = self.reply_delay;
        tokio::spawn(async move {
            tokio::time::sleep(delay / 8).await;
            for event in [EventType::SendingSuccess, EventType::DeliverySuccess] {
                shared.lock().unwrap().report(EventReport::message(
                    event,
                    handle,
                    "TELECOM/MSG/SENT",
                    MessageType::SmsGsm,
                ));
            }
            tokio::time::sleep(delay).await;
            if let Some(person) = person {
                shared.lock().unwrap().receive(person, reply);
            }
        });
        Ok(handle)
    }

    async fn update_inbox(&mut self) -> Result<(), ResponseCode> {
        Ok(())
    }

    async fn set_notification_registration(&mut self, enabled: bool) -> Result<(), ResponseCode> {
        let mut state = self.state.lock().unwrap();
        if !enabled {
            state.reports = None;
            return Ok(());
        }
        let listener = state
            .listener
            .clone()
            .ok_or(ResponseCode::ServiceUnavailable)?;

        let (client, server) = tokio::io::duplex(64 * 1024);
        listener
            .send(Link::stream(server))
            .map_err(|_| ResponseCode::ServiceUnavailable)?;

        let (reports, mut pending) = mpsc::unbounded_channel::<EventReport>();
        state.reports = Some(reports);
        tokio::spawn(async move {
            let mut mns = MnsClient::new(client);
            if mns.connect().await.is_err() {
                return;
            }
            while let Some(report) = pending.recv().await {
                if mns.send_event(0, &report).await.is_err() {
                    break;
                }
            }
            let _ = mns.disconnect().await;
        });
        Ok(())
    }
}

/// The PBAP server of the demo phone.
struct PhoneBook;

impl PhoneBook {
    fn vcards(offset: usize, count: usize) -> Vec<u8> {
        let mut cards = String::from("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Me\r\nN:;;;;\r\nTEL;TYPE=CELL:+14155550100\r\nEND:VCARD\r\n");
        for (name, number) in PEOPLE {
            let (given, family) = name.split_once(' ').unwrap_or((name, ""));
            cards.push_str(&format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\nN:{family};{given};;;\r\nTEL;TYPE=CELL:{number}\r\nEND:VCARD\r\n"
            ));
        }
        cards
            .split_inclusive("END:VCARD\r\n")
            .skip(offset)
            .take(count)
            .collect::<String>()
            .into_bytes()
    }
}

impl Handler for PhoneBook {
    async fn get(&mut self, request: &Request) -> Result<Object, Response> {
        if request.headers.content_type() != Some(pbap::PHONEBOOK_TYPE)
            || request.headers.name() != Some(pbap::MAIN_PHONEBOOK)
        {
            return Err(Response::new(ResponseCode::NotFound));
        }
        let parameters = request.headers.application_parameters();
        let get = |tag| parameters.and_then(|p| p.get_u16(tag));
        let count = get(pbap::tag::MAX_LIST_COUNT).unwrap_or(u16::MAX) as usize;
        let offset = get(pbap::tag::LIST_START_OFFSET).unwrap_or(0) as usize;

        if count == 0 {
            let size = ApplicationParameters::new()
                .with_u16(pbap::tag::PHONEBOOK_SIZE, PEOPLE.len() as u16 + 1);
            return Ok(Object::headers_only([Header::ApplicationParameters(size)]));
        }
        Ok(Object::new([], PhoneBook::vcards(offset, count)))
    }
}

impl Backend for DemoPhone {
    fn name(&self) -> String {
        "Demo".to_string()
    }

    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>> {
        Box::pin(async {
            Ok(vec![Device {
                id: "demo".to_string(),
                name: "Demo iPhone".to_string(),
                address: "00:00:5E:00:53:01".to_string(),
                phone: true,
            }])
        })
    }

    fn connect<'a>(
        &'a self,
        _device: &'a Device,
        service: Service,
    ) -> BoxFuture<'a, anyhow::Result<Link>> {
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(64 * 1024);
            match service {
                Service::MessageAccess => {
                    let store = Mas {
                        state: self.state.clone(),
                        path: Vec::new(),
                        reply_delay: self.reply_delay,
                    };
                    tokio::spawn(async move { MasServer::new(server, store).run().await });
                }
                Service::PhoneBook => {
                    tokio::spawn(async move { Server::new(server, PhoneBook).run().await });
                }
            }
            Ok(Link::stream(client))
        })
    }

    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>> {
        Box::pin(async {
            let (sender, incoming) = mpsc::unbounded_channel();
            self.state.lock().unwrap().listener = Some(sender);
            Ok(Listener::new(incoming, Unregister(self.state.clone())))
        })
    }
}

/// Removes the client's notification server when its [`Listener`] is dropped.
struct Unregister(Arc<Mutex<State>>);

impl Drop for Unregister {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.listener = None;
        state.reports = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!("aGVsbG8=", base64(b"hello"));
        assert_eq!("aGk=", base64(b"hi"));
        assert_eq!("YWJj", base64(b"abc"));
    }

    #[test]
    fn photo_messages_carry_the_picture_when_asked() {
        let png = sunset();
        assert!(png.starts_with(b"\x89PNG"));
        let message = BMessage::new(
            MessageType::Mms,
            BMessageBody::from_text(photo_message("Hi", &png, true)),
        );
        let parts = crate::text::attachments(&message);
        assert_eq!(1, parts.len());
        assert_eq!(png, parts[0].data);
        assert_eq!("Hi", crate::text::message_text(&message));

        let message = BMessage::new(
            MessageType::Mms,
            BMessageBody::from_text(photo_message("Hi", &png, false)),
        );
        assert!(crate::text::attachments(&message).is_empty());
    }
}
