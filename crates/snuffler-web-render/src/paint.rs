//! Backend-neutral waveform painting pipeline.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "rendering maps between pixel, time, sample, and track coordinates"
)]

use std::collections::HashMap;

use ecolor::Color32;
use emath::{Align2, Rect, pos2};
use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};
use snuffler_web_core::marker_store::{MarkerCandidates, MarkerId};
use snuffler_web_core::markers::Marker;
use snuffler_web_core::state::{AppState, Subsort};
use snuffler_web_core::theme::WiggleTheme;
use snuffler_web_core::timeaxis;

use crate::draw::DrawTarget;
use crate::layout::Layout;
use crate::palette::Palette;

/// Track labels and time-axis labels, a shade larger than the chrome's text and
/// bold like snuffler's: Qt paints these with the application's default font at
/// `setBold(True)`, which measures about 7 % wider than 12.5 pt did here.
const LABEL_PT: f32 = 13.5;
/// Snuffler's `MIN_LABEL_SIZE_PT` (`pile_viewer.py:44`): below this the label is
/// drawn only on the hovered row, at exactly this size.
const MIN_LABEL_SIZE_PT: f32 = 6.0;
const AXIS_PT: f32 = 13.5;
/// Size of the per-track y-range annotations ("Show scale ranges").
const SCALE_PT: f32 = 10.0;

/// The coloring key snuffler pairs with each subsort (the sorting `menudef` in
/// `pile_viewer.py`): location for NSLC and the grouped-by-location modes,
/// channel for NSCL/SNCL/LNSC, and the whole net+sta+loc for CNSL.
///
/// The location is the *span's*, not the track's. Under the grouped-by-location
/// subsorts a row carries every location of its channel, and colour is the only
/// thing telling them apart — which is why snuffler colours by location in exactly
/// the modes that group over it.
///
/// An empty key (single-location data) falls back to the track label so the Color
/// lines theme stays varied where snuffler would paint every trace alike — a
/// prettiness choice, see `color-traces-theming` in `tests/parity/known_gaps.toml`.
/// The `"*"` a span-less grouped row offers as its location is no key either, so it
/// falls back the same way. Under the Snuffler Qt theme there is no fallback: that
/// theme's whole claim is to look like snuffler, which does paint them alike.
///
/// `fallback` gates that substitution on the pile actually having no usable key
/// (see [`label_fallback_active`]): in a MIXED pile, a blank location keeps its
/// own pile-wide-stable `""` key instead — a label there would exist only while
/// the row's blank-location member has data in the window, and a key entering or
/// leaving the sorted numbering shifts every key after it on a mere pan.
fn color_key(
    track: &ViewTrack,
    loc: &str,
    subsort: Subsort,
    exact: bool,
    fallback: bool,
) -> String {
    let key = match subsort {
        Subsort::Nslc | Subsort::NscGroupL | Subsort::SncGroupL => loc.to_owned(),
        Subsort::Nscl | Subsort::Sncl | Subsort::Lnsc => track.cha.clone(),
        Subsort::Cnsl => format!("{}.{}.{}", track.net, track.sta, loc),
    };
    if !exact && fallback && (key.is_empty() || key == "*") {
        track.label.clone()
    } else {
        key
    }
}

/// Whether the web theme's label fallback is on: only when the whole pile has
/// no usable coloring key (single/empty-location data), the case the fallback
/// was written for. Then every row contributes its label whether it has data
/// in the window or not, so the label numbering is window-independent too.
fn label_fallback_active(pile_keys: &[String]) -> bool {
    pile_keys
        .iter()
        .all(|key| key.is_empty() || key.as_str() == "*")
}

/// The coloring keys one row contributes to the palette numbering: one per
/// painted span, since a row can carry traces with different keys, or the row's
/// own identity's when it has no spans — so the numbering does not shift when a
/// track happens to have no data in the window. (A span-less *grouped* row is
/// the exception, filtered at the `stroke_color_map` call site: its own "*" is
/// no identity, and the label it would fall back to is window-dependent there.)
fn track_color_keys(
    track: &ViewTrack,
    subsort: Subsort,
    exact: bool,
    fallback: bool,
) -> Vec<String> {
    if track.spans.is_empty() {
        vec![color_key(track, &track.loc, subsort, exact, fallback)]
    } else {
        track
            .spans
            .iter()
            .map(|span| color_key(track, &span.loc, subsort, exact, fallback))
            .collect()
    }
}

/// The pile box style covering `start`, for a stroke standing in for a hidden box.
///
/// A row carries one box per contiguous run, and adjacent runs alternate style, so
/// the span is matched to the run it starts inside rather than to the row.
/// A span outside every run (the pile's coverage and the fetched samples disagreeing
/// at a boundary) takes the row's first style, which is the one it is drawn beside.
fn span_box_style(track: &ViewTrack, start: f64) -> Option<u32> {
    track
        .boxes
        .iter()
        .find(|tbox| start >= tbox.tmin && start <= tbox.tmax)
        .or_else(|| track.boxes.first())
        .map(|tbox| tbox.style)
}

/// Stroke color per coloring key, assigned the way snuffler assigns its colors:
/// the distinct keys sorted, then numbered into the palette in that order
/// (`color_lookup` over the sorted `pile.gather_keys`).
///
/// The numbering base is `pile_keys` — the whole pile's coloring keys, computed
/// bridge-side exactly as snuffler computes `color_keys` (`pile_viewer.py:1753`) —
/// so a key keeps its colour as the view scrolls or pages. When the bridge sent
/// none (an empty pile, or a unit test exercising the assignment alone), the
/// painted tracks' own keys stand in.
///
/// The colours are the palette's ramp.
/// Under the Snuffler Qt theme that ramp is snuffler's own `graph_colors`
/// (`plot/__init__.py:95`) and, past the seventh key, snuffler's own black —
/// `pyrocko.plot.color` indexes that list and returns `(0, 0, 0)` for anything
/// beyond it rather than wrapping.
/// The web themes wrap their ramps instead.
fn stroke_color_map(
    pile_keys: &[String],
    tracks: &[ViewTrack],
    subsort: Subsort,
    wiggle: WiggleTheme,
    pal: &Palette,
) -> HashMap<String, Color32> {
    let exact = wiggle.mirrors_qt_coloring();
    let fallback = label_fallback_active(pile_keys);
    let mut keys: Vec<String> = if pile_keys.is_empty() {
        tracks
            .iter()
            .flat_map(|track| track_color_keys(track, subsort, exact, fallback))
            .collect()
    } else if exact {
        pile_keys.to_vec()
    } else if fallback {
        // No usable pile key (single/empty-location data): the labels ARE the
        // numbering, and every row contributes its own whether occupied or not,
        // so that numbering never moves with the window. The pile-wide
        // empty/wildcard keys are never looked up (the fallback replaces them
        // at draw time), so keeping them would waste a palette slot.
        tracks
            .iter()
            .flat_map(|track| track_color_keys(track, subsort, exact, fallback))
            .collect()
    } else {
        // Real location keys make up the numbering, and the label fallback is
        // off (see `color_key`): a blank location keeps its own pile-wide ""
        // key. Only the grouped-row "*" wildcard is dropped — it is a row
        // identity, not a key anything draws with. A span-less grouped row
        // contributes nothing: the keys it would paint with are locations the
        // pile-wide list already carries, and anything window-dependent it
        // added would renumber every key sorting after it on a mere pan.
        let mut keys: Vec<String> = pile_keys
            .iter()
            .filter(|key| key.as_str() != "*")
            .cloned()
            .collect();
        keys.extend(tracks.iter().flat_map(|track| {
            if track.spans.is_empty() && track.loc == "*" {
                return Vec::new();
            }
            track_color_keys(track, subsort, exact, fallback)
        }));
        keys
    };
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .enumerate()
        .map(|(index, key)| {
            let color = if exact {
                pal.strokes.get(index).copied().unwrap_or(Color32::BLACK)
            } else {
                pal.stroke(index)
            };
            (key, color)
        })
        .collect()
}

/// C's `%g` at its default precision, plus snuffler's `num_to_html` rewrite of
/// e-notation into `×10^n` (`pile_viewer.py:150`) — the format its scale numbers
/// are printed in.
fn fmt_g(v: f64) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    if !v.is_finite() {
        return format!("{v}");
    }
    let exp = v.abs().log10().floor() as i32;
    let trim = |mut s: String| {
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    };
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        trim(format!("{v:.decimals$}"))
    } else {
        let mantissa = v / 10_f64.powi(exp);
        format!("{} ×10^{exp}", trim(format!("{mantissa:.5}")))
    }
}

/// The distinct amplitude scales a row draws against, in span order: one for most
/// rows, several only where a location-grouped row carries traces scaled
/// differently. Zero lines and scale annotations are drawn per scale, not per
/// span — the segments of one gapped channel share a scale and must not draw it
/// twice.
fn distinct_span_scales(spans: &[ViewSpan]) -> Vec<&ViewSpan> {
    let mut seen: Vec<(f64, f64, f64, f64)> = Vec::new();
    let mut distinct = Vec::new();
    for span in spans {
        let key = (span.ymin, span.ymax, span.ymean, span.yscale);
        if !seen.contains(&key) {
            seen.push(key);
            distinct.push(span);
        }
    }
    distinct
}

/// Length of a time-axis tick mark (`ticklen`, `TimeAx.drawit`, `pile_viewer.py:547`).
const QT_TICK_LEN: f32 = 10.0;

const SDAY: f64 = 86_400.0;

/// One axis tick with everything the second pass needs to place its labels.
struct TickDraw {
    tick: f64,
    x: f32,
    ushift: f32,
    labels: timeaxis::TickLabels,
}

