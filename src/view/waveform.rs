//! The waveform canvas: vertically stacked tracks, a shared adaptive time axis,
//! NSLC labels, snuffler-style amplitude scaling, an optional waterfall view,
//! markers, and mouse/keyboard interaction.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "rendering maps between f32/f64 pixel & time coordinates and integer \
              sample/track indices; these casts are inherent to the drawing math"
)]

use egui::{
    Align2, Color32, ColorImage, FontId, Key, Rect, Sense, Stroke, TextureHandle, TextureOptions,
    Theme,
};

use crate::bridge::{ViewData, ViewTrack};
use crate::state::{AppState, Colormap, Subsort, ViewDrag, ViewMode, WiggleTheme};
use crate::view::draw::{DrawTarget, EguiTarget, SvgTarget};
use crate::view::markers::{Marker, PHASE_KEYS};
use crate::view::timeaxis;

const AXIS_H: f32 = 52.0;
const LABEL_PT: f32 = 12.5;
const AXIS_PT: f32 = 12.5;
/// Width of the vertical track scrollbar carved off the right edge.
const SCROLLBAR_W: f32 = 12.0;

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

fn palette(theme: Theme, wiggle: WiggleTheme) -> Palette {
    match (theme, wiggle) {
        (Theme::Dark, WiggleTheme::SnufflerQt) => Palette {
            bg: Color32::from_rgb(12, 15, 18),
            trace: Color32::from_rgb(206, 212, 218),
            grid: Color32::from_rgb(42, 49, 56),
            zero: Color32::from_rgb(32, 37, 43),
            axis_text: Color32::from_rgb(188, 196, 204),
            sep: Color32::from_rgb(48, 55, 62),
            label_bg: Color32::from_rgba_unmultiplied(12, 15, 18, 218),
            cursor: Color32::from_rgba_unmultiplied(255, 255, 255, 55),
        },
        (Theme::Light, WiggleTheme::SnufflerQt) => Palette {
            bg: Color32::from_rgb(255, 255, 255),
            trace: Color32::from_rgb(85, 87, 83),
            grid: Color32::from_rgb(224, 224, 224),
            zero: Color32::from_rgb(236, 236, 236),
            axis_text: Color32::from_rgb(85, 87, 83),
            sep: Color32::from_rgb(214, 214, 214),
            label_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 100),
            cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        },
        (Theme::Dark, WiggleTheme::ColorLines) => Palette {
            bg: Color32::from_rgb(22, 24, 27),
            trace: Color32::from_rgb(222, 224, 228),
            grid: Color32::from_rgb(48, 51, 56),
            zero: Color32::from_rgb(40, 43, 48),
            axis_text: Color32::from_rgb(196, 200, 206),
            sep: Color32::from_rgb(54, 57, 62),
            label_bg: Color32::from_rgba_unmultiplied(16, 18, 20, 205),
            cursor: Color32::from_rgba_unmultiplied(255, 255, 255, 50),
        },
        (Theme::Light, WiggleTheme::ColorLines) => Palette {
            bg: Color32::from_rgb(250, 250, 250),
            trace: Color32::from_rgb(45, 48, 52),
            grid: Color32::from_rgb(224, 224, 224),
            zero: Color32::from_rgb(236, 236, 236),
            axis_text: Color32::from_rgb(64, 64, 64),
            sep: Color32::from_rgb(214, 214, 214),
            label_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 216),
            cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        },
        (Theme::Dark, WiggleTheme::NightBands) => Palette {
            bg: Color32::from_rgb(6, 12, 16),
            trace: Color32::from_rgb(213, 230, 226),
            grid: Color32::from_rgb(28, 45, 52),
            zero: Color32::from_rgb(21, 34, 40),
            axis_text: Color32::from_rgb(176, 206, 207),
            sep: Color32::from_rgb(35, 56, 64),
            label_bg: Color32::from_rgba_unmultiplied(6, 12, 16, 225),
            cursor: Color32::from_rgba_unmultiplied(137, 230, 211, 72),
        },
        (Theme::Light, WiggleTheme::NightBands) => Palette {
            bg: Color32::from_rgb(248, 250, 247),
            trace: Color32::from_rgb(39, 58, 62),
            grid: Color32::from_rgb(216, 225, 219),
            zero: Color32::from_rgb(230, 236, 231),
            axis_text: Color32::from_rgb(52, 76, 78),
            sep: Color32::from_rgb(202, 215, 207),
            label_bg: Color32::from_rgba_unmultiplied(248, 250, 247, 220),
            cursor: Color32::from_rgba_unmultiplied(0, 76, 72, 44),
        },
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
    let img = build_image(view, t, style)?;
    Some(ctx.load_texture(format!("wf{idx}"), img, TextureOptions::NEAREST))
}

