//! One module per wiggle theme, each building the [`Palette`] for a canvas polarity.
//!
//! A theme is data.
//! The painter reads the palette and never asks which theme it is; the two
//! snuffler-fidelity rules it does ask about live on
//! `WiggleTheme::mirrors_qt_coloring`.
//! To add a theme, add its variant to `WiggleTheme`, a module here that builds its
//! palette from the shared [`canvas`], a stroke ramp and seven box fills, and an
//! arm in [`crate::palette::palette`].
//! The tests in this module sweep every theme and polarity for the legibility
//! floors, so a new theme is measured the moment it is dispatched.
//!
//! Every theme keeps snuffler's colour meanings and changes only the colours: a box
//! is coloured by the pile's box style, a stroke by its coloring key, a marker by
//! its kind.
//! The empty canvas is never tinted; every web theme draws on the same plain white
//! or dark ground, and what differs is the mood of the seven box colours, the ink
//! that harmonises with them, and how strong the fills are.
//! The web themes draw no box outlines: the fill is the box, and a border on every
//! row is more line than the wiggle it frames.
//!
//! The web colours are designed as pigment families and fitted in `OKLab` by
//! `scripts/fit-wiggle-themes.py`, then reviewed in the running viewer.
//! Candidates bound lightness, chroma and contrast before the fit, keeping a
//! coloured ink from collapsing towards black or becoming a luminous neon.
//! Every measurement uses rounded sRGB bytes, including all 21 pairs.
//! Separation targets saturate; beyond them, the fit favours the family's intended
//! chroma relationships and a quiet, consistent ink weight.
//! Light fills stay below 1.9:1 against white and dark fills below 2.2:1,
//! preserving the monochrome ink above every block.
//! Dark coloured strokes stay below 12.5:1 to avoid luminous accents.
//!
//! The stroke ramps are *ordered* as well as fitted, because data has two or
//! three coloring keys far more often than seven and the first entries are the ones
//! a reader compares.
//! Snuffler's own ramp is built that way — red, blue, green, 30.9 apart over its
//! first three and 9.2 over all seven — so the ramps here put their furthest colours
//! first instead of spreading the seven evenly.
//! Light and dark are fitted together against whichever separates worse, since a key
//! keeps one index across a theme switch and so the two canvases share an ordering.
//! The first three hues are designed as distinct lead pigments, with four supporting
//! colours completing the family rather than an evenly spaced colour wheel.

pub mod afterglow;
pub mod daybreak;
pub mod fieldbook;
pub mod fjord;
pub mod herbarium;
pub mod kiln;
pub mod snuffler_qt;

use ecolor::Color32;
use snuffler_web_core::markers::MarkerPalette;
use snuffler_web_core::theme::Theme;

use crate::palette::{BoxStyle, Palette};

/// A colour as the `(r, g, b)` triple it was fitted to, so a theme's tables read as
/// the hex they were designed in.
pub(crate) type Rgb = (u8, u8, u8);

/// An opaque [`Color32`] from a triple.
pub(crate) const fn rgb((red, green, blue): Rgb) -> Color32 {
    Color32::from_rgb(red, green, blue)
}

/// Seven bare box fills, the web themes' only box element.
pub(crate) fn boxes(fills: [Rgb; 7]) -> [BoxStyle; 7] {
    fills.map(|fill| BoxStyle {
        fill: rgb(fill),
        frame: None,
    })
}

/// The plain canvas every web theme draws on, for one polarity.
///
/// The ground is never tinted by a theme; a theme may replace the ink with its
/// palette's own ([`Self::with_ink`]) and supplies everything coloured.
pub(crate) struct WebCanvas {
    theme: Theme,
    background: Rgb,
    trace: Rgb,
    grid: Rgb,
    zero: Rgb,
    axis_text: Rgb,
    separator: Rgb,
}

