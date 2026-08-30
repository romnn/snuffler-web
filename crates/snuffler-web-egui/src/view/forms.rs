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
                .selected_text(state.dl_datacenter.clone())
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
                            &mut state.dl_datacenter,
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
                    ui.text_edit_singleline(&mut state.dl_base_url);
                    ui.end_row();
                    for (label, value) in [
                        ("Network", &mut state.dl_net),
                        ("Station", &mut state.dl_sta),
                        ("Location", &mut state.dl_loc),
                        ("Channel", &mut state.dl_cha),
                        ("Start (UTC)", &mut state.dl_start),
                        ("End (UTC)", &mut state.dl_end),
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
                ui.checkbox(&mut state.dl_around_event, "Around active event");
                if state.dl_around_event {
                    ui.add(
                        egui::Slider::new(&mut state.dl_radius_deg, 0.1..=30.0).text("radius [°]"),
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

    let datacenter = fdsn_datacenter(&state.dl_datacenter, &state.dl_base_url);
    let station_request = || StationRequest {
        datacenter: datacenter.clone(),
        net: state.dl_net.clone(),
        sta: state.dl_sta.clone(),
        loc: state.dl_loc.clone(),
        cha: state.dl_cha.clone(),
        start: state.dl_start.clone(),
        end: state.dl_end.clone(),
        radius_deg: state.dl_around_event.then_some(state.dl_radius_deg),
    };
    DownloadRequests {
        waveforms: fetch_waveforms.then(|| WaveformDownloadRequest {
            datacenter: datacenter.clone(),
            net: state.dl_net.clone(),
            sta: state.dl_sta.clone(),
            loc: state.dl_loc.clone(),
            cha: state.dl_cha.clone(),
            start: state.dl_start.clone(),
            end: state.dl_end.clone(),
        }),
        stations: fetch_stations.then(&station_request),
        responses: fetch_responses.then(&station_request),
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
                        ("Host", &mut state.live_seedlink_host),
                        ("Port", &mut state.live_seedlink_port),
                        ("Network", &mut state.live_seedlink_net),
                        ("Station", &mut state.live_seedlink_sta),
                        ("Selectors", &mut state.live_seedlink_selectors),
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
                        egui::DragValue::new(&mut state.live_seedlink_follow_s)
                            .range(10.0..=86_400.0),
                    );
                    ui.end_row();
                    ui.label("Keep [s]");
                    ui.add(
                        egui::DragValue::new(&mut state.live_seedlink_retain_s)
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
                        host: state.live_seedlink_host.clone(),
                        port: state.live_seedlink_port.clone(),
                        net: state.live_seedlink_net.clone(),
                        sta: state.live_seedlink_sta.clone(),
                        selectors: state.live_seedlink_selectors.clone(),
                        follow_s: state.live_seedlink_follow_s,
                        retain_s: state.live_seedlink_retain_s,
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
                .selected_text(state.cat_datacenter.clone())
                .show_ui(ui, |ui| {
                    for datacenter in ["geofon", "usgs", "earthscope", "iris", "ingv"] {
                        ui.selectable_value(
                            &mut state.cat_datacenter,
                            datacenter.to_owned(),
                            datacenter,
                        );
                    }
                });
            egui::Grid::new("cat_grid").num_columns(2).show(ui, |ui| {
                for (label, value) in [
                    ("Custom base URL", &mut state.cat_base_url),
                    ("Start (UTC)", &mut state.cat_start),
                    ("End (UTC)", &mut state.cat_end),
                ] {
                    ui.label(label);
                    ui.text_edit_singleline(value);
                    ui.end_row();
                }
            });
            // Qt's magnitude range runs to 10 (catalogs.py) — a cap at 9
            // silently excludes the largest events there are.
            ui.add(egui::Slider::new(&mut state.cat_minmag, 0.0..=10.0).text("Min magnitude"));
            ui.checkbox(
                &mut state.cat_with_mt,
                "Fetch moment tensors (beachball / Kagan)",
            );
            submit = ui.button("Search").clicked();
            ui.small("Events become markers on the time axis at their origin times.");
        });
    state.show_catalog = open;

    submit.then(|| CatalogRequest {
        datacenter: fdsn_datacenter(&state.cat_datacenter, &state.cat_base_url),
        start: state.cat_start.clone(),
        end: state.cat_end.clone(),
        minmag: state.cat_minmag,
        with_mt: state.cat_with_mt,
        marker_budget: Some(snuffler_web_core::bridge::ProtoMarkerBudget {
            max_markers: snuffler_web_core::marker_store::MarkerBudget::standard().get(),
        }),
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
