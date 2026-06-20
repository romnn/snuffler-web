//! The "Main Controls" panel: filters, gain, rotation, scaling, sorting, view
//! mode, the waterfall colorbar, and track count — mirroring snuffler's bottom
//! control strip (`pile_viewer.py` `controls()` + `ColorbarControl`).

use crate::state::{
    AppState, ColorScale, Colormap, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode,
    WiggleTheme,
};

const FILTER_MIN_HZ: f64 = 0.001;
const FILTER_MAX_HZ: f64 = 1000.0;
const LABEL_W: f32 = 76.0;
const VALUE_W: f32 = 86.0;

fn format_hz(value: Option<f64>) -> String {
    match value {
        None => "0 Hz".into(),
        Some(v) if v < 1.0 => format!("{v:.3} Hz"),
        Some(v) if v < 100.0 => format!("{v:.2} Hz"),
        Some(v) => format!("{v:.1} Hz"),
    }
}

fn filter_to_pos(value: Option<f64>) -> f64 {
    let Some(hz) = value else {
        return 0.0;
    };
    let hz = hz.clamp(FILTER_MIN_HZ, FILTER_MAX_HZ);
    (hz / FILTER_MIN_HZ).ln() / (FILTER_MAX_HZ / FILTER_MIN_HZ).ln()
}

fn pos_to_filter(pos: f64) -> Option<f64> {
    if pos <= 0.0 {
        return None;
    }
    Some(FILTER_MIN_HZ * (FILTER_MAX_HZ / FILTER_MIN_HZ).powf(pos.clamp(0.0, 1.0)))
}

fn control_row(ui: &mut egui::Ui, label: &str, value: String, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let row_h = ui.spacing().interact_size.y;
        ui.add_sized([LABEL_W, row_h], egui::Label::new(label));
        ui.allocate_ui_with_layout(
            egui::vec2(VALUE_W, row_h),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(egui::RichText::new(value).monospace());
            },
        );
        ui.spacing_mut().slider_width = ui.available_width().max(160.0);
        add(ui);
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
pub fn show(ui: &mut egui::Ui, state: &mut AppState, ntracks_total: usize) {
    let waterfall = state.view_mode == ViewMode::Waterfall;

    let mut highpass_pos = filter_to_pos(state.highpass);
    control_row(ui, "Highpass", format_hz(state.highpass), |ui| {
        ui.add(egui::Slider::new(&mut highpass_pos, 0.0..=1.0).show_value(false));
    });
    state.highpass = pos_to_filter(highpass_pos);
    if let Some(hz) = state.highpass {
        state.highpass_hz = hz;
    }

    let mut lowpass_pos = filter_to_pos(state.lowpass);
    control_row(ui, "Lowpass", format_hz(state.lowpass), |ui| {
        ui.add(egui::Slider::new(&mut lowpass_pos, 0.0..=1.0).show_value(false));
    });
    state.lowpass = pos_to_filter(lowpass_pos);
    if let Some(hz) = state.lowpass {
        state.lowpass_hz = hz;
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
        control_row(ui, "Gain", format!("{:.2}", state.gain), |ui| {
            ui.add(
                egui::Slider::new(&mut state.gain, 0.001..=1000.0)
                    .logarithmic(true)
                    .show_value(false),
            );
        });
    }

    control_row(ui, "Rotate", format!("{:.1}°", state.rotate_deg), |ui| {
        ui.add(egui::Slider::new(&mut state.rotate_deg, -180.0..=180.0).show_value(false));
    });

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

        let max_tracks = ntracks_total.max(1);
        ui.add(egui::Slider::new(&mut state.ntracks_shown, 1..=max_tracks).text("Tracks"));

        ui.separator();
        ui.label("marker:");
        for k in 0u8..8 {
            let Some(&col) = crate::view::markers::MARKER_COLORS.get(usize::from(k)) else {
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
            egui::ComboBox::from_label("Color scale")
                .selected_text(state.wf_color_scale.label())
                .show_ui(ui, |ui| {
                    for cs in [ColorScale::Log, ColorScale::Sqrt, ColorScale::Lin] {
                        ui.selectable_value(&mut state.wf_color_scale, cs, cs.label());
                    }
                });

            ui.separator();
            ui.checkbox(&mut state.wf_reverse, "Reverse");
            ui.checkbox(&mut state.wf_symmetry, "Symmetry");
            ui.checkbox(&mut state.wf_abs, "Abs");
            ui.checkbox(&mut state.wf_integrate, "Integrate");
        });
    }
}
