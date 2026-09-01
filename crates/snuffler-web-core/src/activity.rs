//! Typed lifecycle state for Python and local work.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::task::Waker;

use crate::bridge::BridgeError;

pub use snuffler_web_proto::bridge::v1::{
    ActivityMeta, CancellationCapability, Lane, Operation, Policy,
};

const VIEW_FEEDBACK_DELAY_S: f64 = 0.5;

/// Identity minted by [`ActivityLedger`] for one visible unit of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(u64);

impl JobId {
    /// Returns the monotonic value used to order ledger entries.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one Pyodide worker instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RuntimeEpoch(u32);

impl RuntimeEpoch {
    /// Creates the identity reported by the transport.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the value carried by transport messages.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Cloneable reference to lifecycle state owned by an [`ActivityLedger`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobHandle {
    id: JobId,
}

impl JobHandle {
    /// Returns this job's ledger identity.
    #[must_use]
    pub const fn id(self) -> JobId {
        self.id
    }
}

/// User-facing progress for a running job.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    /// The operation has not reported a measurable unit.
    Indeterminate,
    /// Discrete completed units and an optional known total.
    Count {
        /// Units completed so far.
        done: u64,
        /// Total units, when known.
        total: Option<u64>,
        /// Current operation phase.
        phase: Arc<str>,
    },
    /// A normalized completion fraction.
    Fraction {
        /// Completion in the inclusive range `0.0..=1.0`.
        fraction: f32,
        /// Current operation phase.
        phase: Arc<str>,
    },
}

/// Non-ready runtime state suitable for a submission rejection.
#[derive(Clone, Debug, thiserror::Error)]
pub enum RuntimeFault {
    /// A replacement worker is starting.
    #[error("Python is restarting")]
    Restarting,
    /// Worker creation or transport failed.
    #[error("Python runtime failed: {0}")]
    Failed(BridgeError),
}

/// Health of the Python runtime, independent of individual job failures.
#[derive(Clone, Debug)]
pub enum RuntimeState {
    /// Pyodide is starting and has not accepted work yet.
    Booting,
    /// The worker can accept work.
    Ready,
    /// The previous worker died and a replacement is starting.
    Restarting,
    /// The runtime itself failed.
    Failed(BridgeError),
}

impl RuntimeState {
    fn submission_fault(&self) -> Option<RuntimeFault> {
        match self {
            Self::Booting | Self::Ready => None,
            Self::Restarting => Some(RuntimeFault::Restarting),
            Self::Failed(error) => Some(RuntimeFault::Failed(error.clone())),
        }
    }
}

/// Why a submission was rejected synchronously.
#[derive(Clone, Debug, thiserror::Error)]
pub enum RejectReason {
    /// Python is unavailable for the reason carried here.
    #[error("runtime unavailable: {0}")]
    RuntimeUnavailable(RuntimeFault),
    /// The bounded activity queue has no remaining capacity.
    #[error("activity queue is full at depth {depth}")]
    QueueFull {
        /// Queue depth at rejection time.
        depth: usize,
    },
    /// A local executor was asked to run a Python-lane operation.
    #[error("operation {operation:?} is not a local job")]
    WrongLane {
        /// Misclassified operation.
        operation: Operation,
    },
}

/// Failure attached to one job rather than to runtime health.
#[derive(Clone, Debug, thiserror::Error)]
pub enum JobError {
    /// Admission rejected the submission.
    #[error("submission rejected: {0}")]
    Rejected(RejectReason),
    /// The worker disappeared before this queued job dispatched.
    #[error("Python restarted before this job dispatched")]
    RuntimeLost,
    /// A dispatched bridge call failed.
    #[error(transparent)]
    Bridge(BridgeError),
}

/// Terminal result of one job.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// Work completed and its result was applied.
    Succeeded,
    /// Work failed without changing runtime health.
    Failed(JobError),
    /// Cancellation won before commit.
    Cancelled,
    /// Cancellation lost to a commit already in progress.
    TooLate,
    /// A composite flow stopped after committing an observable prefix.
    Stopped {
        /// Children committed before cancellation took effect.
        committed: u32,
        /// Children not attempted after cancellation took effect.
        skipped: u32,
    },
    /// The worker died after this job was dispatched.
    Lost(RuntimeEpoch),
    /// The defensive session-generation check rejected the result.
    Stale,
}

