//! Ledger-fenced dispatch: the per-lane permit future, job submission, the
//! worker transport every generated client call rides on, and the progress
//! attribution for the active job-lane flow.

#![allow(
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    reason = "split out of the web module and glued with the same glob import; \
              owned parameters cross the JS/async boundary as in the parent"
)]

use super::*;

struct DispatchPermit {
    inner: Rc<RefCell<Inner>>,
    handle: JobHandle,
}

thread_local! {
    static ACTIVE_LEDGER_JOBS: StdRefCell<Vec<(JobHandle, crate::activity::Lane)>> =
        const { StdRefCell::new(Vec::new()) };
}

pub(super) fn require_registered_job(
    operation: pb::Operation,
    method: &'static str,
) -> Result<JobHandle, TransportError> {
    ACTIVE_LEDGER_JOBS.with(|active| {
        active
            .borrow()
            .iter()
            .find(|(_, lane)| *lane == operation.activity().lane)
            .map(|(handle, _)| *handle)
            .map_or_else(
                || {
                    Err(TransportError::new(
                        method,
                        "Python call has no registered activity job",
                    ))
                },
                Ok,
            )
    })
}

impl Future for DispatchPermit {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.inner.borrow().activity.is_terminal(this.handle) {
            return Poll::Ready(false);
        }
        if this
            .inner
            .borrow_mut()
            .activity
            .try_dispatch(this.handle, context.waker())
        {
            Poll::Ready(true)
        } else {
            Poll::Pending
        }
    }
}

pub(super) fn submit_job<F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    detail: Option<String>,
    dedup_key: Vec<u8>,
    work: F,
) -> Submission
where
    F: Future<Output = Result<(), BridgeError>> + 'static,
{
    sync_runtime(&inner);
    let submission = inner.borrow_mut().activity.submit(
        operation,
        detail,
        dedup_key,
        js_sys::Date::now() / 1000.0,
    );
    // A rejected or joined submission must not cancel the work it failed to
    // replace — superseding is a property of admitted replacements only.
    if operation.activity().supersedes && matches!(submission, Submission::Accepted(_)) {
        let active: Vec<JobHandle> = inner
            .borrow()
            .activity
            .records()
            .filter(|(handle, record)| {
                *handle != submission.handle()
                    && record.operation().activity().lane == crate::activity::Lane::Job
                    && matches!(
                        record.state(),
                        JobState::Dispatching | JobState::Running { .. }
                    )
            })
            .map(|(handle, _)| handle)
            .collect();
        for handle in active {
            let cancel = inner
                .borrow_mut()
                .activity
                .request_cancel(handle, js_sys::Date::now() / 1000.0);
            if matches!(
                cancel,
                CancelRequest::Interrupt | CancelRequest::AlreadyCancelling
            ) {
                match cancel_job(handle) {
                    Ok(pb::CancelDisposition::TooLate) => {
                        inner.borrow_mut().activity.mark_cancel_too_late(handle);
                    }
                    Ok(_) => {}
                    Err(error) => inner.borrow_mut().diagnostic = Some(error),
                }
            }
        }
    }
    if let Submission::Accepted(handle) = submission {
        let scheduled_inner = inner.clone();
        let scheduled_repaint = repaint.clone();
        spawn_local(async move {
            let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
            let dispatch = DispatchPermit {
                inner: scheduled_inner.clone(),
                handle,
            }
            .await;
            if !dispatch {
                scheduled_repaint.request_repaint();
                return;
            }
            scheduled_repaint.request_repaint();
            ACTIVE_LEDGER_JOBS.with(|active| {
                active
                    .borrow_mut()
                    .push((handle, operation.activity().lane));
            });
            let result = work.await;
            ACTIVE_LEDGER_JOBS.with(|active| {
                active.borrow_mut().retain(|(active, _)| *active != handle);
            });
            let runtime_info = js_runtime_info();
            let runtime_missing = runtime_info.is_null() || runtime_info.is_undefined();
            if result.is_err() && runtime_missing {
                let mut state = scheduled_inner.borrow_mut();
                // Report the loss only if nothing else already did (an explicit
                // restart or the sync poll can beat this future); a second
                // report would re-retire the replacement runtime's state. The
                // earlier report already terminated this job as Lost.
                if state
                    .runtime_epoch
                    .is_none_or(|current| current == epoch.get())
                {
                    note_runtime_lost(&mut state, epoch);
                    state.runtime_epoch = Some(js_current_runtime_epoch());
                }
            } else {
                let outcome = match result {
                    Ok(()) => Outcome::Succeeded,
                    Err(BridgeError::Call(CallError::Cancelled { .. })) => Outcome::Cancelled,
                    Err(BridgeError::Call(CallError::Stale { .. })) => Outcome::Stale,
                    Err(error) => Outcome::Failed(JobError::Bridge(error)),
                };
                scheduled_inner
                    .borrow_mut()
                    .activity
                    .complete(handle, outcome);
            }
            scheduled_repaint.request_repaint();
        });
    }
    submission
}

