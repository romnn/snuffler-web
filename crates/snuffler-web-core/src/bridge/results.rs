//! Results and snapshots the bridge hands to the frontend.

use std::rc::Rc;

use crate::activity::{JobHandle, JobRecord, RuntimeState};

use super::{ViewMeta, ViewSpan};

/// A view plus its out-of-band sample buffer.
#[derive(Clone, Debug, Default)]
pub struct ViewData {
    /// Track metadata and time bounds.
    pub meta: ViewMeta,
    /// Uniform samples or packed waterfall cells.
    pub ys: Vec<f32>,
}

impl ViewData {
    /// Raw samples for one protobuf span.
    #[must_use]
    pub fn samples<'a>(&'a self, span: &ViewSpan) -> &'a [f32] {
        let offset = usize::try_from(span.offset).unwrap_or(usize::MAX);
        let count = usize::try_from(span.count).unwrap_or(usize::MAX);
        let end = offset.saturating_add(count).min(self.ys.len());
        self.ys.get(offset.min(self.ys.len())..end).unwrap_or(&[])
    }
}

/// A rendered pyrocko/matplotlib figure.
#[derive(Clone, Debug, Default)]
pub struct FigureData {
    /// Encoded PNG.
    pub png: Vec<u8>,
    /// Render failure text.
    pub error: String,
}

/// The latest render of one figure window, plus the job that will replace it.
#[derive(Clone, Debug, Default)]
pub(super) struct FigureSlot {
    pub(super) data: FigureData,
    /// Bumped on every render, so the frontend decodes the PNG only when it changed.
    pub(super) generation: u64,
    /// The pending render the window shows a spinner for.
    pub(super) job: Option<JobHandle>,
}

/// How a Python command line completed.
#[derive(Clone, Debug)]
pub enum CommandOutcome {
    /// The command and any requested view change succeeded.
    Succeeded,
    /// The command was rejected with user-facing detail.
    Failed(String),
}

/// Where a `goto` command asked the view to jump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GotoTarget {
    /// The time to show, in epoch seconds.
    pub time: f64,
    /// How much to show, when the command named a date precise enough to imply one.
    pub length_s: Option<f64>,
}

/// Where `SeisBench` inference runs, and how far the running picker has got.
#[derive(Clone, Debug)]
pub struct SeisbenchStatus {
    /// The inference device, as the panel names it ("WebGPU" or "CPU (wasm)").
    pub device: String,
    /// Progress of the running picker, or `None` while none runs.
    pub progress: Option<SeisbenchProgress>,
}

/// Progress reported by the in-browser `SeisBench` runtime.
#[derive(Clone, Debug)]
pub struct SeisbenchProgress {
    /// Completed fraction.
    pub fraction: f32,
    /// Current inference phase.
    pub label: String,
}

/// Runtime health plus every visible job, as the per-frame activity readout draws it.
#[derive(Clone, Debug)]
pub struct ActivitySnapshot {
    /// Health of the Python runtime.
    pub runtime: RuntimeState,
    /// Every ledger record, shared so idle frames reuse one allocation.
    pub jobs: Rc<Vec<(JobHandle, JobRecord)>>,
}
