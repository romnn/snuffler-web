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

use std::collections::HashMap;

use egui::{
    Align2, Color32, ColorImage, FontId, Key, Rect, Sense, Stroke, TextureHandle, TextureOptions,
};

use crate::view::draw::{EguiTarget, RasterImage, SvgTarget};
use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};
use snuffler_web_core::marker_store::{MarkerCandidates, MarkerId};
use snuffler_web_core::markers::{Marker, PHASE_KEYS};
use snuffler_web_core::state::{
    ActiveEventReq, AppState, DEFAULT_NTRACKS_SHOWN, MAX_TRACK_H, MIN_TRACK_H, PickingState,
    ScalingGroup, SortMode, ViewDrag, ViewMode,
};
use snuffler_web_render::layout::{AXIS_H, Layout, compute_layout, track_geometry};
use snuffler_web_render::paint::{marker_matches_track, paint};
use snuffler_web_render::palette::{Theme, canvas_theme, palette};

/// Qt clamps the floating pick to the system's working time range
/// (`update_picking`, `pile_viewer.py:4192`) — on a modern host that probe lands
/// on years 100..3900.
const WORKING_TMIN: f64 = -59_011_459_200.0;
/// Upper bound of the working time range (see [`WORKING_TMIN`]).
const WORKING_TMAX: f64 = 60_904_915_200.0;

/// Width of the vertical track scrollbar carved off the right edge.
const SCROLLBAR_W: f32 = 12.0;
/// Maximum endpoint candidates inspected for one pointer position.
const MAX_HIT_CANDIDATES: usize = 256;

#[derive(Clone, Copy, Debug, Default)]
enum MarkerHit {
    #[default]
    None,
    Marker(MarkerId),
    Overloaded(usize),
}

