//! The angle wheel: one ring of dots per camera height, outermost first.
//! Filled = photo position done, outlined = next, faint = to do. The needle
//! shows where the turntable actually is.

use egui::{Color32, Pos2, Sense, Shape, Stroke, Ui, Vec2};

pub struct WheelState<'a> {
    pub shots_per_ring: u32,
    pub rings: u32,
    /// Done positions per ring.
    pub done: &'a [u32],
    /// Ring/position the scan is on, if scanning.
    pub current: Option<(u32, u32)>,
    /// Turntable angle in degrees (any range).
    pub table_degrees: Option<f64>,
    pub center_title: String,
    pub center_sub: String,
    /// Photo transfer camera → PC, 0..1 (None = idle).
    pub transfer: Option<f32>,
    /// Speed / result under the center text.
    pub transfer_label: Option<String>,
}

/// Screen point at `deg` (0° = top, clockwise) and `radius` around `c`.
fn polar(c: Pos2, radius: f32, deg: f64) -> Pos2 {
    let a = deg.to_radians() as f32;
    Pos2::new(c.x + radius * a.sin(), c.y - radius * a.cos())
}

pub fn show(ui: &mut Ui, size: f32, w: &WheelState) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter_at(rect);
    let v = ui.visuals();
    let accent = v.selection.bg_fill;
    let faint = v.weak_text_color().gamma_multiply(0.5);
    let text = v.strong_text_color();
    let c = rect.center();
    let outer = size * 0.46;

    // Plate.
    painter.circle_filled(c, outer, v.faint_bg_color);
    painter.circle_stroke(c, outer, Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color));

    // One track per ring, outermost = ring 0.
    let rings = w.rings.max(1);
    let n = w.shots_per_ring.max(1);
    let track_gap = (outer * 0.55) / rings as f32;
    for ring in 0..rings {
        let r = outer * 0.88 - ring as f32 * track_gap;
        let dot = (std::f32::consts::PI * r / n as f32 * 0.55).clamp(1.5, 7.0);
        let done = w.done.get(ring as usize).copied().unwrap_or(0);
        for i in 0..n {
            let p = polar(c, r, 360.0 * i as f64 / n as f64);
            if i < done {
                painter.circle_filled(p, dot, accent);
            } else if w.current == Some((ring, i)) {
                painter.circle_stroke(p, dot + 1.5, Stroke::new(2.0, accent));
            } else {
                painter.circle_filled(p, dot * 0.7, faint);
            }
        }
    }

    // 0° marker.
    painter.line_segment([polar(c, outer, 0.0), polar(c, outer + 8.0, 0.0)], Stroke::new(2.0, text));

    // Needle at the table's actual angle.
    if let Some(deg) = w.table_degrees {
        let tip = polar(c, outer * 0.97, deg);
        let base = polar(c, outer * 0.30, deg);
        painter.line_segment([base, tip], Stroke::new(2.5, Color32::from_rgb(0xF2, 0x8C, 0x28)));
        let left = polar(c, outer * 0.90, deg - 2.5);
        let right = polar(c, outer * 0.90, deg + 2.5);
        painter.add(Shape::convex_polygon(
            vec![tip, left, right],
            Color32::from_rgb(0xF2, 0x8C, 0x28),
            Stroke::NONE,
        ));
    }

    // Center label.
    let hub = outer * 0.28;
    painter.circle_filled(c, hub, v.extreme_bg_color);
    painter.circle_stroke(c, hub, Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color));

    // Transfer ring around the hub: fills clockwise from the top.
    if let Some(p) = w.transfer {
        let r = hub + 7.0;
        let width = (size * 0.012).clamp(3.0, 6.0);
        painter.circle_stroke(c, r, Stroke::new(width, faint));
        let green = Color32::from_rgb(0x3D, 0xC2, 0x6B);
        let color = if p >= 1.0 { green } else { accent };
        let steps = ((96.0 * p).ceil() as usize).max(2);
        let pts: Vec<Pos2> = (0..=steps).map(|i| polar(c, r, 360.0 * p as f64 * i as f64 / steps as f64)).collect();
        if p > 0.0 {
            painter.add(Shape::line(pts, Stroke::new(width, color)));
        }
    }
    painter.text(
        c - Vec2::new(0.0, size * 0.025),
        egui::Align2::CENTER_CENTER,
        &w.center_title,
        egui::FontId::proportional((size * 0.06).clamp(14.0, 34.0)),
        text,
    );
    painter.text(
        c + Vec2::new(0.0, size * 0.04),
        egui::Align2::CENTER_CENTER,
        &w.center_sub,
        egui::FontId::proportional((size * 0.032).clamp(10.0, 16.0)),
        v.weak_text_color(),
    );
    if let Some(label) = &w.transfer_label {
        painter.text(
            c + Vec2::new(0.0, size * 0.078),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional((size * 0.024).clamp(9.0, 13.0)),
            v.weak_text_color(),
        );
    }
}
