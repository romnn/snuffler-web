//! The app's fonts, text sizes, and how a wiggle theme pins light or dark.

use egui::FontId;
use snuffler_web_core::theme::WiggleTheme;

/// Apply a wiggle theme's claim on the frontend's light/dark preference, and
/// return the user's own preference to hold on to for as long as it is overruled.
///
/// A single-canvas theme takes the *whole* frontend with it, not just the
/// waveform: a white canvas inside dark chrome is neither theme, it just looks
/// broken.
/// The choice it overrules is not discarded — it comes back the moment a theme with
/// both canvases is selected, and it is what gets persisted meanwhile.
pub(super) fn pin_theme(
    ctx: &egui::Context,
    theme: WiggleTheme,
    chosen: egui::ThemePreference,
) -> Option<egui::ThemePreference> {
    if let Some(polarity) = theme.pinned_polarity() {
        let pinned = match polarity {
            snuffler_web_core::theme::Theme::Light => egui::ThemePreference::Light,
            snuffler_web_core::theme::Theme::Dark => egui::ThemePreference::Dark,
        };
        ctx.set_theme(pinned);
        (chosen != pinned).then_some(chosen)
    } else {
        ctx.set_theme(chosen);
        None
    }
}

/// Installs the fonts, the chrome colours, and the text sizes.
pub(super) fn configure_look(ctx: &egui::Context) {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle;
    crate::fonts::configure(ctx);
    crate::chrome::install(ctx);

    // Both styles, not the active one: egui keeps a `Style` per theme, and
    // `set_global_style` writes only whichever is current, so the sizes below would
    // vanish on the first light/dark switch.
    ctx.all_styles_mut(|style| {
        // Some 18 % over egui's defaults, which are tuned for dense tool windows
        // rather than for an app somebody reads for hours.
        // The canvas is *not* scaled with this: its labels are held to snuffler's own
        // metrics (`LABEL_PT`, `AXIS_PT` in the render crate).
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, Proportional)),
            (TextStyle::Body, FontId::new(17.0, Proportional)),
            (TextStyle::Monospace, FontId::new(16.0, Monospace)),
            (TextStyle::Button, FontId::new(17.0, Proportional)),
            (TextStyle::Small, FontId::new(14.5, Proportional)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
}
