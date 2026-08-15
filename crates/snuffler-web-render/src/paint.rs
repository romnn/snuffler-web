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
/// An empty key (single-location data) falls back to the track label so the Color
/// lines theme stays varied where snuffler would paint every trace alike — a
/// prettiness choice, see `color-traces-theming` in `tests/parity/known_gaps.toml`.
/// The `"*"` a span-less grouped row offers as its location is no key either, so it
/// falls back the same way. Under the Snuffler Qt theme there is no fallback: that
/// theme's whole claim is to look like snuffler, which does paint them alike.
fn color_key(track: &ViewTrack, loc: &str, subsort: Subsort, exact: bool) -> String {
    let key = match subsort {
        Subsort::Nslc | Subsort::NscGroupL | Subsort::SncGroupL => loc.to_owned(),
        Subsort::Nscl | Subsort::Sncl | Subsort::Lnsc => track.cha.clone(),
        Subsort::Cnsl => format!("{}.{}.{}", track.net, track.sta, loc),
    };
    if !exact && (key.is_empty() || key == "*") {
        track.label.clone()
    } else {
        key
    }
}

/// Whether a theme reproduces snuffler's own colouring rather than this frontend's.
const fn colors_exactly_like_qt(theme: WiggleTheme) -> bool {
    matches!(theme, WiggleTheme::SnufflerQt)
}

/// The Snuffler Web stroke ramp: seven hues, each at the lightness its canvas needs.
///
/// Built in `OKLab` and fitted rather than picked by eye, against four measurements at
/// once: how far apart the colours are, how far apart they stay for a colour-blind
/// reader, how each reads as ink on its canvas, and how each reads crossing somebody
/// else's row band.
///
/// The hues are evenly spaced from a common origin, so index for index the two modes
/// are the same hue and only lightness moves — a coloring key keeps its identity when
/// the canvas flips. Within each mode the lightness **alternates** between two rungs,
/// and that zig-zag is the part a hand-picked ramp tends to miss: protanopia and
/// deuteranopia collapse the red-green axis, so a palette separating only by hue
/// separates by almost nothing. Neighbouring hues at different lightness stay apart on
/// an axis no dichromacy removes. Worst-case pairwise distance under simulated
/// protanopia, deuteranopia and tritanopia comes out at **3.9 light and 6.3 dark**,
/// against 1.8 and 2.5 for the ramp this replaces, where 0 means two traces are the
/// same colour.
///
/// Two judgement calls are worth naming. Chroma sits at 86% of what the gamut allows
/// rather than at the edge: the last 14% buys about half a point of separation and
/// costs the electric, buzzing colours that make a stack unpleasant to sit in front
/// of. And the contrast floor is 3:1, WCAG's standard for *graphical objects*, not the
/// 4.5:1 for body text — applying the text figure to a one-pixel line was what forced
/// an earlier version of this ramp down into browns and olives to satisfy a rule that
/// was never about lines. Strokes land at 3.90-6.47 against the light canvas and
/// 5.71-10.33 against the dark one, and the worst stroke-over-any-band pairing — any
/// stroke, any band, since bands key on station and strokes on location — holds at
/// 3.38 and 4.41.
///
/// The *order* is fitted too, and it matters more than it looks: keys are numbered in
/// sorted order, so a three-location dataset only ever sees the first three entries.
/// Ordering by hue put an olive there. These are ordered greedily by worst-case
/// separation instead — blue, crimson, orange leads at 10.4 against 5.2 for the
/// hue order, and the one genuinely weak colour, the olive a yellow becomes when a
/// white canvas forces it dark, is demoted to seventh where most data never reaches it.
const fn web_stroke_palette(theme: Theme) -> [(u8, u8, u8); 7] {
    match theme {
        Theme::Light => [
            (0x38, 0x71, 0xED),
            (0xAD, 0x26, 0x54),
            (0xAE, 0x66, 0x26),
            (0x2D, 0x8E, 0x63),
            (0x8E, 0x28, 0xB4),
            (0x22, 0x6E, 0x7C),
            (0x68, 0x67, 0x1E),
        ],
        Theme::Dark => [
            (0xA1, 0xBE, 0xF6),
            (0xF3, 0x5B, 0x86),
            (0xF7, 0xAA, 0x70),
            (0x4A, 0xDC, 0x9B),
            (0xC8, 0x66, 0xF1),
            (0x39, 0xA9, 0xBC),
            (0x9F, 0x9E, 0x33),
        ],
    }
}

