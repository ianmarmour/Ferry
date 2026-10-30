//! Photos and other attachments in conversations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Rect, Response, Sense, Stroke, Ui};

use super::theme::{bold_font, Palette};
use crate::model::{Attachment, AttachmentKind, Direction, Message};

/// Decoded photos, by file, decoded the first time they are shown.
#[derive(Default)]
pub(super) struct Photos {
    textures: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

impl Photos {
    fn texture(&mut self, ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        self.textures
            .entry(path.to_path_buf())
            .or_insert_with(|| load(ctx, path))
            .clone()
    }
}

/// Decodes a photo, scaled down to at most 640 pixels on a side.
fn load(ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
    let bytes = std::fs::read(path).ok()?;
    let image = match image::load_from_memory(&bytes) {
        Ok(image) => image.thumbnail(640, 640).to_rgba8(),
        Err(error) => {
            log::info!("cannot show {}: {}", path.display(), error);
            return None;
        }
    };
    let size = [image.width() as usize, image.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    Some(ctx.load_texture(
        path.display().to_string(),
        color,
        egui::TextureOptions::LINEAR,
    ))
}

/// Opens a file with the system's default application.
pub(super) fn open(path: &Path) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(error) = std::process::Command::new(program).arg(path).spawn() {
        log::warn!("cannot open {}: {}", path.display(), error);
    }
}

/// The left or right edge of a bubble of `width` in `row`.
fn place(row: Rect, outgoing: bool, width: f32) -> f32 {
    let margin = 16.0;
    if outgoing {
        row.right() - margin - width
    } else {
        row.left() + margin
    }
}

/// Shows an attachment: the photo itself, or a card naming it. Clicking opens it.
pub(super) fn attachment(
    ui: &mut Ui,
    photos: &mut Photos,
    message: &Message,
    attachment: &Attachment,
    palette: &Palette,
) -> Response {
    let outgoing = message.direction == Direction::Outgoing;
    let texture = match (&attachment.kind, &attachment.file) {
        (AttachmentKind::Photo, Some(file)) => photos.texture(ui.ctx(), file),
        _ => None,
    };

    let response = match texture {
        Some(texture) => {
            let size = texture.size_vec2();
            let max = vec2((ui.available_width() * 0.6).clamp(120.0, 300.0), 320.0);
            let shown = size * (max.x / size.x).min(max.y / size.y).min(1.0);
            let (row, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), shown.y), Sense::click());
            let rect = Rect::from_min_size(pos2(place(row, outgoing, shown.x), row.top()), shown);
            egui::Image::new((texture.id(), shown))
                .corner_radius(16)
                .paint_at(ui, rect);
            response
        }
        None => card(ui, outgoing, attachment, palette),
    };

    match &attachment.file {
        Some(file) => {
            if response.clicked() {
                open(file);
            }
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        }
        None => response.on_hover_text(
            "Your phone didn't send this attachment over Bluetooth. Open it on your phone to see it.",
        ),
    }
}

/// A card standing in for an attachment that can't be shown inline.
fn card(ui: &mut Ui, outgoing: bool, attachment: &Attachment, palette: &Palette) -> Response {
    let (fill, color) = if outgoing {
        (palette.sent, palette.sent_text)
    } else {
        (palette.received, palette.received_text)
    };
    let size = vec2(220.0, 56.0);
    let (row, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), size.y), Sense::click());
    let rect = Rect::from_min_size(pos2(place(row, outgoing, size.x), row.top()), size);
    let painter = ui.painter();
    painter.rect_filled(rect, 17, fill);

    // A picture frame with a mountain and the sun.
    let frame = Rect::from_center_size(pos2(rect.left() + 30.0, rect.center().y), vec2(26.0, 20.0));
    let stroke = Stroke::new(1.6, color);
    painter.rect_stroke(frame, 4, stroke, egui::StrokeKind::Inside);
    match attachment.kind {
        AttachmentKind::Photo => {
            painter.circle_filled(frame.left_top() + vec2(18.0, 6.0), 2.4, color);
            painter.add(egui::Shape::line(
                vec![
                    frame.left_bottom() + vec2(3.0, -3.0),
                    frame.left_bottom() + vec2(10.0, -11.0),
                    frame.left_bottom() + vec2(15.0, -6.0),
                    frame.left_bottom() + vec2(18.0, -9.0),
                    frame.right_bottom() + vec2(-3.0, -3.0),
                ],
                stroke,
            ));
        }
        AttachmentKind::Video => {
            let center = frame.center();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + vec2(-4.0, -5.0),
                    center + vec2(5.0, 0.0),
                    center + vec2(-4.0, 5.0),
                ],
                color,
                Stroke::NONE,
            ));
        }
        AttachmentKind::Audio | AttachmentKind::File => {
            for i in 0..3 {
                let y = frame.top() + 6.0 + i as f32 * 4.0;
                painter.line_segment(
                    [pos2(frame.left() + 5.0, y), pos2(frame.right() - 5.0, y)],
                    stroke,
                );
            }
        }
    }

    let label = attachment.kind.label(1);
    let hint = if attachment.file.is_some() {
        "Click to open"
    } else {
        "View on your phone"
    };
    painter.text(
        pos2(rect.left() + 54.0, rect.top() + 11.0),
        egui::Align2::LEFT_TOP,
        label,
        bold_font(14.0),
        color,
    );
    painter.text(
        pos2(rect.left() + 54.0, rect.top() + 30.0),
        egui::Align2::LEFT_TOP,
        hint,
        egui::FontId::proportional(12.0),
        color.gamma_multiply(0.75),
    );
    response
}
