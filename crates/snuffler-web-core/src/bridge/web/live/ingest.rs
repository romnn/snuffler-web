//! Feeding live records to pyrocko: one `IngestMiniSeed` in flight per stream, with
//! the batches that arrive meanwhile coalesced in a bounded mailbox.
//!
//! The decisions live in the host-side state machine (`bridge::ingest`); this module
//! submits the ingests it asks for and retires the socket when it closes the stream.

use super::*;

/// Ingests one batch at once, or queues it while an ingest is in flight.
pub(super) fn ingest_live_mseed(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    live: LiveStream,
    content: Vec<u8>,
    seq: u64,
) {
    let action =
        inner
            .borrow_mut()
            .accept_live_batch(&live, content, seq, js_sys::Date::now() / 1000.0);
    match action {
        BatchAction::Dispatch(request) => dispatch_live_ingest(inner, repaint, request, live),
        BatchAction::Held => {}
        BatchAction::Closed => {
            defer_live_socket_retirement();
            repaint.request_repaint();
        }
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
            let completion = work_inner.borrow_mut().complete_live_ingest(
                reply,
                work_own_job.get(),
                expected_first,
                expected_last,
                &work_live,
            );
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
            inner.borrow_mut().note_live_ingest_admitted(handle);
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
