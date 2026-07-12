//! Backend-neutral waveform painting pipeline.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "rendering maps between pixel, time, sample, and track coordinates"
)]

use ecolor::Color32;
use emath::{Align2, Rect, pos2};
use snuffler_web_core::bridge::{ViewData, ViewTrack};
use snuffler_web_core::markers::{MARKER_COLORS, Marker};
use snuffler_web_core::state::{AppState, Subsort, WiggleTheme};
use snuffler_web_core::timeaxis;

use crate::draw::DrawTarget;
use crate::layout::Layout;
use crate::palette::{Palette, Theme};

const LABEL_PT: f32 = 12.5;
const AXIS_PT: f32 = 12.5;

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

fn track_time_bounds(data_tmin: f64, view: &ViewData, track: &ViewTrack) -> Option<(f64, f64)> {
    if track.nf > 0 && track.nt > 0 {
        return Some((
            data_tmin + track.t0_rel,
            data_tmin + track.t0_rel + track.nt.saturating_sub(1) as f64 * track.dt,
        ));
    }
    if track.dt > 0.0 && track.count > 0 {
        return Some((
            data_tmin + track.t0_rel,
            data_tmin + track.t0_rel + track.count.saturating_sub(1) as f64 * track.dt,
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
            Some((
                data_tmin + track.t0_rel + tmin,
                data_tmin + track.t0_rel + tmax,
            ))
        });
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

    target.fill_rect(rect, 0.0, pal.bg);

    if state.show_boxes && !waterfall && state.wiggle_theme == WiggleTheme::SnufflerQt {
        target.push_clip(track_area);
        for (k, t) in visible.iter().enumerate() {
            let Some((t0, t1)) = track_time_bounds(data_tmin, view, t) else {
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
        target.pop_clip();
    } else if state.color_traces && !waterfall && theme_uses_row_tints(state.wiggle_theme) {
        target.push_clip(track_area);
        for (k, t) in visible.iter().enumerate() {
            let top = top0 + k as f32 * row_h;
            let fill = row_tint(trace_color(t, state.subsort), theme, state.wiggle_theme);
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
            let t_abs = data_tmin + t.t0_rel;
            let x0 = x_of(t_abs);
            let x1 = x_of(t_abs + t.nt as f64 * t.dt);
            let r = Rect::from_min_max(pos2(x0, top + 1.0), pos2(x1, top + row_h - 1.0));
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
                pos2(track_area.left(), cl_top),
                pos2(track_area.right(), cl_bot),
            ));
            let y_of = |v: f32| mid - (v - t.ymean) * amp;

            if state.show_zero_lines {
                let yz = y_of(0.0);
                target.line(
                    pos2(track_area.left(), yz),
                    pos2(track_area.right(), yz),
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
                    let base_abs = data_tmin + t.t0_rel;
                    let start_idx = ((view_tmin - time_margin - base_abs) / t.dt)
                        .floor()
                        .max(0.0) as usize;
                    let end_idx = ((view_tmax + time_margin - base_abs) / t.dt)
                        .ceil()
                        .clamp(0.0, samples.len() as f64)
                        as usize;
                    let points: Vec<_> = samples
                        .get(start_idx..end_idx)
                        .unwrap_or(&[])
                        .iter()
                        .enumerate()
                        .filter_map(|(i, &v)| {
                            let t_abs = base_abs + (start_idx + i) as f64 * t.dt;
                            let x = x_of(t_abs);
                            in_x(x).then(|| pos2(x, y_of(v)))
                        })
                        .collect();
                    if points.len() >= 2 {
                        target.polyline(points, 1.0, trace_col);
                    }
                } else if let Some(&v) = samples.first() {
                    let t_abs = data_tmin + t.t0_rel;
                    let half = 0.5 * t.dt;
                    let x0 = x_of(t_abs - half);
                    let x1 = x_of(t_abs + half);
                    target.line(pos2(x0, y_of(v)), pos2(x1, y_of(v)), 1.0, trace_col);
                }
            } else if t.dt == 0.0 {
                let start = (t.offset as usize).min(view.ys.len());
                let end =
                    ((t.offset as usize) + (t.count as usize).saturating_mul(2)).min(view.ys.len());
                if let Some(points_data) = view.ys.get(start..end)
                    && points_data.len() >= 4
                {
                    let lo_rel = (view_tmin - time_margin - data_tmin - t.t0_rel) as f32;
                    let hi_rel = (view_tmax + time_margin - data_tmin - t.t0_rel) as f32;
                    let (start_pair, end_pair) = point_pair_bounds(points_data, lo_rel, hi_rel);
                    let points: Vec<_> = points_data
                        .get(start_pair..end_pair)
                        .unwrap_or(&[])
                        .chunks_exact(2)
                        .filter_map(|pair| match pair {
                            [px, py] => {
                                let x = x_of(data_tmin + t.t0_rel + f64::from(*px));
                                in_x(x).then(|| pos2(x, y_of(*py)))
                            }
                            _ => None,
                        })
                        .collect();
                    if points.len() >= 2 {
                        target.polyline(points, 1.0, trace_col);
                    }
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
                        let x = x_of(data_tmin + t.t0_rel + f64::from(*t0));
                        if !in_x(x) {
                            continue;
                        }
                        target.line(pos2(x, y_of(*ylo)), pos2(x, y_of(*yhi)), 1.0, trace_col);
                    }
                }
            }
            target.pop_clip();

            if state.show_scale_axes {
                let xa = rect.right() - 2.0;
                target.line(pos2(xa, top + 3.0), pos2(xa, bot - 3.0), 1.0, pal.sep);
            }
            if state.show_scale_ranges {
                target.text(
                    pos2(rect.right() - 6.0, top + 3.0),
                    Align2::RIGHT_TOP,
                    &format!("{:+.2e}", t.ymax),
                    10.0,
                    pal.axis_text,
                );
                target.text(
                    pos2(rect.right() - 6.0, bot - 3.0),
                    Align2::RIGHT_BOTTOM,
                    &format!("{:+.2e}", t.ymin),
                    10.0,
                    pal.axis_text,
                );
            }
        }

        // NSLC label with a translucent background
        let label_pos = pos2(track_area.left() + 5.0, top + 3.0);
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

/// Render the current wiggle/waterfall view to a standalone SVG string at the
/// given pixel size, reusing the exact [`paint`] geometry (vector traces/axes,
/// the waterfall heatmap embedded as a PNG).
fn marker_matches_track(nslc: &(String, String, String, String), t: &ViewTrack) -> bool {
    let f = |pat: &str, val: &str| pat.is_empty() || pat == "*" || pat == val;
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
    use emath::{Rect, Vec2, pos2};
    use snuffler_web_core::bridge::{ViewData, ViewMeta};
    use snuffler_web_core::state::{AppState, WiggleTheme};

    use crate::draw::SvgTarget;
    use crate::layout::compute_layout;
    use crate::palette::{Theme, palette};

    use super::paint;

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
        let mut target = SvgTarget::new(640.0, 360.0, Vec::new(), |text, size| {
            Vec2::new(text.len() as f32 * size * 0.5, size)
        });

        paint(&mut target, &state, &view, &colors, &layout, theme, false);
        let svg = target.finish();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("<line"));
        assert!(svg.contains("<text"));
    }
}
