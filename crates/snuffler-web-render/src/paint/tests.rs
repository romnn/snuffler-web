//! Tests of the paint pipeline, through [`paint`] and its parts.

use std::collections::HashMap;

use ecolor::Color32;
use emath::{Rect, Vec2, pos2};
use snuffler_web_core::bridge::{ViewData, ViewMeta, ViewSpan, ViewTrack};
use snuffler_web_core::state::{AppState, Subsort};
use snuffler_web_core::theme::WiggleTheme;

use crate::draw::SvgTarget;
use crate::layout::compute_layout;
use crate::palette::{Palette, Theme, palette};

use super::colors::{color_key, stroke_color_map};
use super::markers::{MarkerSnippet, draw_marker_snippets};
use super::{Frame, paint};
use snuffler_web_core::markers::marker_matches_track;

/// A 640 × 360 SVG target whose text measures half its point size per character.
fn svg_target() -> SvgTarget {
    SvgTarget::new(640.0, 360.0, HashMap::new(), |text, size| {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test labels are a few dozen characters"
        )]
        let characters = text.len() as f32;
        Vec2::new(characters * size * 0.5, size)
    })
}

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

// These pin the half of `color-traces-theming` (tests/parity/known_gaps.toml) that mirrors
// snuffler: the per-subsort coloring keys and the sorted-key numbering.
// The palette itself is deliberately this frontend's own.

/// The stroke colors as a fixed-size array, so the asserts can destructure instead of
/// indexing.
///
/// These tracks carry no spans, so each looks up the one key its own identity earns; the
/// fallback only satisfies the conversion's type and is unreachable, the map being built from
/// these same keys.
fn colors_of<const N: usize>(tracks: &[ViewTrack; N], subsort: Subsort) -> [Color32; N] {
    let map = stroke_color_map(
        &[],
        tracks.as_slice(),
        subsort,
        WiggleTheme::Fieldbook,
        &palette(Theme::Light, WiggleTheme::Fieldbook),
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
    // `pyrocko.plot.color` indexes a seven-entry list and returns (0, 0, 0) for anything past it
    // rather than wrapping, so an eighth location goes black in snuffler instead of repeating
    // the first colour.
    // The Qt theme follows.
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
    // The label fallback exists so Color lines stays varied on single-location data.
    // Snuffler has no such fallback — every empty location is one key — and the Qt theme's claim
    // is to look like snuffler, not to look good.
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
        WiggleTheme::Fieldbook,
        &palette(Theme::Light, WiggleTheme::Fieldbook),
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
        WiggleTheme::Fieldbook,
        &palette(Theme::Light, WiggleTheme::Fieldbook),
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
        WiggleTheme::Fieldbook,
        &palette(Theme::Light, WiggleTheme::Fieldbook),
    );
    let alone = stroke_color_map(
        &pile_keys,
        &[occupied],
        Subsort::NscGroupL,
        WiggleTheme::Fieldbook,
        &palette(Theme::Light, WiggleTheme::Fieldbook),
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
        WiggleTheme::Fieldbook,
        &palette(Theme::Light, WiggleTheme::Fieldbook),
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
    let hit = |(net, sta, loc, cha): (&str, &str, &str, &str)| {
        marker_matches_track(
            &(
                net.to_owned(),
                sta.to_owned(),
                loc.to_owned(),
                cha.to_owned(),
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
        let m = snuffler_web_core::markers::Marker::new(t, t, 0);
        let id = state.markers.insert(m);
        state.markers.set_selected(id, k < 5);
    }
    let view = ViewData {
        meta: ViewMeta {
            data_tmin: state.tmin,
            data_tmax: state.tmax,
            ..ViewMeta::default()
        },
        ys: Vec::new(),
    };
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
    let layout = compute_layout(&state, &view, rect);
    let theme = Theme::Dark;
    let colors = palette(theme, WiggleTheme::SnufflerQt);
    let mut target = svg_target();
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
        deltat: 0.1,
        yscale: 1.0,
        ..ViewSpan::default()
    }];
    let view = ViewData {
        meta: ViewMeta {
            data_tmin: state.tmin,
            data_tmax: state.tmax,
            tracks: vec![row],
            ..ViewMeta::default()
        },
        ys: vec![0.0; 600],
    };
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
    let layout = compute_layout(&state, &view, rect);
    let theme = Theme::Light;
    let colors = palette(theme, WiggleTheme::SnufflerQt);
    let mut target = svg_target();
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
        deltat: 1.0,
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
        let view = ViewData::default();
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
        let layout = compute_layout(&state, &view, rect);
        let pal = palette(Theme::Light, WiggleTheme::SnufflerQt);
        let frame = Frame {
            state: &state,
            view: &view,
            pal: &pal,
            lay: &layout,
            boxes_visible: true,
        };
        let mut target = svg_target();
        draw_marker_snippets(
            &mut target,
            &frame,
            MarkerSnippet {
                marker_ids: &marker_ids,
                track: &row,
                span: &span,
                samples: &seg_samples,
                start: 0.0,
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
        wiggle_theme: WiggleTheme::SnufflerQt,
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
            data_tmin: state.tmin,
            data_tmax: state.tmax,
            tracks: vec![row],
            ..ViewMeta::default()
        },
        ys: Vec::new(),
    };
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
    let layout = compute_layout(&state, &view, rect);
    let theme = Theme::Light;
    let colors = palette(theme, WiggleTheme::SnufflerQt);
    let mut target = svg_target();
    paint(&mut target, &state, &view, &colors, &layout, false, false);
    let svg = target.finish();
    // Style 0's frame is snuffler's Tango orange3, stroked at full alpha, so its hex survives
    // `Color32`'s premultiplied storage exactly (the alpha-100 fill quantizes a unit off).
    // Its presence means the box was painted, which needs the one-pixel width floor — the raw
    // projection of ninety seconds here is 0.05 px, which `fill_rect` drops.
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
            data_tmin: state.tmin,
            ..ViewMeta::default()
        },
        ys: Vec::new(),
    };
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
    let layout = compute_layout(&state, &view, rect);
    let theme = Theme::Dark;
    let colors = palette(theme, WiggleTheme::SnufflerQt);
    let mut target = svg_target();

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
        relative_start_s: 0.0,
        deltat: 1.0,
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
    let mut target = svg_target();
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
            relative_start_s: 0.0,
            deltat: 1.0,
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
        wiggle_theme: WiggleTheme::Afterglow,
        show_boxes: Some(false),
        ..AppState::default()
    };
    let view = ViewData {
        meta: ViewMeta {
            data_tmin: tmin,
            data_tmax: tmax,
            tracks: vec![row],
            color_keys: vec!["00".to_owned(), "10".to_owned(), "20".to_owned()],
            subsort: "nsc_l".to_owned(),
            ..ViewMeta::default()
        },
        ys: vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5],
    };
    let pal = palette(Theme::Dark, WiggleTheme::Afterglow);

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
            data_tmin: tmin,
            data_tmax: tmax,
            tracks: vec![
                styled_track("AAA", 0, tmin, tmax),
                styled_track("BBB", 3, tmin, tmax),
            ],
            ..ViewMeta::default()
        },
        ys: vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5],
    };
    let pal = palette(Theme::Dark, WiggleTheme::Kiln);
    let hidden = scene(WiggleTheme::Kiln, false);

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
    let shown = scene(WiggleTheme::Kiln, true);
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
        relative_start_s: -3.0,
        deltat: 3.0,
        count: 6,
        offset: 0,
        ymin: -1.0,
        ymax: 1.0,
        yscale: 1.0,
        ..ViewSpan::default()
    }];
    let view = ViewData {
        meta: ViewMeta {
            data_tmin: state.tmin,
            data_tmax: state.tmax,
            tracks: vec![t],
            ..ViewMeta::default()
        },
        ys: vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5],
    };
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
    let layout = compute_layout(&state, &view, rect);
    let colors = palette(Theme::Light, WiggleTheme::Fieldbook);
    let mut target = svg_target();
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
            data_tmin: state.tmin,
            data_tmax: state.tmax,
            tracks: vec![track("HHZ", "12.3 km"), track("HHN", "")],
            ..ViewMeta::default()
        },
        ys: Vec::new(),
    };
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 360.0));
    let layout = compute_layout(&state, &view, rect);
    let colors = palette(Theme::Dark, WiggleTheme::SnufflerQt);
    let mut target = svg_target();

    paint(&mut target, &state, &view, &colors, &layout, false, true);
    let svg = target.finish();
    // The annotated label appears whole; the unannotated one carries no
    // decoration — in particular no "no coords" placeholder, which snuffler
    // never writes.
    assert!(svg.contains("12.3 km GE APE  HHZ"), "{svg}");
    assert!(svg.contains(">GE APE  HHN<"), "{svg}");
    assert!(!svg.contains("no coords"), "{svg}");
}