impl MarkerHit {
    fn marker(self) -> Option<MarkerId> {
        match self {
            Self::Marker(id) => Some(id),
            Self::None | Self::Overloaded(_) => None,
        }
    }
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

/// Render the current wiggle/waterfall view to a standalone SVG string at the
/// given pixel size, reusing the exact [`paint`] geometry (vector traces/axes,
/// the waterfall heatmap embedded as a PNG). `margin` frames the canvas by
/// widening the `viewBox`, for the print-shaped export.
pub fn render_svg(
    ctx: &egui::Context,
    state: &AppState,
    view: &ViewData,
    size: egui::Vec2,
    theme: egui::Theme,
    margin: f32,
) -> String {
    let theme = canvas_theme(
        Theme::from_dark(theme == egui::Theme::Dark),
        state.wiggle_theme,
    );
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
                .layout_no_wrap(
                    text.to_owned(),
                    crate::view::canvas_font(size),
                    Color32::WHITE,
                )
                .size()
        })
    });
    target.set_margin(margin);
    paint(&mut target, state, view, &pal, &lay, waterfall, true);
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
    deltatmin: f64,
) -> egui::Response {
    let theme = canvas_theme(
        Theme::from_dark(ui.ctx().theme() == egui::Theme::Dark),
        state.wiggle_theme,
    );
    let pal = palette(theme, state.wiggle_theme);
    ui.ctx()
        .options_mut(|o| o.tessellation_options.feathering = state.antialiasing);
    // Reserve the whole canvas for the painter; the waveform and the scrollbar
    // each take their own interaction sub-rect so they never fight over drags.
    let (bg, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
    let full_rect = bg.rect;
    // Ground the whole canvas before anything else draws on it. `paint` covers
    // the waveform rect and the scrollbar its own strip, and those two leave the
    // corner between them beside the time axis; the central panel carries no
    // frame, so that sliver would show eframe's clear color instead of the theme's.
    painter.rect_filled(full_rect, 0.0, pal.bg);
    let track_area_h = (full_rect.height() - AXIS_H).max(1.0);

    let Some(view) = view else {
        painter.text(
            full_rect.center(),
            Align2::CENTER_CENTER,
            "Loading…",
            FontId::proportional(16.0),
            pal.axis_text,
        );
        handle_keyboard(
            ui,
            state,
            &[],
            0,
            track_area_h,
            full_rect.width(),
            scene_range,
            deltatmin,
        );
        // Still return a click-sensing response so an empty/loading canvas is
        // right-clickable (Qt's context menu works with no data too).
        return ui.interact(full_rect, bg.id.with("wave"), Sense::click_and_drag());
    };

    let waterfall = state.view_mode == ViewMode::Waterfall;
    if waterfall {
        wf.ensure(ui.ctx(), view, view_epoch);
    }

    let total = view.meta.tracks.len();
    handle_keyboard(
        ui,
        state,
        &view.meta.tracks,
        total,
        track_area_h,
        full_rect.width(),
        scene_range,
        deltatmin,
    );

    // Resolve the controls into row geometry and cache the on-screen window so
    // the next view request fetches the visible page.
    // The cached count is the full page (`n_page`), not the possibly-clamped
    // `n_draw`: it sizes the per-track payload budget and rides the request
    // identity, so a partial last page must not change the horizontal resolution
    // (and force a re-fetch) on a purely vertical scroll.
    // A page reaching past the last row just returns fewer rows.
    let g = track_geometry(state, total, track_area_h);
    state.track_height = g.height;
    state.track_scroll = g.scroll;
    state.track_offset = g.off;
    state.ntracks_shown = g.n_page.max(1);

    let overflow = (total as f32) > track_area_h / g.row_h.max(1.0) + 1e-3;
    let sb_w = if overflow { SCROLLBAR_W } else { 0.0 };
    let wave_rect = Rect::from_min_max(
        full_rect.min,
        egui::pos2(full_rect.max.x - sb_w, full_rect.max.y),
    );
    let wave_resp = ui.interact(wave_rect, bg.id.with("wave"), Sense::click_and_drag());

    let lay = compute_layout(state, view, wave_rect);
    let mut target = EguiTarget::new(painter.clone(), &wf.textures);
    paint(&mut target, state, view, &pal, &lay, waterfall, false);

    if overflow {
        let strip = Rect::from_min_max(
            egui::pos2(wave_rect.max.x, lay.track_area.top()),
            egui::pos2(full_rect.max.x, lay.track_area.bottom()),
        );
        draw_scrollbar(ui, bg.id.with("scrollbar"), strip, state, total, lay.row_h);
    }

    // The hovered track row, for the only-label-the-hovered-row rule snuffler
    // applies when rows get too short for every label (pile_viewer.py:3530).
    state.hover_track_row = wave_resp.hover_pos().and_then(|p| {
        (lay.track_area.contains(p) && lay.row_h > 0.0)
            .then(|| lay.off + ((p.y - lay.top0) / lay.row_h).max(0.0) as usize)
    });

    let pointer_hit = wave_resp
        .hover_pos()
        .filter(|position| lay.track_area.contains(*position))
        .map_or(MarkerHit::None, |position| {
            marker_hit_at(state, &lay, position)
        });
    state.marker_hit_overload = match pointer_hit {
        MarkerHit::Overloaded(count) => Some(count),
        MarkerHit::None | MarkerHit::Marker(_) => None,
    };
    state.hovered_marker = if state.picking.is_none()
        && state.view_drag.is_none()
        && state.uncertainty_drag_last_y.is_none()
    {
        pointer_hit.marker()
    } else {
        None
    };

    // Transient hover crosshair — drawn only on screen, never in an export.
    if let Some(p) = wave_resp.hover_pos()
        && lay.track_area.contains(p)
    {
        if matches!(pointer_hit, MarkerHit::Marker(_)) {
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

    handle_interaction(ui, &wave_resp, state, &lay, pointer_hit);
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
    let theme = canvas_theme(
        Theme::from_dark(ui.ctx().theme() == egui::Theme::Dark),
        state.wiggle_theme,
    );
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
///
/// Qt's `marker_under_cursor` (`pile_viewer.py:2146`): a hit lands within
/// `click_tolerance` (5 px worth of time) of either *endpoint*, never across
/// the span between them, so a wide span marker can be clicked through to
/// reach what lies behind it.
/// The first marker in list order wins, as Qt's loop returns it; the row under
/// the pointer must be one the marker's patterns take.
fn marker_hit_at(state: &AppState, lay: &Layout<'_>, pos: egui::Pos2) -> MarkerHit {
    let Layout {
        rect,
        track_area: area,
        view_tmin,
        window,
        visible,
        row_h,
        top0,
        ..
    } = *lay;
    if !area.contains(pos) {
        return MarkerHit::None;
    }
    let time = view_tmin + f64::from((pos.x - rect.left()) / rect.width().max(1.0)) * window;
    let tolerance = 5.0 * window / f64::from(rect.width().max(1.0));
    let candidates = state
        .markers
        .candidates_near(time, tolerance, MAX_HIT_CANDIDATES);
    let MarkerCandidates::Exact(ids) = candidates else {
        let MarkerCandidates::Overloaded { count } = candidates else {
            return MarkerHit::None;
        };
        return MarkerHit::Overloaded(count);
    };
    let row = ((pos.y - top0) / row_h.max(1.0)).floor().max(0.0) as usize;
    let Some(track) = visible.get(row) else {
        return MarkerHit::None;
    };
    for id in ids {
        let Some(marker) = state.markers.by_id(id) else {
            continue;
        };
        if !state.marker_kind_visible(marker.kind) {
            continue;
        }
        let row_hit = marker.nslc_ids.is_empty()
            || marker
                .nslc_ids
                .iter()
                .any(|nslc| marker_matches_track(nslc, track));
        if !row_hit {
            continue;
        }
        return MarkerHit::Marker(id);
    }
    MarkerHit::None
}

#[expect(
    clippy::too_many_lines,
    reason = "pointer interaction combines pan, zoom, marker picking, and uncertainty dragging against one projection context"
)]
fn handle_interaction(
    ui: &egui::Ui,
    resp: &egui::Response,
    state: &mut AppState,
    lay: &Layout<'_>,
    pointer_hit: MarkerHit,
) {
    let Layout {
        rect,
        track_area,
        view_tmin,
        window,
        visible,
        row_h,
        top0,
        ..
    } = *lay;
    let t_at_x =
        |x: f32| view_tmin + f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * window;
    let modifiers = ui.input(|i| i.modifiers);
    // Plain wheel scrolling arrives via `smooth_scroll_delta`, but egui re-routes
    // ctrl/⌘+wheel into a zoom gesture (zeroing the scroll delta).
    // Snuffler's ctrl+wheel track-zoom is recovered from the raw wheel events,
    // which keep their real modifiers.
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

    // Every channel gathered onto the row under the cursor — what Qt's
    // `get_nslc_ids_for_track` hands the pick, so a pick on a grouped row binds
    // to each concrete location on it. A row with no data in the window falls
    // back to its identity pattern.
    let track_nslc = |y: f32| -> Vec<(String, String, String, String)> {
        let rel = y - top0;
        if rel < 0.0 || row_h <= 0.0 {
            return Vec::new();
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rel>=0 and row_h>0, so the row index is a non-negative whole number; \
                      truncation to the containing row is the intended mapping"
        )]
        let idx = (rel / row_h) as usize;
        let Some(t) = visible.get(idx) else {
            return Vec::new();
        };
        let mut ids: Vec<(String, String, String, String)> = t
            .spans
            .iter()
            .map(|s| (t.net.clone(), t.sta.clone(), s.loc.clone(), t.cha.clone()))
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            ids.push((t.net.clone(), t.sta.clone(), t.loc.clone(), t.cha.clone()));
        }
        ids
    };
    // Unclamped pixel→time, for the floating pick and its auto-pan: `t_at_x`
    // clamps to the view, but Qt's `time_projection.rev` runs straight through
    // the edge — that overshoot *is* the auto-pan signal.
    let t_at_x_unclamped =
        |x: f32| view_tmin + f64::from((x - rect.left()) / rect.width().max(1.0)) * window;

    // Interactive picking, armed by a double-click (single-shot) or Escape
    // (batch). A floating pick follows the cursor; press → (drag) → release
    // commits a marker (instant if no drag), and batch mode re-arms a new pick
    // inheriting kind/phase/polarity. The wheel-scroll block above stays live so
    // the user can scroll stations mid-batch; pan/zoom and marker-select below
    // are suppressed while picking.
    if state.picking.is_some() {
        let (pressed, released, ipos) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
            )
        });
        let cursor = resp.hover_pos().or(ipos);
        let clamp_t = |t: f64| t.clamp(WORKING_TMIN, WORKING_TMAX);
        // Dragging the pick past the window edge pans the view after it, by up
        // to a tenth of the window per 50 ms timer tick in Qt
        // (`animate_picking`, pile_viewer.py:4133) — here per frame, scaled by
        // the frame time so the rate matches.
        if let Some(p) = cursor {
            let mouset = t_at_x_unclamped(p.x);
            let over = if mouset < state.tmin {
                mouset - state.tmin
            } else if mouset > state.tmax {
                mouset - state.tmax
            } else {
                0.0
            };
            if over != 0.0 {
                let frame_dt = f64::from(ui.input(|i| i.stable_dt)).clamp(0.0, 0.2);
                let dt = over.clamp(-window / 10.0, window / 10.0) * (frame_dt / 0.05);
                state.tmin += dt;
                state.tmax += dt;
                // Keep panning while the pointer rests beyond the edge.
                ui.ctx().request_repaint();
            }
        }
        if let Some(pk) = state.picking.as_mut() {
            if pressed && let Some(p) = ipos {
                let t = clamp_t(t_at_x_unclamped(p.x));
                pk.down_t = Some(t);
                pk.tmin = t;
                pk.tmax = t;
                pk.nslc_ids = track_nslc(p.y);
            } else if let Some(down) = pk.down_t {
                if let Some(p) = cursor {
                    let cur = clamp_t(t_at_x_unclamped(p.x));
                    pk.tmin = down.min(cur);
                    pk.tmax = down.max(cur);
                    // Qt re-reads the track under the cursor on every motion
                    // (`update_picking` refreshes `nslc_ids`), so a drag that
                    // ends on another row scopes the pick to that row.
                    pk.nslc_ids = track_nslc(p.y);
                }
            } else if let Some(p) = cursor {
                let t = clamp_t(t_at_x_unclamped(p.x));
                pk.tmin = t;
                pk.tmax = t;
                pk.nslc_ids = track_nslc(p.y);
            }
        }
        if released
            && let Some(pk) = state.picking.clone()
            && pk.down_t.is_some()
        {
            let mut rel_nslc = cursor.map(|p| track_nslc(p.y)).unwrap_or_default();
            if rel_nslc.is_empty() {
                rel_nslc.clone_from(&pk.nslc_ids);
            }
            let mut m = Marker::new(pk.tmin, pk.tmax, pk.kind);
            m.phase.clone_from(&pk.phase);
            m.polarity = pk.polarity;
            m.event_hash.clone_from(&pk.event_hash);
            m.event_time = pk.event_time;
            m.nslc_ids = rel_nslc;
            m.selected = true;
            deselect_all(state);
            state.markers.insert(m);
            state.picking = pk.batch.then(|| PickingState {
                batch: true,
                down_t: None,
                tmin: pk.tmin,
                tmax: pk.tmin,
                kind: pk.kind,
                phase: pk.phase.clone(),
                polarity: pk.polarity,
                event_hash: pk.event_hash.clone(),
                event_time: pk.event_time,
                nslc_ids: Vec::new(),
            });
            if state.picking.is_some() {
                // Qt's batch re-arm passes through `start_picking`, whose
                // `deselect_all` covers the marker just committed — only a
                // single-shot pick stays selected.
                deselect_all(state);
            }
        }
        return;
    }

    // Pressing *on* a marker selects it there and then, and must not arm the
    // pan/zoom drag — Qt's press handler is an if/elif chain
    // (pile_viewer.py:2042), so a hand tremor while clicking a marker cannot
    // rezoom the view. Shift adds to the selection instead of replacing it.
    let pressed_marker = ui
        .input(|i| i.pointer.primary_pressed())
        .then(|| pointer_hit.marker())
        .flatten();
    if let Some(hit) = pressed_marker {
        if !modifiers.shift {
            deselect_all(state);
        }
        state.markers.edit(hit, |marker| marker.selected = true);
    }

    if resp.drag_started_by(egui::PointerButton::Primary)
        && !modifiers.shift
        && pressed_marker.is_none()
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
        if let Some(hit) = pointer_hit.marker() {
            state.markers.edit(hit, |marker| marker.selected = true);
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

    // A double-click *arms* single-shot picking (Qt's `mouseDoubleClickEvent` →
    // `start_picking`, pile_viewer.py:2089): a floating pick follows the cursor
    // and the next press-drag-release commits it, scoped to the track under the
    // cursor, fresh at kind 0 with everything deselected on entry. (Qt swallows
    // the release that follows its double-click press; egui reports the double
    // click only after that release, so there is nothing to swallow here.)
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        deselect_all(state);
        let t = t_at_x(p.x);
        state.picking = Some(PickingState {
            batch: false,
            down_t: None,
            tmin: t,
            tmax: t,
            kind: 0,
            phase: None,
            polarity: None,
            event_hash: None,
            event_time: None,
            nslc_ids: track_nslc(p.y),
        });
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
    // Qt changes the shown count by max(1, shown/5) per notch
    // (pile_viewer.py:2622).
    // The height cap grows with the area so a single-track view stays reachable,
    // as Qt's count clamp to [1, ntracks] keeps it.
    let amount = (cur_n / 5.0).max(1.0);
    let new_n = (cur_n + amount * notches).max(1.0);
    let new_h = (area / new_n).clamp(MIN_TRACK_H, MAX_TRACK_H.max(area));
    let idx_at_cursor = state.track_scroll + anchor.clamp(0.0, 1.0) * cur_n;
    state.track_scroll = (idx_at_cursor - anchor.clamp(0.0, 1.0) * (area / new_h)).max(0.0);
    state.track_height = new_h;
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "one keyboard pass over one view context; the extra track slice is what \
              Tab's active-event walk orders by"
)]
fn handle_keyboard(
    ui: &egui::Ui,
    state: &mut AppState,
    tracks: &[ViewTrack],
    total: usize,
    track_area_h: f32,
    canvas_w: f32,
    scene_range: Option<(f64, f64)>,
    deltatmin: f64,
) {
    // Don't fire single-key shortcuts while a text field (FDSN forms, command
    // line) has the keyboard, or typing "n" would jump markers.
    //
    // Asked of the text edit specifically, not of focus in general:
    // `egui_wants_keyboard_input` is true whenever *any* widget holds focus, so a
    // menu item activated from the keyboard would keep every single-key shortcut
    // dead until a pointer click on the canvas.
    // Snuffler has no such state: closing a menu returns the keyboard to the viewer.
    //
    // An open menu still swallows them, the way a QMenu holds the keyboard while it
    // is up.
    if ui.ctx().text_edit_focused() || ui.ctx().any_popup_open() {
        return;
    }
    // Read before entering the input closure: `ui.input` holds the egui context
    // lock while its closure runs, so asking the context anything from inside
    // (`memory()`, another `input()`) self-deadlocks — on the web that surfaced
    // as an `Instant::now` panic out of the lock's debug timeout.
    let widget_focused = ui.ctx().memory(|memory| memory.focused().is_some());
    ui.input(|i| {
        let win = (state.tmax - state.tmin).max(1e-9);
        let shift = i.modifiers.shift;
        let any_key_pressed = i
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }));
        // Qt dispatches its letter commands on the typed text, so a Ctrl, Cmd or
        // Alt chord produces a control character and matches nothing
        // (pile_viewer.py:2225); its accelerators are separate QActions.
        // Same split here: the accelerators first, single-key commands only when no
        // chord modifier is down, so Ctrl+P is not the previous-marker key.
        if i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt {
            if (i.modifiers.ctrl || i.modifiers.command) && !i.modifiers.alt {
                // Snuffler's File accelerators (pile_viewer.py:874-921). Ctrl+O
                // and Ctrl+S shadow browser chords, deliberately: egui consumes
                // the handled key, and snuffler muscle memory outweighs the
                // host's Open-file and Save-page here.
                if i.key_pressed(Key::O) {
                    state.open_files_requested = true;
                }
                if i.key_pressed(Key::S) {
                    state.save_markers_requested = Some(shift);
                }
                if i.key_pressed(Key::E) {
                    state.export_image_requested = true;
                }
            }
            return;
        }
        // Snuffler's two-stroke chords, bound there as QKeySequences
        // (pile_viewer.py:935, :987): a bare `s` arms the chord and the next
        // keypress completes or cancels it, whatever it is — a Qt sequence
        // swallows its second key the same way.
        if state.pending_s_chord {
            if !any_key_pressed {
                return;
            }
            state.pending_s_chord = false;
            if i.key_pressed(Key::I) {
                state.scaling_group = ScalingGroup::Individual;
            } else if i.key_pressed(Key::C) {
                state.scaling_group = ScalingGroup::Common;
            } else if i.key_pressed(Key::S) {
                state.scaling_group = ScalingGroup::PerStation;
            } else if i.key_pressed(Key::N) {
                state.sort = SortMode::Name;
            } else if i.key_pressed(Key::D) {
                state.sort = SortMode::Distance;
            }
            return;
        }
        if i.key_pressed(Key::S) && !shift {
            state.pending_s_chord = true;
            return;
        }
        // Arrows and Space are the keys egui's focused widgets also act on: a
        // focused slider plus Left would both move the slider and nudge the
        // selected markers, a focused checkbox plus Space would toggle and page.
        // Qt's sliders decline those keys (`QSliderNoWheel.keyPressEvent`), so
        // the viewer sees them; here the widget keeps them while it has focus
        // (`widget_focused`, read before this closure took the context lock).
        // The browser's Fullscreen API needs a real user gesture and a DOM call —
        // eframe's viewport command is unhandled on the web — so the app performs
        // the toggle. Snuffler binds `f` (View ▸ Fullscreen); its `q`/`x` Close
        // pair is deliberately not bound: a page cannot close a tab it did not
        // open, and an inert key is worse than an absent one.
        if i.key_pressed(Key::F) {
            state.fullscreen_toggle_requested = true;
        }
        if i.key_pressed(Key::Space) && !widget_focused {
            state.tmin += win;
            state.tmax += win;
        }
        if i.key_pressed(Key::B) {
            state.tmin -= win;
            state.tmax -= win;
        }
        // Track-zoom keys (snuffler: + shows one track more, - one less, = resets
        // to the initial count), applied as a track height: more tracks means
        // shorter rows.
        // Plus is Shift+Equals on most layouts, so accept either for "more"; bare
        // Equals (no shift) is the reset (auto-fit sentinel).
        let cur_h = resolved_height(state, track_area_h);
        let cur_n = (track_area_h.max(1.0) / cur_h).round().max(1.0);
        // Qt clamps the shown *count* to [1, ntracks] (pile_viewer.py:2717), so a
        // single-track view must be reachable however tall the canvas is: the
        // height cap grows with the track area rather than stopping at 400 px.
        let max_track_h = MAX_TRACK_H.max(track_area_h);
        if i.key_pressed(Key::Plus) || (i.key_pressed(Key::Equals) && shift) {
            state.track_height =
                (track_area_h.max(1.0) / (cur_n + 1.0)).clamp(MIN_TRACK_H, max_track_h);
        }
        if i.key_pressed(Key::Minus) && !shift {
            state.track_height =
                (track_area_h.max(1.0) / (cur_n - 1.0).max(1.0)).clamp(MIN_TRACK_H, max_track_h);
        }
        // Shift+Minus is `_`, snuffler's Demean toggle (its menu shortcut).
        if i.key_pressed(Key::Minus) && shift {
            state.demean = !state.demean;
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
            state.remove_selected_markers();
        }

        // Selection navigation (snuffler keyPressEvent, pile_viewer.py:2258-2368).
        if i.key_pressed(Key::Tab) {
            cycle_selection(state, !shift, tracks, cur_n, scene_range, deltatmin);
        }
        if i.key_pressed(Key::N) {
            select_relative(state, true, shift); // Shift+N: events only
        }
        if i.key_pressed(Key::P) {
            select_relative(state, false, shift); // Shift+P: events only
        }
        if i.key_pressed(Key::A) {
            // Both forms skip hidden kinds (pile_viewer.py:2381-2392). 'A' is
            // additive — it only ever *sets* selected; 'a' is absolute, keeping
            // exactly the markers with an endpoint inside the view, so a span
            // wider than the screen is dropped, as Qt drops it.
            let kinds = state.visible_marker_kinds.clone();
            if shift {
                for m in &mut state.markers {
                    if kinds.contains(&m.kind) {
                        m.selected = true;
                    }
                }
            } else {
                for m in &mut state.markers {
                    m.selected = ((state.tmin <= m.tmin && m.tmin <= state.tmax)
                        || (state.tmin <= m.tmax && m.tmax <= state.tmax))
                        && kinds.contains(&m.kind);
                }
            }
        }
        if i.key_pressed(Key::D) {
            deselect_all(state);
        }
        if i.key_pressed(Key::G) {
            go_to_selection(state, shift, scene_range, deltatmin); // Shift+G: tight
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
                // events or plain markers that can convert — never a selected
                // phase pick.
                let candidates: Vec<usize> = state
                    .markers
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.selected && (m.is_event || m.phase.is_none()))
                    .map(|(idx, _)| idx)
                    .collect();
                if let &[idx] = candidates.as_slice() {
                    // Qt snapshots the marker's station before conversion removes
                    // its track scope, then uses that station to locate the event.
                    let station_nslc = state.markers.get(idx).and_then(|marker| {
                        (marker.nslc_ids.len() == 1)
                            .then(|| marker.nslc_ids.first().cloned())
                            .flatten()
                    });
                    for m in &mut state.markers {
                        m.active = false;
                    }
                    if let Some(m) = state.markers.get_mut(idx) {
                        if !m.is_event {
                            convert_to_event(m);
                        }
                        // Every Qt event has an identity — `get_event_hash`
                        // falls back to hashing the event's own fields — and
                        // the picks' association rides on it, so a locally
                        // created event synthesizes one from its origin time.
                        if m.event_hash.is_none() {
                            m.event_hash = Some(format!("local:{:x}", m.tmin.to_bits()));
                        }
                        m.active = true;
                    }
                    // Qt derives a hand-made event's coordinates from the
                    // marker's single station only when it has exactly one
                    // channel (`len(nslcs) == 1`, pile_viewer.py:2416).
                    let (hash, t) = state
                        .markers
                        .get(idx)
                        .map_or((None, 0.0), |m| (m.event_hash.clone(), m.tmin));
                    for m in &mut state.markers {
                        if m.selected && m.phase.is_some() {
                            m.event_hash.clone_from(&hash);
                            m.event_time = Some(t);
                        }
                    }
                    state.set_active_event_request = Some(ActiveEventReq {
                        deactivate: false,
                        event_hash: hash,
                        t,
                        nslc: station_nslc,
                    });
                } else {
                    for &idx in &candidates {
                        if let Some(m) = state.markers.get_mut(idx) {
                            convert_to_event(m);
                        }
                    }
                }
            }
        }

        // First-motion polarity on the floating pick or the selected phase
        // picks (pile_viewer.py:2233-2251).
        if i.key_pressed(Key::ArrowUp) && !widget_focused {
            set_polarity(state, true, shift);
        }
        if i.key_pressed(Key::ArrowDown) && !widget_focused {
            set_polarity(state, false, shift);
        }

        // Nudge selected picks by exactly one screen pixel of time, as snuffler
        // quantises it (pile_viewer.py:2533); Shift = ×10.
        if (i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowRight)) && !widget_focused {
            let dir = if i.key_pressed(Key::ArrowLeft) {
                -1.0
            } else {
                1.0
            };
            let amount = if shift { 10.0 } else { 1.0 };
            let dt = dir * amount * win / f64::from(canvas_w.max(1.0));
            for m in &mut state.markers {
                if m.selected && !m.is_event {
                    m.tmin += dt;
                    m.tmax += dt;
                }
            }
        }

        // Escape toggles batch picking (snuffler): enters batch mode, or aborts an
        // in-progress pick / exits the mode — take() discards the floating pick.
        // `start_picking` deselects everything on entry, so nothing but the
        // floating pick can be acted on, and a fresh pick is always kind 0.
        if i.key_pressed(Key::Escape) && state.picking.take().is_none() {
            deselect_all(state);
            state.picking = Some(PickingState {
                batch: true,
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
                // While picking, number/F keys retarget the floating pick so the
                // choice carries into the next re-armed pick (snuffler's
                // floating_or_selected_markers); otherwise they recolour the
                // selection. A digit never latches a kind for future picks —
                // Qt's fresh picks are always kind 0.
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
                // F10 demotes phase picks back to plain markers (Qt's
                // `convert_to_marker` in `set_phase_kind`, pile_viewer.py:2654),
                // dropping name, polarity, uncertainty and the event
                // association; plain markers and events pass through untouched.
                if let Some(pk) = state.picking.as_mut() {
                    pk.phase = None;
                    pk.polarity = None;
                    pk.event_hash = None;
                    pk.event_time = None;
                } else {
                    for m in &mut state.markers {
                        if m.selected && m.phase.is_some() {
                            m.phase = None;
                            m.polarity = None;
                            m.uncertainty = None;
                            m.event_hash = None;
                            m.event_time = None;
                        }
                    }
                }
            } else if i.key_pressed(*key)
                && let Some(phase) = PHASE_KEYS.get(n)
            {
                // F1-F9 name the pick and associate it with the active event —
                // `set_event(self.get_active_event())` on a phase pick, and
                // `convert_to_phase_marker(event, name, None, False)` on a
                // plain marker, which also resets its polarity. Events are
                // never given a phase (pile_viewer.py:2654).
                let active_event = state
                    .markers
                    .iter()
                    .find(|m| m.active)
                    .map(|m| (m.event_hash.clone(), m.tmin));
                if let Some(pk) = state.picking.as_mut() {
                    if pk.phase.is_none() {
                        pk.polarity = None;
                    }
                    pk.phase = Some((*phase).to_string());
                    pk.event_hash = active_event.as_ref().and_then(|(hash, _)| hash.clone());
                    pk.event_time = active_event.as_ref().map(|(_, time)| *time);
                } else {
                    for m in &mut state.markers {
                        if m.selected && !m.is_event {
                            if m.phase.is_none() {
                                m.polarity = None;
                            }
                            m.phase = Some((*phase).to_string());
                            m.event_hash = active_event.as_ref().and_then(|(hash, _)| hash.clone());
                            m.event_time = active_event.as_ref().map(|(_, time)| *time);
                        }
                    }
                }
            }
        }
    });
}

