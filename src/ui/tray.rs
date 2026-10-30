//! The system tray icon, which keeps Tidings reachable while its window is closed.
//!
//! On Linux it is a StatusNotifierItem over D-Bus (KDE, Xfce, Cinnamon, and GNOME
//! with the AppIndicator extension); on Windows and macOS it uses the platform tray
//! through `tray-icon`. Either way, clicks become [`Wake`]s for the window.

use std::sync::{Arc, Mutex};

use super::icon;

/// Why the window's loop should wake up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// The session has news.
    Session,
    /// Show the window.
    Open,
    /// Quit Tidings.
    Quit,
}

/// Wakes the window's loop, whether a window is open or not.
#[derive(Clone)]
pub struct Waker {
    sender: std::sync::mpsc::Sender<Wake>,
    context: Arc<Mutex<Option<egui::Context>>>,
}

impl Waker {
    /// A waker and the receiver of its wakes.
    pub fn new() -> (Waker, std::sync::mpsc::Receiver<Wake>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let waker = Waker {
            sender,
            context: Arc::new(Mutex::new(None)),
        };
        (waker, receiver)
    }

    /// Sets the context of the open window, if any.
    pub fn set_context(&self, context: Option<egui::Context>) {
        *self
            .context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = context;
    }

    /// Wakes the loop.
    pub fn wake(&self, wake: Wake) {
        let _ = self.sender.send(wake);
        let context = self
            .context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(context) = context.as_ref() {
            context.request_repaint();
        }
    }
}

fn tooltip(unread: usize) -> String {
    match unread {
        0 => "Tidings".to_string(),
        1 => "Tidings: 1 unread message".to_string(),
        unread => format!("Tidings: {} unread messages", unread),
    }
}

#[cfg(target_os = "linux")]
pub use linux::Tray;

#[cfg(target_os = "linux")]
mod linux {
    use ksni::menu::StandardItem;
    use ksni::{MenuItem, TrayMethods};

    use super::{icon, tooltip, Wake, Waker};

    struct Item {
        waker: Waker,
        unread: usize,
    }

    fn argb(size: u32, badge: bool) -> ksni::Icon {
        let (rgba, size) = icon::rgba(size, badge);
        let data = rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
            .collect();
        ksni::Icon {
            width: size as i32,
            height: size as i32,
            data,
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "tidings".to_string()
        }

        fn title(&self) -> String {
            "Tidings".to_string()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22, 32, 48, 64]
                .into_iter()
                .map(|size| argb(size, self.unread > 0))
                .collect()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: tooltip(self.unread),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.waker.wake(Wake::Open);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: "Open Tidings".to_string(),
                    activate: Box::new(|item: &mut Self| item.waker.wake(Wake::Open)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit Tidings".to_string(),
                    activate: Box::new(|item: &mut Self| item.waker.wake(Wake::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// The tray icon.
    pub struct Tray {
        handle: ksni::Handle<Item>,
        runtime: tokio::runtime::Handle,
        unread: usize,
    }

    impl Tray {
        /// Shows the tray icon, or returns `None` if the desktop has no tray.
        pub fn start(runtime: &tokio::runtime::Handle, waker: Waker) -> Option<Tray> {
            let item = Item { waker, unread: 0 };
            match runtime.block_on(item.spawn()) {
                Ok(handle) => Some(Tray {
                    handle,
                    runtime: runtime.clone(),
                    unread: 0,
                }),
                Err(error) => {
                    log::warn!(
                        "no system tray ({}); on GNOME, install the AppIndicator extension",
                        error
                    );
                    None
                }
            }
        }

        /// Shows the number of unread messages.
        pub fn set_unread(&mut self, unread: usize) {
            if unread == self.unread {
                return;
            }
            self.unread = unread;
            let handle = self.handle.clone();
            self.runtime.spawn(async move {
                handle.update(|item| item.unread = unread).await;
            });
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub use native::Tray;

#[cfg(any(windows, target_os = "macos"))]
mod native {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{icon, tooltip, Wake, Waker};

    fn image(badge: bool) -> Option<tray_icon::Icon> {
        let (rgba, size) = icon::rgba(64, badge);
        tray_icon::Icon::from_rgba(rgba, size, size).ok()
    }

    /// The tray icon.
    pub struct Tray {
        icon: TrayIcon,
        unread: usize,
    }

    impl Tray {
        /// Shows the tray icon; must be called on the main thread with the event loop
        /// running.
        pub fn start(waker: Waker) -> Option<Tray> {
            let menu = Menu::new();
            let open = MenuItem::new("Open Tidings", true, None);
            let quit = MenuItem::new("Quit Tidings", true, None);
            menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
                .ok()?;

            let (open, quit) = (open.id().clone(), quit.id().clone());
            let menu_waker = waker.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if event.id == open {
                    menu_waker.wake(Wake::Open);
                } else if event.id == quit {
                    menu_waker.wake(Wake::Quit);
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    waker.wake(Wake::Open);
                }
            }));

            let built = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip(tooltip(0))
                .with_icon(image(false)?)
                .build();
            match built {
                Ok(icon) => Some(Tray { icon, unread: 0 }),
                Err(error) => {
                    log::warn!("no system tray: {}", error);
                    None
                }
            }
        }

        /// Shows the number of unread messages.
        pub fn set_unread(&mut self, unread: usize) {
            if unread == self.unread {
                return;
            }
            self.unread = unread;
            let _ = self.icon.set_icon(image(unread > 0));
            let _ = self.icon.set_tooltip(Some(tooltip(unread)));
        }
    }
}
