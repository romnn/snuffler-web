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
use snuffler_web_core::markers::{MARKER_COLORS, Marker};
use snuffler_web_core::state::{AppState, Subsort, WiggleTheme};
use snuffler_web_core::timeaxis;

use crate::draw::DrawTarget;
use crate::layout::Layout;
use crate::palette::{Palette, Theme};

const LABEL_PT: f32 = 12.5;
const AXIS_PT: f32 = 12.5;
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
/// An empty key (single-location data) deliberately falls back to the track
/// label so the Color lines theme stays varied where snuffler would paint every
/// trace alike — a signed-off prettiness choice, see `color-traces-theming` in
/// `tests/parity/known_gaps.toml`. The `"*"` a span-less grouped row offers as
/// its location is no key either, so it falls back the same way.
fn color_key(track: &ViewTrack, loc: &str, subsort: Subsort) -> String {
    let key = match subsort {
        Subsort::Nslc | Subsort::NscGroupL | Subsort::SncGroupL => loc.to_owned(),
        Subsort::Nscl | Subsort::Sncl | Subsort::Lnsc => track.cha.clone(),
        Subsort::Cnsl => format!("{}.{}.{}", track.net, track.sta, loc),
    };
    if key.is_empty() || key == "*" {
        track.label.clone()
    } else {
        key
    }
}

/// The coloring keys one row contributes to the palette numbering: one per
/// painted span, since a row can carry traces with different keys, or the row's
/// own identity's when it has no spans — so the numbering does not shift when a
/// track happens to have no data in the window.
fn track_color_keys(track: &ViewTrack, subsort: Subsort) -> Vec<String> {
    if track.spans.is_empty() {
        vec![color_key(track, &track.loc, subsort)]
    } else {
        track
            .spans
            .iter()
            .map(|span| color_key(track, &span.loc, subsort))
            .collect()
    }
}