/// Renders the persistent waveform content (everything except the transient hover
/// crosshair) to any [`DrawTarget`].
///
/// This is the snuffler `drawit` analogue: the on-screen and SVG export backends
/// drive identical geometry through it.
#[expect(
    clippy::too_many_lines,
    reason = "the per-row trace pass shares one projection context; splitting it would \
              obscure the layered draw order"
)]
pub fn paint(
    target: &mut dyn DrawTarget,
    state: &AppState,
    view: &ViewData,
    pal: &Palette,
    lay: &Layout<'_>,
    waterfall: bool,
    export: bool,
) {
    let rect = lay.rect;
    let track_area = lay.track_area;
    let data_tmin = lay.data_tmin;
    let view_tmin = lay.view_tmin;
    let view_tmax = lay.view_tmax;
    let window = lay.window;
    let visible = lay.visible;
    let row_h = lay.row_h;
    let top0 = lay.top0;
    let x_of = |t: f64| rect.left() + ((t - view_tmin) / window) as f32 * rect.width();
    let point_margin = 4.0;
    let time_margin = f64::from(point_margin / rect.width().max(1.0)) * window;
    // Colour from the subsort the reply was produced under, not the live UI
    // state: after a subsort change, a whole round trip still paints the old
    // reply's tracks and color_keys, and keying those with the new subsort would
    // give a transient palette belonging to neither state.
    // An unknown wire value (an empty pre-first-reply view) falls back to the
    // live state.
    let subsort = Subsort::from_wire(&view.meta.subsort).unwrap_or(state.subsort);
    let label_fallback = label_fallback_active(&view.meta.color_keys);

    let qt_colors = state.wiggle_theme.mirrors_qt_coloring();
    let boxes_visible = state.boxes_visible(pal.theme);
    // The boxes and the strokes carry two different classes: a box is the pile's
    // box style, a stroke its coloring key.
    // Hiding the boxes takes one of those off the screen entirely, so on the web
    // themes the strokes take it over and a wiggle is drawn in the colour its box
    // would have had.
    // Without this, hiding the boxes on data with a single location code leaves
    // every trace one colour.
    //
    // A location-grouped row is the exception, and keeps colouring by the coloring
    // key: there several traces share one row and the key is what tells them apart.
    // The box style cannot, because it steps per contiguous run, so every trace of
    // the row would land on the run its window starts in.
    // Snuffler Qt keeps colouring by the coloring key throughout, boxes or no
    // boxes, because upstream's two toggles are independent.
    let strokes_carry_box_style = !boxes_visible && !qt_colors && !subsort.groups_locations();
    let stroke_colors = if state.color_traces_for(subsort, pal.theme) {
        stroke_color_map(
            &view.meta.color_keys,
            &view.meta.tracks,
            subsort,
            state.wiggle_theme,
            pal,
        )
    } else {
        HashMap::new()
    };

    target.fill_rect(rect, 0.0, pal.bg);

    if boxes_visible && !waterfall {
        draw_trace_boxes(target, pal, lay, &x_of);
    }
    draw_time_axis(target, state, pal, lay, &x_of, export);

    // Snuffler's label-size ladder (pile_viewer.py:3505): shrink the label font
    // until a row fits more than one line of it, stopping at 2 pt; whether the
    // result clears the 6 pt floor decides between labelling every row and
    // labelling only the hovered one.
    let mut label_pt = LABEL_PT;
    while label_pt > 2.0 {
        if row_h / target.measure("0", label_pt).y.max(1.0) > 1.0 {
            break;
        }
        label_pt -= 1.0;
    }

    // The marker-overload verdict, settled once before the trace loop: the
    // snippet pass below walks every marker per span, so under overload it is
    // skipped along with the marker layers instead of paying markers × spans
    // on every repaint first.
    let markers_over = markers_overloaded(state);
    let marker_ids = if markers_over {
        Vec::new()
    } else {
        match state.markers.overlapping(state.tmin, state.tmax, 2000) {
            MarkerCandidates::Exact(ids) => ids,
            MarkerCandidates::Overloaded { .. } => Vec::new(),
        }
    };

    // tracks (clipped to the track area so a partially-scrolled first/last row
    // never bleeds over the time axis or the top edge)
    target.push_clip(track_area);
    for (k, t) in visible.iter().enumerate() {
        let top = top0 + k as f32 * row_h;
        let bot = top + row_h;
        let mid = top + row_h * 0.5;

        // The waterfall has no wiggle to carry the row's identity, so it keeps an
        // outlined cell whichever theme is on.
        if boxes_visible && waterfall {
            let r = Rect::from_min_max(
                pos2(rect.left() + 0.5, top + 1.0),
                pos2(rect.right() - 0.5, bot - 1.0),
            );
            stroke_rect(target, r, 1.0, pal.sep);
        }

        // Independent of the bands, and most useful alongside them: a band is keyed
        // on station, so the three channels of one station share a colour and the
        // boundaries between them are exactly where nothing else marks a row.
        if k > 0 && state.track_separators() {
            target.line(
                pos2(rect.left(), top),
                pos2(rect.right(), top),
                1.0,
                pal.sep,
            );
        }

        if waterfall && t.nf > 0 {
            for span in &t.spans {
                let Some(time_bins) = span.count.checked_div(t.nf) else {
                    continue;
                };
                let t_abs = data_tmin + span.t0_rel;
                let x0 = x_of(t_abs);
                let x1 = x_of(t_abs + time_bins as f64 * span.dt);
                let r = Rect::from_min_max(pos2(x0, top + 1.0), pos2(x1, top + row_h - 1.0));
                // Keyed by payload offset, not by row: one strip per segment, and a
                // row can carry several.
                target.waterfall(r, span.offset);
            }
        } else {
            // "Clip traces" clips to the same 90% inner box the trace is scaled
            // into, so a spike is cut exactly at ymin/ymax as Qt cuts it
            // (pile_viewer.py:3186, :3462).
            // Off, the trace may run across the whole track area.
            let (cl_top, cl_bot) = if state.clip_traces {
                (mid - row_h * 0.45, mid + row_h * 0.45)
            } else {
                (track_area.top(), track_area.bottom())
            };
            target.push_clip(Rect::from_min_max(
                pos2(track_area.left(), cl_top),
                pos2(track_area.right(), cl_bot),
            ));
            // Display scale (units→pixels) comes entirely from pyrocko (yscale), and
            // is per span: snuffler projects per (track, scaling key), so two sensors
            // sharing a location-grouped row each keep their own amplitude scale.
            // The projection stays f64 until the final pixel, because a DC offset
            // that dwarfs the span would quantize visibly at f32.
            let y_of = |span: &ViewSpan, v: f64| {
                let half = span.yscale.max(1e-12);
                let amp = f64::from(row_h) * 0.45 * f64::from(state.gain) / half;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "one pixel coordinate at the end of an f64 projection"
                )]
                let px = (f64::from(mid) - (v - span.ymean) * amp) as f32;
                px
            };

            // A row with no data in the window has no entries here, having no
            // projection to draw zero lines or scale annotations against.
            let show_scales = state.show_scale_ranges || state.show_scale_axes;
            let scale_spans = if state.show_zero_lines || show_scales {
                distinct_span_scales(&t.spans)
            } else {
                Vec::new()
            };

            let colored_strokes = state.color_traces_for(subsort, pal.theme);
            let box_style_stroke = |start: f64| {
                span_box_style(t, start).map_or(pal.trace, |style| {
                    pal.stroke(usize::try_from(style).unwrap_or(0))
                })
            };
            // One polyline per span, never one across them: the segments of a
            // gapped channel share a row but must not be joined by a line drawn
            // through data that is not there.
            for span in &t.spans {
                let trace_col = match (colored_strokes, strokes_carry_box_style) {
                    (false, _) => pal.trace,
                    (true, true) => box_style_stroke(data_tmin + span.t0_rel),
                    (true, false) => stroke_colors
                        .get(&color_key(t, &span.loc, subsort, qt_colors, label_fallback))
                        .copied()
                        .unwrap_or(pal.trace),
                };
                // Uniform samples only: the bridge chops exactly as snuffler does
                // and drops anything shorter than two samples (pile_viewer.py:3905).
                if span.dt > 0.0 {
                    let samples = view.samples(span);
                    if samples.len() >= 2 {
                        let base_abs = data_tmin + span.t0_rel;
                        let start_idx = ((view_tmin - time_margin - base_abs) / span.dt)
                            .floor()
                            .max(0.0) as usize;
                        // floor + 2, not ceil: the range end is exclusive, and
                        // it must reach one sample PAST the right edge so the
                        // crossing segment exists (floor lands on the last
                        // sample at or before the edge; +2 includes the first
                        // one beyond it, whether or not the edge sits exactly
                        // on the grid).
                        let end_idx = (((view_tmax + time_margin - base_abs) / span.dt).floor()
                            + 2.0)
                            .clamp(0.0, samples.len() as f64)
                            as usize;
                        // Every sample in the widened slice is emitted, outside
                        // points included, and the clip rect cuts the crossing
                        // segment geometrically (as Qt's painter does).
                        // Filtering points by x would drop that segment, so at
                        // picking zooms the wiggle would detach from both edges.
                        let points: Vec<_> = samples
                            .get(start_idx..end_idx)
                            .unwrap_or(&[])
                            .iter()
                            .enumerate()
                            .map(|(i, &v)| {
                                let t_abs = base_abs + (start_idx + i) as f64 * span.dt;
                                pos2(x_of(t_abs), y_of(span, f64::from(v)))
                            })
                            .collect();
                        if points.len() >= 2 {
                            target.polyline(points, 1.0, trace_col);
                        }
                        if !markers_over {
                            draw_marker_snippets(
                                target,
                                state,
                                pal,
                                MarkerSnippet {
                                    marker_ids: &marker_ids,
                                    track: t,
                                    span,
                                    samples,
                                    base_abs,
                                },
                                &|t_abs, v| Some(pos2(x_of(t_abs), y_of(span, v))),
                            );
                        }
                    }
                }
            }
            target.pop_clip();

            // Scale annotations and zero lines, per distinct scale on the row,
            // following pile_viewer.py:3345-3434: Show Scale Axes *replaces*
            // Show Scale Ranges when both are on; neither draws on a row
            // shorter than about four text lines; each scale steps `uoff`
            // leftward past the one before it; the numbers take the trace
            // colour when Color Traces is on; and the zero line stops short of
            // the y == 0 tick label rather than running under it.
            let nlinesavail = row_h / target.measure("0", SCALE_PT).y.max(1.0);
            let mut uoff: f32 = 0.0;
            for &span in &scale_spans {
                let mut umax_zeroline = track_area.right();
                let mut uoffnext = uoff;
                let col = if colored_strokes {
                    stroke_colors
                        .get(&color_key(t, &span.loc, subsort, qt_colors, label_fallback))
                        .copied()
                        .unwrap_or(pal.trace)
                } else {
                    pal.trace
                };
                if show_scales && nlinesavail > 3.0 {
                    if state.show_scale_axes {
                        let yinc = span.yinc;
                        if yinc > 0.0 && yinc.is_finite() {
                            let ymin_annot = (span.ymin / yinc).ceil() * yinc;
                            let ny = ((span.ymax / yinc).floor() - (span.ymin / yinc).ceil())
                                .max(0.0) as i64
                                + 1;
                            let factor = 10_f64.powi(span.yexp);
                            for iy in 0..ny {
                                let y = ymin_annot + iy as f64 * yinc;
                                let v = y_of(span, y);
                                target.line(
                                    pos2(track_area.right() - 10.0 - uoff, v),
                                    pos2(track_area.right() - uoff, v),
                                    1.0,
                                    col,
                                );
                                // The shared exponent rides the topmost number
                                // only (pile_viewer.py:3369).
                                let sexp = if iy == ny - 1 && span.yexp != 0 {
                                    format!(" ×10^{}", span.yexp)
                                } else {
                                    String::new()
                                };
                                let label = format!("{}{}", fmt_g(y / factor), sexp);
                                let w = target.measure(&label, SCALE_PT).x;
                                target.text(
                                    pos2(track_area.right() - 20.0 - uoff, v),
                                    Align2::RIGHT_CENTER,
                                    &label,
                                    SCALE_PT,
                                    col,
                                );
                                uoffnext = uoffnext.max(w + 30.0);
                                if y == 0.0 {
                                    umax_zeroline = track_area.right() - 20.0 - w - 10.0 - uoff;
                                }
                            }
                        }
                    } else {
                        // Ranges: min bottom-right, max top-right, `%g` numbers,
                        // with the little bracket when boxes are off
                        // (pile_viewer.py:3396).
                        if !boxes_visible {
                            let x0 = track_area.right() - 20.0 - uoff;
                            let x1 = track_area.right() - 10.0 - uoff;
                            target.line(pos2(x0, bot), pos2(x1, bot), 1.0, col);
                            target.line(pos2(x1, bot), pos2(x1, top), 1.0, col);
                            target.line(pos2(x1, top), pos2(x0, top), 1.0, col);
                        }
                        let low = fmt_g(span.ymin);
                        let high = fmt_g(span.ymax);
                        let x = track_area.right() - 15.0 - uoff;
                        target.text(pos2(x, bot), Align2::RIGHT_BOTTOM, &low, SCALE_PT, col);
                        target.text(pos2(x, top), Align2::RIGHT_TOP, &high, SCALE_PT, col);
                        let w = target
                            .measure(&high, SCALE_PT)
                            .x
                            .max(target.measure(&low, SCALE_PT).x);
                        uoffnext = uoffnext.max(w + 10.0);
                    }
                }
                if state.show_zero_lines {
                    let yz = y_of(span, 0.0);
                    // Only inside this row — snuffler's own guard (`vmin <= v <=
                    // vmax`): a scale whose zero level lies beyond the row's box
                    // draws no line rather than a stray one across a neighbour.
                    if yz >= top && yz <= bot {
                        let zero_col = if show_scales { col } else { pal.zero };
                        target.line(
                            pos2(track_area.left(), yz),
                            pos2(umax_zeroline, yz),
                            1.0,
                            zero_col,
                        );
                    }
                }
                uoff = uoffnext;
            }
        }

        // The track label, exactly as the bridge composed it: snuffler's
        // space-joined gather key, sort value first (`plabel`,
        // pile_viewer.py:3521), anchored middle-left at x = 10 as snuffler
        // anchors it. The font shrinks with the row until it would drop below
        // snuffler's 6 pt floor; below that only the hovered row keeps a label,
        // drawn at the floor size (pile_viewer.py:3505, :3526).
        let hovered = state.hover_track_row == Some(k + lay.off);
        let (pt, draw_this_label) = if label_pt >= MIN_LABEL_SIZE_PT {
            (label_pt, true)
        } else {
            (MIN_LABEL_SIZE_PT, hovered)
        };
        if draw_this_label {
            let label_pos = pos2(track_area.left() + 10.0, mid);
            let size = target.measure(&t.label, pt);
            let bg = Rect::from_min_size(pos2(label_pos.x, label_pos.y - size.y * 0.5), size)
                .expand(pal.label_pad);
            target.fill_rect(bg, pal.label_rounding, pal.label_bg);
            target.text(label_pos, Align2::LEFT_CENTER, &t.label, pt, pal.axis_text);
        }
    }
    target.pop_clip();

    // Markers are painted without the track clip — Qt sets none here, and the
    // event captions deliberately rise above the track area's top edge. Clip to
    // the widget's own extent so far-offscreen span ends stay put.
    target.push_clip(Rect::from_min_max(
        pos2(rect.left(), rect.top()),
        pos2(rect.right(), track_area.bottom()),
    ));
    draw_markers(target, state, pal, lay, &x_of, markers_over, &marker_ids);
    target.pop_clip();

    target.push_clip(track_area);
    // Floating pick in batch/interactive picking mode: a vertical line (instant)
    // or span rect (drag), coloured by the pick's kind. The x-range guard skips
    // the un-hovered initial state (tmin==0 maps far off-screen).
    if let Some(pk) = &state.picking {
        let x0 = x_of(pk.tmin);
        if x0 >= track_area.left() - 2.0 && x0 <= track_area.right() + 2.0 {
            let col = pal.markers.kind(pk.kind, true);
            if pk.tmax > pk.tmin {
                let x1 = x_of(pk.tmax);
                let r =
                    Rect::from_min_max(pos2(x0, track_area.top()), pos2(x1, track_area.bottom()));
                target.fill_rect(
                    r,
                    0.0,
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 40),
                );
            }
            target.line(
                pos2(x0, track_area.top()),
                pos2(x0, track_area.bottom()),
                2.0,
                col,
            );
        }
    }
    target.pop_clip();
}

