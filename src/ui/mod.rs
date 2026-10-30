//! ## User Interface
//!
//! An egui window in the style of Messages on the Mac: conversations in a sidebar,
//! the selected conversation as bubbles, and a composer. The UI never talks to the
//! phone itself; it reads the shared [`Store`] and sends [`Command`]s to the
//! [`Session`].
//!
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::{Color32, Frame, Margin, Ui, UserAttentionType, ViewportCommand};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::bluetooth::{Backend, Device};
use crate::model::{Conversation, Message, Store};
use crate::notify::Notifier;
use crate::session::{cache_path, Command, Event, Outbox, Session, State, Timing};

mod sidebar;
pub mod theme;
mod thread;
mod welcome;
mod widgets;

use theme::Palette;

/// What is persisted between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    /// The phone used last.
    last_device: Option<Device>,
    /// Whether to connect to it on startup.
    auto_connect: bool,
    /// Whether to show desktop notifications for new messages.
    notifications: bool,
    /// Light, dark, or following the system.
    appearance: Appearance,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            last_device: None,
            auto_connect: true,
            notifications: true,
            appearance: Appearance::System,
        }
    }
}

/// The window's appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Appearance {
    System,
    Light,
    Dark,
}

impl From<Appearance> for egui::ThemePreference {
    fn from(appearance: Appearance) -> egui::ThemePreference {
        match appearance {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        }
    }
}

const SETTINGS_KEY: &str = "ferry";

/// What the main area shows.
#[derive(Debug, Clone, PartialEq)]
enum View {
    /// Nothing selected.
    Empty,
    /// A conversation, by key.
    Conversation(String),
    /// A new message, with its recipient being chosen.
    Compose,
}

/// The recipient of a new message.
#[derive(Debug, Clone, Default)]
struct Compose {
    query: String,
    recipient: Option<(String, String)>,
    focus: bool,
}

struct Notice {
    text: String,
    since: Instant,
}

/// The Ferry window.
pub struct FerryApp {
    session: Session,
    events: mpsc::UnboundedReceiver<Event>,
    store: Arc<Mutex<Store>>,
    notifier: Notifier,
    backend: String,
    demo: bool,

    state: State,
    devices: Option<Result<Vec<Device>, String>>,
    settings: Settings,
    auto_connect: bool,

    view: View,
    search: String,
    drafts: HashMap<String, String>,
    compose: Compose,
    notices: Vec<Notice>,
    scroll_to_bottom: bool,
    title: String,
    conversations: (u64, Vec<Conversation>),
    thread: Option<(u64, String, Vec<Message>)>,
}

/// Where messages are cached.
pub fn cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Ferry").map(|dirs| dirs.data_dir().join("phones"))
}

