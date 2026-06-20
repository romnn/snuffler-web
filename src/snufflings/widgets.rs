use egui::{Align, DragValue, Slider};

pub fn param_row(ui: &mut egui::Ui, label: &str, value: &mut f64, min: f64, max: f64) {
    ui.horizontal(|ui| {
        ui.add_sized([160.0, 0.0], egui::Label::new(label).halign(Align::Max));
        ui.add(
            DragValue::new(value)
                .speed((max - min) / 500.0)
                .range(min..=max),
        );
        ui.add(Slider::new(value, min..=max).show_value(false));
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
    let mut pos = match *value {
        Some(v) => freq_to_pos(v, min, max),
        None if low_is_none => 0.0,
        None => 1.0,
    };
    ui.horizontal(|ui| {
        ui.add_sized([160.0, 0.0], egui::Label::new(label).halign(Align::Max));
        ui.label(value.map_or_else(|| "off".to_owned(), |v| format!("{v:.4}")));
        ui.add(Slider::new(&mut pos, 0.0..=1.0).show_value(false));
    });
    *value = if (low_is_none && pos <= 0.0) || (!low_is_none && pos >= 1.0) {
        None
    } else {
        Some(pos_to_freq(pos, min, max))
    };
}

pub fn run_button(ui: &mut egui::Ui) -> bool {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.button("Run").clicked()
    })
    .inner
}

fn freq_to_pos(value: f64, min: f64, max: f64) -> f64 {
    ((value.clamp(min, max).ln() - min.ln()) / (max.ln() - min.ln())).clamp(0.0, 1.0)
}

fn pos_to_freq(pos: f64, min: f64, max: f64) -> f64 {
    (min.ln() + pos.clamp(0.0, 1.0) * (max.ln() - min.ln())).exp()
}