/// Snuffler's trace boxes, under every theme: one per contiguous run of a
/// channel's data, in the style the pile's `determine_box_styles` numbered it
/// with, so a box's colour means the same thing whatever the palette.
///
/// The boxes come from the pile, not from the fetched spans (`draw_trace_boxes`
/// reads `pile.iter_traces`, `pile_viewer.py:2968`), so a channel `min_deltat_allow`
/// declined to fetch still shows where its data lies.
/// Without that, a view zoomed far beyond the data goes blank with nothing left to
/// navigate back to.
/// Past 500 traces in the window the bridge hands each row merged per-style runs
/// whose gaps close, which is `draw_trace_boxes`' own batching.
fn draw_trace_boxes(
    target: &mut dyn DrawTarget,
    pal: &Palette,
    lay: &Layout<'_>,
    x_of: &impl Fn(f64) -> f32,
) {
    let track_area = lay.track_area;
    target.push_clip(track_area);
    for (k, t) in lay.visible.iter().enumerate() {
        let top = lay.top0 + k as f32 * lay.row_h + lay.row_h * 0.05;
        let bot = lay.top0 + k as f32 * lay.row_h + lay.row_h * 0.95;
        if bot <= top {
            continue;
        }
        for tbox in &t.boxes {
            if tbox.tmax < lay.view_tmin || tbox.tmin > lay.view_tmax {
                continue;
            }
            let x0 = x_of(tbox.tmin).clamp(track_area.left(), track_area.right());
            // Qt strokes the outline with a cosmetic pen, so a box thinner than a
            // pixel still marks its spot; flooring the width at one pixel is this
            // painter's equivalent.
            let x1 = x_of(tbox.tmax)
                .clamp(track_area.left(), track_area.right())
                .max(x0 + 1.0);
            let style = pal.box_style(usize::try_from(tbox.style).unwrap_or(0));
            let r = Rect::from_min_max(pos2(x0, top), pos2(x1, bot));
            target.fill_rect(r, 0.0, style.fill);
            if let Some(frame) = style.frame {
                stroke_rect(target, r, 1.0, frame);
            }
        }
    }
    target.pop_clip();
}

