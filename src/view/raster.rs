//! High-DPI PNG export by rasterizing the view's SVG with resvg.
//!
//! This module is the only place that depends on resvg / usvg / tiny-skia, and it
//! is compiled solely when the `svg-raster` feature is on (the default). Building
//! with `--no-default-features` drops the whole vector→raster stack from the
//! bundle; the PNG export then falls back to capturing the egui framebuffer at
//! screen resolution (see `app.rs`).

use resvg::{tiny_skia, usvg};

/// Embedded font so SVG `<text>` renders the same as the on-screen view — resvg
/// has no system fonts to fall back on under wasm.
const UBUNTU_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Ubuntu-Regular.ttf");

/// Rasterize `svg` to PNG bytes at `scale`× its declared point size, giving a
/// crisp high-DPI image. `width`/`height` are the SVG viewBox size in points.
/// Returns `None` if parsing, allocation, or encoding fails.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the target pixel size is validated finite and >= 1 before the cast"
)]
pub fn rasterize_svg(svg: &str, width: f32, height: f32, scale: f32) -> Option<Vec<u8>> {
    let mut opt = usvg::Options {
        font_family: "Ubuntu".to_owned(),
        ..usvg::Options::default()
    };
    {
        let db = opt.fontdb_mut();
        db.load_font_data(UBUNTU_REGULAR.to_vec());
        db.set_sans_serif_family("Ubuntu");
    }
    let tree = usvg::Tree::from_str(svg, &opt).ok()?;

    let w = (width * scale).round();
    let h = (height * scale).round();
    if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // The view always paints an opaque background first, so the rendered pixmap is
    // fully opaque and tiny-skia's premultiplied buffer equals straight RGBA here.
    crate::view::draw::encode_png_rgba(pixmap.width(), pixmap.height(), pixmap.data())
}
