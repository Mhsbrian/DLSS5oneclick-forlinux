//! The mark: a rounded square holding a stroke-built "5" whose top bar ends in a
//! forward chevron (one click, forward). Painted as vectors so it scales crisply.

use eframe::egui::{self, epaint, Color32, CornerRadius, Pos2, Rect, Stroke};

/// Paint the mark into `rect` (square). `chevron` off below ~24 px.
pub fn paint_mark(painter: &egui::Painter, rect: Rect, square: Color32, glyph: Color32) {
    let u = rect.width() / 24.0;
    let p = |x: f32, y: f32| Pos2::new(rect.left() + x * u, rect.top() + y * u);
    let chevron = rect.width() >= 24.0;
    let w = if chevron { 2.4 * u } else { 3.0 * u };
    let stroke = Stroke::new(w, glyph);

    let inner = Rect::from_min_max(p(1.0, 1.0), p(23.0, 23.0));
    painter.rect_filled(
        inner,
        CornerRadius::same((6.0 * u).round().clamp(1.0, 255.0) as u8),
        square,
    );

    // top bar, spine, shelf
    painter.line_segment([p(8.0, 6.5), p(16.0, 6.5)], stroke);
    painter.line_segment([p(8.0, 6.5), p(8.0, 11.5)], stroke);
    painter.line_segment([p(8.0, 11.5), p(13.2, 11.5)], stroke);
    // round caps
    for c in [p(8.0, 6.5), p(16.0, 6.5), p(8.0, 11.5), p(8.5, 18.5)] {
        painter.circle_filled(c, w / 2.0, glyph);
    }
    // bowl: half circle centred (13.2, 15) r 3.5, from top to bottom on the right
    let (cx, cy, r) = (13.2, 15.0, 3.5);
    let pts: Vec<Pos2> = (0..=24)
        .map(|i| {
            let t = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i as f32 / 24.0);
            p(cx + r * t.cos(), cy + r * t.sin())
        })
        .collect();
    painter.add(epaint::PathShape::line(pts, stroke));
    painter.line_segment([p(13.2, 18.5), p(8.5, 18.5)], stroke);

    if chevron {
        let cs = Stroke::new(1.8 * u, glyph);
        painter.add(epaint::PathShape::line(
            vec![p(15.0, 4.5), p(17.5, 6.5), p(15.0, 8.5)],
            cs,
        ));
        painter.circle_filled(p(17.5, 6.5), 0.9 * u, glyph);
    }
}

/// Window/taskbar icon decoded from the bundled PNG.
pub fn icon_data() -> Option<egui::IconData> {
    let bytes = include_bytes!("../assets/icon-64.png");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    })
}
