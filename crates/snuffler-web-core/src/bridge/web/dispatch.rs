//! Ledger-fenced dispatch: job submission, the per-lane permit future, the
//! worker transport every generated client call rides on, and the lane owner a
//! running flow checks its identity against.

use std::pin::Pin;
use std::task::{Context, Poll};

use crate::activity::Lane;

use super::*;

thread_local! {
    /// The job that holds each Python lane's permit right now.
    /// The transport reads it to tag every call with its owning job.
    static LANE_OWNERS: RefCell<LaneOwners> = const { RefCell::new(LaneOwners::new()) };
}

struct LaneOwners {
    job: Option<JobHandle>,
    view: Option<JobHandle>,
}

impl LaneOwners {
    const fn new() -> Self {
        Self {
            job: None,
            view: None,
        }
    }

    const fn get(&self, lane: Lane) -> Option<JobHandle> {
        match lane {
            Lane::Job => self.job,
            Lane::View => self.view,
            Lane::Local => None,
        }
    }

    fn slot_mut(&mut self, lane: Lane) -> Option<&mut Option<JobHandle>> {
        match lane {
            Lane::Job => Some(&mut self.job),
            Lane::View => Some(&mut self.view),
            Lane::Local => None,
        }
    }

    /// Records `handle` as the lane's owner; false when the lane is already owned.
    fn claim(&mut self, handle: JobHandle, lane: Lane) -> bool {
        let Some(slot) = self.slot_mut(lane) else {
            return true;
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(handle);
        true
    }

    fn release(&mut self, handle: JobHandle, lane: Lane) {
        let Some(slot) = self.slot_mut(lane) else {
            return;
        };
        if *slot == Some(handle) {
            *slot = None;
        }
    }
}

/// The job that owns `operation`'s lane, which every Python call must run under.
pub(super) fn lane_owner(
    operation: pb::Operation,
    method: &'static str,
) -> Result<JobHandle, TransportError> {
    LANE_OWNERS.with(|owners| {
        owners
            .borrow()
            .get(operation.activity().lane)
            .ok_or_else(|| {
                TransportError::new(method, "Python call has no registered activity job")
            })
    })
}

/// Releases every lane.
/// Runtime loss must call this: the ledger terminates the owning jobs, but their
/// executor futures may still be alive, and a stale owner would let a retired flow
/// keep issuing children against the replacement interpreter while blocking every
/// new job-lane submission with an occupied lane.
pub(super) fn clear_lane_registrations() {
    LANE_OWNERS.with(|owners| {
        let mut owners = owners.borrow_mut();
        owners.job = None;
        owners.view = None;
    });
}

/// Whether the flow owning `flow` must stop at its next child boundary.
///
/// A multi-child flow has windows with no transport call in flight, so a cancel there
/// finds nothing to interrupt; the Cancelling state is the request, honored here.
///
/// The identity comparison is exact.
/// After a runtime loss frees the lane, it may already be owned by a replacement job,
/// and a ghost flow that consulted the lane alone would adopt that live job's identity:
/// its children would commit into the replacement interpreter's session under the
/// wrong job id.
pub(super) fn flow_must_stop(inner: &Rc<RefCell<Inner>>, flow: JobHandle) -> bool {
    let owner = LANE_OWNERS.with(|owners| owners.borrow().get(Lane::Job));
    if owner != Some(flow) {
        return true;
    }
    inner.borrow().activity.get(flow).is_none_or(|record| {
        matches!(record.state(), JobState::Cancelling { .. }) || record.state().is_terminal()
    })
}

/// Stops a composite flow at a child boundary.
/// Every flow re-checks after an unbounded browser-local await (a picker, a directory
/// walk): the flow may have been cancelled, or retired outright by a runtime loss, while
/// the dialog was open, and its next child must not run.
pub(super) fn ensure_flow_live(
    inner: &Rc<RefCell<Inner>>,
    flow: JobHandle,
    method: &'static str,
) -> Result<(), BridgeError> {
    if flow_must_stop(inner, flow) {
        return Err(BridgeError::Call(CallError::Cancelled { method }));
    }
    Ok(())
}

/// Records progress against the flow's own ledger identity.
/// By handle rather than by lane owner: a retired flow's handle is terminal, where
/// `update_progress` is a no-op, so a ghost can never overwrite a replacement job's
/// progress bar.
pub(super) fn update_flow_progress(
    inner: &Rc<RefCell<Inner>>,
    flow: JobHandle,
    done: u64,
    total: u64,
    phase: String,
) {
    inner.borrow_mut().activity.update_progress(
        flow,
        Progress::Count {
            done,
            total: Some(total),
            phase: phase.into(),
        },
    );
}

/// Resolves once the ledger grants the job its lane, or to false once the job is
/// already terminal (cancelled while queued, or lost with its runtime).
struct LanePermit {
    inner: Rc<RefCell<Inner>>,
    handle: JobHandle,
}

impl Future for LanePermit {
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

/// Submits one unit of Python work and runs `work` once its lane is free.
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
    submit_flow_with_outcome(
        inner,
        repaint,
        operation,
        detail,
        dedup_key,
        move |_flow| async move { work.await.map(|()| Outcome::Succeeded) },
    )
}

/// [`submit_job`] for work that must know its own ledger identity.
/// A flow that suspends on browser-local awaits (pickers, directory walks, downloads)
/// can outlive a runtime loss, and only its own [`JobHandle`] lets it detect that the
/// job lane has since been freed or handed to a replacement job.
pub(super) fn submit_flow<W, F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    detail: Option<String>,
    dedup_key: Vec<u8>,
    work: W,
) -> Submission
where
    W: FnOnce(JobHandle) -> F + 'static,
    F: Future<Output = Result<(), BridgeError>> + 'static,
{
    submit_flow_with_outcome(
        inner,
        repaint,
        operation,
        detail,
        dedup_key,
        move |flow| async move { work(flow).await.map(|()| Outcome::Succeeded) },
    )
}

