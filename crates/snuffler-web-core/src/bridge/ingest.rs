//! The live ingest state machine: which `SeedLink` batch goes to pyrocko now, which
//! waits in the bounded mailbox, and what an acknowledgement does to the stream.
//!
//! Every batch carries its sequence number, and every acknowledgement echoes the
//! stream, session and sequence range it covers.
//! A gap, a reordering or a stale acknowledgement closes the stream rather than
//! ingesting out of order, and the diagnostic says why.
//!
//! The wasm client in `web::live` only feeds socket batches and worker replies in, and
//! carries out the [`BatchAction`] or [`LiveIngestCompletion`] it gets back.

use super::live::{LiveIngestBatch, LiveIngestMailbox, LiveIngestState, LiveStream};
use super::proto::pb::{self, IngestMiniSeedRequest};
use super::{BridgeError, CallError, Inner, JobHandle, SeedlinkError};
use crate::seedlink::FrameError;

/// Most buffered bytes the mailbox holds before dropping its oldest batches.
const MAILBOX_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Longest a batch waits in the mailbox before it is dropped as too old, in seconds.
const MAILBOX_MAX_RESIDENCE_S: f64 = 10.0;

/// What the client does with a batch the socket delivered.
pub(super) enum BatchAction {
    /// Sends this request to pyrocko; the stream already counts it as in flight.
    Dispatch(IngestMiniSeedRequest),
    /// Nothing: the batch waits in the mailbox, or belongs to a retired stream.
    Held,
    /// The stream closed with a diagnostic, so its socket has to retire.
    Closed,
}

/// What an acknowledged ingest means for the stream.
pub(super) enum LiveIngestCompletion {
    /// The stream was retired while the ingest ran.
    Superseded,
    Accepted {
        /// The coalesced mailbox, to ingest next; the stream already counts it as in
        /// flight.
        next_request: Option<IngestMiniSeedRequest>,
        /// Whether this batch started the session.
        reset_session: bool,
    },
    Failed(BridgeError),
}

impl Inner {
    /// Takes one socket batch: sends it to pyrocko at once, queues it behind the
    /// ingest in flight, or closes the stream when it is not the batch expected next.
    pub(super) fn accept_live_batch(
        &mut self,
        live: &LiveStream,
        content: Vec<u8>,
        seq: u64,
        now_s: f64,
    ) -> BatchAction {
        if !self.live.ingest.is_current(live.stream, live.session) {
            return BatchAction::Held;
        }
        if content.len() > MAILBOX_MAX_BYTES {
            return self.close_live_stream(
                FrameError::Oversized {
                    announced: content.len(),
                    limit: MAILBOX_MAX_BYTES,
                }
                .into(),
            );
        }
        let in_flight = self.live.ingest_job.is_some();
        if !in_flight && matches!(self.live.ingest, LiveIngestState::ResetInFlight { .. }) {
            return BatchAction::Held;
        }
        let expected = self.live.expected_seq();
        if seq != expected {
            return self.close_live_stream(SeedlinkError::OutOfOrder {
                received: seq,
                expected,
            });
        }

        // Queue behind the ingest in flight
        if in_flight {
            self.enqueue_live_batch(live, content, seq, now_s);
            return BatchAction::Held;
        }

        // Ingest now
        self.live
            .mark_in_flight(live.stream, live.session, seq, seq);
        BatchAction::Dispatch(IngestMiniSeedRequest {
            source: live.source.clone(),
            content,
            start_time: live.started_at,
            // Batch 0 replaces the pile with the live stream.
            reset: seq == 0,
            retain_seconds: live.retain_s,
            stream_id: live.stream.0,
            session_epoch: live.session.0,
            batch_seq_first: seq,
            batch_seq_last: seq,
        })
    }

    /// Records the admitted ingest job, so later batches queue behind it.
    pub(super) fn note_live_ingest_admitted(&mut self, job: JobHandle) {
        self.live.ingest_job = Some(job);
    }

    /// Ends the stream with `error` as the visible reason.
    fn close_live_stream(&mut self, error: SeedlinkError) -> BatchAction {
        self.live.retire();
        self.diagnostic = Some(error.into());
        BatchAction::Closed
    }

