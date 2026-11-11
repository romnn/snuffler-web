//! Feeding live records to pyrocko: one `IngestMiniSeed` in flight per stream, with
//! the batches that arrive meanwhile coalesced in a bounded mailbox.
//!
//! Every batch carries its sequence number, and every acknowledgement echoes the
//! stream, session and sequence range it covers.
//! A gap, a reordering or a stale acknowledgement closes the stream rather than
//! ingesting out of order.

use super::*;

/// Most buffered bytes the mailbox holds before dropping its oldest batches.
const MAILBOX_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Longest a batch waits in the mailbox before it is dropped as too old, in seconds.
const MAILBOX_MAX_RESIDENCE_S: f64 = 10.0;

/// Ingests one batch at once, or queues it while an ingest is in flight.
pub(super) fn ingest_live_mseed(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    live: LiveStream,
    content: Vec<u8>,
    seq: u64,
) {
    if content.len() > MAILBOX_MAX_BYTES {
        set_diagnostic(
            &inner,
            SeedlinkError::from(crate::seedlink::FrameError::Oversized {
                announced: content.len(),
                limit: MAILBOX_MAX_BYTES,
            })
            .into(),
        );
        close_live_ingest(&inner, live.stream, live.session);
        repaint.request_repaint();
        return;
    }

    // Queue behind the ingest in flight, or check the batch is the one expected next
    let mut sequence_fault = None;
    let should_queue = {
        let state = inner.borrow();
        if !state.live.ingest.is_current(live.stream, live.session) {
            return;
        }
        if state.live.ingest_job.is_some() {
            true
        } else {
            let expected = match state.live.ingest {
                LiveIngestState::Opening { .. } => 0,
                LiveIngestState::Streaming { next_seq, .. }
                | LiveIngestState::Lagged { next_seq, .. } => next_seq,
                LiveIngestState::ResetInFlight { .. } | LiveIngestState::Closed => return,
            };
            if seq != expected {
                sequence_fault = Some(expected);
            }
            false
        }
    };
    if let Some(expected) = sequence_fault {
        close_live_ingest(&inner, live.stream, live.session);
        set_diagnostic(
            &inner,
            BridgeError::browser(
                "ingesting SeedLink data",
                format!("received batch {seq}, expected {expected}"),
            ),
        );
        repaint.request_repaint();
        return;
    }
    if should_queue {
        enqueue_live_batch(
            &inner,
            live.stream,
            live.session,
            LiveIngestBatch {
                source: live.source,
                content,
                seq,
                received_s: js_sys::Date::now() / 1000.0,
            },
        );
        return;
    }

    // Ingest now
    let request = IngestMiniSeedRequest {
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
    };
    dispatch_live_ingest(inner, repaint, request, live);
}

/// Adds a batch to the mailbox, dropping the oldest ones past its bounds.
fn enqueue_live_batch(
    inner: &Rc<RefCell<Inner>>,
    stream: StreamId,
    session: LiveSessionEpoch,
    batch: LiveIngestBatch,
) {
    let now_s = js_sys::Date::now() / 1000.0;
    let batch_seq = batch.seq;

    // A sequence fault is terminal for the whole acquisition, not just the
    // ingest state: the socket retires too, or the UI keeps a visibly live
    // session whose every later packet is dropped.
    let sequence_fault = {
        let mut state = inner.borrow_mut();
        let expected = state.live.mailbox.batches.back().map_or_else(
            || match state.live.ingest {
                LiveIngestState::ResetInFlight { .. } => 1,
                LiveIngestState::Streaming { next_seq, .. }
                | LiveIngestState::Lagged { next_seq, .. } => next_seq,
                LiveIngestState::Opening { .. } | LiveIngestState::Closed => 0,
            },
            |queued| queued.seq.wrapping_add(1),
        );
        let is_fault = batch_seq != expected;
        if is_fault {
            state.live.retire();
        }
        is_fault
    };
    if sequence_fault {
        defer_live_socket_retirement();
        return;
    }

    // Enqueue, then drop from the front past the size and age bounds
    let newly_lagged_bytes = {
        let mut state = inner.borrow_mut();
        let live = &mut state.live;
        live.mailbox.bytes = live.mailbox.bytes.saturating_add(batch.content.len());
        live.mailbox.batches.push_back(batch);
        while live
            .mailbox
            .batches
            .front()
            .is_some_and(|oldest| now_s - oldest.received_s > MAILBOX_MAX_RESIDENCE_S)
            || live.mailbox.bytes > MAILBOX_MAX_BYTES
        {
            let Some(dropped) = live.mailbox.batches.pop_front() else {
                break;
            };
            live.mailbox.bytes = live.mailbox.bytes.saturating_sub(dropped.content.len());
            live.mailbox.dropped_bytes = live
                .mailbox
                .dropped_bytes
                .saturating_add(u64::try_from(dropped.content.len()).unwrap_or(u64::MAX));
        }

        let dropped_bytes = live.mailbox.dropped_bytes;
        if dropped_bytes == 0 {
            0
        } else {
            let was_lagged = matches!(live.ingest, LiveIngestState::Lagged { .. });
            let next_seq = live
                .mailbox
                .batches
                .back()
                .map_or(batch_seq.wrapping_add(1), |queued| {
                    queued.seq.wrapping_add(1)
                });
            live.ingest = LiveIngestState::Lagged {
                stream,
                session,
                next_seq,
                dropped_bytes,
            };
            // Notify once per lag episode: dropped_bytes keeps accumulating until an
            // acknowledgement drains the mailbox, so repeating the notice on every
            // enqueue would re-bump the snuffling text generation without new
            // information.
            if was_lagged { 0 } else { dropped_bytes }
        }
    };
    if newly_lagged_bytes > 0 {
        inner.borrow_mut().install_snuffling_text(
            format!("SeedLink lagged: dropped {newly_lagged_bytes} buffered bytes"),
            false,
        );
    }
}

