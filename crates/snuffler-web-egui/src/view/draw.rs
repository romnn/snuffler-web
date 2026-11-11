//! Egui drawing backend and image conversion for the shared renderer.

use std::collections::HashMap;

use egui::{Align2, Color32, Pos2, Rect, Vec2};

pub use snuffler_web_render::draw::{DrawTarget, RasterImage, SvgTarget, encode_png_rgba};

/// Paints shared waveform draw calls through an egui painter.
pub struct EguiTarget<'a> {
    base: egui::Painter,
    clips: Vec<egui::Painter>,
    textures: &'a HashMap<u64, egui::TextureHandle>,
}

impl<'a> EguiTarget<'a> {
    /// Wraps an egui painter and the waterfall textures for the visible spans,
    /// keyed by payload offset.
    pub fn new(painter: egui::Painter, textures: &'a HashMap<u64, egui::TextureHandle>) -> Self {
        Self {
            base: painter,
            clips: Vec::new(),
            textures,
        }
    }

    /// The painter clipped to the innermost pushed clip rect.
    fn current(&self) -> &egui::Painter {
        self.clips.last().unwrap_or(&self.base)
    }
}

impl DrawTarget for EguiTarget<'_> {
    fn fill_rect(&mut self, rect: Rect, rounding: f32, fill: Color32) {
        self.current().rect_filled(rect, rounding, fill);
    }

    fn line(&mut self, from: Pos2, to: Pos2, width: f32, color: Color32) {
        self.current()
            .line_segment([from, to], egui::Stroke::new(width, color));
    }

    fn polyline(&mut self, points: Vec<Pos2>, width: f32, color: Color32) {
        self.current()
            .add(egui::Shape::line(points, egui::Stroke::new(width, color)));
    }

    fn polygon(&mut self, points: Vec<Pos2>, fill: Color32) {
        self.current().add(egui::Shape::convex_polygon(
            points,
            fill,
            egui::Stroke::NONE,
        ));
    }

    fn text(&mut self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        self.current()
            .text(pos, align, text, crate::view::canvas_font(size), color);
    }

    fn measure(&self, text: &str, size: f32) -> Vec2 {
        self.base.ctx().fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    crate::view::canvas_font(size),
                    Color32::WHITE,
                )
                .size()
        })
    }

    fn waterfall(&mut self, rect: Rect, key: u64) {
        if let Some(texture) = self.textures.get(&key) {
            self.current().image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    fn push_clip(&mut self, rect: Rect) {
        self.clips.push(self.current().with_clip_rect(rect));
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }
}