/// The time axis, following `TimeAx.drawit` (`pile_viewer.py:539`).
///
/// It uses snuffler's own tick choice and calendar anchoring
/// (`timeaxis::make_ticks`, with its `approx_ticks = 7`), the abbreviation ladder
/// measured against the tick spacing, the second and third context rows centred
/// under the ticks where their unit rolls over, and a left-edge fallback row when
/// none did.
///
/// Both offsets are snuffler's, rearranged rather than guessed.
/// `drawit` gives the tick label a *baseline* at `vmin + rect0.height() + ticklen`,
/// which puts the top of its glyphs exactly `ticklen` below the top of the axis
/// area; each row under it stacks by the height of the label above.
/// Measuring a digit stands in for `QFontMetrics.boundingRect`.
#[expect(
    clippy::too_many_lines,
    reason = "one axis pass, kept in drawit's order so it can be read against it"
)]
fn draw_time_axis(
    target: &mut dyn DrawTarget,
    state: &AppState,
    pal: &Palette,
    lay: &Layout<'_>,
    x_of: &impl Fn(f64) -> f32,
    export: bool,
) {
    let (rect, track_area, window) = (lay.rect, lay.track_area, lay.window);
    let (axis_ticks, tinc) = timeaxis::make_ticks(lay.view_tmin, lay.view_tmax, 7.0);
    let axis_top = track_area.bottom();
    let tick_label_h = target.measure("0", AXIS_PT).y;
    let row0_y = axis_top + QT_TICK_LEN;
    let row1_y = row0_y + tick_label_h;
    let row2_y = row1_y + tick_label_h;
    let axis_pad = 10.0;
    let mut draws: Vec<TickDraw> = Vec::new();
    // Qt computes the abbreviation level per tick in its first pass and its
    // second pass reads the loop variable after that pass has finished, so the
    // last tick's level applies to every label.
    // Label widths at one increment are near-equal, which is what makes it work.
    let mut abbr_level = 0_usize;
    for tick in &axis_ticks {
        let x = x_of(*tick);
        // Screen chrome only: snuffler's savesvg carries no grid, so an exported
        // figure must not either.
        if state.show_time_grid && !export {
            target.line(
                pos2(x, track_area.top()),
                pos2(x, track_area.bottom()),
                1.0,
                pal.grid,
            );
        }
        // The tick mark snuffler always draws, grid or no grid: ten pixels down
        // from the top of the axis area (`ticklen`, pile_viewer.py:546).
        target.line(
            pos2(x, axis_top),
            pos2(x, axis_top + QT_TICK_LEN),
            1.0,
            pal.axis_text,
        );
        let mut labels = timeaxis::tick_to_labels(*tick, tinc);
        // Qt hides the year at the epoch on sub-day windows, assuming synthetic
        // data (pile_viewer.py:569).
        if *tick == 0.0 && window < SDAY {
            if labels.l2.is_some() {
                labels.l2 = None;
            } else {
                labels.l1 = None;
            }
        }
        let pinc_approx = (x_of(*tick + tinc) - x).max(0.0);
        let ushift = if labels.l0_center {
            pinc_approx * 0.5
        } else {
            0.0
        };
        abbr_level = 0;
        for candidate in [labels.l0.as_str(), labels.l0_brief.as_str(), ""] {
            if target.measure(candidate, AXIS_PT).x <= pinc_approx * 0.9 {
                break;
            }
            abbr_level += 1;
        }
        draws.push(TickDraw {
            tick: *tick,
            x,
            ushift,
            labels,
        });
    }

    let mut l1_hits = 0_u32;
    let mut l2_hits = 0_u32;
    let mut first_tick_with_label: Option<f64> = None;
    for draw in &draws {
        let label0 = match abbr_level {
            0 => draw.labels.l0.as_str(),
            1 => draw.labels.l0_brief.as_str(),
            _ => "",
        };
        let w0 = target.measure(label0, AXIS_PT).x;
        let cx = draw.x + draw.ushift;
        if rect.left() + axis_pad < cx - w0 * 0.5 && cx + w0 * 0.5 < rect.right() - axis_pad {
            if first_tick_with_label.is_none() {
                first_tick_with_label = Some(draw.tick);
            }
            target.text(
                pos2(cx, row0_y),
                Align2::CENTER_TOP,
                label0,
                AXIS_PT,
                pal.axis_text,
            );
        }
        if let Some(l1) = &draw.labels.l1 {
            let w1 = target.measure(l1, AXIS_PT).x;
            if rect.left() + axis_pad < draw.x - w1 * 0.5
                && draw.x + w1 * 0.5 < rect.right() - axis_pad
            {
                target.text(
                    pos2(draw.x, row1_y),
                    Align2::CENTER_TOP,
                    l1,
                    AXIS_PT,
                    pal.axis_text,
                );
                l1_hits += 1;
            }
        }
        if let Some(l2) = &draw.labels.l2 {
            let w2 = target.measure(l2, AXIS_PT).x;
            if rect.left() + axis_pad < draw.x - w2 * 0.5
                && draw.x + w2 * 0.5 < rect.right() - axis_pad
            {
                target.text(
                    pos2(draw.x, row2_y),
                    Align2::CENTER_TOP,
                    l2,
                    AXIS_PT,
                    pal.axis_text,
                );
                l2_hits += 1;
            }
        }
    }

    // Left-edge fallback: the context row the first labelled tick would carry,
    // drawn only when no on-screen tick triggered one (pile_viewer.py:636).
    let fallback_tick = first_tick_with_label.unwrap_or(lay.view_tmin);
    let (mut fb_l1, mut fb_l2) = timeaxis::l1_l2_tick(fallback_tick, tinc);
    if -3600.0 * 25.0 < fallback_tick && fallback_tick <= 3600.0 * 25.0 && window < SDAY {
        // The epoch-year suppression again, for the fallback (pile_viewer.py:641).
        if fb_l2.is_empty() {
            fb_l1.clear();
        } else {
            fb_l2.clear();
        }
    }
    if l1_hits == 0 && !fb_l1.is_empty() {
        target.text(
            pos2(rect.left() + axis_pad, row1_y),
            Align2::LEFT_TOP,
            &fb_l1,
            AXIS_PT,
            pal.axis_text,
        );
    }
    if l2_hits == 0 && !fb_l2.is_empty() {
        target.text(
            pos2(rect.left() + axis_pad, row2_y),
            Align2::LEFT_TOP,
            &fb_l2,
            AXIS_PT,
            pal.axis_text,
        );
    }
    // The axis baseline snuffler closes `drawit` with (pile_viewer.py:668).
    target.line(
        pos2(rect.left(), axis_top),
        pos2(rect.right(), axis_top),
        1.0,
        pal.axis_text,
    );
}

/// Outlines `r` with four `width`-wide strokes.
fn stroke_rect(target: &mut dyn DrawTarget, r: Rect, width: f32, color: Color32) {
    target.line(r.left_top(), r.right_top(), width, color);
    target.line(r.right_top(), r.right_bottom(), width, color);
    target.line(r.right_bottom(), r.left_bottom(), width, color);
    target.line(r.left_bottom(), r.left_top(), width, color);
}

