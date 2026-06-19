//! The waveform canvas: vertically stacked tracks, a shared adaptive time axis,
//! NSLC labels, snuffler-style amplitude scaling, an optional waterfall
//! (spectrogram) view, markers, and mouse/keyboard interaction.

use egui::{
    Align2, Color32, ColorImage, FontId, Key, Rect, Sense, Shape, Stroke, TextureHandle,
    TextureOptions,
};

use crate::bridge::{ViewData, ViewTrack};
use crate::state::{AppState, ScalingMode, ViewMode};
use crate::view::markers::{Marker, PHASE_KEYS};
use crate::view::timeaxis;

const AXIS_H: f32 = 32.0;
const LABEL_PT: f32 = 12.5;
const AXIS_PT: f32 = 12.5;

struct Palette {
    bg: Color32,
    trace: Color32,
    grid: Color32,
    zero: Color32,
    axis_text: Color32,
    sep: Color32,
    label_bg: Color32,
    cursor: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(22, 24, 27),
            trace: Color32::from_rgb(222, 224, 228),
            grid: Color32::from_rgb(48, 51, 56),
            zero: Color32::from_rgb(40, 43, 48),
            axis_text: Color32::from_rgb(196, 200, 206),
            sep: Color32::from_rgb(54, 57, 62),
            label_bg: Color32::from_rgba_unmultiplied(16, 18, 20, 205),
            cursor: Color32::from_rgba_unmultiplied(255, 255, 255, 50),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(250, 250, 250),
            trace: Color32::from_rgb(45, 48, 52),
            grid: Color32::from_rgb(224, 224, 224),
            zero: Color32::from_rgb(236, 236, 236),
            axis_text: Color32::from_rgb(64, 64, 64),
            sep: Color32::from_rgb(214, 214, 214),
            label_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 216),
            cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        }
    }
}

/// Per-frame cache of waterfall textures, rebuilt only when the view data
/// (epoch) or colormap clip changes.
#[derive(Default)]
pub struct WaterfallCache {
    epoch: u64,
    clip: (f32, f32),
    textures: Vec<Option<TextureHandle>>,
}

impl WaterfallCache {
    fn ensure(&mut self, ctx: &egui::Context, view: &ViewData, epoch: u64, clip: (f32, f32)) {
        if self.epoch == epoch && self.clip == clip && !self.textures.is_empty() {
            return;
        }
        self.epoch = epoch;
        self.clip = clip;
        self.textures = view
            .meta
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| build_texture(ctx, view, t, i, clip))
            .collect();
    }
}

fn build_texture(
    ctx: &egui::Context,
    view: &ViewData,
    t: &ViewTrack,
    idx: usize,
    clip: (f32, f32),
) -> Option<TextureHandle> {
    if t.nf == 0 || t.nt == 0 {
        return None;
    }
    let (nf, nt) = (t.nf, t.nt);
    let data = view.samples(t);
    if data.len() < nf * nt {
        return None;
    }
    let span = (t.ymax - t.ymin).max(1e-6);
    let lo = t.ymin + clip.0 * span;
    let hi = t.ymin + clip.1 * span;
    let range = (hi - lo).max(1e-6);
    let mut rgba = vec![0u8; nt * nf * 4];
    for c in 0..nt {
        for r in 0..nf {
            let f = nf - 1 - r; // low freq at the bottom
            let v = data[c * nf + f];
            let tn = ((v - lo) / range).clamp(0.0, 1.0);
            let [cr, cg, cb] = viridis(tn);
            let p = (r * nt + c) * 4;
            rgba[p] = cr;
            rgba[p + 1] = cg;
            rgba[p + 2] = cb;
            rgba[p + 3] = 255;
        }
    }
    let img = ColorImage::from_rgba_unmultiplied([nt, nf], &rgba);
    Some(ctx.load_texture(format!("wf{idx}"), img, TextureOptions::LINEAR))
}

