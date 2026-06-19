//! The "Main Controls" panel: filters, gain, rotation, scaling, sorting, view
//! mode, the waterfall colorbar, and track count — mirroring snuffler's bottom
//! control strip (`pile_viewer.py` `controls()` + `ColorbarControl`).

use crate::state::{
    AppState, ColorScale, Colormap, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode,
};

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
pub fn show(ui: &mut egui::Ui, state: &mut AppState, ntracks_total: usize) {
    let waterfall = state.view_mode == ViewMode::Waterfall;

    // snuffler's Main Controls: full-width sliders stacked one per row.
    let track_w = (ui.available_width() - 180.0).max(160.0);

    ui.horizontal(|ui| {
        let mut hp_on = state.highpass.is_some();
        ui.checkbox(&mut hp_on, "HP");
        ui.spacing_mut().slider_width = track_w;
        ui.add_enabled(
            hp_on,
            egui::Slider::new(&mut state.highpass_hz, 0.001..=1000.0)
                .logarithmic(true)
                .suffix(" Hz")
                .text("Highpass"),
        );
        state.highpass = hp_on.then_some(state.highpass_hz);
    });

    ui.horizontal(|ui| {
        let mut lp_on = state.lowpass.is_some();
        ui.checkbox(&mut lp_on, "LP");
        ui.spacing_mut().slider_width = track_w;
        ui.add_enabled(
            lp_on,
            egui::Slider::new(&mut state.lowpass_hz, 0.001..=1000.0)
                .logarithmic(true)
                .suffix(" Hz")
                .text("Lowpass"),
        );
        state.lowpass = lp_on.then_some(state.lowpass_hz);
    });

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
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = track_w;
            ui.add(
                egui::Slider::new(&mut state.gain, 0.001..=1000.0)
                    .logarithmic(true)
                    .text("Gain"),
            );
        });
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = track_w;
        ui.add(
            egui::Slider::new(&mut state.rotate_deg, -180.0..=180.0)
                .suffix("°")
                .text("Rotate"),
        );
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
