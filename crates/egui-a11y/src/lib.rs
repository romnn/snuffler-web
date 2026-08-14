//! Web accessibility for egui, built on the [`accesskit_dom`] projection.
//!
//! egui builds a complete AccessKit tree — role, label, value, bounds and
//! parent/child links for every widget — on any platform once
//! [`egui::Context::enable_accesskit`] is called. On the web that tree normally goes
//! nowhere: there is no native accessibility API to hand it to, and eframe's web
//! backend drops it. This crate carries it the last mile in both directions:
//!
//! - [`install`] builds an [`accesskit_dom::Adapter`] that mirrors each pass's tree
//!   into transparent DOM proxies over the canvas, so screen readers and browser
//!   automation see real controls instead of one opaque canvas.
//! - Input comes back the other way: clicks, focus moves and key presses on the
//!   proxies are queued as [`egui::Event`]s and handed to the next pass by
//!   [`A11y::drain`], called from the app's `raw_input_hook`.
//!
//! The one piece that cannot live here is the hook delivering egui's per-pass
//! [`TreeUpdate`](egui::accesskit::TreeUpdate)s out of the web runner: that is the
//! runner's API, not egui's. The app connects it with one line — against a patched
//! eframe today, against whatever hook lands upstream tomorrow:
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
//! [`announce_changes`] works on every platform: it only annotates egui's own tree,
//! and native AccessKit adapters honour live regions just as the DOM projection does.

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

/// Mark a widget as a live region, so changes to its text are announced without
/// the user having to go looking for them.
///
/// egui never sets this itself, and it cannot: which of an app's labels are worth
/// interrupting someone for is a judgement only the app can make. A no-op while
/// AccessKit is off, since egui then builds no nodes to annotate.
pub fn announce_changes(ctx: &egui::Context, response: &egui::Response, urgency: Urgency) {
    let live = match urgency {
        Urgency::Polite => egui::accesskit::Live::Polite,
        Urgency::Urgent => egui::accesskit::Live::Assertive,
    };
    ctx.accesskit_node_builder(response.id, |node| node.set_live(live))
        .unwrap_or_default();
}

/// Mark a widget as selected or not, so its state can be read off the row itself
/// rather than inferred from a count somewhere else.
///
/// egui gives `selectable_label` a *toggled* state, which surfaces as
/// `aria-checked` — right for a toggle button, wrong for a row of a selection list,
/// where the question is which rows are picked. AccessKit distinguishes the two and
/// so does ARIA, but only the app knows which it means. A no-op while AccessKit is
/// off, since egui then builds no nodes to annotate.
pub fn publish_selected(ctx: &egui::Context, response: &egui::Response, selected: bool) {
    ctx.accesskit_node_builder(response.id, |node| node.set_selected(selected))
        .unwrap_or_default();
}
