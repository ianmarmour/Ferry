//! The connection screen, shown until a phone is connected.

use egui::{vec2, Align, Frame, Layout, Margin, RichText, Sense, Stroke, Ui};

use super::theme::{bold_font, Palette};
use super::widgets::{icon_button, paint_phone, paint_refresh};
use super::TidingsApp;
use crate::session::{Command, State};

const STEPS: [&str; 3] = [
    "Pair your iPhone with this computer in your system's Bluetooth settings.",
    "On the iPhone, open Settings › Bluetooth, tap ⓘ next to this computer and turn on Show Notifications and Sync Contacts.",
    "Choose your iPhone below. Tidings connects like a car does, so nothing needs to be installed on the phone.",
];

impl TidingsApp {
    pub(super) fn welcome(&mut self, ui: &mut Ui, palette: &Palette) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.set_max_width(460.0);
                    ui.add_space((ui.available_height() * 0.12).max(40.0));
                    ui.label(RichText::new("Tidings").font(bold_font(28.0)));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Your iPhone's messages, on your computer.")
                            .color(palette.secondary),
                    );
                    ui.add_space(22.0);

                    if let State::Retrying { device, error } = &self.state {
                        Frame::new()
                            .fill(palette.banner)
                            .corner_radius(10)
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(
                                    RichText::new(format!("Can't reach {}", device.name))
                                        .font(bold_font(13.5)),
                                );
                                ui.label(RichText::new(error).size(12.5));
                                ui.label(
                                    RichText::new("Tidings keeps trying in the background.")
                                        .size(12.5)
                                        .color(palette.secondary),
                                );
                            });
                        ui.add_space(14.0);
                    }

                    Frame::new()
                        .fill(palette.sidebar)
                        .corner_radius(12)
                        .inner_margin(Margin::same(16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                for (i, step) in STEPS.iter().enumerate() {
                                    ui.horizontal_top(|ui| {
                                        ui.label(
                                            RichText::new(format!("{}.", i + 1))
                                                .color(palette.secondary),
                                        );
                                        ui.add(
                                            egui::Label::new(RichText::new(*step).size(13.0))
                                                .wrap(),
                                        );
                                    });
                                }
                            });
                        });

                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Paired Devices").font(bold_font(13.5)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, 22.0, "Refresh", paint_refresh).clicked() {
                                self.session.send(Command::RefreshDevices);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    self.device_list(ui, palette);
                });
            });
    }

    fn device_list(&mut self, ui: &mut Ui, palette: &Palette) {
        let devices = match &self.devices {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Looking for paired devices…").color(palette.secondary));
                });
                return;
            }
            Some(Err(error)) => {
                ui.label(
                    RichText::new(format!("Bluetooth is unavailable: {}", error))
                        .color(palette.failed),
                );
                return;
            }
            Some(Ok(devices)) if devices.is_empty() => {
                ui.label(
                    RichText::new("No paired devices. Pair your iPhone first.")
                        .color(palette.secondary),
                );
                return;
            }
            Some(Ok(devices)) => {
                let mut devices = devices.clone();
                devices.sort_by_key(|device| !device.phone);
                devices
            }
        };

        let busy = self.state.device().cloned();
        let mut connect = None;
        Frame::new()
            .stroke(Stroke::new(1.0, palette.separator))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (i, device) in devices.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(28.0, 34.0), Sense::hover());
                        let color = if device.phone {
                            palette.accent
                        } else {
                            palette.secondary
                        };
                        paint_phone(ui, rect.shrink(6.0), color);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&device.name).font(bold_font(13.5)));
                            ui.label(
                                RichText::new(&device.address)
                                    .size(11.5)
                                    .color(palette.secondary),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let current = busy.as_ref() == Some(device);
                            if current && matches!(self.state, State::Connecting(_)) {
                                ui.spinner();
                            } else if ui
                                .button(if current { "Retry" } else { "Connect" })
                                .clicked()
                            {
                                connect = Some(device.clone());
                            }
                        });
                    });
                }
            });

        if let Some(device) = connect {
            self.connect(device);
        }
    }
}
