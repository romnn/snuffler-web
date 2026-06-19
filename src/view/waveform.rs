//! The waveform canvas: vertically stacked tracks, a shared adaptive time axis,
//! NSLC labels, snuffler-style amplitude scaling, an optional waterfall
//! (spectrogram) view, markers, and mouse/keyboard interaction.

use egui::{
    Align2, Color32, ColorImage, FontId, Key, Rect, Sense, Shape, Stroke, TextureHandle,
    TextureOptions,
};

use crate::bridge::{ViewData, ViewTrack};
use crate::state::{AppState, Colormap, Subsort, ViewMode};
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

/// Style inputs that (with the data epoch) determine the waterfall textures, so
/// we only rebuild when the data or the colorbar settings change.
#[derive(Default, Clone, Copy, PartialEq)]
struct WfStyle {
    cmap: u8,
    reverse: bool,
    symmetry: bool,
    clip: (f32, f32),
}

/// Per-frame cache of waterfall textures, rebuilt only when the view data or the
/// colorbar style changes.
#[derive(Default)]
pub struct WaterfallCache {
    epoch: u64,
    style: WfStyle,
    textures: Vec<Option<TextureHandle>>,
}

impl WaterfallCache {
    fn ensure(&mut self, ctx: &egui::Context, view: &ViewData, epoch: u64, style: WfStyle) {
        if self.epoch == epoch && self.style == style && !self.textures.is_empty() {
            return;
        }
        self.epoch = epoch;
        self.style = style;
        self.textures = view
            .meta
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| build_texture(ctx, view, t, i, style))
            .collect();
    }
}

fn cmap_index(c: Colormap) -> u8 {
    match c {
        Colormap::Viridis => 0,
        Colormap::Magma => 1,
        Colormap::Inferno => 2,
        Colormap::Gray => 3,
        Colormap::Rainbow => 4,
    }
}

