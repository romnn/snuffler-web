//! Image and marker exports, which run as local ledger jobs and, on the threaded
//! build, off the main thread.

use snuffler_web_core::activity::{JobHandle, Operation, Submission};
use snuffler_web_core::bridge::BridgeError;
use snuffler_web_core::markers::Marker;

use super::App;
use crate::view::marker_table::markers_to_file;
use crate::view::waveform;

/// A PNG rasterization running on the rayon pool.
#[cfg(feature = "threads")]
pub(super) struct PngExport {
    receiver: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
    filename: String,
    job: JobHandle,
}

/// A marker serialization running on the rayon pool.
#[cfg(feature = "threads")]
pub(super) struct MarkerExport {
    receiver: std::sync::mpsc::Receiver<snuffler_web_core::bridge::MarkerFile>,
    job: JobHandle,
}

/// Which markers a marker export writes.
#[derive(Clone, Copy)]
pub(super) enum MarkerExportScope {
    All,
    Selected,
}

/// Supersampling factor for the resvg-backed PNG export: the output is this many
/// times the on-screen point size per axis, giving a crisp print/retina image.
const PNG_EXPORT_SCALE: f32 = 2.0;

impl App {
    fn complete_png_rasterization(
        &mut self,
        job: JobHandle,
        png: Option<Vec<u8>>,
        filename: String,
    ) {
        self.session.bridge.finish_local(
            job,
            png.as_ref().map_or_else(
                || {
                    Err(BridgeError::browser(
                        "rendering a PNG",
                        "rasterization failed",
                    ))
                },
                |_| Ok(()),
            ),
        );
        if let Some(png) = png {
            let _submission = self.session.bridge.save_image_png(png, filename);
        }
    }

