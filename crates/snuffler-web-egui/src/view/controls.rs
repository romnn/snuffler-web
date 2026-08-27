//! The "Main Controls" panel: filters, gain, rotation, track height, marker
//! kind, and the waterfall colorbar — mirroring snuffler's bottom control strip
//! (`pile_viewer.py` `controls()` + `ColorbarControl`). View mode, wiggle theme,
//! scaling and sorting are not repeated here; the menu bar owns them.

use snuffler_web_core::state::{
    AppState, ColorScale, Colormap, MAX_TRACK_H, MIN_TRACK_H, ViewMode,
};

const LABEL_W: f32 = 104.0;
const VALUE_W: f32 = 96.0;
const DEFAULT_GAIN: f32 = 1.0;
const DEFAULT_ROTATE_DEG: f64 = 0.0;

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
        Some(v) if v < 1.0 => format_compact(v, 3),
        Some(v) if v < 100.0 => format_compact(v, 2),
        Some(v) => format_compact(v, 1),
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
    let rotate = text.trim().parse::<f64>().ok()?;
    rotate.is_finite().then_some(rotate.clamp(-180.0, 180.0))
}

/// The corner-frequency range the two filter controls span, from the loaded
/// data's sampling-rate band exactly as snuffler narrows its controls on every
/// pile change (`adjust_controls`, `pile_viewer.py:4843`): the top corner is the
/// finest trace's Nyquist, the bottom four decades under the coarsest trace's.
fn filter_range(deltat_range: (f64, f64)) -> (f64, f64) {
    let (dtmin, dtmax) = deltat_range;
    ((0.5 / dtmax) * 0.0001, 0.5 / dtmin)
}

fn filter_to_unit(hz: f64, min_hz: f64, max_hz: f64) -> f64 {
    let hz = hz.clamp(min_hz, max_hz);
    (hz / min_hz).ln() / (max_hz / min_hz).ln()
}

fn unit_to_filter(unit: f64, min_hz: f64, max_hz: f64) -> f64 {
    min_hz * (max_hz / min_hz).powf(unit.clamp(0.0, 1.0))
}

fn highpass_to_pos(value: Option<f64>, min_hz: f64, max_hz: f64) -> f64 {
    let Some(hz) = value else {
        return 0.0;
    };
    filter_to_unit(hz, min_hz, max_hz)
}

fn pos_to_highpass(pos: f64, min_hz: f64, max_hz: f64) -> Option<f64> {
    if pos <= 0.0 {
        return None;
    }
    Some(unit_to_filter(pos, min_hz, max_hz))
}

fn lowpass_to_pos(value: Option<f64>, min_hz: f64, max_hz: f64) -> f64 {
    let Some(hz) = value else {
        return 1.0;
    };
    filter_to_unit(hz, min_hz, max_hz)
}

fn pos_to_lowpass(pos: f64, min_hz: f64, max_hz: f64) -> Option<f64> {
    if pos >= 1.0 {
        return None;
    }
    Some(unit_to_filter(pos, min_hz, max_hz))
}

struct ControlRowResponse {
    label: egui::Response,
    value: egui::Response,
}

fn control_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    display_value: String,
    add: impl FnOnce(&mut egui::Ui),
) -> ControlRowResponse {
    ui.horizontal(|ui| {
        let value_id = ui.make_persistent_id(("control-value", label));
        if !ui.memory(|m| m.has_focus(value_id)) {
            *value = display_value;
        }

        let row_h = ui.spacing().interact_size.y;
        let (label_rect, label_resp) =
            ui.allocate_exact_size(egui::vec2(LABEL_W, row_h), egui::Sense::click());
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
        label_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), label)
        });
        let value_resp = ui.add_sized(
            [VALUE_W, row_h],
            egui::TextEdit::singleline(value)
                .id(value_id)
                .font(egui::TextStyle::Monospace)
                .interactive(true),
        );
        ui.spacing_mut().slider_width = ui.available_width().max(160.0);
        add(ui);
        ControlRowResponse {
            label: label_resp,
            value: value_resp,
        }
    })
    .inner
}