/// Compact viridis approximation (9 anchors, linear interpolation).
fn viridis(t: f32) -> [u8; 3] {
    const A: [[f32; 3]; 9] = [
        [68., 1., 84.],
        [72., 40., 120.],
        [62., 73., 137.],
        [49., 104., 142.],
        [38., 130., 142.],
        [31., 158., 137.],
        [53., 183., 121.],
        [110., 206., 88.],
        [253., 231., 37.],
    ];
    let t = t.clamp(0.0, 1.0) * 8.0;
    let i = (t.floor() as usize).min(7);
    let f = t - i as f32;
    let lerp = |a: f32, b: f32| (a + (b - a) * f) as u8;
    [
        lerp(A[i][0], A[i + 1][0]),
        lerp(A[i][1], A[i + 1][1]),
        lerp(A[i][2], A[i + 1][2]),
    ]
}

fn track_half(t: &ViewTrack) -> f32 {
    (t.ymax - t.ymean).max(t.ymean - t.ymin).max(1e-12)
}

/// Draw the canvas and process interaction.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    view: Option<&ViewData>,
    wf: &mut WaterfallCache,
    view_epoch: u64,
) {
    let pal = palette(ui.visuals().dark_mode);
    let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = resp.rect;
    painter.rect_filled(rect, 0.0, pal.bg);

    let track_area = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - AXIS_H));

    let Some(view) = view else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Loading…",
            FontId::proportional(16.0),
            pal.axis_text,
        );
        handle_keyboard(ui, state, 0);
        return;
    };
    let meta = &view.meta;
    let waterfall = state.view_mode == ViewMode::Waterfall;
    if waterfall {
        wf.ensure(ui.ctx(), view, view_epoch, state.wf_clip);
    }

    let view_tmin = meta.view_tmin;
    let window = (meta.view_tmax - meta.view_tmin).max(1e-9);
    let x_of = |t_rel: f64| rect.left() + (t_rel / window) as f32 * rect.width();
    let t_at_x =
        |x: f32| view_tmin + ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * window;

    let total = meta.tracks.len();
    handle_keyboard(ui, state, total);
    let n_shown = state.ntracks_shown.clamp(1, total.max(1));
    let max_off = total.saturating_sub(n_shown);
    state.track_offset = state.track_offset.min(max_off);
    let off = state.track_offset;
    let visible: &[ViewTrack] = &meta.tracks[off..(off + n_shown).min(total)];
    let row_h = track_area.height() / n_shown.max(1) as f32;

    let common_half = if state.scaling == ScalingMode::Common {
        visible.iter().map(track_half).fold(1e-12_f32, f32::max)
    } else {
        0.0
    };

    // time grid + axis labels
    let tinc = timeaxis::nice_tinc(window, (rect.width() / 90.0).max(2.0) as f64);
    let axis_font = FontId::proportional(AXIS_PT);
    for tick in timeaxis::ticks(view_tmin, meta.view_tmax, tinc) {
        let x = x_of(tick - view_tmin);
        painter.line_segment(
            [
                egui::pos2(x, track_area.top()),
                egui::pos2(x, track_area.bottom()),
            ],
            Stroke::new(1.0, pal.grid),
        );
        if x > rect.left() + 76.0 {
            painter.text(
                egui::pos2(x, track_area.bottom() + 4.0),
                Align2::CENTER_TOP,
                timeaxis::tick_label(tick, tinc),
                axis_font.clone(),
                pal.axis_text,
            );
        }
    }
    painter.text(
        egui::pos2(rect.left() + 5.0, track_area.bottom() + 4.0),
        Align2::LEFT_TOP,
        timeaxis::context_label(view_tmin),
        axis_font.clone(),
        pal.axis_text,
    );

    // tracks
    let label_font = FontId::proportional(LABEL_PT);
    for (k, t) in visible.iter().enumerate() {
        let top = track_area.top() + k as f32 * row_h;
        let mid = top + row_h * 0.5;
        if k > 0 {
            painter.line_segment(
                [egui::pos2(rect.left(), top), egui::pos2(rect.right(), top)],
                Stroke::new(1.0, pal.sep),
            );
        }

        if waterfall && t.nf > 0 {
            if let Some(Some(tex)) = wf.textures.get(off + k) {
                let x0 = x_of(t.t0_rel);
                let x1 = x_of(t.t0_rel + t.nt as f64 * t.dt);
                let r = Rect::from_min_max(
                    egui::pos2(x0, top + 1.0),
                    egui::pos2(x1, top + row_h - 1.0),
                );
                painter.image(
                    tex.id(),
                    r,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        } else {
            let half = if state.scaling == ScalingMode::Common {
                common_half
            } else {
                track_half(t)
            };
            let amp = row_h * 0.45 * state.gain / half;
            let clamp_y = |y: f32| y.clamp(track_area.top(), track_area.bottom());
            let y_of = |v: f32| clamp_y(mid - (v - t.ymean) * amp);

            let yz = y_of(0.0);
            painter.line_segment(
                [
                    egui::pos2(track_area.left(), yz),
                    egui::pos2(track_area.right(), yz),
                ],
                Stroke::new(1.0, pal.zero),
            );

            let samples = view.samples(t);
            if samples.len() >= 2 {
                let mut pts = Vec::with_capacity(samples.len());
                for (i, &v) in samples.iter().enumerate() {
                    let t_rel = t.t0_rel + i as f64 * t.dt;
                    pts.push(egui::pos2(x_of(t_rel), y_of(v)));
                }
                painter.add(Shape::line(pts, Stroke::new(1.0, pal.trace)));
            }
        }

        // NSLC label with a translucent background
        let label_pos = egui::pos2(track_area.left() + 5.0, top + 3.0);
        let galley = painter.layout_no_wrap(t.label.clone(), label_font.clone(), pal.axis_text);
        let bg = Rect::from_min_size(label_pos, galley.size()).expand(3.0);
        painter.rect_filled(bg, 3.0, pal.label_bg);
        painter.galley(label_pos, galley, pal.axis_text);
    }

    draw_markers(&painter, state, track_area, view_tmin, &x_of);
    if let Some((a, b)) = state.pending_span {
        let r = Rect::from_min_max(
            egui::pos2(x_of(a - view_tmin), track_area.top()),
            egui::pos2(x_of(b - view_tmin), track_area.bottom()),
        );
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(52, 101, 164, 40));
    }

    if let Some(p) = resp.hover_pos() {
        if track_area.contains(p) {
            painter.line_segment(
                [
                    egui::pos2(p.x, track_area.top()),
                    egui::pos2(p.x, track_area.bottom()),
                ],
                Stroke::new(1.0, pal.cursor),
            );
        }
    }

    handle_interaction(ui, &resp, state, view_tmin, window, rect, total, &t_at_x);
}

