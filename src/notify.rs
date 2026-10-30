//! ## Desktop Notifications
//!
//! New messages are announced with the platform's notifications through
//! `notify-rust`: the freedesktop notification service over D-Bus on Linux, the
//! Notification Center on macOS and toast notifications on Windows.
//!

/// Shows desktop notifications without blocking the UI.
pub struct Notifier {
    runtime: tokio::runtime::Handle,
}

impl Notifier {
    /// Creates a notifier delivering notifications from `runtime`.
    pub fn new(runtime: tokio::runtime::Handle) -> Notifier {
        Notifier { runtime }
    }

    /// Notifies of a message from `sender`, with the sender's photo if there is one.
    pub fn show(&self, sender: &str, text: &str, photo: Option<&std::path::Path>) {
        let mut notification = notify_rust::Notification::new();
        notification.appname("Tidings").summary(sender).body(text);
        if let Some(photo) = photo {
            notification.image_path(&photo.to_string_lossy());
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            notification
                .icon(crate::ui::APP_ID)
                .hint(notify_rust::Hint::DesktopEntry(
                    crate::ui::APP_ID.to_string(),
                ))
                .hint(notify_rust::Hint::Category("im.received".to_string()));
            self.runtime.spawn(async move {
                if let Err(error) = notification.show_async().await {
                    log::warn!("cannot show a notification: {}", error);
                }
            });
        }

        #[cfg(not(all(unix, not(target_os = "macos"))))]
        self.runtime.spawn_blocking(move || {
            if let Err(error) = notification.show() {
                log::warn!("cannot show a notification: {}", error);
            }
        });
    }
}