/// Rasterize one track's waterfall cells into an RGBA image (the texture source
/// for the egui view, and the embedded PNG for SVG export).
#[allow(
    clippy::indexing_slicing,
    reason = "index provably in bounds: data.len() >= nf*nt is checked above, and \
              c<nt, f<nf so c*nf+f < nf*nt; rgba is sized nt*nf*4 and p+3 < that bound"
)]
fn build_image(view: &ViewData, t: &ViewTrack, style: WfStyle) -> Option<ColorImage> {
    if t.nf == 0 || t.nt == 0 {
        return None;
    }
    let (nf, nt) = (t.nf as usize, t.nt as usize);
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
            let f = nf - 1 - r;
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
    Some(ColorImage::from_rgba_unmultiplied([nt, nf], &rgba))
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
/// to the full label when that element is empty (e.g. blank location codes), so
/// the default colouring stays varied like snuffler's.
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
    let key = if key.is_empty() { &track.label } else { key };
    let hash = key.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(131).wrapping_add(u32::from(byte))
    });
    let (red, green, blue) = PALETTE
        .get(hash as usize % PALETTE.len())
        .copied()
        .unwrap_or((0x88, 0x88, 0x88));
    Color32::from_rgb(red, green, blue)
}

fn row_tint(color: Color32, theme: Theme, wiggle: WiggleTheme) -> Color32 {
    let (target, keep) = match (theme, wiggle) {
        (Theme::Light, WiggleTheme::SnufflerQt) => (255.0, 0.30),
        (Theme::Dark, WiggleTheme::SnufflerQt) => (17.0, 0.36),
        (Theme::Light, WiggleTheme::NightBands) => (248.0, 0.26),
        (Theme::Dark, WiggleTheme::NightBands) => (11.0, 0.48),
        (Theme::Light, WiggleTheme::ColorLines) => (250.0, 0.0),
        (Theme::Dark, WiggleTheme::ColorLines) => (24.0, 0.0),
    };
    let mix = |channel: u8| {
        let value = f32::from(channel) * keep + target * (1.0 - keep);
        value.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()))
}

fn qt_box_style(index: usize) -> (Color32, Color32) {
    type Rgb = (u8, u8, u8);
    const STYLES: [(Rgb, Rgb); 7] = [
        ((252, 175, 62), (206, 92, 0)),
        ((114, 159, 207), (32, 74, 135)),
        ((252, 233, 79), (196, 160, 0)),
        ((138, 226, 52), (78, 154, 6)),
        ((233, 185, 110), (143, 89, 2)),
        ((173, 127, 168), (92, 53, 102)),
        ((239, 41, 41), (164, 0, 0)),
    ];
    let (fill, frame) = STYLES
        .get(index % STYLES.len())
        .copied()
        .unwrap_or(((128, 128, 128), (64, 64, 64)));
    (
        Color32::from_rgba_unmultiplied(fill.0, fill.1, fill.2, 100),
        Color32::from_rgb(frame.0, frame.1, frame.2),
    )
}

fn theme_uses_row_tints(theme: WiggleTheme) -> bool {
    matches!(theme, WiggleTheme::NightBands)
}

fn theme_uses_colored_strokes(theme: WiggleTheme) -> bool {
    matches!(theme, WiggleTheme::ColorLines)
}

fn track_time_bounds(view: &ViewData, track: &ViewTrack) -> Option<(f64, f64)> {
    if track.nf > 0 && track.nt > 0 {
        return Some((
            track.t0_rel,
            track.t0_rel + track.nt.saturating_sub(1) as f64 * track.dt,
        ));
    }
    if track.dt > 0.0 && track.count > 0 {
        return Some((
            track.t0_rel,
            track.t0_rel + track.count.saturating_sub(1) as f64 * track.dt,
        ));
    }
    if track.dt == 0.0 && track.count > 0 {
        let start = (track.offset as usize).min(view.ys.len());
        let end =
            ((track.offset as usize) + (track.count as usize).saturating_mul(2)).min(view.ys.len());
        return view.ys.get(start..end).and_then(|points| {
            let mut chunks = points.chunks_exact(2);
            let [first_t, _] = chunks.next()? else {
                return None;
            };
            let mut tmin = f64::from(*first_t);
            let mut tmax = tmin;
            for pair in chunks {
                let [pt, _] = pair else { continue };
                let t = f64::from(*pt);
                tmin = tmin.min(t);
                tmax = tmax.max(t);
            }
            Some((track.t0_rel + tmin, track.t0_rel + tmax))
        });
    }
    None
}

/// Geometry of one waveform frame, derived from the view + canvas rect. Shared by
/// the on-screen path and the SVG export so both lay tracks out identically.
struct Layout<'a> {
    rect: Rect,
    track_area: Rect,
    view_tmin: f64,
    window: f64,
    visible: &'a [ViewTrack],
    off: usize,
    n_shown: usize,
    row_h: f32,
    /// Y of the top edge of the first visible row. Equals `track_area.top()` when
    /// rows are snapped; sits above it when continuous scrolling exposes a partial
    /// first row.
    top0: f32,
}

/// Vertical track layout derived from the user controls. `top_shift` is how far
/// the first visible row's top edge sits above the track area (always `0` when
/// snapped); `height`/`scroll` are the resolved (sentinel- and clamp-corrected)
/// values that [`show`] writes back into [`AppState`].
struct TrackGeom {
    row_h: f32,
    off: usize,
    n_draw: usize,
    top_shift: f32,
    scroll: f32,
    height: f32,
}