/// The shared canvas for a polarity: white or near-black, with neutral chrome held
/// at the same steps in both (grid about 1.25:1, zero line and separator about
/// 1.5-1.9:1, axis text 8:1 and up).
pub(crate) const fn canvas(theme: Theme) -> WebCanvas {
    match theme {
        Theme::Light => WebCanvas {
            theme,
            background: (255, 255, 255),
            trace: (24, 26, 32),
            grid: (228, 230, 235),
            zero: (196, 200, 208),
            axis_text: (52, 56, 66),
            separator: (206, 210, 218),
        },
        Theme::Dark => WebCanvas {
            theme,
            background: (18, 19, 24),
            trace: (232, 234, 240),
            grid: (40, 43, 52),
            zero: (62, 66, 78),
            axis_text: (170, 176, 190),
            separator: (52, 56, 66),
        },
    }
}

impl WebCanvas {
    /// The same canvas with the theme's own ink for the monochrome wiggle.
    pub(crate) const fn with_ink(self, ink: Rgb) -> Self {
        Self { trace: ink, ..self }
    }

    /// Assembles a palette from this canvas, a stroke ramp and box styles.
    ///
    /// The rest is what every web theme shares: a rounded, padded track label plate
    /// in the canvas colour, a cursor cut from the ink at low alpha, the marker
    /// shades fitted for this polarity, and a marker label plate in the canvas
    /// colour rather than Qt's white.
    pub(crate) fn palette(self, ramp: [Rgb; 7], boxes: [BoxStyle; 7]) -> Palette {
        let (markers, cursor_alpha) = match self.theme {
            Theme::Light => (MarkerPalette::SNUFFLER_WEB_LIGHT, 46),
            Theme::Dark => (MarkerPalette::SNUFFLER_WEB_DARK, 52),
        };
        let (bg_r, bg_g, bg_b) = self.background;
        let (ink_r, ink_g, ink_b) = self.trace;
        Palette {
            theme: self.theme,
            background: rgb(self.background),
            trace: rgb(self.trace),
            grid: rgb(self.grid),
            zero: rgb(self.zero),
            axis_text: rgb(self.axis_text),
            separator: rgb(self.separator),
            label_background: Color32::from_rgba_unmultiplied(bg_r, bg_g, bg_b, 214),
            label_padding: 3.0,
            label_rounding: 3.0,
            cursor: Color32::from_rgba_unmultiplied(ink_r, ink_g, ink_b, cursor_alpha),
            strokes: ramp.map(rgb),
            boxes,
            markers,
            marker_label_background: rgb(self.background),
        }
    }
}

#[cfg(test)]
mod tests {
    use ecolor::Color32;
    use snuffler_web_core::markers::MarkerPalette;
    use snuffler_web_core::theme::{StrokePolicy, Theme, WiggleTheme};

    use crate::palette::{Palette, palette};
    use crate::test_color::{
        contrast, hue_drift, oklab_chroma, oklab_hue, over, relative_luminance, worst_pair,
    };

    /// Every web theme on every canvas it renders on, each once.
    fn web_palettes() -> Vec<(WiggleTheme, Theme, Palette)> {
        WiggleTheme::all()
            .filter(|wiggle| !wiggle.mirrors_qt_coloring())
            .flat_map(|wiggle| {
                let polarities = wiggle
                    .pinned_polarity()
                    .map_or(Theme::ALL.to_vec(), |one| vec![one]);
                polarities
                    .into_iter()
                    .map(move |theme| (wiggle, theme, palette(theme, wiggle)))
            })
            .collect()
    }

    #[test]
    fn every_web_theme_has_both_canvases() {
        // A theme that only works on one canvas is not a theme of this frontend;
        // only the Qt theme, which reproduces a white-only application, may pin.
        for wiggle in WiggleTheme::all() {
            assert_eq!(
                wiggle.pinned_polarity().is_some(),
                wiggle.mirrors_qt_coloring(),
                "{wiggle:?} pins its canvas"
            );
        }
    }

