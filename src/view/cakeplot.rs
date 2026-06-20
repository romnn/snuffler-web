//! egui rendering of the Cake Phase "Plot Model" and "Plot Rays" windows.
//!
//! These mirror pyrocko's matplotlib `cake_plot.my_model_plot` /
//! `cake_plot.my_rays_plot` (depth increases downward; ray paths over a shaded
//! layer cross-section), but all geometry is computed by pyrocko and handed over
//! as numeric arrays in km — this module only draws.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke};

use crate::bridge::{
    CakeDiscontinuity, CakeLayer, CakeLegendEntry, CakeModelPlot, CakePoint, CakeRaysPlot,
};
use crate::view::markers::MARKER_COLORS;

/// Scarlet red (palette index 0) — a fallback if a color id is out of range.
const DEFAULT_PHASE_COLOR: Color32 = Color32::from_rgb(204, 0, 0);

fn bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(22, 24, 27)
    } else {
        Color32::from_rgb(250, 250, 250)
    }
}

fn palette(color: u8) -> Color32 {
    MARKER_COLORS
        .get(usize::from(color) & 7)
        .copied()
        .unwrap_or(DEFAULT_PHASE_COLOR)
}

/// A low-alpha tint for a model layer, mirroring `cake_plot`'s water/gradient/
/// homogeneous shade tables (alternating brightness by layer index).
fn layer_shade(lay: &CakeLayer, idx: usize, dark: bool) -> Color32 {
    let base = if lay.water {
        Color32::from_rgb(52, 101, 164) // sky blue
    } else if lay.gradient {
        Color32::from_rgb(143, 89, 2) // chocolate
    } else {
        Color32::from_rgb(245, 121, 0) // orange
    };
    let alpha: u8 = if idx.is_multiple_of(2) { 30 } else { 18 };
    let alpha = if dark { alpha } else { alpha.saturating_add(8) };
    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
}

/// Shaded layers + labelled discontinuity lines, shared by both plots.
fn draw_model_background(
    painter: &egui::Painter,
    rect: Rect,
    y_of: &dyn Fn(f32) -> f32,
    layers: &[CakeLayer],
    discs: &[CakeDiscontinuity],
    dark: bool,
) {
    for (idx, lay) in layers.iter().enumerate() {
        let (top, bot) = (y_of(lay.ztop_km), y_of(lay.zbot_km));
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left(), top.min(bot)),
                Pos2::new(rect.right(), top.max(bot)),
            ),
            0.0,
            layer_shade(lay, idx, dark),
        );
    }

    let line_col = if dark {
        Color32::from_rgb(120, 120, 120)
    } else {
        Color32::from_rgb(95, 95, 95)
    };
    let text_col = if dark {
        Color32::from_gray(205)
    } else {
        Color32::from_gray(60)
    };
    for d in discs {
        let y = y_of(d.z_km);
        if y < rect.top() - 1.0 || y > rect.bottom() + 1.0 {
            continue;
        }
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, line_col),
        );
        if let Some(name) = &d.name {
            painter.text(
                Pos2::new(rect.right() - 4.0, y),
                Align2::RIGHT_BOTTOM,
                name,
                FontId::proportional(11.0),
                text_col,
            );
        }
    }
}

/// Draw a velocity-depth curve from flat interleaved `(v_km_s, z_km)` pairs,
/// breaking the polyline where `v == 0` (e.g. S in the liquid outer core).
fn draw_profile(
    painter: &egui::Painter,
    flat: &[f32],
    x_of: &dyn Fn(f32) -> f32,
    y_of: &dyn Fn(f32) -> f32,
    col: Color32,
) {
    let mut run: Vec<Pos2> = Vec::new();
    for pair in flat.chunks_exact(2) {
        let [v, z] = pair else { continue };
        if *v <= 0.0 {
            flush_run(painter, &mut run, col);
            continue;
        }
        run.push(Pos2::new(x_of(*v), y_of(*z)));
    }
    flush_run(painter, &mut run, col);
}

fn flush_run(painter: &egui::Painter, run: &mut Vec<Pos2>, col: Color32) {
    if run.len() >= 2 {
        painter.add(Shape::line(std::mem::take(run), Stroke::new(2.0, col)));
    } else {
        run.clear();
    }
}

fn annotate(painter: &egui::Painter, rect: Rect, text: String, dark: bool) {
    painter.text(
        rect.left_top() + egui::vec2(5.0, 3.0),
        Align2::LEFT_TOP,
        text,
        FontId::proportional(11.0),
        if dark {
            Color32::GRAY
        } else {
            Color32::DARK_GRAY
        },
    );
}

