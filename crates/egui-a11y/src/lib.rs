//! Web accessibility for egui, built on the [`accesskit_dom`] projection.
//!
//! egui builds a complete AccessKit tree once [`egui::Context::enable_accesskit`] is called,
//! but on the web nothing consumes it.
//! This crate carries it both ways:
//!
//! - [`install`] builds an [`accesskit_dom::Adapter`] that mirrors each pass's tree into DOM
//!   proxies over the canvas.
//! - Clicks, focus moves and key presses on the proxies are queued as [`egui::Event`]s and
//!   handed to the next pass by [`A11y::drain`], called from the app's `raw_input_hook`.
//!
//! The hook delivering egui's per-pass [`TreeUpdate`](egui::accesskit::TreeUpdate)s belongs
//! to the web runner, so the app connects it itself:
//!
//! ```ignore
//! # // `ignore`: `install` and the eframe hook only exist on wasm32, so this
//! # // cannot compile as a host doctest; it is exercised end to end by the
//! # // browser suite of the app embedding it.
//! let a11y = egui_a11y::install(&ctx, &canvas, accesskit_dom::Options::default())?;
//! eframe::web::accesskit::set_update_handler({
//!     let a11y = a11y.clone();
//!     move |update| a11y.on_tree_update(update)
//! });
//! ```
//!
//! [`announce_changes`] and [`publish_selected`] work on every platform: they only annotate
//! egui's own tree.

pub use accesskit_dom;

pub mod keys;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
pub use web::{A11y, install};

/// How insistently a live region interrupts what a screen reader is saying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Announced once the user is idle. Right for status and progress.
    Polite,
    /// Interrupts whatever is being read. Reserve it for errors.
    Urgent,
}

/// Marks a widget as a live region, so changes to its text are announced.
///
/// egui never sets this itself, since only the app knows which labels are worth an
/// interruption.
/// A no-op while AccessKit is off.
pub fn announce_changes(ctx: &egui::Context, response: &egui::Response, urgency: Urgency) {
    let live = match urgency {
        Urgency::Polite => egui::accesskit::Live::Polite,
        Urgency::Urgent => egui::accesskit::Live::Assertive,
    };
    ctx.accesskit_node_builder(response.id, |node| node.set_live(live))
        .unwrap_or_default();
}

/// Marks a widget as selected or not, as a row of a selection list is.
///
/// egui gives `selectable_label` a toggled state, which surfaces as `aria-checked` and suits
/// a toggle button but not a list row.
/// A no-op while AccessKit is off.
pub fn publish_selected(ctx: &egui::Context, response: &egui::Response, selected: bool) {
    ctx.accesskit_node_builder(response.id, |node| node.set_selected(selected))
        .unwrap_or_default();
}
