//! The snuffling panel and the actions its buttons and menus ask for.

use super::App;
use crate::snufflings::Action as SnufflingAction;

impl App {
    pub(super) fn show_snuffling_windows(&mut self, ctx: &egui::Context) {
        if let Some(name) = self.session.bridge.take_pending_cake_model() {
            if name.starts_with("GF Store: ") {
                self.session.snufflings.add_gf_store(name);
            } else {
                self.session.snufflings.add_cake_model(name);
            }
        }

        let seisbench = self.session.bridge.seisbench_status();
        let runtime = crate::snufflings::SeisbenchRuntimeStatus {
            device: seisbench.device,
            progress: seisbench
                .progress
                .map(|progress| crate::snufflings::SeisbenchProgress {
                    fraction: progress.fraction,
                    label: progress.label,
                }),
        };
        // The pile's Nyquist, for Qt's corner-range narrowing; 0 deltat = no data.
        let deltatmin = self.session.scene_deltatmin();
        let nyquist = (deltatmin > 0.0).then(|| 0.5 / deltatmin);
        if let Some(action) = crate::snufflings::show_panel(
            ctx,
            &mut self.snuffling_panels,
            &mut self.session.snufflings,
            self.session.state.highpass,
            self.session.state.lowpass,
            nyquist,
            &runtime,
        ) {
            self.apply_snuffling_action(action);
        }
        self.figures.show(ctx, &mut self.session);
    }

    pub(super) fn apply_snuffling_action(&mut self, action: SnufflingAction) {
        let params = self.session.snuffling_params(800);
        match action {
            SnufflingAction::Spectrum => {
                self.session.snufflings.spectrum.open_plot();
                self.figures.invalidate("ampspec");
            }
            SnufflingAction::Spectrogram => {
                self.session.snufflings.spectrogram.open_plot();
                self.figures.invalidate("spectrogram");
            }
            SnufflingAction::Polarization => {
                self.session.snufflings.polarization.open_plot();
                self.figures.invalidate("polarization");
            }
            SnufflingAction::Run(name) => {
                let _submission = self.session.bridge.run_snuffling(name.into(), params);
            }
            SnufflingAction::ReloadSnufflings => {
                self.session.state.reload_snufflings_requested = true;
            }
            SnufflingAction::GeofonPreset(min_magnitude) => {
                // The visible time range, as Qt's presets query it.
                // FDSN wants ISO stamps; `format_timestamp` writes a space, so swap in the 'T'.
                let iso =
                    |t: f64| snuffler_web_core::timeaxis::format_timestamp(t).replacen(' ', "T", 1);
                let state = &self.session.state;
                let request = snuffler_web_core::bridge::CatalogRequest {
                    datacenter: "geofon".to_owned(),
                    start: iso(state.tmin),
                    end: iso(state.tmax),
                    min_magnitude,
                    with_moment_tensors: state.catalog_form.with_moment_tensors,
                    marker_budget: Some(
                        snuffler_web_core::markers::MarkerBudget::standard().into(),
                    ),
                };
                let _submission = self.session.bridge.catalog_search(request);
            }
            SnufflingAction::Rms => {
                let _submission = self.session.bridge.rms_traces(params);
            }
            SnufflingAction::CakePlotModel => {
                self.session.snufflings.cake.open_model_plot();
                self.figures.invalidate("cake_model");
            }
            SnufflingAction::CakePlotRays => {
                self.session.snufflings.cake.open_rays_plot();
                self.figures.invalidate("cake_rays");
            }
            SnufflingAction::AddCakeModel => {
                let _submission = self.session.bridge.add_cake_model();
            }
            SnufflingAction::LoadGfStoreUrl => {
                let url = self.session.snufflings.seismosizer.load_url();
                if !url.trim().is_empty() {
                    let _submission = self.session.bridge.load_gf_store_url(url);
                }
            }
            #[cfg(feature = "gf")]
            SnufflingAction::AddGfStore => {
                let _submission = self.session.bridge.add_gf_store();
            }
        }
    }
}
