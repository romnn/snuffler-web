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

use std::cell::Cell;
use std::collections::HashMap;

use egui::{
    Align2, Color32, ColorImage, FontId, Key, Rect, Sense, Stroke, TextureHandle, TextureOptions,
};

use crate::view::draw::{EguiTarget, RasterImage, SvgTarget};
use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};
use snuffler_web_core::markers::{Marker, PHASE_KEYS};
use snuffler_web_core::state::{
    ActiveEventReq, AppState, DEFAULT_NTRACKS_SHOWN, MAX_TRACK_H, MIN_TRACK_H, PickingState,
    ViewDrag, ViewMode,
};
use snuffler_web_render::layout::{AXIS_H, compute_layout, track_geometry};
use snuffler_web_render::paint::{marker_row_spans, paint};
use snuffler_web_render::palette::{Theme, palette};

/// Width of the vertical track scrollbar carved off the right edge.
const SCROLLBAR_W: f32 = 12.0;
const PAINT_PERF_LOG_INTERVAL_MS: f64 = 1_000.0;
const PAINT_PERF_SLOW_MS: f64 = 8.0;

thread_local! {
    static LAST_PAINT_PERF_LOG_MS: Cell<f64> = const { Cell::new(0.0) };
}

/// Per-frame cache of waterfall textures, rebuilt only when the view data epoch
/// changes. The pixels are coloured in Python (matplotlib) and packed into the
/// `ys` buffer, so any colormap/clip change re-fetches the view (new epoch) — the
/// frontend keeps no colormap state of its own.
#[derive(Default)]
pub struct WaterfallCache {
    epoch: u64,
    /// Keyed by the span's payload offset: a track can carry one strip per segment
    /// of its channel, so a row index would not identify one.
    textures: HashMap<u64, TextureHandle>,
}

impl WaterfallCache {
    fn ensure(&mut self, ctx: &egui::Context, view: &ViewData, epoch: u64) {
        if self.epoch == epoch && !self.textures.is_empty() {
            return;
        }
        self.epoch = epoch;
        self.textures = view
            .meta
            .tracks
            .iter()
            .flat_map(|t| t.spans.iter().map(move |span| (t, span)))
            .filter_map(|(t, span)| {
                build_texture(ctx, view, t, span).map(|texture| (span.offset, texture))
            })
            .collect();
    }
}

fn build_texture(
    ctx: &egui::Context,
    view: &ViewData,
    t: &ViewTrack,
    span: &ViewSpan,
) -> Option<TextureHandle> {
    let img = build_image(view, t, span)?;
    let key = span.offset;
    Some(ctx.load_texture(format!("wf{key}"), img, TextureOptions::NEAREST))
}

/// Converts the shared raw waterfall image into an egui texture source.
fn build_image(view: &ViewData, t: &ViewTrack, span: &ViewSpan) -> Option<ColorImage> {
    let image = snuffler_web_render::waterfall::build_image(view, t, span)?;
    let size = [
        usize::try_from(image.width).ok()?,
        usize::try_from(image.height).ok()?,
    ];
    Some(ColorImage::from_rgba_unmultiplied(size, &image.rgba))
}

/// Counts the decoded points sent through the painter for performance logging.
fn paint_payload_points(visible: &[ViewTrack]) -> usize {
    visible
        .iter()
        .filter(|track| track.nf == 0)
        .flat_map(|track| track.spans.iter())
        .map(|span| {
            let count = usize::try_from(span.count).unwrap_or(usize::MAX);
            if span.dt < 0.0 {
                count.saturating_mul(2)
            } else {
                count
            }
        })
        .sum::<usize>()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "diagnostic counters are logged as f64 for compact timing output"
)]
fn maybe_log_paint_perf(
    waterfall: bool,
    total_ms: f64,
    view: &ViewData,
    visible: &[ViewTrack],
    row_h: f32,
    rect: Rect,
) {
    let now = js_sys::Date::now();
    let should_log = total_ms >= PAINT_PERF_SLOW_MS
        || LAST_PAINT_PERF_LOG_MS.with(|last| {
            let elapsed = now - last.get();
            if elapsed >= PAINT_PERF_LOG_INTERVAL_MS {
                last.set(now);
                true
            } else {
                false
            }
        });
    if !should_log {
        return;
    }
    let points = paint_payload_points(visible);
    log::info!(
        "[paint] mode={} total_ms={:.2} visible_tracks={} payload_points={} ys={} row_h={:.1} rect={:.0}x{:.0}",
        if waterfall { "waterfall" } else { "wiggle" },
        total_ms,
        visible.len(),
        points,
        view.ys.len(),
        row_h,
        rect.width(),
        rect.height(),
    );
}

