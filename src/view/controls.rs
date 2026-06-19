//! The "Main Controls" panel: filters, gain, rotation, scaling, sorting, view
//! mode, track count. Mirrors snuffler's bottom control strip.

use crate::state::{AppState, ScalingMode, SortMode, ViewMode};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ntracks_total: usize) {
    ui.horizontal_wrapped(|ui| {
        let mut hp_on = state.highpass.is_some();
        ui.checkbox(&mut hp_on, "HP");
        ui.add_enabled(
            hp_on,
            egui::Slider::new(&mut state.highpass_hz, 0.001..=50.0)
                .logarithmic(true)
                .suffix(" Hz")
                .text("Highpass"),
        );
        state.highpass = hp_on.then_some(state.highpass_hz);

        ui.separator();

        let mut lp_on = state.lowpass.is_some();
        ui.checkbox(&mut lp_on, "LP");
        ui.add_enabled(
            lp_on,
            egui::Slider::new(&mut state.lowpass_hz, 0.01..=100.0)
                .logarithmic(true)
                .suffix(" Hz")
                .text("Lowpass"),
        );
        state.lowpass = lp_on.then_some(state.lowpass_hz);

        ui.separator();

        ui.add(
            egui::Slider::new(&mut state.gain, 0.001..=1000.0)
                .logarithmic(true)
                .text("Gain"),
        );

        ui.separator();

        ui.add(
            egui::Slider::new(&mut state.rotate_deg, -180.0..=180.0)
                .suffix("°")
                .text("Rotate"),
        );
    });

    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_label("View")
            .selected_text(match state.view_mode {
                ViewMode::Wiggle => "Wiggle",
                ViewMode::Waterfall => "Waterfall",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.view_mode, ViewMode::Wiggle, "Wiggle");
                ui.selectable_value(
                    &mut state.view_mode,
                    ViewMode::Waterfall,
                    "Waterfall (spectrogram)",
                );
            });

        if state.view_mode == ViewMode::Waterfall {
            ui.add(
                egui::Slider::new(&mut state.wf_clip.0, 0.0..=0.8)
                    .text("Contrast")
                    .show_value(false),
            );
        }

        ui.separator();

        egui::ComboBox::from_label("Scaling")
            .selected_text(match state.scaling {
                ScalingMode::Individual => "Individual",
                ScalingMode::Common => "Common",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.scaling, ScalingMode::Individual, "Individual");
                ui.selectable_value(&mut state.scaling, ScalingMode::Common, "Common");
            });

        ui.separator();

        egui::ComboBox::from_label("Sort")
            .selected_text(match state.sort {
                SortMode::Name => "Name",
                SortMode::Reverse => "Name ↓",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.sort, SortMode::Name, "Name");
                ui.selectable_value(&mut state.sort, SortMode::Reverse, "Name ↓");
            });

        ui.separator();

        let max_tracks = ntracks_total.max(1);
        ui.add(egui::Slider::new(&mut state.ntracks_shown, 1..=max_tracks).text("Tracks shown"));

        ui.separator();
        ui.label("marker:");
        for k in 0u8..8 {
            let col = crate::view::markers::MARKER_COLORS[k as usize];
            let selected = state.marker_kind == k;
            let txt = egui::RichText::new(format!("{k}")).color(col).strong();
            if ui.selectable_label(selected, txt).clicked() {
                state.marker_kind = k;
            }
        }
    });
}
