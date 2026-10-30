//! Colors, fonts and visuals in the style of Messages on the Mac.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Theme, Visuals};

/// The colors of one appearance.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub window: Color32,
    pub sidebar: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub separator: Color32,
    pub field: Color32,
    pub accent: Color32,
    pub selection_text: Color32,
    pub sent: Color32,
    pub sent_text: Color32,
    pub received: Color32,
    pub received_text: Color32,
    pub failed: Color32,
    pub online: Color32,
    pub warning: Color32,
    pub avatar_top: Color32,
    pub avatar_bottom: Color32,
    pub banner: Color32,
}

/// The palette of light mode.
pub const LIGHT: Palette = Palette {
    window: Color32::WHITE,
    sidebar: Color32::from_rgb(0xF2, 0xF1, 0xF6),
    text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
    secondary: Color32::from_rgb(0x86, 0x86, 0x8B),
    separator: Color32::from_rgb(0xDE, 0xDE, 0xE3),
    field: Color32::from_rgb(0xE3, 0xE3, 0xE8),
    accent: Color32::from_rgb(0x0A, 0x84, 0xFF),
    selection_text: Color32::WHITE,
    sent: Color32::from_rgb(0x0B, 0x84, 0xFE),
    sent_text: Color32::WHITE,
    received: Color32::from_rgb(0xE9, 0xE9, 0xEB),
    received_text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
    failed: Color32::from_rgb(0xFF, 0x3B, 0x30),
    online: Color32::from_rgb(0x34, 0xC7, 0x59),
    warning: Color32::from_rgb(0xFF, 0x9F, 0x0A),
    avatar_top: Color32::from_rgb(0xA5, 0xAB, 0xB8),
    avatar_bottom: Color32::from_rgb(0x85, 0x89, 0x94),
    banner: Color32::from_rgb(0xFF, 0xF4, 0xD6),
};

/// The palette of dark mode.
pub const DARK: Palette = Palette {
    window: Color32::from_rgb(0x1E, 0x1E, 0x1E),
    sidebar: Color32::from_rgb(0x28, 0x28, 0x2A),
    text: Color32::from_rgb(0xF5, 0xF5, 0xF7),
    secondary: Color32::from_rgb(0x98, 0x98, 0x9D),
    separator: Color32::from_rgb(0x3A, 0x3A, 0x3C),
    field: Color32::from_rgb(0x3A, 0x3A, 0x3C),
    accent: Color32::from_rgb(0x0A, 0x84, 0xFF),
    selection_text: Color32::WHITE,
    sent: Color32::from_rgb(0x0B, 0x84, 0xFE),
    sent_text: Color32::WHITE,
    received: Color32::from_rgb(0x3B, 0x3B, 0x3D),
    received_text: Color32::from_rgb(0xF5, 0xF5, 0xF7),
    failed: Color32::from_rgb(0xFF, 0x45, 0x3A),
    online: Color32::from_rgb(0x30, 0xD1, 0x58),
    warning: Color32::from_rgb(0xFF, 0x9F, 0x0A),
    avatar_top: Color32::from_rgb(0x8E, 0x93, 0x9E),
    avatar_bottom: Color32::from_rgb(0x6C, 0x70, 0x7A),
    banner: Color32::from_rgb(0x4A, 0x3F, 0x22),
};

impl Palette {
    /// The palette of the current appearance.
    pub fn of(ctx: &egui::Context) -> Palette {
        match ctx.theme() {
            Theme::Dark => DARK,
            Theme::Light => LIGHT,
        }
    }
}

/// The family of the semibold font, for names and headings.
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// A font of the bold family.
pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, bold())
}

/// System fonts close to the platform's UI font, regular then semibold.
fn system_fonts() -> (&'static [&'static str], &'static [&'static str]) {
    if cfg!(target_os = "macos") {
        (
            &[
                "/System/Library/Fonts/SFNS.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
            ],
            &["/System/Library/Fonts/Supplemental/Arial Bold.ttf"],
        )
    } else if cfg!(windows) {
        (
            &["C:\\Windows\\Fonts\\segoeui.ttf"],
            &["C:\\Windows\\Fonts\\seguisb.ttf", "C:\\Windows\\Fonts\\segoeuib.ttf"],
        )
    } else {
        (
            &[
                "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
                "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
                "/usr/share/fonts/inter/Inter-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            ],
            &[
                "/usr/share/fonts/opentype/inter/Inter-SemiBold.otf",
                "/usr/share/fonts/truetype/inter/Inter-SemiBold.ttf",
                "/usr/share/fonts/inter/Inter-SemiBold.otf",
                "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
                "/usr/share/fonts/google-noto/NotoSans-SemiBold.ttf",
                "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
                "/usr/share/fonts/truetype/ubuntu/Ubuntu-M.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
            ],
        )
    }
}

fn load_first(candidates: &[&str]) -> Option<FontData> {
    candidates
        .iter()
        .find_map(|path| std::fs::read(path).ok())
        .map(FontData::from_owned)
}

/// Uses the platform's UI font when it can be found, keeping egui's fonts as
/// fallbacks for symbols and emoji.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let (regular, semibold) = system_fonts();
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let regular = load_first(regular);
    let has_regular = regular.is_some();
    if let Some(data) = regular {
        fonts.font_data.insert("system".to_string(), Arc::new(data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "system".to_string());
    }

    let mut bold_family = Vec::new();
    if let Some(data) = load_first(semibold) {
        fonts.font_data.insert("system-bold".to_string(), Arc::new(data));
        bold_family.push("system-bold".to_string());
    } else if has_regular {
        bold_family.push("system".to_string());
    }
    bold_family.extend(fallbacks);
    fonts.families.insert(bold(), bold_family);

    ctx.set_fonts(fonts);
}

fn visuals(theme: Theme, palette: &Palette) -> Visuals {
    let mut visuals = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    visuals.panel_fill = palette.window;
    visuals.window_fill = palette.window;
    visuals.extreme_bg_color = palette.field;
    visuals.override_text_color = Some(palette.text);
    visuals.hyperlink_color = palette.accent;
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.separator);
    visuals.window_corner_radius = 10.into();
    visuals.menu_corner_radius = 8.into();
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = 6.into();
    }
    visuals
}

/// Applies the styles of both appearances; egui follows the system's.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Light, visuals(Theme::Light, &LIGHT));
    ctx.set_visuals_of(Theme::Dark, visuals(Theme::Dark, &DARK));

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Heading, FontId::new(20.0, bold())),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
    });
}
