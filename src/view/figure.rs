//! Display of rendered pyrocko/matplotlib figures as egui textures.
//!
//! The frontend never re-draws these figures itself — it shows the *exact* PNG
//! that pyrocko's matplotlib code produced in Pyodide (see `bridge.py`
//! `_render_figure_png`), so a figure window can never drift from what the Qt
//! snuffler shows. This module only decodes the PNG once per render and blits it.

use std::collections::HashMap;

use crate::bridge::{Bridge, FigureData};

/// Per-kind cache of the decoded figure texture, refreshed only when the bridge's
/// render generation for that kind changes (decoding a PNG every frame would be
/// wasteful).
#[derive(Default)]
pub struct FigureCache {
    textures: HashMap<String, (u64, egui::TextureHandle)>,
}

impl FigureCache {
    /// Draw the latest figure for `kind`: a spinner until the first render lands,
    /// the error text if rendering failed, else the image fit to the panel width.
    pub fn show(&mut self, ui: &mut egui::Ui, bridge: &Bridge, kind: &str) {
        let generation = bridge.figure_gen(kind);
        if generation == 0 {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Rendering figure…");
            });
            return;
        }
        let Some(figure) = bridge.figure(kind) else {
            return;
        };
        if !figure.error.is_empty() {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.colored_label(egui::Color32::from_rgb(0xC0, 0x60, 0x40), &figure.error);
            });
            return;
        }

        let needs_reload = self
            .textures
            .get(kind)
            .is_none_or(|(cached, _)| *cached != generation);
        if needs_reload && let Some(image) = decode_png(&figure) {
            let handle = ui.ctx().load_texture(
                format!("figure:{kind}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.textures.insert(kind.to_owned(), (generation, handle));
        }

        if let Some((_, texture)) = self.textures.get(kind) {
            // Figures are rendered at a fixed high resolution; fit them to the
            // window width (allowing modest upscaling), preserving aspect ratio.
            let size = texture.size_vec2();
            let scale = (ui.available_width().max(64.0) / size.x.max(1.0)).min(2.0);
            egui::ScrollArea::both().show(ui, |ui| {
                ui.add(egui::Image::new(texture).fit_to_exact_size(size * scale));
            });
        }
    }
}

/// Decode matplotlib's Agg PNG (RGBA, or RGB with an added opaque alpha) into an
/// egui `ColorImage`. Returns `None` on any decode error (the window then just
/// shows nothing for that generation).
fn decode_png(figure: &FigureData) -> Option<egui::ColorImage> {
    let decoder = png::Decoder::new(std::io::Cursor::new(&figure.png));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let width = info.width as usize;
    let height = info.height as usize;
    let data = buf.get(..info.buffer_size())?;
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(width * height * 4);
            for px in data.chunks_exact(3) {
                out.extend_from_slice(px);
                out.push(255);
            }
            out
        }
        _ => return None,
    };
    if rgba.len() != width * height * 4 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [width, height],
        &rgba,
    ))
}