/// Resolve the track-height / snap / scroll controls into concrete row geometry.
/// Pure: callers (the live view and the SVG export) get identical results, and
/// [`show`] additionally writes the resolved values back as the next view
/// request's track window.
fn track_geometry(state: &AppState, total: usize, area_h: f32) -> TrackGeom {
    let area = area_h.max(1.0);
    let total_f = total.max(1) as f32;
    let mut height = if state.track_height > 0.0 {
        state.track_height
    } else {
        area / crate::state::DEFAULT_NTRACKS_SHOWN as f32
    };
    height = height.clamp(
        crate::state::MIN_TRACK_H,
        area.max(crate::state::MIN_TRACK_H),
    );

    if state.snap_tracks {
        // Whole tracks that exactly tile the area (snuffler's classic layout). The
        // rendered offset snaps to a whole track, but `scroll` is kept fractional
        // so a slow scrollbar drag accumulates instead of being rounded away each
        // frame (which would otherwise leave it stuck below the ±0.5 step).
        let n = (area / height).round().clamp(1.0, total_f) as usize;
        let row_h = area / n as f32;
        let max_off = total.saturating_sub(n);
        let scroll = state.track_scroll.clamp(0.0, max_off as f32);
        let off = (scroll.round().clamp(0.0, max_off as f32)) as usize;
        let n_draw = n.min(total.saturating_sub(off));
        TrackGeom {
            row_h,
            off,
            n_draw,
            top_shift: 0.0,
            scroll,
            height: row_h,
        }
    } else {
        let row_h = height;
        let visible_f = area / row_h;
        let max_scroll = (total_f - visible_f).max(0.0);
        let scroll = state.track_scroll.clamp(0.0, max_scroll);
        let off = scroll.floor() as usize;
        let top_shift = (scroll - off as f32) * row_h;
        let n_draw = ((top_shift + area) / row_h).ceil() as usize;
        let n_draw = n_draw.min(total.saturating_sub(off));
        TrackGeom {
            row_h,
            off,
            n_draw,
            top_shift,
            scroll,
            height,
        }
    }
}

fn compute_layout<'a>(state: &AppState, view: &'a ViewData, rect: Rect) -> Layout<'a> {
    let meta = &view.meta;
    let track_area = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - AXIS_H));
    let window = (meta.view_tmax - meta.view_tmin).max(1e-9);
    let total = meta.tracks.len();
    let g = track_geometry(state, total, track_area.height());
    let visible = meta
        .tracks
        .get(g.off..(g.off + g.n_draw).min(total))
        .unwrap_or(&[]);
    Layout {
        rect,
        track_area,
        view_tmin: meta.view_tmin,
        window,
        visible,
        off: g.off,
        n_shown: g.n_draw,
        row_h: g.row_h,
        top0: track_area.top() - g.top_shift,
    }
}