/// Shell-style glob match (`*`, `?`, `[seq]`, `[!seq]`), anchored at both ends —
/// the dialect `util.match_nslc` builds from `fnmatch.translate`, which is what
/// snuffler matches a marker's channel patterns with (`marker.py:220`). Patterns
/// are a handful of characters, so the backtracking `*` is harmless.
fn glob_match(pattern: &str, value: &str) -> bool {
    fn rec(p: &[char], v: &[char]) -> bool {
        match p.split_first() {
            None => v.is_empty(),
            Some(('*', rest)) => (0..=v.len()).any(|i| rec(rest, v.get(i..).unwrap_or(&[]))),
            Some(('?', rest)) => v.split_first().is_some_and(|(_, vrest)| rec(rest, vrest)),
            Some(('[', rest)) => {
                let Some(end) = rest.iter().position(|&c| c == ']') else {
                    // An unterminated class matches a literal '[', as fnmatch's does.
                    return v
                        .split_first()
                        .is_some_and(|(&c0, vrest)| c0 == '[' && rec(rest, vrest));
                };
                let (class, tail) = rest.split_at(end);
                let tail = tail.get(1..).unwrap_or(&[]);
                let (neg, class) = match class.split_first() {
                    Some(('!', c)) => (true, c),
                    _ => (false, class),
                };
                let Some((&ch, vrest)) = v.split_first() else {
                    return false;
                };
                let mut matched = false;
                // Walk the class as `lo-hi` ranges and single characters; a
                // dash without a following bound is a literal, as in fnmatch.
                let mut cursor = class;
                while let Some((&c0, more)) = cursor.split_first() {
                    if let Some(('-', more2)) = more.split_first()
                        && let Some((&hi, more3)) = more2.split_first()
                    {
                        if c0 <= ch && ch <= hi {
                            matched = true;
                        }
                        cursor = more3;
                    } else {
                        if c0 == ch {
                            matched = true;
                        }
                        cursor = more;
                    }
                }
                (matched != neg) && rec(tail, vrest)
            }
            Some((&c, rest)) => v
                .split_first()
                .is_some_and(|(&c0, vrest)| c0 == c && rec(rest, vrest)),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    rec(&p, &v)
}

/// Whether an NSLC-scoped marker belongs on a row. Per-component fnmatch,
/// exactly `util.match_nslc` over a pattern tuple: a partial wildcard like
/// `BH?` or `6*` matches, and an empty component matches only an empty code —
/// Qt's Cake Phase writes `('*', '*')` for "any location, any channel", and
/// `''` means "the blank location". A track that groups over a component
/// reports `'*'` for it and accepts any pattern there, since the row holds
/// every value. Matching the identity rather than the spans keeps a marker on
/// its row even when the marker's own location has no data in the window.
#[must_use]
pub fn marker_matches_track(nslc: &(String, String, String, String), t: &ViewTrack) -> bool {
    let f = |pat: &str, val: &str| val == "*" || glob_match(pat, val);
    f(&nslc.0, &t.net) && f(&nslc.1, &t.sta) && f(&nslc.2, &t.loc) && f(&nslc.3, &t.cha)
}

/// Whether a marker's patterns take one concrete trace of a row — `span.loc`
/// standing in for the grouped rows' `'*'` — which is what Qt's `draw_trace`
/// matches (`match_nslc(tr.nslc_id)`, `marker.py:387`) before highlighting the
/// snippet under a span marker.
fn marker_matches_trace(m: &Marker, t: &ViewTrack, loc: &str) -> bool {
    m.nslc_ids.is_empty()
        || m.nslc_ids.iter().any(|n| {
            glob_match(&n.0, &t.net)
                && glob_match(&n.1, &t.sta)
                && glob_match(&n.2, loc)
                && glob_match(&n.3, &t.cha)
        })
}

/// The snippet-highlight half of Qt's `Marker.draw_trace` (`marker.py:381`):
/// under a span marker the covered samples are re-drawn in the marker's colour
/// at width 1, with outlined 4x4 squares on the first and last covered sample;
/// an instantaneous pick gets one square at its interpolated amplitude. Runs
/// inside the trace loop because that is where the samples and their y
/// projection live — exactly where Qt calls it.
#[derive(Clone, Copy)]
struct MarkerSnippet<'a> {
    marker_ids: &'a [MarkerId],
    track: &'a ViewTrack,
    span: &'a ViewSpan,
    samples: &'a [f32],
    base_abs: f64,
}

fn draw_marker_snippets(
    target: &mut dyn DrawTarget,
    state: &AppState,
    pal: &Palette,
    snippet: MarkerSnippet<'_>,
    px: &impl Fn(f64, f64) -> Option<emath::Pos2>,
) {
    let MarkerSnippet {
        marker_ids,
        track,
        span,
        samples,
        base_abs,
    } = snippet;
    // Qt chops with `snap=(ceil, floor)` and `include_last`: the first sample
    // at or after tmin through the last at or before tmax, clipped to the
    // trace — its `drawpoint(..., clip=True)` squares land on the trace's own
    // end samples when the marker runs past them. A marker that misses the
    // segment entirely raises `NoData` in Qt's chop and draws nothing;
    // clamping it onto the segment instead would fabricate an endpoint square
    // on the final sample of every earlier segment of a gapped channel.
    let last = samples.len() - 1;
    let chop = |m: &Marker| {
        let i0 = ((m.tmin - base_abs) / span.dt).ceil().max(0.0) as usize;
        let i1f = ((m.tmax - base_abs) / span.dt).floor();
        if i1f < i0 as f64 || i0 > last {
            return None;
        }
        Some((i0, (i1f as usize).min(last)))
    };
    let square = |target: &mut dyn DrawTarget, pt: emath::Pos2, col: Color32| {
        // Qt's `drawpoint`: an unfilled 4x4 rect under a width-2 pen.
        let r = Rect::from_center_size(pt, emath::Vec2::splat(4.0));
        stroke_rect(target, r, 2.0, col);
    };
    for (id, m) in marker_ids
        .iter()
        .filter_map(|id| state.markers.by_id(*id).map(|marker| (*id, marker)))
    {
        // The same visibility filter the marker layers apply — Qt's strict
        // window overlap, written without the negation — plus Qt's per-trace
        // pattern match against this concrete trace.
        if m.is_event
            || m.tmin >= state.tmax
            || state.tmin >= m.tmax
            || !state.marker_kind_visible(m.kind)
            || !marker_matches_trace(m, track, &span.loc)
        {
            continue;
        }
        let col = m.color_in(&pal.markers, state.hovered_marker == Some(id));
        if m.is_span() {
            let Some((i0, i1)) = chop(m) else {
                continue;
            };
            let pts: Vec<_> = (i0..=i1)
                .filter_map(|j| {
                    let &s = samples.get(j)?;
                    px(base_abs + j as f64 * span.dt, f64::from(s))
                })
                .collect();
            if pts.len() >= 2 {
                target.polyline(pts.clone(), 1.0, col);
            }
            for j in [i0, i1] {
                if let Some(&s) = samples.get(j)
                    && let Some(pt) = px(base_abs + j as f64 * span.dt, f64::from(s))
                {
                    square(target, pt, col);
                }
            }
        } else {
            // `tr.interpolate(tmin)`: linear between the neighbouring samples.
            let rel = (m.tmin - base_abs) / span.dt;
            if rel < 0.0 || rel > last as f64 {
                continue;
            }
            let j = (rel.floor() as usize).min(last.saturating_sub(1));
            let frac = rel - j as f64;
            if let (Some(&lo), Some(&hi)) = (samples.get(j), samples.get((j + 1).min(last))) {
                let v = f64::from(lo) * (1.0 - frac) + f64::from(hi) * frac;
                if let Some(pt) = px(m.tmin, v) {
                    square(target, pt, col);
                }
            }
        }
    }
}

/// Draws a straight dashed line by emitting the on-segments one by one — the
/// draw targets have no dash primitive, and Qt's marker guides need the
/// selected dash (`setDashPattern([5, 3])`, `marker.py:344`) to read as selected.
/// `dash`/`gap` are in pixels; Qt specifies its patterns in units of the pen
/// width, so callers scale by `width` first.
fn dashed_line(
    target: &mut dyn DrawTarget,
    from: emath::Pos2,
    to: emath::Pos2,
    width: f32,
    color: Color32,
    dash: f32,
    gap: f32,
) {
    let delta = to - from;
    let len = delta.length();
    if len <= f32::EPSILON || dash <= 0.0 {
        target.line(from, to, width, color);
        return;
    }
    let dir = delta / len;
    let mut start = 0.0;
    while start < len {
        let end = (start + dash).min(len);
        target.line(from + dir * start, from + dir * end, width, color);
        start = end + gap;
    }
}

/// Draws a marker label the way `gui_util.draw_label` does: text in the
/// marker's colour on a plate (`Palette::marker_label_bg`, Qt's white under the Qt
/// theme), and — for the active event or its picks — the plate widened by 2 px
/// each side and outlined instead of merely filled.
fn draw_marker_label(
    target: &mut dyn DrawTarget,
    pos: emath::Pos2,
    align: Align2,
    text: &str,
    col: Color32,
    plate_bg: Color32,
    outline: bool,
) {
    let size = target.measure(text, LABEL_PT);
    let x0 = match align.x() {
        emath::Align::Min => pos.x,
        emath::Align::Center => pos.x - size.x * 0.5,
        emath::Align::Max => pos.x - size.x,
    };
    let y0 = match align.y() {
        emath::Align::Min => pos.y,
        emath::Align::Center => pos.y - size.y * 0.5,
        emath::Align::Max => pos.y - size.y,
    };
    let mut plate = Rect::from_min_size(pos2(x0, y0), size);
    if outline {
        // Qt widens the outlined plate (`rect.adjust(-2, 0, 2, 0)`) and strokes
        // it with the marker pen.
        plate = Rect::from_min_max(
            pos2(plate.left() - 2.0, plate.top()),
            pos2(plate.right() + 2.0, plate.bottom()),
        );
        target.fill_rect(plate, 0.0, plate_bg);
        stroke_rect(target, plate, 1.0, col);
    } else {
        target.fill_rect(plate, 0.0, plate_bg);
    }
    target.text(pos, align, text, LABEL_PT, col);
}

/// The label a marker carries, composed as Qt composes it with no separators
/// (`PhaseMarker.get_label`, `marker.py:707`): phase name, then the polarity
/// arrow. Events label themselves separately (`EventMarker.label`).
fn marker_label(m: &Marker) -> String {
    let mut label = m.phase.clone().unwrap_or_default();
    label.push_str(m.polarity_symbol());
    if m.automatic == Some(true) {
        label.push('@');
    }
    label
}

/// Whether more markers sit in the fetch window than snuffler draws one by one
/// — the 2000 of `with_key_in_limited` (`pile_viewer.py:3027`), the window
/// widened by the longest marker span, exactly `markers_deltat_max`.
///
/// Qt limits only its overview layer with this: the per-trace loop iterates
/// the unlimited `with_key_in` (`pile_viewer.py:3480`), so above the threshold
/// Qt still pays for, and draws, every snippet and per-track vertical. Here
/// the same verdict silences those passes too — a documented divergence
/// (`marker-overload-full-quiet` in the known-gaps register), because the
/// browser repaints the whole view on every interaction frame and a
/// markers × spans pass per frame is exactly the cost the count readout
/// exists to avoid.
fn markers_overloaded(state: &AppState) -> bool {
    state.markers.overload_count(state.tmin, state.tmax) > 2000
}

/// Draws every visible marker the way snuffler layers them (`drawit`,
/// `pile_viewer.py:3476`): first the per-track parts each marker paints inside
/// its matching rows (`Marker.draw_trace` — uncertainty boxes, verticals at
/// both ends, the label), then the overview parts painted across the whole
/// track area (`Marker.draw` — the top span line, the full-height guides of
/// selected/hovered/unscoped markers, event triangles and captions). The
/// snippet-highlight half of `draw_trace` lives in the trace loop, which is
/// where the samples and their projection are.
///
/// `overloaded` is the [`markers_overloaded`] verdict, computed by the caller
/// because the trace loop consumes it first: when set, both layers here give
/// way to the dotted overview line and the count readout.
#[expect(
    clippy::too_many_lines,
    reason = "one marker pass over one projection context, layered exactly as Qt \
              layers it; splitting per layer would duplicate the filters"
)]
fn draw_markers(
    target: &mut dyn DrawTarget,
    state: &AppState,
    pal: &Palette,
    lay: &Layout<'_>,
    x_of: &impl Fn(f64) -> f32,
    overloaded: bool,
    marker_ids: &[MarkerId],
) {
    let Layout {
        track_area: area,
        top0,
        visible,
        row_h,
        ..
    } = *lay;
    let fg = pal.axis_text;
    let v0 = area.top();
    let v1 = area.bottom();

    // Past the limit nothing is drawn one by one: a dotted overview line plus
    // a count take over (Qt's TooMany fallback, pile_viewer.py:3030) — a
    // catalog load or a picker run over a long window reaches that easily.
    if overloaded {
        let summary = state.markers.summary();
        let n_all = state.markers.len();
        let n_sel = summary.selected;
        let suffix = if n_sel == n_all {
            " (all selected)".to_owned()
        } else if n_sel > 0 {
            format!(" ({n_sel} selected)")
        } else {
            String::new()
        };
        // Qt takes the first marker's tmin and the last's tmax; its list is
        // sorted by tmin, so that is the earliest start and the end of the
        // latest-starting marker.
        let first_x = x_of(summary.earliest_marker_start).max(area.left());
        let last_x = x_of(summary.latest_starting_marker_end).min(area.right());
        // DotLine on a width-2 pen: 2 px dots, 4 px gaps.
        dashed_line(
            target,
            pos2(first_x, v0),
            pos2(last_x, v0),
            2.0,
            fg,
            2.0,
            4.0,
        );
        draw_marker_label(
            target,
            pos2(first_x + 10.0, v0 - 10.0),
            Align2::LEFT_BOTTOM,
            &format!("{n_all} Markers{suffix}"),
            fg,
            pal.marker_label_bg,
            false,
        );
        return;
    }

    // Qt's visibility filter, applied per marker in both layers: strict window
    // overlap and a kind the user has not hidden (pile_viewer.py:3067).
    let shown = |m: &Marker| {
        m.tmin < state.tmax && state.tmin < m.tmax && state.marker_kind_visible(m.kind)
    };
    let active_hash = state.markers.summary().active_hash;

    // Per-track layer (`Marker.draw_trace`): events draw nothing here
    // (`EventMarker.draw_trace` is a pass). An unscoped marker matches every
    // trace, so it paints these on every row, exactly as Qt does.
    for (id, m) in marker_ids
        .iter()
        .filter_map(|id| state.markers.by_id(*id).map(|marker| (*id, marker)))
    {
        if m.is_event || !shown(m) {
            continue;
        }
        let hovered = state.hovered_marker == Some(id);
        let col = m.color_in(&pal.markers, hovered);
        let u_min = x_of(m.tmin);
        let u_max = x_of(m.tmax);
        let label = marker_label(m);
        // A pick made on the active event outlines its label and shifts it two
        // pixels further left (marker.py:444).
        let outlined = m.event_hash.is_some() && m.event_hash == active_hash;
        let du = if outlined { -7.0 } else { -5.0 };
        let rows: Vec<usize> = if m.nslc_ids.is_empty() {
            (0..visible.len()).collect()
        } else {
            visible
                .iter()
                .enumerate()
                .filter(|(_, t)| m.nslc_ids.iter().any(|n| marker_matches_track(n, t)))
                .map(|(k, _)| k)
                .collect()
        };
        for k in rows {
            let ytop = top0 + k as f32 * row_h;
            let ybot = ytop + row_h;
            // Phase-pick uncertainty as translucent boxes under everything else
            // (`PhaseMarker.draw_trace`, marker.py:687): side bands at alpha 50,
            // the span itself at 35, full row height.
            if let Some(u) = m.uncertainty
                && u > 0.0
            {
                let band = |a: f32, b: f32, alpha: u8| {
                    (
                        Rect::from_min_max(pos2(a.min(b), ytop), pos2(a.max(b), ybot)),
                        Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha),
                    )
                };
                let (rl, cl) = band(x_of(m.tmin - u), u_min, 50);
                target.fill_rect(rl, 0.0, cl);
                let (rr, cr) = band(u_max, x_of(m.tmax + u), 50);
                target.fill_rect(rr, 0.0, cr);
                let (rm, cm) = band(u_min, u_max, 35);
                target.fill_rect(rm, 0.0, cm);
            }
            // Row-height verticals at *both* ends (marker.py:403): the tmax
            // line is the only thing showing where a span marker ends.
            target.line(pos2(u_min, ytop), pos2(u_min, ybot), 2.0, col);
            target.line(pos2(u_max, ytop), pos2(u_max, ybot), 2.0, col);
            if !label.is_empty() {
                draw_marker_label(
                    target,
                    pos2(u_min + du, ytop),
                    Align2::RIGHT_TOP,
                    &label,
                    col,
                    pal.marker_label_bg,
                    outlined,
                );
            }
        }
    }

    // Overview layer (`Marker.draw` via `draw_visible_markers`), painted on top.
    for (id, m) in marker_ids
        .iter()
        .filter_map(|id| state.markers.by_id(*id).map(|marker| (*id, marker)))
    {
        if !shown(m) {
            continue;
        }
        let hovered = state.hovered_marker == Some(id);
        let col = m.color_in(&pal.markers, hovered);
        let u_min = x_of(m.tmin);
        let u_max = x_of(m.tmax);
        // The short span line every marker gets at the top of the track area
        // (marker.py:335), one pixel proud of each end.
        target.line(pos2(u_min - 1.0, v0), pos2(u_max + 1.0, v0), 2.0, col);
        if m.selected || hovered || m.nslc_ids.is_empty() {
            // Full-height guides at both ends: dashed 5-3 (in pen widths) when
            // selected or hovered, grey when hovered-but-unselected — the dash
            // is what shows which markers the next keystroke acts on. An event
            // draws them only when selected or hovered (`draw_line=False`).
            let dashed = m.selected || hovered;
            let guide_col = if hovered && !m.selected {
                Color32::from_rgb(150, 150, 150)
            } else {
                col
            };
            if !m.is_event || m.selected || hovered {
                for t in [m.tmin, m.tmax] {
                    let u = x_of(t);
                    if dashed {
                        dashed_line(target, pos2(u, v0), pos2(u, v1), 2.0, guide_col, 10.0, 6.0);
                    } else {
                        target.line(pos2(u, v0), pos2(u, v1), 2.0, guide_col);
                    }
                }
            }
            if m.is_event {
                // Qt's paired triangles at tmin (marker.py:349): 9 px tall with
                // 0.577·s half-width, hanging from the top edge and standing on
                // the bottom one.
                let s = 9.0;
                target.polygon(
                    vec![
                        pos2(u_min - 0.577 * s, v0),
                        pos2(u_min, v0 + s),
                        pos2(u_min + 0.577 * s, v0),
                    ],
                    col,
                );
                target.polygon(
                    vec![
                        pos2(u_min - 0.577 * s, v1),
                        pos2(u_min, v1 - s),
                        pos2(u_min + 0.577 * s, v1),
                    ],
                    col,
                );
            }
        }
        if m.is_event {
            // The event caption, centred above the top edge with its baseline
            // 10 px up (`EventMarker.draw_label`, marker.py:561), outlined for
            // the active event.
            draw_marker_label(
                target,
                pos2(u_min, v0 - 10.0),
                Align2::CENTER_BOTTOM,
                &m.event_label(),
                col,
                pal.marker_label_bg,
                m.active,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use emath::{Rect, Vec2, pos2};
    use snuffler_web_core::bridge::{ViewData, ViewMeta, ViewSpan, ViewTrack};
    use snuffler_web_core::state::AppState;
    use snuffler_web_core::theme::WiggleTheme;

    use crate::draw::SvgTarget;
    use crate::layout::compute_layout;
    use crate::palette::{Palette, Theme, palette};

    use super::{
        MarkerSnippet, color_key, draw_marker_snippets, marker_matches_track, paint,
        stroke_color_map,
    };
    use ecolor::Color32;
    use snuffler_web_core::state::Subsort;

    fn track(net: &str, sta: &str, loc: &str, cha: &str, label: &str) -> ViewTrack {
        ViewTrack {
            net: net.into(),
            sta: sta.into(),
            loc: loc.into(),
            cha: cha.into(),
            label: label.into(),
            ..ViewTrack::default()
        }
    }

    // These pin the half of `color-traces-theming` (tests/parity/known_gaps.toml)
    // that mirrors snuffler: the per-subsort coloring keys and the sorted-key
    // numbering. The palette itself is deliberately this frontend's own.

    /// The stroke colors as a fixed-size array, so the asserts can destructure
    /// instead of indexing. These tracks carry no spans, so each looks up the
    /// one key its own identity earns; the fallback only satisfies the
    /// conversion's type and is unreachable, the map being built from these
    /// same keys.
    fn colors_of<const N: usize>(tracks: &[ViewTrack; N], subsort: Subsort) -> [Color32; N] {
        let map = stroke_color_map(
            &[],
            tracks.as_slice(),
            subsort,
            WiggleTheme::SnufflerWeb,
            &palette(Theme::Light, WiggleTheme::SnufflerWeb),
        );
        tracks
            .iter()
            .map(|track| {
                map.get(&color_key(track, &track.loc, subsort, false, true))
                    .copied()
                    .unwrap_or(Color32::TRANSPARENT)
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or([Color32::TRANSPARENT; N])
    }

    #[test]
    fn qt_theme_takes_snufflers_palette_and_blacks_out_past_it() {
        // `pyrocko.plot.color` indexes a seven-entry list and returns (0, 0, 0) for
        // anything past it rather than wrapping, so an eighth location goes black in
        // snuffler instead of repeating the first colour. The Qt theme follows.
        let tracks: Vec<ViewTrack> = ["00", "10", "20", "30", "40", "50", "60", "70"]
            .iter()
            .map(|loc| track("XX", "AAA", loc, "HHZ", "XX.AAA.HHZ"))
            .collect();
        let map = stroke_color_map(
            &[],
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerQt,
            &palette(Theme::Light, WiggleTheme::SnufflerQt),
        );
        assert_eq!(map.get("00").copied(), Some(Color32::from_rgb(204, 0, 0)));
        assert_eq!(map.get("60").copied(), Some(Color32::from_rgb(237, 212, 0)));
        assert_eq!(map.get("70").copied(), Some(Color32::BLACK));
    }

    #[test]
    fn qt_theme_paints_empty_locations_alike_where_the_other_themes_vary_them() {
        // The label fallback exists so Color lines stays varied on single-location
        // data. Snuffler has no such fallback — every empty location is one key — and
        // the Qt theme's claim is to look like snuffler, not to look good.
        let tracks = [
            track("XX", "AAA", "", "HHZ", "XX.AAA..HHZ"),
            track("XX", "BBB", "", "HHN", "XX.BBB..HHN"),
        ];
        let qt = stroke_color_map(
            &[],
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerQt,
            &palette(Theme::Light, WiggleTheme::SnufflerQt),
        );
        let web = stroke_color_map(
            &[],
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerWeb,
            &palette(Theme::Light, WiggleTheme::SnufflerWeb),
        );
        assert_eq!(qt.len(), 1, "snuffler paints them alike");
        assert_eq!(web.len(), 2, "this frontend's own themes vary them");
    }

    #[test]
    fn nslc_subsort_colors_by_location() {
        let tracks = [
            track("XX", "AAA", "00", "HHZ", "XX.AAA.00.HHZ"),
            track("XX", "BBB", "00", "HHN", "XX.BBB.00.HHN"),
            track("XX", "CCC", "10", "HHZ", "XX.CCC.10.HHZ"),
        ];
        // Shared location means a shared color; a different location does not.
        let [a, b, c] = colors_of(&tracks, Subsort::Nslc);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn cnsl_subsort_colors_by_net_sta_loc_composite() {
        // Same station code under two networks: snuffler's CNSL coloring key is
        // the (net, sta, loc) tuple, so these two must not share a color.
        let tracks = [
            track("XX", "AAA", "", "HHZ", "XX.AAA..HHZ"),
            track("YY", "AAA", "", "HHZ", "YY.AAA..HHZ"),
        ];
        let [a, b] = colors_of(&tracks, Subsort::Cnsl);
        assert_ne!(a, b);
    }

    #[test]
    fn empty_location_falls_back_to_per_track_label() {
        // Single-location data would collapse to one color under snuffler's key;
        // the label fallback keeps the Color lines theme varied instead — the
        // signed-off prettiness choice in the register entry.
        let tracks = [
            track("XX", "AAA", "", "HHZ", "XX.AAA..HHZ"),
            track("XX", "BBB", "", "HHZ", "XX.BBB..HHZ"),
        ];
        let [a, b] = colors_of(&tracks, Subsort::Nslc);
        assert_ne!(a, b);
    }

    #[test]
    fn grouped_rows_color_each_location_with_its_own_key() {
        // Under a "Grouped by Location" subsort a row carries one span per
        // location, and colour is the only thing telling the sensors apart — so
        // the spans of one row must key differently, and rows sharing a location
        // share its colour by sharing its key.
        let mut grouped = track("XL", "CO01", "*", "BHZ", "XL.CO01.BHZ");
        grouped.spans = vec![
            ViewSpan {
                loc: "00".into(),
                ..ViewSpan::default()
            },
            ViewSpan {
                loc: "10".into(),
                ..ViewSpan::default()
            },
        ];
        let mut single = track("XL", "CO02", "*", "BHZ", "XL.CO02.BHZ");
        single.spans = vec![ViewSpan {
            loc: "00".into(),
            ..ViewSpan::default()
        }];
        let tracks = [grouped, single];
        let map = stroke_color_map(
            &[],
            &tracks,
            Subsort::NscGroupL,
            WiggleTheme::SnufflerWeb,
            &palette(Theme::Light, WiggleTheme::SnufflerWeb),
        );
        let [grouped, _] = &tracks;
        // Two locations across the stack, so exactly two keys get numbered.
        assert_eq!(map.len(), 2);
        let first = map.get(&color_key(grouped, "00", Subsort::NscGroupL, false, true));
        let second = map.get(&color_key(grouped, "10", Subsort::NscGroupL, false, true));
        assert!(first.is_some());
        assert!(second.is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn empty_grouped_rows_leave_a_location_numbering_alone() {
        // A span-less grouped row paints nothing, and the keys it would paint
        // with — its sensors' locations — are already in the pile-wide list.
        // Letting its label pad the numbering instead would add and remove a
        // key as data moves in and out of the window, recoloring other
        // stations' traces on a mere pan or zoom.
        let pile_keys = vec!["00".to_owned(), "10".to_owned()];
        let mut occupied = track("XL", "CO01", "*", "BHZ", "XL CO01 BHZ");
        occupied.spans = vec![
            ViewSpan {
                loc: "00".into(),
                ..ViewSpan::default()
            },
            ViewSpan {
                loc: "10".into(),
                ..ViewSpan::default()
            },
        ];
        let empty = track("XL", "CO02", "*", "BHZ", "XL CO02 BHZ");
        let with_empty = stroke_color_map(
            &pile_keys,
            &[occupied.clone(), empty],
            Subsort::NscGroupL,
            WiggleTheme::SnufflerWeb,
            &palette(Theme::Light, WiggleTheme::SnufflerWeb),
        );
        let alone = stroke_color_map(
            &pile_keys,
            &[occupied],
            Subsort::NscGroupL,
            WiggleTheme::SnufflerWeb,
            &palette(Theme::Light, WiggleTheme::SnufflerWeb),
        );
        assert_eq!(with_empty, alone);

        // Single-location data filters the pile list to nothing, so there the
        // labels ARE the numbering and an empty row keeps contributing its own.
        let single_occupied = track("XL", "CO01", "*", "BHZ", "XL CO01 BHZ");
        let single_empty = track("XL", "CO02", "*", "BHZ", "XL CO02 BHZ");
        let labels = stroke_color_map(
            &[String::new()],
            &[single_occupied, single_empty.clone()],
            Subsort::NscGroupL,
            WiggleTheme::SnufflerWeb,
            &palette(Theme::Light, WiggleTheme::SnufflerWeb),
        );
        assert!(labels.contains_key(&single_empty.label));
    }

    #[test]
    fn markers_match_grouped_rows_through_the_location_wildcard() {
        // A location-grouped row carries '*' as its location (the bridge writes
        // snuffler's own `track_patterns` wildcard), so a marker picked on one
        // sensor stays on the row even when that sensor has no data in the
        // current window — and never leaks onto another station's row.
        let grouped = track("XL", "CO01", "*", "BHZ", "XL.CO01.BHZ");
        let other = track("XL", "CO02", "00", "BHZ", "XL.CO02.00.BHZ");
        let nslc = (
            "XL".to_owned(),
            "CO01".to_owned(),
            "10".to_owned(),
            "BHZ".to_owned(),
        );
        assert!(marker_matches_track(&nslc, &grouped));
        assert!(!marker_matches_track(&nslc, &other));
    }

    #[test]
    fn assignment_numbers_sorted_keys_independent_of_track_order() {
        let first = track("XX", "AAA", "00", "HHZ", "a");
        let second = track("XX", "BBB", "10", "HHZ", "b");
        // A key's color depends on the sorted key set, never on which track came
        // first — the same rule as snuffler's `color_lookup` over sorted keys.
        let [fa, fb] = colors_of(&[first.clone(), second.clone()], Subsort::Nslc);
        let [rb, ra] = colors_of(&[second, first], Subsort::Nslc);
        assert_eq!(fa, ra);
        assert_eq!(fb, rb);
    }

    // These pin `marker-nslc-wildcards` (tests/parity/known_gaps.toml): Qt
    // matches a marker's channel patterns with `util.match_nslc`, i.e. shell
    // globs per component, so partial wildcards must land on their rows.
    #[test]
    fn marker_patterns_match_with_shell_globs_per_component() {
        let t = track("XR", "STA01", "", "BHZ", "XR STA01  BHZ");
        let hit = |n: (&str, &str, &str, &str)| {
            marker_matches_track(
                &(
                    n.0.to_owned(),
                    n.1.to_owned(),
                    n.2.to_owned(),
                    n.3.to_owned(),
                ),
                &t,
            )
        };
        // Qt's Cake Phase writes ('*', '*') for location and channel.
        assert!(hit(("XR", "STA01", "*", "*")));
        // Partial wildcards: ?, prefix-*, and a character class.
        assert!(hit(("XR", "STA01", "", "BH?")));
        assert!(hit(("X*", "STA0[12]", "", "B*")));
        assert!(!hit(("XR", "STA01", "", "HH?")));
        assert!(!hit(("XR", "STA0[!1]", "", "BHZ")));
        // An empty pattern component matches only an empty code.
        assert!(hit(("XR", "STA01", "", "BHZ")));
        assert!(!hit(("XR", "", "", "BHZ")));
    }

    // Pins the readout of `marker-overload-full-quiet`: past 2000 markers in
    // the window snuffler collapses its overview layer to a count readout
    // (pile_viewer.py:3030), and this port silences the per-trace passes
    // with it.
    #[test]
    fn thousands_of_markers_collapse_to_the_count_readout() {
        let mut state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_060.0,
            ..AppState::default()
        };
        for k in 0..2001 {
            let t = state.tmin + f64::from(k) * 0.01;
            let mut m = snuffler_web_core::markers::Marker::new(t, t, 0);
            m.selected = k < 5;
            state.markers.insert(m);
        }
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                view_tmax: state.tmax,
                ..ViewMeta::default()
            },
            ys: Vec::new(),
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let theme = Theme::Dark;
        let colors = palette(theme, WiggleTheme::SnufflerQt);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });
        paint(&mut target, &state, &view, &colors, &layout, false, false);
        let svg = target.finish();
        // The readout takes Qt's exact wording, with the selected count.
        assert!(svg.contains("2001 Markers (5 selected)"));
    }

    /// Paints one track holding real samples under `n` span markers of kind 2
    /// and returns the SVG, for the overload assertions below.
    fn svg_with_span_and_markers(n: u32) -> String {
        let mut state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_060.0,
            ..AppState::default()
        };
        for k in 0..n {
            let t0 = state.tmin + f64::from(k) * 0.02;
            state
                .markers
                .insert(snuffler_web_core::markers::Marker::new(t0, t0 + 0.5, 2));
        }
        let mut row = track("XX", "STA01", "", "HHZ", "XX.STA01..HHZ");
        row.spans = vec![ViewSpan {
            count: 600,
            dt: 0.1,
            yscale: 1.0,
            ..ViewSpan::default()
        }];
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                view_tmax: state.tmax,
                tracks: vec![row],
                ..ViewMeta::default()
            },
            ys: vec![0.0; 600],
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let theme = Theme::Light;
        let colors = palette(theme, WiggleTheme::SnufflerQt);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });
        paint(&mut target, &state, &view, &colors, &layout, false, false);
        target.finish()
    }

    // The snippet pass obeys the same overload verdict as the marker layers.
    // Qt above the limit keeps paying for its per-trace marker decorations
    // (its trace loop iterates the unlimited `with_key_in`,
    // pile_viewer.py:3480); this painter deliberately silences the snippet
    // pass with everything else — the divergence documented on
    // `marker-overload-full-quiet` — so the render loop never does
    // markers × spans work behind the count readout.
    #[test]
    fn overload_also_silences_the_snippet_pass_over_real_samples() {
        // Below the limit the kind-2 markers reach the SVG in their idle
        // skyblue3: the snippet re-draw and the per-track verticals both
        // carry it.
        let below = svg_with_span_and_markers(2000);
        assert!(below.contains("#204a87"));
        // One marker more and only the count readout survives — no marker
        // colour anywhere, snippets over real samples included.
        let above = svg_with_span_and_markers(2001);
        assert!(above.contains("2001 Markers"));
        assert!(!above.contains("#204a87"));
    }

    // Qt's snippet chop (`tr.chop` in `Marker.draw_trace`, marker.py:410)
    // raises `NoData` for a marker that misses the segment, so nothing is
    // drawn on it.
    // Clamping the chop indices instead would put a spurious endpoint square on
    // an earlier segment's final sample of a gapped channel.
    #[test]
    fn a_span_marker_after_a_segment_draws_no_square_on_it() {
        // One hundred 1 Hz samples from t = 0: the segment ends at t = 99,
        // well inside the 0..400 view window the markers overlap.
        let seg_samples: Vec<f32> = vec![0.0; 100];
        let span = ViewSpan {
            count: 100,
            dt: 1.0,
            yscale: 1.0,
            ..ViewSpan::default()
        };
        let row = track("XX", "STA01", "", "HHZ", "XX.STA01..HHZ");
        #[expect(
            clippy::cast_possible_truncation,
            reason = "test projection: sample times map straight to pixels"
        )]
        let px = |t_abs: f64, v: f64| Some(pos2(t_abs as f32, v as f32));
        let svg_for = |marker: snuffler_web_core::markers::Marker| {
            let mut state = AppState {
                tmin: 0.0,
                tmax: 400.0,
                ..AppState::default()
            };
            state.markers.insert(marker);
            let marker_ids: Vec<_> = state.markers.iter_with_ids().map(|(id, _)| id).collect();
            let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
                Vec2::new(text.len() as f32 * size * 0.5, size)
            });
            draw_marker_snippets(
                &mut target,
                &state,
                &palette(Theme::Light, WiggleTheme::SnufflerQt),
                MarkerSnippet {
                    marker_ids: &marker_ids,
                    track: &row,
                    span: &span,
                    samples: &seg_samples,
                    base_abs: 0.0,
                },
                &px,
            );
            target.finish()
        };
        // Wholly past the segment: silence, as after Qt's NoData.
        let past = svg_for(snuffler_web_core::markers::Marker::new(200.0, 210.0, 0));
        assert!(!past.contains("<line") && !past.contains("<polyline"));
        // Straddling the segment's end: the snippet and its endpoint squares
        // land on the covered samples, proving the rejection is not too eager.
        let straddling = svg_for(snuffler_web_core::markers::Marker::new(50.0, 210.0, 0));
        assert!(straddling.contains("<polyline"));
    }

    // Pins the pile-backed half of the trace boxes: a channel `min_deltat_allow`
    // declined to fetch arrives with no spans but with its pile coverage on
    // `ViewTrack::boxes`, and the painter must still mark it — floored at one
    // pixel, the way Qt's cosmetic pen strokes a sub-pixel `drawRect`.
    #[test]
    fn an_unfetched_channel_still_paints_its_pile_backed_box() {
        let state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_701_209_600.0,
            ..AppState::default()
        };
        let mut row = track("XX", "STA01", "", "HHZ", "XX.STA01..HHZ");
        // Ninety seconds of coverage in a two-week window: three millionths of
        // the width, far below one pixel, and no spans to derive anything from.
        row.boxes = vec![snuffler_web_core::bridge::TraceBox {
            tmin: 1_700_600_000.0,
            tmax: 1_700_600_090.0,
            style: 0,
        }];
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                view_tmax: state.tmax,
                tracks: vec![row],
                ..ViewMeta::default()
            },
            ys: Vec::new(),
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let theme = Theme::Light;
        let colors = palette(theme, WiggleTheme::SnufflerQt);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });
        paint(&mut target, &state, &view, &colors, &layout, false, false);
        let svg = target.finish();
        // Style 0's frame is snuffler's Tango orange3, stroked at full alpha, so
        // its hex survives `Color32`'s premultiplied storage exactly (the
        // alpha-100 fill quantizes a unit off). Its presence means the box was
        // painted, which needs the one-pixel width floor — the raw projection of
        // ninety seconds here is 0.05 px, which `fill_rect` drops.
        assert!(svg.contains("#ce5c00"));
    }

    #[test]
    fn complete_pipeline_renders_axes_to_svg_on_the_host() {
        let state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_060.0,
            ..AppState::default()
        };
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                ..ViewMeta::default()
            },
            ys: Vec::new(),
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let theme = Theme::Dark;
        let colors = palette(theme, WiggleTheme::SnufflerQt);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });

        paint(&mut target, &state, &view, &colors, &layout, false, false);
        let svg = target.finish();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("<line"));
        assert!(svg.contains("<text"));
    }

    /// Builds one track carrying a single span and a box of the given style.
    fn styled_track(sta: &str, style: u32, tmin: f64, tmax: f64) -> ViewTrack {
        let mut t = track("XX", sta, "", "HHZ", &format!("XX.{sta}..HHZ"));
        t.spans = vec![ViewSpan {
            t0_rel: 0.0,
            dt: 1.0,
            count: 6,
            offset: 0,
            ymin: -1.0,
            ymax: 1.0,
            yscale: 1.0,
            ..ViewSpan::default()
        }];
        t.boxes = vec![snuffler_web_core::bridge::TraceBox { tmin, tmax, style }];
        t
    }

    /// Every `stroke` colour a paint pass drew a wiggle in, in document order.
    fn wiggle_strokes(state: &AppState, view: &ViewData, pal: &Palette) -> Vec<String> {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(state, view, rect);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });
        paint(&mut target, state, view, pal, &layout, false, false);
        target
            .finish()
            .split("<polyline ")
            .skip(1)
            .filter_map(|tag| tag.split("stroke=\"").nth(1)?.split('"').next())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_grouped_row_colours_its_traces_by_location_even_with_the_boxes_hidden() {
        // A location-grouped subsort puts every location of a channel on one row, and
        // the coloring key is the only thing telling those traces apart.
        // The box style cannot do it: it steps per contiguous run, so every trace of
        // the row starts inside the same run and keying the strokes on it would paint
        // the whole row a single colour.
        let (tmin, tmax) = (1_700_000_000.0, 1_700_000_010.0);
        let mut row = track("IU", "MAJO", "*", "BH1", "IU MAJO BH1");
        row.spans = ["00", "10"]
            .into_iter()
            .map(|loc| ViewSpan {
                t0_rel: 0.0,
                dt: 1.0,
                count: 6,
                offset: 0,
                ymin: -1.0,
                ymax: 1.0,
                yscale: 1.0,
                loc: loc.to_owned(),
                ..ViewSpan::default()
            })
            .collect();
        row.boxes = vec![snuffler_web_core::bridge::TraceBox {
            tmin,
            tmax,
            style: 0,
        }];
        let state = AppState {
            tmin,
            tmax,
            subsort: Subsort::NscGroupL,
            wiggle_theme: WiggleTheme::SnufflerWeb5,
            show_boxes: Some(false),
            ..AppState::default()
        };
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: tmin,
                view_tmax: tmax,
                tracks: vec![row],
                color_keys: vec!["00".to_owned(), "10".to_owned(), "20".to_owned()],
                subsort: "nsc_l".to_owned(),
                ..ViewMeta::default()
            },
            ys: vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5],
        };
        let pal = palette(Theme::Dark, WiggleTheme::SnufflerWeb5);

        // Both traces are drawn, and in the two colours their locations number to.
        let strokes = wiggle_strokes(&state, &view, &pal);
        let hex = |c: Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
        assert_eq!(
            strokes,
            vec![hex(pal.stroke(0)), hex(pal.stroke(1))],
            "a grouped row must colour its traces by location"
        );
        assert_ne!(strokes.first(), strokes.get(1));
    }

    #[test]
    fn hiding_the_boxes_moves_their_class_onto_the_wiggles() {
        // The boxes carry the pile's box style and the strokes carry the coloring
        // key, which are two different classes.
        // On data with one location code — the common case — every trace shares a
        // coloring key, so colouring by it once the boxes are hidden would leave
        // twelve identical wiggles and no way to tell the rows apart.
        // The strokes take the box style over instead, and a wiggle is drawn in the
        // colour its box would have had.
        let (tmin, tmax) = (1_700_000_000.0, 1_700_000_010.0);
        let scene = |wiggle, boxes| AppState {
            tmin,
            tmax,
            wiggle_theme: wiggle,
            show_boxes: Some(boxes),
            ..AppState::default()
        };
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: tmin,
                view_tmax: tmax,
                tracks: vec![
                    styled_track("AAA", 0, tmin, tmax),
                    styled_track("BBB", 3, tmin, tmax),
                ],
                ..ViewMeta::default()
            },
            ys: vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5],
        };
        let pal = palette(Theme::Dark, WiggleTheme::SnufflerWeb1);
        let hidden = scene(WiggleTheme::SnufflerWeb1, false);

        // Both rows share the empty location, so the coloring key cannot separate
        // them; the two box styles can, and that is what is drawn.
        let strokes = wiggle_strokes(&hidden, &view, &pal);
        let expected = |style: usize| {
            let c = pal.stroke(style);
            format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
        };
        assert_eq!(strokes, vec![expected(0), expected(3)]);
        assert_ne!(expected(0), expected(3));

        // With the boxes back the boxes carry the class again, so the mood theme
        // returns to snuffler's single ink for the wiggles.
        let shown = scene(WiggleTheme::SnufflerWeb1, true);
        let inked = format!(
            "#{:02x}{:02x}{:02x}",
            pal.trace.r(),
            pal.trace.g(),
            pal.trace.b()
        );
        assert_eq!(wiggle_strokes(&shown, &view, &pal), vec![inked.clone(); 2]);

        // Snuffler Qt keeps its grey wiggle with the boxes hidden, because
        // upstream's Show Boxes and Color Traces are independent toggles.
        let qt = scene(WiggleTheme::SnufflerQt, false);
        let qt_pal = palette(Theme::Light, WiggleTheme::SnufflerQt);
        let qt_ink = format!(
            "#{:02x}{:02x}{:02x}",
            qt_pal.trace.r(),
            qt_pal.trace.g(),
            qt_pal.trace.b()
        );
        assert_eq!(wiggle_strokes(&qt, &view, &qt_pal), vec![qt_ink; 2]);
    }

    #[test]
    fn boundary_crossing_samples_are_emitted_for_the_clip_to_cut() {
        // At picking zooms adjacent samples sit many pixels apart, and the
        // segment reaching the canvas edge has its outer endpoint off-screen.
        // The points are emitted and the clip rect cuts the line geometrically,
        // as Qt's painter does, so the wiggle stays attached to both edges.
        let state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_010.0,
            ..AppState::default()
        };
        let mut t = track("XX", "AAA", "", "HHZ", "XX.AAA..HHZ");
        t.spans = vec![ViewSpan {
            t0_rel: -3.0,
            dt: 3.0,
            count: 6,
            offset: 0,
            ymin: -1.0,
            ymax: 1.0,
            yscale: 1.0,
            ..ViewSpan::default()
        }];
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                view_tmax: state.tmax,
                tracks: vec![t],
                ..ViewMeta::default()
            },
            ys: vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5],
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let colors = palette(Theme::Light, WiggleTheme::SnufflerWeb);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });
        paint(&mut target, &state, &view, &colors, &layout, false, false);
        let svg = target.finish();
        let xs: Vec<f32> = svg
            .split("<polyline points=\"")
            .skip(1)
            .flat_map(|rest| {
                rest.split('"')
                    .next()
                    .unwrap_or("")
                    .split(' ')
                    .filter_map(|pair| pair.split(',').next().and_then(|x| x.parse::<f32>().ok()))
            })
            .collect();
        assert!(!xs.is_empty(), "the span must produce a polyline");
        let min_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
        let max_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            min_x < 0.0,
            "the point before the left edge is emitted (got min x {min_x})"
        );
        assert!(
            max_x > 640.0,
            "the point past the right edge is emitted (got max x {max_x})"
        );
    }

    #[test]
    fn track_labels_are_drawn_verbatim_as_the_bridge_composed_them() {
        let state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_060.0,
            ..AppState::default()
        };
        // The bridge composes snuffler's whole label — sort value first, then
        // the space-joined nslc (`plabel`, pile_viewer.py:3521) — and the
        // painter must not re-compose or decorate it.
        let track = |cha: &str, annotation: &str| ViewTrack {
            net: "GE".to_owned(),
            sta: "APE".to_owned(),
            cha: cha.to_owned(),
            label: if annotation.is_empty() {
                format!("GE APE  {cha}")
            } else {
                format!("{annotation} GE APE  {cha}")
            },
            sort_annotation: annotation.to_owned(),
            ..ViewTrack::default()
        };
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                view_tmax: state.tmax,
                tracks: vec![track("HHZ", "12.3 km"), track("HHN", "")],
                ..ViewMeta::default()
            },
            ys: Vec::new(),
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let colors = palette(Theme::Dark, WiggleTheme::SnufflerQt);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });

        paint(&mut target, &state, &view, &colors, &layout, false, true);
        let svg = target.finish();
        // The annotated label appears whole; the unannotated one carries no
        // decoration — in particular no "no coords" placeholder, which snuffler
        // never writes.
        assert!(svg.contains("12.3 km GE APE  HHZ"), "{svg}");
        assert!(svg.contains(">GE APE  HHN<"), "{svg}");
        assert!(!svg.contains("no coords"), "{svg}");
    }
}
