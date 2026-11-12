//! The "Main Controls" panel: filters, gain, rotation, visible tracks, marker kind,
//! and the waterfall colorbar.
//!
//! It mirrors snuffler's bottom control strip (`pile_viewer.py` `controls()` and
//! `ColorbarControl`).
//! View mode, wiggle theme, scaling and sorting are not repeated here; the menu bar
//! owns them.

use std::ops::RangeInclusive;

use egui::AtomExt as _;
use snuffler_web_core::state::{AppState, ColorScale, Colormap, ViewMode, WaterfallSettings};

/// Width of a control row's label column.
const LABEL_WIDTH_PX: f32 = 120.0;
/// Width of a control row's text field.
const VALUE_WIDTH_PX: f32 = 96.0;

/// The narrowest clip band the colorbar allows, as a fraction of the power range.
const MIN_CLIP_BAND: f32 = 0.05;
/// The highest the lower clip handle goes, one minimum band below the top.
const MAX_CLIP_LOW: f32 = 0.95;

/// The gain row: a logarithmic slider, reset to 1 by a double-click on the label.
const GAIN: NumberControl<f32> = NumberControl {
    label: "Gain:",
    default: 1.0,
    range: 0.001..=1000.0,
    logarithmic: true,
    format: |gain| format_compact(f64::from(gain), 3),
    parse: parse_gain,
};

/// The rotation row, in degrees, reset to 0 by a double-click on the label.
const ROTATE: NumberControl<f64> = NumberControl {
    label: "Rotate [deg]:",
    default: 0.0,
    range: -180.0..=180.0,
    logarithmic: false,
    format: |degrees| format_compact(degrees, 1),
    parse: parse_rotate,
};

enum ParsedFilter {
    Off,
    Hz(f64),
}

fn format_compact(value: f64, digits: usize) -> String {
    let mut text = format!("{value:.digits$}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').into();
    }
    text
}

fn format_hz(value: Option<f64>) -> String {
    match value {
        None => "off".into(),
        Some(hz) if hz < 1.0 => format_compact(hz, 3),
        Some(hz) if hz < 100.0 => format_compact(hz, 2),
        Some(hz) => format_compact(hz, 1),
    }
}

fn parse_hz(text: &str, min_hz: f64, max_hz: f64) -> Option<ParsedFilter> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("off") || text.eq_ignore_ascii_case("none") {
        return Some(ParsedFilter::Off);
    }
    let hz = text.parse::<f64>().ok()?;
    if !hz.is_finite() {
        return None;
    }
    if hz <= 0.0 {
        return Some(ParsedFilter::Off);
    }
    Some(ParsedFilter::Hz(hz.clamp(min_hz, max_hz)))
}

fn parse_gain(text: &str) -> Option<f32> {
    let gain = text.trim().parse::<f32>().ok()?;
    gain.is_finite().then_some(gain.clamp(0.001, 1000.0))
}

fn parse_rotate(text: &str) -> Option<f64> {
    let degrees = text.trim().parse::<f64>().ok()?;
    degrees.is_finite().then_some(degrees.clamp(-180.0, 180.0))
}

/// The corner-frequency range the two filter controls span, from the loaded data's
/// sampling-rate band.
///
/// This is exactly how snuffler narrows its controls on every pile change
/// (`adjust_controls`, `pile_viewer.py:4843`): the top corner is the finest trace's
/// Nyquist, the bottom four decades under the coarsest trace's.
fn filter_range(deltat_range: (f64, f64)) -> (f64, f64) {
    let (deltat_min, deltat_max) = deltat_range;
    ((0.5 / deltat_max) * 0.0001, 0.5 / deltat_min)
}

fn filter_to_unit(hz: f64, min_hz: f64, max_hz: f64) -> f64 {
    let hz = hz.clamp(min_hz, max_hz);
    (hz / min_hz).ln() / (max_hz / min_hz).ln()
}

fn unit_to_filter(unit: f64, min_hz: f64, max_hz: f64) -> f64 {
    min_hz * (max_hz / min_hz).powf(unit.clamp(0.0, 1.0))
}

