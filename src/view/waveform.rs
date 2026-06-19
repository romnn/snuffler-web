//! The waveform canvas: vertically stacked tracks, a shared adaptive time axis,
//! NSLC labels, snuffler-style amplitude scaling, an optional waterfall
//! (spectrogram) view, markers, and mouse/keyboard interaction.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "rendering maps between f32/f64 pixel & time coordinates and integer \
              sample/track indices; these casts are inherent to the drawing math"
)]

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

#[allow(
    clippy::indexing_slicing,
    reason = "index provably in bounds: data.len() >= nf*nt is checked above, and \
              c<nt, f<nf so c*nf+f < nf*nt; rgba is sized nt*nf*4 and p+3 < that bound"
)]
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

/// Round and clamp a colour value (already in 0..=255) to an 8-bit component.
fn to_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// 9-anchor colormap lookup with linear interpolation.
fn lut(anchors: &[[f32; 3]; 9], value: f32) -> [u8; 3] {
    let scaled = (value.clamp(0.0, 1.0) * 8.0).clamp(0.0, 8.0);
    let idx = (scaled as usize).min(7);
    let frac = scaled - idx as f32;
    let lo = anchors.get(idx).copied().unwrap_or([0.0; 3]);
    let hi = anchors.get(idx + 1).copied().unwrap_or(lo);
    let lerp = |a: f32, b: f32| to_u8(a + (b - a) * frac);
    [lerp(lo[0], hi[0]), lerp(lo[1], hi[1]), lerp(lo[2], hi[2])]
}

/// Simple blue→red hue sweep.
fn rainbow(value: f32) -> [u8; 3] {
    let hue = (1.0 - value.clamp(0.0, 1.0)) * 240.0;
    let sextant = hue / 60.0;
    let side = 1.0 - (sextant % 2.0 - 1.0).abs();
    let (red, green, blue) = match sextant as i32 {
        0 => (1.0, side, 0.0),
        1 => (side, 1.0, 0.0),
        2 => (0.0, 1.0, side),
        3 => (0.0, side, 1.0),
        _ => (side, 0.0, 1.0),
    };
    [
        to_u8(red * 255.0),
        to_u8(green * 255.0),
        to_u8(blue * 255.0),
    ]
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
fn trace_color(track: &ViewTrack, subsort: Subsort) -> Color32 {
    const PALETTE: [(u8, u8, u8); 7] = [
        (0xCC, 0x4A, 0x4A),
        (0x3A, 0x86, 0xC8),
        (0x3F, 0xA6, 0x5E),
        (0xC9, 0x92, 0x2E),
        (0x8E, 0x55, 0xC9),
        (0x2F, 0xA8, 0xA8),
        (0xC8, 0x5F, 0x9C),
    ];
    let key = match subsort.color_index() {
        0 => &track.net,
        1 => &track.sta,
        2 => &track.loc,
        _ => &track.cha,
    };
    let key = if key.is_empty() { &track.cha } else { key };
    let hash = key.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(131).wrapping_add(u32::from(byte))
    });
    let (red, green, blue) = PALETTE
        .get(hash as usize % PALETTE.len())
        .copied()
        .unwrap_or((0x88, 0x88, 0x88));
    Color32::from_rgb(red, green, blue)
}