pub fn render_svg(
    ctx: &egui::Context,
    state: &AppState,
    view: &ViewData,
    size: egui::Vec2,
    theme: egui::Theme,
) -> String {
    let theme = Theme::from_dark(theme == egui::Theme::Dark);
    let pal = palette(theme, state.wiggle_theme);
    let rect = Rect::from_min_size(egui::Pos2::ZERO, size);
    let lay = compute_layout(state, view, rect);
    let waterfall = state.view_mode == ViewMode::Waterfall;
    let images: HashMap<u64, RasterImage> = if waterfall {
        view.meta
            .tracks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= lay.off && *i < lay.off + lay.n_shown)
            .flat_map(|(_, t)| t.spans.iter().map(move |span| (t, span)))
            .filter_map(|(t, span)| {
                snuffler_web_render::waterfall::build_image(view, t, span)
                    .map(|image| (span.offset, image))
            })
            .collect()
    } else {
        HashMap::new()
    };
    let measure_ctx = ctx.clone();
    let mut target = SvgTarget::new(size.x, size.y, images, move |text, size| {
        measure_ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::WHITE)
                .size()
        })
    });
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
) -> egui::Response {
    let theme = Theme::from_dark(ui.ctx().theme() == egui::Theme::Dark);
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
        // Still return a click-sensing response so an empty/loading canvas is
        // right-clickable (Qt's context menu works with no data too).
        return ui.interact(full_rect, bg.id.with("wave"), Sense::click_and_drag());
    };

    let waterfall = state.view_mode == ViewMode::Waterfall;
    if waterfall {
        wf.ensure(ui.ctx(), view, view_epoch);
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
    let paint_t0 = js_sys::Date::now();
    paint(&mut target, state, view, &pal, &lay, theme, waterfall);
    maybe_log_paint_perf(
        waterfall,
        js_sys::Date::now() - paint_t0,
        view,
        lay.visible,
        lay.row_h,
        wave_rect,
    );

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
    wave_resp
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
    let theme = Theme::from_dark(ui.ctx().theme() == egui::Theme::Dark);
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

/// Finds the topmost visible marker under a pointer position.
#[expect(
    clippy::too_many_arguments,
    reason = "marker hit-testing needs the view and projection context to map a click to a marker"
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
        if !state.marker_kind_visible(marker.kind) {
            continue;
        }
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
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "pointer interaction combines pan, zoom, marker picking, and uncertainty dragging against one projection context"
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

    // Batch/interactive picking (Escape). A floating pick follows the cursor;
    // press → (drag) → release commits a marker (instant if no drag), and batch
    // mode re-arms a new pick inheriting kind/phase/polarity. The wheel-scroll
    // block above stays live so the user can scroll stations mid-batch; pan/zoom,
    // place-on-double-click and marker-select below are suppressed while picking.
    if state.picking.is_some() {
        let (pressed, released, ipos) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
            )
        });
        let cursor = resp.hover_pos().or(ipos);
        let track_nslc = |y: f32| -> Option<(String, String, String, String)> {
            let rel = y - top0;
            if rel < 0.0 || row_h <= 0.0 {
                return None;
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rel>=0 and row_h>0, so the row index is a non-negative whole number; \
                          truncation to the containing row is the intended mapping"
            )]
            let idx = (rel / row_h) as usize;
            visible
                .get(idx)
                .map(|t| (t.net.clone(), t.sta.clone(), t.loc.clone(), t.cha.clone()))
        };
        if let Some(pk) = state.picking.as_mut() {
            if pressed && let Some(p) = ipos {
                let t = t_at_x(p.x);
                pk.down_t = Some(t);
                pk.tmin = t;
                pk.tmax = t;
                pk.nslc = track_nslc(p.y);
            } else if let Some(down) = pk.down_t {
                if let Some(p) = cursor {
                    let cur = t_at_x(p.x);
                    pk.tmin = down.min(cur);
                    pk.tmax = down.max(cur);
                }
            } else if let Some(p) = cursor {
                let t = t_at_x(p.x);
                pk.tmin = t;
                pk.tmax = t;
                pk.nslc = track_nslc(p.y);
            }
        }
        if released
            && let Some(pk) = state.picking.clone()
            && pk.down_t.is_some()
        {
            let rel_nslc = cursor.and_then(|p| track_nslc(p.y)).or(pk.nslc.clone());
            let mut m = Marker::new(pk.tmin, pk.tmax, pk.kind);
            m.phase.clone_from(&pk.phase);
            m.polarity = pk.polarity;
            m.nslc = rel_nslc;
            m.selected = true;
            for existing in &mut state.markers {
                existing.selected = false;
            }
            state.markers.push(m);
            state.picking = pk.batch.then(|| PickingState {
                batch: true,
                down_t: None,
                tmin: pk.tmin,
                tmax: pk.tmin,
                kind: pk.kind,
                phase: pk.phase.clone(),
                polarity: pk.polarity,
                nslc: None,
            });
        }
        return;
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

    // Shift+drag adjusts the time uncertainty of selected phase markers, drawn as
    // ± brackets (snuffler's set_uncertainty_y). Latched at press so it survives a
    // mid-drag shift release; the horizontal seconds-per-pixel is deliberately
    // applied to the vertical drag, exactly like pyrocko.
    if resp.drag_started_by(egui::PointerButton::Primary)
        && modifiers.shift
        && let Some(p) = resp.interact_pointer_pos()
    {
        if let Some(hit) = marker_hit_at(
            state, visible, track_area, top0, row_h, view_tmin, window, rect, p,
        ) && let Some(m) = state.markers.get_mut(hit)
        {
            m.selected = true;
        }
        state.uncertainty_drag_last_y = Some(p.y);
    }

    if let Some(last_y) = state.uncertainty_drag_last_y
        && resp.dragged_by(egui::PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let sec_per_px = window / f64::from(rect.width().max(1.0));
        let delta = f64::from(p.y - last_y) * sec_per_px;
        for m in &mut state.markers {
            if m.selected && m.phase.is_some() {
                m.uncertainty = Some((m.uncertainty.unwrap_or(0.0) + delta).max(0.0));
            }
        }
        state.uncertainty_drag_last_y = Some(p.y);
    }

    if resp.dragged_by(egui::PointerButton::Primary)
        && !modifiers.shift
        && state.uncertainty_drag_last_y.is_none()
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
        state.uncertainty_drag_last_y = None;
    }

    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at_x(p.x);
            // Selected, and the only one selected — snuffler's `stop_picking` ends
            // with `floating_marker.selected = True` after a `deselect_all`, so the
            // marker just placed is the one every following keystroke acts on.
            // Leaving it unselected makes the whole marker-editing vocabulary
            // (kind digits, phase keys, backspace) do nothing until the user picks
            // it out by hand.
            for m in &mut state.markers {
                m.selected = false;
            }
            let mut m = Marker::new(t, t, state.marker_kind);
            m.selected = true;
            state.markers.push(m);
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
        area / DEFAULT_NTRACKS_SHOWN as f32
    };
    h.clamp(MIN_TRACK_H, area.max(MIN_TRACK_H))
}

