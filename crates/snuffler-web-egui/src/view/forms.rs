//! Data-source forms for waveform, station, catalog, and live-stream requests.

use snuffler_web_core::bridge::{
    CatalogRequest, SeedlinkParams, StationRequest, WaveformDownloadRequest,
};
use snuffler_web_core::state::AppState;

/// Requests submitted from the FDSN waveform window during one frame.
#[derive(Default)]
pub struct DownloadRequests {
    /// Waveform query submitted by the user.
    pub waveforms: Option<WaveformDownloadRequest>,
    /// Station query submitted by the user.
    pub stations: Option<StationRequest>,
    /// Response query submitted by the user.
    pub responses: Option<StationRequest>,
    /// Save the loaded waveforms + station table (Qt's Save trigger).
    pub save: bool,
}

/// Intent submitted from the live `SeedLink` window.
pub enum SeedlinkIntent {
    /// Starts a new stream after replacing the current source.
    Start(SeedlinkParams),
    /// Stops the active stream.
    Stop,
    /// Downloads the live buffer as `MiniSEED` — the browser twin of Qt's
    /// continuous stream-to-disk recording, on demand instead of silently.
    SaveBuffer,
}

/// Draws the FDSN waveform window and returns requests triggered this frame.
#[expect(
    clippy::too_many_lines,
    reason = "one form, drawn top to bottom; splitting the rows hides the layout"
)]
pub fn download(ctx: &egui::Context, state: &mut AppState) -> DownloadRequests {
    if !state.show_download {
        return DownloadRequests::default();
    }

    let mut open = true;
    let mut fetch_waveforms = false;
    let mut fetch_stations = false;
    let mut fetch_responses = false;
    let mut save = false;
    egui::Window::new("Download waveforms (FDSN)")
        .open(&mut open)
        .default_width(420.0)
        .show(ctx, |ui| {
            egui::ComboBox::from_label("Data center")
                .selected_text(state.download_form.datacenter.clone())
                .show_ui(ui, |ui| {
                    for datacenter in [
                        "geofon",
                        "earthscope",
                        "iris",
                        "orfeus",
                        "ingv",
                        "resif",
                        "bgr",
                    ] {
                        ui.selectable_value(
                            &mut state.download_form.datacenter,
                            datacenter.to_owned(),
                            datacenter,
                        );
                    }
                });
            egui::Grid::new("dl_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Custom base URL");
                    ui.text_edit_singleline(&mut state.download_form.base_url);
                    ui.end_row();
                    for (label, value) in [
                        ("Network", &mut state.download_form.net),
                        ("Station", &mut state.download_form.sta),
                        ("Location", &mut state.download_form.loc),
                        ("Channel", &mut state.download_form.cha),
                        ("Start (UTC)", &mut state.download_form.start),
                        ("End (UTC)", &mut state.download_form.end),
                    ] {
                        ui.label(label);
                        ui.text_edit_singleline(value);
                        ui.end_row();
                    }
                });
            // Qt's primary download workflow searches by radius around the
            // selected event's epicentre (download.py:28); the code selectors
            // above still narrow the result.
            ui.horizontal(|ui| {
                ui.checkbox(&mut state.download_form.around_event, "Around active event");
                if state.download_form.around_event {
                    ui.add(
                        egui::Slider::new(&mut state.download_form.radius_deg, 0.1..=30.0)
                            .text("radius [°]"),
                    );
                }
            });
            ui.horizontal(|ui| {
                fetch_waveforms = ui.button("Fetch waveforms").clicked();
                fetch_stations = ui.button("Fetch station coords").clicked();
                fetch_responses = ui.button("Fetch responses").clicked();
            });
            ui.horizontal(|ui| {
                // Qt's Save trigger writes MiniSEED plus a station file
                // (download.py:147); the browser twin hands both over as
                // downloads.
                save = ui
                    .button("Save (MiniSEED + stations)")
                    .on_hover_text(
                        "Download the loaded waveforms as MiniSEED and the \
                         station coordinates as a pyrocko station file.",
                    )
                    .clicked();
            });
            ui.small(
                "Wildcards * and ? allowed (e.g. channel BH? = BHZ/BHN/BHE). \
                 Responses enable Options ▸ Deconvolution.",
            );
        });
    state.show_download = open;

    let datacenter = fdsn_datacenter(
        &state.download_form.datacenter,
        &state.download_form.base_url,
    );
    let station_request = || StationRequest {
        datacenter: datacenter.clone(),
        net: state.download_form.net.clone(),
        sta: state.download_form.sta.clone(),
        loc: state.download_form.loc.clone(),
        cha: state.download_form.cha.clone(),
        start: state.download_form.start.clone(),
        end: state.download_form.end.clone(),
        radius_deg: state
            .download_form
            .around_event
            .then_some(state.download_form.radius_deg),
    };
    DownloadRequests {
        waveforms: fetch_waveforms.then(|| WaveformDownloadRequest {
            datacenter: datacenter.clone(),
            net: state.download_form.net.clone(),
            sta: state.download_form.sta.clone(),
            loc: state.download_form.loc.clone(),
            cha: state.download_form.cha.clone(),
            start: state.download_form.start.clone(),
            end: state.download_form.end.clone(),
        }),
        stations: fetch_stations.then(station_request),
        responses: fetch_responses.then(station_request),
        save,
    }
}

