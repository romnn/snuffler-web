//! Display of rendered pyrocko/matplotlib figures as egui textures.
//!
//! The frontend never re-draws these figures itself.
//! It shows the *exact* PNG that pyrocko's matplotlib code produced in Pyodide
//! (see `web/snuffler_bridge/figures.py`, `render`), so a figure window can never
//! drift from what the Qt snuffler shows.
//! This module only decodes the PNG once per render and blits it.

use std::collections::HashMap;

use snuffler_web_core::bridge::{Bridge, FigureData, FigureRequest, MomentTensor};
use snuffler_web_core::session::Session;

/// Owns decoded textures and request keys for every matplotlib figure window.
///
/// Requests are keyed with the marker generation they were built from, so the
/// per-frame freshness check compares cheap fields and only rebuilds the
/// selected-span list (which clones the NSLC strings of every selected marker)
/// when a marker actually changed.
#[derive(Default)]
pub struct FigureWindows {
    cache: FigureCache,
    requests: HashMap<String, (u64, FigureRequest)>,
    beachball: Option<[f64; 6]>,
}

impl FigureWindows {
    /// Forces the named figure to be rendered again when its window is next drawn.
    pub fn invalidate(&mut self, kind: &str) {
        self.requests.remove(kind);
    }

    /// Opens the focal-mechanism window for a moment tensor in NED components.
    pub fn open_beachball(&mut self, moment_tensor: [f64; 6]) {
        self.beachball = Some(moment_tensor);
        self.invalidate("beachball");
    }

    /// Drops focal-mechanism state when annotations are replaced with a new source.
    pub fn clear_beachball(&mut self) {
        self.beachball = None;
        self.invalidate("beachball");
    }

    /// Draws every requested pyrocko figure and updates persistent open flags.
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session) {
        if session.snufflings.spectrum.show_plot()
            && !self.show_kind(
                ctx,
                session,
                "ampspec",
                "Amplitude spectrum",
                [620.0, 460.0],
            )
        {
            session.snufflings.spectrum.close_plot();
        }
        if session.snufflings.spectrogram.show_plot()
            && !self.show_kind(ctx, session, "spectrogram", "Spectrogram", [680.0, 480.0])
        {
            session.snufflings.spectrogram.close_plot();
        }
        if session.snufflings.polarization.show_plot()
            && !self.show_kind(ctx, session, "polarization", "Polarization", [760.0, 420.0])
        {
            session.snufflings.polarization.close_plot();
        }
        if session.snufflings.cake.show_model_plot()
            && !self.show_kind(ctx, session, "cake_model", "Cake model", [620.0, 480.0])
        {
            session.snufflings.cake.close_model_plot();
        }
        if session.snufflings.cake.show_rays_plot()
            && !self.show_kind(ctx, session, "cake_rays", "Cake ray paths", [680.0, 480.0])
        {
            session.snufflings.cake.close_rays_plot();
        }
        self.show_beachball(ctx, &session.bridge);
    }

    fn show_kind(
        &mut self,
        ctx: &egui::Context,
        session: &Session,
        kind: &str,
        title: &str,
        size: [f32; 2],
    ) -> bool {
        let generation = session.state.markers.generation();
        let dark = ctx.theme() == egui::Theme::Dark;
        let bridge_has_request =
            session.bridge.figure(kind).is_some() || session.bridge.figure_job(kind).is_some();
        let fresh = bridge_has_request
            && self
                .requests
                .get(kind)
                .is_some_and(|(cached_generation, cached)| {
                    *cached_generation == generation
                        && cached.dark == dark
                        && cached.params.as_ref().is_some_and(|params| {
                            params.view == Some(session.view_params(800))
                                && params.snufflings == Some(session.snufflings.params())
                        })
                });
        if !fresh {
            let request = FigureRequest {
                kind: kind.to_owned(),
                params: Some(session.snuffling_params(800)),
                width_px: 1100,
                height_px: 760,
                dpi: 110.0,
                dark,
                moment_tensor: None,
            };
            // A marker edit that left the request identical (a kind cycled, a
            // note renamed) refreshes the generation key without re-rendering.
            if self
                .requests
                .get(kind)
                .is_none_or(|(_, cached)| cached != &request)
            {
                let _submission = session.bridge.render_figure(request.clone());
            }
            self.requests.insert(kind.to_owned(), (generation, request));
        }

        let mut open = true;
        egui::Window::new(title)
            .open(&mut open)
            .default_size(size)
            .show(ctx, |ui| self.cache.show(ui, &session.bridge, kind));
        if !open {
            self.invalidate(kind);
        }
        open
    }

    fn show_beachball(&mut self, ctx: &egui::Context, bridge: &Bridge) {
        let Some(moment_tensor) = self.beachball else {
            return;
        };
        let request = FigureRequest {
            kind: "beachball".to_owned(),
            params: None,
            width_px: 520,
            height_px: 520,
            dpi: 120.0,
            dark: ctx.theme() == egui::Theme::Dark,
            moment_tensor: Some(MomentTensor {
                mnn: moment_tensor[0],
                mee: moment_tensor[1],
                mdd: moment_tensor[2],
                mne: moment_tensor[3],
                mnd: moment_tensor[4],
                med: moment_tensor[5],
            }),
        };
        if self
            .requests
            .get("beachball")
            .is_none_or(|(_, cached)| cached != &request)
        {
            let _submission = bridge.render_figure(request.clone());
            self.requests.insert("beachball".to_owned(), (0, request));
        }

        let mut open = true;
        egui::Window::new("Focal mechanism")
            .open(&mut open)
            .default_size([440.0, 480.0])
            .show(ctx, |ui| self.cache.show(ui, bridge, "beachball"));
        if !open {
            self.clear_beachball();
        }
    }
}

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
        let generation = bridge.figure_generation(kind);
        if generation == 0 {
            if bridge.figure_job(kind).is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Rendering figure…");
                });
            } else {
                ui.label("No figure has been rendered yet.");
            }
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

/// Decode matplotlib's Agg PNG (RGBA, or RGB with an added opaque alpha) into an egui
/// `ColorImage`.
///
/// Returns `None` on any decode error (the window then just shows nothing for that
/// generation).
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
            let (pixels, _) = data.as_chunks::<3>();
            for px in pixels {
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
