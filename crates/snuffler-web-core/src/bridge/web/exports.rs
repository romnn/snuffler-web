//! Operations that hand the user a file: waveform, station and marker exports, and
//! the frontend's rendered PNG and SVG images.

use super::*;

/// Starts a browser download of `bytes`.
async fn download_bytes(
    bytes: Vec<u8>,
    filename: String,
    mime: String,
    action: &'static str,
) -> Result<(), BridgeError> {
    js_download_file(bytes, filename, mime)
        .await
        .map_err(|error| BridgeError::browser(action, js_value_to_string(error)))?;
    Ok(())
}

/// Starts a browser download of a file pyrocko serialized.
async fn download_blob(
    result: Result<pb::FileBlob, CallError>,
    action: &'static str,
) -> Result<(), BridgeError> {
    let blob = result?;
    download_bytes(blob.content, blob.filename, blob.mime, action).await
}

impl Bridge {
    /// Qt's download Save trigger: the loaded waveforms as one `MiniSEED`
    /// download plus the station table as a pyrocko station file.
    ///
    /// A refusal remains a typed recoverable diagnostic, so either successful
    /// half still downloads and the viewer stays operational.
    pub fn save_waveforms_and_stations(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ExportWaveforms,
            Some("waveforms and stations".to_owned()),
            Vec::new(),
            move |flow| async move {
                // Both halves run even when the first fails: one refused file
                // must not cost the user the other download.
                // The first failure takes the diagnostic slot.
                let waveforms =
                    download_blob(client().export_waveforms().await, "downloading waveforms").await;
                ensure_flow_live(&inner, flow, "waveform and station export")?;
                let stations =
                    download_blob(client().export_stations().await, "downloading stations").await;
                let failed = [waveforms, stations].into_iter().find_map(Result::err);
                inner.borrow_mut().diagnostic.clone_from(&failed);
                match failed {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            },
        )
    }

    /// The waveform half alone: the live window's "save the buffer as `MiniSEED`",
    /// since a stream rarely has a station table to save with it.
    /// A refusal surfaces as a recoverable diagnostic.
    pub fn save_waveforms(&self) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ExportWaveforms,
            Some("waveforms".to_owned()),
            Vec::new(),
            async move {
                let result =
                    download_blob(client().export_waveforms().await, "downloading waveforms").await;
                settle_diagnostic(&inner, result)
            },
        )
    }

    /// Serializes markers through pyrocko and downloads the resulting text.
    /// A failure surfaces as a typed recoverable diagnostic.
    pub fn save_markers(&self, markers: MarkerFile) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::MarkersToText,
            Some("markers.txt".to_owned()),
            Vec::new(),
            async move {
                let result = match client().markers_to_text(markers).await {
                    Ok(text) => {
                        download_bytes(
                            text.value.into_bytes(),
                            "markers.txt".to_owned(),
                            "text/plain".to_owned(),
                            "downloading markers",
                        )
                        .await
                    }
                    Err(error) => Err(error.into()),
                };
                settle_diagnostic(&inner, result)
            },
        )
    }

    /// Trigger a browser download of encoded image bytes (PNG export).
    pub fn save_image_png(&self, bytes: Vec<u8>, filename: String) -> Submission {
        let detail = Some(filename.clone());
        submit_local_future(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ExportPng,
            detail,
            async move {
                js_save_png(bytes, filename).await.map_err(|error| {
                    BridgeError::browser("saving a PNG", js_value_to_string(error))
                })?;
                Ok(())
            },
        )
    }

    /// Trigger a browser download of an SVG document (vector image export).
    pub fn save_image_svg(&self, svg: String, filename: String) -> Submission {
        let detail = Some(filename.clone());
        submit_local_future(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ExportSvg,
            detail,
            async move {
                js_save_svg(svg, filename).await.map_err(|error| {
                    BridgeError::browser("saving an SVG", js_value_to_string(error))
                })?;
                Ok(())
            },
        )
    }
}