/// Draws the live `SeedLink` window and returns its stream intent.
pub fn seedlink(ctx: &egui::Context, state: &mut AppState, active: bool) -> Option<SeedlinkIntent> {
    if !state.show_live_seedlink {
        return None;
    }

    let mut open = true;
    let mut intent = None;
    egui::Window::new("Live SeedLink")
        .open(&mut open)
        .default_width(420.0)
        .show(ctx, |ui| {
            egui::Grid::new("seedlink_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    for (label, value) in [
                        ("Host", &mut state.seedlink_form.host),
                        ("Port", &mut state.seedlink_form.port),
                        ("Network", &mut state.seedlink_form.net),
                        ("Station", &mut state.seedlink_form.sta),
                        ("Selectors", &mut state.seedlink_form.selectors),
                    ] {
                        ui.label(label);
                        ui.text_edit_singleline(value);
                        ui.end_row();
                    }
                    // Qt takes the follow window from `--follow N` and keeps
                    // every sample; the browser's bounded heap needs a keep
                    // limit, so both numbers are on the form instead.
                    ui.label("Follow [s]");
                    ui.add(
                        egui::DragValue::new(&mut state.seedlink_form.follow_s)
                            .range(10.0..=86_400.0),
                    );
                    ui.end_row();
                    ui.label("Keep [s]");
                    ui.add(
                        egui::DragValue::new(&mut state.seedlink_form.retain_s)
                            .range(60.0..=86_400.0),
                    )
                    .on_hover_text(
                        "Live data older than this is discarded — the browser tab's \
                         memory is one bounded block.",
                    );
                    ui.end_row();
                });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!active, egui::Button::new("Start"))
                    .clicked()
                {
                    intent = Some(SeedlinkIntent::Start(SeedlinkParams {
                        host: state.seedlink_form.host.clone(),
                        port: state.seedlink_form.port.clone(),
                        net: state.seedlink_form.net.clone(),
                        sta: state.seedlink_form.sta.clone(),
                        selectors: state.seedlink_form.selectors.clone(),
                        follow_s: state.seedlink_form.follow_s,
                        retain_s: state.seedlink_form.retain_s,
                    }));
                }
                if ui.add_enabled(active, egui::Button::new("Stop")).clicked() {
                    intent = Some(SeedlinkIntent::Stop);
                }
                // Qt records the stream to disk continuously; a browser cannot
                // write silently, so the buffer saves on demand instead.
                if ui
                    .button("Save buffer (MiniSEED)")
                    .on_hover_text("Download the received live data as MiniSEED.")
                    .clicked()
                {
                    intent = Some(SeedlinkIntent::SaveBuffer);
                }
            });
            // The WebSocket route to a SeedLink server exists only through the
            // local proxy; saying so beats a connect error with no context.
            ui.small(
                "Live streaming tunnels through snuffler-web-proxy on \
                 127.0.0.1:17684 (run `micromux`), so it works only for a \
                 locally served app — the deployed site cannot reach SeedLink \
                 servers.",
            );
        });
    state.show_live_seedlink = open;
    intent
}

/// Draws the catalog window and returns a submitted event query.
pub fn catalog(ctx: &egui::Context, state: &mut AppState) -> Option<CatalogRequest> {
    if !state.show_catalog {
        return None;
    }

    let mut open = true;
    let mut submit = false;
    egui::Window::new("Catalog search (events)")
        .open(&mut open)
        .default_width(380.0)
        .show(ctx, |ui| {
            egui::ComboBox::from_label("Catalog")
                .selected_text(state.catalog_form.datacenter.clone())
                .show_ui(ui, |ui| {
                    for datacenter in ["geofon", "usgs", "earthscope", "iris", "ingv"] {
                        ui.selectable_value(
                            &mut state.catalog_form.datacenter,
                            datacenter.to_owned(),
                            datacenter,
                        );
                    }
                });
            egui::Grid::new("cat_grid").num_columns(2).show(ui, |ui| {
                for (label, value) in [
                    ("Custom base URL", &mut state.catalog_form.base_url),
                    ("Start (UTC)", &mut state.catalog_form.start),
                    ("End (UTC)", &mut state.catalog_form.end),
                ] {
                    ui.label(label);
                    ui.text_edit_singleline(value);
                    ui.end_row();
                }
            });
            // Qt's magnitude range runs to 10 (catalogs.py), so the largest events
            // stay reachable.
            ui.add(
                egui::Slider::new(&mut state.catalog_form.min_magnitude, 0.0..=10.0)
                    .text("Min magnitude"),
            );
            ui.checkbox(
                &mut state.catalog_form.with_moment_tensors,
                "Fetch moment tensors (beachball / Kagan)",
            );
            submit = ui.button("Search").clicked();
            ui.small("Events become markers on the time axis at their origin times.");
        });
    state.show_catalog = open;

    submit.then(|| CatalogRequest {
        datacenter: fdsn_datacenter(&state.catalog_form.datacenter, &state.catalog_form.base_url),
        start: state.catalog_form.start.clone(),
        end: state.catalog_form.end.clone(),
        min_magnitude: state.catalog_form.min_magnitude,
        with_moment_tensors: state.catalog_form.with_moment_tensors,
        marker_budget: Some(snuffler_web_core::markers::MarkerBudget::standard().into()),
    })
}

fn fdsn_datacenter(selected: &str, custom_base_url: &str) -> String {
    let custom = custom_base_url.trim();
    if custom.is_empty() {
        selected.to_owned()
    } else {
        custom.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::fdsn_datacenter;

    #[test]
    fn custom_fdsn_endpoint_overrides_the_preset() {
        assert_eq!(
            fdsn_datacenter("https://preset.example", " https://custom.example "),
            "https://custom.example"
        );
    }
}