#[test]
fn station_locations_and_distances_align_right_without_overlapping_station_identity() {
    for (enabled, width, location_visible) in [
        (true, 640.0, true),
        (false, 640.0, false),
        (true, 180.0, false),
    ] {
        let state = AppState {
            tmin: 1_700_000_000.0,
            tmax: 1_700_000_060.0,
            show_station_locations: enabled,
            hovered_track_row: Some(0),
            ..AppState::default()
        };
        let view = ViewData {
            meta: ViewMeta {
                data_tmin: state.tmin,
                data_tmax: state.tmax,
                tracks: vec![ViewTrack {
                    label: "12.3 km GE APE HHZ".to_owned(),
                    station_label: "GE APE HHZ".to_owned(),
                    distance_label: "12.3 km".to_owned(),
                    place: Some(snuffler_web_core::bridge::StationPlace {
                        region: "Bayern".to_owned(),
                        country: "Germany".to_owned(),
                        ..snuffler_web_core::bridge::StationPlace::default()
                    }),
                    ..ViewTrack::default()
                }],
                ..ViewMeta::default()
            },
            ys: Vec::new(),
        };
        let layout = compute_layout(
            &state,
            &view,
            Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(width, 360.0)),
        );
        let colors = palette(Theme::Dark, WiggleTheme::Fieldbook);
        let mut target = svg_target();
        paint(&mut target, &state, &view, &colors, &layout, false, true);
        let svg = target.finish();

        // Narrow rows and the disabled option preserve the original compact label.
        assert_eq!(svg.contains("Bayern, Germany"), location_visible, "{svg}");
        assert_eq!(svg.contains(">GE APE HHZ<"), location_visible, "{svg}");
        assert_eq!(svg.contains(">12.3 km<"), location_visible, "{svg}");
        assert_eq!(
            svg.contains("12.3 km GE APE HHZ"),
            !location_visible,
            "{svg}"
        );
    }
}