/// How much of a station's colour survives into the band behind its rows.
///
/// Dark takes a larger share because the same step buys less contrast against a dark
/// canvas. Both stay a whisper against their canvas where snuffler's boxes run
/// 1.10-3.30, and both are low enough that a stroke crossing *any* band — not only
/// its own, since the band is keyed on station and the stroke on location — holds at
/// 3.38 and 4.41. Snuffler's own worst case there is 1.01: a butter stroke on a plum
/// box, invisible.
const fn web_band_share(theme: Theme) -> f32 {
    match theme {
        Theme::Light => 0.09,
        Theme::Dark => 0.13,
    }
}

/// Band colour per station, for the rows behind a stack of traces.
///
/// Keyed on `net.sta` rather than on the stroke's own coloring key, which is the
/// mistake the first version of this made: under a location-grouped subsort every
/// row's first trace is location `00`, so every band came out the same red and the
/// whole canvas took a wash that said nothing.
///
/// A station is the right key because it is what the rows *are* grouped into. Sorted
/// by distance, a stack runs station by station, three channels at a time; banding on
/// station makes those blocks visible at a glance and gives the eye something to
/// track when scrolling. The stroke keeps the finer key — location, or whatever the
/// subsort colours by — so background and line encode two different things and
/// neither is redundant.
fn station_band_colors(
    visible: &[ViewTrack],
    pal: &Palette,
    theme: Theme,
) -> HashMap<String, Color32> {
    let mut keys: Vec<String> = visible
        .iter()
        .map(|track| format!("{}.{}", track.net, track.sta))
        .collect();
    keys.sort();
    keys.dedup();
    let ramp = web_stroke_palette(theme);
    let share = web_band_share(theme);
    keys.into_iter()
        .enumerate()
        .map(|(index, key)| {
            let (red, green, blue) = ramp
                .get(index % ramp.len())
                .copied()
                .unwrap_or((0x88, 0x88, 0x88));
            let band = web_toward_canvas(Color32::from_rgb(red, green, blue), pal, share);
            (key, band)
        })
        .collect()
}

/// The stroke's hue, pulled `keep` of the way back to the canvas.
fn web_toward_canvas(stroke: Color32, pal: &Palette, keep: f32) -> Color32 {
    let mix = |ink: u8, ground: u8| {
        (f32::from(ink) * keep + f32::from(ground) * (1.0 - keep)).round() as u8
    };
    Color32::from_rgb(
        mix(stroke.r(), pal.bg.r()),
        mix(stroke.g(), pal.bg.g()),
        mix(stroke.b(), pal.bg.b()),
    )
}

/// The coloring keys one row contributes to the palette numbering: one per
/// painted span, since a row can carry traces with different keys, or the row's
/// own identity's when it has no spans — so the numbering does not shift when a
/// track happens to have no data in the window.
fn track_color_keys(track: &ViewTrack, subsort: Subsort, exact: bool) -> Vec<String> {
    if track.spans.is_empty() {
        vec![color_key(track, &track.loc, subsort, exact)]
    } else {
        track
            .spans
            .iter()
            .map(|span| color_key(track, &span.loc, subsort, exact))
            .collect()
    }
}