/// Drains the mailbox into one request covering every queued batch.
fn take_mailbox_request(state: &mut Inner, live: &LiveStream) -> Option<IngestMiniSeedRequest> {
    let mailbox = &mut state.live.mailbox;
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

/// What an acknowledged ingest means for the stream.
enum LiveIngestCompletion {
    /// The stream was retired while the ingest ran.
    Superseded,
    Accepted {
        /// The coalesced mailbox, to ingest next.
        next_request: Option<IngestMiniSeedRequest>,
        /// Whether this batch started the session.
        reset_session: bool,
    },
    Failed(BridgeError),
}

/// Applies one `IngestMiniSeed` reply to the stream state.
fn complete_live_ingest(
    state: &mut Inner,
    reply: Result<pb::IngestMiniSeedResult, CallError>,
    job: Option<JobHandle>,
    expected_first: u64,
    expected_last: u64,
    live: &LiveStream,
) -> LiveIngestCompletion {
    if state.live.ingest_job == job {
        state.live.ingest_job = None;
    }
    if !state.live.ingest.is_current(live.stream, live.session) {
        return LiveIngestCompletion::Superseded;
    }

    // Check the acknowledgement
    let ack = match reply {
        Ok(ack) => ack,
        Err(error) => {
            // Batch N failed terminally; dispatching the buffered successors would
            // mutate the session after the stream's terminal failure.
            state.live.ingest = LiveIngestState::Closed;
            state.live.mailbox = LiveIngestMailbox::default();
            return LiveIngestCompletion::Failed(error.into());
        }
    };
    if ack.stream_id != live.stream.0
        || ack.session_epoch != live.session.0
        || ack.ack_seq_first != expected_first
        || ack.ack_seq_last != expected_last
    {
        state.live.ingest = LiveIngestState::Closed;
        state.live.mailbox = LiveIngestMailbox::default();
        return LiveIngestCompletion::Failed(BridgeError::browser(
            "acknowledging SeedLink data",
            "stale or out-of-order ingest acknowledgement",
        ));
    }

    // Batch 0 starts the session: drop the previous source's annotations
    let reset_session = expected_first == 0;
    if reset_session {
        state.diagnostic = None;
        state.snuffling = None;
        state.clear_event_state();
        state.live.started_at = Some(live.started_at);
    }

    // Install the grown scene and advance the stream
    let received_next_seq = match state.live.ingest {
        LiveIngestState::Lagged { next_seq, .. } => next_seq,
        _ => expected_last.wrapping_add(1),
    };
    state.scene.clone_from(&ack.scene);
    state.scene_generation += 1;
    state.clear_view();
    if let LiveIngestState::Lagged { dropped_bytes, .. } = state.live.ingest {
        log::warn!("SeedLink resumed after acknowledging a gap of {dropped_bytes} bytes");
    }
    state.live.ingest = LiveIngestState::Streaming {
        stream: live.stream,
        session: live.session,
        next_seq: received_next_seq.max(expected_last.wrapping_add(1)),
    };
    LiveIngestCompletion::Accepted {
        next_request: take_mailbox_request(state, live),
        reset_session,
    }
}

/// Submits one ingest and chains the coalesced mailbox after it.
fn dispatch_live_ingest(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    request: IngestMiniSeedRequest,
    live: LiveStream,
) {
    let expected_first = request.batch_seq_first;
    let expected_last = request.batch_seq_last;

    let work_inner = inner.clone();
    let work_repaint = repaint.clone();
    // Filled in once the ledger has minted the handle, before the work can run.
    let own_job = Rc::new(Cell::new(None));
    let work_own_job = own_job.clone();
    let work_live = live.clone();
    let submission = submit_job(
        inner.clone(),
        repaint.clone(),
        Operation::IngestMiniSeed,
        Some(request.source.clone()),
        Vec::new(),
        async move {
            let reply = client().ingest_mini_seed(request).await;
            let completion = {
                let mut state = work_inner.borrow_mut();
                complete_live_ingest(
                    &mut state,
                    reply,
                    work_own_job.get(),
                    expected_first,
                    expected_last,
                    &work_live,
                )
            };
            match completion {
                LiveIngestCompletion::Superseded => Ok(()),
                LiveIngestCompletion::Accepted {
                    next_request,
                    reset_session,
                } => {
                    if let Some(next_request) = next_request {
                        dispatch_live_ingest(work_inner, work_repaint, next_request, work_live);
                    }
                    if reset_session {
                        js_clear_open_sources();
                    }
                    Ok(())
                }
                LiveIngestCompletion::Failed(error) => {
                    defer_live_socket_retirement();
                    Err(error)
                }
            }
        },
    );
    match submission {
        Submission::Accepted(handle) | Submission::Joined(handle) => {
            own_job.set(Some(handle));
            let mut state = inner.borrow_mut();
            if expected_first == 0 {
                state.live.ingest = LiveIngestState::ResetInFlight {
                    stream: live.stream,
                    session: live.session,
                };
            }
            state.live.ingest_job = Some(handle);
        }
        Submission::Rejected(_, reason) => {
            close_live_ingest(&inner, live.stream, live.session);
            set_diagnostic(
                &inner,
                BridgeError::browser(
                    "starting SeedLink ingestion",
                    format!("ingest admission was rejected: {reason}"),
                ),
            );
            repaint.request_repaint();
        }
    }
}