/// Draw the canvas and process interaction.
#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
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
        |x: f32| view_tmin + f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * window;

    let total = meta.tracks.len();
    handle_keyboard(ui, state, total);
    let n_shown = state.ntracks_shown.clamp(1, total.max(1));
    let max_off = total.saturating_sub(n_shown);
    state.track_offset = state.track_offset.min(max_off);
    let off = state.track_offset;
    let visible: &[ViewTrack] = meta
        .tracks
        .get(off..(off + n_shown).min(total))
        .unwrap_or(&[]);
    let row_h = track_area.height() / n_shown.max(1) as f32;

    // time grid + axis labels
    let tinc = timeaxis::nice_tinc(window, f64::from((rect.width() / 90.0).max(2.0)));
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
            let trace_clip = Rect::from_min_max(
                egui::pos2(track_area.left(), cl_top),
                egui::pos2(track_area.right(), cl_bot),
            );
            let trace_painter = painter.with_clip_rect(trace_clip);
            let y_of = |v: f32| mid - (v - t.ymean) * amp;

            if state.show_zero_lines {
                let yz = y_of(0.0);
                trace_painter.line_segment(
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
            if t.dt > 0.0 {
                let samples = view.samples(t);
                if samples.len() >= 2 {
                    let points: Vec<_> = samples
                        .iter()
                        .enumerate()
                        .map(|(i, &v)| {
                            let t_rel = t.t0_rel + i as f64 * t.dt;
                            egui::pos2(x_of(t_rel), y_of(v))
                        })
                        .collect();
                    trace_painter.add(Shape::line(points, Stroke::new(1.0, trace_col)));
                }
            } else if t.dt == 0.0 {
                let start = t.offset.min(view.ys.len());
                let end = (t.offset + t.count.saturating_mul(2)).min(view.ys.len());
                if let Some(points_data) = view.ys.get(start..end)
                    && points_data.len() >= 4
                {
                    let points: Vec<_> = points_data
                        .chunks_exact(2)
                        .filter_map(|pair| match pair {
                            [px, py] => {
                                Some(egui::pos2(x_of(t.t0_rel + f64::from(*px)), y_of(*py)))
                            }
                            _ => None,
                        })
                        .collect();
                    trace_painter.add(Shape::line(points, Stroke::new(1.0, trace_col)));
                }
            } else if t.dt < 0.0 {
                let start = t.offset.min(view.ys.len());
                let end = (t.offset + t.count.saturating_mul(3)).min(view.ys.len());
                if let Some(envelope) = view.ys.get(start..end)
                    && envelope.len() >= 3
                {
                    let stroke = Stroke::new(1.0, trace_col);
                    for triple in envelope.chunks_exact(3) {
                        let [t0, ylo, yhi] = triple else {
                            continue;
                        };
                        let x = x_of(t.t0_rel + f64::from(*t0));
                        trace_painter.line_segment(
                            [egui::pos2(x, y_of(*ylo)), egui::pos2(x, y_of(*yhi))],
                            stroke,
                        );
                    }
                }
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

    draw_markers(
        &painter, state, track_area, visible, row_h, view_tmin, &x_of,
    );
    if let Some((a, b)) = state.pending_span {
        let r = Rect::from_min_max(
            egui::pos2(x_of(a - view_tmin), track_area.top()),
            egui::pos2(x_of(b - view_tmin), track_area.bottom()),
        );
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(52, 101, 164, 40));
    }

    if let Some(p) = resp.hover_pos()
        && track_area.contains(p)
    {
        painter.line_segment(
            [
                egui::pos2(p.x, track_area.top()),
                egui::pos2(p.x, track_area.bottom()),
            ],
            Stroke::new(1.0, pal.cursor),
        );
    }

    handle_interaction(ui, &resp, state, view_tmin, window, rect, total, &t_at_x);
}

/// Does an NSLC-scoped marker apply to this track? Empty or `*` fields are
/// wildcards (snuffler's per-channel marker matching).
fn marker_matches_track(nslc: &(String, String, String, String), t: &ViewTrack) -> bool {
    let f = |pat: &str, val: &str| pat.is_empty() || pat == "*" || pat == val;
    f(&nslc.0, &t.net) && f(&nslc.1, &t.sta) && f(&nslc.2, &t.loc) && f(&nslc.3, &t.cha)
}

fn draw_markers(
    painter: &egui::Painter,
    state: &AppState,
    area: Rect,
    visible: &[ViewTrack],
    row_h: f32,
    view_tmin: f64,
    x_of: &impl Fn(f64) -> f32,
) {
    for m in &state.markers {
        let col = m.color();
        let x0 = x_of(m.tmin - view_tmin);
        if (x0 < area.left() - 2.0 || x0 > area.right() + 2.0) && !m.is_span() {
            continue;
        }
        // Vertical extent: full canvas for channel-agnostic markers; otherwise
        // one segment per matching track row, so per-station picks form a moveout.
        let spans: Vec<(f32, f32)> = match &m.nslc {
            None => vec![(area.top(), area.bottom())],
            Some(nslc) => visible
                .iter()
                .enumerate()
                .filter(|(_, t)| marker_matches_track(nslc, t))
                .map(|(k, _)| {
                    let top = area.top() + k as f32 * row_h;
                    (top, top + row_h)
                })
                .collect(),
        };
        let label = match (&m.phase, m.polarity_symbol()) {
            (Some(ph), sym) if !sym.is_empty() => format!("{ph} {sym}"),
            (Some(ph), _) => ph.clone(),
            (None, sym) if !sym.is_empty() => sym.to_string(),
            (None, _) => String::new(),
        };
        let w = if m.selected { 2.5 } else { 1.0 };
        for (ytop, ybot) in spans {
            if m.is_span() {
                let x1 = x_of(m.tmax - view_tmin);
                let r = Rect::from_min_max(egui::pos2(x0, ytop), egui::pos2(x1, ybot));
                painter.rect_filled(
                    r,
                    0.0,
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 30),
                );
            }
            painter.line_segment(
                [egui::pos2(x0, ytop), egui::pos2(x0, ybot)],
                Stroke::new(w, col),
            );
            if m.is_event {
                let s = 6.0;
                painter.add(Shape::convex_polygon(
                    vec![
                        egui::pos2(x0, ytop + s),
                        egui::pos2(x0 - s, ytop),
                        egui::pos2(x0 + s, ytop),
                    ],
                    col,
                    Stroke::NONE,
                ));
            }
            if !label.is_empty() {
                painter.text(
                    egui::pos2(x0 + 3.0, ytop + 2.0),
                    Align2::LEFT_TOP,
                    &label,
                    FontId::proportional(LABEL_PT),
                    col,
                );
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "interaction needs the full view + projection context to map pointer input to time"
)]
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
                .map_or(view_tmin + window * 0.5, |p| t_at_x(p.x));
            let factor = f64::from((-scroll * 0.0015).exp());
            zoom_time(state, pivot, factor);
        }
    }

    if modifiers.shift {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at_x(p.x);
            if resp.drag_started() {
                state.pending_span = Some((t, t));
            } else if resp.dragged()
                && let Some((a, _)) = state.pending_span
            {
                state.pending_span = Some((a, t));
            }
        }
        if resp.drag_stopped()
            && let Some((a, b)) = state.pending_span.take()
            && (a - b).abs() * (f64::from(rect.width()) / window) > 3.0
        {
            state.markers.push(Marker::new(a, b, state.marker_kind));
        }
    } else if resp.dragged() {
        let dx = f64::from(resp.drag_delta().x);
        let dt = -dx / f64::from(rect.width()) * window;
        state.tmin += dt;
        state.tmax += dt;
    }

    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at_x(p.x);
            state.markers.push(Marker::new(t, t, state.marker_kind));
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let tol_t = 5.0 / f64::from(rect.width()) * window;
        let mut hit = None;
        for (i, m) in state.markers.iter().enumerate() {
            if (m.tmin - t_at_x(p.x)).abs() < tol_t {
                hit = Some(i);
            }
        }
        for m in &mut state.markers {
            m.selected = false;
        }
        if let Some(m) = hit.and_then(|i| state.markers.get_mut(i)) {
            m.selected = true;
        }
    }
}

