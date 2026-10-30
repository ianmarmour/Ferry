//! A conversation: its header, the message bubbles and the composer; and the
//! recipient picker of a new message.

use egui::{
    pos2, vec2, Align, CornerRadius, FontId, Frame, Id, Key, KeyboardShortcut, Layout, Margin,
    Modifiers, Rect, RichText, Sense, Stroke, Ui,
};

use super::theme::{bold_font, Palette};
use super::widgets::{long_date, send_button};
use super::{FerryApp, View};
use crate::contacts::{conversation_key, format_address};
use crate::model::{Direction, Message, Status};

/// Messages further apart than this get a date between them.
const GAP: i64 = 15 * 60;

/// What the composer asked for.
enum Composed {
    Nothing,
    Send(String),
}

impl FerryApp {
    pub(super) fn conversation_view(&mut self, ui: &mut Ui, key: &str, palette: &Palette) {
        let messages = self.thread(key);
        let Some(last) = messages.last().cloned() else {
            self.view = View::Empty;
            return;
        };
        let address = last.address.clone();
        let name = {
            let store = self.store();
            let reported = messages.iter().rev().find_map(|m| m.name.clone());
            store.display_name(&address, reported.as_deref())
        };

        header(ui, palette, |ui| {
            ui.label(RichText::new("To:").color(palette.secondary));
            ui.label(RichText::new(&name).font(bold_font(14.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if conversation_key(&name) != conversation_key(&address) {
                    ui.label(
                        RichText::new(format_address(&address))
                            .color(palette.secondary)
                            .size(12.5),
                    );
                }
            });
        });

        let draft_key = key.to_string();
        let mut draft = self.drafts.remove(&draft_key).unwrap_or_default();
        let composed = self.composer(ui, &mut draft, palette);
        if let Composed::Send(text) = composed {
            self.send(&address, &text);
        }
        if !draft.is_empty() {
            self.drafts.insert(draft_key, draft);
        }

        let mut retry = None;
        let scroll = std::mem::take(&mut self.scroll_to_bottom);
        let photos = &mut self.photos;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(palette.window))
            .show(ui, |ui| {
                let area = egui::ScrollArea::vertical()
                    .id_salt(("thread", key))
                    .auto_shrink([false, false])
                    .stick_to_bottom(true);
                area.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(12.0);
                    let last_outgoing = messages
                        .iter()
                        .rposition(|m| m.direction == Direction::Outgoing);

                    for (i, message) in messages.iter().enumerate() {
                        let previous = i.checked_sub(1).map(|i| &messages[i]);
                        let next = messages.get(i + 1);
                        let new_group = previous.is_none_or(|p| message.time - p.time > GAP);
                        if new_group {
                            ui.add_space(if i == 0 { 4.0 } else { 14.0 });
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new(long_date(message.time))
                                        .size(11.5)
                                        .color(palette.secondary),
                                );
                            });
                            ui.add_space(6.0);
                        } else if previous.is_some_and(|p| p.direction != message.direction) {
                            ui.add_space(8.0);
                        } else {
                            ui.add_space(2.0);
                        }

                        let tail = next.is_none_or(|n| {
                            n.direction != message.direction || n.time - message.time > GAP
                        });
                        for (n, attachment) in message.attachments.iter().enumerate() {
                            if n > 0 {
                                ui.add_space(2.0);
                            }
                            super::photos::attachment(ui, photos, message, attachment, palette);
                        }
                        if !message.text.is_empty() || message.attachments.is_empty() {
                            if !message.attachments.is_empty() {
                                ui.add_space(2.0);
                            }
                            let response = bubble(ui, message, tail, palette);
                            response.context_menu(|ui| {
                                if ui.button("Copy").clicked() {
                                    ui.ctx().copy_text(message.text.clone());
                                    ui.close();
                                }
                            });
                        }

                        if let Some(clicked) =
                            caption(ui, message, Some(i) == last_outgoing, palette)
                        {
                            if clicked {
                                retry = Some(message.clone());
                            }
                        }
                    }
                    ui.add_space(14.0);
                    if scroll {
                        ui.scroll_to_cursor(Some(Align::BOTTOM));
                    }
                });
            });

        if let Some(message) = retry {
            self.retry(&message);
        }
    }

    pub(super) fn compose_view(&mut self, ui: &mut Ui, palette: &Palette) {
        let mut chosen: Option<(String, String)> = None;

        header(ui, palette, |ui| {
            ui.label(RichText::new("To:").color(palette.secondary));
            match self.compose.recipient.clone() {
                Some((name, _)) => {
                    let token = egui::Button::new(
                        RichText::new(format!("{}  ✕", name)).color(palette.accent),
                    )
                    .fill(palette.accent.gamma_multiply(0.12))
                    .corner_radius(10);
                    if ui.add(token).on_hover_text("Change recipient").clicked() {
                        self.compose.recipient = None;
                        self.compose.focus = true;
                    }
                }
                None => {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.compose.query)
                            .id(Id::unique("recipient"))
                            .hint_text("Name, phone number or email")
                            .frame(Frame::NONE)
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut self.compose.focus) {
                        field.request_focus();
                    }
                    let entered =
                        field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                    if entered {
                        chosen = self.typed_recipient();
                    }
                }
            }
        });

        if self.compose.recipient.is_none() && chosen.is_none() {
            chosen = self.contact_picker(ui, palette);
        }

        if let Some((name, address)) = chosen {
            let key = conversation_key(&address);
            let exists = self
                .store()
                .messages
                .iter()
                .any(|m| m.conversation() == key);
            if exists {
                self.open(key);
                return;
            }
            self.compose.recipient = Some((name, address));
        }

        let Some((_, address)) = self.compose.recipient.clone() else {
            return;
        };
        let mut draft = self.drafts.remove("").unwrap_or_default();
        if let Composed::Send(text) = self.composer(ui, &mut draft, palette) {
            self.send(&address, &text);
            self.compose = Default::default();
            self.open(conversation_key(&address));
            return;
        }
        if !draft.is_empty() {
            self.drafts.insert(String::new(), draft);
        }
    }

    /// The recipient typed in the To field, if it is a number or an address.
    fn typed_recipient(&self) -> Option<(String, String)> {
        let query = self.compose.query.trim();
        let digits = query.chars().filter(char::is_ascii_digit).count();
        let address =
            query.contains('@') || (digits >= 3 && !query.chars().any(char::is_alphabetic));
        if address {
            let name = self.store().display_name(query, None);
            return Some((name, query.to_string()));
        }
        // A name: take the first match.
        let store = self.store();
        let contact = store
            .contacts
            .iter()
            .find(|contact| contact.matches(query))?;
        let address = contact.addresses().next()?.to_string();
        Some((contact.name.clone(), address))
    }

    /// Lists the contacts matching the To field; returns the one picked.
    fn contact_picker(&mut self, ui: &mut Ui, palette: &Palette) -> Option<(String, String)> {
        let query = self.compose.query.clone();
        let entries: Vec<(String, String, String)> = {
            let store = self.store();
            store
                .contacts
                .iter()
                .filter(|contact| contact.matches(&query))
                .flat_map(|contact| {
                    let phones = contact
                        .phones
                        .iter()
                        .map(|phone| (phone.label.clone(), phone.number.clone()));
                    let emails = contact
                        .emails
                        .iter()
                        .map(|email| ("email".to_string(), email.clone()));
                    phones
                        .chain(emails)
                        .map(|(label, address)| (contact.name.clone(), label, address))
                        .collect::<Vec<_>>()
                })
                .take(200)
                .collect()
        };

        let mut picked = None;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(palette.window).inner_margin(Margin::symmetric(8, 6)))
            .show(ui, |ui| {
                if entries.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let text = if self.store().contacts.is_empty() {
                            "Type a phone number or email address.\nContacts appear here once your phone shares them."
                        } else {
                            "No matching contacts. Type a phone number or email address."
                        };
                        ui.label(RichText::new(text).color(palette.secondary));
                    });
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (name, label, address) in &entries {
                        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                        if response.hovered() {
                            ui.painter().rect_filled(rect, 8.0, palette.field.gamma_multiply(0.6));
                        }
                        super::widgets::paint_avatar(ui, pos2(rect.left() + 24.0, rect.center().y), 16.0, name, palette);
                        let painter = ui.painter();
                        painter.text(
                            pos2(rect.left() + 50.0, rect.top() + 7.0),
                            egui::Align2::LEFT_TOP,
                            name,
                            bold_font(13.5),
                            palette.text,
                        );
                        painter.text(
                            pos2(rect.left() + 50.0, rect.top() + 25.0),
                            egui::Align2::LEFT_TOP,
                            format!("{}  {}", label, format_address(address)),
                            FontId::proportional(12.0),
                            palette.secondary,
                        );
                        if response.clicked() {
                            picked = Some((name.clone(), address.clone()));
                        }
                    }
                });
            });
        picked
    }

    /// The text field and send button at the bottom.
    fn composer(&mut self, ui: &mut Ui, draft: &mut String, palette: &Palette) -> Composed {
        let connected = self.connected();
        let mut composed = Composed::Nothing;

        egui::Panel::bottom("composer")
            .frame(
                Frame::new()
                    .fill(palette.window)
                    .inner_margin(Margin::symmetric(14, 10)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let width = ui.available_width() - 36.0;
                    let field = Frame::new()
                        .stroke(Stroke::new(1.0, palette.separator))
                        .corner_radius(16)
                        .inner_margin(Margin::symmetric(12, 5))
                        .show(ui, |ui| {
                            ui.set_width(width - 26.0);
                            let hint = if connected {
                                "Text Message"
                            } else {
                                "Connect your iPhone to send messages"
                            };
                            ui.add(
                                egui::TextEdit::multiline(draft)
                                    .id(Id::unique("composer"))
                                    .hint_text(hint)
                                    .frame(Frame::NONE)
                                    .desired_rows(1)
                                    .desired_width(f32::INFINITY)
                                    .font(FontId::proportional(14.0))
                                    .return_key(KeyboardShortcut::new(
                                        Modifiers::SHIFT,
                                        Key::Enter,
                                    )),
                            )
                        })
                        .inner;

                    let ready = connected && !draft.trim().is_empty();
                    let enter = field.has_focus()
                        && ui.input(|input| {
                            input.key_pressed(Key::Enter) && input.modifiers.is_none()
                        });
                    let clicked = send_button(ui, ready, palette).clicked();
                    if ready && (enter || clicked) {
                        composed = Composed::Send(std::mem::take(draft));
                        field.request_focus();
                    }
                });
            });
        composed
    }
}

