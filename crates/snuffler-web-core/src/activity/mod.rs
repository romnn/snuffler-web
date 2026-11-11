//! Typed lifecycle state for Python and local work, owned by the [`ActivityLedger`].
//!
//! The ledger is the only queue of Python work (adr/0006): it admits, deduplicates,
//! dispatches, cancels and settles every job.
//! This module holds the vocabulary the frontend reads; `ledger` holds the rules.

use std::sync::Arc;

use crate::bridge::BridgeError;

mod ledger;

pub use ledger::ActivityLedger;
pub use snuffler_web_proto::bridge::v1::{
    ActivityMeta, CancellationCapability, Lane, Operation, Policy,
};

/// How long view work stays out of the activity readout, so ordinary panning shows none.
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
        /// Lower bound on children not attempted after cancellation took
        /// effect — a lazily enumerated flow only knows more pages existed,
        /// not how many entries they held.
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