#[allow(
    clippy::too_many_arguments,
    clippy::cast_precision_loss,
    reason = "transport diagnostics convert byte counts to MB and keep separate phase timings"
)]
fn log_transport_perf(
    method: &str,
    request_bytes: usize,
    result_bytes: usize,
    ys_bytes: usize,
    js_call_ms: f64,
    result_copy_ms: f64,
    ys_copy_ms: f64,
    total_ms: f64,
) {
    if !method.ends_with("/GetView") {
        return;
    }
    log::info!(
        "[bridge.transport] method=getView request_bytes={} result_bytes={} ys_bytes={} ys_mb={:.3} js_call_ms={:.2} rust_result_copy_ms={:.2} rust_ys_copy_ms={:.2} total_ms={:.2}",
        request_bytes,
        result_bytes,
        ys_bytes,
        ys_bytes as f64 / 1_000_000.0,
        js_call_ms,
        result_copy_ms,
        ys_copy_ms,
        total_ms,
    );
}

/// The frontend's `BridgeTransport`: the generated typed client marshals each
/// encoded request through `js_call` to the worker and reads back the
/// `BridgeResult` bytes (and the out-of-band `ys` for buffer methods). Unit
/// struct — the worker holds all session state.
pub(super) struct WorkerTransport;

const BRIDGE_TRANSPORT_PERF_LOG: bool = false;

impl pb::BridgeTransport for WorkerTransport {
    async fn unary(
        &self,
        operation: pb::Operation,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<Vec<u8>, TransportError> {
        let job = require_registered_job(operation, method)?;
        let obj = js_call(method, request, Vec::new(), job.id().get())
            .await
            .map_err(|e| TransportError::new(method, jsval_to_string(e)))?;
        object_uint8(&obj, "result").map_err(|e| TransportError::new(method, e.to_string()))
    }

    async fn unary_with_buffer(
        &self,
        operation: pb::Operation,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<(Vec<u8>, Vec<f32>), TransportError> {
        let job = require_registered_job(operation, method)?;
        let marshal = |e: MarshalError| TransportError::new(method, e.to_string());
        if !BRIDGE_TRANSPORT_PERF_LOG {
            let obj = js_call(method, request, Vec::new(), job.id().get())
                .await
                .map_err(|e| TransportError::new(method, jsval_to_string(e)))?;
            return Ok((
                object_uint8(&obj, "result").map_err(marshal)?,
                object_f32(&obj, "ys").map_err(marshal)?,
            ));
        }
        let t0 = js_sys::Date::now();
        let request_bytes = request.len();
        let obj = js_call(method, request, Vec::new(), job.id().get())
            .await
            .map_err(|e| TransportError::new(method, jsval_to_string(e)))?;
        let js_done = js_sys::Date::now();
        let result = object_uint8(&obj, "result").map_err(marshal)?;
        let result_done = js_sys::Date::now();
        let ys = object_f32(&obj, "ys").map_err(marshal)?;
        let done = js_sys::Date::now();
        log_transport_perf(
            method,
            request_bytes,
            result.len(),
            ys.len() * 4,
            js_done - t0,
            result_done - js_done,
            done - result_done,
            done - t0,
        );
        Ok((result, ys))
    }
}

pub(super) fn client() -> pb::BridgeServiceClient<WorkerTransport> {
    pb::BridgeServiceClient::new(WorkerTransport)
}

pub(super) fn update_active_progress(inner: &Rc<RefCell<Inner>>, done: u64, total: u64, phase: String) {
    ACTIVE_LEDGER_JOBS.with(|active| {
        let job = active
            .borrow()
            .iter()
            .find(|(_, lane)| *lane == crate::activity::Lane::Job)
            .map(|(handle, _)| *handle);
        if let Some(job) = job {
            inner.borrow_mut().activity.update_progress(
                job,
                Progress::Count {
                    done,
                    total: Some(total),
                    phase: phase.into(),
                },
            );
        }
    });
}

/// Whether the ledger has a cancellation pending against the job-lane flow
/// currently executing. A multi-child flow has windows with no transport
/// call in flight, so a cancel there finds nothing to interrupt; the
/// Cancelling state is the request, honored at the next child boundary.
pub(super) fn active_job_cancelled(inner: &Rc<RefCell<Inner>>) -> bool {
    ACTIVE_LEDGER_JOBS.with(|active| {
        let job = active
            .borrow()
            .iter()
            .find(|(_, lane)| *lane == crate::activity::Lane::Job)
            .map(|(handle, _)| *handle);
        job.is_some_and(|job| {
            inner
                .borrow()
                .activity
                .get(job)
                .is_some_and(|record| matches!(record.state(), JobState::Cancelling { .. }))
        })
    })
}
