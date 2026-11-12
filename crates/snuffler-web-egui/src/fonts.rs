//! Embedded Geist faces and symbol fallbacks for the UI and waveform canvas.

/// The bold font family used to draw and measure waveform labels.
pub(crate) const CANVAS_FAMILY: &str = "canvas";

/// Proportional UI text and regular exported text.
pub(crate) const REGULAR: &[u8] = include_bytes!("../assets/fonts/Geist-Regular.ttf");
/// Waveform labels, axis ticks, and bold exported text.
pub(crate) const BOLD: &[u8] = include_bytes!("../assets/fonts/Geist-Bold.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/GeistMono-Regular.ttf");
const SYMBOLS: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Applies Geist throughout the UI, retaining egui's missing-glyph fallbacks.
pub(crate) fn configure(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, data) in [
        ("geist_regular", REGULAR),
        ("geist_bold", BOLD),
        ("geist_mono", MONO),
        ("jetbrains_fallback", SYMBOLS),
    ] {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(data)),
        );
    }
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "geist_regular".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "geist_mono".to_owned());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("jetbrains_fallback".to_owned());
    }
    fonts.families.insert(
        egui::FontFamily::Name(CANVAS_FAMILY.into()),
        vec![
            "geist_bold".to_owned(),
            "geist_regular".to_owned(),
            "geist_mono".to_owned(),
            "jetbrains_fallback".to_owned(),
        ],
    );
    ctx.set_fonts(fonts);
}