/// The "Plot Model" window: P/S velocity (x) against depth (y, downward).
pub fn draw_model_plot(ui: &mut egui::Ui, plot: Option<&CakeModelPlot>) {
    let Some(plot) = plot else {
        ui.label("Computing…");
        return;
    };
    if plot.vp.len() < 2 && plot.vs.len() < 2 {
        ui.label("No model data.");
        return;
    }
    let dark = ui.visuals().dark_mode;
    let size = egui::vec2(
        ui.available_width().max(280.0),
        ui.available_height().max(320.0),
    );
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    let rect = resp.rect;
    painter.rect_filled(rect, 0.0, bg(dark));

    let vmax = (plot.vmax_km * 1.2).max(1e-3);
    let zmax = plot.zmax_km.max(1e-3);
    let zpad = zmax * 0.05;
    let zspan = (zmax + 2.0 * zpad).max(1e-6);
    let x_of = |v: f32| rect.left() + (v / vmax).clamp(0.0, 1.0) * rect.width();
    // Depth increases downward: top of the rect is the surface (z = 0).
    let y_of = |z: f32| rect.top() + ((z + zpad) / zspan).clamp(0.0, 1.0) * rect.height();

    draw_model_background(
        &painter,
        rect,
        &y_of,
        &plot.layers,
        &plot.discontinuities,
        dark,
    );
    draw_profile(&painter, &plot.vp, &x_of, &y_of, palette(0));
    draw_profile(&painter, &plot.vs, &x_of, &y_of, palette(2));

    annotate(
        &painter,
        rect,
        format!(
            "{}  —  vp (red) / vs (blue) [km/s] vs depth [km]",
            plot.title
        ),
        dark,
    );
}

/// Source (event) dot and receiver (station) triangles for the ray-paths plot.
fn draw_ray_markers(
    painter: &egui::Painter,
    x_of: &dyn Fn(f32) -> f32,
    y_of: &dyn Fn(f32) -> f32,
    source: &CakePoint,
    receivers: &[CakePoint],
    dark: bool,
) {
    let mark_col = if dark { Color32::WHITE } else { Color32::BLACK };
    painter.circle_filled(
        Pos2::new(x_of(source.x_km), y_of(source.z_km)),
        4.0,
        mark_col,
    );
    for r in receivers {
        let p = Pos2::new(x_of(r.x_km), y_of(r.z_km));
        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(p.x - 5.0, p.y - 9.0),
                Pos2::new(p.x + 5.0, p.y - 9.0),
                p,
            ],
            mark_col,
            Stroke::NONE,
        ));
    }
}

/// Phase legend (swatch + name) in the top-right of the ray-paths plot.
fn draw_ray_legend(painter: &egui::Painter, rect: Rect, legend: &[CakeLegendEntry], dark: bool) {
    let text_col = if dark {
        Color32::from_gray(210)
    } else {
        Color32::from_gray(40)
    };
    let mut ly = rect.top() + 8.0;
    let lx = rect.right() - 116.0;
    for entry in legend.iter().take(14) {
        painter.line_segment(
            [Pos2::new(lx, ly), Pos2::new(lx + 18.0, ly)],
            Stroke::new(2.5, palette(entry.color)),
        );
        painter.text(
            Pos2::new(lx + 24.0, ly),
            Align2::LEFT_CENTER,
            &entry.phase,
            FontId::proportional(11.0),
            text_col,
        );
        ly += 16.0;
    }
}

/// The "Plot Rays" window: ray paths in a distance (x) / depth (y, downward)
/// cross-section over the shaded model, with source dot and receiver markers.
pub fn draw_rays_plot(ui: &mut egui::Ui, plot: Option<&CakeRaysPlot>) {
    let Some(plot) = plot else {
        ui.label("Computing…");
        return;
    };
    let dark = ui.visuals().dark_mode;
    let size = egui::vec2(
        ui.available_width().max(320.0),
        ui.available_height().max(320.0),
    );
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    let rect = resp.rect;
    painter.rect_filled(rect, 0.0, bg(dark));

    let muted = if dark {
        Color32::GRAY
    } else {
        Color32::DARK_GRAY
    };
    if plot.rays.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No rays — enable phases, set an active event + stations, then Plot Rays.",
            FontId::proportional(12.0),
            muted,
        );
        return;
    }

    let xmin = plot.xmin_km;
    let xmax = plot.xmax_km.max(xmin + 1e-3);
    let xpad = (xmax - xmin) * 0.05;
    let xspan = (xmax - xmin + 2.0 * xpad).max(1e-6);
    let zmax = plot.zmax_km.max(1e-3);
    let zpad = zmax * 0.05;
    let zspan = (zmax + 2.0 * zpad).max(1e-6);
    let x_of = |x: f32| rect.left() + ((x - xmin + xpad) / xspan).clamp(0.0, 1.0) * rect.width();
    let y_of = |z: f32| rect.top() + ((z + zpad) / zspan).clamp(0.0, 1.0) * rect.height();

    draw_model_background(
        &painter,
        rect,
        &y_of,
        &plot.layers,
        &plot.discontinuities,
        dark,
    );

    for ray in &plot.rays {
        let col = palette(ray.color);
        for seg in &ray.segs {
            let pts: Vec<Pos2> = seg
                .chunks_exact(2)
                .filter_map(|p| {
                    let [x, z] = p else { return None };
                    Some(Pos2::new(x_of(*x), y_of(*z)))
                })
                .collect();
            if pts.len() >= 2 {
                painter.add(Shape::line(pts, Stroke::new(1.5, col)));
            }
        }
    }

    draw_ray_markers(&painter, &x_of, &y_of, &plot.source, &plot.receivers, dark);
    draw_ray_legend(&painter, rect, &plot.legend, dark);

    annotate(
        &painter,
        rect,
        format!("{}  —  distance [km] vs depth [km]", plot.title),
        dark,
    );
}