    #[test]
    fn every_theme_keeps_its_ink_and_boxes_legible() {
        // The monochrome wiggle is what most data is read in, so it clears 7:1 on
        // every canvas.
        // Snuffler's aluminium5 sits at 7.3 and the web inks sit with it: a grey
        // wiggle is what snuffler's light canvas reads like.
        // The ink and the boxes are coupled, since the wiggle has to read over
        // whatever it crosses, and snuffler resolves that by keeping its boxes pale:
        // 1.10-1.84, with its pen clearing 3.98 over the scarletred one.
        // The 3:1 graphical-object floor leaves room for stronger blocks than
        // snuffler's own 3.98 permits; a theme may choose softer fills for more
        // comfortable ink without changing that acceptance floor.
        // The dark canvas has no such coupling — its ink is light on a dark ground
        // and has the whole range to itself — so its blocks reach 2.6.
        for wiggle in WiggleTheme::all() {
            for theme in Theme::ALL {
                let pal = palette(theme, wiggle);
                let ink = contrast(pal.trace, pal.background);
                assert!(ink >= 7.0, "{wiggle:?}/{theme:?}: ink is {ink:.2}");
                for (index, style) in pal.boxes.iter().enumerate() {
                    let fill = over(style.fill, pal.background);
                    let tint = contrast(fill, pal.background);
                    assert!(
                        (1.08..2.6).contains(&tint),
                        "{wiggle:?}/{theme:?}: box {index} fill is {tint:.2}"
                    );
                    let through = contrast(pal.trace, fill);
                    assert!(
                        through >= 3.0,
                        "{wiggle:?}/{theme:?}: ink over box {index} is {through:.2}"
                    );
                    // The web themes draw bare fills; only snuffler's boxes carry an
                    // outline, and it has to stand off the fill.
                    match style.frame {
                        Some(frame) => {
                            assert!(
                                wiggle.mirrors_qt_coloring(),
                                "{wiggle:?} outlines its boxes"
                            );
                            let edge = contrast(frame, fill);
                            assert!(edge >= 1.4, "{wiggle:?}: box {index} frame is {edge:.2}");
                        }
                        None => assert!(
                            !wiggle.mirrors_qt_coloring(),
                            "the Qt theme lost its outlines"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn web_strokes_read_on_their_canvas_and_over_every_box() {
        // 4.5:1 is WCAG's body-text floor, held for a one-pixel line because the
        // line is mostly antialiased blend: at the 3:1 floor for graphical objects a
        // stroke reads as a pale thread.
        // Boxes are keyed on the pile's style and strokes on the coloring key, so any
        // stroke can cross any filled box; a theme that colours strokes only on
        // grouped rows may let them sit at the graphics floor over a colour block.
        for (wiggle, theme, pal) in web_palettes() {
            let over_fill_floor = match wiggle.stroke_policy() {
                StrokePolicy::Colored => 3.5,
                StrokePolicy::Monochrome | StrokePolicy::ColoredWhenBoxesCannotSeparate => 3.0,
            };
            for (index, stroke) in pal.strokes.iter().enumerate() {
                let chroma = oklab_chroma(*stroke);
                assert!(
                    chroma >= 0.06,
                    "{wiggle:?}/{theme:?}: stroke {index} lost its colour: {chroma:.3}"
                );
                let ink = contrast(*stroke, pal.background);
                assert!(
                    ink >= 4.5,
                    "{wiggle:?}/{theme:?}: stroke {index} is {ink:.2}"
                );
                // Ink is darker than a light canvas and lighter than a dark one.
                let darker = relative_luminance(*stroke) < relative_luminance(pal.background);
                assert_eq!(
                    darker,
                    theme == Theme::Light,
                    "{wiggle:?}/{theme:?}: stroke {index} is not ink"
                );
                for style in &pal.boxes {
                    let through = contrast(*stroke, over(style.fill, pal.background));
                    assert!(
                        through >= over_fill_floor,
                        "{wiggle:?}/{theme:?}: stroke {index} over a box is {through:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_web_theme_keeps_its_seven_classes_apart() {
        // A box colour and a stroke colour are both class identities: "these two
        // rows are the same colour" is a statement about the data, so two classes
        // that land on one colour are a defect, not a blemish.
        // The measure is the closest of all 21 pairs, because that is the one a
        // reader trips over; about 2 is the smallest difference the eye catches at
        // all, so these floors sit several times above merely-visible.
        // The floors leave the muted themes room, since a wash trades some
        // separation for its lightness, while still catching class collisions.
        // Strokes carry more of the load than fills: they are thin, and under
        // `ColoredWhenBoxesCannotSeparate` they are the only thing left once the
        // boxes are hidden.
        for (wiggle, theme, pal) in web_palettes() {
            let fills: Vec<Color32> = pal
                .boxes
                .iter()
                .map(|style| over(style.fill, pal.background))
                .collect();
            let boxes = worst_pair(&fills);
            assert!(
                boxes >= 6.0,
                "{wiggle:?}/{theme:?}: two boxes are {boxes:.1} apart"
            );
            let strokes = worst_pair(&pal.strokes);
            assert!(
                strokes >= 10.0,
                "{wiggle:?}/{theme:?}: two strokes are {strokes:.1} apart"
            );
        }
    }

    #[test]
    fn a_theme_puts_its_furthest_colours_first() {
        // Real data has two or three coloring keys far more often than seven, so the
        // first entries of the ramp are the ones a reader actually compares.
        // Snuffler's own is built that way — red, then blue, then green: 30.9 apart
        // over its first three, falling to 9.2 over all seven — while a ramp fitted
        // only for its worst pair spreads the seven evenly and lands near 14
        // everywhere, which is worse exactly where it is read most.
        // These floors sit below snuffler's because the ramps also hold a chroma
        // floor: the fit will otherwise park an entry at the bottom of the lightness
        // band, where chroma collapses and a "coloured" trace comes out black.
        for (wiggle, theme, pal) in web_palettes() {
            let prefix = |count: usize| pal.strokes.get(..count).map_or(f32::INFINITY, worst_pair);
            let two = prefix(2);
            assert!(
                two >= 22.0,
                "{wiggle:?}/{theme:?}: first two keys are {two:.1} apart"
            );
            let three = prefix(3);
            assert!(
                three >= 18.0,
                "{wiggle:?}/{theme:?}: first three keys are {three:.1} apart"
            );
        }
    }

    #[test]
    fn web_canvas_chrome_keeps_the_same_steps_on_every_theme() {
        // The zero line is a data reference and reads at every step; the separator
        // and grid are decoration and only have to be there; axis text is read.
        for (wiggle, theme, pal) in web_palettes() {
            let zero = contrast(pal.zero, pal.background);
            let separator = contrast(pal.separator, pal.background);
            let grid = contrast(pal.grid, pal.background);
            let axis = contrast(pal.axis_text, pal.background);
            assert!(zero >= 1.3, "{wiggle:?}/{theme:?}: zero line is {zero:.2}");
            assert!(
                separator >= 1.2,
                "{wiggle:?}/{theme:?}: separator is {separator:.2}"
            );
            assert!(grid >= 1.1, "{wiggle:?}/{theme:?}: grid is {grid:.2}");
            assert!(axis >= 4.5, "{wiggle:?}/{theme:?}: axis text is {axis:.2}");
        }
    }

    #[test]
    fn a_stroke_index_keeps_its_hue_across_the_canvases() {
        // Light and dark have their own ink weights, but key N stays the same colour
        // family in both, so a coloring key keeps its identity across a switch.
        for wiggle in WiggleTheme::all() {
            if wiggle.pinned_polarity().is_some() {
                continue;
            }
            let light = palette(Theme::Light, wiggle);
            let dark = palette(Theme::Dark, wiggle);
            for (index, (l, d)) in light.strokes.iter().zip(&dark.strokes).enumerate() {
                let drift = hue_drift(oklab_hue(*l), oklab_hue(*d));
                assert!(
                    drift < 15.0,
                    "{wiggle:?}: hue {index} drifted {drift:.1} deg between canvases"
                );
            }
        }
    }

    #[test]
    fn marker_kinds_keep_their_family_on_every_theme() {
        // "Kind 2 is blue" is a fact snuffler users carry between the two apps, so
        // each web palette's kind has to sit on the Tango family's hue — measured
        // in OKLab against the Qt idle shade — in the idle and the lit shade alike.
        // Kind 7 is grey, which has no hue to keep; it stays neutral instead.
        let qt = MarkerPalette::SNUFFLER_QT;
        for (wiggle, theme, pal) in web_palettes() {
            for kind in 0..7u8 {
                let family = oklab_hue(qt.kind(kind, false));
                for lit in [false, true] {
                    let drift = hue_drift(oklab_hue(pal.markers.kind(kind, lit)), family);
                    assert!(
                        drift < 12.0,
                        "{wiggle:?}/{theme:?}: kind {kind} (lit {lit}) drifted {drift:.1} deg"
                    );
                }
            }
            for lit in [false, true] {
                // The greys carry the canvas ramp's slight cool cast, which is well
                // under the chroma of a tinted colour.
                let chroma = oklab_chroma(pal.markers.kind(7, lit));
                assert!(
                    chroma < 0.03,
                    "{wiggle:?}/{theme:?}: kind 7 is not grey: {chroma:.3}"
                );
            }
        }
    }

    #[test]
    fn marker_kinds_stay_eight_distinguishable_colours() {
        // A kind is read off its colour, so the palette is only as good as its
        // closest pair; snuffler's own shades manage 10.7.
        // Both the idle and the lit set are palettes in their own right, since
        // several markers can be selected at once.
        for (wiggle, theme, pal) in web_palettes() {
            for lit in [false, true] {
                let shades: Vec<Color32> =
                    (0..8u8).map(|kind| pal.markers.kind(kind, lit)).collect();
                let worst = worst_pair(&shades);
                assert!(
                    worst >= 11.0,
                    "{wiggle:?}/{theme:?}: two marker kinds (lit {lit}) are {worst:.1} apart"
                );
            }
        }
    }

    #[test]
    fn marker_shades_read_on_their_canvas_and_lift_when_lit() {
        // An idle marker clears the 3:1 floor for a graphical object on its own
        // canvas, and selection reads as a brightening against the kind's own idle
        // shade rather than against the ground.
        // On white those two pull apart: lighter is less contrast there, so a lit
        // shade sits *below* its idle one — as snuffler's do, its butter2 reaching
        // only 1.5 against the paper.
        for (wiggle, theme, pal) in web_palettes() {
            let (idle_floor, lit_floor) = match theme {
                Theme::Light => (3.0, 2.2),
                Theme::Dark => (4.3, 4.5),
            };
            for kind in 0..8u8 {
                let idle = pal.markers.kind(kind, false);
                let lit = pal.markers.kind(kind, true);
                let idle_contrast = contrast(idle, pal.background);
                let lit_contrast = contrast(lit, pal.background);
                assert!(
                    idle_contrast >= idle_floor,
                    "{wiggle:?}/{theme:?}: idle kind {kind} is {idle_contrast:.2}"
                );
                assert!(
                    lit_contrast >= lit_floor,
                    "{wiggle:?}/{theme:?}: lit kind {kind} is {lit_contrast:.2}"
                );
                assert!(
                    relative_luminance(lit) > relative_luminance(idle),
                    "{wiggle:?}/{theme:?}: lit kind {kind} is not lighter than idle"
                );
            }
        }
    }

    #[test]
    fn qt_theme_is_snufflers_own_numbers() {
        // The theme that claims fidelity is pinned to pyrocko's constants: the
        // graph_colors ramp, the Tango "1" fills at box_alpha 100 under Tango "3"
        // frames in snuffler's box_styles order, and aluminium5 for the pen — on
        // white whichever polarity is asked for, since the theme pins its canvas.
        let pal = palette(Theme::Dark, WiggleTheme::SnufflerQt);
        assert_eq!(pal.background, Color32::WHITE);
        assert_eq!(pal.trace, Color32::from_rgb(85, 87, 83));
        assert_eq!(pal.strokes.first(), Some(&Color32::from_rgb(204, 0, 0)));
        assert_eq!(pal.strokes.last(), Some(&Color32::from_rgb(237, 212, 0)));
        let first = pal.box_style(0);
        assert_eq!(
            first.fill,
            Color32::from_rgba_unmultiplied(252, 175, 62, 100)
        );
        assert_eq!(first.frame, Some(Color32::from_rgb(206, 92, 0)));
        // Style indices wrap the way `box_styles[istyle % len(box_styles)]` does.
        assert_eq!(pal.box_style(7), first);
    }
}
