//! The waveform canvas: vertically stacked tracks, a shared adaptive time axis,
//! NSLC labels, snuffler-style amplitude scaling, an optional waterfall view,
//! markers, and mouse/keyboard interaction.
//!
//! [`show`] paints through the toolkit-free painter, then hands the frame's input to
//! [`pointer`] and [`keyboard`], which edit the markers through [`marker_edits`].

mod keyboard;
mod marker_edits;
mod picking;
mod pointer;
mod scrollbar;
mod track_zoom;

use std::collections::HashMap;

use egui::{
    Align2, Color32, ColorImage, FontId, Rect, Sense, Stroke, TextureHandle, TextureOptions,
};
use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};
use snuffler_web_core::state::{AppState, ViewMode};
use snuffler_web_render::layout::{AXIS_HEIGHT, Layout, compute_layout, track_geometry};
use snuffler_web_render::paint::{paint, station_place_text};
use snuffler_web_render::palette::Palette;

use self::keyboard::KeyboardContext;
use self::pointer::MarkerHit;
use crate::view::draw::{EguiTarget, RasterImage, SvgTarget};
use crate::view::palette_for;

/// Width of the vertical track scrollbar carved off the right edge.
const SCROLLBAR_WIDTH_PX: f32 = 12.0;

/// Per-frame cache of waterfall textures, rebuilt only when the view data epoch
/// changes.
///
/// The pixels are coloured in Python (matplotlib) and packed into the `ys` buffer,
/// so any colormap or clip change re-fetches the view (new epoch).
/// The frontend keeps no colormap state of its own.
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
            .flat_map(|track| track.spans.iter().map(move |span| (track, span)))
            .filter_map(|(track, span)| {
                build_texture(ctx, view, track, span).map(|texture| (span.offset, texture))
            })
            .collect();
    }
}

fn build_texture(
    ctx: &egui::Context,
    view: &ViewData,
    track: &ViewTrack,
    span: &ViewSpan,
) -> Option<TextureHandle> {
    let image = build_image(view, track, span)?;
    let key = span.offset;
    Some(ctx.load_texture(format!("wf{key}"), image, TextureOptions::NEAREST))
}

/// Converts the shared raw waterfall image into an egui texture source.
fn build_image(view: &ViewData, track: &ViewTrack, span: &ViewSpan) -> Option<ColorImage> {
    let image = snuffler_web_render::waterfall::build_image(view, track, span)?;
    let size = [
        usize::try_from(image.width).ok()?,
        usize::try_from(image.height).ok()?,
    ];
    Some(ColorImage::from_rgba_unmultiplied(size, &image.rgba))
}