/// Immediate result of asking the ledger to cancel one visible job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelRequest {
    /// Queued work was removed and is already terminal.
    Cancelled,
    /// The active transport request must be cancelled.
    Interrupt,
    /// An earlier request is already cancelling the same work.
    AlreadyCancelling,
    /// The job has already become terminal.
    TooLate,
    /// No ledger entry has this identity.
    NotFound,
    /// Running work declared no safe cancellation mechanism.
    NotCancellable,
}

/// Status of a cancellation request while the job remains nonterminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationStatus {
    /// The worker has not yet resolved the cancellation request.
    Pending,
    /// The request arrived after the worker entered its commit phase.
    TooLate,
}

/// Current lifecycle state of one ledger entry.
#[derive(Clone, Debug)]
pub enum JobState {
    /// Accepted but not dispatched.
    Queued,
    /// Sent to a worker but not yet observed executing.
    Dispatching,
    /// Observed running in Python or a local executor.
    Running {
        /// Start time supplied by the executor, in seconds.
        since_s: f64,
        /// Latest progress snapshot.
        progress: Progress,
    },
    /// Cancellation has been requested.
    Cancelling {
        /// Cancellation-request time supplied by the caller, in seconds.
        since_s: f64,
        /// Status of the cancellation-versus-commit race.
        status: CancellationStatus,
    },
    /// One authoritative terminal result has arrived.
    Done(Outcome),
}

impl JobState {
    /// Reports whether this state carries the job's authoritative outcome.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Done(_))
    }
}

/// Result of synchronous admission into the activity ledger.
#[must_use]
#[derive(Clone, Debug)]
pub enum Submission {
    /// New work was accepted and is queued or dispatching.
    Accepted(JobHandle),
    /// Identical deduplicated work already exists.
    Joined(JobHandle),
    /// The attempt is visible as an already-failed ledger entry.
    Rejected(JobHandle, RejectReason),
}

impl Submission {
    /// Returns the handle for accepted, joined, and rejected submissions.
    #[must_use]
    pub const fn handle(&self) -> JobHandle {
        match self {
            Self::Accepted(handle) | Self::Joined(handle) | Self::Rejected(handle, _) => *handle,
        }
    }
}

/// One immutable submission description plus its mutable lifecycle state.
#[derive(Clone, Debug)]
pub struct JobRecord {
    operation: Operation,
    detail: Option<String>,
    dedup_key: Vec<u8>,
    submitted_at_s: f64,
    state: JobState,
}

impl JobRecord {
    /// Operation whose generated metadata describes this job.
    #[must_use]
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    /// Optional user-facing filename, slug, model, or other detail.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Returns how long a short view refresh should remain hidden from activity feedback.
    #[must_use]
    pub fn feedback_delay(&self, now_s: f64) -> Option<std::time::Duration> {
        if self.operation.activity().lane != Lane::View {
            return None;
        }
        let age_s = (now_s - self.submitted_at_s).max(0.0);
        (age_s < VIEW_FEEDBACK_DELAY_S)
            .then(|| std::time::Duration::from_secs_f64(VIEW_FEEDBACK_DELAY_S - age_s))
    }

    /// Latest lifecycle state.
    #[must_use]
    pub const fn state(&self) -> &JobState {
        &self.state
    }
}

/// Bounded owner of every queued and active Python or local job.
pub struct ActivityLedger {
    runtime: RuntimeState,
    capacity: usize,
    next_id: u64,
    records: HashMap<JobId, JobRecord>,
    queue: VecDeque<JobId>,
    dedup: HashMap<(Operation, Vec<u8>), JobId>,
    active_job: Option<JobId>,
    active_view: Option<JobId>,
    waiters: HashMap<JobId, Waker>,
    /// Bumped on every observable mutation, so a per-frame consumer can reuse
    /// its previous snapshot instead of cloning every record every frame.
    revision: u64,
}

impl Default for ActivityLedger {
    fn default() -> Self {
        Self::new(64)
    }
}