/// Which of the two filter corners a control row edits.
#[derive(Clone, Copy)]
enum Corner {
    Highpass,
    Lowpass,
}

impl Corner {
    fn label(self) -> &'static str {
        match self {
            Self::Highpass => "Highpass [Hz]:",
            Self::Lowpass => "Lowpass [Hz]:",
        }
    }

    /// Slider position of a corner; "off" sits at the end that filters nothing.
    fn slider_pos(self, value: Option<f64>, min_hz: f64, max_hz: f64) -> f64 {
        match (self, value) {
            (_, Some(hz)) => filter_to_unit(hz, min_hz, max_hz),
            (Self::Highpass, None) => 0.0,
            (Self::Lowpass, None) => 1.0,
        }
    }

    /// The corner a slider position selects; its "off" end turns the filter off.
    fn value_at(self, pos: f64, min_hz: f64, max_hz: f64) -> Option<f64> {
        let off = match self {
            Self::Highpass => pos <= 0.0,
            Self::Lowpass => pos >= 1.0,
        };
        (!off).then(|| unit_to_filter(pos, min_hz, max_hz))
    }
}

/// One filter-corner row: a double-click on the label switches it off, the
/// slider and the text field set it, and `last_hz` remembers the last corner
/// that was on.
fn filter_row(
    ui: &mut egui::Ui,
    corner: Corner,
    value: &mut Option<f64>,
    text: &mut String,
    last_hz: &mut f64,
    (min_hz, max_hz): (f64, f64),
) {
    let mut pos = corner.slider_pos(*value, min_hz, max_hz);
    let mut slider_changed = false;
    let row = control_row(ui, corner.label(), text, format_hz(*value), |ui| {
        slider_changed = ui
            .add(egui::Slider::new(&mut pos, 0.0..=1.0).show_value(false))
            .changed();
    });
    if row.label.double_clicked() {
        *value = None;
        *text = format_hz(*value);
    } else if slider_changed {
        *value = corner.value_at(pos, min_hz, max_hz);
        if let Some(hz) = *value {
            *last_hz = hz;
        }
        if !row.value.has_focus() {
            *text = format_hz(*value);
        }
    } else if value_committed(ui, &row.value) {
        if let Some(parsed) = parse_hz(text, min_hz, max_hz) {
            *value = match parsed {
                ParsedFilter::Off => None,
                ParsedFilter::Hz(hz) => Some(hz),
            };
            if let Some(hz) = *value {
                *last_hz = hz;
            }
            if row.value.lost_focus() || ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                *text = format_hz(*value);
            }
        } else if row.value.lost_focus() {
            *text = format_hz(*value);
        }
    }
}

/// One numeric control row's behaviour: a double-click on the label resets the
/// default, and the slider and the text field set the value.
struct NumberControl<T> {
    label: &'static str,
    default: T,
    range: RangeInclusive<T>,
    logarithmic: bool,
    format: fn(T) -> String,
    parse: fn(&str) -> Option<T>,
}

/// Shows one [`NumberControl`] row editing `value`, with `text` as its text field.
fn number_row<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    control: &NumberControl<T>,
    value: &mut T,
    text: &mut String,
) {
    let format = control.format;
    let mut slider_value = *value;
    let mut slider_changed = false;
    let row = control_row(ui, control.label, text, format(*value), |ui| {
        slider_changed = ui
            .add(
                egui::Slider::new(&mut slider_value, control.range.clone())
                    .logarithmic(control.logarithmic)
                    .show_value(false),
            )
            .changed();
    });
    if row.label.double_clicked() {
        *value = control.default;
        *text = format(*value);
    } else if slider_changed {
        *value = slider_value;
        if !row.value.has_focus() {
            *text = format(*value);
        }
    } else if value_committed(ui, &row.value) {
        if let Some(parsed) = (control.parse)(text) {
            *value = parsed;
            if row.value.lost_focus() || ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                *text = format(*value);
            }
        } else if row.value.lost_focus() {
            *text = format(*value);
        }
    }
}

