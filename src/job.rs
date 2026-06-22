//! A progress overlay for heavy Rust work that runs off the egui main thread.
//!
//! `update()` runs on the single browser main thread every frame, so a long
//! synchronous call there freezes the whole UI (see `AGENTS.md`). Work that would
//! block — currently the resvg PNG export — is handed to the rayon worker pool
//! (`wasm-bindgen-rayon`); the main thread keeps drawing and shows this overlay
//! until the result lands.

/// Dim the UI and show a centered, labelled progress indicator. Call every frame
/// the background work is still running. Always indeterminate (a spinner): the
/// off-thread jobs are single, opaque calls with no sub-step progress to report.
pub fn progress_overlay(ctx: &egui::Context, label: &str) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("job_overlay_scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::Pos2::ZERO)
        .interactable(true)
        .show(ctx, |ui| {
            // Eat clicks so the overlay is modal, and paint a translucent veil.
            ui.allocate_rect(screen, egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(128));
        });

    egui::Window::new("job_overlay_window")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_width(220.0);
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(label);
                ui.add_space(10.0);
                ui.spinner();
                ui.add_space(6.0);
            });
        });
}
