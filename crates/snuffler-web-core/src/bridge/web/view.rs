//! The view lane: coalescing the frontend's per-frame view requests into at most one
//! `GetView` in flight, and applying replies that pass the staleness fences.

use std::time::Duration;

use super::*;

/// How soon a frame polls again while a view is pending: about one 60 Hz frame.
const VIEW_POLL_INTERVAL: Duration = Duration::from_millis(17);
/// How long a failed view request waits before it is retried.
const VIEW_RETRY_DELAY_S: f64 = 0.5;
/// How soon a frame polls again while the retry delay runs.
const VIEW_RETRY_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How soon a frame polls again while the runtime is down.
const RUNTIME_DOWN_POLL_INTERVAL: Duration = Duration::from_millis(200);

impl Bridge {
    /// Request a view for the given params, coalescing rapid changes so that
    /// at most one Python call is in flight at a time.
    #[must_use]
    pub fn ensure_view(&self, params: ViewParams) -> Option<Submission> {
        {
            let mut state = self.inner.borrow_mut();
            let requests = &mut state.view_requests;

            // Already on screen: only keep it wanted while an older request is out
            if requests.applied_params.as_ref() == Some(&params) {
                if requests.in_flight.is_none() {
                    requests.desired_params = None;
                } else {
                    requests.desired_params = Some(params);
                    self.repaint.request_repaint_after(VIEW_POLL_INTERVAL);
                }
                return requests
                    .in_flight
                    .as_ref()
                    .map(|in_flight| Submission::Joined(in_flight.job));
            }

            // Already in flight
            let joined = requests
                .in_flight
                .as_ref()
                .filter(|in_flight| in_flight.params == params)
                .map(|in_flight| in_flight.job);
            if let Some(job) = joined {
                requests.desired_params = None;
                self.repaint.request_repaint_after(VIEW_POLL_INTERVAL);
                return Some(Submission::Joined(job));
            }

            // Wanted next: wait for the request in flight, the retry delay, or the runtime
            requests.desired_params = Some(params);
            if let Some(in_flight) = &requests.in_flight {
                self.repaint.request_repaint_after(VIEW_POLL_INTERVAL);
                return Some(Submission::Joined(in_flight.job));
            }
            let now_s = js_sys::Date::now() / 1000.0;
            if now_s < requests.retry_after_s {
                self.repaint.request_repaint_after(VIEW_RETRY_POLL_INTERVAL);
                return None;
            }
            // While the runtime is down, submitting would only mint one rejected
            // ledger record per frame; the runtime state itself is already on screen,
            // and `desired_params` re-dispatches on recovery.
            // Booting still submits: those jobs queue until Ready.
            if matches!(
                state.activity.runtime(),
                RuntimeState::Restarting | RuntimeState::Failed(_)
            ) {
                self.repaint
                    .request_repaint_after(RUNTIME_DOWN_POLL_INTERVAL);
                return None;
            }
        }
        self.dispatch_view()
    }

    /// Sends the desired view as the one `GetView` in flight.
    fn dispatch_view(&self) -> Option<Submission> {
        let (params, canonical_params, request_generation) = {
            let mut state = self.inner.borrow_mut();
            let requests = &mut state.view_requests;
            if let Some(in_flight) = &requests.in_flight {
                return Some(Submission::Joined(in_flight.job));
            }
            let mut params = requests.desired_params.take()?;
            if requests.applied_params.as_ref() == Some(&params) {
                return None;
            }
            let canonical_params = params.clone();
            requests.last_sequence = requests.last_sequence.wrapping_add(1).max(1);
            params.view_sequence = requests.last_sequence;
            (params, canonical_params, requests.generation)
        };
        self.repaint.request_repaint_after(VIEW_POLL_INTERVAL);

        let bridge = self.clone();
        let reply_canonical_params = canonical_params.clone();
        let dedup_key = params.encode_to_vec();
        let submission = submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::GetView,
            None,
            dedup_key,
            async move {
                let sequence = params.view_sequence;
                let reply = client()
                    .get_view(params)
                    .await
                    .map(|(meta, ys)| ViewData { meta, ys });
                let outcome = bridge.apply_view_reply(
                    reply,
                    sequence,
                    &reply_canonical_params,
                    request_generation,
                );
                {
                    let mut state = bridge.inner.borrow_mut();
                    let finished = state
                        .view_requests
                        .in_flight
                        .as_ref()
                        .is_some_and(|in_flight| in_flight.params == reply_canonical_params);
                    if finished {
                        state.view_requests.in_flight = None;
                    }
                }
                bridge.dispatch_desired_view();
                // The first drawn view is the moment to warm the scientific stack.
                if outcome.is_ok() {
                    let _submission = bridge.prepare_scientific_tools();
                }
                outcome
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            self.inner.borrow_mut().view_requests.in_flight = Some(InFlightView {
                job: handle,
                params: canonical_params,
            });
        }
        Some(submission)
    }