/// The bar at the top of the conversation.
fn header(ui: &mut Ui, palette: &Palette, contents: impl FnOnce(&mut Ui)) {
    let top = if cfg!(target_os = "macos") { 18 } else { 0 };
    egui::Panel::top("header")
        .frame(Frame::new().fill(palette.window).inner_margin(Margin {
            left: 16,
            right: 16,
            top: 12 + top,
            bottom: 12,
        }))
        .show(ui, |ui| {
            ui.horizontal(contents);
        });
}

/// Draws a message bubble; `tail` marks the last bubble of a group.
fn bubble(ui: &mut Ui, message: &Message, tail: bool, palette: &Palette) -> egui::Response {
    let outgoing = message.direction == Direction::Outgoing;
    let failed = matches!(message.status, Status::Failed(_));
    let (fill, color) = if outgoing {
        (palette.sent, palette.sent_text)
    } else {
        (palette.received, palette.received_text)
    };
    let fill = if message.status == Status::Sending {
        fill.gamma_multiply(0.7)
    } else {
        fill
    };

    let padding = vec2(12.0, 7.0);
    let margin = 16.0;
    let failed_icon = if failed { 26.0 } else { 0.0 };
    let max_width = (ui.available_width() * 0.68).clamp(120.0, 520.0);
    let galley = ui.painter().layout(
        message.text.clone(),
        FontId::proportional(14.5),
        color,
        max_width - 2.0 * padding.x,
    );
    let size = galley.size() + 2.0 * padding;
    let size = vec2(size.x.max(34.0), size.y);

    let (row, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), size.y), Sense::click());
    let x = if outgoing {
        row.right() - margin - failed_icon - size.x
    } else {
        row.left() + margin
    };
    let rect = Rect::from_min_size(pos2(x, row.top()), size);

    let radius = 17;
    let corners = match (outgoing, tail) {
        (true, true) => CornerRadius {
            nw: radius,
            ne: radius,
            sw: radius,
            se: 5,
        },
        (false, true) => CornerRadius {
            nw: radius,
            ne: radius,
            sw: 5,
            se: radius,
        },
        _ => CornerRadius::same(radius),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, corners, fill);
    painter.galley(rect.min + padding, galley, color);

    if failed {
        let center = pos2(rect.right() + 14.0, rect.center().y);
        painter.circle_filled(center, 9.0, palette.failed);
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            "!",
            bold_font(13.0),
            egui::Color32::WHITE,
        );
    }
    response
}

/// The status under a sent message: delivery for the latest, failures for any. Returns
/// whether "Try Again" was clicked, if it is shown.
fn caption(ui: &mut Ui, message: &Message, latest: bool, palette: &Palette) -> Option<bool> {
    let (text, color) = match &message.status {
        Status::Failed(reason) => (format!("Not Delivered · {}", reason), palette.failed),
        _ if !latest => return None,
        Status::Sending => ("Sending…".to_string(), palette.secondary),
        Status::Sent => ("Sent".to_string(), palette.secondary),
        Status::Delivered => ("Delivered".to_string(), palette.secondary),
        Status::Received => return None,
    };

    let mut clicked = false;
    ui.add_space(3.0);
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        ui.add_space(if matches!(message.status, Status::Failed(_)) {
            42.0
        } else {
            18.0
        });
        if matches!(message.status, Status::Failed(_)) {
            clicked = ui
                .add(
                    egui::Label::new(RichText::new("Try Again").size(11.5).color(palette.accent))
                        .sense(Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked();
        }
        ui.add(egui::Label::new(RichText::new(text).size(11.5).color(color)).truncate());
    });
    Some(clicked)
}