/// The two interactive parts of a control row.
struct ControlRowResponse {
    label: egui::Response,
    value: egui::Response,
}

/// Lays out one control row: a label, a text field showing `display_value` while
/// unfocused, and whatever `add` puts after them.
fn control_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    display_value: String,
    add: impl FnOnce(&mut egui::Ui),
) -> ControlRowResponse {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let value_id = ui.make_persistent_id(("control-value", label));
        if !ui.memory(|memory| memory.has_focus(value_id)) {
            *value = display_value;
        }

        let row_height = ui.spacing().interact_size.y;
        let (label_rect, label_response) =
            ui.allocate_exact_size(egui::vec2(LABEL_WIDTH_PX, row_height), egui::Sense::click());
        ui.painter().text(
            label_rect.right_center(),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().text_color(),
        );
        // Painting straight to the canvas leaves egui with an interactive area and
        // no idea what is in it, so the accessibility tree gets an unnamed control
        // and the text field and slider beside it have no label to borrow either.
        label_response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), label)
        });
        let value_response = ui.add_sized(
            [VALUE_WIDTH_PX, row_height],
            egui::TextEdit::singleline(value)
                .id(value_id)
                .font(egui::TextStyle::Monospace)
                .interactive(true),
        );
        ui.spacing_mut().slider_width = ui.available_width().max(160.0);
        add(ui);
        ControlRowResponse {
            label: label_response,
            value: value_response,
        }
    })
    .inner
}

/// Whether the text field's value should be parsed this frame: on every edit, on
/// leaving the field, and on Enter.
fn value_committed(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.changed()
        || response.lost_focus()
        || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
}

/// Shows the panel.
///
/// `track_count` is the pile's row count, and `deltat_range` the finest and coarsest
/// sampling intervals, which bound the filter corners.
/// `show_readout` fills the remaining space to the right of the marker controls.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    track_count: usize,
    deltat_range: (f64, f64),
    show_readout: impl FnOnce(&mut egui::Ui, &mut AppState),
) {
    ui.add_space(4.0);
    let waterfall = state.view_mode == ViewMode::Waterfall;
    let (min_hz, max_hz) = filter_range(deltat_range);
    // Loading finer or coarser data moves the band; corners set for the old pile
    // follow it in, as Qt's controls clamp their value on `set_range`.
    for corner in [&mut state.highpass, &mut state.lowpass] {
        if let Some(hz) = corner.as_mut() {
            *hz = hz.clamp(min_hz, max_hz);
        }
    }

    filter_row(
        ui,
        Corner::Highpass,
        &mut state.highpass,
        &mut state.highpass_text,
        &mut state.last_highpass_hz,
        (min_hz, max_hz),
    );
    filter_row(
        ui,
        Corner::Lowpass,
        &mut state.lowpass,
        &mut state.lowpass_text,
        &mut state.last_lowpass_hz,
        (min_hz, max_hz),
    );
    // Snuffler's passband_check warning is posted to the permanent status line, the
    // counterpart of Qt's status area: a warning drawn here would vanish with the
    // panel `c` hides.

    // Gain is meaningless for the waterfall, so snuffler hides it there.
    if !waterfall {
        number_row(ui, &GAIN, &mut state.gain, &mut state.gain_text);
    }
    number_row(ui, &ROTATE, &mut state.rotate_deg, &mut state.rotate_text);

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        visible_tracks_controls(ui, state, track_count);
        ui.separator();
        trace_display_button(ui, state);
        ui.separator();
        marker_kind_buttons(ui, state);
        show_readout(ui, state);
    });

    // Snuffler's `ColorbarControl`, shown only in waterfall mode
    if waterfall {
        ui.horizontal_wrapped(|ui| colorbar_controls(ui, &mut state.waterfall));
    }
}