    /// Installs a `GetView` reply unless a newer invalidation or a staleness fence
    /// rejects it.
    fn apply_view_reply(
        &self,
        reply: Result<ViewData, CallError>,
        sequence: u64,
        canonical_params: &ViewParams,
        request_generation: u64,
    ) -> Result<(), BridgeError> {
        let mut state = self.inner.borrow_mut();
        // An invalidation since dispatch already wants a different view.
        if state.view_requests.generation != request_generation {
            return Ok(());
        }
        let view = match reply {
            Ok(view) => view,
            Err(error) => {
                let requests = &mut state.view_requests;
                requests.retry_after_s = js_sys::Date::now() / 1000.0 + VIEW_RETRY_DELAY_S;
                requests.desired_params = Some(canonical_params.clone());
                return Err(error.into());
            }
        };

        // Staleness fences: the reply must answer this request, from the current session
        let requests = &mut state.view_requests;
        if view.meta.view_sequence != sequence
            || view.meta.session_generation < requests.python_session_generation
        {
            requests.desired_params = Some(canonical_params.clone());
            return Err(BridgeError::browser(
                "applying a waveform view",
                "stale view reply",
            ));
        }
        requests.python_session_generation = view.meta.session_generation;
        requests.retry_after_s = 0.0;
        // Every reply that passed the fences is applied, even when a newer window is
        // already desired.
        // During a continuous drag the desired params are always newer than any reply,
        // and skipping the fresh reply would freeze the canvas at the pre-gesture
        // window for the whole gesture.
        // The newer request still goes out: `desired_params` stays set and
        // `dispatch_desired_view` compares it against `applied_params`.
        requests.applied_params = Some(canonical_params.clone());
        if requests.desired_params.as_ref() == Some(canonical_params) {
            requests.desired_params = None;
        }
        state.view = Some(Arc::new(view));
        state.view_epoch += 1;
        Ok(())
    }

    /// Dispatches the desired view once nothing is in flight and no retry delay runs.
    fn dispatch_desired_view(&self) {
        let should_dispatch = {
            let mut state = self.inner.borrow_mut();
            let requests = &mut state.view_requests;
            if requests.in_flight.is_some() {
                false
            } else if js_sys::Date::now() / 1000.0 < requests.retry_after_s {
                self.repaint.request_repaint_after(VIEW_RETRY_POLL_INTERVAL);
                false
            } else if requests.desired_params.is_some()
                && requests.applied_params != requests.desired_params
            {
                true
            } else {
                requests.desired_params = None;
                false
            }
        };
        if should_dispatch {
            let _submission = self.dispatch_view();
        }
    }

    /// Asks Python, once per runtime, to import the scientific stack every analysis needs.
    fn prepare_scientific_tools(&self) -> Option<Submission> {
        {
            let mut state = self.inner.borrow_mut();
            if state.scientific_tools_requested {
                return None;
            }
            state.scientific_tools_requested = true;
        }
        Some(submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::PrepareScientificTools,
            None,
            Vec::new(),
            async move {
                client().prepare_scientific_tools().await?;
                Ok(())
            },
        ))
    }
}
