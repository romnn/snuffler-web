//! Backend-agnostic 2-D drawing for the waveform view.
//!
//! Waveform painters issue draw calls against [`DrawTarget`] instead of a toolkit API.
//!
//! [`SvgTarget`] serializes those calls to a standalone SVG. Frontends provide their own
//! on-screen target implementations.

use std::collections::HashMap;
use std::fmt::Write as _;

use ecolor::Color32;
use emath::{Align, Align2, Pos2, Rect, Vec2};

type MeasureText = dyn Fn(&str, f32) -> Vec2;

/// A 2-D surface the waveform view paints onto in logical points.
pub trait DrawTarget {
    /// Fills a rounded rectangle.
    fn fill_rect(&mut self, rect: Rect, rounding: f32, fill: Color32);
    /// Draws one line segment.
    fn line(&mut self, a: Pos2, b: Pos2, width: f32, color: Color32);
    /// Draws a connected open path.
    fn polyline(&mut self, pts: Vec<Pos2>, width: f32, color: Color32);
    /// Fills a polygon.
    fn polygon(&mut self, pts: Vec<Pos2>, fill: Color32);
    /// Draws aligned text at the given logical point size.
    fn text(&mut self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32);
    /// Measures text with the same metrics used by the corresponding frontend.
    fn measure(&self, text: &str, size: f32) -> Vec2;
    /// Draws the cached waterfall image for one span, keyed by its payload
    /// offset — a track can carry several strips, so a row index would not
    /// identify one.
    fn waterfall(&mut self, rect: Rect, key: u64);
    /// Intersects subsequent drawing with `rect`.
    fn push_clip(&mut self, rect: Rect);
    /// Restores the previous clipping region.
    fn pop_clip(&mut self);
}

/// Raw straight-alpha RGBA pixels used by backend-neutral image drawing.
#[derive(Clone, Debug)]
pub struct RasterImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA8 bytes.
    pub rgba: Vec<u8>,
}

/// Accumulates draw calls into a standalone SVG document.
///
/// Wiggle traces become vector paths while waterfall heatmaps remain embedded PNG images.
pub struct SvgTarget {
    measure: Box<MeasureText>,
    body: String,
    defs: String,
    images: HashMap<u64, RasterImage>,
    next_clip: u32,
    open_clips: u32,
    width: f32,
    height: f32,
}

impl SvgTarget {
    /// Creates an SVG target using frontend-provided font metrics.
    pub fn new(
        width: f32,
        height: f32,
        images: HashMap<u64, RasterImage>,
        measure: impl Fn(&str, f32) -> Vec2 + 'static,
    ) -> Self {
        Self {
            measure: Box::new(measure),
            body: String::new(),
            defs: String::new(),
            images,
            next_clip: 0,
            open_clips: 0,
            width,
            height,
        }
    }

    /// Assemble the final `<svg>` document.
    #[must_use]
    pub fn finish(mut self) -> String {
        while self.open_clips > 0 {
            self.body.push_str("</g>");
            self.open_clips -= 1;
        }
        let (w, h) = (self.width, self.height);
        let mut out = String::with_capacity(self.body.len() + self.defs.len() + 256);
        let _ = write!(
            out,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
             width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.0} {h:.0}\">"
        );
        out.push_str(
            "<defs><style>text{font-family:'Ubuntu',system-ui,-apple-system,sans-serif;}</style>",
        );
        out.push_str(&self.defs);
        out.push_str("</defs>");
        out.push_str(&self.body);
        out.push_str("</svg>");
        out
    }
}