/// Clears the marker selection.
fn deselect_all(state: &mut AppState) {
    for marker in &mut state.markers {
        marker.selected = false;
    }
}

/// Converts a plain marker to an event, as Qt's `convert_to_event_marker` does
/// (`marker.py:495`): the span collapses to its start and the channel scope is
/// dropped, since an event is an instant that belongs to no track.
fn convert_to_event(m: &mut Marker) {
    m.is_event = true;
    m.phase = None;
    m.uncertainty = None;
    m.polarity = None;
    m.tmax = m.tmin;
    m.nslc_ids.clear();
}

/// Up/Down first-motion polarity, Qt's exact gate (`pile_viewer.py:2233`): only
/// *phase* markers take one — a repeated press in the same direction clears it
/// again, Shift forces "undecided" — and the floating pick takes precedence
/// over the selection (`floating_or_selected_markers`), so a polarity chosen
/// mid-pick carries into the next batch pick.
fn set_polarity(state: &mut AppState, up: bool, shift: bool) {
    let flip = |cur: Option<i8>| -> Option<i8> {
        if shift {
            Some(0)
        } else {
            let want = if up { 1 } else { -1 };
            if cur == Some(want) { None } else { Some(want) }
        }
    };
    if let Some(pk) = state.picking.as_mut() {
        if pk.phase.is_some() {
            pk.polarity = flip(pk.polarity);
        }
        return;
    }
    for m in &mut state.markers {
        if m.selected && m.phase.is_some() {
            m.polarity = flip(m.polarity);
        }
    }
}

