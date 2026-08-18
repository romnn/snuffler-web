pub use snuffler_web_core::snufflings::seismosizer::Config;

use super::Action;
use super::widgets::{param_row, run_button};

const WAVEFORM_TYPES: [&str; 6] = [
    "Displacement [m]",
    "Displacement [nm]",
    "Velocity [m/s]",
    "Velocity [nm/s]",
    "Acceleration [m/s^2]",
    "Acceleration [nm/s^2]",
];

/// Qt's four source-time-function choices (seismosizer.py:35).
const STF_TYPES: [&str; 4] = ["half sin", "triangular", "boxcar", "None"];

/// Qt's five Seismosizer source snufflings (seismosizer.py:280-560), as one
/// panel with a source-type choice: same parameters, one window.
const SOURCE_TYPES: [&str; 5] = [
    "DCSource",
    "MTSource",
    "SFSource",
    "RectangularSource",
    "PseudoDynamicRupture",
];

fn choice_row(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 0.0],
            egui::Label::new(label).halign(egui::Align::Max),
        );
        egui::ComboBox::from_id_salt(label)
            .selected_text(value.as_str())
            .show_ui(ui, |ui| {
                for opt in options {
                    ui.selectable_value(value, (*opt).to_owned(), *opt);
                }
            });
    });
}