fn build_texture(
    ctx: &egui::Context,
    view: &ViewData,
    t: &ViewTrack,
    idx: usize,
    style: WfStyle,
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
    let (mut lo, mut hi) = (t.ymin + style.clip.0 * span, t.ymin + style.clip.1 * span);
    if style.symmetry {
        let mid = 0.5 * (lo + hi);
        let h = 0.5 * (hi - lo);
        lo = mid - h;
        hi = mid + h;
    }
    let range = (hi - lo).max(1e-6);
    let mut rgba = vec![0u8; nt * nf * 4];
    for c in 0..nt {
        for r in 0..nf {
            let f = nf - 1 - r; // low freq at the bottom
            let v = data[c * nf + f];
            let mut tn = ((v - lo) / range).clamp(0.0, 1.0);
            if style.reverse {
                tn = 1.0 - tn;
            }
            let [cr, cg, cb] = colormap(style.cmap, tn);
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

fn colormap(cmap: u8, t: f32) -> [u8; 3] {
    match cmap {
        1 => lut(&MAGMA, t),
        2 => lut(&INFERNO, t),
        3 => {
            let g = (t.clamp(0.0, 1.0) * 255.0) as u8;
            [g, g, g]
        }
        4 => rainbow(t),
        _ => lut(&VIRIDIS, t),
    }
}

/// 9-anchor colormap lookup with linear interpolation.
fn lut(a: &[[f32; 3]; 9], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0) * 8.0;
    let i = (t.floor() as usize).min(7);
    let f = t - i as f32;
    let l = |x: f32, y: f32| (x + (y - x) * f) as u8;
    [
        l(a[i][0], a[i + 1][0]),
        l(a[i][1], a[i + 1][1]),
        l(a[i][2], a[i + 1][2]),
    ]
}

/// Simple blue→red hue sweep.
fn rainbow(t: f32) -> [u8; 3] {
    let h = (1.0 - t.clamp(0.0, 1.0)) * 240.0;
    let x = 1.0 - ((h / 60.0) % 2.0 - 1.0).abs();
    let (r, g, b) = match (h / 60.0) as i32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        _ => (x, 0.0, 1.0),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

const VIRIDIS: [[f32; 3]; 9] = [
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
const MAGMA: [[f32; 3]; 9] = [
    [0., 0., 4.],
    [28., 16., 68.],
    [79., 18., 123.],
    [129., 37., 129.],
    [181., 54., 122.],
    [229., 80., 100.],
    [251., 135., 97.],
    [254., 194., 135.],
    [252., 253., 191.],
];
const INFERNO: [[f32; 3]; 9] = [
    [0., 0., 4.],
    [31., 12., 72.],
    [85., 15., 109.],
    [136., 34., 106.],
    [186., 54., 85.],
    [227., 89., 51.],
    [249., 140., 10.],
    [249., 201., 50.],
    [252., 255., 164.],
];

/// Colour a trace by the NSLC element the current subsort colours by. Falls back
/// to the channel when that element is empty (e.g. blank location codes), so the
/// default colouring stays varied like snuffler's.
fn trace_color(t: &ViewTrack, ss: Subsort) -> Color32 {
    let key = match ss.color_index() {
        0 => &t.net,
        1 => &t.sta,
        2 => &t.loc,
        _ => &t.cha,
    };
    let key = if key.is_empty() { &t.cha } else { key };
    const PALETTE: [(u8, u8, u8); 7] = [
        (0xCC, 0x4A, 0x4A),
        (0x3A, 0x86, 0xC8),
        (0x3F, 0xA6, 0x5E),
        (0xC9, 0x92, 0x2E),
        (0x8E, 0x55, 0xC9),
        (0x2F, 0xA8, 0xA8),
        (0xC8, 0x5F, 0x9C),
    ];
    let h = key
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(131).wrapping_add(b as u32));
    let (r, g, b) = PALETTE[(h as usize) % PALETTE.len()];
    Color32::from_rgb(r, g, b)
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
    ui.ctx()
        .options_mut(|o| o.tessellation_options.feathering = state.antialiasing);
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
        let style = WfStyle {
            cmap: cmap_index(state.wf_cmap),
            reverse: state.wf_reverse,
            symmetry: state.wf_symmetry,
            clip: state.wf_clip,
        };
        wf.ensure(ui.ctx(), view, view_epoch, style);
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
    let scale_font = FontId::proportional(10.0);
    for (k, t) in visible.iter().enumerate() {
        let top = track_area.top() + k as f32 * row_h;
        let bot = top + row_h;
        let mid = top + row_h * 0.5;

        if state.show_boxes {
            let s = Stroke::new(1.0, pal.sep);
            let r = Rect::from_min_max(
                egui::pos2(rect.left() + 0.5, top + 1.0),
                egui::pos2(rect.right() - 0.5, bot - 1.0),
            );
            painter.line_segment([r.left_top(), r.right_top()], s);
            painter.line_segment([r.right_top(), r.right_bottom()], s);
            painter.line_segment([r.right_bottom(), r.left_bottom()], s);
            painter.line_segment([r.left_bottom(), r.left_top()], s);
        } else if k > 0 {
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
            // Display scale (units→pixels) comes entirely from pyrocko (yscale).
            let half = t.yscale.max(1e-12);
            let amp = row_h * 0.45 * state.gain / half;
            // "Clip traces" keeps each trace inside its own box; off lets it spill.
            let (cl_top, cl_bot) = if state.clip_traces {
                (top + 1.0, bot - 1.0)
            } else {
                (track_area.top(), track_area.bottom())
            };
            let y_of = |v: f32| (mid - (v - t.ymean) * amp).clamp(cl_top, cl_bot);

            if state.show_zero_lines {
                let yz = y_of(0.0);
                painter.line_segment(
                    [
                        egui::pos2(track_area.left(), yz),
                        egui::pos2(track_area.right(), yz),
                    ],
                    Stroke::new(1.0, pal.zero),
                );
            }

            let trace_col = if state.color_traces {
                trace_color(t, state.subsort)
            } else {
                pal.trace
            };
            let samples = view.samples(t);
            if samples.len() >= 2 {
                let mut pts = Vec::with_capacity(samples.len());
                for (i, &v) in samples.iter().enumerate() {
                    let t_rel = t.t0_rel + i as f64 * t.dt;
                    pts.push(egui::pos2(x_of(t_rel), y_of(v)));
                }
                painter.add(Shape::line(pts, Stroke::new(1.0, trace_col)));
            }

            if state.show_scale_axes {
                let xa = rect.right() - 2.0;
                painter.line_segment(
                    [egui::pos2(xa, top + 3.0), egui::pos2(xa, bot - 3.0)],
                    Stroke::new(1.0, pal.sep),
                );
            }
            if state.show_scale_ranges {
                painter.text(
                    egui::pos2(rect.right() - 6.0, top + 3.0),
                    Align2::RIGHT_TOP,
                    format!("{:+.2e}", t.ymax),
                    scale_font.clone(),
                    pal.axis_text,
                );
                painter.text(
                    egui::pos2(rect.right() - 6.0, bot - 3.0),
                    Align2::RIGHT_BOTTOM,
                    format!("{:+.2e}", t.ymin),
                    scale_font.clone(),
                    pal.axis_text,
                );
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
