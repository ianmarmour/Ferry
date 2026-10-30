//! Small drawing helpers: avatars, icons and dates.

use chrono::{DateTime, Datelike, Local, TimeZone};
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{pos2, vec2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};

use super::theme::{bold_font, Palette};
use crate::contacts::initials;

/// Interpolates between two colors.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(channel(a.r(), b.r()), channel(a.g(), b.g()), channel(a.b(), b.b()))
}

/// A circle filled with a vertical gradient.
fn gradient_circle(center: Pos2, radius: f32, top: Color32, bottom: Color32) -> Shape {
    let mut mesh = Mesh::default();
    let segments = 48;
    let color_at = |y: f32| mix(top, bottom, ((y - (center.y - radius)) / (2.0 * radius)).clamp(0.0, 1.0));

    mesh.vertices.push(Vertex {
        pos: center,
        uv: WHITE_UV,
        color: color_at(center.y),
    });
    for i in 0..=segments {
        let angle = i as f32 / segments as f32 * std::f32::consts::TAU;
        let pos = center + radius * vec2(angle.cos(), angle.sin());
        mesh.vertices.push(Vertex {
            pos,
            uv: WHITE_UV,
            color: color_at(pos.y),
        });
    }
    for i in 1..=segments as u32 {
        mesh.indices.extend_from_slice(&[0, i, i + 1]);
    }
    Shape::mesh(mesh)
}

/// Paints a contact avatar: initials, or a silhouette for unknown numbers.
pub fn paint_avatar(ui: &Ui, center: Pos2, radius: f32, name: &str, palette: &Palette) {
    let painter = ui.painter();
    painter.add(gradient_circle(center, radius, palette.avatar_top, palette.avatar_bottom));

    match initials(name) {
        Some(initials) => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                initials,
                bold_font(radius * 0.8),
                Color32::WHITE,
            );
        }
        None => {
            let head = center + vec2(0.0, -radius * 0.2);
            painter.circle_filled(head, radius * 0.3, Color32::WHITE);
            let body_center = center + vec2(0.0, radius * 0.72);
            let points: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let angle = std::f32::consts::PI * (1.0 + i as f32 / 24.0);
                    body_center + vec2(radius * 0.55 * angle.cos(), radius * 0.45 * angle.sin())
                })
                .collect();
            painter.add(Shape::convex_polygon(points, Color32::WHITE, Stroke::NONE));
        }
    }
}

/// A borderless icon button; `paint` draws the icon in the given rect and color.
pub fn icon_button(ui: &mut Ui, size: f32, tooltip: &str, paint: impl FnOnce(&Ui, Rect, Color32)) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let palette = Palette::of(ui.ctx());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, palette.field);
    }
    let color = if ui.is_enabled() { palette.secondary } else { palette.separator };
    paint(ui, rect.shrink(size * 0.22), color);
    response.on_hover_text(tooltip)
}

/// The compose icon: a square with a pencil.
pub fn paint_compose(ui: &Ui, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    let painter = ui.painter();
    let square = Rect::from_min_max(rect.min + vec2(0.0, rect.height() * 0.15), rect.max - vec2(rect.width() * 0.15, 0.0));
    painter.line_segment([square.left_top(), pos2(square.center().x, square.top())], stroke);
    painter.line_segment([square.left_top(), square.left_bottom()], stroke);
    painter.line_segment([square.left_bottom(), square.right_bottom()], stroke);
    painter.line_segment([square.right_bottom(), pos2(square.right(), square.center().y)], stroke);
    painter.line_segment([pos2(rect.center().x - 1.0, rect.center().y + 2.0), rect.right_top()], Stroke::new(2.0, color));
}

