//! Operations on what describes the pile: stations, responses, events, markers,
//! the event registry and the map.

use super::*;

impl Bridge {
    /// Fetch station coordinates (FDSN station service) to enable distance /
    /// azimuth sorting; forces the view to re-dispatch so the sort recomputes.
    pub fn fetch_stations(&self, params: StationRequest) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::FetchStations,
            None,
            Vec::new(),
            async move {
                let result = client().fetch_stations(params).await?;
                let mut state = inner.borrow_mut();
                state.install_snuffling_result(result);
                state.invalidate_view_request();
                Ok(())
            },
        )
    }

    /// Fetch instrument responses (`StationXML`) so Deconvolution can run;
    /// forces a re-dispatch so the deconvolved view recomputes.
    pub fn fetch_responses(&self, params: StationRequest) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::FetchResponses,
            None,
            Vec::new(),
            async move {
                let result = client().fetch_responses(params).await?;
                let mut state = inner.borrow_mut();
                state.install_snuffling_result(result);
                state.invalidate_view_request();
                Ok(())
            },
        )
    }

    /// Search a CORS-enabled FDSN event service; results become event markers.
    /// The largest-magnitude hit becomes the active event, so this forces a
    /// re-dispatch for the distance/azimuth sort to recompute against it.
    pub fn catalog_search(&self, params: CatalogRequest) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::CatalogSearch,
            None,
            Vec::new(),
            async move {
                let result = client().catalog_search(params).await?;
                let mut state = inner.borrow_mut();
                state.install_snuffling_result(result);
                state.invalidate_view_request();
                Ok(())
            },
        )
    }

    /// Load a marker file via a browser picker; results land as markers.
    pub fn load_markers(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadMarkers,
            None,
            Vec::new(),
            move |flow| async move {
                let texts =
                    picked_file_texts(js_load_markers().await, "markers", "loading markers")?;
                ensure_flow_live(&inner, flow, "loading markers")?;
                let Some(text) = texts.into_iter().next() else {
                    return Ok(());
                };
                let loaded = client()
                    .load_markers(pb::LoadMarkersRequest {
                        text,
                        marker_budget: Some(standard_marker_budget()),
                    })
                    .await?;
                let mut state = inner.borrow_mut();
                state.diagnostic = None;
                let marker_count = loaded.markers.len();
                state.install_snuffling_result(SnufflingResult {
                    show_window: true,
                    text: format!("loaded {marker_count} marker(s)"),
                    markers: loaded.markers,
                    scene: None,
                    origin: String::new(),
                    event_state: loaded.event_state,
                    markers_dropped: loaded.markers_dropped,
                    marker_budget: loaded.marker_budget,
                    nslc_table: loaded.nslc_table,
                });
                Ok(())
            },
        )
    }

    /// Pick and merge local `StationXML` files (instrument responses for
    /// deconvolution + station coordinates for sorting) using the same parsing
    /// as fetched `FDSN` responses.
    pub fn load_station_xml(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadStationXml,
            None,
            Vec::new(),
            move |flow| async move {
                let texts = picked_file_texts(
                    js_load_station_xml().await,
                    "stationxml",
                    "loading StationXML",
                )?;
                ensure_flow_live(&inner, flow, "loading StationXML")?;
                if texts.is_empty() {
                    return Ok(());
                }
                let result = client()
                    .load_station_xml(pb::LoadStationXmlRequest { texts })
                    .await?;
                let mut state = inner.borrow_mut();
                state.diagnostic = None;
                state.install_snuffling_result(result);
                Ok(())
            },
        )
    }

    /// Pick a local pyrocko station file and load its station coordinates
    /// (distance/azimuth sorting, cake, map) via `model.load_stations`.
    pub fn load_stations(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadStations,
            None,
            Vec::new(),
            move |flow| async move {
                let texts =
                    picked_file_texts(js_load_stations().await, "stations", "loading stations")?;
                ensure_flow_live(&inner, flow, "loading stations")?;
                if texts.is_empty() {
                    return Ok(());
                }
                let result = client()
                    .load_stations(pb::LoadStationsRequest {
                        text: texts.join("\n"),
                    })
                    .await?;
                let mut state = inner.borrow_mut();
                state.diagnostic = None;
                state.install_snuffling_result(result);
                Ok(())
            },
        )
    }

    /// Pick a local pyrocko event file and load it: event markers + the active
    /// event (largest magnitude) for distance/azimuth sorting, which forces a
    /// re-dispatch so that sort recomputes against the new active event.
    pub fn load_events(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadEvents,
            None,
            Vec::new(),
            move |flow| async move {
                let texts = picked_file_texts(js_load_events().await, "events", "loading events")?;
                ensure_flow_live(&inner, flow, "loading events")?;
                let Some(text) = texts.into_iter().next() else {
                    return Ok(());
                };
                let result = client()
                    .load_events(pb::LoadEventsRequest {
                        text,
                        marker_budget: Some(standard_marker_budget()),
                    })
                    .await?;
                let mut state = inner.borrow_mut();
                state.diagnostic = None;
                state.install_snuffling_result(result);
                state.invalidate_view_request();
                Ok(())
            },
        )
    }

    /// Fetch station + event coordinates for the Map overlay.
    pub fn fetch_map_data(&self, request: MapRequest) -> Submission {
        let request_generation = {
            let mut state = self.inner.borrow_mut();
            if let Some(job) = state.map_job
                && state.map_request.as_ref() == Some(&request)
            {
                return Submission::Joined(job);
            }
            if state.map_request.as_ref() != Some(&request) {
                state.map = None;
            }
            state.map_request = Some(request.clone());
            state.map_request_generation = state.map_request_generation.wrapping_add(1);
            state.map_request_generation
        };
        let inner = self.inner.clone();
        let dedup_key = request.encode_to_vec();
        let submission = submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::MapData,
            None,
            dedup_key,
            async move {
                let result = client().map_data(request).await;
                let mut state = inner.borrow_mut();
                // A newer request owns the map now; this reply is dropped.
                if state.map_request_generation == request_generation {
                    state.map_job = None;
                    state.map = Some(result?);
                }
                Ok(())
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            self.inner.borrow_mut().map_job = Some(handle);
        }
        submission
    }

    /// Sets or clears the active event and installs Python's canonical registry.
    pub fn set_active_event(&self, request: SetActiveEventRequest) -> Submission {
        self.submit_event_registry_edit(Operation::SetActiveEvent, async move {
            client().set_active_event(request).await
        })
    }

    /// Replaces one canonical event after an inline marker-table edit.
    pub fn update_event(&self, event: EventSnapshot) -> Submission {
        self.submit_event_registry_edit(Operation::UpdateEvent, async move {
            client().update_event(event).await
        })
    }

    /// Removes canonical events whose final frontend marker was deleted.
    pub fn remove_events(&self, request: RemoveEventsRequest) -> Submission {
        self.submit_event_registry_edit(Operation::RemoveEvents, async move {
            client().remove_events(request).await
        })
    }

    /// One edit of the canonical event registry: run the rpc, then install the
    /// registry Python returns.
    /// The three editors above differ only in the rpc they run.
    fn submit_event_registry_edit<F>(&self, operation: Operation, call: F) -> Submission
    where
        F: Future<Output = Result<ActiveEventResult, CallError>> + 'static,
    {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            operation,
            None,
            Vec::new(),
            async move {
                let event_state = call.await?;
                let mut state = inner.borrow_mut();
                state.diagnostic = None;
                state.install_event_state(event_state);
                Ok(())
            },
        )
    }
}