#[expect(
    clippy::too_many_lines,
    reason = "Qt's five source snufflings in one panel: one parameter row each"
)]
pub fn panel(ui: &mut egui::Ui, config: &mut Config) -> Option<Action> {
    ui.label("Synthetic seismograms from any of Qt's five source models (pyrocko.gf).");
    ui.add_space(6.0);
    let mut action = None;

    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 0.0],
            egui::Label::new("GF Store").halign(egui::Align::Max),
        );
        egui::ComboBox::from_id_salt("seis_store")
            .selected_text(if config.store.is_empty() {
                "<none>"
            } else {
                config.store.as_str()
            })
            .show_ui(ui, |ui| {
                let stores = config.stores.clone();
                for s in &stores {
                    ui.selectable_value(&mut config.store, s.clone(), s);
                }
            });
        #[cfg(feature = "gf")]
        if ui
            .button("Add GF Store…")
            .on_hover_text("Mount a Green's-functions store directory (pyrocko.gf).")
            .clicked()
        {
            action = Some(Action::AddGfStore);
        }
    });

    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 0.0],
            egui::Label::new("… or from URL").halign(egui::Align::Max),
        );
        ui.add(
            egui::TextEdit::singleline(&mut config.load_url)
                .desired_width(220.0)
                .hint_text("https://host/path/to/store"),
        );
        if ui
            .add_enabled(
                !config.load_url.trim().is_empty(),
                egui::Button::new("Load"),
            )
            .on_hover_text("Fetch a GF store's files from an HTTP base URL (needs a .manifest).")
            .clicked()
        {
            action = Some(Action::LoadGfStoreUrl);
        }
    });

    choice_row(
        ui,
        "Waveform type",
        &mut config.waveform_type,
        &WAVEFORM_TYPES,
    );
    let mut selected_source = config.source_type.clone();
    choice_row(ui, "Source type", &mut selected_source, &SOURCE_TYPES);
    // Route the change through the config so each source type keeps its own
    // dial values, as Qt's five separate snuffling panels do — rendering the
    // shared fields under another type's bounds must not clamp them away.
    config.switch_source(&selected_source);
    let source = config.source_type.as_str();
    param_row(ui, "Time [s]", &mut config.time, -50.0, 50.0);
    param_row(ui, "North shift [km]", &mut config.north_km, -50.0, 50.0);
    param_row(ui, "East shift [km]", &mut config.east_km, -50.0, 50.0);
    // Qt's point sources go 100 km above the datum — an elevated source is a
    // legitimate synthetic (seismosizer.py:297) — while the extended ruptures
    // bury their reference at it (seismosizer.py:462, :525).
    let depth_min = if matches!(source, "RectangularSource" | "PseudoDynamicRupture") {
        0.0
    } else {
        -100.0
    };
    param_row(ui, "Depth [km]", &mut config.depth_km, depth_min, 600.0);
    if matches!(source, "DCSource" | "MTSource" | "RectangularSource") {
        // Qt's MT panel reaches down to small and negative magnitudes
        // (seismosizer.py:347); the DC and rectangular panels run 0..10.
        let (mag_min, mag_max) = if source == "MTSource" {
            (-1.0, 9.0)
        } else {
            (0.0, 10.0)
        };
        param_row(ui, "Magnitude", &mut config.magnitude, mag_min, mag_max);
    }
    if matches!(
        source,
        "DCSource" | "RectangularSource" | "PseudoDynamicRupture"
    ) {
        param_row(ui, "Strike", &mut config.strike, -180.0, 180.0);
        param_row(ui, "Dip", &mut config.dip, 0.0, 90.0);
        param_row(ui, "Rake", &mut config.rake, -180.0, 180.0);
    }
    if source == "MTSource" {
        // Qt's relative components; scaled to the magnitude's moment in the
        // bridge (seismosizer.py:385).
        // The diagonals range over ±√2 and the off-diagonals over ±1
        // (seismosizer.py:349-359).
        let labels = [
            "rel Mnn", "rel Mee", "rel Mdd", "rel Mne", "rel Mnd", "rel Med",
        ];
        for (i, (value, label)) in config.rm6.iter_mut().zip(labels).enumerate() {
            let bound = if i < 3 { std::f64::consts::SQRT_2 } else { 1.0 };
            param_row(ui, label, value, -bound, bound);
        }
    }
    if source == "SFSource" {
        for (value, label) in
            config
                .forces
                .iter_mut()
                .zip(["North force [N]", "East force [N]", "Down force [N]"])
        {
            param_row(ui, label, value, -1e9, 1e9);
        }
    }
    if matches!(source, "RectangularSource" | "PseudoDynamicRupture") {
        // Qt caps a rectangular rupture at 100 km and only the pseudo-dynamic
        // one at 300 km (seismosizer.py:472, :535).
        let length_max = if source == "PseudoDynamicRupture" {
            300.0
        } else {
            100.0
        };
        param_row(ui, "Length [km]", &mut config.length_km, 0.1, length_max);
        param_row(ui, "Width [km]", &mut config.width_km, 0.1, 50.0);
        param_row(ui, "Nucleation X", &mut config.nucleation_x, -1.0, 1.0);
        param_row(ui, "Nucleation Y", &mut config.nucleation_y, -1.0, 1.0);
    }
    if source == "RectangularSource" {
        param_row(
            ui,
            "Rupture velocity [m/s]",
            &mut config.velocity,
            0.0,
            5000.0,
        );
    }
    if source == "PseudoDynamicRupture" {
        param_row(ui, "Slip [m]", &mut config.slip, 0.0, 20.0);
        param_row(ui, "Gamma", &mut config.gamma, 0.1, 1.5);
        let mut nx = f64::from(config.nx);
        let mut ny = f64::from(config.ny);
        param_row(ui, "nx", &mut nx, 1.0, 20.0);
        param_row(ui, "ny", &mut ny, 1.0, 20.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "slider-bounded 1..=20 whole patch counts"
        )]
        {
            config.nx = nx.round() as u32;
            config.ny = ny.round() as u32;
        }
        ui.horizontal(|ui| {
            ui.add_space(160.0);
            ui.checkbox(&mut config.tapered, "Tapered tractions");
        });
    }
    // Qt's STF duration ceiling is per source panel: 50 s for the point
    // sources, 100 s for the single force, 20 s for the extended ruptures
    // (seismosizer.py:307, :425, :482, :549).
    let stf_max = match source {
        "SFSource" => 100.0,
        "RectangularSource" | "PseudoDynamicRupture" => 20.0,
        _ => 50.0,
    };
    param_row(
        ui,
        "STF duration [s]",
        &mut config.stf_duration,
        0.0,
        stf_max,
    );
    choice_row(ui, "STF type", &mut config.stf_type, &STF_TYPES);

    if run_button(ui) {
        action = Some(Action::Run("seismosizer"));
    }
    action
}