impl FerryApp {
    /// Creates the window's state and starts the session on `runtime`. A `demo`
    /// session keeps nothing on disk.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        backend: Arc<dyn Backend>,
        runtime: tokio::runtime::Handle,
        demo: bool,
    ) -> FerryApp {
        theme::install(&cc.egui_ctx);

        let settings: Settings = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, SETTINGS_KEY))
            .unwrap_or_default();
        cc.egui_ctx.set_theme(settings.appearance);
        let cache_dir = if demo { None } else { cache_dir() };

        // Show the last phone's history right away, before it connects.
        let store = match (&cache_dir, &settings.last_device) {
            (Some(dir), Some(device)) => Store::open(cache_path(dir, device)),
            _ => Store::in_memory(),
        };
        let store = Arc::new(Mutex::new(store));

        let (sender, events) = mpsc::unbounded_channel();
        let ctx = cc.egui_ctx.clone();
        let outbox = Outbox::new(sender, move || ctx.request_repaint());

        let _runtime = runtime.enter();
        let session = Session::start(
            backend.clone(),
            store.clone(),
            cache_dir,
            Timing::default(),
            outbox,
        );
        session.send(Command::RefreshDevices);

        let mut auto_connect = settings.auto_connect || demo;
        if let (false, true, Some(device)) = (demo, settings.auto_connect, &settings.last_device) {
            session.send(Command::Connect(device.clone()));
            auto_connect = false;
        }

        FerryApp {
            session,
            events,
            store,
            notifier: Notifier::new(runtime),
            backend: backend.name(),
            demo,
            state: State::Disconnected,
            devices: None,
            settings,
            auto_connect,
            view: View::Empty,
            search: String::new(),
            drafts: HashMap::new(),
            compose: Compose::default(),
            notices: Vec::new(),
            scroll_to_bottom: false,
            title: String::new(),
            conversations: (u64::MAX, Vec::new()),
            thread: None,
        }
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn connected(&self) -> bool {
        matches!(self.state, State::Connected { .. })
    }

    fn connect(&mut self, device: Device) {
        self.session.send(Command::Connect(device));
    }

    fn notice(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !self.notices.iter().any(|notice| notice.text == text) {
            self.notices.push(Notice {
                text,
                since: Instant::now(),
            });
        }
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Devices(devices) => {
                    if self.demo && self.auto_connect {
                        if let Some(device) =
                            devices.as_ref().ok().and_then(|devices| devices.first())
                        {
                            self.auto_connect = false;
                            self.connect(device.clone());
                        }
                    }
                    self.devices = Some(devices);
                }
                Event::State(state) => {
                    if let State::Connected { device, .. } = &state {
                        if !self.demo && self.settings.last_device.as_ref() != Some(device) {
                            self.settings.last_device = Some(device.clone());
                        }
                    }
                    self.state = state;
                }
                Event::Changed => {}
                Event::Received(ids) => self.announce(ctx, &ids),
                Event::Notice(text) => self.notice(text),
            }
        }
    }

    /// Notifies the user of new messages they are not looking at.
    fn announce(&mut self, ctx: &egui::Context, ids: &[u64]) {
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let mut notified = false;

        for &id in ids {
            let store = self.store();
            let Some(message) = store.get(id) else {
                continue;
            };
            let visible = focused && self.view == View::Conversation(message.conversation());
            if visible {
                continue;
            }
            notified = true;
            if self.settings.notifications {
                let title = store.display_name(&message.address, message.name.as_deref());
                self.notifier.show(&title, &message.text);
            }
        }

        if notified && !focused {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                UserAttentionType::Informational,
            ));
        }
    }

    /// Marks the open conversation read while the window is focused.
    fn mark_read(&mut self, ctx: &egui::Context) {
        let View::Conversation(key) = &self.view else {
            return;
        };
        if !ctx.input(|input| input.viewport().focused.unwrap_or(true)) {
            return;
        }
        let key = key.clone();
        let mut store = self.store();
        if store.unread_in(&key) == 0 {
            return;
        }
        let handles = store.mark_read(&key);
        drop(store);
        self.session.send(Command::MarkRead(handles));
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let unread = self.store().unread();
        let title = match unread {
            0 => "Ferry".to_string(),
            unread => format!("Ferry ({})", unread),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// The conversations, recomputed only when the store changed.
    fn conversations(&mut self) -> Vec<Conversation> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.conversations.0 != store.revision() {
            self.conversations = (store.revision(), store.conversations());
        }
        self.conversations.1.clone()
    }

    /// The messages of conversation `key`, recomputed only when the store changed.
    fn thread(&mut self, key: &str) -> Vec<Message> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = matches!(&self.thread, Some((revision, cached, _)) if *revision == store.revision() && cached == key);
        if !fresh {
            self.thread = Some((store.revision(), key.to_string(), store.thread(key)));
        }
        self.thread
            .as_ref()
            .map(|(_, _, messages)| messages.clone())
            .unwrap_or_default()
    }

    /// Opens a conversation.
    fn open(&mut self, key: String) {
        if self.view != View::Conversation(key.clone()) {
            self.view = View::Conversation(key);
            self.scroll_to_bottom = true;
        }
    }

    /// Adds a message to the store and asks the session to send it.
    fn send(&mut self, address: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let id = self
            .store()
            .add_outgoing(address, text, chrono::Utc::now().timestamp());
        self.session.send(Command::Send {
            id,
            address: address.to_string(),
            text: text.to_string(),
        });
        self.scroll_to_bottom = true;
    }

    /// Sends a failed message again.
    fn retry(&mut self, message: &Message) {
        self.store()
            .set_status(message.id, crate::model::Status::Sending);
        self.session.send(Command::Send {
            id: message.id,
            address: message.address.clone(),
            text: message.text.clone(),
        });
    }

    fn notices(&mut self, ui: &mut Ui, palette: &Palette) {
        self.notices
            .retain(|notice| notice.since.elapsed() < Duration::from_secs(20));
        let mut dismissed = None;
        for (i, notice) in self.notices.iter().enumerate() {
            Frame::new()
                .fill(palette.banner)
                .inner_margin(Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&notice.text).size(12.5)).wrap(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").clicked() {
                                dismissed = Some(i);
                            }
                        });
                    });
                });
        }
        if let Some(i) = dismissed {
            self.notices.remove(i);
        }
        if !self.notices.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
    }

    fn main_area(&mut self, ui: &mut Ui, palette: &Palette) {
        self.notices(ui, palette);

        match self.view.clone() {
            View::Conversation(key) => self.conversation_view(ui, &key, palette),
            View::Compose => self.compose_view(ui, palette),
            View::Empty if self.connected() => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("No Conversation Selected")
                            .color(palette.secondary)
                            .size(16.0),
                    );
                });
            }
            View::Empty => self.welcome(ui, palette),
        }
    }
}

impl eframe::App for FerryApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.process_events(&ctx);
        self.mark_read(&ctx);
        self.update_title(&ctx);

        let palette = Palette::of(&ctx);

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(300.0)
            .size_range(240.0..=420.0)
            .frame(Frame::new().fill(palette.sidebar))
            .show(ui, |ui| self.sidebar(ui, &palette));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(palette.window))
            .show(ui, |ui| self.main_area(ui, &palette));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::TRANSPARENT.to_normalized_gamma_f32()
    }
}

/// Opens the window and runs until it is closed.
pub fn run(
    backend: Arc<dyn Backend>,
    runtime: tokio::runtime::Handle,
    demo: bool,
) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Ferry")
        .with_app_id("ferry")
        .with_inner_size([1000.0, 680.0])
        .with_min_inner_size([680.0, 440.0]);
    if cfg!(target_os = "macos") {
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let name = if demo { "Ferry Demo" } else { "Ferry" };
    eframe::run_native(
        name,
        options,
        Box::new(move |cc| Ok(Box::new(FerryApp::new(cc, backend, runtime, demo)))),
    )
}
