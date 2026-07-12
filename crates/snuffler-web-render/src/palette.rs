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

/// Selects the waveform palette for the frontend color scheme and user theme.
#[must_use]
pub fn palette(theme: Theme, wiggle: WiggleTheme) -> Palette {
    match (theme, wiggle) {
        (Theme::Dark, WiggleTheme::SnufflerQt) => Palette {
            bg: Color32::from_rgb(12, 15, 18),
            trace: Color32::from_rgb(206, 212, 218),
            grid: Color32::from_rgb(42, 49, 56),
            zero: Color32::from_rgb(32, 37, 43),
            axis_text: Color32::from_rgb(188, 196, 204),
            sep: Color32::from_rgb(48, 55, 62),
            label_bg: Color32::from_rgba_unmultiplied(12, 15, 18, 218),
            cursor: Color32::from_rgba_unmultiplied(255, 255, 255, 55),
        },
        (Theme::Light, WiggleTheme::SnufflerQt) => Palette {
            bg: Color32::from_rgb(255, 255, 255),
            trace: Color32::from_rgb(85, 87, 83),
            grid: Color32::from_rgb(224, 224, 224),
            zero: Color32::from_rgb(236, 236, 236),
            axis_text: Color32::from_rgb(85, 87, 83),
            sep: Color32::from_rgb(214, 214, 214),
            label_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 100),
            cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        },
        (Theme::Dark, WiggleTheme::ColorLines) => Palette {
            bg: Color32::from_rgb(22, 24, 27),
            trace: Color32::from_rgb(222, 224, 228),
            grid: Color32::from_rgb(48, 51, 56),
            zero: Color32::from_rgb(40, 43, 48),
            axis_text: Color32::from_rgb(196, 200, 206),
            sep: Color32::from_rgb(54, 57, 62),
            label_bg: Color32::from_rgba_unmultiplied(16, 18, 20, 205),
            cursor: Color32::from_rgba_unmultiplied(255, 255, 255, 50),
        },
        (Theme::Light, WiggleTheme::ColorLines) => Palette {
            bg: Color32::from_rgb(250, 250, 250),
            trace: Color32::from_rgb(45, 48, 52),
            grid: Color32::from_rgb(224, 224, 224),
            zero: Color32::from_rgb(236, 236, 236),
            axis_text: Color32::from_rgb(64, 64, 64),
            sep: Color32::from_rgb(214, 214, 214),
            label_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 216),
            cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        },
        (Theme::Dark, WiggleTheme::NightBands) => Palette {
            bg: Color32::from_rgb(6, 12, 16),
            trace: Color32::from_rgb(213, 230, 226),
            grid: Color32::from_rgb(28, 45, 52),
            zero: Color32::from_rgb(21, 34, 40),
            axis_text: Color32::from_rgb(176, 206, 207),
            sep: Color32::from_rgb(35, 56, 64),
            label_bg: Color32::from_rgba_unmultiplied(6, 12, 16, 225),
            cursor: Color32::from_rgba_unmultiplied(137, 230, 211, 72),
        },
        (Theme::Light, WiggleTheme::NightBands) => Palette {
            bg: Color32::from_rgb(248, 250, 247),
            trace: Color32::from_rgb(39, 58, 62),
            grid: Color32::from_rgb(216, 225, 219),
            zero: Color32::from_rgb(230, 236, 231),
            axis_text: Color32::from_rgb(52, 76, 78),
            sep: Color32::from_rgb(202, 215, 207),
            label_bg: Color32::from_rgba_unmultiplied(248, 250, 247, 220),
            cursor: Color32::from_rgba_unmultiplied(0, 76, 72, 44),
        },
    }
}
