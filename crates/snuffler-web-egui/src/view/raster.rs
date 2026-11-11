//! High-DPI PNG export by rasterizing the view's SVG with resvg.
//!
//! This module is the only place that depends on resvg / usvg / tiny-skia.

use resvg::{tiny_skia, usvg};

/// Embedded font so SVG `<text>` renders the same as the on-screen view — resvg
/// has no system fonts to fall back on under wasm.
const UBUNTU_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Ubuntu-Regular.ttf");
/// The canvas is drawn bold, so the exported PNG needs the bold face too — resvg
/// resolves `font-weight: bold` against this database and would otherwise fall back
/// to the regular one, exporting an image the app never showed.
const UBUNTU_BOLD: &[u8] = include_bytes!("../../assets/fonts/Ubuntu-Bold.ttf");

/// Rasterize `svg` to PNG bytes at `scale`× its declared point size, giving a crisp high-DPI
/// image.
///
/// `width`/`height` are the SVG viewBox size in points.
/// Returns `None` if parsing, allocation, or encoding fails.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the target pixel size is validated finite and >= 1 before the cast"
)]
pub fn rasterize_svg(svg: &str, width: f32, height: f32, scale: f32) -> Option<Vec<u8>> {
    let mut options = usvg::Options {
        font_family: "Ubuntu".to_owned(),
        ..usvg::Options::default()
    };
    {
        let database = options.fontdb_mut();
        database.load_font_data(UBUNTU_REGULAR.to_vec());
        database.load_font_data(UBUNTU_BOLD.to_vec());
        database.set_sans_serif_family("Ubuntu");
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
