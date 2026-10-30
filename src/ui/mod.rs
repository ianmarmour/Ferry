//! ## User Interface
//!
//! An egui window in the style of Messages on the Mac: conversations in a sidebar,
//! the selected conversation as bubbles, and a composer. The UI never talks to the
//! phone itself; it reads the shared [`Store`] and sends [`Command`]s to the
//! [`Session`].
//!
//! Closing the window keeps Ferry running in the system tray, connected to the phone
//! and showing notifications. On Windows and macOS the window is hidden; on Linux,
//! where Wayland cannot hide windows, it is closed and opened again from the tray.
//!
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::{
    Color32, Frame, Key, KeyboardShortcut, Margin, Modifiers, Ui, UserAttentionType,
    ViewportCommand,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::bluetooth::{Backend, Device};
use crate::model::{Conversation, Message, Store};
use crate::notify::Notifier;
use crate::session::{cache_path, Command, Event, Outbox, Session, State, Timing};

pub mod icon;
mod instance;
mod photos;
mod sidebar;
pub mod theme;
mod thread;
pub mod tray;
mod welcome;
mod widgets;

use theme::Palette;
use tray::{Tray, Wake, Waker};

/// The application id: the name of the desktop entry and icon on Linux.
pub const APP_ID: &str = "io.github.ianmarmour.Ferry";

/// Whether closing the window hides it, rather than closing it and opening a new one
/// later. Wayland does not let applications hide their windows.
const HIDE_ON_CLOSE: bool = !cfg!(target_os = "linux");

/// What is persisted between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Whether closing the window keeps Ferry running in the tray.
    keep_running: bool,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Ferry")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    fn load() -> Settings {
        Settings::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Settings::path() else { return };
        let written = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|bytes| Store::write_snapshot(&path, &bytes));
        if let Err(error) = written {
            log::warn!("cannot save settings: {}", error);
        }
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            last_device: None,
            auto_connect: true,
            notifications: true,
            appearance: Appearance::System,
            keep_running: true,
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

/// Ferry: the session, the tray and the state of the window, which outlive the
/// window itself.
pub struct FerryApp {
    session: Session,
    events: mpsc::UnboundedReceiver<Event>,
    store: Arc<Mutex<Store>>,
    notifier: Notifier,
    backend: String,
    demo: bool,
    runtime: tokio::runtime::Handle,
    waker: Waker,
    wakes: Receiver<Wake>,
    tray: Option<Tray>,
    /// Whether the window is showing.
    visible: bool,
    quitting: bool,
    /// Whether the user was told that closing keeps Ferry in the tray.
    told_about_tray: bool,
    saved_settings: Settings,

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
    photos: photos::Photos,
}

/// Where messages are cached.
pub fn cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Ferry").map(|dirs| dirs.data_dir().join("phones"))
}

impl FerryApp {
    /// Starts the session on `runtime`. A `demo` session keeps nothing on disk.
    fn new(
        backend: Arc<dyn Backend>,
        runtime: tokio::runtime::Handle,
        demo: bool,
        waker: Waker,
        wakes: Receiver<Wake>,
    ) -> FerryApp {
        let settings = if demo {
            Settings::default()
        } else {
            Settings::load()
        };
        let cache_dir = if demo { None } else { cache_dir() };

        // Show the last phone's history right away, before it connects.
        let store = match (&cache_dir, &settings.last_device) {
            (Some(dir), Some(device)) => Store::open(cache_path(dir, device)),
            _ => Store::in_memory(),
        };
        let store = Arc::new(Mutex::new(store));

        let (sender, events) = mpsc::unbounded_channel();
        let session_waker = waker.clone();
        let outbox = Outbox::new(sender, move || session_waker.wake(Wake::Session));

        let session = {
            let _runtime = runtime.enter();
            Session::start(
                backend.clone(),
                store.clone(),
                cache_dir,
                Timing::default(),
                outbox,
            )
        };
        session.send(Command::RefreshDevices);

        let mut auto_connect = settings.auto_connect || demo;
        if let (false, true, Some(device)) = (demo, settings.auto_connect, &settings.last_device) {
            session.send(Command::Connect(device.clone()));
            auto_connect = false;
        }

        // The Windows and macOS trays need the event loop, so they start with the window.
        #[cfg(target_os = "linux")]
        let tray = Tray::start(&runtime, waker.clone());
        #[cfg(not(target_os = "linux"))]
        let tray = None;

        FerryApp {
            session,
            events,
            store,
            notifier: Notifier::new(runtime.clone()),
            backend: backend.name(),
            demo,
            runtime,
            waker,
            wakes,
            tray,
            visible: false,
            quitting: false,
            told_about_tray: false,
            saved_settings: settings.clone(),
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
            photos: photos::Photos::default(),
        }
    }

    /// Sets up a new window.
    fn attach(&mut self, cc: &eframe::CreationContext<'_>) {
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(self.settings.appearance);
        self.waker.set_context(Some(cc.egui_ctx.clone()));
        self.visible = true;
        self.title.clear();

        #[cfg(any(windows, target_os = "macos"))]
        if self.tray.is_none() {
            self.tray = Tray::start(self.waker.clone());
        }
    }

    /// Whether closing the window keeps Ferry running.
    fn keeps_running(&self) -> bool {
        self.tray.is_some() && self.settings.keep_running && !self.quitting
    }