fn value_committed(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.changed()
        || response.lost_focus()
        || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    ntracks_total: usize,
    deltat_range: (f64, f64),
) {
    let waterfall = state.view_mode == ViewMode::Waterfall;
    let (min_hz, max_hz) = filter_range(deltat_range);
    // Loading finer or coarser data moves the band; corners set for the old
    // pile follow it in, as Qt's controls clamp their value on `set_range`.
    for corner in [&mut state.highpass, &mut state.lowpass] {
        if let Some(hz) = corner.as_mut() {
            *hz = hz.clamp(min_hz, max_hz);
        }
    }

    let mut highpass_pos = highpass_to_pos(state.highpass, min_hz, max_hz);
    let mut highpass_slider_changed = false;
    let highpass_row = control_row(
        ui,
        "Highpass [Hz]:",
        &mut state.highpass_text,
        format_hz(state.highpass),
        |ui| {
            highpass_slider_changed = ui
                .add(egui::Slider::new(&mut highpass_pos, 0.0..=1.0).show_value(false))
                .changed();
        },
    );
    if highpass_row.label.double_clicked() {
        state.highpass = None;
        state.highpass_text = format_hz(state.highpass);
    } else if highpass_slider_changed {
        state.highpass = pos_to_highpass(highpass_pos, min_hz, max_hz);
        if let Some(hz) = state.highpass {
            state.highpass_hz = hz;
        }
        if !highpass_row.value.has_focus() {
            state.highpass_text = format_hz(state.highpass);
        }
    } else if value_committed(ui, &highpass_row.value) {
        if let Some(parsed) = parse_hz(&state.highpass_text, min_hz, max_hz) {
            state.highpass = match parsed {
                ParsedFilter::Off => None,
                ParsedFilter::Hz(hz) => Some(hz),
            };
            if let Some(hz) = state.highpass {
                state.highpass_hz = hz;
            }
            if highpass_row.value.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                state.highpass_text = format_hz(state.highpass);
            }
        } else if highpass_row.value.lost_focus() {
            state.highpass_text = format_hz(state.highpass);
        }
    }

    let mut lowpass_pos = lowpass_to_pos(state.lowpass, min_hz, max_hz);
    let mut lowpass_slider_changed = false;
    let lowpass_row = control_row(
        ui,
        "Lowpass [Hz]:",
        &mut state.lowpass_text,
        format_hz(state.lowpass),
        |ui| {
            lowpass_slider_changed = ui
                .add(egui::Slider::new(&mut lowpass_pos, 0.0..=1.0).show_value(false))
                .changed();
        },
    );
    if lowpass_row.label.double_clicked() {
        state.lowpass = None;
        state.lowpass_text = format_hz(state.lowpass);
    } else if lowpass_slider_changed {
        state.lowpass = pos_to_lowpass(lowpass_pos, min_hz, max_hz);
        if let Some(hz) = state.lowpass {
            state.lowpass_hz = hz;
        }
        if !lowpass_row.value.has_focus() {
            state.lowpass_text = format_hz(state.lowpass);
        }
    } else if value_committed(ui, &lowpass_row.value) {
        if let Some(parsed) = parse_hz(&state.lowpass_text, min_hz, max_hz) {
            state.lowpass = match parsed {
                ParsedFilter::Off => None,
                ParsedFilter::Hz(hz) => Some(hz),
            };
            if let Some(hz) = state.lowpass {
                state.lowpass_hz = hz;
            }
            if lowpass_row.value.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                state.lowpass_text = format_hz(state.lowpass);
            }
        } else if lowpass_row.value.lost_focus() {
            state.lowpass_text = format_hz(state.lowpass);
        }
    }

    // snuffler's passband_check warning is posted to the permanent status line
    // (`app.rs`), the counterpart of Qt's status area — a warning drawn here
    // would vanish with the panel `c` hides.

    // Gain is meaningless for the waterfall, so snuffler hides it there.
    if !waterfall {
        let mut gain = state.gain;
        let mut gain_slider_changed = false;
        let gain_row = control_row(
            ui,
            "Gain:",
            &mut state.gain_text,
            format_compact(f64::from(state.gain), 3),
            |ui| {
                gain_slider_changed = ui
                    .add(
                        egui::Slider::new(&mut gain, 0.001..=1000.0)
                            .logarithmic(true)
                            .show_value(false),
                    )
                    .changed();
            },
        );
        if gain_row.label.double_clicked() {
            state.gain = DEFAULT_GAIN;
            state.gain_text = format_compact(f64::from(state.gain), 3);
        } else if gain_slider_changed {
            state.gain = gain;
            if !gain_row.value.has_focus() {
                state.gain_text = format_compact(f64::from(state.gain), 3);
            }
        } else if value_committed(ui, &gain_row.value) {
            if let Some(parsed) = parse_gain(&state.gain_text) {
                state.gain = parsed;
                if gain_row.value.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    state.gain_text = format_compact(f64::from(state.gain), 3);
                }
            } else if gain_row.value.lost_focus() {
                state.gain_text = format_compact(f64::from(state.gain), 3);
            }
        }
    }

    let mut rotate = state.rotate_deg;
    let mut rotate_slider_changed = false;
    let rotate_row = control_row(
        ui,
        "Rotate [deg]:",
        &mut state.rotate_text,
        format_compact(state.rotate_deg, 1),
        |ui| {
            rotate_slider_changed = ui
                .add(egui::Slider::new(&mut rotate, -180.0..=180.0).show_value(false))
                .changed();
        },
    );
    if rotate_row.label.double_clicked() {
        state.rotate_deg = DEFAULT_ROTATE_DEG;
        state.rotate_text = format_compact(state.rotate_deg, 1);
    } else if rotate_slider_changed {
        state.rotate_deg = rotate;
        if !rotate_row.value.has_focus() {
            state.rotate_text = format_compact(state.rotate_deg, 1);
        }
    } else if value_committed(ui, &rotate_row.value) {
        if let Some(parsed) = parse_rotate(&state.rotate_text) {
            state.rotate_deg = parsed;
            if rotate_row.value.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                state.rotate_text = format_compact(state.rotate_deg, 1);
            }
        } else if rotate_row.value.lost_focus() {
            state.rotate_text = format_compact(state.rotate_deg, 1);
        }
    }

    ui.separator();

    ui.horizontal_wrapped(|ui| {
        // Track height (pixels per row). With "Align" on, the layout snaps this to
        // a height that tiles the canvas with whole tracks; off lets rows take the
        // exact height and scroll continuously.
        let mut height = if state.track_height > 0.0 {
            state.track_height
        } else {
            40.0
        };
        ui.label("Track height:");
        let resp = ui.add(egui::Slider::new(&mut height, MIN_TRACK_H..=MAX_TRACK_H).suffix(" px"));
        if resp.changed() {
            state.track_height = height;
        }
        resp.on_hover_text(format!("{} tracks total", ntracks_total.max(1)));
        ui.checkbox(&mut state.snap_tracks, "Align")
            .on_hover_text("Snap track height and scrolling to whole traces");

        ui.separator();
        // The pointer twin of the digit keys: clicking a kind recolours the
        // selected markers (or retargets the floating pick), exactly what the
        // key does — it never latches a kind for future picks, which in
        // snuffler are always kind 0. `marker_kind` only remembers the last
        // applied choice for the highlight.
        ui.label("Marker:");
        for k in 0u8..8 {
            let Some(&col) = snuffler_web_core::markers::MARKER_COLORS.get(usize::from(k)) else {
                continue;
            };
            let selected = state.marker_kind == k;
            let txt = egui::RichText::new(format!("{k}")).color(col).strong();
            if ui
                .selectable_label(selected, txt)
                .on_hover_text("Change color of selected markers")
                .clicked()
            {
                state.marker_kind = k;
                if let Some(pk) = state.picking.as_mut() {
                    pk.kind = k;
                } else {
                    for m in &mut state.markers {
                        if m.selected {
                            m.kind = k;
                        }
                    }
                }
            }
        }
    });

    // Waterfall colorbar row (snuffler's ColorbarControl, shown only in waterfall).
    if waterfall {
        ui.horizontal_wrapped(|ui| {
            // `from_label` rather than a detached `ui.label` + `from_id_salt`:
            // the label is what names the combo in the accessibility tree, and a
            // nameless picker reads as whatever value it happens to hold.
            egui::ComboBox::from_label("Colormap")
                .selected_text(state.wf_cmap.label())
                .show_ui(ui, |ui| {
                    for cm in Colormap::ALL {
                        ui.selectable_value(&mut state.wf_cmap, cm, cm.label());
                    }
                });
            egui::ComboBox::from_label("Scale")
                .selected_text(state.wf_color_scale.label())
                .show_ui(ui, |ui| {
                    for cs in ColorScale::ALL {
                        ui.selectable_value(&mut state.wf_color_scale, cs, cs.label());
                    }
                });

            ui.separator();
            ui.label("Clip low:");
            let low = ui.add(egui::Slider::new(&mut state.wf_clip.0, 0.0..=0.95).show_value(false));
            ui.label("Clip high:");
            let high =
                ui.add(egui::Slider::new(&mut state.wf_clip.1, 0.05..=1.0).show_value(false));
            // Snuffler's colorbar semantics (gui/util.py:817, :914): with Symmetry
            // on, moving either clip handle mirrors the other around 0.5; a drag
            // that would squeeze the band below its 0.05 minimum width clamps the
            // handle being dragged rather than silently pushing the other one.
            if low.changed() {
                if state.wf_symmetry {
                    state.wf_clip.1 = (1.0 - state.wf_clip.0).clamp(0.05, 1.0);
                }
                state.wf_clip.0 = state.wf_clip.0.min(state.wf_clip.1 - 0.05).max(0.0);
            }
            if high.changed() {
                if state.wf_symmetry {
                    state.wf_clip.0 = (1.0 - state.wf_clip.1).clamp(0.0, 0.95);
                }
                state.wf_clip.1 = state.wf_clip.1.max(state.wf_clip.0 + 0.05).min(1.0);
            }
            // Qt's colorbar resets its clip band on a double-click (gui/util.py:883).
            if low.double_clicked() || high.double_clicked() {
                state.wf_clip = (0.0, 1.0);
            }
            // Qt's colorbar wheel widens or narrows the clip band symmetrically,
            // Symmetry locked only, refusing a step below the minimum width
            // (`wheelEvent`, gui/util.py:886). Qt maps a 120-unit wheel notch to
            // 0.024 of the band; egui reports points, ~50 a notch, so the factor
            // keeps one notch worth one Qt notch.
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if state.wf_symmetry && scroll != 0.0 && (low.hovered() || high.hovered()) {
                let delta = -f64::from(scroll) * (0.024 / 50.0);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "clip positions live in 0..=1; f32 is the state's own precision"
                )]
                let (lo, hi) = (
                    (f64::from(state.wf_clip.0) + delta).max(0.0) as f32,
                    (f64::from(state.wf_clip.1) - delta).min(1.0) as f32,
                );
                if hi - lo >= 0.05 {
                    state.wf_clip = (lo, hi);
                }
            }

            ui.separator();
            ui.checkbox(&mut state.wf_reverse, "Reverse");
            // Symmetry is a live constraint on the clip pair, exactly snuffler's
            // (`set_symetry`, gui/util.py:817): switching it on snaps the band // spellcheck:ignore-line
            // symmetric around 0.5 by the tighter side.
            if ui.checkbox(&mut state.wf_symmetry, "Symmetry").changed() && state.wf_symmetry {
                let clip_max = 1.0 - state.wf_clip.0.min(1.0 - state.wf_clip.1);
                state.wf_clip = (1.0 - clip_max, clip_max);
            }
            // Qt un-checks Symmetry whenever Absolute goes on, and re-checks it
            // when Absolute goes off (`toggle_absolute`, gui/util.py:776).
            if ui.checkbox(&mut state.wf_abs, "Abs").changed() {
                state.wf_symmetry = !state.wf_abs;
                if state.wf_symmetry {
                    let clip_max = 1.0 - state.wf_clip.0.min(1.0 - state.wf_clip.1);
                    state.wf_clip = (1.0 - clip_max, clip_max);
                }
            }
            ui.checkbox(&mut state.wf_integrate, "Integrate");
        });
    }
}