fn draw_markers(
    painter: &egui::Painter,
    state: &AppState,
    area: Rect,
    view_tmin: f64,
    x_of: &impl Fn(f64) -> f32,
) {
    for m in &state.markers {
        let col = m.color();
        let x0 = x_of(m.tmin - view_tmin);
        if (x0 < area.left() - 2.0 || x0 > area.right() + 2.0) && !m.is_span() {
            continue;
        }
        if m.is_span() {
            let x1 = x_of(m.tmax - view_tmin);
            let r = Rect::from_min_max(egui::pos2(x0, area.top()), egui::pos2(x1, area.bottom()));
            painter.rect_filled(
                r,
                0.0,
                Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 30),
            );
        }
        let w = if m.selected { 2.5 } else { 1.0 };
        painter.line_segment(
            [egui::pos2(x0, area.top()), egui::pos2(x0, area.bottom())],
            Stroke::new(w, col),
        );
        if m.is_event {
            let s = 6.0;
            painter.add(Shape::convex_polygon(
                vec![
                    egui::pos2(x0, area.top() + s),
                    egui::pos2(x0 - s, area.top()),
                    egui::pos2(x0 + s, area.top()),
                ],
                col,
                Stroke::NONE,
            ));
        }
        if let Some(ph) = &m.phase {
            painter.text(
                egui::pos2(x0 + 3.0, area.top() + 2.0),
                Align2::LEFT_TOP,
                ph,
                FontId::proportional(LABEL_PT),
                col,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_interaction(
    ui: &egui::Ui,
    resp: &egui::Response,
    state: &mut AppState,
    view_tmin: f64,
    window: f64,
    rect: Rect,
    total: usize,
    t_at_x: &impl Fn(f32) -> f64,
) {
    let modifiers = ui.input(|i| i.modifiers);
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);

    if scroll != 0.0 && resp.hovered() {
        if modifiers.ctrl || modifiers.command {
            state.gain *= (scroll * 0.0015).exp();
        } else if modifiers.shift {
            let step = if scroll < 0.0 { 1isize } else { -1isize };
            let no = state.track_offset as isize + step;
            state.track_offset = no.clamp(0, total.saturating_sub(1) as isize) as usize;
        } else {
            let pivot = resp
                .hover_pos()
                .map(|p| t_at_x(p.x))
                .unwrap_or(view_tmin + window * 0.5);
            let factor = (-scroll * 0.0015).exp() as f64;
            zoom_time(state, pivot, factor);
        }
    }

    if modifiers.shift {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at_x(p.x);
            if resp.drag_started() {
                state.pending_span = Some((t, t));
            } else if resp.dragged() {
                if let Some((a, _)) = state.pending_span {
                    state.pending_span = Some((a, t));
                }
            }
        }
        if resp.drag_stopped() {
            if let Some((a, b)) = state.pending_span.take() {
                if (a - b).abs() * (rect.width() as f64 / window) > 3.0 {
                    state.markers.push(Marker::new(a, b, state.marker_kind));
                }
            }
        }
    } else if resp.dragged() {
        let dx = resp.drag_delta().x as f64;
        let dt = -dx / rect.width() as f64 * window;
        state.tmin += dt;
        state.tmax += dt;
    }

    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at_x(p.x);
            state.markers.push(Marker::new(t, t, state.marker_kind));
        }
    } else if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let tol_t = 5.0 / rect.width() as f64 * window;
            let mut hit = None;
            for (i, m) in state.markers.iter().enumerate() {
                if (m.tmin - t_at_x(p.x)).abs() < tol_t {
                    hit = Some(i);
                }
            }
            for m in &mut state.markers {
                m.selected = false;
            }
            if let Some(i) = hit {
                state.markers[i].selected = true;
            }
        }
    }
}

