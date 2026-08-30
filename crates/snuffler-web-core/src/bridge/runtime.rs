//! Runtime identity and live-ingest state owned beside the worker transport.

/// Visible live waveform window once enough samples have arrived.
pub const LIVE_FOLLOW_SECONDS: f64 = 300.0;
/// Amount of live data kept in the Pyodide trace pile before older samples are discarded.
pub const LIVE_RETAIN_SECONDS: f64 = 1800.0;

/// Metadata for the currently active `SeedLink` acquisition session.
#[derive(Clone, Copy, Debug)]
pub struct LiveSeedlinkSession {
    /// Epoch time when the current live source was started by the user.
    pub started_at: f64,
    /// Width of the live-follow viewport, in seconds.
    pub follow_seconds: f64,
}

/// User-entered parameters for a live `SeedLink` acquisition.
#[derive(Clone, Debug, Default)]
pub struct SeedlinkParams {
    /// `SeedLink` server hostname.
    pub host: String,
    /// `SeedLink` TCP port.
    pub port: String,
    /// Network selector.
    pub net: String,
    /// Station selector.
    pub sta: String,
    /// Channel selector expression.
    pub selectors: String,
    /// Width of the live-follow viewport in seconds.
    pub follow_s: f64,
    /// Seconds of live data to retain before older samples are discarded.
    pub retain_s: f64,
}

/// The Python runtime reported by the live worker.
#[derive(Clone, Debug)]
pub struct RuntimeInfo {
    /// Python version.
    pub python: String,
    /// `NumPy` version.
    pub numpy: String,
    /// Pyrocko version.
    pub pyrocko: String,
    /// Whether the interpreter confirmed pthread support.
    pub threaded: bool,
    /// Browser hardware concurrency, or zero when unavailable.
    pub cores: u32,
}

/// Progress reported by the in-browser `SeisBench` runtime.
#[derive(Clone, Debug)]
pub struct SeisbenchProgress {
    /// Completed fraction.
    pub fraction: f32,
    /// Current inference phase.
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StreamId(pub(super) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SessionEpoch(pub(super) u64);

#[derive(Clone, Debug, Default)]
pub(super) enum LiveIngestState {
    #[default]
    Closed,
    Opening {
        stream: StreamId,
        session: SessionEpoch,
    },
    ResetInFlight {
        stream: StreamId,
        session: SessionEpoch,
    },
    Streaming {
        stream: StreamId,
        session: SessionEpoch,
        next_seq: u64,
    },
    Lagged {
        stream: StreamId,
        session: SessionEpoch,
        next_seq: u64,
        dropped_bytes: u64,
    },
}

#[derive(Clone, Debug)]
pub(super) struct LiveIngestBatch {
    pub(super) source: String,
    pub(super) content: Vec<u8>,
    pub(super) seq: u64,
    pub(super) received_s: f64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct LiveIngestMailbox {
    pub(super) batches: std::collections::VecDeque<LiveIngestBatch>,
    pub(super) bytes: usize,
    pub(super) dropped_seq_first: Option<u64>,
    pub(super) dropped_seq_last: Option<u64>,
    pub(super) dropped_bytes: u64,
}
