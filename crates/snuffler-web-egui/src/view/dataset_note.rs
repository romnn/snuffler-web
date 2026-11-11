//! What the loaded dataset says about itself.
//!
//! The window that opens when an example loads, and that `Help ▸ About this dataset` opens
//! again.
//! It is deliberately not the snuffling-output window: a snuffling result answers a question
//! that was just asked and is finished being read, while this answers "what am I looking at",
//! which someone may want back an hour later — and if the two shared a window, running one
//! RMS would throw the description away for good.

use snuffler_web_core::bridge::BundleAbout;
use snuffler_web_core::state::AppState;

pub fn show(ctx: &egui::Context, state: &mut AppState, note: &BundleAbout) {
    if !state.show_dataset_note {
        return;
    }
    let mut open = true;
    egui::Window::new("About this dataset")
        .open(&mut open)
        .default_size([560.0, 420.0])
        // Collapsed on arrival.
        // This window opens itself whenever a dataset that describes itself loads, so expanded it
        // would land on top of the very waveforms it is describing; the title bar still says there
        // is something to read, one click away.
        // `default_open` sets the initial state only — once expanded, egui remembers that.
        .default_open(false)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if !note.title.is_empty() {
                        ui.heading(&note.title);
                        ui.add_space(6.0);
                    }
                    // Wrapped proportional text, not the monospace the snuffling
                    // window uses: this is prose, and the only thing the dataset
                    // aligns in columns is its bullets, which wrap fine.
                    ui.label(&note.body);
                });
        });
    state.show_dataset_note = open;
}