/// Stroke color per coloring key, assigned the way snuffler assigns its Tango
/// colors: the distinct keys sorted, then numbered into the palette in that
/// order (`color_lookup` over the sorted `pile.gather_keys`). Only the
/// assignment mirrors Qt; the palette is deliberately this frontend's own, tuned
/// to its themes. Numbering runs over the painted tracks' keys rather than the
/// whole pile, which only shows when track paging hides a key entirely.
fn stroke_color_map(visible: &[ViewTrack], subsort: Subsort) -> HashMap<String, Color32> {
    const PALETTE: [(u8, u8, u8); 7] = [
        (0xCC, 0x4A, 0x4A),
        (0x3A, 0x86, 0xC8),
        (0x3F, 0xA6, 0x5E),
        (0xC9, 0x92, 0x2E),
        (0x8E, 0x55, 0xC9),
        (0x2F, 0xA8, 0xA8),
        (0xC8, 0x5F, 0x9C),
    ];
    let mut keys: Vec<String> = visible
        .iter()
        .flat_map(|track| track_color_keys(track, subsort))
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .enumerate()
        .map(|(index, key)| {
            // With the modulo the palette fetch cannot miss; the grey is a
            // defensive fallback, not a code path.
            let (red, green, blue) = PALETTE
                .get(index % PALETTE.len())
                .copied()
                .unwrap_or((0x88, 0x88, 0x88));
            (key, Color32::from_rgb(red, green, blue))
        })
        .collect()
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

/// The distinct amplitude scales a row draws against, in span order: one for most
/// rows, several only where a location-grouped row carries traces scaled
/// differently. Zero lines and scale annotations are drawn per scale, not per
/// span — the segments of one gapped channel share a scale and must not draw it
/// twice.
fn distinct_span_scales(spans: &[ViewSpan]) -> Vec<&ViewSpan> {
    let mut seen: Vec<(f32, f32, f32, f32)> = Vec::new();
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

/// Time bounds of one run of samples. A track holds one of these per segment of its
/// channel, so a gapped channel gets a box per piece rather than one box bridging
/// the gap.
fn span_time_bounds(
    data_tmin: f64,
    view: &ViewData,
    track: &ViewTrack,
    span: &ViewSpan,
) -> Option<(f64, f64)> {
    let t0 = data_tmin + span.t0_rel;
    if track.nf > 0 {
        let time_bins = span.count.checked_div(track.nf)?;
        if time_bins == 0 {
            return None;
        }
        return Some((t0, t0 + time_bins.saturating_sub(1) as f64 * span.dt));
    }
    if span.dt > 0.0 && span.count > 0 {
        return Some((t0, t0 + span.count.saturating_sub(1) as f64 * span.dt));
    }
    if span.dt == 0.0 && span.count > 0 {
        let points = view.points(span, 2);
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
        return Some((t0 + tmin, t0 + tmax));
    }
    None
}

fn point_pair_bounds(points: &[f32], lo: f32, hi: f32) -> (usize, usize) {
    let pairs = points.len() / 2;
    let lower = |target: f32| {
        let mut left = 0usize;
        let mut right = pairs;
        while left < right {
            let mid = left + (right - left) / 2;
            let t = points.get(mid * 2).copied().unwrap_or(f32::INFINITY);
            if t < target {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        left
    };
    let start = lower(lo).saturating_sub(1);
    let end = lower(hi).saturating_add(1).min(pairs);
    (start * 2, end * 2)
}

/// Render the persistent waveform content (everything except the transient hover
/// crosshair) to any [`DrawTarget`]. This is the snuffler `drawit` analogue: the
/// on-screen and SVG export backends drive identical geometry through it.
#[expect(
    clippy::too_many_lines,
    reason = "cohesive draw routine; splitting would obscure the layered draw order"
)]
pub fn paint(
    target: &mut dyn DrawTarget,
    state: &AppState,
    view: &ViewData,
    pal: &Palette,
    lay: &Layout<'_>,
    theme: Theme,
    waterfall: bool,
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
    let in_x = |x: f32| x >= rect.left() - point_margin && x <= rect.right() + point_margin;

    // Stroke colors by coloring key; the row tints and the colored strokes both
    // look their key up here.
    let stroke_colors = if state.color_traces
        && (theme_uses_row_tints(state.wiggle_theme)
            || theme_uses_colored_strokes(state.wiggle_theme))
    {
        stroke_color_map(visible, state.subsort)
    } else {
        HashMap::new()
    };

    target.fill_rect(rect, 0.0, pal.bg);

    if state.show_boxes && !waterfall && state.wiggle_theme == WiggleTheme::SnufflerQt {
        target.push_clip(track_area);
        for (k, t) in visible.iter().enumerate() {
            for span in &t.spans {
                let Some((t0, t1)) = span_time_bounds(data_tmin, view, t, span) else {
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
                let r = Rect::from_min_max(pos2(x0, top), pos2(x1, bot));
                target.fill_rect(r, 0.0, fill);
                target.line(r.left_top(), r.right_top(), 1.0, frame);
                target.line(r.right_top(), r.right_bottom(), 1.0, frame);
                target.line(r.right_bottom(), r.left_bottom(), 1.0, frame);
                target.line(r.left_bottom(), r.left_top(), 1.0, frame);
            }
        }
        target.pop_clip();
    } else if state.color_traces && !waterfall && theme_uses_row_tints(state.wiggle_theme) {
        target.push_clip(track_area);
        // This branch's guard is part of the `stroke_colors` compute condition, so
        // the map is populated here and a row's key cannot miss it.
        for (k, t) in visible.iter().enumerate() {
            let top = top0 + k as f32 * row_h;
            // The row tint is one colour for the whole row, so it takes the
            // first trace's key — this frontend's own theme, not something
            // snuffler has an opinion about.
            let key = t.spans.first().map_or_else(
                || color_key(t, &t.loc, state.subsort),
                |span| color_key(t, &span.loc, state.subsort),
            );
            let Some(color) = stroke_colors.get(&key) else {
                continue;
            };
            let fill = row_tint(*color, theme, state.wiggle_theme);
            target.fill_rect(
                Rect::from_min_max(
                    pos2(track_area.left(), top),
                    pos2(track_area.right(), top + row_h),
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
    for tick in timeaxis::ticks(view_tmin, view_tmax, tinc) {
        let x = x_of(tick);
        target.line(
            pos2(x, track_area.top()),
            pos2(x, track_area.bottom()),
            1.0,
            pal.grid,
        );
        let label = timeaxis::tick_label(tick, tinc);
        let w = target.measure(&label, AXIS_PT).x;
        if rect.left() + axis_pad < x - w * 0.5 && x + w * 0.5 < rect.right() - axis_pad {
            target.text(
                pos2(x, tick_label_y),
                Align2::CENTER_TOP,
                &label,
                AXIS_PT,
                pal.axis_text,
            );
        }
    }
    target.text(
        pos2(rect.left() + axis_pad, context_label_y),
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
                pos2(rect.left() + 0.5, top + 1.0),
                pos2(rect.right() - 0.5, bot - 1.0),
            );
            target.line(r.left_top(), r.right_top(), 1.0, pal.sep);
            target.line(r.right_top(), r.right_bottom(), 1.0, pal.sep);
            target.line(r.right_bottom(), r.left_bottom(), 1.0, pal.sep);
            target.line(r.left_bottom(), r.left_top(), 1.0, pal.sep);
        } else if !state.show_boxes && k > 0 {
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
            // "Clip traces" keeps each trace inside its own box; off lets it spill.
            let (cl_top, cl_bot) = if state.clip_traces {
                (top + 1.0, bot - 1.0)
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
            let y_of = |span: &ViewSpan, v: f32| {
                let half = span.yscale.max(1e-12);
                let amp = row_h * 0.45 * state.gain / half;
                mid - (v - span.ymean) * amp
            };

            // A row with no data in the window has no entries here, having no
            // projection to draw zero lines or scale annotations against.
            let scale_spans = if state.show_zero_lines || state.show_scale_ranges {
                distinct_span_scales(&t.spans)
            } else {
                Vec::new()
            };

            if state.show_zero_lines {
                for &span in &scale_spans {
                    let yz = y_of(span, 0.0);
                    // Only inside this row — snuffler's own guard (`vmin <= v <=
                    // vmax`): a scale whose zero level lies beyond the row's box
                    // draws no line rather than a stray one across a neighbour.
                    if yz < top || yz > bot {
                        continue;
                    }
                    target.line(
                        pos2(track_area.left(), yz),
                        pos2(track_area.right(), yz),
                        1.0,
                        pal.zero,
                    );
                }
            }

            let colored_strokes =
                state.color_traces && theme_uses_colored_strokes(state.wiggle_theme);
            // One polyline per span, never one across them: the segments of a
            // gapped channel share a row but must not be joined by a line drawn
            // through data that is not there.
            for span in &t.spans {
                let trace_col = if colored_strokes {
                    stroke_colors
                        .get(&color_key(t, &span.loc, state.subsort))
                        .copied()
                        .unwrap_or(pal.trace)
                } else {
                    pal.trace
                };
                if span.dt > 0.0 {
                    let samples = view.samples(span);
                    if samples.len() >= 2 {
                        let base_abs = data_tmin + span.t0_rel;
                        let start_idx = ((view_tmin - time_margin - base_abs) / span.dt)
                            .floor()
                            .max(0.0) as usize;
                        let end_idx = ((view_tmax + time_margin - base_abs) / span.dt)
                            .ceil()
                            .clamp(0.0, samples.len() as f64)
                            as usize;
                        let points: Vec<_> = samples
                            .get(start_idx..end_idx)
                            .unwrap_or(&[])
                            .iter()
                            .enumerate()
                            .filter_map(|(i, &v)| {
                                let t_abs = base_abs + (start_idx + i) as f64 * span.dt;
                                let x = x_of(t_abs);
                                in_x(x).then(|| pos2(x, y_of(span, v)))
                            })
                            .collect();
                        if points.len() >= 2 {
                            target.polyline(points, 1.0, trace_col);
                        }
                    } else if let Some(&v) = samples.first() {
                        let t_abs = data_tmin + span.t0_rel;
                        let half = 0.5 * span.dt;
                        let x0 = x_of(t_abs - half);
                        let x1 = x_of(t_abs + half);
                        target.line(
                            pos2(x0, y_of(span, v)),
                            pos2(x1, y_of(span, v)),
                            1.0,
                            trace_col,
                        );
                    }
                } else if span.dt == 0.0 {
                    let points_data = view.points(span, 2);
                    if points_data.len() >= 4 {
                        let lo_rel = (view_tmin - time_margin - data_tmin - span.t0_rel) as f32;
                        let hi_rel = (view_tmax + time_margin - data_tmin - span.t0_rel) as f32;
                        let (start_pair, end_pair) = point_pair_bounds(points_data, lo_rel, hi_rel);
                        let points: Vec<_> = points_data
                            .get(start_pair..end_pair)
                            .unwrap_or(&[])
                            .chunks_exact(2)
                            .filter_map(|pair| match pair {
                                [px, py] => {
                                    let x = x_of(data_tmin + span.t0_rel + f64::from(*px));
                                    in_x(x).then(|| pos2(x, y_of(span, *py)))
                                }
                                _ => None,
                            })
                            .collect();
                        if points.len() >= 2 {
                            target.polyline(points, 1.0, trace_col);
                        }
                    }
                } else {
                    let envelope = view.points(span, 3);
                    for triple in envelope.chunks_exact(3) {
                        let [t0, ylo, yhi] = triple else {
                            continue;
                        };
                        let x = x_of(data_tmin + span.t0_rel + f64::from(*t0));
                        if !in_x(x) {
                            continue;
                        }
                        target.line(
                            pos2(x, y_of(span, *ylo)),
                            pos2(x, y_of(span, *yhi)),
                            1.0,
                            trace_col,
                        );
                    }
                }
            }
            target.pop_clip();

            if state.show_scale_axes {
                let xa = rect.right() - 2.0;
                target.line(pos2(xa, top + 3.0), pos2(xa, bot - 3.0), 1.0, pal.sep);
            }
            if state.show_scale_ranges {
                // One pair of numbers per distinct scale on the row, stepped left of
                // the one before it. Snuffler steps its annotations too, but by a
                // running max of label widths, which overlaps them at three scales
                // or more; the sum here keeps every pair readable and agrees with
                // Qt on the one- and two-scale rows that actually occur.
                let mut uoff = 0.0;
                for &span in &scale_spans {
                    let high = format!("{:+.2e}", span.ymax);
                    let low = format!("{:+.2e}", span.ymin);
                    let x = rect.right() - 6.0 - uoff;
                    target.text(
                        pos2(x, top + 3.0),
                        Align2::RIGHT_TOP,
                        &high,
                        SCALE_PT,
                        pal.axis_text,
                    );
                    target.text(
                        pos2(x, bot - 3.0),
                        Align2::RIGHT_BOTTOM,
                        &low,
                        SCALE_PT,
                        pal.axis_text,
                    );
                    let width = target
                        .measure(&high, SCALE_PT)
                        .x
                        .max(target.measure(&low, SCALE_PT).x);
                    uoff += width + 10.0;
                }
            }
        }

        // NSLC label with a translucent background, followed by what the active
        // sort ordered this track by when the bridge annotated it.
        let label = if t.sort_annotation.is_empty() {
            t.label.clone()
        } else {
            format!("{} ({})", t.label, t.sort_annotation)
        };
        let label_pos = pos2(track_area.left() + 5.0, top + 3.0);
        let bg = Rect::from_min_size(label_pos, target.measure(&label, LABEL_PT)).expand(3.0);
        if !state.color_traces || !theme_uses_row_tints(state.wiggle_theme) {
            target.fill_rect(bg, 3.0, pal.label_bg);
        }
        target.text(label_pos, Align2::LEFT_TOP, &label, LABEL_PT, pal.axis_text);
    }
    target.pop_clip();

    target.push_clip(track_area);
    draw_markers(target, state, track_area, top0, visible, row_h, &x_of);
    if let Some((a, b)) = state.pending_span {
        let r = Rect::from_min_max(
            pos2(x_of(a), track_area.top()),
            pos2(x_of(b), track_area.bottom()),
        );
        target.fill_rect(r, 0.0, Color32::from_rgba_unmultiplied(52, 101, 164, 40));
    }
    // Floating pick in batch/interactive picking mode: a vertical line (instant)
    // or span rect (drag), coloured by the pick's kind. The x-range guard skips
    // the un-hovered initial state (tmin==0 maps far off-screen).
    if let Some(pk) = &state.picking {
        let x0 = x_of(pk.tmin);
        if x0 >= track_area.left() - 2.0 && x0 <= track_area.right() + 2.0 {
            let col = MARKER_COLORS
                .get((pk.kind & 7) as usize)
                .copied()
                .unwrap_or(Color32::from_rgb(204, 0, 0));
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

/// Whether an NSLC-scoped marker belongs on a row, matched the way snuffler's
/// `track_patterns` match: `*` (or an empty element) on the marker's side takes
/// any value, and `*` on the row's side — the location a grouped row carries —
/// takes any marker. Matching the identity rather than the spans keeps a marker
/// on its row even when the marker's own location has no data in the window,
/// exactly as Qt's patterns do.
fn marker_matches_track(nslc: &(String, String, String, String), t: &ViewTrack) -> bool {
    let f = |pat: &str, val: &str| pat.is_empty() || pat == "*" || val == "*" || pat == val;
    f(&nslc.0, &t.net) && f(&nslc.1, &t.sta) && f(&nslc.2, &t.loc) && f(&nslc.3, &t.cha)
}

/// Returns the vertical row spans to which an optional NSLC-scoped marker applies.
#[must_use]
pub fn marker_row_spans(
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

fn draw_markers(
    target: &mut dyn DrawTarget,
    state: &AppState,
    area: Rect,
    top0: f32,
    visible: &[ViewTrack],
    row_h: f32,
    x_of: &impl Fn(f64) -> f32,
) {
    for m in &state.markers {
        if !state.marker_kind_visible(m.kind) {
            continue;
        }
        let col = m.color();
        let x0 = x_of(m.tmin);
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
        let w = if m.active {
            5.0
        } else if m.selected {
            4.0
        } else {
            2.0
        };
        for (ytop, ybot) in spans {
            if m.is_span() {
                let x1 = x_of(m.tmax);
                let r = Rect::from_min_max(pos2(x0, ytop), pos2(x1, ybot));
                target.fill_rect(
                    r,
                    0.0,
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 30),
                );
            }
            // Phase-pick uncertainty as translucent ± brackets under the line
            // (snuffler marker.py three-box draw: side bands α50, middle α35).
            if let Some(u) = m.uncertainty
                && u > 0.0
            {
                let x1 = x_of(m.tmax);
                let band = |a: f32, b: f32, alpha: u8| {
                    (
                        Rect::from_min_max(pos2(a.min(b), ytop), pos2(a.max(b), ybot)),
                        Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha),
                    )
                };
                let (rl, cl) = band(x_of(m.tmin - u), x0, 50);
                target.fill_rect(rl, 0.0, cl);
                let (rr, cr) = band(x1, x_of(m.tmax + u), 50);
                target.fill_rect(rr, 0.0, cr);
                let (rm, cm) = band(x0, x1, 35);
                target.fill_rect(rm, 0.0, cm);
            }
            target.line(pos2(x0, ytop), pos2(x0, ybot), w, col);
            if m.is_event {
                let s = 6.0;
                target.polygon(
                    vec![pos2(x0, ytop + s), pos2(x0 - s, ytop), pos2(x0 + s, ytop)],
                    col,
                );
                // The active event marker gets an outlined label box on the axis
                // (snuffler's outline_label) so the current reference stands out.
                if m.active {
                    let box_rect = Rect::from_min_max(
                        pos2(x0 - s - 2.0, ytop - 2.0),
                        pos2(x0 + s + 34.0, ytop + 15.0),
                    );
                    target.line(box_rect.left_top(), box_rect.right_top(), 1.5, col);
                    target.line(box_rect.right_top(), box_rect.right_bottom(), 1.5, col);
                    target.line(box_rect.right_bottom(), box_rect.left_bottom(), 1.5, col);
                    target.line(box_rect.left_bottom(), box_rect.left_top(), 1.5, col);
                }
            }
            if !label.is_empty() {
                target.text(
                    pos2(x0 + 3.0, ytop + 2.0),
                    Align2::LEFT_TOP,
                    &label,
                    LABEL_PT,
                    col,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use emath::{Rect, Vec2, pos2};
    use snuffler_web_core::bridge::{ViewData, ViewMeta, ViewTrack};
    use snuffler_web_core::state::{AppState, WiggleTheme};

    use crate::draw::SvgTarget;
    use crate::layout::compute_layout;
    use crate::palette::{Theme, palette};

    use super::{color_key, marker_matches_track, paint, stroke_color_map};
    use ecolor::Color32;
    use snuffler_web_core::bridge::ViewSpan;
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
        let map = stroke_color_map(tracks.as_slice(), subsort);
        tracks
            .iter()
            .map(|track| {
                map.get(&color_key(track, &track.loc, subsort))
                    .copied()
                    .unwrap_or(Color32::TRANSPARENT)
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or([Color32::TRANSPARENT; N])
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
        let map = stroke_color_map(&tracks, Subsort::NscGroupL);
        let [grouped, _] = &tracks;
        // Two locations across the stack, so exactly two keys get numbered.
        assert_eq!(map.len(), 2);
        let first = map.get(&color_key(grouped, "00", Subsort::NscGroupL));
        let second = map.get(&color_key(grouped, "10", Subsort::NscGroupL));
        assert!(first.is_some());
        assert!(second.is_some());
        assert_ne!(first, second);
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

        paint(&mut target, &state, &view, &colors, &layout, theme, false);
        let svg = target.finish();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("<line"));
        assert!(svg.contains("<text"));
    }

    #[test]
    fn track_labels_carry_the_sort_annotation_in_parentheses() {
        let state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_060.0,
            ..AppState::default()
        };
        let track = |cha: &str, annotation: &str| ViewTrack {
            net: "GE".to_owned(),
            sta: "APE".to_owned(),
            cha: cha.to_owned(),
            label: format!("GE.APE..{cha}"),
            sort_annotation: annotation.to_owned(),
            ..ViewTrack::default()
        };
        let view = ViewData {
            meta: ViewMeta {
                view_tmin: state.tmin,
                view_tmax: state.tmax,
                tracks: vec![track("HHZ", "12.3 km"), track("HHN", "no coords")],
            },
            ys: Vec::new(),
        };
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let colors = palette(Theme::Dark, WiggleTheme::SnufflerQt);
        let mut target = SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });

        paint(
            &mut target,
            &state,
            &view,
            &colors,
            &layout,
            Theme::Dark,
            false,
        );
        let svg = target.finish();
        assert!(svg.contains("GE.APE..HHZ (12.3 km)"), "{svg}");
        assert!(svg.contains("GE.APE..HHN (no coords)"), "{svg}");
    }
}
