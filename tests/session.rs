//! End to end: the session worker against the simulated iPhone, over real MAP, MNS and
//! PBAP sessions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferry::bluetooth::demo::DemoPhone;
use ferry::bluetooth::Backend;
use ferry::contacts::conversation_key;
use ferry::model::{Direction, Status, Store};
use ferry::session::{Command, Event, Outbox, Session, State, Timing};
use tokio::sync::mpsc;
use tokio::time::timeout;

struct Harness {
    phone: DemoPhone,
    store: Arc<Mutex<Store>>,
    session: Session,
    events: mpsc::UnboundedReceiver<Event>,
}

impl Harness {
    fn start() -> Harness {
        let phone = DemoPhone::new().with_reply_delay(Duration::from_millis(300));
        let store = Arc::new(Mutex::new(Store::in_memory()));
        let (sender, events) = mpsc::unbounded_channel();
        let timing = Timing {
            operation: Duration::from_secs(5),
            retry: vec![Duration::from_millis(100)],
            ..Timing::default()
        };

        let session = Session::start(
            Arc::new(phone.clone()),
            store.clone(),
            None,
            timing,
            Outbox::new(sender, || {}),
        );
        Harness {
            phone,
            store,
            session,
            events,
        }
    }

    /// Waits for an event matching `predicate`.
    async fn until(&mut self, what: &str, mut predicate: impl FnMut(&Event) -> bool) -> Event {
        let waiting = async {
            loop {
                let event = self.events.recv().await.expect("session running");
                if predicate(&event) {
                    return event;
                }
            }
        };
        timeout(Duration::from_secs(10), waiting)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {}", what))
    }

    /// Waits until the store satisfies `predicate`.
    async fn until_store(&mut self, what: &str, predicate: impl Fn(&Store) -> bool) {
        let store = self.store.clone();
        if predicate(&store.lock().unwrap()) {
            return;
        }
        self.until(what, |event| {
            if let Event::Notice(notice) = event {
                panic!("{}", notice);
            }
            predicate(&store.lock().unwrap())
        })
        .await;
    }

    async fn connect(&mut self) {
        let device = self.phone.devices().await.unwrap().remove(0);
        self.session.send(Command::Connect(device));
        self.until("the first sync", |event| {
            matches!(event, Event::State(State::Connected { syncing: false, .. }))
        })
        .await;
    }
}

const JANE: &str = "+14155550101";

#[tokio::test(flavor = "multi_thread")]
async fn downloads_messages_and_contacts() {
    let mut harness = Harness::start();
    harness.connect().await;
    assert!(harness.phone.notifications_registered());

    harness
        .until_store("contacts", |store| store.contacts.len() == 7)
        .await;

    let store = harness.store.lock().unwrap();
    assert_eq!(14, store.messages.len());
    let conversations = store.conversations();
    assert_eq!(6, conversations.len());

    let jane = conversations
        .iter()
        .find(|c| c.key == conversation_key(JANE))
        .unwrap();
    assert_eq!("Jane Appleseed", jane.title);
    assert_eq!("Perfect, I'll bring coffee ☕️", jane.preview);
    assert_eq!(1, jane.unread);

    let thread = store.thread(&jane.key);
    assert_eq!(3, thread.len());
    assert_eq!(Direction::Outgoing, thread[1].direction);
    assert_eq!("Yes! 10am at the farmers market", thread[1].text);
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_messages_and_receives_replies() {
    let mut harness = Harness::start();
    harness.connect().await;

    let id = harness.store.lock().unwrap().add_outgoing(
        JANE,
        "See you there!",
        chrono::Utc::now().timestamp(),
    );
    harness.session.send(Command::Send {
        id,
        address: JANE.to_string(),
        text: "See you there!".to_string(),
    });

    harness
        .until_store("delivery", |store| {
            store.get(id).unwrap().status == Status::Delivered
        })
        .await;
    assert_eq!(
        Some(&"See you there!".to_string()),
        harness.phone.folder("sent").last()
    );

    let received = harness
        .until("the reply", |event| matches!(event, Event::Received(_)))
        .await;
    let Event::Received(ids) = received else {
        unreachable!()
    };
    let store = harness.store.lock().unwrap();
    let reply = store.get(ids[0]).unwrap();
    assert_eq!("Sounds good!", reply.text);
    assert_eq!(conversation_key(JANE), reply.conversation());
    assert!(!reply.read);

    // The pushed message is in the phone's Sent folder; it must not be duplicated.
    assert_eq!(
        1,
        store
            .messages
            .iter()
            .filter(|m| m.text == "See you there!")
            .count()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notifications_and_read_status() {
    let mut harness = Harness::start();
    harness.connect().await;

    harness.phone.receive("+14155550103", "Ship it?");
    let Event::Received(ids) = harness
        .until("the new message", |event| {
            matches!(event, Event::Received(_))
        })
        .await
    else {
        unreachable!()
    };

    let handles = {
        let mut store = harness.store.lock().unwrap();
        assert_eq!("Ship it?", store.get(ids[0]).unwrap().text);
        let key = store.get(ids[0]).unwrap().conversation();
        store.mark_read(&key)
    };
    assert_eq!(1, handles.len());
    harness.session.send(Command::MarkRead(handles));

    harness.session.send(Command::Disconnect);
    harness
        .until("disconnection", |event| {
            matches!(event, Event::State(State::Disconnected))
        })
        .await;
    assert!(!harness.phone.notifications_registered());
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_while_disconnected_fails() {
    let mut harness = Harness::start();
    let id = harness
        .store
        .lock()
        .unwrap()
        .add_outgoing(JANE, "Hello?", 0);
    harness.session.send(Command::Send {
        id,
        address: JANE.to_string(),
        text: "Hello?".to_string(),
    });

    harness
        .until_store("failure", |store| {
            matches!(store.get(id).unwrap().status, Status::Failed(_))
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_does_not_duplicate_messages() {
    let mut harness = Harness::start();
    harness.connect().await;
    let before = harness.store.lock().unwrap().messages.len();

    harness.session.send(Command::Disconnect);
    harness
        .until("disconnection", |event| {
            matches!(event, Event::State(State::Disconnected))
        })
        .await;
    harness.connect().await;

    assert_eq!(before, harness.store.lock().unwrap().messages.len());
}