/// A magnifying glass.
pub fn paint_search(ui: &Ui, rect: Rect, color: Color32) {
    let radius = rect.width() * 0.32;
    let center = rect.left_top() + vec2(radius + 1.0, radius + 1.0);
    ui.painter().circle_stroke(center, radius, Stroke::new(1.4, color));
    let handle = center + vec2(radius, radius) * std::f32::consts::FRAC_1_SQRT_2;
    ui.painter().line_segment([handle, rect.right_bottom()], Stroke::new(1.6, color));
}

/// Circular arrows, for refreshing.
pub fn paint_refresh(ui: &Ui, rect: Rect, color: Color32) {
    let center = rect.center();
    let radius = rect.width() * 0.42;
    let points: Vec<Pos2> = (0..=20)
        .map(|i| {
            let angle = -1.2 + i as f32 / 20.0 * 5.0;
            center + radius * vec2(angle.cos(), angle.sin())
        })
        .collect();
    let tip = *points.last().expect("not empty");
    ui.painter().add(Shape::line(points, Stroke::new(1.5, color)));
    ui.painter().line_segment([tip, tip + vec2(radius * 0.55, -radius * 0.1)], Stroke::new(1.5, color));
    ui.painter().line_segment([tip, tip + vec2(0.1 * radius, -radius * 0.55)], Stroke::new(1.5, color));
}

/// An iPhone outline.
pub fn paint_phone(ui: &Ui, rect: Rect, color: Color32) {
    let body = Rect::from_center_size(rect.center(), vec2(rect.height() * 0.55, rect.height()));
    ui.painter().rect_stroke(body, 3.0, Stroke::new(1.5, color), egui::StrokeKind::Inside);
    ui.painter().line_segment(
        [pos2(body.center().x - 2.0, body.top() + 3.0), pos2(body.center().x + 2.0, body.top() + 3.0)],
        Stroke::new(1.5, color),
    );
}

/// The send button: a filled circle with an up arrow.
pub fn send_button(ui: &mut Ui, enabled: bool, palette: &Palette) -> Response {
    let size = 26.0;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), sense);
    let fill = if enabled { palette.accent } else { palette.separator };
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size / 2.0, fill);

    let stroke = Stroke::new(2.2, Color32::WHITE);
    let top = rect.center() + vec2(0.0, -6.0);
    painter.line_segment([top, rect.center() + vec2(0.0, 6.5)], stroke);
    painter.line_segment([top, top + vec2(-5.0, 5.0)], stroke);
    painter.line_segment([top, top + vec2(5.0, 5.0)], stroke);
    response.on_hover_text("Send")
}

/// A rounded search field.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, palette: &Palette) -> Response {
    egui::Frame::new()
        .fill(palette.field)
        .corner_radius(7)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                paint_search(ui, rect, palette.secondary);
                ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(hint)
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                )
            })
            .inner
        })
        .inner
}

fn local(time: i64) -> DateTime<Local> {
    Local
        .timestamp_opt(time, 0)
        .single()
        .unwrap_or_else(Local::now)
}

fn clock(time: &DateTime<Local>) -> String {
    time.format("%-I:%M %p").to_string()
}

/// The date of a conversation in the sidebar: a time today, then the day, then the date.
pub fn short_date(time: i64) -> String {
    let time = local(time);
    let today = Local::now().date_naive();
    let days = (today - time.date_naive()).num_days();
    match days {
        0 => clock(&time),
        1 => "Yesterday".to_string(),
        2..=6 => time.format("%A").to_string(),
        _ => format!("{}/{}/{:02}", time.month(), time.day(), time.year() % 100),
    }
}

/// The date shown above a group of messages.
pub fn long_date(time: i64) -> String {
    let time = local(time);
    let today = Local::now().date_naive();
    let days = (today - time.date_naive()).num_days();
    match days {
        0 => format!("Today {}", clock(&time)),
        1 => format!("Yesterday {}", clock(&time)),
        2..=6 => format!("{} {}", time.format("%A"), clock(&time)),
        _ => format!("{} at {}", time.format("%b %-d, %Y"), clock(&time)),
    }
}