    /// Adds a batch to the mailbox, dropping the oldest ones past its bounds.
    fn enqueue_live_batch(&mut self, live: &LiveStream, content: Vec<u8>, seq: u64, now_s: f64) {
        let mailbox = &mut self.live.mailbox;
        mailbox.bytes = mailbox.bytes.saturating_add(content.len());
        mailbox.batches.push_back(LiveIngestBatch {
            source: live.source.clone(),
            content,
            seq,
            received_s: now_s,
        });

        // Drop from the front past the size and age bounds
        let mut dropped_now = false;
        while mailbox
            .batches
            .front()
            .is_some_and(|oldest| now_s - oldest.received_s > MAILBOX_MAX_RESIDENCE_S)
            || mailbox.bytes > MAILBOX_MAX_BYTES
        {
            let Some(dropped) = mailbox.batches.pop_front() else {
                break;
            };
            dropped_now = true;
            mailbox.bytes = mailbox.bytes.saturating_sub(dropped.content.len());
            mailbox.dropped_bytes = mailbox
                .dropped_bytes
                .saturating_add(u64::try_from(dropped.content.len()).unwrap_or(u64::MAX));
        }
        if !dropped_now {
            return;
        }

        // The stream goes on with a gap, which the status line keeps showing
        let dropped_bytes = mailbox.dropped_bytes;
        let next_seq = mailbox
            .batches
            .back()
            .map_or(seq.wrapping_add(1), |queued| queued.seq.wrapping_add(1));
        self.live.ingest = LiveIngestState::Lagged {
            stream: live.stream,
            session: live.session,
            next_seq,
            dropped_bytes,
        };
        self.diagnostic = Some(SeedlinkError::Lagged { dropped_bytes }.into());
    }

    /// Drains the mailbox into one request covering every queued batch.
    fn take_mailbox_request(&mut self, live: &LiveStream) -> Option<IngestMiniSeedRequest> {
        let mailbox = &mut self.live.mailbox;
        let (Some(first), Some(last)) = (mailbox.batches.front(), mailbox.batches.back()) else {
            return None;
        };
        let source = first.source.clone();
        let batch_seq_first = first.seq;
        let batch_seq_last = last.seq;
        let mut content = Vec::with_capacity(mailbox.bytes);
        for batch in mailbox.batches.drain(..) {
            content.extend_from_slice(&batch.content);
        }
        *mailbox = LiveIngestMailbox::default();
        Some(IngestMiniSeedRequest {
            source,
            content,
            start_time: live.started_at,
            reset: false,
            retain_seconds: live.retain_s,
            stream_id: live.stream.0,
            session_epoch: live.session.0,
            batch_seq_first,
            batch_seq_last,
        })
    }