/// Renders the current wiggle or waterfall view to a standalone SVG string at the
/// given pixel size.
///
/// This reuses the exact [`paint`] geometry: vector traces and axes, with the
/// waterfall heatmap embedded as a PNG.
/// `margin` frames the canvas by widening the `viewBox`, for the print-shaped export.
pub fn render_svg(
    ctx: &egui::Context,
    state: &AppState,
    view: &ViewData,
    size: egui::Vec2,
    theme: egui::Theme,
    margin: f32,
) -> String {
    let pal = palette_for(theme, state.wiggle_theme);
    let rect = Rect::from_min_size(egui::Pos2::ZERO, size);
    let lay = compute_layout(state, view, rect);
    let waterfall = state.view_mode == ViewMode::Waterfall;
    let images: HashMap<u64, RasterImage> = if waterfall {
        lay.visible_tracks
            .iter()
            .flat_map(|track| track.spans.iter().map(move |span| (track, span)))
            .filter_map(|(track, span)| {
                snuffler_web_render::waterfall::build_image(view, track, span)
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

/// Draws the canvas and processes this frame's interaction.
///
/// `scene_range` is the whole pile's time span and `deltatmin` its finest sampling
/// interval, which the go-to-selection keys size their windows from.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    view: Option<&ViewData>,
    waterfall_cache: &mut WaterfallCache,
    view_epoch: u64,
    scene_range: Option<(f64, f64)>,
    deltatmin: f64,
) -> egui::Response {
    let pal = palette_for(ui.ctx().theme(), state.wiggle_theme);
    ui.ctx()
        .options_mut(|options| options.tessellation_options.feathering = state.antialiasing);
    // Reserve the whole canvas for the painter; the waveform and the scrollbar
    // each take their own interaction sub-rect so they never fight over drags.
    let (canvas, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
    let full_rect = canvas.rect;
    // Ground the whole canvas before anything else draws on it.
    // `paint` covers the waveform rect and the scrollbar its own strip, and those
    // two leave the corner between them beside the time axis.
    // The central panel carries no frame, so that sliver would show eframe's clear
    // color instead of the theme's.
    painter.rect_filled(full_rect, 0.0, pal.background);
    let track_area_height = (full_rect.height() - AXIS_HEIGHT).max(1.0);
    let keyboard_context = KeyboardContext {
        canvas_width: full_rect.width(),
        scene_range,
        deltatmin,
    };

    let Some(view) = view else {
        painter.text(
            full_rect.center(),
            Align2::CENTER_CENTER,
            "Loading…",
            FontId::proportional(16.0),
            pal.axis_text,
        );
        keyboard::handle_keyboard(ui, state, &[], &keyboard_context);
        // Still return a click-sensing response so an empty or loading canvas is
        // right-clickable (Qt's context menu works with no data too).
        return ui.interact(full_rect, canvas.id.with("wave"), Sense::click_and_drag());
    };

    let waterfall = state.view_mode == ViewMode::Waterfall;
    if waterfall {
        waterfall_cache.ensure(ui.ctx(), view, view_epoch);
    }
    let tracks = &view.meta.tracks;
    keyboard::handle_keyboard(ui, state, tracks, &keyboard_context);

    // Paint
    let overflows = fit_track_rows(state, tracks.len(), track_area_height);
    let scrollbar_width = if overflows { SCROLLBAR_WIDTH_PX } else { 0.0 };
    let wave_rect = Rect::from_min_max(
        full_rect.min,
        egui::pos2(full_rect.max.x - scrollbar_width, full_rect.max.y),
    );
    let wave_response = ui.interact(wave_rect, canvas.id.with("wave"), Sense::click_and_drag());
    let lay = compute_layout(state, view, wave_rect);
    let pointer_hit = update_hover(state, &lay, &wave_response);
    let mut target = EguiTarget::new(painter.clone(), &waterfall_cache.textures);
    paint(&mut target, state, view, &pal, &lay, waterfall, false);
    publish_annotations(ui, state, view, canvas.id, &lay);

    if overflows {
        let strip = Rect::from_min_max(
            egui::pos2(wave_rect.max.x, lay.track_area.top()),
            egui::pos2(full_rect.max.x, lay.track_area.bottom()),
        );
        scrollbar::draw_scrollbar(
            ui,
            canvas.id.with("scrollbar"),
            strip,
            state,
            pal.theme,
            tracks.len(),
            lay.row_height,
        );
    }

    // Hover, then the input that acts on it
    draw_crosshair(ui, &painter, &pal, &lay, &wave_response, pointer_hit);
    pointer::handle_pointer(ui, &wave_response, state, &lay, pointer_hit);
    wave_response
}

/// Exposes the drawn geographic annotations to accessibility tools.
fn publish_annotations(
    ui: &mut egui::Ui,
    state: &AppState,
    view: &ViewData,
    canvas_id: egui::Id,
    lay: &snuffler_web_render::layout::Layout<'_>,
) {
    if let Some(text) = state.epicentre_location(&view.meta) {
        ui.interact(
            Rect::from_min_max(
                egui::pos2(lay.rect.left(), lay.track_area.bottom()),
                lay.rect.max,
            ),
            canvas_id.with("event-place"),
            Sense::hover(),
        )
        .widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Epicentre: {text}"))
        });
    }

    if state.show_station_locations {
        for (index, track) in lay.visible_tracks.iter().enumerate() {
            let Some(place) = track.place.as_ref() else {
                continue;
            };
            let text = station_place_text(place);
            if !text.is_empty() {
                ui.interact(
                    Rect::from_min_max(
                        egui::pos2(lay.rect.left(), lay.row_top(index)),
                        egui::pos2(lay.rect.right(), lay.row_top(index) + lay.row_height),
                    ),
                    canvas_id.with(("station-place", index)),
                    Sense::hover(),
                )
                .widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Label,
                        true,
                        format!("Station location: {} · {text}", track.label),
                    )
                });
            }
        }
    }
}

