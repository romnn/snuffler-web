//! The "Main Controls" panel: filters, gain, rotation, scaling, sorting, view
//! mode, the waterfall colorbar, and track height — mirroring snuffler's bottom
//! control strip (`pile_viewer.py` `controls()` + `ColorbarControl`).

use snuffler_web_core::state::{
    AppState, Colormap, MAX_TRACK_H, MIN_TRACK_H, ScalingBase, ScalingGroup, SortMode, Subsort,
    ViewMode, WiggleTheme,
};

const FILTER_MIN_HZ: f64 = 0.001;
const FILTER_MAX_HZ: f64 = 25.0;
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

fn parse_hz(text: &str) -> Option<ParsedFilter> {
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
    Some(ParsedFilter::Hz(hz.clamp(FILTER_MIN_HZ, FILTER_MAX_HZ)))
}

fn parse_gain(text: &str) -> Option<f32> {
    let gain = text.trim().parse::<f32>().ok()?;
    gain.is_finite().then_some(gain.clamp(0.001, 1000.0))
}

fn parse_rotate(text: &str) -> Option<f64> {
    let rotate = text.trim().parse::<f64>().ok()?;
    rotate.is_finite().then_some(rotate.clamp(-180.0, 180.0))
}

fn filter_to_unit(hz: f64) -> f64 {
    let hz = hz.clamp(FILTER_MIN_HZ, FILTER_MAX_HZ);
    (hz / FILTER_MIN_HZ).ln() / (FILTER_MAX_HZ / FILTER_MIN_HZ).ln()
}

fn unit_to_filter(unit: f64) -> f64 {
    FILTER_MIN_HZ * (FILTER_MAX_HZ / FILTER_MIN_HZ).powf(unit.clamp(0.0, 1.0))
}

fn highpass_to_pos(value: Option<f64>) -> f64 {
    let Some(hz) = value else {
        return 0.0;
    };
    filter_to_unit(hz)
}

fn pos_to_highpass(pos: f64) -> Option<f64> {
    if pos <= 0.0 {
        return None;
    }
    Some(unit_to_filter(pos))
}

fn lowpass_to_pos(value: Option<f64>) -> f64 {
    let Some(hz) = value else {
        return 1.0;
    };
    filter_to_unit(hz)
}