impl DrawTarget for SvgTarget {
    fn fill_rect(&mut self, rect: Rect, rounding: f32, fill: Color32) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let (col, op) = css_color(fill);
        let rx = if rounding > 0.0 {
            format!(" rx=\"{rounding:.1}\"")
        } else {
            String::new()
        };
        let _ = write!(
            self.body,
            "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\"{} fill=\"{}\" fill-opacity=\"{:.3}\"/>",
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height(),
            rx,
            col,
            op
        );
    }

    fn line(&mut self, a: Pos2, b: Pos2, width: f32, color: Color32) {
        let (col, op) = css_color(color);
        let _ = write!(
            self.body,
            "<line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" stroke=\"{}\" stroke-opacity=\"{:.3}\" stroke-width=\"{:.2}\"/>",
            a.x, a.y, b.x, b.y, col, op, width
        );
    }

    fn polyline(&mut self, pts: Vec<Pos2>, width: f32, color: Color32) {
        if pts.len() < 2 {
            return;
        }
        let (col, op) = css_color(color);
        let mut d = String::with_capacity(pts.len() * 12);
        for p in &pts {
            let _ = write!(d, "{:.2},{:.2} ", p.x, p.y);
        }
        let _ = write!(
            self.body,
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{:.3}\" stroke-width=\"{:.2}\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/>",
            d.trim_end(),
            col,
            op,
            width
        );
    }

    fn polygon(&mut self, pts: Vec<Pos2>, fill: Color32) {
        if pts.is_empty() {
            return;
        }
        let (col, op) = css_color(fill);
        let mut d = String::with_capacity(pts.len() * 12);
        for p in &pts {
            let _ = write!(d, "{:.2},{:.2} ", p.x, p.y);
        }
        let _ = write!(
            self.body,
            "<polygon points=\"{}\" fill=\"{}\" fill-opacity=\"{:.3}\"/>",
            d.trim_end(),
            col,
            op
        );
    }

    fn text(&mut self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        if text.is_empty() {
            return;
        }
        let (col, op) = css_color(color);
        let anchor = match align.x() {
            Align::Min => "start",
            Align::Center => "middle",
            Align::Max => "end",
        };
        // Draw targets anchor text by its top/center/bottom; map to the SVG baseline that
        // places that edge at `pos.y`.
        let baseline = match align.y() {
            Align::Min => "text-before-edge",
            Align::Center => "central",
            Align::Max => "text-after-edge",
        };
        let _ = write!(
            self.body,
            "<text x=\"{:.2}\" y=\"{:.2}\" font-size=\"{:.2}\" fill=\"{}\" fill-opacity=\"{:.3}\" text-anchor=\"{}\" dominant-baseline=\"{}\">{}</text>",
            pos.x,
            pos.y,
            size,
            col,
            op,
            anchor,
            baseline,
            escape_xml(text)
        );
    }

    fn measure(&self, text: &str, size: f32) -> Vec2 {
        (self.measure)(text, size)
    }

    fn waterfall(&mut self, rect: Rect, key: u64) {
        let Some(img) = self.images.get(&key) else {
            return;
        };
        let Some(png) = encode_png_rgba(img.width, img.height, &img.rgba) else {
            return;
        };
        let _ = write!(
            self.body,
            "<image x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" preserveAspectRatio=\"none\" image-rendering=\"pixelated\" xlink:href=\"data:image/png;base64,{}\"/>",
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height(),
            base64(&png)
        );
    }

    fn push_clip(&mut self, rect: Rect) {
        let id = self.next_clip;
        self.next_clip += 1;
        let _ = write!(
            self.defs,
            "<clipPath id=\"c{id}\"><rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\"/></clipPath>",
            rect.min.x,
            rect.min.y,
            rect.width().max(0.0),
            rect.height().max(0.0)
        );
        let _ = write!(self.body, "<g clip-path=\"url(#c{id})\">");
        self.open_clips += 1;
    }

    fn pop_clip(&mut self) {
        if self.open_clips > 0 {
            self.body.push_str("</g>");
            self.open_clips -= 1;
        }
    }
}

/// `#rrggbb` plus a 0..1 opacity for a straight, un-premultiplied colour.
fn css_color(color: Color32) -> (String, f32) {
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    (
        format!("#{red:02x}{green:02x}{blue:02x}"),
        f32::from(alpha) / 255.0,
    )
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Minimal standard base64 (used to inline the waterfall PNG into the SVG).
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let enc = |b: u8| char::from(*T.get(usize::from(b) & 63).unwrap_or(&b'A'));
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(enc(b0 >> 2));
        out.push(enc((b0 << 4) | (b1 >> 4)));
        out.push(if chunk.len() > 1 {
            enc((b1 << 2) | (b2 >> 6))
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { enc(b2) } else { '=' });
    }
    out
}

/// Encodes raw RGBA8 pixels as PNG bytes.
#[must_use]
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ecolor::Color32;
    use emath::{Align2, Rect, pos2, vec2};

    use super::{DrawTarget, SvgTarget};

    #[test]
    fn svg_target_preserves_clips_and_escapes_labels() {
        let mut target = SvgTarget::new(200.0, 100.0, HashMap::new(), |text, size| {
            vec2(text.chars().fold(0.0, |width, _| width + size * 0.5), size)
        });
        target.push_clip(Rect::from_min_max(pos2(2.0, 3.0), pos2(80.0, 60.0)));
        target.fill_rect(
            Rect::from_min_max(pos2(4.0, 5.0), pos2(20.0, 30.0)),
            2.0,
            Color32::from_rgb(1, 2, 3),
        );
        target.text(
            pos2(10.0, 12.0),
            Align2::LEFT_TOP,
            "P < S & surface",
            12.0,
            Color32::WHITE,
        );
        target.pop_clip();

        let svg = target.finish();
        assert!(svg.contains("<clipPath id=\"c0\">"));
        assert!(svg.contains("clip-path=\"url(#c0)\""));
        assert!(svg.contains("P &lt; S &amp; surface"));
        assert!(svg.ends_with("</svg>"));
    }
}