/// Resolves the track controls into row geometry and caches the on-screen window,
/// so the next view request fetches the visible page.
///
/// Returns whether the tracks overflow the area, which is when the scrollbar shows.
///
/// The cached count is the full page (`rows_per_page`), not the possibly clamped
/// `rows_drawn`.
/// It sizes the per-track payload budget and rides the request identity, so a
/// partial last page must not change the horizontal resolution (and force a
/// re-fetch) on a purely vertical scroll.
/// A page reaching past the last row just returns fewer rows.
fn fit_track_rows(state: &mut AppState, track_count: usize, track_area_height: f32) -> bool {
    let geometry = track_geometry(state, track_count, track_area_height);
    state.track_scroll = geometry.scroll;
    state.track_offset = geometry.first_row;
    state.ntracks_shown = geometry.rows_per_page.max(1);

    #[expect(
        clippy::cast_precision_loss,
        reason = "a track count is far below f32's exact integer range"
    )]
    let track_count = track_count as f32;
    track_count > track_area_height / geometry.row_height.max(1.0) + 1e-3
}

/// Records what the pointer hovers: the track row and the marker under it.
fn update_hover(state: &mut AppState, lay: &Layout<'_>, response: &egui::Response) -> MarkerHit {
    // The hovered track row, for the only-label-the-hovered-row rule snuffler
    // applies when rows get too short for every label (pile_viewer.py:3530).
    state.hovered_track_row = response.hover_pos().and_then(|position| {
        (lay.track_area.contains(position) && lay.row_height > 0.0).then(|| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped at zero, and truncation to the containing row is the mapping"
            )]
            let row = ((position.y - lay.first_row_top) / lay.row_height).max(0.0) as usize;
            lay.first_row + row
        })
    });

    let pointer_hit = response
        .hover_pos()
        .filter(|position| lay.track_area.contains(*position))
        .map_or(MarkerHit::None, |position| {
            pointer::marker_hit_at(state, lay, position)
        });
    state.marker_hit_overload = match pointer_hit {
        MarkerHit::Overloaded(count) => Some(count),
        MarkerHit::None | MarkerHit::Marker(_) => None,
    };
    // No hover highlight while a pick, a pan or an uncertainty drag holds the
    // pointer.
    state.hovered_marker = if state.picking.is_none()
        && state.view_drag.is_none()
        && state.uncertainty_drag_last_y.is_none()
    {
        pointer_hit.marker()
    } else {
        None
    };
    pointer_hit
}

/// The transient hover crosshair, drawn only on screen and never in an export.
fn draw_crosshair(
    ui: &egui::Ui,
    painter: &egui::Painter,
    pal: &Palette,
    lay: &Layout<'_>,
    response: &egui::Response,
    pointer_hit: MarkerHit,
) {
    let Some(position) = response.hover_pos() else {
        return;
    };
    if !lay.track_area.contains(position) {
        return;
    }
    if matches!(pointer_hit, MarkerHit::Marker(_)) {
        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
    }
    painter.line_segment(
        [
            egui::pos2(position.x, lay.track_area.top()),
            egui::pos2(position.x, lay.track_area.bottom()),
        ],
        Stroke::new(1.0_f32, pal.cursor),
    );
}