fn zoom_time(state: &mut AppState, pivot: f64, factor: f64) {
    let factor = factor.clamp(0.02, 50.0);
    state.tmin = pivot - (pivot - state.tmin) * factor;
    state.tmax = pivot + (state.tmax - pivot) * factor;
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
fn handle_keyboard(ui: &egui::Ui, state: &mut AppState, total: usize) {
    // Don't fire single-key shortcuts while a text field (FDSN forms, command
    // line) has the keyboard — otherwise typing "n" would jump markers.
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    ui.input(|i| {
        let win = (state.tmax - state.tmin).max(1e-9);
        let shift = i.modifiers.shift;
        if i.key_pressed(Key::Space) {
            state.tmin += win * 0.8;
            state.tmax += win * 0.8;
        }
        if i.key_pressed(Key::B) {
            state.tmin -= win * 0.8;
            state.tmax -= win * 0.8;
        }
        // Track-zoom keys (snuffler: + shows one track MORE, - one LESS, = resets
        // to the initial count). Plus is Shift+Equals on most layouts, so accept
        // either for "more"; bare Equals (no shift) is the reset.
        if i.key_pressed(Key::Plus) || (i.key_pressed(Key::Equals) && shift) {
            state.ntracks_shown = (state.ntracks_shown + 1).min(total.max(1));
        }
        if i.key_pressed(Key::Minus) {
            state.ntracks_shown = state.ntracks_shown.saturating_sub(1).max(1);
        }
        if i.key_pressed(Key::Equals) && !shift {
            state.ntracks_shown = crate::state::DEFAULT_NTRACKS_SHOWN.min(total.max(1));
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

        // Selection navigation (snuffler keyPressEvent, pile_viewer.py:2258-2368).
        if i.key_pressed(Key::Tab) {
            cycle_selection(state, !shift);
        }
        if i.key_pressed(Key::N) {
            select_relative(state, true, shift); // Shift+N: events only
        }
        if i.key_pressed(Key::P) {
            select_relative(state, false, shift); // Shift+P: events only
        }
        if i.key_pressed(Key::A) {
            if shift {
                for m in &mut state.markers {
                    m.selected = true;
                }
            } else {
                for m in &mut state.markers {
                    m.selected = m.tmin <= state.tmax && m.tmax >= state.tmin; // overlaps view
                }
            }
        }
        if i.key_pressed(Key::D) || i.key_pressed(Key::Escape) {
            for m in &mut state.markers {
                m.selected = false;
            }
        }
        if i.key_pressed(Key::G) {
            go_to_selection(state, shift); // Shift+G: tight
        }
        if i.key_pressed(Key::M) {
            state.show_marker_editor = !state.show_marker_editor;
        }
        if i.key_pressed(Key::C) {
            state.show_controls = !state.show_controls;
        }
        if i.key_pressed(Key::Questionmark) || (i.key_pressed(Key::Slash) && shift) {
            state.show_help = true;
        }
        if i.key_pressed(Key::Colon) || (i.key_pressed(Key::Semicolon) && shift) {
            state.show_command = true;
        }

        // e: convert selected markers to event markers; E (shift): back to plain
        // markers (snuffler's "convert to event" / "deactivate event").
        if i.key_pressed(Key::E) {
            let to_event = !shift;
            for m in &mut state.markers {
                if m.selected {
                    m.is_event = to_event;
                    if to_event {
                        m.phase = None; // an event marker is not a phase pick
                    }
                }
            }
        }

        // First-motion polarity on selected phase picks (pile_viewer.py:2233-2251).
        if i.key_pressed(Key::ArrowUp) {
            set_polarity_selected(state, if shift { Some(0) } else { Some(1) });
        }
        if i.key_pressed(Key::ArrowDown) {
            set_polarity_selected(state, if shift { Some(0) } else { Some(-1) });
        }

        // Nudge selected picks (pile_viewer.py:2484-2491); Shift = ×10.
        if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowRight) {
            let dir = if i.key_pressed(Key::ArrowLeft) {
                -1.0
            } else {
                1.0
            };
            let amount = if shift { 10.0 } else { 1.0 };
            let dt = dir * amount * win * 0.002;
            for m in &mut state.markers {
                if m.selected && !m.is_event {
                    m.tmin += dt;
                    m.tmax += dt;
                }
            }
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
            if i.key_pressed(*key)
                && let Some(phase) = PHASE_KEYS.get(n)
            {
                for m in &mut state.markers {
                    if m.selected {
                        m.phase = Some((*phase).to_string());
                    }
                }
            }
        }
    });
}

/// Toggle/assign polarity on selected non-event markers, matching snuffler's
/// Up/Down logic: a repeated press in the same direction clears it again.
fn set_polarity_selected(state: &mut AppState, target: Option<i8>) {
    for m in &mut state.markers {
        if m.selected && !m.is_event {
            m.polarity = match target {
                Some(0) => Some(0),
                t if m.polarity == t => None,
                t => t,
            };
        }
    }
}

/// Recenter the view on the selected markers (snuffler's `go_to_selection`).
/// `tight` zooms to the selected span (±10% margin) instead of keeping the
/// current window length.
fn go_to_selection(state: &mut AppState, tight: bool) {
    let sel: Vec<&Marker> = state.markers.iter().filter(|m| m.selected).collect();
    if sel.is_empty() {
        return;
    }
    let tmin = sel.iter().map(|m| m.tmin).fold(f64::INFINITY, f64::min);
    let tmax = sel.iter().map(|m| m.tmax).fold(f64::NEG_INFINITY, f64::max);
    let win = (state.tmax - state.tmin).max(1e-9);
    if tight && tmax > tmin {
        let pad = (tmax - tmin) * 0.1;
        state.tmin = tmin - pad;
        state.tmax = tmax + pad;
    } else {
        let mid = 0.5 * (tmin + tmax);
        state.tmin = mid - win * 0.5;
        state.tmax = mid + win * 0.5;
    }
}

/// Select the next/previous marker by time and recenter (snuffler's `n`/`p`,
/// or `N`/`P` to step through event markers only).
fn select_relative(state: &mut AppState, forward: bool, events_only: bool) {
    let win = (state.tmax - state.tmin).max(1e-9);
    let center = 0.5 * (state.tmin + state.tmax);
    let reference = state
        .markers
        .iter()
        .filter(|m| m.selected)
        .map(|m| m.tmin)
        .fold(None, |acc: Option<f64>, t| {
            Some(match acc {
                None => t,
                Some(a) if forward => a.max(t),
                Some(a) => a.min(t),
            })
        })
        .unwrap_or(center);

    let pick = state
        .markers
        .iter()
        .enumerate()
        .filter(|(_, m)| !events_only || m.is_event)
        .filter(|(_, m)| {
            if forward {
                m.tmin > reference
            } else {
                m.tmin < reference
            }
        })
        .min_by(|(_, a), (_, b)| {
            let (ka, kb) = if forward {
                (a.tmin, b.tmin)
            } else {
                (-a.tmin, -b.tmin)
            };
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(idx, m)| (idx, m.tmin));

    if let Some((target, t)) = pick {
        for (i, m) in state.markers.iter_mut().enumerate() {
            m.selected = i == target;
        }
        state.tmin = t - win * 0.5;
        state.tmax = t + win * 0.5;
    }
}

/// Tab/Shift+Tab cycle through markers in time order, selecting one at a time
/// and recentering when it falls outside the view (snuffler's Tab/Backtab).
fn cycle_selection(state: &mut AppState, forward: bool) {
    if state.markers.is_empty() {
        return;
    }
    // (index, tmin) pairs sorted in time order — avoids indexing back into markers.
    let mut order: Vec<(usize, f64)> = state
        .markers
        .iter()
        .enumerate()
        .map(|(i, m)| (i, m.tmin))
        .collect();
    order.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    if !forward {
        order.reverse();
    }
    let last_selected_pos = order
        .iter()
        .enumerate()
        .filter_map(|(pos, entry)| {
            let selected = state.markers.get(entry.0).is_some_and(|m| m.selected);
            selected.then_some(pos)
        })
        .max();
    let next_pos = last_selected_pos.map_or(0, |pos| pos + 1);
    let Some(target) = order.get(next_pos).map(|entry| entry.0) else {
        return;
    };

    let mut tgo = None;
    for (i, m) in state.markers.iter_mut().enumerate() {
        m.selected = i == target;
        if m.selected {
            tgo = Some(m.tmin);
        }
    }
    if let Some(t) = tgo
        && !(state.tmin < t && t < state.tmax)
    {
        let win = (state.tmax - state.tmin).max(1e-9);
        state.tmin = t - win * 0.5;
        state.tmax = t + win * 0.5;
    }
}
