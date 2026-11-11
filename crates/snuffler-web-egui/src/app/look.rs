//! The app's fonts, text sizes, and how a wiggle theme pins light or dark.

use egui::FontId;
use snuffler_web_core::theme::WiggleTheme;

/// The egui font family the waveform canvas draws every label in: bold, as
/// snuffler's canvas is.
///
/// Registered in [`configure_look`].
pub const CANVAS_FAMILY: &str = "canvas";

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
    // egui's bundled proportional face is Ubuntu-Light, which renders very thin.
    // Register Ubuntu Regular as the primary proportional font (same typeface, one
    // weight up) and keep the built-in fonts as fallbacks for glyph coverage.
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "ubuntu_regular".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/Ubuntu-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(Proportional)
        .or_default()
        .insert(0, "ubuntu_regular".to_owned());
    // egui's bundled monospace face is Hack, which carries no arrows — and the ':'
    // command line spells its history keys as ↑/↓, which would draw as tofu.
    // JetBrains Mono covers the arrows (and ± for marker polarities).
    fonts.font_data.insert(
        "jetbrains_mono".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/JetBrainsMono-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(Monospace)
        .or_default()
        .insert(0, "jetbrains_mono".to_owned());
    // Also the last proportional fallback: Ubuntu has no arrows either, so this is
    // what keeps a ↑ from turning into tofu in any label that happens to use one.
    fonts
        .families
        .entry(Proportional)
        .or_default()
        .push("jetbrains_mono".to_owned());
    // Everything snuffler paints onto its canvas is bold: `TimeAx.drawit` sets it on
    // the axis (pile_viewer.py:543), and the paint pass sets a bold font before it
    // draws anything else (pile_viewer.py:3203, applied at :3229).
    // That is what makes its track labels and tick numbers read at a glance.
    // The canvas asks for this family by name; the chrome around it keeps the
    // regular one.
    fonts.font_data.insert(
        "ubuntu_bold".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/Ubuntu-Bold.ttf"
        ))),
    );
    fonts.families.insert(
        egui::FontFamily::Name(CANVAS_FAMILY.into()),
        vec![
            "ubuntu_bold".to_owned(),
            // Fallbacks for what Ubuntu Bold has no glyph for — the arrows and the
            // per-track ± the regular stack already covers.
            "jetbrains_mono".to_owned(),
            "Ubuntu-Light".to_owned(),
        ],
    );
    ctx.set_fonts(fonts);
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
