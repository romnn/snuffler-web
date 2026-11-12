//! High-DPI PNG export by rasterizing the view's SVG with resvg.
//!
//! This module is the only place that depends on resvg / usvg / tiny-skia.

use resvg::{tiny_skia, usvg};

/// Rasterize `svg` to PNG bytes at `scale`× its declared point size, giving a crisp high-DPI
/// image.
///
/// `width`/`height` are the SVG viewBox size in points.
/// The embedded Geist faces match the waveform canvas.
/// Returns `None` if parsing, allocation, or encoding fails.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the target pixel size is validated finite and >= 1 before the cast"
)]
pub fn rasterize_svg(svg: &str, width: f32, height: f32, scale: f32) -> Option<Vec<u8>> {
    let mut options = usvg::Options {
        font_family: "Geist".to_owned(),
        ..usvg::Options::default()
    };
    {
        let database = options.fontdb_mut();
        database.load_font_data(crate::fonts::REGULAR.to_vec());
        database.load_font_data(crate::fonts::BOLD.to_vec());
        database.set_sans_serif_family("Geist");
    }
    let tree = usvg::Tree::from_str(svg, &options).ok()?;

    let pixel_width = (width * scale).round();
    let pixel_height = (height * scale).round();
    if !pixel_width.is_finite()
        || !pixel_height.is_finite()
        || pixel_width < 1.0
        || pixel_height < 1.0
    {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(pixel_width as u32, pixel_height as u32)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // The view always paints an opaque background first, so the rendered pixmap is
    // fully opaque and tiny-skia's premultiplied buffer equals straight RGBA here.
    crate::view::draw::encode_png_rgba(pixmap.width(), pixmap.height(), pixmap.data())
}