/// [`submit_flow`] for work that reports its own terminal [`Outcome`] (a
/// partial stop, a too-late cancellation) instead of the plain success/failure
/// mapping.
pub(super) fn submit_flow_with_outcome<W, F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    detail: Option<String>,
    dedup_key: Vec<u8>,
    work: W,
) -> Submission
where
    W: FnOnce(JobHandle) -> F + 'static,
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
    // replace: superseding is a property of admitted replacements only.
    if operation.activity().supersedes
        && let Submission::Accepted(handle) = submission
    {
        cancel_superseded_jobs(&inner, handle);
    }
    if let Submission::Accepted(handle) = submission {
        #[expect(
            clippy::disallowed_methods,
            reason = "the accepted ledger handle below owns this executor task"
        )]
        spawn_local(run_admitted_job(inner, repaint, operation, handle, work));
    }
    submission
}

/// Cancels every job-lane job already dispatched or running, other than `replacement`.
fn cancel_superseded_jobs(inner: &Rc<RefCell<Inner>>, replacement: JobHandle) {
    let active: Vec<JobHandle> = inner
        .borrow()
        .activity
        .records()
        .filter(|(handle, record)| {
            *handle != replacement
                && record.operation().activity().lane == Lane::Job
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
        if !matches!(
            cancel,
            CancelRequest::Interrupt | CancelRequest::AlreadyCancelling
        ) {
            continue;
        }
        match cancel_job(handle) {
            Ok(pb::CancelDisposition::TooLate) => {
                inner.borrow_mut().activity.mark_cancel_too_late(handle);
            }
            Ok(_) => {}
            Err(error) => inner.borrow_mut().diagnostic = Some(error),
        }
    }
}

/// Registers executor-owned local work and runs it at once, off the Python lanes.
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
        #[expect(
            clippy::disallowed_methods,
            reason = "the accepted local activity handle below owns this browser task"
        )]
        spawn_local(async move {
            let outcome = match work.await {
                Ok(()) => Outcome::Succeeded,
                Err(error) => {
                    inner.borrow_mut().diagnostic = Some(error.clone());
                    Outcome::Failed(JobError::Bridge(error))
                }
            };
            inner.borrow_mut().activity.complete(handle, outcome);
            repaint.request_repaint();
        });
    }
    submission
}

/// Waits for the lane permit, runs `work` as the lane's owner, and settles the job.
async fn run_admitted_job<W, F>(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    operation: Operation,
    handle: JobHandle,
    work: W,
) where
    W: FnOnce(JobHandle) -> F,
    F: Future<Output = Result<Outcome, BridgeError>>,
{
    let epoch = RuntimeEpoch::new(js_current_runtime_epoch());

    // Wait for the lane
    let dispatched = LanePermit {
        inner: inner.clone(),
        handle,
    }
    .await;
    repaint.request_repaint();
    if !dispatched {
        return;
    }

    // Run as the lane's owner
    let lane = operation.activity().lane;
    let claimed = LANE_OWNERS.with(|owners| owners.borrow_mut().claim(handle, lane));
    let result = if claimed {
        work(handle).await
    } else {
        Err(BridgeError::browser(
            "dispatching Python work",
            "the activity ledger granted an occupied lane",
        ))
    };
    LANE_OWNERS.with(|owners| {
        owners.borrow_mut().release(handle, lane);
    });

    // Settle
    let runtime_missing = runtime_info().is_none();
    if result.is_err() && runtime_missing {
        let mut state = inner.borrow_mut();
        // Report the loss only if nothing else already did (an explicit restart
        // or the sync can beat this future); a second report would retire the
        // replacement runtime's state.
        // The earlier report already terminated this job as Lost.
        if state.runtime_epoch.is_none_or(|current| current == epoch)
            && !matches!(state.activity.runtime(), RuntimeState::Failed(_))
        {
            note_runtime_lost(&mut state, epoch);
            state.runtime_epoch = Some(RuntimeEpoch::new(js_current_runtime_epoch()));
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

/// The generated client's transport: each encoded request goes through `js_call` to
/// the worker, tagged with the job that owns its lane.
/// A unit struct, because the worker holds all session state.
pub(super) struct WorkerTransport;

impl pb::BridgeTransport for WorkerTransport {
    async fn unary(
        &self,
        operation: pb::Operation,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<Vec<u8>, TransportError> {
        let job = lane_owner(operation, method)?;
        let reply = js_call(method, request, job.id().get())
            .await
            .map_err(|err| TransportError::new(method, js_value_to_string(err)))?;
        object_bytes_field(&reply, "result")
            .map_err(|err| TransportError::new(method, err.to_string()))
    }

    async fn unary_with_buffer(
        &self,
        operation: pb::Operation,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<(Vec<u8>, Vec<f32>), TransportError> {
        let job = lane_owner(operation, method)?;
        let reply = js_call(method, request, job.id().get())
            .await
            .map_err(|err| TransportError::new(method, js_value_to_string(err)))?;
        let marshal_error = |err: MarshalError| TransportError::new(method, err.to_string());
        Ok((
            object_bytes_field(&reply, "result").map_err(marshal_error)?,
            object_f32_field(&reply, "ys").map_err(marshal_error)?,
        ))
    }
}

/// The typed `BridgeService` client over the worker.
pub(super) fn client() -> pb::BridgeServiceClient<WorkerTransport> {
    pb::BridgeServiceClient::new(WorkerTransport)
}
