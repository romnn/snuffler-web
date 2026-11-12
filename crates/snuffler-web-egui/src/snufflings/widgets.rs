use egui::{Align, DragValue, Slider};

/// A parameter row.
///
/// Qt gives every `Param` whose minimum is above zero a logarithmic control
/// (`gui/util.py:512`) — a linear 0.1–3600 s slider puts every useful value in its first
/// three percent of travel — so this does too.
pub fn param_row(ui: &mut egui::Ui, label: &str, value: &mut f64, min: f64, max: f64) {
    ui.horizontal(|ui| {
        ui.add_sized([160.0, 0.0], egui::Label::new(label).halign(Align::Max));
        ui.add(
            DragValue::new(value)
                .speed((max - min) / 500.0)
                .range(min..=max),
        );
        ui.add(
            Slider::new(value, min..=max)
                .logarithmic(min > 0.0)
                .show_value(false),
        );
    });
}

pub fn optional_freq_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<f64>,
    min: f64,
    max: f64,
    low_is_none: bool,
) {
    let mut frequency = value.unwrap_or(if low_is_none { min } else { max });
    ui.horizontal(|ui| {
        let caption = ui.add_sized([160.0, 0.0], egui::Label::new(label).halign(Align::Max));
        ui.label(value.map_or_else(|| "off".to_owned(), |v| format!("{v:.4}")));
        ui.add(
            Slider::new(&mut frequency, min..=max)
                .logarithmic(true)
                .show_value(false),
        )
        .labelled_by(caption.id);
    });
    *value = if (low_is_none && frequency <= min) || (!low_is_none && frequency >= max) {
        None
    } else {
        Some(frequency)
    };
}

/// The highpass and lowpass corner rows of a snuffling with its own passband.
///
/// Qt re-narrows the corner ranges to the loaded band on every pile change
/// (`adjust_controls`): a corner above Nyquist is silently discarded downstream,
/// so the slider must not offer one.
pub fn passband_rows(
    ui: &mut egui::Ui,
    highpass: &mut Option<f64>,
    lowpass: &mut Option<f64>,
    nyquist: Option<f64>,
) {
    let fmax = nyquist.unwrap_or(1000.0).clamp(0.002, 1000.0);
    optional_freq_row(ui, "Highpass [Hz]", highpass, 0.001, fmax, true);
    optional_freq_row(ui, "Lowpass [Hz]", lowpass, 0.001, fmax, false);
}

/// The "Copy passband from Main" button, which takes over the main controls'
/// `(highpass, lowpass)` corners.
pub fn copy_passband_row(
    ui: &mut egui::Ui,
    (highpass, lowpass): (&mut Option<f64>, &mut Option<f64>),
    (main_highpass, main_lowpass): (Option<f64>, Option<f64>),
) {
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        if ui.button("Copy passband from Main").clicked() {
            *highpass = main_highpass;
            *lowpass = main_lowpass;
        }
    });
}

/// A checkbox aligned with the value column of the parameter rows.
pub fn checkbox_row(ui: &mut egui::Ui, value: &mut bool, label: &str) {
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.checkbox(value, label);
    });
}

/// A labelled drop-down over a fixed set of string options.
pub fn choice_row(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    ui.horizontal(|ui| {
        let caption = ui.add_sized([160.0, 0.0], egui::Label::new(label).halign(Align::Max));
        egui::ComboBox::from_id_salt(label)
            .selected_text(value.as_str())
            .show_ui(ui, |ui| {
                for opt in options {
                    ui.selectable_value(value, (*opt).to_owned(), *opt);
                }
            })
            .response
            .labelled_by(caption.id);
    });
}

pub fn run_button(ui: &mut egui::Ui) -> bool {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.button("Run").clicked()
    })
    .inner
}
