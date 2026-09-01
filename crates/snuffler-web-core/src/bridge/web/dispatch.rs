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
    static REGISTERED_LANE_JOBS: StdRefCell<RegisteredLaneJobs> =
        const { StdRefCell::new(RegisteredLaneJobs::new()) };
}

struct RegisteredLaneJobs {
    job: Option<JobHandle>,
    view: Option<JobHandle>,
}

impl RegisteredLaneJobs {
    const fn new() -> Self {
        Self {
            job: None,
            view: None,
        }
    }

    const fn get(&self, lane: crate::activity::Lane) -> Option<JobHandle> {
        match lane {
            crate::activity::Lane::Job => self.job,
            crate::activity::Lane::View => self.view,
            crate::activity::Lane::Local => None,
        }
    }

    fn slot_mut(&mut self, lane: crate::activity::Lane) -> Option<&mut Option<JobHandle>> {
        match lane {
            crate::activity::Lane::Job => Some(&mut self.job),
            crate::activity::Lane::View => Some(&mut self.view),
            crate::activity::Lane::Local => None,
        }
    }

    fn register(&mut self, handle: JobHandle, lane: crate::activity::Lane) -> bool {
        let Some(slot) = self.slot_mut(lane) else {
            return true;
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(handle);
        true
    }

    fn unregister(&mut self, handle: JobHandle, lane: crate::activity::Lane) {
        let Some(slot) = self.slot_mut(lane) else {
            return;
        };
        if *slot == Some(handle) {
            *slot = None;
        }
    }
}

pub(super) fn require_registered_job(
    operation: pb::Operation,
    method: &'static str,
) -> Result<JobHandle, TransportError> {
    REGISTERED_LANE_JOBS.with(|active| {
        active.borrow().get(operation.activity().lane).map_or_else(
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
    submit_job_with_outcome(inner, repaint, operation, detail, dedup_key, async move {
        work.await.map(|()| Outcome::Succeeded)
    })
}

pub(super) fn submit_local_future<F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    detail: Option<String>,
    work: F,
) -> Submission
where
    F: Future<Output = Result<(), BridgeError>> + 'static,
{
    let submission =
        inner
            .borrow_mut()
            .activity
            .submit_local(operation, detail, js_sys::Date::now() / 1000.0);
    if let Submission::Accepted(handle) = submission {
        let scheduled_inner = inner;
        let scheduled_repaint = repaint;
        #[expect(
            clippy::disallowed_methods,
            reason = "the accepted local activity handle below owns this browser task"
        )]
        spawn_local(async move {
            let result = work.await;
            let outcome = match result {
                Ok(()) => Outcome::Succeeded,
                Err(error) => {
                    scheduled_inner.borrow_mut().diagnostic = Some(error.clone());
                    Outcome::Failed(JobError::Bridge(error))
                }
            };
            scheduled_inner
                .borrow_mut()
                .activity
                .complete(handle, outcome);
            scheduled_repaint.request_repaint();
        });
    }
    submission
}

async fn run_admitted_job<F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    handle: JobHandle,
    work: F,
) where
    F: Future<Output = Result<Outcome, BridgeError>>,
{
    let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
    let dispatch = DispatchPermit {
        inner: inner.clone(),
        handle,
    }
    .await;
    if !dispatch {
        repaint.request_repaint();
        return;
    }
    repaint.request_repaint();
    let lane = operation.activity().lane;
    let registered = REGISTERED_LANE_JOBS.with(|active| active.borrow_mut().register(handle, lane));
    let result = if registered {
        work.await
    } else {
        Err(BridgeError::browser(
            "dispatching Python work",
            "the activity ledger granted an occupied lane",
        ))
    };
    REGISTERED_LANE_JOBS.with(|active| {
        active.borrow_mut().unregister(handle, lane);
    });
    let runtime_missing = runtime_info().is_none();
    if result.is_err() && runtime_missing {
        let mut state = inner.borrow_mut();
        // Report the loss only if nothing else already did (an explicit restart
        // or the sync poll can beat this future); a second report would retire
        // the replacement runtime's state.
        // The earlier report already terminated this job as Lost.
        if state
            .runtime_epoch
            .is_none_or(|current| current == epoch.get())
            && !matches!(state.activity.runtime(), RuntimeState::Failed(_))
        {
            note_runtime_lost(&mut state, epoch);
            state.runtime_epoch = Some(js_current_runtime_epoch());
        }
    } else {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(BridgeError::Call(CallError::Cancelled { .. })) => Outcome::Cancelled,
            Err(BridgeError::Call(CallError::Stale { .. })) => Outcome::Stale,
            Err(error) => Outcome::Failed(JobError::Bridge(error)),
        };
        inner.borrow_mut().activity.complete(handle, outcome);
    }
    repaint.request_repaint();
}

pub(super) fn submit_job_with_outcome<F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    detail: Option<String>,
    dedup_key: Vec<u8>,
    work: F,
) -> Submission
where
    F: Future<Output = Result<Outcome, BridgeError>> + 'static,
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
        #[expect(
            clippy::disallowed_methods,
            reason = "the accepted ledger handle below owns this executor task"
        )]
        spawn_local(run_admitted_job(
            inner.clone(),
            repaint.clone(),
            operation,
            handle,
            work,
        ));
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

pub(super) fn update_current_job_progress(
    inner: &Rc<RefCell<Inner>>,
    done: u64,
    total: u64,
    phase: String,
) {
    REGISTERED_LANE_JOBS.with(|active| {
        let job = active.borrow().get(crate::activity::Lane::Job);
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
pub(super) fn current_job_cancellation_requested(inner: &Rc<RefCell<Inner>>) -> bool {
    REGISTERED_LANE_JOBS.with(|active| {
        let job = active.borrow().get(crate::activity::Lane::Job);
        job.is_some_and(|job| {
            inner
                .borrow()
                .activity
                .get(job)
                .is_some_and(|record| matches!(record.state(), JobState::Cancelling { .. }))
        })
    })
}
