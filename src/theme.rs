//! Palette, fonts and widget style for the "instrument panel" look.
//!
//! Fonts are bundled (all SIL OFL, licences in assets/fonts):
//! Sora Bold for the title, IBM Plex Sans for UI text, JetBrains Mono for paths and the log.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
pub const PANEL: Color32 = Color32::from_rgb(0x15, 0x18, 0x1e);
pub const HEADER: Color32 = Color32::from_rgb(0x12, 0x15, 0x1a);
pub const TILE: Color32 = Color32::from_rgb(0x1a, 0x1e, 0x25);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2a, 0x33);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x39);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe9, 0xec);
pub const TEXT_SOFT: Color32 = Color32::from_rgb(0xc9, 0xcd, 0xd4);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x7d, 0x85, 0x92);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x5c, 0x64, 0x72);
pub const TEXT_OFF: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xad);
pub const RING_OFF: Color32 = Color32::from_rgb(0x4a, 0x51, 0x60);
pub const ACCENT: Color32 = Color32::from_rgb(0xc4, 0xc9, 0xd2);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
/// Something works but has fallen behind: an update, not a fault.
pub const WARN: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x5c);

pub const SORA: &str = "sora";
pub const PLEX_MEDIUM: &str = "plex-medium";
pub const PLEX_SEMIBOLD: &str = "plex-semibold";

pub fn sora(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SORA.into()))
}
pub fn plex(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn plex_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(PLEX_MEDIUM.into()))
}
pub fn plex_semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(PLEX_SEMIBOLD.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, key: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(key.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        &mut fonts,
        "plex",
        include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    );
    add(
        &mut fonts,
        PLEX_MEDIUM,
        include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
    );
    add(
        &mut fonts,
        PLEX_SEMIBOLD,
        include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    );
    add(
        &mut fonts,
        SORA,
        include_bytes!("../assets/fonts/Sora-Bold.ttf"),
    );
    add(
        &mut fonts,
        "jbmono",
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "plex".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "jbmono".to_owned());
    for name in [SORA, PLEX_MEDIUM, PLEX_SEMIBOLD] {
        // Fall back to the regular proportional chain for glyphs the face lacks.
        let mut chain = vec![name.to_owned()];
        chain.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), chain);
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, sora(15.0)),
            (TextStyle::Body, plex(13.0)),
            (TextStyle::Button, plex_medium(13.0)),
            (TextStyle::Small, plex(11.0)),
            (TextStyle::Monospace, mono(11.5)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.interact_size = egui::vec2(40.0, 28.0);

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = BG;
        v.faint_bg_color = TILE;
        v.code_bg_color = TILE;
        v.hyperlink_color = ACCENT;
        v.error_fg_color = DANGER;
        v.selection.bg_fill = Color32::from_rgb(0x2f, 0x35, 0x42);
        v.selection.stroke = Stroke::new(1.0, ACCENT);

        let w = &mut v.widgets;
        for wv in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            wv.corner_radius = CornerRadius::same(8);
            wv.expansion = 0.0;
        }
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.bg_fill = TILE;
        w.inactive.weak_bg_fill = TILE;
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT_SOFT);
        w.hovered.bg_fill = Color32::from_rgb(0x21, 0x26, 0x2e);
        w.hovered.weak_bg_fill = Color32::from_rgb(0x21, 0x26, 0x2e);
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3a, 0x41, 0x4d));
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        w.active.bg_fill = Color32::from_rgb(0x2a, 0x30, 0x3a);
        w.active.weak_bg_fill = Color32::from_rgb(0x2a, 0x30, 0x3a);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0, TEXT);
        w.open.bg_fill = TILE;
        w.open.weak_bg_fill = TILE;
        w.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        w.open.fg_stroke = Stroke::new(1.0, TEXT);
    });
}