/// A shortcut for the two complementary combinations of the existing drawing options.
fn trace_display_button(ui: &mut egui::Ui, state: &mut AppState) {
    let canvas = crate::view::palette_for(ui.ctx().theme(), state.wiggle_theme).theme;
    let boxes = state.boxes_visible(canvas);
    let colored = state.color_traces(canvas);
    let label = match (boxes, colored) {
        (false, true) => "Stroke",
        (true, false) => "Fill",
        _ => "Mixed",
    };
    let waterfall = state.view_mode == ViewMode::Waterfall;
    let enabled = boxes != colored && !waterfall;
    let tooltip = if waterfall {
        "Disabled in Waterfall view; switch to Wiggle to toggle colored strokes and filled boxes."
    } else {
        match (boxes, colored) {
            (false, true) => {
                "Switch to filled boxes with monochrome traces instead of colored strokes."
            }
            (true, false) => "Switch to colored strokes without boxes.",
            (true, true) => {
                "Disabled because Show boxes and Color traces are both on; change one in View to toggle colored strokes and filled boxes."
            }
            (false, false) => {
                "Disabled because Show boxes and Color traces are both off; change one in View to toggle colored strokes and filled boxes."
            }
        }
    };
    let icon_id = ui.make_persistent_id("trace-display-icon");
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            let output = egui::Button::new((
                egui::Atom::custom(icon_id, egui::vec2(20.0, 16.0)),
                label
                    .atom_grow(true)
                    .atom_align(egui::Align2::CENTER_CENTER),
            ))
            .min_size(egui::vec2(104.0, ui.spacing().interact_size.y))
            .atom_ui(ui);
            if let Some(rect) = output.rect(icon_id) {
                let ink = ui.style().interact(&output.response).fg_stroke.color;
                let rect = rect.shrink(1.0);
                let painter = ui.painter();
                if boxes == colored {
                    painter.rect_stroke(
                        rect,
                        2.0,
                        egui::Stroke::new(1.5_f32, ink),
                        egui::StrokeKind::Inside,
                    );
                    painter.line_segment(
                        [
                            rect.left_center() + egui::vec2(4.0, 0.0),
                            rect.right_center() - egui::vec2(4.0, 0.0),
                        ],
                        egui::Stroke::new(1.5_f32, ink),
                    );
                } else {
                    if boxes {
                        let opacity = if ui.visuals().dark_mode { 0.35 } else { 0.18 };
                        painter.rect_filled(rect, 2.0, ink.gamma_multiply(opacity));
                    }
                    let point = |x, y| rect.lerp_inside(egui::vec2(x, y));
                    painter.add(egui::Shape::line(
                        vec![
                            point(0.0, 0.5),
                            point(0.25, 0.5),
                            point(0.4, 0.1),
                            point(0.6, 0.9),
                            point(0.75, 0.5),
                            point(1.0, 0.5),
                        ],
                        egui::Stroke::new(1.5_f32, ink),
                    ));
                }
            }
            output.response
        })
        .inner;
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            response.enabled(),
            format!("Trace display: {label}"),
        )
    });
    if response
        .on_hover_text(tooltip)
        .on_disabled_hover_text(tooltip)
        .clicked()
    {
        state.show_boxes = Some(!boxes);
        state.show_color_traces = Some(!colored);
    }
}

