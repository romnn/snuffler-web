//! Colour measurements the theme and painter tests assert with: WCAG contrast,
//! `OKLab` hue and chroma, and the perceptual distance between two colours.

use ecolor::Color32;

/// A channel's linear-light value, undoing the sRGB transfer curve.
fn linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
pub(crate) fn relative_luminance(c: Color32) -> f32 {
    0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
}

/// WCAG contrast ratio, at least 1.
pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (relative_luminance(a), relative_luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The `OKLab` coordinates of a colour, `(lightness, a, b)`.
fn oklab(color: Color32) -> (f32, f32, f32) {
    let (red, green, blue) = (linear(color.r()), linear(color.g()), linear(color.b()));
    let long = (0.412_221_5 * red + 0.536_332_55 * green + 0.051_445_995 * blue).cbrt();
    let medium = (0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue).cbrt();
    let short = (0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue).cbrt();
    let axis_a = 1.977_998_5 * long - 2.428_592_2 * medium + 0.450_593_7 * short;
    let axis_b = 0.025_904_037 * long + 0.782_771_77 * medium - 0.808_675_77 * short;
    let lightness = 0.210_454_26 * long + 0.793_617_8 * medium - 0.004_072_047 * short;
    (lightness, axis_a, axis_b)
}

/// Hue angle in `OKLab`, in degrees.
/// Channel ordering is not a usable proxy: an olive sits exactly where red and
/// green cross, so two shades of the same hue can order their channels
/// differently.
pub(crate) fn oklab_hue(color: Color32) -> f32 {
    let (_, axis_a, axis_b) = oklab(color);
    axis_b.atan2(axis_a).to_degrees().rem_euclid(360.0)
}

/// Chroma in `OKLab`: zero for a grey, whatever its lightness.
pub(crate) fn oklab_chroma(color: Color32) -> f32 {
    let (_, axis_a, axis_b) = oklab(color);
    axis_a.hypot(axis_b)
}

/// Perceptual distance between two colours: `OKLab` ΔE, scaled by 100 so the
/// numbers read on the familiar scale.
/// About 2 is the smallest difference the eye catches on two large flat areas, and
/// 10 is comfortably "a different colour" at a glance.
pub(crate) fn delta_e(a: Color32, b: Color32) -> f32 {
    let (a_l, a_a, a_b) = oklab(a);
    let (b_l, b_a, b_b) = oklab(b);
    let (d_l, d_a, d_b) = (a_l - b_l, a_a - b_a, a_b - b_b);
    (d_l * d_l + d_a * d_a + d_b * d_b).sqrt() * 100.0
}

/// The distance between the two closest colours in a set.
///
/// This is the number that decides whether a palette reads as N classes: the eye
/// finds the collision, not the average, so a ramp is only as distinguishable as
/// its worst pair — and the pair can be any of them.
/// Comparing neighbours alone is what let index 1 and index 5 of a seven-colour
/// ramp land on the same colour.
pub(crate) fn worst_pair(colors: &[Color32]) -> f32 {
    colors
        .iter()
        .enumerate()
        .flat_map(|(index, a)| colors.iter().skip(index + 1).map(move |b| delta_e(*a, *b)))
        .fold(f32::INFINITY, f32::min)
}

/// The shortest angular distance between two hues, in degrees.
pub(crate) fn hue_drift(a: f32, b: f32) -> f32 {
    let raw = (a - b).abs();
    raw.min(360.0 - raw)
}

/// `fill` composited over `bg` by its alpha, the colour the eye actually sees.
pub(crate) fn over(fill: Color32, bg: Color32) -> Color32 {
    let [r, g, b, a] = fill.to_srgba_unmultiplied();
    // Integer arithmetic, rounded: an 8-bit blend needs no float and no cast.
    let mix = |ink: u8, ground: u8| {
        let blend =
            (u32::from(ink) * u32::from(a) + u32::from(ground) * (255 - u32::from(a)) + 127) / 255;
        u8::try_from(blend).unwrap_or(u8::MAX)
    };
    Color32::from_rgb(mix(r, bg.r()), mix(g, bg.g()), mix(b, bg.b()))
}
