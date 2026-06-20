//! The "Main Controls" panel: filters, gain, rotation, scaling, sorting, view
//! mode, the waterfall colorbar, and track height — mirroring snuffler's bottom
//! control strip (`pile_viewer.py` `controls()` + `ColorbarControl`).

use crate::state::{
    AppState, ColorScale, Colormap, MAX_TRACK_H, MIN_TRACK_H, ScalingBase, ScalingGroup, SortMode,
    Subsort, ViewMode, WiggleTheme,
};

const FILTER_MIN_HZ: f64 = 0.001;
const FILTER_MAX_HZ: f64 = 25.0;
const LABEL_W: f32 = 104.0;
const VALUE_W: f32 = 96.0;

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

fn control_row(ui: &mut egui::Ui, label: &str, mut value: String, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let row_h = ui.spacing().interact_size.y;
        let (label_rect, _) =
            ui.allocate_exact_size(egui::vec2(LABEL_W, row_h), egui::Sense::hover());
        ui.painter().text(
            label_rect.right_center(),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().text_color(),
        );
        ui.add_sized(
            [VALUE_W, row_h],
            egui::TextEdit::singleline(&mut value)
                .font(egui::TextStyle::Monospace)
                .interactive(false),
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

    let mut highpass_pos = highpass_to_pos(state.highpass);
    control_row(ui, "Highpass [Hz]:", format_hz(state.highpass), |ui| {
        ui.add(egui::Slider::new(&mut highpass_pos, 0.0..=1.0).show_value(false));
    });
    state.highpass = pos_to_highpass(highpass_pos);
    if let Some(hz) = state.highpass {
        state.highpass_hz = hz;
    }

    let mut lowpass_pos = lowpass_to_pos(state.lowpass);
    control_row(ui, "Lowpass [Hz]:", format_hz(state.lowpass), |ui| {
        ui.add(egui::Slider::new(&mut lowpass_pos, 0.0..=1.0).show_value(false));
    });
    state.lowpass = pos_to_lowpass(lowpass_pos);
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
        control_row(
            ui,
            "Gain:",
            format_compact(f64::from(state.gain), 3),
            |ui| {
                ui.add(
                    egui::Slider::new(&mut state.gain, 0.001..=1000.0)
                        .logarithmic(true)
                        .show_value(false),
                );
            },
        );
    }

    control_row(
        ui,
        "Rotate [deg]:",
        format_compact(state.rotate_deg, 1),
        |ui| {
            ui.add(egui::Slider::new(&mut state.rotate_deg, -180.0..=180.0).show_value(false));
        },
    );

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
