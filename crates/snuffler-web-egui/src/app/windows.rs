//! The auxiliary windows: help, about, the download, catalog and `SeedLink` forms,
//! the map, and the marker table.

use super::App;

impl App {
    /// Shows every auxiliary window that is open and acts on what it returns.
    pub(super) fn show_auxiliary_windows(&mut self, ctx: &egui::Context) {
        crate::view::help::show(ctx, &mut self.session.state);
        crate::view::help::show_snuffling_help(ctx, &mut self.session.state);
        // Only crosses the JS seam while the window is actually open.
        let runtime = self
            .session
            .state
            .show_about
            .then(|| self.session.bridge.runtime_info())
            .flatten();
        crate::view::about::show(ctx, &mut self.session.state, runtime.as_ref());
        // Cloned out of the bridge only while the window is open, like the
        // runtime info above.
        if self.session.state.show_dataset_note
            && let Some(note) = self.session.bridge.dataset_note()
        {
            crate::view::dataset_note::show(ctx, &mut self.session.state, &note);
        }
        let downloads = crate::view::forms::download(ctx, &mut self.session.state);
        if let Some(request) = downloads.waveforms {
            let _submission = self.session.bridge.download_waveforms(request);
        }
        if let Some(request) = downloads.stations {
            let _submission = self.session.bridge.fetch_stations(request);
        }
        if let Some(request) = downloads.responses {
            let _submission = self.session.bridge.fetch_responses(request);
        }
        if downloads.save {
            let _submission = self.session.bridge.save_waveforms_and_stations();
        }

        let seedlink = crate::view::forms::seedlink(
            ctx,
            &mut self.session.state,
            self.session.bridge.seedlink_active(),
        );
        match seedlink {
            Some(crate::view::forms::SeedlinkIntent::Start(params)) => {
                self.session.start_seedlink(params);
            }
            Some(crate::view::forms::SeedlinkIntent::Stop) => {
                self.session.bridge.stop_seedlink();
            }
            Some(crate::view::forms::SeedlinkIntent::SaveBuffer) => {
                let _submission = self.session.bridge.save_waveforms();
            }
            None => {}
        }
        if let Some(request) = crate::view::forms::catalog(ctx, &mut self.session.state) {
            let _submission = self.session.bridge.catalog_search(request);
        }
        crate::view::map::window(
            ctx,
            &mut self.map,
            &mut self.session.state,
            &self.session.bridge,
        );
        if let Some(moment_tensor) =
            self.marker_table
                .show(ctx, &mut self.session.state, &self.session.bridge)
        {
            self.figures.open_beachball(moment_tensor);
        }
        crate::snufflings::show_output(ctx, &mut self.session.state);
    }
}