fn zoom_time(state: &mut AppState, pivot: f64, factor: f64) {
    let factor = factor.clamp(0.02, 50.0);
    state.tmin = pivot - (pivot - state.tmin) * factor;
    state.tmax = pivot + (state.tmax - pivot) * factor;
}

fn handle_keyboard(ui: &egui::Ui, state: &mut AppState, total: usize) {
    ui.input(|i| {
        let win = (state.tmax - state.tmin).max(1e-9);
        if i.key_pressed(Key::Space) {
            state.tmin += win * 0.8;
            state.tmax += win * 0.8;
        }
        if i.key_pressed(Key::B) {
            state.tmin -= win * 0.8;
            state.tmax -= win * 0.8;
        }
        if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
            state.ntracks_shown = state.ntracks_shown.saturating_sub(2).max(1);
        }
        if i.key_pressed(Key::Minus) {
            state.ntracks_shown = (state.ntracks_shown + 2).min(total.max(1));
        }
        if i.key_pressed(Key::PageDown) {
            state.track_offset =
                (state.track_offset + state.ntracks_shown).min(total.saturating_sub(1));
        }
        if i.key_pressed(Key::PageUp) {
            state.track_offset = state.track_offset.saturating_sub(state.ntracks_shown);
        }
        if i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete) {
            state.markers.retain(|m| !m.selected);
        }

        let num_keys = [
            Key::Num0,
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
        ];
        for (n, key) in num_keys.iter().enumerate() {
            if i.key_pressed(*key) {
                state.marker_kind = n as u8;
                for m in &mut state.markers {
                    if m.selected {
                        m.kind = n as u8;
                    }
                }
            }
        }

        let f_keys = [
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
        ];
        for (n, key) in f_keys.iter().enumerate() {
            if i.key_pressed(*key) {
                for m in &mut state.markers {
                    if m.selected {
                        m.phase = Some(PHASE_KEYS[n].to_string());
                    }
                }
            }
        }
    });
}
