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
use snuffler_web_core::markers::{MARKER_COLORS, Marker};
use snuffler_web_core::state::{AppState, Subsort, WiggleTheme};
use snuffler_web_core::timeaxis;

use crate::draw::DrawTarget;
use crate::layout::Layout;
use crate::palette::{Palette, Theme};

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
/// (`color_lookup` over the sorted `pile.gather_keys`).
///
/// The numbering base is `pile_keys` — the whole pile's coloring keys, computed
/// bridge-side exactly as snuffler computes `color_keys` (`pile_viewer.py:1753`) —
/// so a key keeps its colour as the view scrolls or pages. When the bridge sent
/// none (an empty pile, or a unit test exercising the assignment alone), the
/// painted tracks' own keys stand in.
///
/// The Snuffler Qt theme takes snuffler's own `graph_colors` (`plot/__init__.py:95`)
/// and, past the seventh key, snuffler's own black — `pyrocko.plot.color` indexes
/// that list and returns `(0, 0, 0)` for anything beyond it rather than wrapping.
/// That theme only ever renders on white (`WiggleTheme::forces_light`), which is the
/// canvas `graph_colors` was picked for. Snuffler Web brings its own ramp, chosen for
/// whichever canvas is up.
fn stroke_color_map(
    pile_keys: &[String],
    tracks: &[ViewTrack],
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
    let mut keys: Vec<String> = if pile_keys.is_empty() {
        tracks
            .iter()
            .flat_map(|track| track_color_keys(track, subsort, exact))
            .collect()
    } else if exact {
        pile_keys.to_vec()
    } else {
        // Snuffler Web replaces an empty key with the track label (see
        // `color_key`), so the pile-wide empty key never gets looked up there —
        // keeping it would waste a palette slot and shift every later key. The
        // label fallbacks come from the full track universe (every pile row is a
        // track now, occupied or not), so they are as scroll-stable as the keys.
        let mut keys: Vec<String> = pile_keys
            .iter()
            .filter(|key| !key.is_empty() && key.as_str() != "*")
            .cloned()
            .collect();
        keys.extend(
            tracks
                .iter()
                .flat_map(|track| track_color_keys(track, subsort, exact)),
        );
        keys
    };
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

/// Render the persistent waveform content (everything except the transient hover
/// crosshair) to any [`DrawTarget`]. This is the snuffler `drawit` analogue: the
/// on-screen and SVG export backends drive identical geometry through it.
#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "cohesive draw routine against one projection context; splitting would \
              obscure the layered draw order"
)]
pub fn paint(
    target: &mut dyn DrawTarget,
    state: &AppState,
    view: &ViewData,
    pal: &Palette,
    lay: &Layout<'_>,
    theme: Theme,
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
    let in_x = |x: f32| x >= rect.left() - point_margin && x <= rect.right() + point_margin;

    // Stroke colors by coloring key; the row tints and the colored strokes both
    // look their key up here.
    let qt_colors = colors_exactly_like_qt(state.wiggle_theme);
    let stroke_colors = if state.color_traces() {
        stroke_color_map(
            &view.meta.color_keys,
            &view.meta.tracks,
            state.subsort,
            state.wiggle_theme,
            theme,
        )
    } else {
        HashMap::new()
    };

    target.fill_rect(rect, 0.0, pal.bg);

    if state.show_boxes && !waterfall && state.wiggle_theme == WiggleTheme::SnufflerQt {
        target.push_clip(track_area);
        for (k, t) in visible.iter().enumerate() {
            let top = top0 + k as f32 * row_h + row_h * 0.05;
            let bot = top0 + k as f32 * row_h + row_h * 0.95;
            if bot <= top {
                continue;
            }
            // The boxes come from the pile, not from the fetched spans
            // (`draw_trace_boxes` reads `pile.iter_traces`, pile_viewer.py:2968),
            // so a channel `min_deltat_allow` declined to fetch still shows where
            // its data lies — without this, a view zoomed far beyond the data
            // goes blank with nothing left to navigate back to.
            for tbox in &t.boxes {
                if tbox.tmax < view_tmin || tbox.tmin > view_tmax {
                    continue;
                }
                let x0 = x_of(tbox.tmin).clamp(track_area.left(), track_area.right());
                // Qt strokes the outline with a cosmetic pen, so a box thinner
                // than a pixel still marks its spot; flooring the width at one
                // pixel is this painter's equivalent.
                let x1 = x_of(tbox.tmax)
                    .clamp(track_area.left(), track_area.right())
                    .max(x0 + 1.0);
                let (fill, frame) = qt_box_style(usize::try_from(tbox.style).unwrap_or(0));
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
        // Drawn per pile box rather than across the full width, which is what makes
        // it a *box* and not a stripe: a gap in the data is a gap in the band, as it
        // is in Qt.
        // Past 500 traces in the window the bridge hands each row merged per-style
        // runs whose gaps close — but that is `draw_trace_boxes`' own batching, so
        // Qt's boxes close them identically.
        // On continuous data the boxes meet and it reads as one clean row.
        // Pile-backed like the Qt theme's boxes, so the band outlives the zooms
        // where `min_deltat_allow` declines to fetch the channel itself.
        target.push_clip(track_area);
        // Numbered over the whole track universe, not the scroll slice, so a
        // station keeps its band colour as the stack scrolls — the same
        // stability rule the stroke palette follows.
        let bands = station_band_colors(&view.meta.tracks, pal, theme);
        for (k, t) in visible.iter().enumerate() {
            let top = top0 + k as f32 * row_h;
            let Some(band) = bands.get(&format!("{}.{}", t.net, t.sta)) else {
                continue;
            };
            for tbox in &t.boxes {
                if tbox.tmax < view_tmin || tbox.tmin > view_tmax {
                    continue;
                }
                let x0 = x_of(tbox.tmin).clamp(track_area.left(), track_area.right());
                let x1 = x_of(tbox.tmax)
                    .clamp(track_area.left(), track_area.right())
                    .max(x0 + 1.0);
                target.fill_rect(
                    Rect::from_min_max(pos2(x0, top), pos2(x1, top + row_h)),
                    0.0,
                    *band,
                );
            }
        }
        target.pop_clip();
    }

    // The time axis, following `TimeAx.drawit` (pile_viewer.py:539): snuffler's
    // own tick choice and calendar anchoring (`timeaxis::make_ticks`, with its
    // `approx_ticks = 7`), the abbreviation ladder measured against the tick
    // spacing, the second and third context rows centred under the ticks where
    // their unit rolls over, and a left-edge fallback row when none did.
    //
    // Both offsets are snuffler's, rearranged rather than guessed. `drawit`
    // gives the tick label a *baseline* at `vmin + rect0.height() + ticklen`,
    // which puts the top of its glyphs exactly `ticklen` below the top of the
    // axis area; each row under it stacks by the height of the label above.
    // Measuring a digit stands in for `QFontMetrics.boundingRect`.
    let (axis_ticks, tinc) = timeaxis::make_ticks(view_tmin, view_tmax, 7.0);
    let axis_top = track_area.bottom();
    let tick_label_h = target.measure("0", AXIS_PT).y;
    let row0_y = axis_top + QT_TICK_LEN;
    let row1_y = row0_y + tick_label_h;
    let row2_y = row1_y + tick_label_h;
    let axis_pad = 10.0;
    let mut draws: Vec<TickDraw> = Vec::new();
    // Qt computes the abbreviation level per tick in its first pass and its
    // second pass reads the loop variable after that pass has finished — so the
    // last tick's level applies to every label. Reproduced: label widths at one
    // increment are near-equal, which is what makes it work there.
    let mut abbr_level = 0_usize;
    for tick in &axis_ticks {
        let x = x_of(*tick);
        // Screen chrome only: snuffler's savesvg carries no grid, so an
        // exported figure — the artifact people put in papers — must not either.
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
    let fallback_tick = first_tick_with_label.unwrap_or(view_tmin);
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
        if state.show_boxes && waterfall {
            let r = Rect::from_min_max(
                pos2(rect.left() + 0.5, top + 1.0),
                pos2(rect.right() - 0.5, bot - 1.0),
            );
            target.line(r.left_top(), r.right_top(), 1.0, pal.sep);
            target.line(r.right_top(), r.right_bottom(), 1.0, pal.sep);
            target.line(r.right_bottom(), r.left_bottom(), 1.0, pal.sep);
            target.line(r.left_bottom(), r.left_top(), 1.0, pal.sep);
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
            // (pile_viewer.py:3186, :3462) — clipping to the row instead let a
            // spike spill about 5% of the row height past its own range. Off,
            // the trace may run across the whole track area.
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
            // The projection arithmetic stays f64 until the final pixel, because
            // the range scalars are float64 end to end now (a DC offset that
            // dwarfs the span would quantize visibly at f32).
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

            let colored_strokes = state.color_traces();
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
                // Uniform samples only: the bridge chops exactly as snuffler does
                // and drops anything shorter than two samples (pile_viewer.py:3905),
                // so the old interleaved-extrema and single-sample forms no longer
                // arrive.
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
                                in_x(x).then(|| pos2(x, y_of(span, f64::from(v))))
                            })
                            .collect();
                        if points.len() >= 2 {
                            target.polyline(points, 1.0, trace_col);
                        }
                        if !markers_over {
                            draw_marker_snippets(
                                target,
                                state,
                                MarkerSnippet {
                                    marker_ids: &marker_ids,
                                    track: t,
                                    span,
                                    samples,
                                    base_abs,
                                },
                                &|t_abs, v| {
                                    let x = x_of(t_abs);
                                    in_x(x).then(|| pos2(x, y_of(span, v)))
                                },
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
                        .get(&color_key(t, &span.loc, state.subsort, qt_colors))
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
                        if !state.show_boxes {
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
    draw_markers(
        target,
        state,
        track_area,
        top0,
        visible,
        row_h,
        &x_of,
        pal.axis_text,
        markers_over,
        &marker_ids,
    );
    target.pop_clip();

    target.push_clip(track_area);
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

/// Returns the vertical row spans to which an optional NSLC-scoped marker applies.
#[must_use]
pub fn marker_row_spans(
    marker: &Marker,
    area: Rect,
    top0: f32,
    visible: &[ViewTrack],
    row_h: f32,
) -> Vec<(f32, f32)> {
    if marker.nslc_ids.is_empty() {
        return vec![(area.top(), area.bottom())];
    }
    visible
        .iter()
        .enumerate()
        .filter(|(_, t)| marker.nslc_ids.iter().any(|n| marker_matches_track(n, t)))
        .map(|(k, _)| {
            let top = top0 + k as f32 * row_h;
            (top, top + row_h)
        })
        .collect()
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
        let r = Rect::from_min_max(pos2(pt.x - 2.0, pt.y - 2.0), pos2(pt.x + 2.0, pt.y + 2.0));
        target.line(r.left_top(), r.right_top(), 2.0, col);
        target.line(r.right_top(), r.right_bottom(), 2.0, col);
        target.line(r.right_bottom(), r.left_bottom(), 2.0, col);
        target.line(r.left_bottom(), r.left_top(), 2.0, col);
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
        let col = m.color_for(state.hovered_marker == Some(id));
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

/// The white plate Qt's `draw_label` always paints under a marker label
/// (`QColor(255, 255, 255)`, gui/util.py:293) so it stays readable over any
/// wiggle, whatever the theme.
const MARKER_LABEL_BG: Color32 = Color32::WHITE;

/// Draws a marker label the way `gui_util.draw_label` does: text in the
/// marker's colour on the white plate, and — for the active event or its picks
/// — the plate widened by 2 px each side and outlined instead of merely filled.
fn draw_marker_label(
    target: &mut dyn DrawTarget,
    pos: emath::Pos2,
    align: Align2,
    text: &str,
    col: Color32,
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
        target.fill_rect(plate, 0.0, MARKER_LABEL_BG);
        target.line(plate.left_top(), plate.right_top(), 1.0, col);
        target.line(plate.right_top(), plate.right_bottom(), 1.0, col);
        target.line(plate.right_bottom(), plate.left_bottom(), 1.0, col);
        target.line(plate.left_bottom(), plate.left_top(), 1.0, col);
    } else {
        target.fill_rect(plate, 0.0, MARKER_LABEL_BG);
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
    clippy::too_many_arguments,
    reason = "one marker pass over one projection context, layered exactly as Qt \
              layers it; splitting per layer would duplicate the filters"
)]
fn draw_markers(
    target: &mut dyn DrawTarget,
    state: &AppState,
    area: Rect,
    top0: f32,
    visible: &[ViewTrack],
    row_h: f32,
    x_of: &impl Fn(f64) -> f32,
    fg: Color32,
    overloaded: bool,
    marker_ids: &[MarkerId],
) {
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
        let col = m.color_for(hovered);
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
        let col = m.color_for(hovered);
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
    use snuffler_web_core::state::{AppState, WiggleTheme};

    use crate::draw::SvgTarget;
    use crate::layout::compute_layout;
    use crate::palette::{Theme, palette};

    use super::{
        MarkerSnippet, color_key, draw_marker_snippets, marker_matches_track, paint,
        stroke_color_map, web_band_share, web_stroke_palette, web_toward_canvas,
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

    // The box-style assignment these tests used to pin — per-trace styling so
    // co-located instruments differ, and a gap starting a new style — moved to
    // the bridge (`_box_styles`, web/bridge.py), which transcribes snuffler's
    // `determine_box_styles` over the pile instead of re-deriving styles from
    // the fetched spans.

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
            &[],
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
            &[],
            &tracks,
            Subsort::Nslc,
            WiggleTheme::SnufflerQt,
            Theme::Light,
        );
        let web = stroke_color_map(
            &[],
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
            &[],
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
        paint(
            &mut target,
            &state,
            &view,
            &colors,
            &layout,
            theme,
            false,
            false,
        );
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
        paint(
            &mut target,
            &state,
            &view,
            &colors,
            &layout,
            theme,
            false,
            false,
        );
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
    // drawn on it. The regression this pins: clamping the chop indices instead
    // put a spurious endpoint square on an earlier segment's final sample
    // whenever a span marker sat past that segment on a gapped channel.
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
    // pixel, the way Qt's cosmetic pen strokes a sub-pixel `drawRect`. Boxes
    // derived from the fetched spans instead leave a zoomed-far-out view blank,
    // with nothing to navigate back to.
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
        paint(
            &mut target,
            &state,
            &view,
            &colors,
            &layout,
            theme,
            false,
            false,
        );
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

        paint(
            &mut target,
            &state,
            &view,
            &colors,
            &layout,
            theme,
            false,
            false,
        );
        let svg = target.finish();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("<line"));
        assert!(svg.contains("<text"));
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
                notices: Vec::new(),
                color_keys: Vec::new(),
                session_generation: 0,
                view_sequence: 0,
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
            true,
        );
        let svg = target.finish();
        // The annotated label appears whole; the unannotated one carries no
        // decoration — in particular no "no coords" placeholder, which snuffler
        // never writes.
        assert!(svg.contains("12.3 km GE APE  HHZ"), "{svg}");
        assert!(svg.contains(">GE APE  HHN<"), "{svg}");
        assert!(!svg.contains("no coords"), "{svg}");
    }
}
