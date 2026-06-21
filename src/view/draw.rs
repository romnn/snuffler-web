//! Backend-agnostic 2-D drawing for the waveform view.
//!
//! [`waveform::paint`](crate::view::waveform) issues its draw calls against a
//! [`DrawTarget`] instead of an `egui::Painter` directly. The [`EguiTarget`]
//! backend forwards to a `Painter` for on-screen rendering; the [`SvgTarget`]
//! backend serializes the very same calls to an `<svg>` string for vector
//! export. This mirrors snuffler-qt, whose single `drawit` routine paints to
//! either the screen or a `QSvgGenerator` depending on the requested output.

use std::fmt::Write as _;

use egui::{Align, Align2, Color32, ColorImage, Pos2, Rect, Vec2};

/// A 2-D surface the waveform view paints onto. Coordinates are in points/pixels
/// (the same space the on-screen `egui::Painter` uses).
pub trait DrawTarget {
    fn fill_rect(&mut self, rect: Rect, rounding: f32, fill: Color32);
    fn line(&mut self, a: Pos2, b: Pos2, width: f32, color: Color32);
    fn polyline(&mut self, pts: Vec<Pos2>, width: f32, color: Color32);
    fn polygon(&mut self, pts: Vec<Pos2>, fill: Color32);
    fn text(&mut self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32);
    /// Size of `text` at `size` in egui's font metrics. Both backends measure the
    /// same way so label backgrounds and centred ticks land identically.
    fn measure(&self, text: &str, size: f32) -> Vec2;
    /// Draw the waterfall image for visible track `idx`, stretched into `rect`.
    fn waterfall(&mut self, rect: Rect, idx: usize);
    fn push_clip(&mut self, rect: Rect);
    fn pop_clip(&mut self);
}

/// On-screen backend: forwards every call to an `egui::Painter`, maintaining a
/// stack of clip rects (egui clipping is per-painter, so a clip pushes a derived
/// painter and a pop drops back to the parent).
pub struct EguiTarget<'a> {
    base: egui::Painter,
    clips: Vec<egui::Painter>,
    textures: &'a [Option<egui::TextureHandle>],
}

impl<'a> EguiTarget<'a> {
    pub fn new(painter: egui::Painter, textures: &'a [Option<egui::TextureHandle>]) -> Self {
        Self {
            base: painter,
            clips: Vec::new(),
            textures,
        }
    }

    fn cur(&self) -> &egui::Painter {
        self.clips.last().unwrap_or(&self.base)
    }
}

impl DrawTarget for EguiTarget<'_> {
    fn fill_rect(&mut self, rect: Rect, rounding: f32, fill: Color32) {
        self.cur().rect_filled(rect, rounding, fill);
    }

    fn line(&mut self, a: Pos2, b: Pos2, width: f32, color: Color32) {
        self.cur()
            .line_segment([a, b], egui::Stroke::new(width, color));
    }

    fn polyline(&mut self, pts: Vec<Pos2>, width: f32, color: Color32) {
        self.cur()
            .add(egui::Shape::line(pts, egui::Stroke::new(width, color)));
    }

    fn polygon(&mut self, pts: Vec<Pos2>, fill: Color32) {
        self.cur()
            .add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::NONE));
    }

    fn text(&mut self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        self.cur()
            .text(pos, align, text, egui::FontId::proportional(size), color);
    }

    fn measure(&self, text: &str, size: f32) -> Vec2 {
        self.base.ctx().fonts_mut(|f| {
            f.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(size),
                Color32::WHITE,
            )
            .size()
        })
    }

    fn waterfall(&mut self, rect: Rect, idx: usize) {
        if let Some(Some(tex)) = self.textures.get(idx) {
            self.cur().image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    fn push_clip(&mut self, rect: Rect) {
        let p = self.cur().with_clip_rect(rect);
        self.clips.push(p);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }
}

/// SVG backend: serializes draw calls to an `<svg>` string. Wiggle traces become
/// `<polyline>`, axes/labels `<line>`/`<text>`, and the waterfall heatmap an
/// embedded base64 PNG `<image>` (it is inherently raster), yielding a clean
/// vector overlay over raster waterfall cells.
pub struct SvgTarget {
    ctx: egui::Context,
    body: String,
    defs: String,
    images: Vec<Option<ColorImage>>,
    next_clip: u32,
    open_clips: u32,
    width: f32,
    height: f32,
}

impl SvgTarget {
    pub fn new(
        ctx: egui::Context,
        width: f32,
        height: f32,
        images: Vec<Option<ColorImage>>,
    ) -> Self {
        Self {
            ctx,
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
        // egui anchors text by its top/center/bottom; map to the SVG baseline that
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
        self.ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(size),
                Color32::WHITE,
            )
            .size()
        })
    }

    fn waterfall(&mut self, rect: Rect, idx: usize) {
        let Some(Some(img)) = self.images.get(idx) else {
            return;
        };
        let Some(png) = encode_png(img) else {
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

/// `#rrggbb` plus a 0..1 opacity for an egui colour (straight, un-premultiplied).
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

/// Encode an RGBA `ColorImage` as PNG bytes (8-bit). Shared by the PNG export and
/// the SVG waterfall embedding. Returns `None` rather than panicking on failure.
pub fn encode_png(img: &ColorImage) -> Option<Vec<u8>> {
    let w = u32::try_from(img.size[0]).ok()?;
    let h = u32::try_from(img.size[1]).ok()?;
    encode_png_rgba(w, h, img.as_raw())
}

/// Encode raw 8-bit RGBA pixels as PNG bytes. `pub(crate)` so the optional
/// [`raster`](crate::view::raster) backend can reuse it.
pub(crate) fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}