/// Recenter the view on selected markers, or on the full scene if nothing is
/// selected (snuffler's `go_to_selection` fallback).
fn go_to_selection(
    state: &mut AppState,
    tight: bool,
    scene_range: Option<(f64, f64)>,
    deltatmin: f64,
) {
    let sel: Vec<&Marker> = state.markers.iter().filter(|m| m.selected).collect();
    let (tmin, tmax) = if sel.is_empty() {
        if tight {
            // Snuffler's tight case with nothing selected zooms *in* to one
            // trace-visibility length around the view centre (pile_viewer.py:2814).
            let center = f64::midpoint(state.tmin, state.tmax);
            (center, center)
        } else {
            // With nothing selected, `g` shows the whole pile (pile_viewer.py:2817).
            let Some(range) = scene_range else {
                return;
            };
            range
        }
    } else {
        (
            sel.iter().map(|m| m.tmin).fold(f64::INFINITY, f64::min),
            sel.iter().map(|m| m.tmax).fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let (tmin, tmax) = snuffler_web_core::session::good_looking_time_range(
        state.visible_length.jump_samples(),
        deltatmin,
        (state.tmin, state.tmax),
        tmin,
        tmax,
        tight,
    );
    state.tmin = tmin;
    state.tmax = tmax;
}

/// Select the next/previous marker by time and recenter (snuffler's `n`/`p`,
/// or `N`/`P` to step through event markers only), skipping hidden kinds.
fn select_relative(state: &mut AppState, forward: bool, events_only: bool) {
    let win = (state.tmax - state.tmin).max(1e-9);
    let center = f64::midpoint(state.tmin, state.tmax);
    // Qt's reference quirk (pile_viewer.py:2325): only lowercase `n` takes the
    // *maximum* selected time; `p`, `P` — and `N`, whose guard is
    // lowercase-only upstream — take the minimum. An upstream slip, reproduced
    // deliberately: the same keystrokes land on the same marker on both sides.
    let take_max = forward && !events_only;
    let selected_ref = state
        .markers
        .iter()
        .filter(|m| m.selected)
        .map(|m| m.tmin)
        .fold(None, |acc: Option<f64>, t| {
            Some(match acc {
                None => t,
                Some(a) if take_max => a.max(t),
                Some(a) => a.min(t),
            })
        });
    let reference = selected_ref.unwrap_or(center);

    let pick = state
        .markers
        .iter()
        .enumerate()
        .filter(|(_, m)| state.marker_kind_visible(m.kind))
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

    // With a selection but no further marker, Qt still re-centres on the
    // reference time — `tgo` was already set before the search.
    let mut tgo = selected_ref;
    if let Some((target, t)) = pick {
        for (i, m) in state.markers.iter_mut().enumerate() {
            m.selected = i == target;
        }
        tgo = Some(t);
    }
    if let Some(t) = tgo {
        state.tmin = t - win * 0.5;
        state.tmax = t + win * 0.5;
    }
}

/// Tab/Backtab walk *the phase picks of the active event* and nothing else
/// (`pile_viewer.py:2258`): kind-visible picks ordered by track row then
/// mid-time (Backtab reverses), one selected at a time, the view recentring
/// when the target sits off screen and the track stack scrolling to show its
/// row. With no active event the keys do nothing; a pick whose channel has no
/// row falls back to `go_to_selection`.
fn cycle_selection(
    state: &mut AppState,
    forward: bool,
    tracks: &[ViewTrack],
    cur_n: f32,
    scene_range: Option<(f64, f64)>,
    deltatmin: f64,
) {
    let Some(active_hash) = state
        .markers
        .iter()
        .find(|m| m.active)
        .and_then(|m| m.event_hash.clone())
    else {
        return;
    };
    // Qt's `marker_to_itrack`: the row the pick's channel gathers into, -1 when
    // it has none (an unscoped pick, or a channel with no row).
    let itrack_of = |m: &Marker| -> i64 {
        // Qt's `one_nslc()` raises unless the pick has exactly one channel, and
        // the walk treats that as "no row" (-1).
        let [n] = m.nslc_ids.as_slice() else {
            return -1;
        };
        tracks
            .iter()
            .position(|t| marker_matches_track(n, t))
            .map_or(-1, |k| k as i64)
    };
    let mut order: Vec<(usize, i64, f64)> = state
        .markers
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.phase.is_some()
                && m.event_hash.as_deref() == Some(active_hash.as_str())
                && state.marker_kind_visible(m.kind)
        })
        .map(|(i, m)| (i, itrack_of(m), f64::midpoint(m.tmin, m.tmax)))
        .collect();
    order.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
    });
    if !forward {
        order.reverse();
    }
    // Qt steps past the highest-placed selected pick, or starts at the first.
    let last_selected_pos = order
        .iter()
        .enumerate()
        .filter_map(|(pos, entry)| {
            let selected = state.markers.get(entry.0).is_some_and(|m| m.selected);
            selected.then_some(pos)
        })
        .max();
    let next_pos = last_selected_pos.map_or(0, |pos| pos + 1);
    let Some(&(target, itrack, _)) = order.get(next_pos) else {
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
    if itrack >= 0 {
        // Scroll the stack until the pick's row is on screen (Qt's
        // `scroll_tracks` calls): up to put it first, down to put it last.
        let itrack = itrack as f32;
        let low = state.track_scroll.floor();
        if itrack < low {
            state.track_scroll = itrack;
        } else if itrack >= low + cur_n {
            state.track_scroll = (itrack + 1.0 - cur_n).max(0.0);
        }
    } else {
        go_to_selection(state, false, scene_range, deltatmin);
    }
}