/// Stroke color per coloring key, assigned the way snuffler assigns its colors:
/// the distinct keys sorted, then numbered into the palette in that order
/// (`color_lookup` over the sorted `pile.gather_keys`). Numbering runs over the
/// painted tracks' keys rather than the whole pile, which only shows when track
/// paging hides a key entirely.
///
/// The Snuffler Qt theme takes snuffler's own `graph_colors` (`plot/__init__.py:95`)
/// and, past the seventh key, snuffler's own black — `pyrocko.plot.color` indexes
/// that list and returns `(0, 0, 0)` for anything beyond it rather than wrapping.
/// That theme only ever renders on white (`WiggleTheme::forces_light`), which is the
/// canvas `graph_colors` was picked for. Snuffler Web brings its own ramp, chosen for
/// whichever canvas is up.
fn stroke_color_map(
    visible: &[ViewTrack],
    subsort: Subsort,
    wiggle: WiggleTheme,
    theme: Theme,
) -> HashMap<String, Color32> {
    // scarletred2, skyblue3, chameleon3, orange2, plum2, chocolate2, butter2.
    const QT_GRAPH_COLORS: [(u8, u8, u8); 7] = [
        (204, 0, 0),
        (32, 74, 135),
        (78, 154, 6),
        (245, 121, 0),
        (117, 80, 123),
        (193, 125, 17),
        (237, 212, 0),
    ];
    let exact = colors_exactly_like_qt(wiggle);
    let mut keys: Vec<String> = visible
        .iter()
        .flat_map(|track| track_color_keys(track, subsort, exact))
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .enumerate()
        .map(|(index, key)| {
            let (red, green, blue) = if exact {
                QT_GRAPH_COLORS.get(index).copied().unwrap_or((0, 0, 0))
            } else {
                let ramp = web_stroke_palette(theme);
                // With the modulo the palette fetch cannot miss; the grey is a
                // defensive fallback, not a code path.
                ramp.get(index % ramp.len())
                    .copied()
                    .unwrap_or((0x88, 0x88, 0x88))
            };
            (key, Color32::from_rgb(red, green, blue))
        })
        .collect()
}

/// Snuffler's box styles: the light Tango '1' fill at `box_alpha = 100`, outlined in
/// the matching '3' (`box_styles`, `pile_viewer.py:198`).
///
/// Snuffler only ever draws these on white and the pairing is built for it — a pale
/// wash under a dark outline. There is no upstream dark mode to copy, so the dark
/// variant keeps snuffler's palette and changes only what the canvas forces it to.
/// Scaling all three channels by one factor leaves hue and saturation untouched
/// (`S = (max - min) / max` is invariant under it) and moves only value, so these are
/// the same seven Tango colours, dimmed until they sit on black the way the originals
/// sit on white. The outline swaps to the light '1' member of the same hue, which
/// reads against dark as the dark '3' reads against white.
///
/// Light is the parity claim and is exact; dark is this adaptation, recorded in
/// `qt-theme-residual-chrome` in `known_gaps.toml`.
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

/// Snuffler's gap/lap tolerance, in samples (`gap_lap_tolerance`, `pile_viewer.py:159`).
const GAP_LAP_TOLERANCE: f64 = 5.0;

/// Length of a time-axis tick mark (`ticklen`, `TimeAx.drawit`, `pile_viewer.py:547`).
const QT_TICK_LEN: f32 = 10.0;

/// A span's channel identity: network, station, location, channel — the leading
/// four elements of the `full_id` snuffler orders its box styles by.
type SpanNslc = (String, String, String, String);

