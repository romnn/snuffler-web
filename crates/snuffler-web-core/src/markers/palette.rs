//! The colours a theme gives the eight marker kinds.

use ecolor::Color32;

/// The colours a theme gives the eight marker kinds.
///
/// Snuffler colours a marker by its `kind` alone, and a marker reads as selected
/// or hovered by changing *shade* within its kind's hue, never by changing hue
/// (`select_color`, marker.py:306).
/// Every palette keeps that contract: index `kind` is the same colour family on
/// every theme (0 red, 1 green, 2 blue, 3 orange, 4 purple, 5 brown, 6 yellow,
/// 7 grey), so a kind-2 pick is blue whatever the canvas, and only the shades
/// move to suit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkerPalette {
    /// What a marker wears when it is neither selected nor hovered.
    pub idle: [Color32; 8],
    /// The shade a selected or hovered marker lifts to.
    pub lit: [Color32; 8],
}

impl MarkerPalette {
    /// Snuffler's own palette (`g_color_b`, marker.py:53): the Tango "3" shades
    /// idle (`colorlist[kind*3 + 2]`) and the "2" shades lit
    /// (`colorlist[kind*3 + 1]`).
    ///
    /// Built for snuffler's white canvas and only coherent on it: skyblue3 and
    /// plum3 sit below 2:1 against a dark one.
    pub const SNUFFLER_QT: Self = Self {
        idle: [
            Color32::from_rgb(164, 0, 0),   // 0 scarletred3
            Color32::from_rgb(78, 154, 6),  // 1 chameleon3
            Color32::from_rgb(32, 74, 135), // 2 skyblue3
            Color32::from_rgb(206, 92, 0),  // 3 orange3
            Color32::from_rgb(92, 53, 102), // 4 plum3
            Color32::from_rgb(143, 89, 2),  // 5 chocolate3
            Color32::from_rgb(196, 160, 0), // 6 butter3
            Color32::from_rgb(85, 87, 83),  // 7 aluminium5
        ],
        lit: [
            Color32::from_rgb(204, 0, 0),     // 0 scarletred2
            Color32::from_rgb(115, 210, 22),  // 1 chameleon2
            Color32::from_rgb(52, 101, 164),  // 2 skyblue2
            Color32::from_rgb(245, 121, 0),   // 3 orange2
            Color32::from_rgb(117, 80, 123),  // 4 plum2
            Color32::from_rgb(193, 125, 17),  // 5 chocolate2
            Color32::from_rgb(237, 212, 0),   // 6 butter2
            Color32::from_rgb(136, 138, 133), // 7 aluminium4
        ],
    };

    /// The Snuffler Web palette for its light canvas.
    ///
    /// Each kind is fitted in `OKLab` at the hue angle of its Tango family, so a
    /// kind keeps the colour it has in snuffler; its lightness and chroma are what
    /// the fit chooses.
    /// What the fit maximises is the closest of the 28 pairs, because that is what
    /// decides whether a kind can be read off its colour at all: it holds 14.4 here,
    /// where snuffler's own shades manage 10.7.
    /// Idle shades hold 3.1-11.3:1 against the paper and lit ones 2.3-7.3:1.
    /// A lit shade may sit lower than the idle one it replaces because on white
    /// lighter *is* less contrast; selection reads as a brightening of 1.35-1.57
    /// against the kind's own idle shade, which is the signal snuffler gives too
    /// (its butter2 sits at 1.5 against the paper).
    pub const SNUFFLER_WEB_LIGHT: Self = Self {
        idle: [
            Color32::from_rgb(0xD3, 0x3E, 0x31), // 0 red
            Color32::from_rgb(0x2D, 0x5A, 0x0B), // 1 green
            Color32::from_rgb(0x27, 0x66, 0xBE), // 2 blue
            Color32::from_rgb(0x64, 0x29, 0x00), // 3 orange
            Color32::from_rgb(0xBB, 0x65, 0xD1), // 4 purple
            Color32::from_rgb(0xCD, 0x81, 0x00), // 5 brown
            Color32::from_rgb(0x85, 0x6B, 0x00), // 6 yellow
            Color32::from_rgb(0x8B, 0x8B, 0x8B), // 7 grey
        ],
        lit: [
            Color32::from_rgb(0xFA, 0x4F, 0x3F),
            Color32::from_rgb(0x37, 0x72, 0x00),
            Color32::from_rgb(0x48, 0x87, 0xE3),
            Color32::from_rgb(0x91, 0x3E, 0x00),
            Color32::from_rgb(0xD9, 0x7A, 0xF1),
            Color32::from_rgb(0xEF, 0x97, 0x00),
            Color32::from_rgb(0x9E, 0x80, 0x00),
            Color32::from_rgb(0xA1, 0xA4, 0x9E),
        ],
    };

    /// The Snuffler Web palette for its dark canvas: the same families, lifted.
    ///
    /// Idle shades hold 4.3-12.5:1 against the dark canvas and lit ones 5.9-15.4:1,
    /// and the closest of the 28 pairs is 16.2.
    /// Selection brightens by 1.22-1.49: on a dark ground "lighter" soon runs into
    /// white, where every kind ends up the same pale tint, so the lit shades lift
    /// less than on paper and keep their chroma instead.
    pub const SNUFFLER_WEB_DARK: Self = Self {
        idle: [
            Color32::from_rgb(0xFF, 0x84, 0x74), // 0 red
            Color32::from_rgb(0x88, 0xDC, 0x53), // 1 green
            Color32::from_rgb(0x61, 0x96, 0xE6), // 2 blue
            Color32::from_rgb(0xC8, 0x59, 0x00), // 3 orange
            Color32::from_rgb(0xE3, 0x9E, 0xF5), // 4 purple
            Color32::from_rgb(0xFF, 0xCB, 0x91), // 5 brown
            Color32::from_rgb(0xC1, 0x9E, 0x00), // 6 yellow
            Color32::from_rgb(0x79, 0x7C, 0x77), // 7 grey
        ],
        lit: [
            Color32::from_rgb(0xFF, 0xA1, 0x92),
            Color32::from_rgb(0xB0, 0xF6, 0x8A),
            Color32::from_rgb(0x7D, 0xB1, 0xFF),
            Color32::from_rgb(0xE5, 0x6F, 0x24),
            Color32::from_rgb(0xF2, 0xBE, 0xFF),
            Color32::from_rgb(0xFF, 0xE6, 0xCA),
            Color32::from_rgb(0xE4, 0xBA, 0x00),
            Color32::from_rgb(0x97, 0x9A, 0x95),
        ],
    };

    /// The colour of a marker kind, idle or lit.
    ///
    /// `kind & 7` always lands inside the palette, so every kind has a colour and
    /// the fallback is unreachable.
    #[must_use]
    pub fn kind(&self, kind: u8, lit: bool) -> Color32 {
        let table = if lit { &self.lit } else { &self.idle };
        table
            .get(usize::from(kind & 7))
            .copied()
            .unwrap_or(Color32::from_rgb(204, 0, 0))
    }
}