fn pos_to_lowpass(pos: f64) -> Option<f64> {
    if pos >= 1.0 {
        return None;
    }
    Some(unit_to_filter(pos))
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
pub fn show(ui: &mut egui::Ui, state: &mut AppState, ntracks_total: usize) {
    let waterfall = state.view_mode == ViewMode::Waterfall;

    let mut highpass_pos = highpass_to_pos(state.highpass);
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
        state.highpass = pos_to_highpass(highpass_pos);
        if let Some(hz) = state.highpass {
            state.highpass_hz = hz;
        }
        if !highpass_row.value.has_focus() {
            state.highpass_text = format_hz(state.highpass);
        }
    } else if value_committed(ui, &highpass_row.value) {
        if let Some(parsed) = parse_hz(&state.highpass_text) {
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

    let mut lowpass_pos = lowpass_to_pos(state.lowpass);
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
        state.lowpass = pos_to_lowpass(lowpass_pos);
        if let Some(hz) = state.lowpass {
            state.lowpass_hz = hz;
        }
        if !lowpass_row.value.has_focus() {
            state.lowpass_text = format_hz(state.lowpass);
        }
    } else if value_committed(ui, &lowpass_row.value) {
        if let Some(parsed) = parse_hz(&state.lowpass_text) {
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

    // snuffler's passband_check: warn when highpass corner >= lowpass corner.
    if let (Some(hp), Some(lp)) = (state.highpass, state.lowpass)
        && hp >= lp
    {
        ui.colored_label(
            egui::Color32::from_rgb(0xCF, 0x8E, 0x00),
            "⚠ Highpass ≥ Lowpass",
        );
    }

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
        egui::ComboBox::from_label("View")
            .selected_text(match state.view_mode {
                ViewMode::Wiggle => "Wiggle",
                ViewMode::Waterfall => "Waterfall",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.view_mode, ViewMode::Wiggle, "Wiggle");
                ui.selectable_value(&mut state.view_mode, ViewMode::Waterfall, "Waterfall");
            });
        if !waterfall {
            egui::ComboBox::from_label("Theme")
                .selected_text(state.wiggle_theme.label())
                .show_ui(ui, |ui| {
                    for theme in WiggleTheme::ALL {
                        ui.selectable_value(&mut state.wiggle_theme, theme, theme.label());
                    }
                });
        }

        ui.separator();

        egui::ComboBox::from_label("Scaling")
            .selected_text(state.scaling_group.label())
            .show_ui(ui, |ui| {
                for g in [
                    ScalingGroup::Individual,
                    ScalingGroup::Common,
                    ScalingGroup::PerStation,
                    ScalingGroup::PerStationLocation,
                    ScalingGroup::PerComponent,
                ] {
                    ui.selectable_value(&mut state.scaling_group, g, g.label());
                }
            });
        egui::ComboBox::from_label("Base")
            .selected_text(state.scaling_base.label())
            .show_ui(ui, |ui| {
                for b in [
                    ScalingBase::MinMax,
                    ScalingBase::Robust,
                    ScalingBase::Mean2Std,
                    ScalingBase::Mean4Std,
                ] {
                    ui.selectable_value(&mut state.scaling_base, b, b.label());
                }
            });

        ui.separator();

        egui::ComboBox::from_label("Sort")
            .selected_text(state.sort.label())
            .show_ui(ui, |ui| {
                for m in [
                    SortMode::Name,
                    SortMode::Distance,
                    SortMode::Azimuth,
                    SortMode::AzimuthBlocks,
                    SortMode::Backazimuth,
                ] {
                    ui.selectable_value(&mut state.sort, m, m.label());
                }
            });
        egui::ComboBox::from_label("Group")
            .selected_text(state.subsort.as_str())
            .show_ui(ui, |ui| {
                for ss in [
                    Subsort::Nslc,
                    Subsort::Nscl,
                    Subsort::Sncl,
                    Subsort::Lnsc,
                    Subsort::Cnsl,
                    Subsort::NscGroupL,
                    Subsort::SncGroupL,
                ] {
                    ui.selectable_value(&mut state.subsort, ss, ss.label());
                }
            });

        ui.separator();

        // Track height (pixels per row). With "Align" on, the layout snaps this to
        // a height that tiles the canvas with whole tracks; off lets rows take the
        // exact height and scroll continuously.
        let mut height = if state.track_height > 0.0 {
            state.track_height
        } else {
            40.0
        };
        let resp = ui.add(
            egui::Slider::new(&mut height, MIN_TRACK_H..=MAX_TRACK_H)
                .text("Track height")
                .suffix(" px"),
        );
        if resp.changed() {
            state.track_height = height;
        }
        resp.on_hover_text(format!("{} tracks total", ntracks_total.max(1)));
        ui.checkbox(&mut state.snap_tracks, "Align")
            .on_hover_text("Snap track height and scrolling to whole traces");

        ui.separator();
        ui.label("marker:");
        for k in 0u8..8 {
            let Some(&col) = snuffler_web_core::markers::MARKER_COLORS.get(usize::from(k)) else {
                continue;
            };
            let selected = state.marker_kind == k;
            let txt = egui::RichText::new(format!("{k}")).color(col).strong();
            if ui.selectable_label(selected, txt).clicked() {
                state.marker_kind = k;
            }
        }
    });

    // Waterfall colorbar row (snuffler's ColorbarControl, shown only in waterfall).
    if waterfall {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_label("Colormap")
                .selected_text(state.wf_cmap.label())
                .show_ui(ui, |ui| {
                    for cm in Colormap::ALL {
                        ui.selectable_value(&mut state.wf_cmap, cm, cm.label());
                    }
                });

            ui.separator();
            ui.add(
                egui::Slider::new(&mut state.wf_clip.0, 0.0..=0.95)
                    .text("clip lo")
                    .show_value(false),
            );
            ui.add(
                egui::Slider::new(&mut state.wf_clip.1, 0.05..=1.0)
                    .text("clip hi")
                    .show_value(false),
            );
            if state.wf_clip.0 > state.wf_clip.1 - 0.05 {
                state.wf_clip.0 = (state.wf_clip.1 - 0.05).max(0.0);
            }

            ui.separator();
            ui.checkbox(&mut state.wf_reverse, "Reverse");
            ui.checkbox(&mut state.wf_symmetry, "Symmetry");
            ui.checkbox(&mut state.wf_abs, "Abs");
            ui.checkbox(&mut state.wf_integrate, "Integrate");
        });
    }
}
