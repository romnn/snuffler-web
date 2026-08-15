//! Waveform color palettes independent of any frontend toolkit.

use ecolor::Color32;
use snuffler_web_core::state::WiggleTheme;

/// Frontend color-scheme polarity needed by the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// Dark surfaces and light foregrounds.
    Dark,
    /// Light surfaces and dark foregrounds.
    Light,
}

impl Theme {
    /// Maps a frontend-specific dark-mode flag into renderer vocabulary.
    #[must_use]
    pub const fn from_dark(dark: bool) -> Self {
        if dark { Self::Dark } else { Self::Light }
    }
}

/// Colors used by one waveform paint pass.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Canvas background.
    pub bg: Color32,
    /// Default waveform stroke.
    pub trace: Color32,
    /// Time-grid lines.
    pub grid: Color32,
    /// Zero-amplitude reference lines.
    pub zero: Color32,
    /// Axis and tick labels.
    pub axis_text: Color32,
    /// Track separators.
    pub sep: Color32,
    /// Background behind track labels.
    pub label_bg: Color32,
    /// Pointer crosshair.
    pub cursor: Color32,
}

/// The canvas polarity a wiggle theme actually renders on.
///
/// Usually the app's own light/dark preference, but a theme may pin it: Snuffler Qt
/// is white-canvas because snuffler is, and the frontend disables the light/dark
/// control while it is selected rather than letting the two disagree.
#[must_use]
pub fn canvas_theme(preference: Theme, wiggle: WiggleTheme) -> Theme {
    if wiggle.forces_light() {
        Theme::Light
    } else {
        preference
    }
}

/// Selects the waveform palette for the frontend color scheme and user theme.
///
/// The Snuffler Qt theme is snuffler's own surface, and only exists on white — the
/// dark arm is unreachable through the UI (`WiggleTheme::forces_light`) and returns
/// the same white palette rather than inventing one.
///
/// Snuffler Web is one design at two luminance levels, not two designs. Both arms
/// share a slightly blue-cool neutral ramp; each picks the step that puts the same
/// contrast between canvas, chrome and ink. The canvases are held just off the
/// extremes (250 rather than 255, 20 rather than 0) because a waveform against pure
/// white or pure black is harsher than it needs to be for hours of reading.
#[must_use]
pub fn palette(theme: Theme, wiggle: WiggleTheme) -> Palette {
    match (theme, wiggle) {
        (_, WiggleTheme::SnufflerQt) => Palette {
            bg: Color32::from_rgb(255, 255, 255),
            trace: Color32::from_rgb(85, 87, 83),
            grid: Color32::from_rgb(224, 224, 224),
            zero: Color32::from_rgb(236, 236, 236),
            axis_text: Color32::from_rgb(85, 87, 83),
            sep: Color32::from_rgb(214, 214, 214),
            label_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 100),
            cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        },
        (Theme::Light, WiggleTheme::SnufflerWeb) => Palette {
            bg: Color32::from_rgb(250, 250, 251),
            trace: Color32::from_rgb(38, 42, 51),
            grid: Color32::from_rgb(226, 228, 233),
            zero: Color32::from_rgb(235, 237, 241),
            axis_text: Color32::from_rgb(90, 96, 108),
            sep: Color32::from_rgb(223, 226, 232),
            label_bg: Color32::from_rgba_unmultiplied(250, 250, 251, 214),
            cursor: Color32::from_rgba_unmultiplied(38, 42, 51, 46),
        },
        (Theme::Dark, WiggleTheme::SnufflerWeb) => Palette {
            bg: Color32::from_rgb(20, 22, 28),
            trace: Color32::from_rgb(226, 229, 236),
            grid: Color32::from_rgb(46, 50, 60),
            zero: Color32::from_rgb(38, 42, 51),
            axis_text: Color32::from_rgb(150, 157, 172),
            sep: Color32::from_rgb(45, 49, 59),
            label_bg: Color32::from_rgba_unmultiplied(20, 22, 28, 214),
            cursor: Color32::from_rgba_unmultiplied(226, 229, 236, 52),
        },
    }
}
