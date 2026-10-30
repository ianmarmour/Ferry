//! The sidebar: connection status, search and the conversation list.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{pos2, vec2, Align, Color32, FontId, Layout, Response, RichText, Sense, Stroke, Ui};

use super::theme::{bold_font, Palette};
use super::widgets::{icon_button, paint_avatar, paint_compose, search_field, short_date};
use super::{FerryApp, View};
use crate::model::Conversation;
use crate::session::{Command, State};

/// Room for the window buttons drawn over the content on macOS.
const TITLE_BAR: f32 = if cfg!(target_os = "macos") { 28.0 } else { 6.0 };

impl FerryApp {
    pub(super) fn sidebar(&mut self, ui: &mut Ui, palette: &Palette) {
        ui.add_space(TITLE_BAR);

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.status(ui, palette);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let compose = icon_button(ui, 26.0, "New Message", paint_compose);
                        if compose.clicked() {
                            self.compose = Default::default();
                            self.compose.focus = true;
                            self.view = View::Compose;
                        }
                    });
                });
                ui.add_space(4.0);
                search_field(ui, &mut self.search, "Search", palette);
            });

        let query = self.search.trim().to_lowercase();
        let conversations: Vec<Conversation> = self
            .conversations()
            .into_iter()
            .filter(|c| {
                query.is_empty()
                    || c.title.to_lowercase().contains(&query)
                    || c.preview.to_lowercase().contains(&query)
                    || c.address.contains(&query)
            })
            .collect();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if conversations.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        let text = if query.is_empty() {
                            "No Messages"
                        } else {
                            "No Results"
                        };
                        ui.label(RichText::new(text).color(palette.secondary));
                    });
                }
                for conversation in &conversations {
                    let selected = self.view == View::Conversation(conversation.key.clone());
                    let row = conversation_row(ui, conversation, selected, palette);
                    if row.clicked() {
                        self.open(conversation.key.clone());
                    }
                    row.context_menu(|ui| {
                        if ui.button("Mark as Read").clicked() {
                            let handles = self.store().mark_read(&conversation.key);
                            self.session.send(Command::MarkRead(handles));
                            ui.close();
                        }
                    });
                }
            });
    }

    /// The phone and its connection state, with a menu.
    fn status(&mut self, ui: &mut Ui, palette: &Palette) {
        let (color, label) = match &self.state {
            State::Disconnected => (palette.secondary, "Not Connected".to_string()),
            State::Connecting(device) => {
                (palette.warning, format!("Connecting to {}…", device.name))
            }
            State::Connected {
                device,
                syncing: true,
                ..
            } => (palette.online, format!("{} · Updating…", device.name)),
            State::Connected { device, .. } => (palette.online, device.name.clone()),
            State::Retrying { device, .. } => {
                (palette.failed, format!("{} · Reconnecting…", device.name))
            }
        };

        let response = ui
            .horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                ui.add(
                    egui::Label::new(RichText::new(label).size(12.5).color(palette.secondary))
                        .truncate(),
                )
            })
            .inner;
        if let State::Retrying { error, .. } = &self.state {
            response.clone().on_hover_text(error.clone());
        }

        egui::Popup::menu(&response.interact(Sense::click())).show(|ui| {
            ui.set_min_width(200.0);
            match self.state.clone() {
                State::Disconnected => {
                    if ui.button("Connect…").clicked() {
                        self.view = View::Empty;
                        self.session.send(Command::RefreshDevices);
                    }
                }
                State::Retrying { .. } => {
                    if ui.button("Retry Now").clicked() {
                        self.session.send(Command::Sync);
                    }
                    if ui.button("Disconnect").clicked() {
                        self.session.send(Command::Disconnect);
                    }
                }
                State::Connecting(_) => {
                    if ui.button("Cancel").clicked() {
                        self.session.send(Command::Disconnect);
                    }
                }
                State::Connected { .. } => {
                    if ui.button("Check for New Messages").clicked() {
                        self.session.send(Command::Sync);
                    }
                    if ui.button("Reload Contacts").clicked() {
                        self.session.send(Command::ReloadContacts);
                    }
                    if ui.button("Disconnect").clicked() {
                        self.session.send(Command::Disconnect);
                    }
                }
            }
            ui.separator();
            if ui.button("Switch Phone…").clicked() {
                self.view = View::Empty;
                self.session.send(Command::Disconnect);
                self.session.send(Command::RefreshDevices);
            }
            ui.checkbox(&mut self.settings.notifications, "Show Notifications");
            ui.horizontal(|ui| {
                let before = self.settings.appearance;
                for (appearance, label) in [
                    (super::Appearance::System, "System"),
                    (super::Appearance::Light, "Light"),
                    (super::Appearance::Dark, "Dark"),
                ] {
                    ui.selectable_value(&mut self.settings.appearance, appearance, label);
                }
                if self.settings.appearance != before {
                    ui.ctx().set_theme(self.settings.appearance);
                }
            });
            if !self.demo {
                ui.checkbox(&mut self.settings.auto_connect, "Connect on Startup");
            }
            ui.separator();
            ui.label(
                RichText::new(format!("Bluetooth: {}", self.backend))
                    .size(11.5)
                    .color(palette.secondary),
            );
        });
    }
}

/// Lays out `text` in at most `rows` rows, ending with an ellipsis if it is cut.
fn truncated(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
    rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: width.max(1.0),
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

fn conversation_row(
    ui: &mut Ui,
    conversation: &Conversation,
    selected: bool,
    palette: &Palette,
) -> Response {
    let height = 66.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let row = rect.shrink2(vec2(8.0, 1.0));
    let painter = ui.painter();
    if selected {
        painter.rect_filled(row, 8.0, palette.accent);
    } else if response.hovered() {
        painter.rect_filled(row, 8.0, palette.field.gamma_multiply(0.6));
    }

    let (text, secondary) = if selected {
        (
            palette.selection_text,
            palette.selection_text.gamma_multiply(0.85),
        )
    } else {
        (palette.text, palette.secondary)
    };

    if conversation.unread > 0 {
        let dot = if selected {
            Color32::WHITE
        } else {
            palette.accent
        };
        painter.circle_filled(pos2(row.left() + 9.0, row.center().y), 4.5, dot);
    }

    let avatar = pos2(row.left() + 40.0, row.center().y);
    paint_avatar(ui, avatar, 21.0, &conversation.title, palette);

    let left = avatar.x + 31.0;
    let right = row.right() - 10.0;
    let date = ui.painter().layout_no_wrap(
        short_date(conversation.time),
        FontId::proportional(12.0),
        secondary,
    );
    let date_pos = pos2(right - date.size().x, row.top() + 10.0);

    let name = truncated(
        ui,
        &conversation.title,
        bold_font(14.0),
        text,
        date_pos.x - left - 8.0,
        1,
    );
    let preview = truncated(
        ui,
        &conversation.preview,
        FontId::proportional(13.0),
        secondary,
        right - left,
        2,
    );

    let painter = ui.painter();
    painter.galley(pos2(left, row.top() + 8.0), name, text);
    painter.galley(date_pos, date, secondary);
    painter.galley(pos2(left, row.top() + 27.0), preview, secondary);

    if !selected {
        painter.line_segment(
            [
                pos2(left, rect.bottom() - 0.5),
                pos2(rect.right() - 8.0, rect.bottom() - 0.5),
            ],
            Stroke::new(0.5, palette.separator),
        );
    }

    response
}
