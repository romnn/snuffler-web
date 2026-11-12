//! Live `SeedLink` acquisition: the user's parameters, the session the frontend
//! follows, and the ingest state machine the wasm client drives.

use std::collections::VecDeque;

use crate::activity::JobHandle;

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

/// What every callback of one socket needs to know about the stream it feeds.
#[derive(Clone, Debug)]
pub(super) struct LiveStream {
    /// The `host:port NET.STA selectors` label the pile shows as the source.
    pub(super) source: String,
    pub(super) started_at: f64,
    pub(super) stream: StreamId,
    pub(super) session: LiveSessionEpoch,
    pub(super) retain_s: f64,
}

/// Identity of one socket's packet stream, echoed back by every ingest acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StreamId(pub(super) u64);

/// Identity of one live session.
/// Retiring a session bumps it, which fences every callback still in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LiveSessionEpoch(pub(super) u64);

/// Where the ingest of one live stream stands.
///
/// Batches are numbered from 0 per stream; `next_seq` is the batch the session
/// expects next, so a gap or a reordering is detected rather than ingested.
#[derive(Clone, Debug, Default)]
pub(super) enum LiveIngestState {
    /// No stream is live.
    #[default]
    Closed,
    /// The socket is open and batch 0 has not been sent yet.
    Opening {
        stream: StreamId,
        session: LiveSessionEpoch,
    },
    /// Batch 0, which resets the Python pile, is in flight.
    ResetInFlight {
        stream: StreamId,
        session: LiveSessionEpoch,
    },
    /// Batches are being acknowledged in order.
    Streaming {
        stream: StreamId,
        session: LiveSessionEpoch,
        next_seq: u64,
    },
    /// The mailbox overflowed and dropped batches; the next acknowledgement resumes.
    Lagged {
        stream: StreamId,
        session: LiveSessionEpoch,
        next_seq: u64,
        dropped_bytes: u64,
    },
}

impl LiveIngestState {
    /// Whether this state belongs to `stream` in `session` and is still open.
    pub(super) fn is_current(&self, stream: StreamId, session: LiveSessionEpoch) -> bool {
        match self {
            Self::Opening {
                stream: current_stream,
                session: current_session,
            }
            | Self::ResetInFlight {
                stream: current_stream,
                session: current_session,
            }
            | Self::Streaming {
                stream: current_stream,
                session: current_session,
                ..
            }
            | Self::Lagged {
                stream: current_stream,
                session: current_session,
                ..
            } => *current_stream == stream && *current_session == session,
            Self::Closed => false,
        }
    }
}

/// One socket message's worth of `miniSEED` records, waiting for the ingest in flight.
#[derive(Clone, Debug)]
pub(super) struct LiveIngestBatch {
    pub(super) source: String,
    pub(super) content: Vec<u8>,
    pub(super) seq: u64,
    /// Browser time the batch arrived, in epoch seconds.
    pub(super) received_s: f64,
}

/// Batches that arrived while an ingest was in flight, coalesced into the next one.
#[derive(Clone, Debug, Default)]
pub(super) struct LiveIngestMailbox {
    pub(super) batches: VecDeque<LiveIngestBatch>,
    /// Total content bytes of `batches`.
    pub(super) bytes: usize,
    /// Bytes dropped since the last acknowledgement drained the mailbox.
    pub(super) dropped_bytes: u64,
}

/// The bridge's live-acquisition state.
#[derive(Default)]
pub(super) struct LiveState {
    /// When the user started the session that is streaming, in epoch seconds.
    pub(super) started_at: Option<f64>,
    /// Follow-viewport width from the start form, in seconds.
    /// 0.0 while no session runs; readers fall back to [`LIVE_FOLLOW_SECONDS`].
    pub(super) follow_s: f64,
    /// The current [`LiveSessionEpoch`] value.
    pub(super) session_epoch: u64,
    /// The most recently minted [`StreamId`] value.
    pub(super) last_stream_id: u64,
    pub(super) ingest: LiveIngestState,
    pub(super) ingest_job: Option<JobHandle>,
    pub(super) mailbox: LiveIngestMailbox,
}

impl LiveState {
    /// Ends the session's state; the socket itself is retired by the caller.
    /// Bumping the session epoch fences any callback still in flight.
    pub(super) fn retire(&mut self) {
        self.session_epoch = self.session_epoch.wrapping_add(1).max(1);
        self.ingest = LiveIngestState::Closed;
        self.mailbox = LiveIngestMailbox::default();
        self.ingest_job = None;
        self.started_at = None;
    }

    /// The batch the socket must deliver next, past those in flight and those queued.
    pub(super) fn expected_seq(&self) -> u64 {
        self.mailbox.batches.back().map_or_else(
            || match self.ingest {
                LiveIngestState::ResetInFlight { .. } => 1,
                LiveIngestState::Streaming { next_seq, .. }
                | LiveIngestState::Lagged { next_seq, .. } => next_seq,
                LiveIngestState::Opening { .. } | LiveIngestState::Closed => 0,
            },
            |queued| queued.seq.wrapping_add(1),
        )
    }

    /// Records that batches `first..=last` went to Python, so the socket's next batch
    /// is the one after `last`.
    ///
    /// Batches that arrive meanwhile are checked against this, so leaving `next_seq` at
    /// `first` would close the stream on the very batch it should queue.
    pub(super) fn mark_in_flight(
        &mut self,
        stream: StreamId,
        session: LiveSessionEpoch,
        first: u64,
        last: u64,
    ) {
        match &mut self.ingest {
            _ if first == 0 => {
                self.ingest = LiveIngestState::ResetInFlight { stream, session };
            }
            LiveIngestState::Streaming { next_seq, .. }
            | LiveIngestState::Lagged { next_seq, .. } => {
                *next_seq = (*next_seq).max(last.wrapping_add(1));
            }
            LiveIngestState::Opening { .. }
            | LiveIngestState::ResetInFlight { .. }
            | LiveIngestState::Closed => {}
        }
    }
}
