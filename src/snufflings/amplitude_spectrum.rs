use egui::{Align2, Color32, FontId, Sense, Shape, Stroke};

use crate::bridge::{SpectrumData, SpectrumSnufflingParams};
use crate::view::markers::MARKER_COLORS;

use super::Action;
use super::widgets::{param_row, run_button};

pub struct Config {
    show_plot: bool,
    smoothing: bool,
    smoothing_bandwidth: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_plot: false,
            smoothing: false,
            smoothing_bandwidth: 40.0,
        }
    }
}

impl Config {
    pub fn params(&self) -> SpectrumSnufflingParams {
        SpectrumSnufflingParams {
            spec_smoothing: self.smoothing,
            spec_smoothing_bandwidth: self.smoothing_bandwidth,
        }
    }

    pub fn show_plot(&self) -> bool {
        self.show_plot
    }

    pub fn open_plot(&mut self) {
        self.show_plot = true;
    }

    pub fn close_plot(&mut self) {
        self.show_plot = false;
    }
}

pub fn panel(ui: &mut egui::Ui, config: &mut Config) -> Option<Action> {
    ui.label("Plot amplitude spectra for the selected/visible traces.");
    ui.add_space(6.0);
    ui.checkbox(&mut config.smoothing, "Smoothing");
    param_row(
        ui,
        "Smoothing band width",
        &mut config.smoothing_bandwidth,
        1.0,
        100.0,
    );
    run_button(ui).then_some(Action::Spectrum)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "screen-space plotting math: f64 frequencies map to f32 pixel coordinates and sample indices to f64 fractions; precision is ample for on-screen rendering"
)]
pub fn draw_plot(ui: &mut egui::Ui, spec: Option<&SpectrumData>) {
    let Some(spec) = spec else {
        ui.label("Computing...");
        return;
    };
    if spec.meta.tracks.is_empty() {
        ui.label("No spectrum (web only).");
        return;
    }
    let dark = ui.visuals().dark_mode;
    let (resp, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width().max(240.0), 240.0),
        Sense::hover(),
    );
    let rect = resp.rect;
    let bg = if dark {
        Color32::from_rgb(22, 24, 27)
    } else {
        Color32::from_rgb(250, 250, 250)
    };
    painter.rect_filled(rect, 0.0, bg);

    let fmin = spec.meta.fmin.max(1e-6);
    let fmax = spec.meta.fmax.max(fmin * 1.0001);
    let (lxmin, lxmax) = (fmin.log10(), fmax.log10());
    let ymin = spec
        .meta
        .tracks
        .iter()
        .map(|t| t.ymin)
        .fold(f32::INFINITY, f32::min);
    let ymax = spec
        .meta
        .tracks
        .iter()
        .map(|t| t.ymax)
        .fold(f32::NEG_INFINITY, f32::max);
    let yspan = (ymax - ymin).max(1e-6);
    let x_of = |f: f64| rect.left() + ((f.log10() - lxmin) / (lxmax - lxmin)) as f32 * rect.width();
    let y_of = |v: f32| rect.bottom() - (v - ymin) / yspan * rect.height();

    let grid = if dark {
        Color32::from_rgb(48, 51, 56)
    } else {
        Color32::from_rgb(225, 225, 225)
    };
    let mut decade = 10f64.powf(lxmin.floor());
    while decade <= fmax {
        if decade >= fmin {
            let x = x_of(decade);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(1.0, grid),
            );
        }
        decade *= 10.0;
    }

    for (ti, t) in spec.meta.tracks.iter().enumerate() {
        let col = MARKER_COLORS
            .get(ti % 8)
            .copied()
            .unwrap_or(Color32::from_rgb(204, 0, 0));
        let end = (t.offset + t.count).min(spec.ys.len());
        let Some(data) = spec.ys.get(t.offset.min(spec.ys.len())..end) else {
            continue;
        };
        if data.len() < 2 {
            continue;
        }
        let n = data.len();
        let pts: Vec<_> = data
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let frac = i as f64 / (n - 1) as f64;
                let f = 10f64.powf(lxmin + frac * (lxmax - lxmin));
                egui::pos2(x_of(f), y_of(v))
            })
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.0, col)));
    }
    painter.text(
        rect.left_top() + egui::vec2(5.0, 3.0),
        Align2::LEFT_TOP,
        format!("{fmin:.3} - {fmax:.0} Hz  (log freq, log amp)"),
        FontId::proportional(11.0),
        if dark {
            Color32::GRAY
        } else {
            Color32::DARK_GRAY
        },
    );
}