/// Visible rows per page; Snap selects whole rows and aligns scrolling.
fn visible_tracks_controls(ui: &mut egui::Ui, state: &mut AppState, track_count: usize) {
    let mut count = state.visible_track_count(track_count);
    #[expect(
        clippy::cast_precision_loss,
        reason = "track counts are within f32's exact integer range"
    )]
    let maximum = track_count.max(1) as f32;
    // Slow pointer dragging while retaining useful keyboard and assistive increments.
    let speed = if ui.input(|input| input.pointer.primary_down()) {
        (f64::from(maximum) / 400.0).max(if state.snap_tracks { 0.05 } else { 0.01 })
    } else if state.snap_tracks {
        1.0
    } else {
        0.1
    };
    // Reserve the widest count, including a decimal even while Snap is on.
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let value_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                format!("{maximum:.1} tracks"),
                font,
                ui.visuals().text_color(),
            )
            .size()
            .x
    }) + 2.0 * ui.spacing().button_padding.x
        + 4.0;
    let decimals = usize::from(!state.snap_tracks);
    let value = ui
        .scope(|ui| {
            ui.style_mut().drag_value_text_style = egui::TextStyle::Monospace;
            ui.add_sized(
                egui::vec2(value_width.max(112.0), ui.spacing().interact_size.y),
                egui::DragValue::new(&mut count)
                    .range(1.0..=maximum)
                    .speed(speed)
                    .max_decimals(decimals)
                    .custom_formatter(move |value, _| format!("{value:.decimals$} tracks"))
                    .custom_parser(|text| {
                        text.trim().trim_end_matches("tracks").trim().parse().ok()
                    }),
            )
        })
        .inner;
    value.widget_info(|| {
        let mut info = egui::WidgetInfo::drag_value(value.enabled(), f64::from(count));
        info.label = Some("Visible track count".to_owned());
        info
    });
    let mut slider = egui::Slider::new(&mut count, 1.0..=maximum).show_value(false);
    if state.snap_tracks {
        slider = slider.integer();
    } else {
        slider = slider.max_decimals(1).step_by(0.1);
    }
    let response = ui.add(slider);
    response.widget_info(|| {
        egui::WidgetInfo::slider(response.enabled(), f64::from(count), "Visible tracks")
    });
    let slider_changed = response.changed();
    response.on_hover_text(format!(
        "Choose how many tracks are visible ({} total)",
        track_count.max(1)
    ));
    if slider_changed || value.changed() {
        state.tracks_per_page = Some(count);
    }
    if ui
        .checkbox(&mut state.snap_tracks, "Snap")
        .on_hover_text(
            "Snap to whole visible tracks and align scrolling; turn off to show fractional tracks.",
        )
        .changed()
        && state.snap_tracks
    {
        state.tracks_per_page = Some(state.visible_track_count(track_count));
    }
}

/// The pointer twin of the digit keys.
///
/// Clicking a kind recolours the selected markers (or retargets the floating pick),
/// exactly what the key does.
/// It never latches a kind for future picks, which in snuffler are always kind 0.
/// `marker_kind` only remembers the last applied choice for the highlight.
fn marker_kind_buttons(ui: &mut egui::Ui, state: &mut AppState) {
    ui.label("Marker:");
    // The digits wear each kind's lit shade, the one a selected marker wears on the
    // canvas, so the button and the marker it recolours match.
    let marker_palette = crate::view::palette_for(ui.ctx().theme(), state.wiggle_theme).markers;
    for kind in 0_u8..8 {
        let label = egui::RichText::new(format!("{kind}"))
            .color(marker_palette.kind(kind, true))
            .strong();
        let clicked = ui
            .selectable_label(state.marker_kind == kind, label)
            .on_hover_text("Change color of selected markers")
            .clicked();
        if !clicked {
            continue;
        }
        state.marker_kind = kind;
        if let Some(pick) = state.picking.as_mut() {
            pick.kind = kind;
        } else {
            for marker in &mut state.markers {
                if marker.selected {
                    marker.kind = kind;
                }
            }
        }
    }
}