impl ActivityLedger {
    /// Creates a ledger with a bounded number of non-terminal entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            runtime: RuntimeState::Booting,
            capacity,
            next_id: 1,
            records: HashMap::new(),
            queue: VecDeque::new(),
            dedup: HashMap::new(),
            active_job: None,
            active_view: None,
            waiters: HashMap::new(),
            revision: 0,
        }
    }

    /// Returns current runtime health.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeState {
        &self.runtime
    }

    /// The value that changes whenever runtime health or any record changed.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Replaces runtime health without conflating it with job errors.
    pub fn set_runtime(&mut self, runtime: RuntimeState) {
        self.runtime = runtime;
        self.changed();
        self.wake_lane_heads();
    }

    /// Admits one operation synchronously so a second click sees it immediately.
    pub fn submit(
        &mut self,
        operation: Operation,
        detail: Option<String>,
        dedup_key: Vec<u8>,
        now_s: f64,
    ) -> Submission {
        if operation.activity().policy == Policy::Dedup
            && let Some(id) = self.dedup.get(&(operation, dedup_key.clone()))
        {
            return Submission::Joined(JobHandle { id: *id });
        }

        if let Some(fault) = self.runtime.submission_fault() {
            return self.reject(
                operation,
                detail,
                now_s,
                RejectReason::RuntimeUnavailable(fault),
            );
        }
        // Supersession must not run ahead of admission: a rejected submission
        // that had already cancelled its predecessor would leave the user with
        // neither the old work nor the new. The superseded entries do count as
        // freed capacity, since admitting the replacement cancels them.
        let superseded: Vec<JobHandle> = if operation.activity().policy == Policy::LatestWins {
            self.queue
                .iter()
                .filter(|id| {
                    self.records
                        .get(id)
                        .is_some_and(|record| record.operation == operation)
                })
                .map(|id| JobHandle { id: *id })
                .collect()
        } else {
            Vec::new()
        };
        let active = self.active_count().saturating_sub(superseded.len());
        if active >= self.capacity {
            return self.reject(
                operation,
                detail,
                now_s,
                RejectReason::QueueFull { depth: active },
            );
        }
        for handle in superseded {
            self.complete(handle, Outcome::Cancelled);
        }

        let handle = self.insert_record(
            operation,
            detail,
            dedup_key.clone(),
            now_s,
            JobState::Queued,
        );
        self.queue.push_back(handle.id);
        if operation.activity().policy == Policy::Dedup {
            self.dedup.insert((operation, dedup_key), handle.id);
        }
        Submission::Accepted(handle)
    }

    /// Starts executor-owned local work without consuming the Python permit.
    pub fn submit_local(
        &mut self,
        operation: Operation,
        detail: Option<String>,
        now_s: f64,
    ) -> Submission {
        if operation.activity().lane != Lane::Local {
            return self.reject(
                operation,
                detail,
                now_s,
                RejectReason::WrongLane { operation },
            );
        }
        let active = self.active_count();
        if active >= self.capacity {
            return self.reject(
                operation,
                detail,
                now_s,
                RejectReason::QueueFull { depth: active },
            );
        }
        let handle = self.insert_record(
            operation,
            detail,
            Vec::new(),
            now_s,
            JobState::Running {
                since_s: now_s,
                progress: Progress::Indeterminate,
            },
        );
        Submission::Accepted(handle)
    }

    /// Returns a job record while retaining ledger ownership of its state.
    #[must_use]
    pub fn get(&self, handle: JobHandle) -> Option<&JobRecord> {
        self.records.get(&handle.id)
    }

    /// Reports whether the sole terminal outcome has already been recorded.
    /// An absent record was pruned, and pruning only removes terminal records,
    /// so absence answers true — otherwise a dispatch future polled after
    /// pruning would park forever with no waker registered.
    #[must_use]
    pub fn is_terminal(&self, handle: JobHandle) -> bool {
        self.records
            .get(&handle.id)
            .is_none_or(|record| record.state.is_terminal())
    }

    /// Visits ledger entries without exposing mutation of their lifecycle state.
    pub fn records(&self) -> impl Iterator<Item = (JobHandle, &JobRecord)> {
        self.records
            .iter()
            .map(|(id, record)| (JobHandle { id: *id }, record))
    }

    /// Number of accepted jobs waiting for their lane permit.
    #[must_use]
    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    /// Attempts to move the first queued job in its lane onto that lane's permit.
    pub fn try_dispatch(&mut self, handle: JobHandle, waker: &Waker) -> bool {
        let Some(record) = self.records.get(&handle.id) else {
            return false;
        };
        let lane = record.operation.activity().lane;
        let active = match lane {
            Lane::View => self.active_view,
            Lane::Job => self.active_job,
            Lane::Local => None,
        };
        let other_active = match lane {
            Lane::View => self.active_job,
            Lane::Job => self.active_view,
            Lane::Local => None,
        };
        let cross_lane_exclusive = record.operation.activity().exclusive_across_lanes
            || other_active
                .and_then(|id| self.records.get(&id))
                .is_some_and(|active| active.operation.activity().exclusive_across_lanes);
        let position = self.queue.iter().position(|id| {
            self.records
                .get(id)
                .is_some_and(|queued| queued.operation.activity().lane == lane)
        });
        if !matches!(self.runtime, RuntimeState::Ready)
            || active.is_some()
            || (cross_lane_exclusive && other_active.is_some())
            || position.and_then(|index| self.queue.get(index).copied()) != Some(handle.id)
        {
            self.waiters.insert(handle.id, waker.clone());
            return false;
        }
        if let Some(position) = position {
            self.queue.remove(position);
        }
        self.waiters.remove(&handle.id);
        match lane {
            Lane::View => self.active_view = Some(handle.id),
            Lane::Job => self.active_job = Some(handle.id),
            Lane::Local => {}
        }
        if let Some(record) = self.records.get_mut(&handle.id) {
            record.state = JobState::Dispatching;
        }
        self.changed();
        true
    }

    /// Marks a dispatched request as executing with indeterminate progress.
    pub fn mark_running(&mut self, handle: JobHandle, now_s: f64) {
        if let Some(record) = self.records.get_mut(&handle.id) {
            let active = match record.operation.activity().lane {
                Lane::View => self.active_view,
                Lane::Job => self.active_job,
                Lane::Local => None,
            };
            if active != Some(handle.id) {
                return;
            }
            // Only a dispatched request may start running: a started event
            // arriving after a cancel must not revert Cancelling (that would
            // disarm mark_cancel_too_late and lose the TooLate outcome), and a
            // duplicate event must not reset recorded progress.
            if !matches!(record.state, JobState::Dispatching) {
                return;
            }
            record.state = JobState::Running {
                since_s: now_s,
                progress: Progress::Indeterminate,
            };
            self.changed();
        }
    }

    /// Marks the job named by a worker event as executing.
    pub fn mark_wire_running(&mut self, job_id: u64, now_s: f64) {
        self.mark_running(JobHandle { id: JobId(job_id) }, now_s);
    }

    /// Replaces progress only while the referenced job is running.
    pub fn update_progress(&mut self, handle: JobHandle, progress: Progress) {
        if let Some(record) = self.records.get_mut(&handle.id)
            && let JobState::Running {
                progress: current, ..
            } = &mut record.state
        {
            *current = progress;
            self.changed();
        }
    }

    /// Applies a transport event to the job identity serialized on the wire.
    pub fn update_wire_progress(&mut self, job_id: u64, progress: Progress) {
        self.update_progress(JobHandle { id: JobId(job_id) }, progress);
    }

    /// Applies the sole terminal outcome and releases the next queued job.
    pub fn complete(&mut self, handle: JobHandle, outcome: Outcome) {
        let Some(record) = self.records.get_mut(&handle.id) else {
            return;
        };
        if record.state.is_terminal() {
            return;
        }
        let cancellation_lost = matches!(
            record.state,
            JobState::Cancelling {
                status: CancellationStatus::TooLate,
                ..
            }
        );
        let outcome = if cancellation_lost && matches!(outcome, Outcome::Succeeded) {
            Outcome::TooLate
        } else {
            outcome
        };
        record.state = JobState::Done(outcome);
        let dedup_key = std::mem::take(&mut record.dedup_key);
        self.dedup.remove(&(record.operation, dedup_key));
        self.changed();
        self.queue.retain(|id| *id != handle.id);
        self.waiters.remove(&handle.id);
        if self.active_job == Some(handle.id) {
            self.active_job = None;
        }
        if self.active_view == Some(handle.id) {
            self.active_view = None;
        }
        self.wake_lane_heads();
    }

    /// Cancels queued work immediately or marks an active cancellable job.
    pub fn request_cancel(&mut self, handle: JobHandle, now_s: f64) -> CancelRequest {
        let Some(record) = self.records.get(&handle.id) else {
            return CancelRequest::NotFound;
        };
        match &record.state {
            JobState::Queued => {
                self.complete(handle, Outcome::Cancelled);
                CancelRequest::Cancelled
            }
            JobState::Dispatching | JobState::Running { .. } => {
                if record.operation.activity().lane == Lane::Local
                    || record.operation.activity().cancellation
                        == CancellationCapability::NotSupported
                {
                    return CancelRequest::NotCancellable;
                }
                if let Some(record) = self.records.get_mut(&handle.id) {
                    record.state = JobState::Cancelling {
                        since_s: now_s,
                        status: CancellationStatus::Pending,
                    };
                }
                self.changed();
                CancelRequest::Interrupt
            }
            JobState::Cancelling { .. } => CancelRequest::AlreadyCancelling,
            JobState::Done(_) => CancelRequest::TooLate,
        }
    }

    /// Records that cancellation lost the transport's commit CAS.
    pub fn mark_cancel_too_late(&mut self, handle: JobHandle) {
        if let Some(record) = self.records.get_mut(&handle.id)
            && let JobState::Cancelling { status, .. } = &mut record.state
        {
            *status = CancellationStatus::TooLate;
            self.changed();
        }
    }

    /// Terminates all work owned by a dead worker without replaying queued intent.
    pub fn runtime_lost(&mut self, epoch: RuntimeEpoch) {
        for active in [self.active_job.take(), self.active_view.take()]
            .into_iter()
            .flatten()
        {
            if let Some(record) = self.records.get_mut(&active)
                && !record.state.is_terminal()
            {
                record.state = JobState::Done(Outcome::Lost(epoch));
            }
        }
        for id in self.queue.drain(..) {
            if let Some(record) = self.records.get_mut(&id) {
                record.state = JobState::Done(Outcome::Failed(JobError::RuntimeLost));
            }
        }
        self.dedup.clear();
        let waiters = std::mem::take(&mut self.waiters);
        for waker in waiters.into_values() {
            waker.wake();
        }
        self.runtime = RuntimeState::Restarting;
        self.changed();
    }

    fn reject(
        &mut self,
        operation: Operation,
        detail: Option<String>,
        now_s: f64,
        reason: RejectReason,
    ) -> Submission {
        let handle = self.insert_record(
            operation,
            detail,
            Vec::new(),
            now_s,
            JobState::Done(Outcome::Failed(JobError::Rejected(reason.clone()))),
        );
        Submission::Rejected(handle, reason)
    }

    fn insert_record(
        &mut self,
        operation: Operation,
        detail: Option<String>,
        dedup_key: Vec<u8>,
        submitted_at_s: f64,
        state: JobState,
    ) -> JobHandle {
        self.prune_history();
        self.changed();
        let id = JobId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.records.insert(
            id,
            JobRecord {
                operation,
                detail,
                dedup_key,
                submitted_at_s,
                state,
            },
        );
        JobHandle { id }
    }

    fn prune_history(&mut self) {
        let history_limit = self.capacity.saturating_mul(4).max(16);
        while self.records.len() >= history_limit {
            let oldest_terminal = self
                .records
                .iter()
                .filter(|(_, record)| record.state.is_terminal())
                .min_by_key(|(id, _)| id.0)
                .map(|(id, _)| *id);
            let Some(id) = oldest_terminal else {
                return;
            };
            self.records.remove(&id);
        }
    }

    fn active_count(&self) -> usize {
        self.records
            .values()
            .filter(|record| !record.state.is_terminal())
            .count()
    }

    fn wake_lane_heads(&self) {
        for lane in [Lane::Job, Lane::View] {
            if let Some(waker) = self
                .queue
                .iter()
                .find(|id| {
                    self.records
                        .get(id)
                        .is_some_and(|record| record.operation.activity().lane == lane)
                })
                .and_then(|id| self.waiters.get(id))
            {
                waker.wake_by_ref();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::{
        ActivityLedger, CancelRequest, JobError, JobState, Operation, Outcome, RuntimeEpoch,
        RuntimeState, Submission,
    };

    #[test]
    fn duplicate_submissions_join_before_any_async_work_can_start() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let first = ledger.submit(Operation::RenderFigure, None, b"spectrum".to_vec(), 1.0);
        let second = ledger.submit(Operation::RenderFigure, None, b"spectrum".to_vec(), 1.0);
        assert!(matches!(first, Submission::Accepted(_)));
        assert!(matches!(second, Submission::Joined(_)));
        assert_eq!(first.handle(), second.handle());
    }

    #[test]
    fn admission_rejection_is_still_a_visible_terminal_entry() {
        let mut ledger = ActivityLedger::new(0);
        ledger.set_runtime(RuntimeState::Ready);
        let rejected = ledger.submit(Operation::RunSnuffling, None, Vec::new(), 1.0);
        assert!(matches!(rejected, Submission::Rejected(_, _)));
        assert!(matches!(
            ledger.get(rejected.handle()).map(super::JobRecord::state),
            Some(JobState::Done(_))
        ));
    }

    #[test]
    fn booting_jobs_wait_and_the_job_lane_dispatches_one_at_a_time() {
        let mut ledger = ActivityLedger::new(4);
        let first = ledger
            .submit(Operation::RunCommand, None, b"first".to_vec(), 1.0)
            .handle();
        let second = ledger
            .submit(Operation::LoadEvents, None, b"second".to_vec(), 1.0)
            .handle();
        assert!(!ledger.try_dispatch(first, Waker::noop()));
        ledger.set_runtime(RuntimeState::Ready);
        assert!(ledger.try_dispatch(first, Waker::noop()));
        assert!(!ledger.try_dispatch(second, Waker::noop()));
        ledger.complete(first, Outcome::Succeeded);
        assert!(ledger.try_dispatch(second, Waker::noop()));
    }

    #[test]
    fn runtime_loss_never_replays_queued_intent() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let active = ledger
            .submit(Operation::RunSnuffling, None, b"active".to_vec(), 1.0)
            .handle();
        let queued = ledger
            .submit(Operation::LoadEvents, None, b"queued".to_vec(), 1.0)
            .handle();
        let epoch = RuntimeEpoch::new(3);
        assert!(ledger.try_dispatch(active, Waker::noop()));
        ledger.runtime_lost(epoch);
        assert!(matches!(
            ledger.get(active).map(super::JobRecord::state),
            Some(JobState::Done(Outcome::Lost(lost))) if *lost == epoch
        ));
        assert!(matches!(
            ledger.get(queued).map(super::JobRecord::state),
            Some(JobState::Done(Outcome::Failed(JobError::RuntimeLost)))
        ));
    }

    #[test]
    fn view_and_job_lanes_can_each_hold_one_permit() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let job = ledger
            .submit(Operation::RunSnuffling, None, b"job".to_vec(), 1.0)
            .handle();
        let view = ledger
            .submit(Operation::GetView, None, b"view".to_vec(), 1.0)
            .handle();
        assert!(ledger.try_dispatch(job, Waker::noop()));
        assert!(ledger.try_dispatch(view, Waker::noop()));
    }

    /// Short view refreshes stay silent, while ordinary work is visible immediately.
    #[test]
    fn view_feedback_has_a_half_second_grace_period() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let view = ledger
            .submit(Operation::GetView, None, Vec::new(), 10.0)
            .handle();
        let job = ledger
            .submit(Operation::RunCommand, None, Vec::new(), 10.0)
            .handle();

        let view_record = ledger.get(view);
        assert!(view_record.is_some_and(|record| record.feedback_delay(10.499).is_some()));
        assert!(view_record.is_some_and(|record| record.feedback_delay(10.5).is_none()));
        assert!(
            ledger
                .get(job)
                .is_some_and(|record| record.feedback_delay(10.0).is_none())
        );
    }

    /// Only the worker's started event advances a dispatched request to running.
    #[test]
    fn worker_started_event_is_authoritative() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let job = ledger
            .submit(Operation::RunSnuffling, None, Vec::new(), 1.0)
            .handle();
        assert!(ledger.try_dispatch(job, Waker::noop()));
        assert!(matches!(
            ledger.get(job).map(super::JobRecord::state),
            Some(JobState::Dispatching)
        ));

        ledger.mark_wire_running(job.id().get(), 2.0);
        assert!(matches!(
            ledger.get(job).map(super::JobRecord::state),
            Some(JobState::Running { .. })
        ));
    }

    #[test]
    fn worker_reload_excludes_the_view_lane() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let reload = ledger
            .submit(Operation::ReloadSnufflings, None, Vec::new(), 1.0)
            .handle();
        let view = ledger
            .submit(Operation::GetView, None, Vec::new(), 1.0)
            .handle();
        assert!(ledger.try_dispatch(reload, Waker::noop()));
        assert!(!ledger.try_dispatch(view, Waker::noop()));
        ledger.complete(reload, Outcome::Succeeded);
        assert!(ledger.try_dispatch(view, Waker::noop()));
    }

    #[test]
    fn cancelling_queued_work_is_immediate() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let active = ledger
            .submit(Operation::RunSnuffling, None, b"active".to_vec(), 1.0)
            .handle();
        let queued = ledger
            .submit(Operation::LoadEvents, None, b"queued".to_vec(), 1.0)
            .handle();
        assert!(ledger.try_dispatch(active, Waker::noop()));

        assert_eq!(ledger.request_cancel(queued, 2.0), CancelRequest::Cancelled);
        assert!(matches!(
            ledger.get(queued).map(super::JobRecord::state),
            Some(JobState::Done(Outcome::Cancelled))
        ));
        assert_eq!(ledger.queued_count(), 0);
    }

    #[test]
    fn cancellation_losing_the_commit_race_has_one_too_late_terminal_outcome() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let job = ledger
            .submit(Operation::RunSnuffling, None, b"job".to_vec(), 1.0)
            .handle();
        assert!(ledger.try_dispatch(job, Waker::noop()));
        ledger.mark_running(job, 2.0);
        assert_eq!(ledger.request_cancel(job, 3.0), CancelRequest::Interrupt);
        ledger.mark_cancel_too_late(job);
        assert!(matches!(
            ledger.get(job).map(super::JobRecord::state),
            Some(JobState::Cancelling {
                status: super::CancellationStatus::TooLate,
                ..
            })
        ));
        ledger.complete(job, Outcome::Succeeded);
        ledger.complete(job, Outcome::Cancelled);

        assert!(matches!(
            ledger.get(job).map(super::JobRecord::state),
            Some(JobState::Done(Outcome::TooLate))
        ));
    }

    /// A started event arriving after a cancel must not revert Cancelling:
    /// that would disarm `mark_cancel_too_late`, and a commit-race loser would
    /// record `Succeeded` instead of `TooLate`.
    #[test]
    fn a_started_event_does_not_resurrect_a_cancelling_job() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let job = ledger
            .submit(Operation::RunSnuffling, None, b"job".to_vec(), 1.0)
            .handle();
        assert!(ledger.try_dispatch(job, Waker::noop()));
        assert_eq!(ledger.request_cancel(job, 2.0), CancelRequest::Interrupt);

        ledger.mark_wire_running(job.id().get(), 3.0);
        assert!(matches!(
            ledger.get(job).map(super::JobRecord::state),
            Some(JobState::Cancelling { .. })
        ));

        ledger.mark_cancel_too_late(job);
        ledger.complete(job, Outcome::Succeeded);
        assert!(matches!(
            ledger.get(job).map(super::JobRecord::state),
            Some(JobState::Done(Outcome::TooLate))
        ));
    }

    /// A rejected replacement must not cancel the queued work it failed to
    /// replace — supersession is a property of admitted submissions only.
    #[test]
    fn a_rejected_replacement_leaves_the_superseded_queue_entry_alone() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let queued = ledger
            .submit(Operation::GetView, None, b"first".to_vec(), 1.0)
            .handle();
        ledger.set_runtime(RuntimeState::Restarting);

        let replacement = ledger.submit(Operation::GetView, None, b"second".to_vec(), 2.0);
        assert!(matches!(replacement, Submission::Rejected(_, _)));
        assert!(!ledger.is_terminal(queued));
        assert_eq!(ledger.queued_count(), 1);
    }

    /// Pruning only removes terminal records, so an absent record answers
    /// terminal — a dispatch future polled after pruning must not park forever.
    #[test]
    fn a_pruned_record_still_reports_terminal() {
        let mut ledger = ActivityLedger::new(4);
        ledger.set_runtime(RuntimeState::Ready);
        let job = ledger
            .submit(Operation::RunCommand, None, b"first".to_vec(), 1.0)
            .handle();
        ledger.complete(job, Outcome::Succeeded);
        for round in 0_u64..32 {
            let filler = ledger
                .submit(
                    Operation::RunCommand,
                    None,
                    round.to_le_bytes().to_vec(),
                    2.0,
                )
                .handle();
            ledger.complete(filler, Outcome::Succeeded);
        }

        assert!(ledger.get(job).is_none(), "the oldest record should prune");
        assert!(ledger.is_terminal(job));
    }
}