/// ctrl+wheel track zoom: change the track height (and hence how many tracks are
/// shown) while keeping the track under the cursor (`anchor`) fixed. Positive
/// `notches` (wheel down) shrink rows to reveal more tracks.
fn zoom_tracks(state: &mut AppState, area_h: f32, anchor: f32, notches: f32) {
    let area = area_h.max(1.0);
    let cur_h = resolved_height(state, area);
    let cur_n = (area / cur_h).max(1.0);
    let new_n = (cur_n + notches).max(1.0);
    let new_h = (area / new_n).clamp(MIN_TRACK_H, MAX_TRACK_H);
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
    //
    // Asked of the text edit specifically, not of focus in general:
    // `egui_wants_keyboard_input` is true whenever *any* widget holds focus, so a
    // menu item activated from the keyboard kept focus afterwards and killed every
    // single-key shortcut until a pointer click on the canvas took it back. That is
    // invisible to a mouse user and a dead end for a keyboard-only one, and
    // snuffler has no such state: closing a menu returns the keyboard to the viewer.
    //
    // An open menu still swallows them, the way a QMenu holds the keyboard while it
    // is up; the point is that closing it gives them back.
    if ui.ctx().text_edit_focused() || ui.ctx().any_popup_open() {
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
            state.track_height =
                (track_area_h.max(1.0) / (cur_n + 1.0)).clamp(MIN_TRACK_H, MAX_TRACK_H);
        }
        if i.key_pressed(Key::Minus) {
            state.track_height =
                (track_area_h.max(1.0) / (cur_n - 1.0).max(1.0)).clamp(MIN_TRACK_H, MAX_TRACK_H);
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
        // 'r' reloads modified files from disk; Shift+R reloads the Python snuffling
        // code (bridge.py). The App consumes the flags since keyboard handlers may
        // not touch the bridge.
        if i.key_pressed(Key::R) {
            if shift {
                state.reload_snufflings_requested = true;
            } else {
                state.reload_requested = true;
            }
        }
        if i.key_pressed(Key::Questionmark) || (i.key_pressed(Key::Slash) && shift) {
            state.show_help = true;
        }
        // ':' is Shift+Semicolon on most layouts; egui reports either depending on
        // whether the browser gave it a logical or a physical key.
        if i.key_pressed(Key::Colon) || (i.key_pressed(Key::Semicolon) && shift) {
            state.command.show();
        }

        // 'e' sets the single selected marker as the ACTIVE event — the distance /
        // Kagan / cake / seismosizer reference (snuffler set_active_event_marker):
        // it converts to an event if needed, deactivates any other, associates the
        // selected phase picks with it, and asks Python to recompute distances/Kagan
        // from it. Several selected plain markers convert to events without
        // activating. 'E' deactivates. The bridge call is deferred to the App.
        if i.key_pressed(Key::E) {
            if shift {
                for m in &mut state.markers {
                    m.active = false;
                }
                state.set_active_event_request = Some(ActiveEventReq {
                    deactivate: true,
                    ..Default::default()
                });
            } else {
                // Candidates for activation are selected markers that are already
                // events (catalog events carry a magnitude label in `phase`) or
                // plain markers that can convert — never a selected phase pick.
                let candidates: Vec<usize> = state
                    .markers
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.selected && (m.is_event || m.phase.is_none()))
                    .map(|(idx, _)| idx)
                    .collect();
                if let &[idx] = candidates.as_slice() {
                    for m in &mut state.markers {
                        m.active = false;
                    }
                    if let Some(m) = state.markers.get_mut(idx) {
                        if !m.is_event {
                            // Converting a plain marker to an event (not an already-
                            // labelled catalog event, whose phase label must stay).
                            m.is_event = true;
                            m.phase = None;
                            m.uncertainty = None;
                        }
                        m.active = true;
                    }
                    let (hash, t, nslc) = state.markers.get(idx).map_or((None, 0.0, None), |m| {
                        (m.event_hash.clone(), m.tmin, m.nslc.clone())
                    });
                    for m in &mut state.markers {
                        if m.selected && m.phase.is_some() {
                            m.event_hash.clone_from(&hash);
                        }
                    }
                    state.set_active_event_request = Some(ActiveEventReq {
                        deactivate: false,
                        event_hash: hash,
                        t,
                        nslc,
                    });
                } else {
                    for &idx in &candidates {
                        if let Some(m) = state.markers.get_mut(idx) {
                            m.is_event = true;
                            m.phase = None;
                            m.uncertainty = None;
                        }
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

        // Escape toggles batch picking (snuffler): enters batch mode, or aborts an
        // in-progress pick / exits the mode — take() discards the floating pick.
        if i.key_pressed(Key::Escape) && state.picking.take().is_none() {
            state.picking = Some(PickingState {
                batch: true,
                kind: state.marker_kind,
                ..Default::default()
            });
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
                let kind = n as u8;
                state.marker_kind = kind;
                // While picking, number/F keys retarget the floating pick so the
                // choice carries into the next re-armed pick (snuffler's
                // floating_or_selected_markers); otherwise they edit the selection.
                if let Some(pk) = state.picking.as_mut() {
                    pk.kind = kind;
                } else {
                    for m in &mut state.markers {
                        if m.selected {
                            m.kind = kind;
                        }
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
                if let Some(pk) = state.picking.as_mut() {
                    pk.phase = None;
                    pk.polarity = None;
                } else {
                    for m in &mut state.markers {
                        if m.selected {
                            m.phase = None;
                            m.polarity = None;
                            m.uncertainty = None; // clearing the phase resets uncertainty
                        }
                    }
                }
            } else if i.key_pressed(*key)
                && let Some(phase) = PHASE_KEYS.get(n)
            {
                if let Some(pk) = state.picking.as_mut() {
                    pk.phase = Some((*phase).to_string());
                } else {
                    for m in &mut state.markers {
                        if m.selected {
                            m.phase = Some((*phase).to_string());
                        }
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
        let mid = f64::midpoint(tmin, tmax);
        state.tmin = mid - win * 0.5;
        state.tmax = mid + win * 0.5;
    }
}

/// Select the next/previous marker by time and recenter (snuffler's `n`/`p`,
/// or `N`/`P` to step through event markers only).
fn select_relative(state: &mut AppState, forward: bool, events_only: bool) {
    let win = (state.tmax - state.tmin).max(1e-9);
    let center = f64::midpoint(state.tmin, state.tmax);
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
