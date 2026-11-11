//! The activity ledger: admission, the two Python lanes, cancellation and settlement.

use std::collections::{HashMap, VecDeque};
use std::task::Waker;

use super::{
    CancelRequest, CancellationCapability, CancellationStatus, JobError, JobHandle, JobId,
    JobRecord, JobState, Lane, Operation, Outcome, Policy, Progress, RejectReason, RuntimeEpoch,
    RuntimeState, Submission,
};

/// Bounded owner of every queued and active Python or local job.
pub struct ActivityLedger {
    runtime: RuntimeState,
    capacity: usize,
    next_id: u64,
    records: HashMap<JobId, JobRecord>,
    queue: VecDeque<JobId>,
    /// Queued or running `DEDUP` work, by operation and encoded request.
    dedup_index: HashMap<(Operation, Vec<u8>), JobId>,
    /// The job holding the job lane's permit.
    job_lane_holder: Option<JobId>,
    /// The job holding the view lane's permit.
    view_lane_holder: Option<JobId>,
    /// Dispatch futures parked until their job reaches the head of its lane.
    parked_wakers: HashMap<JobId, Waker>,
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
            dedup_index: HashMap::new(),
            job_lane_holder: None,
            view_lane_holder: None,
            parked_wakers: HashMap::new(),
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

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Replaces runtime health without conflating it with job errors.
    pub fn set_runtime(&mut self, runtime: RuntimeState) {
        self.runtime = runtime;
        self.bump_revision();
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
            && let Some(id) = self.dedup_index.get(&(operation, dedup_key.clone()))
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
        // Supersession must not run ahead of admission: a rejected submission that had
        // already cancelled its predecessor would leave the user with neither the old
        // work nor the new.
        // The superseded entries do count as freed capacity, since admitting the
        // replacement cancels them.
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
        let active = self.nonterminal_count().saturating_sub(superseded.len());
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
            self.dedup_index.insert((operation, dedup_key), handle.id);
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
        let active = self.nonterminal_count();
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

    /// The job holding `lane`'s permit; the local lane has no permit.
    const fn lane_holder(&self, lane: Lane) -> Option<JobId> {
        match lane {
            Lane::Job => self.job_lane_holder,
            Lane::View => self.view_lane_holder,
            Lane::Local => None,
        }
    }

    /// The job holding the other Python lane's permit.
    const fn other_lane_holder(&self, lane: Lane) -> Option<JobId> {
        match lane {
            Lane::Job => self.view_lane_holder,
            Lane::View => self.job_lane_holder,
            Lane::Local => None,
        }
    }

    /// Attempts to move the first queued job in its lane onto that lane's permit.
    ///
    /// A job that cannot dispatch yet parks `waker`, which the ledger wakes once the
    /// job heads its lane and the lane frees up.
    pub fn try_dispatch(&mut self, handle: JobHandle, waker: &Waker) -> bool {
        let Some(record) = self.records.get(&handle.id) else {
            return false;
        };
        let lane = record.operation.activity().lane;
        let other_holder = self.other_lane_holder(lane);
        // An exclusive job (a worker reload) runs with the other lane idle, either way round.
        let cross_lane_exclusive = record.operation.activity().exclusive_across_lanes
            || other_holder
                .and_then(|id| self.records.get(&id))
                .is_some_and(|holder| holder.operation.activity().exclusive_across_lanes);
        let lane_head = self.queue.iter().position(|id| {
            self.records
                .get(id)
                .is_some_and(|queued| queued.operation.activity().lane == lane)
        });

        // Wait
        if !matches!(self.runtime, RuntimeState::Ready)
            || self.lane_holder(lane).is_some()
            || (cross_lane_exclusive && other_holder.is_some())
            || lane_head.and_then(|index| self.queue.get(index).copied()) != Some(handle.id)
        {
            self.parked_wakers.insert(handle.id, waker.clone());
            return false;
        }

        // Take the permit
        if let Some(lane_head) = lane_head {
            self.queue.remove(lane_head);
        }
        self.parked_wakers.remove(&handle.id);
        match lane {
            Lane::Job => self.job_lane_holder = Some(handle.id),
            Lane::View => self.view_lane_holder = Some(handle.id),
            Lane::Local => {}
        }
        if let Some(record) = self.records.get_mut(&handle.id) {
            record.state = JobState::Dispatching;
        }
        self.bump_revision();
        true
    }

    /// Marks a dispatched request as executing with indeterminate progress.
    fn mark_running(&mut self, handle: JobHandle, now_s: f64) {
        let Some(lane) = self
            .records
            .get(&handle.id)
            .map(|record| record.operation.activity().lane)
        else {
            return;
        };
        if self.lane_holder(lane) != Some(handle.id) {
            return;
        }
        let Some(record) = self.records.get_mut(&handle.id) else {
            return;
        };
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
        self.bump_revision();
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
            self.bump_revision();
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
        self.dedup_index.remove(&(record.operation, dedup_key));
        self.bump_revision();
        self.queue.retain(|id| *id != handle.id);
        self.parked_wakers.remove(&handle.id);
        if self.job_lane_holder == Some(handle.id) {
            self.job_lane_holder = None;
        }
        if self.view_lane_holder == Some(handle.id) {
            self.view_lane_holder = None;
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
                self.bump_revision();
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
            self.bump_revision();
        }
    }

    /// Terminates all work owned by a dead worker without replaying queued intent.
    pub fn runtime_lost(&mut self, epoch: RuntimeEpoch) {
        for holder in [self.job_lane_holder.take(), self.view_lane_holder.take()]
            .into_iter()
            .flatten()
        {
            if let Some(record) = self.records.get_mut(&holder)
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
        self.dedup_index.clear();
        let parked = std::mem::take(&mut self.parked_wakers);
        for waker in parked.into_values() {
            waker.wake();
        }
        self.runtime = RuntimeState::Restarting;
        self.bump_revision();
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
        self.bump_revision();
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

    /// Drops the oldest terminal records once the history outgrows four times the capacity.
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

    fn nonterminal_count(&self) -> usize {
        self.records
            .values()
            .filter(|record| !record.state.is_terminal())
            .count()
    }

    /// Wakes the parked dispatch future at the head of each Python lane.
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
                .and_then(|id| self.parked_wakers.get(id))
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
        assert_eq!(ledger.queue.len(), 0);
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
        assert_eq!(ledger.queue.len(), 1);
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
