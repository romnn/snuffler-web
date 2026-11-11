//! Which colour each wiggle and scale annotation is drawn in.

use std::collections::HashMap;

use ecolor::Color32;
use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};
use snuffler_web_core::state::{AppState, Subsort};
use snuffler_web_core::theme::WiggleTheme;

use crate::palette::Palette;

/// How the wiggles of one paint pass are coloured, settled once per frame.
pub(super) struct TraceColors<'a> {
    pal: &'a Palette,
    subsort: Subsort,
    /// The Snuffler Qt theme's exact coloring: no label fallback, black past the ramp.
    exact: bool,
    /// See [`label_fallback_active`].
    label_fallback: bool,
    /// Whether Color Traces is on for this subsort and theme.
    colored: bool,
    /// Whether the strokes stand in for the hidden boxes' style class.
    strokes_carry_box_style: bool,
    /// The stroke colour per coloring key, empty while Color Traces is off.
    by_key: HashMap<String, Color32>,
}

impl<'a> TraceColors<'a> {
    pub(super) fn settle(state: &AppState, view: &ViewData, pal: &'a Palette) -> Self {
        // Colour from the subsort the reply was produced under, not the live UI
        // state.
        // After a subsort change, a whole round trip still paints the old reply's
        // tracks and color_keys, and keying those with the new subsort would give a
        // transient palette belonging to neither state.
        // An unknown wire value (an empty pre-first-reply view) falls back to the
        // live state.
        let subsort = Subsort::from_wire(&view.meta.subsort).unwrap_or(state.subsort);
        let exact = state.wiggle_theme.mirrors_qt_coloring();
        let colored = state.color_traces_for(subsort, pal.theme);

        // The boxes and the strokes carry two different classes: a box is the
        // pile's box style, a stroke its coloring key.
        // Hiding the boxes takes one of those off the screen entirely, so on the web
        // themes the strokes take it over and a wiggle is drawn in the colour its
        // box would have had.
        // Without this, hiding the boxes on data with a single location code leaves
        // every trace one colour.
        //
        // A location-grouped row is the exception, and keeps colouring by the
        // coloring key: there several traces share one row and the key is what
        // tells them apart.
        // The box style cannot, because it steps per contiguous run, so every trace
        // of the row would land on the run its window starts in.
        // Snuffler Qt keeps colouring by the coloring key throughout, boxes or no
        // boxes, because upstream's two toggles are independent.
        let strokes_carry_box_style =
            !state.boxes_visible(pal.theme) && !exact && !subsort.groups_locations();

        let by_key = if colored {
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
        Self {
            pal,
            subsort,
            exact,
            label_fallback: label_fallback_active(&view.meta.color_keys),
            colored,
            strokes_carry_box_style,
            by_key,
        }
    }

    /// The colour a span's wiggle is drawn in.
    ///
    /// `span_start` is the span's absolute start time, which picks the box run it
    /// lies in when the strokes stand in for the boxes.
    pub(super) fn wiggle(&self, track: &ViewTrack, span: &ViewSpan, span_start: f64) -> Color32 {
        match (self.colored, self.strokes_carry_box_style) {
            (false, _) => self.pal.trace,
            (true, true) => span_box_style(track, span_start).map_or(self.pal.trace, |style| {
                self.pal.stroke(usize::try_from(style).unwrap_or(0))
            }),
            (true, false) => self.by_coloring_key(track, span),
        }
    }

    /// The colour of a span's scale annotations: its coloring key's colour while
    /// Color Traces is on, the plain trace ink otherwise.
    pub(super) fn scale(&self, track: &ViewTrack, span: &ViewSpan) -> Color32 {
        if self.colored {
            self.by_coloring_key(track, span)
        } else {
            self.pal.trace
        }
    }

    fn by_coloring_key(&self, track: &ViewTrack, span: &ViewSpan) -> Color32 {
        let key = color_key(
            track,
            &span.loc,
            self.subsort,
            self.exact,
            self.label_fallback,
        );
        self.by_key.get(&key).copied().unwrap_or(self.pal.trace)
    }
}

/// The coloring key snuffler pairs with each subsort (the sorting `menudef` in
/// `pile_viewer.py`).
///
/// That is the location for NSLC and the grouped-by-location modes, the channel
/// for NSCL/SNCL/LNSC, and the whole net+sta+loc for CNSL.
///
/// The location is the *span's*, not the track's.
/// Under the grouped-by-location subsorts a row carries every location of its
/// channel, and colour is the only thing telling them apart.
/// That is why snuffler colours by location in exactly the modes that group over it.
///
/// An empty key (single-location data) falls back to the track label, so the Color
/// lines theme stays varied where snuffler would paint every trace alike.
/// This is a prettiness choice, see `color-traces-theming` in
/// `tests/parity/known_gaps.toml`.
/// The `"*"` a span-less grouped row offers as its location is no key either, so it
/// falls back the same way.
/// Under the Snuffler Qt theme (`exact`) there is no fallback: that theme's whole
/// claim is to look like snuffler, which does paint them alike.
///
/// `fallback` gates that substitution on the pile actually having no usable key
/// (see [`label_fallback_active`]).
/// In a mixed pile, a blank location keeps its own pile-wide-stable `""` key
/// instead.
/// A label there would exist only while the row's blank-location member has data in
/// the window, and a key entering or leaving the sorted numbering shifts every key
/// after it on a mere pan.
pub(super) fn color_key(
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

/// Whether the web themes' label fallback is on.
///
/// It is on only when the whole pile has no usable coloring key (single or
/// empty-location data), the case the fallback was written for.
/// Then every row contributes its label whether it has data in the window or not,
/// so the label numbering is window-independent too.
fn label_fallback_active(pile_keys: &[String]) -> bool {
    pile_keys
        .iter()
        .all(|key| key.is_empty() || key.as_str() == "*")
}

/// The coloring keys one row contributes to the palette numbering.
///
/// That is one per painted span, since a row can carry traces with different keys,
/// or the row's own identity's when it has no spans, so the numbering does not
/// shift when a track happens to have no data in the window.
/// A span-less *grouped* row is the exception, filtered at the [`stroke_color_map`]
/// call site: its own `"*"` is no identity, and the label it would fall back to is
/// window-dependent there.
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
        .find(|trace_box| start >= trace_box.tmin && start <= trace_box.tmax)
        .or_else(|| track.boxes.first())
        .map(|trace_box| trace_box.style)
}

/// Stroke color per coloring key, assigned the way snuffler assigns its colors:
/// the distinct keys sorted, then numbered into the palette in that order
/// (`color_lookup` over the sorted `pile.gather_keys`).
///
/// The numbering base is `pile_keys`, the whole pile's coloring keys, computed
/// bridge-side exactly as snuffler computes `color_keys` (`pile_viewer.py:1753`).
/// So a key keeps its colour as the view scrolls or pages.
/// When the bridge sent none (an empty pile, or a unit test exercising the
/// assignment alone), the painted tracks' own keys stand in.
///
/// The colours are the palette's ramp.
/// Under the Snuffler Qt theme that ramp is snuffler's own `graph_colors`
/// (`plot/__init__.py:95`) and, past the seventh key, snuffler's own black:
/// `pyrocko.plot.color` indexes that list and returns `(0, 0, 0)` for anything
/// beyond it rather than wrapping.
/// The web themes wrap their ramps instead.
pub(super) fn stroke_color_map(
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
        // No usable pile key (single or empty-location data): the labels are the
        // numbering, and every row contributes its own whether occupied or not, so
        // that numbering never moves with the window.
        // The pile-wide empty and wildcard keys are never looked up (the fallback
        // replaces them at draw time), so keeping them would waste a palette slot.
        tracks
            .iter()
            .flat_map(|track| track_color_keys(track, subsort, exact, fallback))
            .collect()
    } else {
        // Real location keys make up the numbering, and the label fallback is off
        // (see `color_key`): a blank location keeps its own pile-wide `""` key.
        // Only the grouped-row `"*"` wildcard is dropped; it is a row identity, not
        // a key anything draws with.
        // A span-less grouped row contributes nothing: the keys it would paint with
        // are locations the pile-wide list already carries, and anything
        // window-dependent it added would renumber every key sorting after it on a
        // mere pan.
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