/// Render the persistent waveform content (everything except the transient hover
/// crosshair) to any [`DrawTarget`]. This is the snuffler `drawit` analogue: the
/// egui screen and the SVG export drive the identical geometry through it.
#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw routine; splitting would obscure the layered draw order"
)]
fn paint(
    target: &mut dyn DrawTarget,
    state: &AppState,
    view: &ViewData,
    pal: &Palette,
    lay: &Layout,
    theme: Theme,
    waterfall: bool,
) {
    let rect = lay.rect;
    let track_area = lay.track_area;
    let view_tmin = lay.view_tmin;
    let window = lay.window;
    let visible = lay.visible;
    let row_h = lay.row_h;
    let top0 = lay.top0;
    let meta = &view.meta;
    let x_of = |t_rel: f64| rect.left() + (t_rel / window) as f32 * rect.width();

    target.fill_rect(rect, 0.0, pal.bg);

    if state.show_boxes && !waterfall && state.wiggle_theme == WiggleTheme::SnufflerQt {
        target.push_clip(track_area);
        for (k, t) in visible.iter().enumerate() {
            let Some((t0, t1)) = track_time_bounds(view, t) else {
                continue;
            };
            let top = top0 + k as f32 * row_h + row_h * 0.05;
            let bot = top0 + k as f32 * row_h + row_h * 0.95;
            let x0 = x_of(t0).clamp(track_area.left(), track_area.right());
            let x1 = x_of(t1).clamp(track_area.left(), track_area.right());
            if x1 <= x0 || bot <= top {
                continue;
            }
            let (fill, frame) = qt_box_style(lay.off + k);
            let r = Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, bot));
            target.fill_rect(r, 0.0, fill);
            target.line(r.left_top(), r.right_top(), 1.0, frame);
            target.line(r.right_top(), r.right_bottom(), 1.0, frame);
            target.line(r.right_bottom(), r.left_bottom(), 1.0, frame);
            target.line(r.left_bottom(), r.left_top(), 1.0, frame);
        }
        target.pop_clip();
    } else if state.color_traces && !waterfall && theme_uses_row_tints(state.wiggle_theme) {
        target.push_clip(track_area);
        for (k, t) in visible.iter().enumerate() {
            let top = top0 + k as f32 * row_h;
            let fill = row_tint(trace_color(t, state.subsort), theme, state.wiggle_theme);
            target.fill_rect(
                Rect::from_min_max(
                    egui::pos2(track_area.left(), top),
                    egui::pos2(track_area.right(), top + row_h),
                ),
                0.0,
                fill,
            );
        }
        target.pop_clip();
    }

    // time grid + axis labels
    let tinc = timeaxis::nice_tinc(window, f64::from((rect.width() / 90.0).max(2.0)));
    let axis_top = track_area.bottom();
    let tick_label_y = axis_top + 4.0;
    let context_label_y = axis_top + 23.0;
    let axis_pad = 10.0;
    for tick in timeaxis::ticks(view_tmin, meta.view_tmax, tinc) {
        let x = x_of(tick - view_tmin);
        target.line(
            egui::pos2(x, track_area.top()),
            egui::pos2(x, track_area.bottom()),
            1.0,
            pal.grid,
        );
        let label = timeaxis::tick_label(tick, tinc);
        let w = target.measure(&label, AXIS_PT).x;
        if rect.left() + axis_pad < x - w * 0.5 && x + w * 0.5 < rect.right() - axis_pad {
            target.text(
                egui::pos2(x, tick_label_y),
                Align2::CENTER_TOP,
                &label,
                AXIS_PT,
                pal.axis_text,
            );
        }
    }
    target.text(
        egui::pos2(rect.left() + axis_pad, context_label_y),
        Align2::LEFT_TOP,
        &timeaxis::context_label(view_tmin),
        AXIS_PT,
        pal.axis_text,
    );

    // tracks (clipped to the track area so a partially-scrolled first/last row
    // never bleeds over the time axis or the top edge)
    target.push_clip(track_area);
    for (k, t) in visible.iter().enumerate() {
        let top = top0 + k as f32 * row_h;
        let bot = top + row_h;
        let mid = top + row_h * 0.5;

        if state.show_boxes && (state.wiggle_theme != WiggleTheme::SnufflerQt || waterfall) {
            let r = Rect::from_min_max(
                egui::pos2(rect.left() + 0.5, top + 1.0),
                egui::pos2(rect.right() - 0.5, bot - 1.0),
            );
            target.line(r.left_top(), r.right_top(), 1.0, pal.sep);
            target.line(r.right_top(), r.right_bottom(), 1.0, pal.sep);
            target.line(r.right_bottom(), r.left_bottom(), 1.0, pal.sep);
            target.line(r.left_bottom(), r.left_top(), 1.0, pal.sep);
        } else if !state.show_boxes && k > 0 {
            target.line(
                egui::pos2(rect.left(), top),
                egui::pos2(rect.right(), top),
                1.0,
                pal.sep,
            );
        }

        if waterfall && t.nf > 0 {
            let x0 = x_of(t.t0_rel);
            let x1 = x_of(t.t0_rel + t.nt as f64 * t.dt);
            let r =
                Rect::from_min_max(egui::pos2(x0, top + 1.0), egui::pos2(x1, top + row_h - 1.0));
            target.waterfall(r, lay.off + k);
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
            target.push_clip(Rect::from_min_max(
                egui::pos2(track_area.left(), cl_top),
                egui::pos2(track_area.right(), cl_bot),
            ));
            let y_of = |v: f32| mid - (v - t.ymean) * amp;

            if state.show_zero_lines {
                let yz = y_of(0.0);
                target.line(
                    egui::pos2(track_area.left(), yz),
                    egui::pos2(track_area.right(), yz),
                    1.0,
                    pal.zero,
                );
            }

            let trace_col = if state.color_traces && theme_uses_colored_strokes(state.wiggle_theme)
            {
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
                    target.polyline(points, 1.0, trace_col);
                } else if let Some(&v) = samples.first() {
                    let t_rel = t.t0_rel;
                    let half = 0.5 * t.dt;
                    let x0 = x_of(t_rel - half);
                    let x1 = x_of(t_rel + half);
                    target.line(
                        egui::pos2(x0, y_of(v)),
                        egui::pos2(x1, y_of(v)),
                        1.0,
                        trace_col,
                    );
                }
            } else if t.dt == 0.0 {
                let start = (t.offset as usize).min(view.ys.len());
                let end =
                    ((t.offset as usize) + (t.count as usize).saturating_mul(2)).min(view.ys.len());
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
                    target.polyline(points, 1.0, trace_col);
                }
            } else if t.dt < 0.0 {
                let start = (t.offset as usize).min(view.ys.len());
                let end =
                    ((t.offset as usize) + (t.count as usize).saturating_mul(3)).min(view.ys.len());
                if let Some(envelope) = view.ys.get(start..end)
                    && envelope.len() >= 3
                {
                    for triple in envelope.chunks_exact(3) {
                        let [t0, ylo, yhi] = triple else {
                            continue;
                        };
                        let x = x_of(t.t0_rel + f64::from(*t0));
                        target.line(
                            egui::pos2(x, y_of(*ylo)),
                            egui::pos2(x, y_of(*yhi)),
                            1.0,
                            trace_col,
                        );
                    }
                }
            }
            target.pop_clip();

            if state.show_scale_axes {
                let xa = rect.right() - 2.0;
                target.line(
                    egui::pos2(xa, top + 3.0),
                    egui::pos2(xa, bot - 3.0),
                    1.0,
                    pal.sep,
                );
            }
            if state.show_scale_ranges {
                target.text(
                    egui::pos2(rect.right() - 6.0, top + 3.0),
                    Align2::RIGHT_TOP,
                    &format!("{:+.2e}", t.ymax),
                    10.0,
                    pal.axis_text,
                );
                target.text(
                    egui::pos2(rect.right() - 6.0, bot - 3.0),
                    Align2::RIGHT_BOTTOM,
                    &format!("{:+.2e}", t.ymin),
                    10.0,
                    pal.axis_text,
                );
            }
        }

        // NSLC label with a translucent background
        let label_pos = egui::pos2(track_area.left() + 5.0, top + 3.0);
        let bg = Rect::from_min_size(label_pos, target.measure(&t.label, LABEL_PT)).expand(3.0);
        if !state.color_traces || !theme_uses_row_tints(state.wiggle_theme) {
            target.fill_rect(bg, 3.0, pal.label_bg);
        }
        target.text(
            label_pos,
            Align2::LEFT_TOP,
            &t.label,
            LABEL_PT,
            pal.axis_text,
        );
    }
    target.pop_clip();

    target.push_clip(track_area);
    draw_markers(
        target, state, track_area, top0, visible, row_h, view_tmin, &x_of,
    );
    if let Some((a, b)) = state.pending_span {
        let r = Rect::from_min_max(
            egui::pos2(x_of(a - view_tmin), track_area.top()),
            egui::pos2(x_of(b - view_tmin), track_area.bottom()),
        );
        target.fill_rect(r, 0.0, Color32::from_rgba_unmultiplied(52, 101, 164, 40));
    }
    target.pop_clip();
}