    /// Applies one `IngestMiniSeed` reply to the stream state.
    pub(super) fn complete_live_ingest(
        &mut self,
        reply: Result<pb::IngestMiniSeedResult, CallError>,
        job: Option<JobHandle>,
        expected_first: u64,
        expected_last: u64,
        live: &LiveStream,
    ) -> LiveIngestCompletion {
        if self.live.ingest_job == job {
            self.live.ingest_job = None;
        }
        if !self.live.ingest.is_current(live.stream, live.session) {
            return LiveIngestCompletion::Superseded;
        }

        // Check the acknowledgement
        let ack = match reply {
            Ok(ack) => ack,
            Err(error) => {
                // Batch N failed terminally; dispatching the buffered successors would
                // mutate the session after the stream's terminal failure.
                self.live.ingest = LiveIngestState::Closed;
                self.live.mailbox = LiveIngestMailbox::default();
                return LiveIngestCompletion::Failed(error.into());
            }
        };
        if ack.stream_id != live.stream.0
            || ack.session_epoch != live.session.0
            || ack.ack_seq_first != expected_first
            || ack.ack_seq_last != expected_last
        {
            self.live.ingest = LiveIngestState::Closed;
            self.live.mailbox = LiveIngestMailbox::default();
            return LiveIngestCompletion::Failed(BridgeError::browser(
                "acknowledging SeedLink data",
                "stale or out-of-order ingest acknowledgement",
            ));
        }

        // Batch 0 starts the session: drop the previous source's annotations
        let reset_session = expected_first == 0;
        if reset_session {
            self.diagnostic = None;
            self.snuffling = None;
            self.clear_event_state();
            self.live.started_at = Some(live.started_at);
        }

        // Install the grown scene and advance the stream
        let (received_next_seq, dropped_bytes) = match self.live.ingest {
            LiveIngestState::Lagged {
                next_seq,
                dropped_bytes,
                ..
            } => (next_seq, Some(dropped_bytes)),
            _ => (expected_last.wrapping_add(1), None),
        };
        if let Some(dropped_bytes) = dropped_bytes {
            // A loss while batch 0 was in flight must outlive the reset above.
            self.diagnostic = Some(SeedlinkError::Lagged { dropped_bytes }.into());
        }
        self.scene.clone_from(&ack.scene);
        self.scene_generation += 1;
        self.clear_view();
        self.live.ingest = LiveIngestState::Streaming {
            stream: live.stream,
            session: live.session,
            next_seq: received_next_seq.max(expected_last.wrapping_add(1)),
        };
        let next_request = self.take_mailbox_request(live);
        if let Some(request) = &next_request {
            self.live.mark_in_flight(
                live.stream,
                live.session,
                request.batch_seq_first,
                request.batch_seq_last,
            );
        }
        LiveIngestCompletion::Accepted {
            next_request,
            reset_session,
        }
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{self, bail, ensure};

    use super::{BatchAction, Inner, LiveIngestCompletion, MAILBOX_MAX_RESIDENCE_S};
    use crate::activity::{JobHandle, Operation};
    use crate::bridge::live::{LiveSessionEpoch, LiveStream, StreamId};
    use crate::bridge::proto::pb;
    use crate::bridge::{BridgeError, LIVE_FOLLOW_SECONDS, SeedlinkError};

    const STREAM: StreamId = StreamId(1);
    const SESSION: LiveSessionEpoch = LiveSessionEpoch(1);

    fn stream() -> LiveStream {
        LiveStream {
            source: "rtserve.earthscope.org:18000 IU.ANMO BH?".to_owned(),
            started_at: 0.0,
            stream: STREAM,
            session: SESSION,
            retain_s: 1800.0,
        }
    }

    /// A bridge with a freshly started live session.
    fn streaming_inner() -> Inner {
        let mut inner = Inner::default();
        inner.begin_live_session(STREAM, SESSION, LIVE_FOLLOW_SECONDS);
        inner
    }

    /// Any ledger handle; the state machine only compares them.
    fn job(inner: &mut Inner) -> JobHandle {
        inner
            .activity
            .submit(Operation::IngestMiniSeed, None, Vec::new(), 0.0)
            .handle()
    }

    /// Sends one batch the way the socket does and, when it is dispatched, admits its
    /// job the way the client does.
    fn deliver(inner: &mut Inner, seq: u64, now_s: f64) -> eyre::Result<Option<(u64, u64)>> {
        match inner.accept_live_batch(&stream(), vec![0; 512], seq, now_s) {
            BatchAction::Dispatch(request) => {
                let handle = job(inner);
                inner.note_live_ingest_admitted(handle);
                Ok(Some((request.batch_seq_first, request.batch_seq_last)))
            }
            BatchAction::Held => Ok(None),
            BatchAction::Closed => bail!("batch {seq} closed the stream: {:?}", inner.diagnostic),
        }
    }

    /// Acknowledges the ingest in flight as pyrocko does, returning the coalesced range
    /// dispatched next.
    fn acknowledge(inner: &mut Inner, first: u64, last: u64) -> eyre::Result<Option<(u64, u64)>> {
        let reply = Ok(pb::IngestMiniSeedResult {
            scene: None,
            stream_id: STREAM.0,
            session_epoch: SESSION.0,
            ack_seq_first: first,
            ack_seq_last: last,
        });
        let in_flight = inner.live.ingest_job;
        match inner.complete_live_ingest(reply, in_flight, first, last, &stream()) {
            LiveIngestCompletion::Accepted { next_request, .. } => {
                let next =
                    next_request.map(|request| (request.batch_seq_first, request.batch_seq_last));
                if next.is_some() {
                    let handle = job(inner);
                    inner.note_live_ingest_admitted(handle);
                }
                Ok(next)
            }
            LiveIngestCompletion::Superseded => {
                bail!("the ingest of {first}..={last} was superseded")
            }
            LiveIngestCompletion::Failed(error) => {
                bail!("the ingest of {first}..={last} failed: {error}")
            }
        }
    }

    /// Batches that arrive while an earlier one is with pyrocko queue behind it, after
    /// a batch sent on its own and after a coalesced mailbox alike, rather than closing
    /// the stream as a gap.
    #[test]
    fn batches_arriving_during_an_ingest_queue_behind_it() -> eyre::Result<()> {
        let mut inner = streaming_inner();

        // The reset batch, with batch 1 arriving while it is in flight
        ensure!(
            deliver(&mut inner, 0, 0.0)? == Some((0, 0)),
            "batch 0 goes out at once"
        );
        ensure!(
            deliver(&mut inner, 1, 1.0)?.is_none(),
            "batch 1 waits for the reset"
        );
        ensure!(
            acknowledge(&mut inner, 0, 0)? == Some((1, 1)),
            "the mailbox follows the reset"
        );

        // Batch 1 alone in flight, with 2 and 3 arriving meanwhile
        ensure!(
            deliver(&mut inner, 2, 2.0)?.is_none(),
            "batch 2 waits for batch 1"
        );
        ensure!(
            deliver(&mut inner, 3, 3.0)?.is_none(),
            "batch 3 waits for batch 1"
        );
        ensure!(
            acknowledge(&mut inner, 1, 1)? == Some((2, 3)),
            "2 and 3 coalesce"
        );

        // The coalesced ingest in flight, with 4 arriving meanwhile
        ensure!(
            deliver(&mut inner, 4, 4.0)?.is_none(),
            "batch 4 waits for 2..=3"
        );
        ensure!(acknowledge(&mut inner, 2, 3)? == Some((4, 4)), "4 follows");
        ensure!(
            acknowledge(&mut inner, 4, 4)?.is_none(),
            "nothing is left queued"
        );

        // With nothing in flight, the next batch goes out at once again
        ensure!(
            deliver(&mut inner, 5, 5.0)? == Some((5, 5)),
            "batch 5 goes out at once"
        );
        ensure!(
            inner.diagnostic.is_none(),
            "an in-order stream reports nothing"
        );
        Ok(())
    }

    /// A batch that is not the one expected next closes the stream and says why, so the
    /// status line does not fall back to "Ready" with no reason.
    #[test]
    fn a_gap_closes_the_stream_with_a_reason() -> eyre::Result<()> {
        let mut inner = streaming_inner();
        ensure!(
            deliver(&mut inner, 0, 0.0)?.is_some(),
            "batch 0 goes out at once"
        );

        let action = inner.accept_live_batch(&stream(), vec![0; 512], 2, 1.0);

        ensure!(
            matches!(action, BatchAction::Closed),
            "a skipped batch closes the stream"
        );
        ensure!(
            matches!(
                inner.diagnostic,
                Some(BridgeError::Seedlink(SeedlinkError::OutOfOrder {
                    received: 2,
                    expected: 1
                }))
            ),
            "the diagnostic names the gap, got {:?}",
            inner.diagnostic
        );
        ensure!(
            !inner.live.ingest.is_current(STREAM, SESSION),
            "the stream is retired"
        );
        Ok(())
    }

    /// Batches expiring in the mailbox while the reset is in flight leave a loss
    /// diagnostic that survives the reset's acknowledgement and the recovery after it.
    #[test]
    fn mailbox_expiry_during_the_reset_stays_visible() -> eyre::Result<()> {
        let mut inner = streaming_inner();
        ensure!(
            deliver(&mut inner, 0, 0.0)?.is_some(),
            "batch 0 goes out at once"
        );

        // Batch 1 queues, then expires when batch 2 arrives too late after it
        ensure!(
            deliver(&mut inner, 1, 1.0)?.is_none(),
            "batch 1 waits for the reset"
        );
        ensure!(
            deliver(&mut inner, 2, 2.0 + MAILBOX_MAX_RESIDENCE_S)?.is_none(),
            "batch 2 queues"
        );
        ensure_lagged(&inner, 512)?;

        // The reset's acknowledgement clears the previous source's diagnostic, but not
        // this one, and the stream resumes from batch 2
        ensure!(
            acknowledge(&mut inner, 0, 0)? == Some((2, 2)),
            "batch 2 follows the reset"
        );
        ensure_lagged(&inner, 512)?;
        ensure!(
            acknowledge(&mut inner, 2, 2)?.is_none(),
            "nothing is left queued"
        );
        ensure_lagged(&inner, 512)?;
        Ok(())
    }

    /// Batches expiring in the mailbox while the stream runs leave a loss diagnostic
    /// that stays after the stream recovers.
    #[test]
    fn mailbox_expiry_while_streaming_stays_visible() -> eyre::Result<()> {
        let mut inner = streaming_inner();
        ensure!(
            deliver(&mut inner, 0, 0.0)?.is_some(),
            "batch 0 goes out at once"
        );
        ensure!(
            acknowledge(&mut inner, 0, 0)?.is_none(),
            "nothing is queued"
        );
        ensure!(
            deliver(&mut inner, 1, 1.0)?.is_some(),
            "batch 1 goes out at once"
        );

        // Batches 2 and 3 queue, then expire when batch 4 arrives too late after them
        ensure!(
            deliver(&mut inner, 2, 2.0)?.is_none(),
            "batch 2 waits for batch 1"
        );
        ensure!(
            deliver(&mut inner, 3, 3.0)?.is_none(),
            "batch 3 waits for batch 1"
        );
        ensure!(
            deliver(&mut inner, 4, 4.0 + MAILBOX_MAX_RESIDENCE_S)?.is_none(),
            "batch 4 queues"
        );
        ensure_lagged(&inner, 1024)?;

        // The stream resumes from batch 4 and keeps the loss on screen
        ensure!(
            acknowledge(&mut inner, 1, 1)? == Some((4, 4)),
            "batch 4 follows"
        );
        ensure!(
            acknowledge(&mut inner, 4, 4)?.is_none(),
            "nothing is left queued"
        );
        ensure!(
            deliver(&mut inner, 5, 20.0)? == Some((5, 5)),
            "the stream goes on"
        );
        ensure_lagged(&inner, 1024)?;
        Ok(())
    }

    #[test]
    fn a_retired_session_cannot_deliver_batches_or_acknowledge_the_successor() {
        let mut inner = streaming_inner();
        let successor = LiveStream {
            session: LiveSessionEpoch(2),
            ..stream()
        };
        inner.begin_live_session(STREAM, successor.session, LIVE_FOLLOW_SECONDS);
        assert!(matches!(
            inner.accept_live_batch(&stream(), vec![0; 512], 0, 0.0),
            BatchAction::Held
        ));
        assert!(matches!(
            inner.complete_live_ingest(
                Ok(pb::IngestMiniSeedResult::default()),
                None,
                0,
                0,
                &stream()
            ),
            LiveIngestCompletion::Superseded
        ));
        assert!(inner.live.ingest.is_current(STREAM, successor.session));
        assert!(inner.diagnostic.is_none());
        assert!(matches!(
            inner.accept_live_batch(&successor, vec![0; 512], 0, 0.0),
            BatchAction::Dispatch(_)
        ));
    }

    #[test]
    fn an_acknowledgement_from_another_session_cannot_advance_the_stream() -> eyre::Result<()> {
        let mut inner = streaming_inner();
        ensure!(deliver(&mut inner, 0, 0.0)?.is_some());
        let result = inner.complete_live_ingest(
            Ok(pb::IngestMiniSeedResult {
                stream_id: STREAM.0,
                session_epoch: SESSION.0 + 1,
                ..pb::IngestMiniSeedResult::default()
            }),
            inner.live.ingest_job,
            0,
            0,
            &stream(),
        );
        ensure!(matches!(result, LiveIngestCompletion::Failed(_)));
        ensure!(!inner.live.ingest.is_current(STREAM, SESSION));
        Ok(())
    }

    fn ensure_lagged(inner: &Inner, bytes: u64) -> eyre::Result<()> {
        ensure!(
            matches!(
                inner.diagnostic,
                Some(BridgeError::Seedlink(SeedlinkError::Lagged { dropped_bytes })) if dropped_bytes == bytes
            ),
            "expected a loss of {bytes} bytes on screen, got {:?}",
            inner.diagnostic
        );
        Ok(())
    }
}
