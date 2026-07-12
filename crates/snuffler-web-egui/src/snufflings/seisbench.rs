use egui::Align;

pub use snuffler_web_core::snufflings::seisbench::Config;

use super::Action;
use super::widgets::run_button;

const LABEL_W: f32 = 160.0;
const LOCAL_MODEL_BASE_URL: &str = "assets/models/";

#[derive(Clone, Copy)]
struct ModelChoice {
    label: &'static str,
    p_threshold: f64,
    s_threshold: f64,
}

const DEFAULT_MODEL: ModelChoice = ModelChoice {
    label: "PhaseNet / instance",
    p_threshold: 0.086_398_242_819_485_67,
    s_threshold: 0.119_433_303_464_473_5,
};

const MODELS: &[ModelChoice] = &[
    DEFAULT_MODEL,
    ModelChoice {
        label: "EQTransformer / original",
        p_threshold: 0.1,
        s_threshold: 0.1,
    },
    ModelChoice {
        label: "GPD / stead",
        p_threshold: 0.740_418_855_563_376_4,
        s_threshold: 0.753_522_613_817_665_6,
    },
];

pub struct Progress {
    pub fraction: f32,
    pub label: String,
}

pub struct RuntimeStatus {
    pub device: String,
    pub progress: Option<Progress>,
}

pub fn panel(ui: &mut egui::Ui, config: &mut Config, runtime: &RuntimeStatus) -> Option<Action> {
    if !cfg!(feature = "embed-data") && model_base_url().is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(0xC0, 0x60, 0x40),
            "Lean build has no SeisBench ONNX model base configured.",
        );
    }
    ui.horizontal(|ui| {
        ui.add_sized(
            [LABEL_W, 0.0],
            egui::Label::new("Device").halign(Align::Max),
        );
        ui.label(&runtime.device);
    });

    ui.horizontal(|ui| {
        ui.add_sized([LABEL_W, 0.0], egui::Label::new("Model").halign(Align::Max));
        egui::ComboBox::from_id_salt("seisbench_model")
            .selected_text(selected_model(config.selected).label)
            .show_ui(ui, |ui| {
                for (idx, choice) in MODELS.iter().enumerate() {
                    let clicked = ui
                        .selectable_value(&mut config.selected, idx, choice.label)
                        .clicked();
                    if clicked && !config.override_thresholds {
                        config.p_threshold = choice.p_threshold;
                        config.s_threshold = choice.s_threshold;
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        if ui
            .checkbox(&mut config.override_thresholds, "Override thresholds")
            .changed()
            && !config.override_thresholds
        {
            let choice = selected_model(config.selected);
            config.p_threshold = choice.p_threshold;
            config.s_threshold = choice.s_threshold;
        }
    });

    if config.override_thresholds {
        threshold_row(ui, "P threshold", &mut config.p_threshold);
        threshold_row(ui, "S threshold", &mut config.s_threshold);
    }

    if let Some(progress) = &runtime.progress {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_sized(
                [LABEL_W, 0.0],
                egui::Label::new("Progress").halign(Align::Max),
            );
            ui.add(
                egui::ProgressBar::new(progress.fraction.clamp(0.0, 1.0))
                    .desired_width(220.0)
                    .show_percentage(),
            );
        });
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W);
            ui.small(&progress.label);
        });
    }

    run_button(ui).then_some(Action::Run("seisbench_picker"))
}

fn threshold_row(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    ui.horizontal(|ui| {
        ui.add_sized([LABEL_W, 0.0], egui::Label::new(label).halign(Align::Max));
        ui.add(
            egui::DragValue::new(value)
                .speed(0.002)
                .range(0.001..=0.999),
        );
        ui.add(egui::Slider::new(value, 0.001..=0.999).show_value(false));
    });
}

fn selected_model(selected: usize) -> ModelChoice {
    MODELS.get(selected).copied().unwrap_or(DEFAULT_MODEL)
}

fn model_base_url() -> &'static str {
    if cfg!(feature = "embed-data") {
        LOCAL_MODEL_BASE_URL
    } else {
        option_env!("SNUFFLER_ONNX_MODEL_BASE_URL").unwrap_or("")
    }
}