/// Render the current wiggle/waterfall view to a standalone SVG string at the
/// given pixel size, reusing the exact [`paint`] geometry (vector traces/axes,
/// the waterfall heatmap embedded as a PNG).
pub fn render_svg(
    ctx: &egui::Context,
    state: &AppState,
    view: &ViewData,
    size: egui::Vec2,
    theme: Theme,
) -> String {
    let pal = palette(theme, state.wiggle_theme);
    let rect = Rect::from_min_size(egui::Pos2::ZERO, size);
    let lay = compute_layout(state, view, rect);
    let waterfall = state.view_mode == ViewMode::Waterfall;
    let images: Vec<Option<ColorImage>> = if waterfall {
        let style = WfStyle {
            cmap: cmap_index(state.wf_cmap),
            reverse: state.wf_reverse,
            symmetry: state.wf_symmetry,
            clip: state.wf_clip,
        };
        view.meta
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (i >= lay.off && i < lay.off + lay.n_shown)
                    .then(|| build_image(view, t, style))
                    .flatten()
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut target = SvgTarget::new(ctx.clone(), size.x, size.y, images);
    paint(&mut target, state, view, &pal, &lay, theme, waterfall);
    target.finish()
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
    scene_range: Option<(f64, f64)>,
) {
    let theme = ui.ctx().theme();
    let pal = palette(theme, state.wiggle_theme);
    ui.ctx()
        .options_mut(|o| o.tessellation_options.feathering = state.antialiasing);
    // Reserve the whole canvas for the painter; the waveform and the scrollbar
    // each take their own interaction sub-rect so they never fight over drags.
    let (bg, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
    let full_rect = bg.rect;
    let track_area_h = (full_rect.height() - AXIS_H).max(1.0);

    let Some(view) = view else {
        painter.rect_filled(full_rect, 0.0, pal.bg);
        painter.text(
            full_rect.center(),
            Align2::CENTER_CENTER,
            "Loading…",
            FontId::proportional(16.0),
            pal.axis_text,
        );
        handle_keyboard(ui, state, 0, track_area_h, scene_range);
        return;
    };

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

    let total = view.meta.tracks.len();
    handle_keyboard(ui, state, total, track_area_h, scene_range);

    // Resolve the controls into row geometry and cache the on-screen window so the
    // next view request fetches exactly the visible (possibly partial) tracks.
    let g = track_geometry(state, total, track_area_h);
    state.track_height = g.height;
    state.track_scroll = g.scroll;
    state.track_offset = g.off;
    state.ntracks_shown = g.n_draw.max(1);

    let overflow = (total as f32) > track_area_h / g.row_h.max(1.0) + 1e-3;
    let sb_w = if overflow { SCROLLBAR_W } else { 0.0 };
    let wave_rect = Rect::from_min_max(
        full_rect.min,
        egui::pos2(full_rect.max.x - sb_w, full_rect.max.y),
    );
    let rect = wave_rect;
    let wave_resp = ui.interact(wave_rect, bg.id.with("wave"), Sense::click_and_drag());

    let lay = compute_layout(state, view, wave_rect);
    let mut target = EguiTarget::new(painter.clone(), &wf.textures);
    paint(&mut target, state, view, &pal, &lay, theme, waterfall);

    if overflow {
        let strip = Rect::from_min_max(
            egui::pos2(wave_rect.max.x, lay.track_area.top()),
            egui::pos2(full_rect.max.x, lay.track_area.bottom()),
        );
        draw_scrollbar(ui, bg.id.with("scrollbar"), strip, state, total, lay.row_h);
    }

    // Transient hover crosshair — drawn only on screen, never in an export.
    if let Some(p) = wave_resp.hover_pos()
        && lay.track_area.contains(p)
    {
        if marker_hit_at(
            state,
            lay.visible,
            lay.track_area,
            lay.top0,
            lay.row_h,
            lay.view_tmin,
            lay.window,
            lay.rect,
            p,
        )
        .is_some()
        {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
        painter.line_segment(
            [
                egui::pos2(p.x, lay.track_area.top()),
                egui::pos2(p.x, lay.track_area.bottom()),
            ],
            Stroke::new(1.0_f32, pal.cursor),
        );
    }

    let t_at_x = |x: f32| {
        lay.view_tmin + f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * lay.window
    };
    handle_interaction(
        ui,
        &wave_resp,
        state,
        lay.visible,
        lay.top0,
        lay.row_h,
        lay.view_tmin,
        lay.window,
        rect,
        lay.track_area,
        &t_at_x,
    );
}

/// A vertical scrollbar for the track stack, drawn in the reserved right-hand
/// strip. Dragging the thumb or clicking the trough moves `track_scroll`; in
/// snapped mode the next frame's layout rounds the result to whole tracks.
fn draw_scrollbar(
    ui: &egui::Ui,
    id: egui::Id,
    strip: Rect,
    state: &mut AppState,
    total: usize,
    row_h: f32,
) {
    let painter = ui.painter();
    let theme = ui.ctx().theme();
    let total_f = total.max(1) as f32;
    let area = strip.height().max(1.0);
    let fits = (area / row_h.max(1.0)).max(1.0);
    let max_scroll = (total_f - fits).max(0.0);
    if max_scroll <= 0.0 {
        return;
    }

    let resp = ui.interact(strip, id, Sense::click_and_drag());
    let frac_vis = (fits / total_f).clamp(0.05, 1.0);
    let thumb_h = (frac_vis * area).clamp(24.0_f32.min(area), area);

    if resp.dragged() {
        let per_px = max_scroll / (area - thumb_h).max(1.0);
        state.track_scroll =
            (state.track_scroll + resp.drag_delta().y * per_px).clamp(0.0, max_scroll);
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let scroll_frac = (state.track_scroll / max_scroll).clamp(0.0, 1.0);
        let thumb_top = strip.top() + scroll_frac * (area - thumb_h);
        if p.y < thumb_top {
            state.track_scroll = (state.track_scroll - fits).max(0.0);
        } else if p.y > thumb_top + thumb_h {
            state.track_scroll = (state.track_scroll + fits).min(max_scroll);
        }
    }

    let scroll_frac = (state.track_scroll / max_scroll).clamp(0.0, 1.0);
    let thumb_top = strip.top() + scroll_frac * (area - thumb_h);
    let thumb = Rect::from_min_max(
        egui::pos2(strip.left() + 2.0, thumb_top),
        egui::pos2(strip.right() - 2.0, thumb_top + thumb_h),
    );
    let (trough, thumb_col, thumb_hot) = if theme == Theme::Dark {
        (
            Color32::from_gray(38),
            Color32::from_gray(96),
            Color32::from_gray(140),
        )
    } else {
        (
            Color32::from_gray(222),
            Color32::from_gray(168),
            Color32::from_gray(128),
        )
    };
    painter.rect_filled(strip, 0.0, trough);
    let col = if resp.hovered() || resp.dragged() {
        thumb_hot
    } else {
        thumb_col
    };
    painter.rect_filled(thumb, 3.0, col);
}

/// Does an NSLC-scoped marker apply to this track? Empty or `*` fields are
/// wildcards (snuffler's per-channel marker matching).
fn marker_matches_track(nslc: &(String, String, String, String), t: &ViewTrack) -> bool {
    let f = |pat: &str, val: &str| pat.is_empty() || pat == "*" || pat == val;
    f(&nslc.0, &t.net) && f(&nslc.1, &t.sta) && f(&nslc.2, &t.loc) && f(&nslc.3, &t.cha)
}

fn marker_row_spans(
    marker: &Marker,
    area: Rect,
    top0: f32,
    visible: &[ViewTrack],
    row_h: f32,
) -> Vec<(f32, f32)> {
    match &marker.nslc {
        None => vec![(area.top(), area.bottom())],
        Some(nslc) => visible
            .iter()
            .enumerate()
            .filter(|(_, t)| marker_matches_track(nslc, t))
            .map(|(k, _)| {
                let top = top0 + k as f32 * row_h;
                (top, top + row_h)
            })
            .collect(),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "marker hit-testing needs the view + projection context to map a click to a marker"
)]
fn marker_hit_at(
    state: &AppState,
    visible: &[ViewTrack],
    area: Rect,
    top0: f32,
    row_h: f32,
    view_tmin: f64,
    window: f64,
    rect: Rect,
    pos: egui::Pos2,
) -> Option<usize> {
    if !area.contains(pos) {
        return None;
    }
    let x_of = |t: f64| rect.left() + ((t - view_tmin) / window) as f32 * rect.width();
    let tol = 7.0;
    for (idx, marker) in state.markers.iter().enumerate().rev() {
        let row_hit = marker_row_spans(marker, area, top0, visible, row_h)
            .iter()
            .any(|(top, bot)| pos.y >= *top && pos.y <= *bot);
        if !row_hit {
            continue;
        }
        let x0 = x_of(marker.tmin);
        if marker.is_span() {
            let x1 = x_of(marker.tmax);
            let left = x0.min(x1) - tol;
            let right = x0.max(x1) + tol;
            if pos.x >= left && pos.x <= right {
                return Some(idx);
            }
        } else if (pos.x - x0).abs() <= tol {
            return Some(idx);
        }
    }
    None
}

#[allow(
    clippy::too_many_arguments,
    reason = "marker drawing needs the view + projection context to place each marker"
)]
fn draw_markers(
    target: &mut dyn DrawTarget,
    state: &AppState,
    area: Rect,
    top0: f32,
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
        let spans = marker_row_spans(m, area, top0, visible, row_h);
        let label = match (&m.phase, m.polarity_symbol()) {
            (Some(ph), sym) if !sym.is_empty() => format!("{ph} {sym}"),
            (Some(ph), _) => ph.clone(),
            (None, sym) if !sym.is_empty() => sym.to_string(),
            (None, _) => String::new(),
        };
        let w = if m.selected { 4.0 } else { 2.0 };
        for (ytop, ybot) in spans {
            if m.is_span() {
                let x1 = x_of(m.tmax - view_tmin);
                let r = Rect::from_min_max(egui::pos2(x0, ytop), egui::pos2(x1, ybot));
                target.fill_rect(
                    r,
                    0.0,
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 30),
                );
            }
            target.line(egui::pos2(x0, ytop), egui::pos2(x0, ybot), w, col);
            if m.is_event {
                let s = 6.0;
                target.polygon(
                    vec![
                        egui::pos2(x0, ytop + s),
                        egui::pos2(x0 - s, ytop),
                        egui::pos2(x0 + s, ytop),
                    ],
                    col,
                );
            }
            if !label.is_empty() {
                target.text(
                    egui::pos2(x0 + 3.0, ytop + 2.0),
                    Align2::LEFT_TOP,
                    &label,
                    LABEL_PT,
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
    visible: &[ViewTrack],
    top0: f32,
    row_h: f32,
    view_tmin: f64,
    window: f64,
    rect: Rect,
    track_area: Rect,
    t_at_x: &impl Fn(f32) -> f64,
) {
    let modifiers = ui.input(|i| i.modifiers);
    // Plain wheel scrolling arrives via `smooth_scroll_delta`, but egui re-routes
    // ctrl/⌘+wheel into a zoom gesture (zeroing the scroll delta), so qt
    // snuffler's ctrl+wheel track-zoom never fired here. Recover it from the raw
    // wheel events, which keep their real modifiers.
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    let ctrl_wheel = ui.input(raw_ctrl_wheel);
    let area_h = track_area.height();

    if resp.hovered() {
        if ctrl_wheel != 0.0 {
            let anchor = resp.hover_pos().map_or(0.5, |p| {
                ((p.y - track_area.top()) / area_h.max(1.0)).clamp(0.0, 1.0)
            });
            let notches = wheel_notches(state, ctrl_wheel);
            if notches != 0.0 {
                zoom_tracks(state, area_h, anchor, notches);
            }
        } else if scroll != 0.0 {
            if state.snap_tracks {
                let notches = wheel_notches(state, scroll);
                if notches != 0.0 {
                    let visible_n = (area_h / row_h.max(1.0)).max(1.0);
                    let amount = (visible_n / 5.0).max(1.0);
                    state.track_scroll = (state.track_scroll - amount * notches).max(0.0);
                }
            } else {
                // Continuous: scroll by the exact pixel delta, in track units.
                state.track_scroll = (state.track_scroll - scroll / row_h.max(1.0)).max(0.0);
            }
        }
    }

    if resp.drag_started_by(egui::PointerButton::Primary)
        && !modifiers.shift
        && let Some(p) = resp.interact_pointer_pos()
    {
        state.view_drag = Some(ViewDrag {
            x: p.x,
            y: p.y,
            tmin: state.tmin,
            tmax: state.tmax,
            pan_only: p.y >= track_area.bottom(),
        });
    }

    if resp.dragged_by(egui::PointerButton::Primary)
        && !modifiers.shift
        && let (Some(drag), Some(p)) = (state.view_drag, resp.interact_pointer_pos())
    {
        let width = rect.width().max(1.0);
        let height = rect.height().max(1.0);
        let win0 = (drag.tmax - drag.tmin).max(1e-9);
        let dx = f64::from((p.x - drag.x) / width);
        let dy = if drag.pan_only {
            0.0
        } else {
            f64::from((p.y - drag.y) / height)
        };
        let scale = (-dy * 5.0).exp();
        let dtr = scale * win0 - win0;
        let frac = f64::from(((drag.x - rect.left()) / width).clamp(0.0, 1.0));
        let dt = dx * win0 * scale;

        state.tmin = drag.tmin - dt - dtr * frac;
        state.tmax = drag.tmax - dt + dtr * (1.0 - frac);
    }

    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        state.view_drag = None;
    }

    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at_x(p.x);
            state.markers.push(Marker::new(t, t, state.marker_kind));
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(hit) = marker_hit_at(
            state, visible, track_area, top0, row_h, view_tmin, window, rect, p,
        )
    {
        if !modifiers.shift {
            for m in &mut state.markers {
                m.selected = false;
            }
        }
        if let Some(m) = state.markers.get_mut(hit) {
            m.selected = true;
        }
    }
}

/// Sum the vertical delta (in points) of any ctrl/⌘-held raw wheel events this
/// frame, converting line/page units the way egui itself would on the web (its
/// `InputOptions` are private, but the web `line_scroll_speed` is a fixed 8.0).
fn raw_ctrl_wheel(i: &egui::InputState) -> f32 {
    const LINE_PX: f32 = 8.0;
    let page = i.content_rect().height();
    i.events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } if modifiers.ctrl || modifiers.command => Some(match unit {
                egui::MouseWheelUnit::Point => delta.y,
                egui::MouseWheelUnit::Line => LINE_PX * delta.y,
                egui::MouseWheelUnit::Page => page * delta.y,
            }),
            _ => None,
        })
        .sum()
}