/// The waterfall colorbar: colormap, scale, the clip band and its toggles.
fn colorbar_controls(ui: &mut egui::Ui, settings: &mut WaterfallSettings) {
    // `from_label` rather than a detached `ui.label` + `from_id_salt`: the label is
    // what names the combo in the accessibility tree, and a nameless picker reads
    // as whatever value it happens to hold.
    egui::ComboBox::from_label("Colormap")
        .selected_text(settings.colormap.label())
        .show_ui(ui, |ui| {
            for colormap in Colormap::ALL {
                ui.selectable_value(&mut settings.colormap, colormap, colormap.label());
            }
        });
    egui::ComboBox::from_label("Scale")
        .selected_text(settings.color_scale.label())
        .show_ui(ui, |ui| {
            for color_scale in ColorScale::ALL {
                ui.selectable_value(&mut settings.color_scale, color_scale, color_scale.label());
            }
        });

    ui.separator();
    clip_sliders(ui, settings);

    ui.separator();
    ui.checkbox(&mut settings.reverse_colormap, "Reverse");
    // Symmetry is a live constraint on the clip pair, exactly snuffler's
    // (`set_symetry`, gui/util.py:817). // spellcheck:ignore-line
    if ui.checkbox(&mut settings.symmetric, "Symmetry").changed() && settings.symmetric {
        make_clip_band_symmetric(settings);
    }
    // Qt un-checks Symmetry whenever Absolute goes on, and re-checks it when
    // Absolute goes off (`toggle_absolute`, gui/util.py:776).
    if ui
        .checkbox(&mut settings.absolute_amplitudes, "Abs")
        .changed()
    {
        settings.symmetric = !settings.absolute_amplitudes;
        if settings.symmetric {
            make_clip_band_symmetric(settings);
        }
    }
    ui.checkbox(&mut settings.integrate, "Integrate");
}

/// The two clip handles, with snuffler's colorbar semantics (gui/util.py:817, :914).
fn clip_sliders(ui: &mut egui::Ui, settings: &mut WaterfallSettings) {
    ui.label("Clip low:");
    let low =
        ui.add(egui::Slider::new(&mut settings.clip_low, 0.0..=MAX_CLIP_LOW).show_value(false));
    ui.label("Clip high:");
    let high =
        ui.add(egui::Slider::new(&mut settings.clip_high, MIN_CLIP_BAND..=1.0).show_value(false));

    // With Symmetry on, moving either handle mirrors the other around 0.5.
    // A drag that would squeeze the band below its minimum width clamps the handle
    // being dragged rather than silently pushing the other one.
    if low.changed() {
        if settings.symmetric {
            settings.clip_high = (1.0 - settings.clip_low).clamp(MIN_CLIP_BAND, 1.0);
        }
        settings.clip_low = settings
            .clip_low
            .min(settings.clip_high - MIN_CLIP_BAND)
            .max(0.0);
    }
    if high.changed() {
        if settings.symmetric {
            settings.clip_low = (1.0 - settings.clip_high).clamp(0.0, MAX_CLIP_LOW);
        }
        settings.clip_high = settings
            .clip_high
            .max(settings.clip_low + MIN_CLIP_BAND)
            .min(1.0);
    }
    // Qt's colorbar resets its clip band on a double-click (gui/util.py:883).
    if low.double_clicked() || high.double_clicked() {
        settings.clip_low = 0.0;
        settings.clip_high = 1.0;
    }

    // Qt's colorbar wheel widens or narrows the clip band symmetrically, with
    // Symmetry locked only, refusing a step below the minimum width (`wheelEvent`,
    // gui/util.py:886).
    // Qt maps a 120-unit wheel notch to 0.024 of the band, and egui reports points,
    // about 50 a notch, so the factor keeps one notch worth one Qt notch.
    let scroll = ui.input(|input| input.smooth_scroll_delta.y);
    if settings.symmetric && scroll != 0.0 && (low.hovered() || high.hovered()) {
        let delta = -f64::from(scroll) * (0.024 / 50.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clip positions live in 0..=1; f32 is the state's own precision"
        )]
        let (clip_low, clip_high) = (
            (f64::from(settings.clip_low) + delta).max(0.0) as f32,
            (f64::from(settings.clip_high) - delta).min(1.0) as f32,
        );
        if clip_high - clip_low >= MIN_CLIP_BAND {
            settings.clip_low = clip_low;
            settings.clip_high = clip_high;
        }
    }
}

/// Snaps the clip band symmetric around 0.5 by its tighter side, as switching
/// Symmetry on does in snuffler.
fn make_clip_band_symmetric(settings: &mut WaterfallSettings) {
    let clip_max = 1.0 - settings.clip_low.min(1.0 - settings.clip_high);
    settings.clip_low = 1.0 - clip_max;
    settings.clip_high = clip_max;
}