    /// Quits Ferry.
    fn quit(&mut self, ctx: Option<&egui::Context>) {
        self.quitting = true;
        if let Some(ctx) = ctx {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    /// Acts on the tray, other launches and the session; `ctx` is the open window's.
    fn tick(&mut self, ctx: Option<&egui::Context>) {
        while let Ok(wake) = self.wakes.try_recv() {
            match (wake, ctx) {
                (Wake::Open, Some(ctx)) => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.visible = true;
                }
                (Wake::Quit, ctx) => self.quit(ctx),
                (Wake::Open | Wake::Session, _) => {}
            }
        }
        self.update(ctx);
    }

    /// Takes in the session's events and keeps the tray, title and settings current.
    fn update(&mut self, ctx: Option<&egui::Context>) {
        self.process_events(ctx);
        let unread = self.store().unread();
        if let Some(tray) = &mut self.tray {
            tray.set_unread(unread);
        }
        if let Some(ctx) = ctx {
            self.update_title(ctx, unread);
        }
        if self.settings != self.saved_settings {
            if !self.demo {
                self.settings.save();
            }
            self.saved_settings = self.settings.clone();
        }
    }

    /// Runs without a window until the tray asks for one; returns whether to open it.
    fn background(&mut self) -> bool {
        self.visible = false;
        loop {
            match self.wakes.recv_timeout(Duration::from_secs(1)) {
                Ok(Wake::Open) => return true,
                Ok(Wake::Quit) | Err(RecvTimeoutError::Disconnected) => return false,
                Ok(Wake::Session) | Err(RecvTimeoutError::Timeout) => self.update(None),
            }
        }
    }

    /// Opens the window and runs it until it closes.
    fn show_window(&mut self) -> eframe::Result {
        let mut viewport = egui::ViewportBuilder::default()
            .with_title("Ferry")
            .with_app_id(APP_ID)
            .with_icon(Arc::new(icon::window_icon()))
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
        let name = if self.demo { "Ferry Demo" } else { "Ferry" };

        let result = eframe::run_native(
            name,
            options,
            Box::new(|cc| {
                self.attach(cc);
                Ok(Box::new(Window(self)))
            }),
        );
        self.waker.set_context(None);
        self.visible = false;
        result
    }

    /// Disconnects from the phone politely before quitting.
    fn shutdown(&mut self) {
        if self.state == State::Disconnected {
            return;
        }
        self.session.send(Command::Disconnect);
        let events = &mut self.events;
        self.runtime.block_on(async {
            let disconnected = async {
                while let Some(event) = events.recv().await {
                    if event == Event::State(State::Disconnected) {
                        break;
                    }
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(4), disconnected).await;
        });
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

    fn process_events(&mut self, ctx: Option<&egui::Context>) {
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
    fn announce(&mut self, ctx: Option<&egui::Context>, ids: &[u64]) {
        let focused = self.visible
            && ctx.is_some_and(|ctx| ctx.input(|input| input.viewport().focused.unwrap_or(true)));
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
                self.notifier.show(&title, &message.summary());
            }
        }

        if let (true, false, Some(ctx)) = (notified, focused, ctx) {
            if self.visible {
                ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                    UserAttentionType::Informational,
                ));
            }
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

    fn update_title(&mut self, ctx: &egui::Context, unread: usize) {
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

/// A window onto [`FerryApp`].
struct Window<'a>(&'a mut FerryApp);

impl eframe::App for Window<'_> {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.tick(Some(ctx));
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let app = &mut *self.0;
        let ctx = ui.ctx().clone();

        let quit = KeyboardShortcut::new(Modifiers::COMMAND, Key::Q);
        if ctx.input_mut(|input| input.consume_shortcut(&quit)) {
            app.quit(Some(&ctx));
        }
        if ctx.input(|input| input.viewport().close_requested()) && app.keeps_running() {
            if HIDE_ON_CLOSE {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                app.visible = false;
            }
            if !app.told_about_tray {
                app.told_about_tray = true;
                app.notifier.show(
                    "Ferry is still running",
                    "New messages will keep arriving. Open or quit Ferry from its tray icon.",
                );
            }
        }

        app.mark_read(&ctx);
        let palette = Palette::of(&ctx);

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(300.0)
            .size_range(240.0..=420.0)
            .frame(Frame::new().fill(palette.sidebar))
            .show(ui, |ui| app.sidebar(ui, &palette));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(palette.window))
            .show(ui, |ui| app.main_area(ui, &palette));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::TRANSPARENT.to_normalized_gamma_f32()
    }
}

/// Runs Ferry until it quits: the window, then the tray while the window is closed.
pub fn run(
    backend: Arc<dyn Backend>,
    runtime: tokio::runtime::Handle,
    demo: bool,
) -> eframe::Result {
    let (waker, wakes) = Waker::new();
    let name = if demo { "ferry-demo" } else { "ferry" };
    if !instance::claim(name, waker.clone()) {
        log::info!("Ferry is already running; showing its window");
        return Ok(());
    }

    let mut app = FerryApp::new(backend, runtime, demo, waker, wakes);
    loop {
        app.show_window()?;
        if app.quitting || !app.keeps_running() || !app.background() {
            break;
        }
    }
    app.shutdown();
    Ok(())
}