/// Box style index per painted span, assigned the way snuffler assigns box styles
/// (`determine_box_styles`, `pile_viewer.py:2947`).
///
/// Snuffler sorts every trace by `full_id` — network, station, location, channel,
/// then start time — and advances the style whenever the next trace is not a
/// continuation of the last: a different channel, a different sample rate, or a
/// gap wider than `GAP_LAP_TOLERANCE` samples. The colour is therefore a property
/// of the *trace*, not of the row it lands on, and the two consequences are what
/// the boxes are for. Co-located instruments sharing a location-grouped row get
/// different colours, which is the only thing telling them apart there; and a
/// channel arriving in several pieces changes colour at each gap, which is how the
/// boxes show segmentation at all.
///
/// Keying on the row index instead gave every span of a row one colour, so a
/// three-location row painted the same alpha-100 fill three times and reinforced a
/// single hue. Rows do still deepen when they carry several traces — Qt's boxes
/// overlay too, measurably — but there the hues blend instead of compounding.
///
/// Like `stroke_color_map`, the numbering runs over the painted spans rather than
/// the whole pile, so an index can differ from Qt's where paging hides traces.
fn box_style_indices(
    visible: &[ViewTrack],
    data_tmin: f64,
    view: &ViewData,
) -> HashMap<(usize, usize), usize> {
    struct Entry {
        nslc: SpanNslc,
        tmin: f64,
        tmax: f64,
        dt: f64,
        spacing: f64,
        at: (usize, usize),
    }

    let mut entries: Vec<Entry> = Vec::new();
    for (k, track) in visible.iter().enumerate() {
        for (s, span) in track.spans.iter().enumerate() {
            let Some((tmin, tmax)) = span_time_bounds(data_tmin, view, track, span) else {
                continue;
            };
            // A decimated payload carries no sample rate of its own (`dt <= 0`
            // marks the interleaved forms), so the drawn point spacing stands in
            // for it — only ever to size the gap tolerance below.
            let spacing = if span.dt > 0.0 {
                span.dt
            } else {
                let points = span.count.max(2) - 1;
                (tmax - tmin) / points as f64
            };
            entries.push(Entry {
                nslc: (
                    track.net.clone(),
                    track.sta.clone(),
                    span.loc.clone(),
                    track.cha.clone(),
                ),
                tmin,
                tmax,
                dt: span.dt,
                spacing,
                at: (k, s),
            });
        }
    }
    entries.sort_by(|a, b| a.nslc.cmp(&b.nslc).then(a.tmin.total_cmp(&b.tmin)));

    let mut out = HashMap::new();
    let mut style = 0usize;
    let mut previous: Option<(&SpanNslc, f64, f64)> = None;
    for entry in &entries {
        if let Some((nslc, tmax, dt)) = previous {
            // Exact, as snuffler compares it (`other.deltat == tr.deltat`): these are
            // declared sample rates carried through unchanged, not measured ones, so
            // two segments of a channel either report the same rate or are genuinely
            // a rate change — which is one of the things that starts a new style.
            #[expect(clippy::float_cmp, reason = "mirrors snuffler's own == on deltat")]
            let same_rate = dt == entry.dt;
            let continues = *nslc == entry.nslc
                && same_rate
                && (tmax - entry.tmin).abs() < GAP_LAP_TOLERANCE * entry.spacing;
            if !continues {
                style += 1;
            }
        }
        out.insert(entry.at, style);
        previous = Some((&entry.nslc, entry.tmax, entry.dt));
    }
    out
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
    let qt_colors = colors_exactly_like_qt(state.wiggle_theme);
    let stroke_colors = if state.color_traces {
        stroke_color_map(visible, state.subsort, state.wiggle_theme, theme)
    } else {
        HashMap::new()
    };

    target.fill_rect(rect, 0.0, pal.bg);

    if state.show_boxes && !waterfall && state.wiggle_theme == WiggleTheme::SnufflerQt {
        target.push_clip(track_area);
        let box_styles = box_style_indices(visible, data_tmin, view);
        for (k, t) in visible.iter().enumerate() {
            for (s, span) in t.spans.iter().enumerate() {
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
                let (fill, frame) = qt_box_style(box_styles.get(&(k, s)).copied().unwrap_or(0));
                let r = Rect::from_min_max(pos2(x0, top), pos2(x1, bot));
                target.fill_rect(r, 0.0, fill);
                target.line(r.left_top(), r.right_top(), 1.0, frame);
                target.line(r.right_top(), r.right_bottom(), 1.0, frame);
                target.line(r.right_bottom(), r.left_bottom(), 1.0, frame);
                target.line(r.left_bottom(), r.left_top(), 1.0, frame);
            }
        }
        target.pop_clip();
    } else if state.show_boxes && !waterfall {
        // Snuffler Web's answer to the trace box: one band per row, in its station's
        // colour, drawn across the extent its traces actually cover. Opaque, so
        // co-located traces sharing a row cannot stack it into a slab the way
        // snuffler's translucent boxes do — the failure that made every later decision
        // about keeping the wiggle visible through its own background.
        //
        // Drawn per span rather than across the full width, which is what makes it a
        // *box* and not a stripe: a gap in the data is a gap in the band, as it is in
        // Qt. On continuous data the spans meet and it reads as one clean row.
        target.push_clip(track_area);
        let bands = station_band_colors(visible, pal, theme);
        for (k, t) in visible.iter().enumerate() {
            let top = top0 + k as f32 * row_h;
            let Some(band) = bands.get(&format!("{}.{}", t.net, t.sta)) else {
                continue;
            };
            for span in &t.spans {
                let Some((t0, t1)) = span_time_bounds(data_tmin, view, t, span) else {
                    continue;
                };
                let x0 = x_of(t0).clamp(track_area.left(), track_area.right());
                let x1 = x_of(t1).clamp(track_area.left(), track_area.right());
                if x1 <= x0 {
                    continue;
                }
                target.fill_rect(
                    Rect::from_min_max(pos2(x0, top), pos2(x1, top + row_h)),
                    0.0,
                    *band,
                );
            }
        }
        target.pop_clip();
    }

    // time grid + axis labels
    let tinc = timeaxis::nice_tinc(window, f64::from((rect.width() / 90.0).max(2.0)));
    let axis_top = track_area.bottom();
    // Clear of the tick marks, as snuffler pads its labels past them (`pad`, `TimeAx`).
    let tick_label_y = axis_top + QT_TICK_LEN + 2.0;
    let context_label_y = axis_top + QT_TICK_LEN + 21.0;
    let axis_pad = 10.0;
    for tick in timeaxis::ticks(view_tmin, view_tmax, tinc) {
        let x = x_of(tick);
        if state.show_time_grid {
            target.line(
                pos2(x, track_area.top()),
                pos2(x, track_area.bottom()),
                1.0,
                pal.grid,
            );
        }
        // The tick mark snuffler always draws, grid or no grid: ten pixels down from
        // the top of the axis area (`ticklen`, `TimeAx.drawit`, pile_viewer.py:539).
        target.line(
            pos2(x, axis_top),
            pos2(x, axis_top + QT_TICK_LEN),
            1.0,
            pal.axis_text,
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

        // The waterfall has no wiggle to carry the row's identity, so it keeps an
        // outlined cell whichever theme is on.
        if state.show_boxes && waterfall {
            let r = Rect::from_min_max(
                pos2(rect.left() + 0.5, top + 1.0),
                pos2(rect.right() - 0.5, bot - 1.0),
            );
            target.line(r.left_top(), r.right_top(), 1.0, pal.sep);
            target.line(r.right_top(), r.right_bottom(), 1.0, pal.sep);
            target.line(r.right_bottom(), r.left_bottom(), 1.0, pal.sep);
            target.line(r.left_bottom(), r.left_top(), 1.0, pal.sep);
        } else if !state.show_boxes && k > 0 && state.track_separators() {
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

            let colored_strokes = state.color_traces;
            // One polyline per span, never one across them: the segments of a
            // gapped channel share a row but must not be joined by a line drawn
            // through data that is not there.
            for span in &t.spans {
                let trace_col = if colored_strokes {
                    stroke_colors
                        .get(&color_key(t, &span.loc, state.subsort, qt_colors))
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
        {
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
    use std::collections::{BTreeSet, HashMap};

    use emath::{Rect, Vec2, pos2};
    use snuffler_web_core::bridge::{ViewData, ViewMeta, ViewTrack};
    use snuffler_web_core::state::{AppState, WiggleTheme};

    use crate::draw::SvgTarget;
    use crate::layout::compute_layout;
    use crate::palette::{Theme, palette};

    use super::{
        box_style_indices, color_key, marker_matches_track, paint, stroke_color_map,
        web_band_share, web_stroke_palette, web_toward_canvas,
    };
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
        let map = stroke_color_map(
            tracks.as_slice(),
            subsort,
            WiggleTheme::SnufflerWeb,
            Theme::Light,
        );
        tracks
            .iter()
            .map(|track| {
                map.get(&color_key(track, &track.loc, subsort, false))
                    .copied()
                    .unwrap_or(Color32::TRANSPARENT)
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or([Color32::TRANSPARENT; N])
    }

    /// The box styles for one track's spans, in span order.
    fn box_styles_of(tracks: &[ViewTrack]) -> Vec<Vec<usize>> {
        let view = ViewData {
            meta: ViewMeta::default(),
            ys: Vec::new(),
        };
        let map = box_style_indices(tracks, 0.0, &view);
        tracks
            .iter()
            .enumerate()
            .map(|(k, track)| {
                (0..track.spans.len())
                    .filter_map(|s| map.get(&(k, s)).copied())
                    .collect()
            })
            .collect()
    }

    fn span(loc: &str, t0_rel: f64, count: u64) -> ViewSpan {
        ViewSpan {
            loc: loc.into(),
            t0_rel,
            dt: 1.0,
            count,
            ..ViewSpan::default()
        }
    }

    /// How many distinct box styles each track's spans take, in track order. The
    /// counts *are* the claim these tests make, and asserting on them keeps the
    /// test from indexing into a shape the helper might not have produced.
    fn distinct_box_styles(tracks: &[ViewTrack]) -> Vec<usize> {
        box_styles_of(tracks)
            .iter()
            .map(|styles| styles.iter().collect::<BTreeSet<_>>().len())
            .collect()
    }

    #[test]
    fn co_located_spans_on_one_row_take_different_box_styles() {
        // The defect this pins: keyed on the row index, all three spans took one
        // style, so the row painted the same alpha-100 fill three times over and
        // came out at roughly twice the tint. Snuffler keys on the trace, so a
        // location-grouped row carries three *different* boxes — which is also the
        // only thing distinguishing its members.
        let mut grouped = track("IU", "ANMO", "*", "BHZ", "IU.ANMO.BHZ");
        grouped.spans = vec![
            span("00", 0.0, 100),
            span("10", 0.0, 100),
            span("60", 0.0, 100),
        ];
        assert_eq!(distinct_box_styles(&[grouped]), vec![3]);
    }

    #[test]
    fn contiguous_segments_share_a_style_and_a_gap_starts_a_new_one() {
        // Snuffler advances the style only where a trace stops continuing the last
        // one, so an unbroken channel keeps one colour across its segments and a
        // real gap changes it — which is how the boxes show segmentation.
        let mut joined = track("IU", "ANMO", "00", "BHZ", "IU.ANMO.00.BHZ");
        joined.spans = vec![span("00", 0.0, 100), span("00", 100.0, 100)];
        let mut gapped = track("IU", "COLA", "00", "BHZ", "IU.COLA.00.BHZ");
        gapped.spans = vec![span("00", 0.0, 100), span("00", 500.0, 100)];
        assert_eq!(distinct_box_styles(&[joined, gapped]), vec![1, 2]);
    }

    /// WCAG relative luminance, for the contrast asserts below.
    fn relative_luminance(c: Color32) -> f32 {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (relative_luminance(a), relative_luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn web_theme_keeps_the_wiggle_legible_over_every_band_in_both_modes() {
        // The constraint the first dark theme failed. It is not enough for a stroke to
        // clear its *own* band: rows are read together, and any stroke can end up over
        // any band. Snuffler's own worst case is 1.01 — a butter stroke on a plum band,
        // invisible — which it survives only because its bands are nearly white.
        for theme in [Theme::Light, Theme::Dark] {
            let pal = palette(theme, WiggleTheme::SnufflerWeb);
            let strokes: Vec<Color32> = web_stroke_palette(theme)
                .iter()
                .map(|&(r, g, b)| Color32::from_rgb(r, g, b))
                .collect();
            let bands: Vec<Color32> = strokes
                .iter()
                .map(|stroke| web_toward_canvas(*stroke, &pal, web_band_share(theme)))
                .collect();

            for stroke in &strokes {
                // 3:1 is WCAG's floor for graphical objects. The 4.5:1 body-text
                // figure is the wrong rule for a one-pixel line, and holding this
                // ramp to it forced it into browns to satisfy it.
                let ink = contrast(*stroke, pal.bg);
                assert!(
                    ink >= 3.0,
                    "{theme:?}: stroke {stroke:?} is {ink:.2} on the canvas"
                );
                for band in &bands {
                    let over = contrast(*stroke, *band);
                    assert!(
                        over >= 3.0,
                        "{theme:?}: stroke {stroke:?} is {over:.2} on a band"
                    );
                }
            }
            for band in &bands {
                let shout = contrast(*band, pal.bg);
                assert!(
                    shout < 1.6,
                    "{theme:?}: band {band:?} is {shout:.2}, not a whisper"
                );
            }
        }
    }

    /// Hue angle in `OKLab`, in degrees. Channel ordering is not a usable proxy: an
    /// olive sits exactly where red and green cross, so two shades of the same hue
    /// can order their channels differently.
    fn oklab_hue(color: Color32) -> f32 {
        let linear = |value: u8| {
            let value = f32::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        let (red, green, blue) = (linear(color.r()), linear(color.g()), linear(color.b()));
        let long = (0.412_221_5 * red + 0.536_332_55 * green + 0.051_445_995 * blue).cbrt();
        let medium = (0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue).cbrt();
        let short = (0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue).cbrt();
        let axis_a = 1.977_998_5 * long - 2.428_592_2 * medium + 0.450_593_7 * short;
        let axis_b = 0.025_904_037 * long + 0.782_771_77 * medium - 0.808_675_77 * short;
        axis_b.atan2(axis_a).to_degrees().rem_euclid(360.0)
    }

    #[test]
    fn web_theme_is_one_design_at_two_levels() {
        // Same hue per index in both modes, so a coloring key keeps its identity
        // across a theme switch: only lightness moves, and it moves in opposite
        // directions — ink is darker than its canvas in light, lighter in dark.
        let light = web_stroke_palette(Theme::Light);
        let dark = web_stroke_palette(Theme::Dark);
        let bg_light = palette(Theme::Light, WiggleTheme::SnufflerWeb).bg;
        let bg_dark = palette(Theme::Dark, WiggleTheme::SnufflerWeb).bg;
        for (index, (l, d)) in light.iter().zip(dark.iter()).enumerate() {
            let l = Color32::from_rgb(l.0, l.1, l.2);
            let d = Color32::from_rgb(d.0, d.1, d.2);
            let drift = (oklab_hue(l) - oklab_hue(d))
                .abs()
                .min(360.0 - (oklab_hue(l) - oklab_hue(d)).abs());
            assert!(
                drift < 15.0,
                "hue {index} drifted {drift:.1} deg between modes"
            );
            assert!(
                relative_luminance(l) < relative_luminance(bg_light),
                "light stroke {index} is not ink"
            );
            assert!(
                relative_luminance(d) > relative_luminance(bg_dark),
                "dark stroke {index} is not ink"
            );
        }
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
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerQt,
            Theme::Light,
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
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerQt,
            Theme::Light,
        );
        let web = stroke_color_map(
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerWeb,
            Theme::Light,
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
            &tracks,
            Subsort::NscGroupL,
            WiggleTheme::SnufflerWeb,
            Theme::Light,
        );
        let [grouped, _] = &tracks;
        // Two locations across the stack, so exactly two keys get numbered.
        assert_eq!(map.len(), 2);
        let first = map.get(&color_key(grouped, "00", Subsort::NscGroupL, false));
        let second = map.get(&color_key(grouped, "10", Subsort::NscGroupL, false));
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