/// Accumulate raw wheel motion into Qt-style 120-point notches.
fn wheel_notches(state: &mut AppState, scroll: f32) -> f32 {
    const WHEEL_NOTCH: f32 = 120.0;
    state.wheel_pos += scroll;
    let notches = (state.wheel_pos / WHEEL_NOTCH).floor();
    state.wheel_pos -= notches * WHEEL_NOTCH;
    notches.clamp(-5.0, 5.0)
}

/// The current resolved track height in pixels (sentinel `0.0` auto-fits
/// [`DEFAULT_NTRACKS_SHOWN`]), clamped to the slider bounds.
fn resolved_height(state: &AppState, area_h: f32) -> f32 {
    let area = area_h.max(1.0);
    let h = if state.track_height > 0.0 {
        state.track_height
    } else {
        area / crate::state::DEFAULT_NTRACKS_SHOWN as f32
    };
    h.clamp(
        crate::state::MIN_TRACK_H,
        area.max(crate::state::MIN_TRACK_H),
    )
}

/// ctrl+wheel track zoom: change the track height (and hence how many tracks are
/// shown) while keeping the track under the cursor (`anchor`) fixed. Positive
/// `notches` (wheel down) shrink rows to reveal more tracks.
fn zoom_tracks(state: &mut AppState, area_h: f32, anchor: f32, notches: f32) {
    let area = area_h.max(1.0);
    let cur_h = resolved_height(state, area);
    let cur_n = (area / cur_h).max(1.0);
    let new_n = (cur_n + notches).max(1.0);
    let new_h = (area / new_n).clamp(crate::state::MIN_TRACK_H, crate::state::MAX_TRACK_H);
    let idx_at_cursor = state.track_scroll + anchor.clamp(0.0, 1.0) * cur_n;
    state.track_scroll = (idx_at_cursor - anchor.clamp(0.0, 1.0) * (area / new_h)).max(0.0);
    state.track_height = new_h;
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
fn handle_keyboard(
    ui: &egui::Ui,
    state: &mut AppState,
    total: usize,
    track_area_h: f32,
    scene_range: Option<(f64, f64)>,
) {
    // Don't fire single-key shortcuts while a text field (FDSN forms, command
    // line) has the keyboard — otherwise typing "n" would jump markers.
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    ui.input(|i| {
        let win = (state.tmax - state.tmin).max(1e-9);
        let shift = i.modifiers.shift;
        if i.key_pressed(Key::Q) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if i.key_pressed(Key::F) {
            state.fullscreen = !state.fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(state.fullscreen));
        }
        if i.key_pressed(Key::Space) {
            state.tmin += win;
            state.tmax += win;
        }
        if i.key_pressed(Key::B) {
            state.tmin -= win;
            state.tmax -= win;
        }
        // Track-zoom keys (snuffler: + shows one track MORE, - one LESS, = resets
        // to the initial count). These now drive the track height: more tracks =
        // shorter rows. Plus is Shift+Equals on most layouts, so accept either for
        // "more"; bare Equals (no shift) is the reset (auto-fit sentinel).
        let cur_h = resolved_height(state, track_area_h);
        let cur_n = (track_area_h.max(1.0) / cur_h).round().max(1.0);
        if i.key_pressed(Key::Plus) || (i.key_pressed(Key::Equals) && shift) {
            state.track_height = (track_area_h.max(1.0) / (cur_n + 1.0))
                .clamp(crate::state::MIN_TRACK_H, crate::state::MAX_TRACK_H);
        }
        if i.key_pressed(Key::Minus) {
            state.track_height = (track_area_h.max(1.0) / (cur_n - 1.0).max(1.0))
                .clamp(crate::state::MIN_TRACK_H, crate::state::MAX_TRACK_H);
        }
        if i.key_pressed(Key::Equals) && !shift {
            state.track_height = 0.0;
        }
        if i.key_pressed(Key::PageDown) {
            let max_scroll = (total as f32 - cur_n).max(0.0);
            state.track_scroll = (state.track_scroll + cur_n).min(max_scroll);
        }
        if i.key_pressed(Key::PageUp) {
            state.track_scroll = (state.track_scroll - cur_n).max(0.0);
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
        if i.key_pressed(Key::D) {
            for m in &mut state.markers {
                m.selected = false;
            }
        }
        if i.key_pressed(Key::G) {
            go_to_selection(state, shift, scene_range); // Shift+G: tight
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
            if i.key_pressed(*key) && n == 9 {
                for m in &mut state.markers {
                    if m.selected {
                        m.phase = None;
                        m.polarity = None;
                    }
                }
            } else if i.key_pressed(*key)
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

/// Recenter the view on selected markers, or on the full scene if nothing is
/// selected (snuffler's `go_to_selection` fallback).
fn go_to_selection(state: &mut AppState, tight: bool, scene_range: Option<(f64, f64)>) {
    let sel: Vec<&Marker> = state.markers.iter().filter(|m| m.selected).collect();
    let (tmin, tmax) = if sel.is_empty() {
        let Some(range) = scene_range else {
            return;
        };
        range
    } else {
        (
            sel.iter().map(|m| m.tmin).fold(f64::INFINITY, f64::min),
            sel.iter().map(|m| m.tmax).fold(f64::NEG_INFINITY, f64::max),
        )
    };
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