    /// Starts a high-DPI PNG export of the current waveform view.
    ///
    /// SVG generation stays on the main wasm thread because a cloned egui context
    /// shares locks with the live renderer.
    /// A rayon worker contending for those locks reaches epaint's debug lock
    /// timeout, which calls `std::time::Instant` and aborts on wasm, and the browser
    /// main thread may not block on a lock either, so plain `parking_lot` locks do
    /// not fix it.
    /// Rasterization receives only the owned SVG and runs on rayon when available.
    fn start_png_export(&mut self, ctx: &egui::Context, job: JobHandle) {
        let theme = ctx.theme();
        let size = self.last_waveform_rect.size();
        let svg = {
            let Some(view) = self.session.bridge.view() else {
                self.session.bridge.finish_local(
                    job,
                    Err(BridgeError::browser(
                        "rendering a PNG",
                        "no waveform view is available",
                    )),
                );
                return;
            };
            waveform::render_svg(ctx, &self.session.state, &view, size, theme, 0.0)
        };

        #[cfg(feature = "threads")]
        if crate::pool::ready() {
            let (tx, rx) = std::sync::mpsc::channel();
            let (w, h) = (size.x, size.y);
            #[expect(
                clippy::disallowed_methods,
                reason = "the ExportPng ledger job accepted above keeps this rasterization visible"
            )]
            rayon::spawn(move || {
                let png = crate::view::raster::rasterize_svg(&svg, w, h, PNG_EXPORT_SCALE);
                crate::pool::record_worker_png_export();
                let _ = tx.send(png);
            });
            if let Some(displaced) = self.export_rx.take() {
                // Unreachable behind the one-export-at-a-time request guard,
                // but a displaced receiver must not leak its ledger job as
                // permanently running.
                self.session.bridge.finish_local(
                    displaced.job,
                    Err(BridgeError::browser(
                        "exporting PNG",
                        "superseded by a newer export",
                    )),
                );
            }
            self.export_rx = Some(PngExport {
                receiver: rx,
                filename: export_filename("png"),
                job,
            });
            return;
        }

        let png = crate::view::raster::rasterize_svg(&svg, size.x, size.y, PNG_EXPORT_SCALE);
        self.complete_png_rasterization(job, png, export_filename("png"));
    }

    /// Delivers a finished off-thread PNG export while the navbar activity keeps
    /// frames coming and communicates that rasterization is still running.
    #[cfg(feature = "threads")]
    fn poll_png_export(&mut self, ctx: &egui::Context) {
        let status = self
            .export_rx
            .as_ref()
            .map(|export| export.receiver.try_recv());
        match status {
            Some(Ok(png)) => {
                if let Some(export) = self.export_rx.take() {
                    self.complete_png_rasterization(export.job, png, export.filename);
                }
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {
                // Poll the rasterizer at a bounded rate instead of spinning the
                // frame loop at full speed for the duration of the export.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                if let Some(export) = self.export_rx.take() {
                    self.session.bridge.finish_local(
                        export.job,
                        Err(BridgeError::browser(
                            "rendering a PNG",
                            "worker disconnected",
                        )),
                    );
                }
            }
            None => {}
        }
    }

    pub(super) fn request_marker_export(&mut self, scope: MarkerExportScope) {
        if self.marker_prepare.is_some()
            || matches!(scope, MarkerExportScope::Selected)
                && !self
                    .session
                    .state
                    .markers
                    .iter()
                    .any(|marker| marker.selected)
        {
            return;
        }
        #[cfg(feature = "threads")]
        if self.marker_export_rx.is_some() {
            return;
        }
        let submission = self.session.bridge.begin_local(
            Operation::SerializeMarkers,
            Some(match scope {
                MarkerExportScope::All => "all markers".to_owned(),
                MarkerExportScope::Selected => "selected markers".to_owned(),
            }),
        );
        if let Submission::Accepted(job) = submission {
            self.marker_prepare = Some((job, scope));
        }
    }

    fn complete_marker_serialization(
        &mut self,
        job: JobHandle,
        file: snuffler_web_core::bridge::MarkerFile,
    ) {
        self.session.bridge.finish_local(job, Ok(()));
        let _submission = self.session.bridge.save_markers(file);
    }

    pub(super) fn process_marker_export(&mut self, ctx: &egui::Context) {
        let _ = ctx;
        #[cfg(feature = "threads")]
        if let Some(export) = self.marker_export_rx.as_ref() {
            match export.receiver.try_recv() {
                Ok(file) => {
                    let job = export.job;
                    self.marker_export_rx = None;
                    self.complete_marker_serialization(job, file);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let job = export.job;
                    self.marker_export_rx = None;
                    self.session.bridge.finish_local(
                        job,
                        Err(BridgeError::browser(
                            "serializing markers",
                            "worker disconnected",
                        )),
                    );
                }
            }
            return;
        }

        let Some((job, scope)) = self.marker_prepare.take() else {
            return;
        };
        let markers: Vec<Marker> = match scope {
            MarkerExportScope::Selected => self
                .session
                .state
                .markers
                .iter()
                .filter(|marker| marker.selected)
                .cloned()
                .collect(),
            MarkerExportScope::All => self.session.state.markers.iter().cloned().collect(),
        };

        #[cfg(feature = "threads")]
        if crate::pool::ready() {
            let (tx, rx) = std::sync::mpsc::channel();
            #[expect(
                clippy::disallowed_methods,
                reason = "the SerializeMarkers ledger job accepted above owns this serialization"
            )]
            rayon::spawn(move || {
                let _ = tx.send(markers_to_file(&markers));
            });
            self.marker_export_rx = Some(MarkerExport { receiver: rx, job });
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }

        let file = markers_to_file(&markers);
        self.complete_marker_serialization(job, file);
    }

    pub(super) fn handle_export_requests(&mut self, ctx: &egui::Context) {
        if let Some(job) = self.svg_prepare_job.take() {
            let theme = ctx.theme();
            // Qt's SVG export is a deliberate print canvas, not a window grab:
            // 842x595 (A4 landscape in points) with a 2.5 % margin framed into
            // the viewBox (`savesvg`, pile_viewer.py:2884).
            let size = egui::vec2(842.0, 595.0);
            let margin = size.max_elem() * 0.025;
            let svg = self.session.bridge.view().map(|view| {
                waveform::render_svg(ctx, &self.session.state, &view, size, theme, margin)
            });
            if let Some(svg) = svg {
                self.session.bridge.finish_local(job, Ok(()));
                let _submission = self
                    .session
                    .bridge
                    .save_image_svg(svg, export_filename("svg"));
            } else {
                self.session.bridge.finish_local(
                    job,
                    Err(BridgeError::browser(
                        "rendering an SVG",
                        "no waveform view is available",
                    )),
                );
            }
        }
        if std::mem::take(&mut self.svg_request) && self.svg_prepare_job.is_none() {
            let submission = self
                .session
                .bridge
                .begin_local(Operation::ExportSvg, Some("waveform view".to_owned()));
            if let Submission::Accepted(job) = submission {
                self.svg_prepare_job = Some(job);
                ctx.request_repaint();
            }
        }

        // Poll BEFORE consuming a new request: a rasterization that already
        // finished must free its slot first, or a click landing in that gap
        // would be rejected by the one-export-at-a-time guard for no reason.
        #[cfg(feature = "threads")]
        self.poll_png_export(ctx);
        if let Some(job) = self.png_prepare_job.take() {
            self.start_png_export(ctx, job);
        }
        if std::mem::take(&mut self.png_request) {
            #[cfg(feature = "threads")]
            let rasterizing = self.export_rx.is_some();
            #[cfg(not(feature = "threads"))]
            let rasterizing = false;
            // One PNG export at a time: a second start while the first is
            // preparing or rasterizing would overwrite `export_rx` and leak the
            // displaced ledger job as permanently running.
            if self.png_prepare_job.is_none() && !rasterizing {
                let submission = self
                    .session
                    .bridge
                    .begin_local(Operation::ExportPng, Some("waveform view".to_owned()));
                if let Submission::Accepted(job) = submission {
                    self.png_prepare_job = Some(job);
                    ctx.request_repaint();
                }
            }
        }
    }
}

/// A timestamped export name (`snuffler-20260816-142530.png`): a fixed name
/// collides into `snuffler (1).png` on the second export, and the stamp says
/// which session a file in the downloads folder came from.
fn export_filename(ext: &str) -> String {
    let now_s = js_sys::Date::now() / 1000.0;
    let stamp: String = snuffler_web_core::timeaxis::format_full(now_s)
        .chars()
        .take_while(|&c| c != '.')
        .filter(|c| c.is_ascii_digit() || *c == ' ')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    format!("snuffler-{stamp}.{ext}")
}
