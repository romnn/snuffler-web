//! The Python runtime's lifecycle: worker events, cancellation, restarts and the
//! loss of a worker.

use super::*;

/// Terminates everything owned by a dead runtime and resets the fences and
/// latches that are per-interpreter: the replacement session numbers its
/// generations from zero, must be re-warmed, and has no live stream state.
pub(super) fn note_runtime_lost(state: &mut Inner, epoch: RuntimeEpoch) {
    // A view in flight is wanted again once the replacement is up.
    if let Some(in_flight) = state.view_requests.in_flight.take() {
        state.view_requests.desired_params = Some(in_flight.params);
    }
    state.view_requests.python_session_generation = 0;
    state.examples_job = None;
    state.map_job = None;
    state.figures.clear();

    // The owning executor futures may still be alive; without this, a retired
    // composite flow would keep resolving child owners against the replacement
    // interpreter while blocking every new job-lane submission.
    clear_lane_registrations();
    state.activity.runtime_lost(epoch);
    state.scientific_tools_requested = false;

    // The replacement interpreter has no session for the stream to land in, so
    // the socket retires with the runtime.
    // Otherwise the UI keeps a visibly live acquisition whose every packet is
    // rejected against a Closed stream.
    state.live.retire();
    defer_live_socket_retirement();
}

/// Reconciles the bridge with the worker once per frame: detects an idle worker
/// death, marks the runtime ready once it booted, and applies queued worker events.
pub(in crate::bridge) fn sync_runtime(inner: &Rc<RefCell<Inner>>) {
    let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
    let ready = runtime_info().is_some();
    {
        let mut state = inner.borrow_mut();
        if let Some(previous) = state.runtime_epoch
            && previous != epoch
        {
            // An epoch bump with no rejected reply to report it: the worker
            // died or was restarted while idle.
            note_runtime_lost(&mut state, previous);
        }
        state.runtime_epoch = Some(epoch);
        if ready && !matches!(state.activity.runtime(), RuntimeState::Ready) {
            state.activity.set_runtime(RuntimeState::Ready);
        }
    }
    drain_operation_events(inner);
}

/// Asks the worker to interrupt the running job and returns how that went.
pub(in crate::bridge) fn cancel_job(
    handle: JobHandle,
) -> Result<pb::CancelDisposition, BridgeError> {
    let request = pb::CancelJobRequest {
        job_id: handle.id().get(),
    }
    .encode_to_vec();
    let bytes = js_cancel_job(request).map_err(|error| {
        BridgeError::browser("cancelling Python work", js_value_to_string(error))
    })?;
    let reply: pb::CancelJobReply = proto::decode_message(&bytes, "cancelJob")?;
    pb::CancelDisposition::try_from(reply.disposition).map_err(|_| {
        BridgeError::browser("cancelling Python work", "unknown cancellation disposition")
    })
}

pub(in crate::bridge) fn restart_runtime(inner: &Rc<RefCell<Inner>>) {
    let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
    {
        let mut state = inner.borrow_mut();
        note_runtime_lost(&mut state, epoch);
    }
    js_restart_runtime("Python did not stop after cancellation");
    // Record the successor epoch so the next sync does not report the restart a
    // second time.
    inner.borrow_mut().runtime_epoch = Some(RuntimeEpoch::new(js_current_runtime_epoch()));
}

/// Applies the worker's queued boot failures, started events and progress events.
fn drain_operation_events(inner: &Rc<RefCell<Inner>>) {
    for value in js_take_operation_events().iter() {
        // Decode
        let Ok(bytes) = value.dyn_into::<js_sys::Uint8Array>() else {
            set_diagnostic(
                inner,
                BridgeError::browser("reading Python progress", "event was not bytes"),
            );
            continue;
        };
        let event = match pb::WorkerEvent::decode(bytes.to_vec().as_slice()) {
            Ok(event) => event,
            Err(error) => {
                set_diagnostic(
                    inner,
                    BridgeError::browser("decoding Python progress", error.to_string()),
                );
                continue;
            }
        };
        // An event from a previous worker describes jobs that are already settled.
        if event.runtime_epoch != js_current_runtime_epoch() {
            continue;
        }
        let Some(event) = event.event else {
            continue;
        };

        // Runtime failure
        let operation = match event {
            pb::worker_event::Event::RuntimeFailure(failure) => {
                let detail = if failure.kind.is_empty() {
                    failure.message
                } else {
                    format!("{}: {}", failure.kind, failure.message)
                };
                let error = BridgeError::browser("starting Python", detail);
                let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
                let mut state = inner.borrow_mut();
                note_runtime_lost(&mut state, epoch);
                state
                    .activity
                    .set_runtime(RuntimeState::Failed(error.clone()));
                state.diagnostic = Some(error);
                continue;
            }
            pb::worker_event::Event::Operation(operation) => operation,
            pb::worker_event::Event::Boot(_)
            | pb::worker_event::Event::MlDevice(_)
            | pb::worker_event::Event::RuntimeInfo(_) => continue,
        };

        // Job started or progressed
        match operation.event {
            Some(pb::operation_event::Event::Started(_)) => {
                inner
                    .borrow_mut()
                    .activity
                    .mark_wire_running(operation.job_id, js_sys::Date::now() / 1000.0);
            }
            Some(pb::operation_event::Event::Progress(progress)) => {
                let phase = progress.phase.into();
                let value = match progress.progress {
                    Some(pb::operation_progress::Progress::Count(count)) => Progress::Count {
                        done: count.done,
                        total: count.total,
                        phase,
                    },
                    Some(pb::operation_progress::Progress::Fraction(fraction)) => {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "wire progress is normalized before conversion to the UI's f32"
                        )]
                        let fraction = fraction.clamp(0.0, 1.0) as f32;
                        Progress::Fraction { fraction, phase }
                    }
                    None => {
                        set_diagnostic(
                            inner,
                            BridgeError::browser(
                                "reading Python progress",
                                "progress event had no count or fraction",
                            ),
                        );
                        continue;
                    }
                };
                inner
                    .borrow_mut()
                    .activity
                    .update_wire_progress(operation.job_id, value);
            }
            None => {}
        }
    }
}

/// Where `SeisBench` inference runs, as the panel names it.
pub(in crate::bridge) fn seisbench_device() -> String {
    let status = uint8_array_bytes(js_seisbench_device(), "seisbench_device")
        .ok()
        .and_then(|bytes| {
            proto::decode_message::<pb::MlDeviceStatus>(&bytes, "seisbenchDevice").ok()
        })
        .and_then(|status| pb::MlDevice::try_from(status.device).ok());
    match status {
        Some(pb::MlDevice::Webgpu) => "WebGPU".to_owned(),
        Some(pb::MlDevice::Wasm | pb::MlDevice::Unspecified) | None => "CPU (wasm)".to_owned(),
    }
}

/// The booted runtime's versions, or `None` until the worker reports them.
pub(in crate::bridge) fn runtime_info() -> Option<pb::RuntimeInfo> {
    let info = js_runtime_info();
    if info.is_null() || info.is_undefined() {
        return None;
    }
    let bytes = uint8_array_bytes(info, "runtime_info").ok()?;
    proto::decode_message(&bytes, "runtimeInfo").ok()
}

pub(in crate::bridge) fn compute_budget() -> Result<pb::ComputeBudget, BridgeError> {
    Ok(proto::decode_message(
        &js_compute_budget(),
        "computeBudget",
    )?)
}
