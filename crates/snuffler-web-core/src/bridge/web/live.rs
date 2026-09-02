//! The live `SeedLink` client: the socket, its framing buffer, the bounded
//! coalescing mailbox, and the epoch-fenced ingest dispatch.

#![allow(
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    reason = "split out of the web module and glued with the same glob import; \
              owned parameters cross the JS/async boundary as in the parent"
)]

use super::*;

thread_local! {
    static LIVE_SEEDLINK: StdRefCell<Option<LiveSeedlinkHandle>> = const { StdRefCell::new(None) };
}

struct LiveSeedlinkHandle {
    ws: web_sys::WebSocket,
    on_open: Closure<dyn FnMut(web_sys::Event)>,
    on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
    on_close: Closure<dyn FnMut(web_sys::Event)>,
}

#[derive(Clone)]
struct LiveStream {
    source: String,
    started_at: f64,
    stream: StreamId,
    session: SessionEpoch,
    retain_s: f64,
}

impl Drop for LiveSeedlinkHandle {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
        let _ = (
            &self.on_open,
            &self.on_message,
            &self.on_error,
            &self.on_close,
        );
    }
}

impl LiveSeedlinkHandle {
    fn connect(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        params: SeedlinkParams,
        started_at: f64,
        stream: StreamId,
        session: SessionEpoch,
    ) -> Result<Self, SeedlinkError> {
        let conn = seedlink_connection(&params)?;
        let next_seq = Rc::new(Cell::new(0_u64));
        let retain_s = if params.retain_s > 0.0 {
            params.retain_s
        } else {
            LIVE_RETAIN_SECONDS
        };
        let on_open = seedlink_on_open(
            conn.ws.clone(),
            inner.clone(),
            repaint.clone(),
            conn.source.clone(),
            conn.commands,
        );
        let live = LiveStream {
            source: conn.source.clone(),
            started_at,
            stream,
            session,
            retain_s,
        };
        let on_message = seedlink_on_message(inner.clone(), repaint.clone(), live, next_seq);
        let on_error = seedlink_on_error(inner.clone(), repaint.clone(), stream, session);
        let on_close = seedlink_on_close(inner, repaint, conn.source, stream, session);

        conn.ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        conn.ws
            .set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        conn.ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        conn.ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            ws: conn.ws,
            on_open,
            on_message,
            on_error,
            on_close,
        })
    }
}

struct SeedlinkConnection {
    ws: web_sys::WebSocket,
    source: String,
    commands: Vec<String>,
}

fn seedlink_connection(params: &SeedlinkParams) -> Result<SeedlinkConnection, SeedlinkError> {
    let port = params
        .port
        .trim()
        .parse::<u16>()
        .map_err(|source| SeedlinkError::Port {
            port: params.port.trim().to_owned(),
            source,
        })?;
    let source = format!(
        "{}:{} {}.{} {}",
        params.host.trim(),
        port,
        params.net.trim(),
        params.sta.trim(),
        params.selectors.trim()
    );
    let url = seedlink_proxy_url(params.host.trim(), port);
    let ws = web_sys::WebSocket::new(&url).map_err(|e| SeedlinkError::Connect {
        url,
        detail: jsval_to_string(e),
    })?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
    Ok(SeedlinkConnection {
        ws,
        source,
        commands: seedlink_commands(params),
    })
}

fn seedlink_on_open(
    ws: web_sys::WebSocket,
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    source: String,
    commands: Vec<String>,
) -> Closure<dyn FnMut(web_sys::Event)> {
    Closure::wrap(Box::new(move |_event: web_sys::Event| {
        for command in &commands {
            if let Err(err) = ws.send_with_str(command) {
                set_bridge_error(
                    &inner,
                    SeedlinkError::Send {
                        command: command.clone(),
                        detail: jsval_to_string(err),
                    }
                    .into(),
                );
                repaint.request_repaint();
                return;
            }
        }
        log::info!("SeedLink connected: {source}");
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

fn seedlink_on_message(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    live: LiveStream,
    next_seq: Rc<Cell<u64>>,
) -> Closure<dyn FnMut(web_sys::MessageEvent)> {
    let buffer = Rc::new(RefCell::new(Vec::new()));
    Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        let bytes = match seedlink_event_bytes(&event) {
            Ok(bytes) if bytes.is_empty() => return,
            Ok(bytes) => bytes,
            Err(error) => {
                if close_live_ingest(&inner, live.stream, live.session) {
                    set_bridge_error(&inner, SeedlinkError::from(error).into());
                    repaint.request_repaint();
                }
                return;
            }
        };
        let combined = {
            let buf = buffer.borrow();
            buf.len().saturating_add(bytes.len())
        };
        if combined > crate::seedlink::MAX_FRAME_BUFFER_BYTES {
            if close_live_ingest(&inner, live.stream, live.session) {
                set_bridge_error(
                    &inner,
                    SeedlinkError::from(crate::seedlink::FrameError::Oversized {
                        announced: combined,
                        limit: crate::seedlink::MAX_FRAME_BUFFER_BYTES,
                    })
                    .into(),
                );
                repaint.request_repaint();
            }
            return;
        }
        let records = {
            let mut buf = buffer.borrow_mut();
            buf.extend_from_slice(&bytes);
            drain_seedlink_records(&mut buf)
        };
        let records = match records {
            Ok(records) => records,
            Err(error) => {
                if !close_live_ingest(&inner, live.stream, live.session) {
                    return;
                }
                set_bridge_error(&inner, SeedlinkError::from(error).into());
                repaint.request_repaint();
                return;
            }
        };
        if !records.is_empty() {
            let seq = next_seq.get();
            next_seq.set(seq.wrapping_add(1));
            ingest_live_mseed(inner.clone(), repaint.clone(), live.clone(), records, seq);
        }
    }) as Box<dyn FnMut(_)>)
}

fn seedlink_on_error(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    stream: StreamId,
    session: SessionEpoch,
) -> Closure<dyn FnMut(web_sys::ErrorEvent)> {
    Closure::wrap(Box::new(move |event: web_sys::ErrorEvent| {
        if !close_live_ingest(&inner, stream, session) {
            return;
        }
        let msg = event.message();
        set_bridge_error(
            &inner,
            SeedlinkError::Socket {
                detail: if msg.is_empty() {
                    "the browser gave no reason".to_owned()
                } else {
                    msg
                },
            }
            .into(),
        );
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

fn seedlink_on_close(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    source: String,
    stream: StreamId,
    session: SessionEpoch,
) -> Closure<dyn FnMut(web_sys::Event)> {
    Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if close_live_ingest(&inner, stream, session) {
            inner.borrow_mut().live_seedlink_started_at = None;
            log::info!("SeedLink disconnected: {source}");
        }
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

fn seedlink_event_bytes(
    event: &web_sys::MessageEvent,
) -> Result<Vec<u8>, crate::seedlink::FrameError> {
    let data = event.data();
    if data.is_instance_of::<js_sys::ArrayBuffer>() {
        let bytes = js_sys::Uint8Array::new(&data);
        let length = usize::try_from(bytes.byte_length()).unwrap_or(usize::MAX);
        if length > crate::seedlink::MAX_FRAME_BUFFER_BYTES {
            return Err(crate::seedlink::FrameError::Oversized {
                announced: length,
                limit: crate::seedlink::MAX_FRAME_BUFFER_BYTES,
            });
        }
        Ok(bytes.to_vec())
    } else if let Some(text) = data.as_string() {
        let length = text.len();
        if length > crate::seedlink::MAX_FRAME_BUFFER_BYTES {
            return Err(crate::seedlink::FrameError::Oversized {
                announced: length,
                limit: crate::seedlink::MAX_FRAME_BUFFER_BYTES,
            });
        }
        Ok(text.into_bytes())
    } else {
        Ok(Vec::new())
    }
}

fn is_current_live_ingest(
    state: &LiveIngestState,
    stream: StreamId,
    session: SessionEpoch,
) -> bool {
    match state {
        LiveIngestState::Opening {
            stream: current,
            session: current_session,
        }
        | LiveIngestState::ResetInFlight {
            stream: current,
            session: current_session,
        }
        | LiveIngestState::Streaming {
            stream: current,
            session: current_session,
            ..
        }
        | LiveIngestState::Lagged {
            stream: current,
            session: current_session,
            ..
        } => *current == stream && *current_session == session,
        LiveIngestState::Closed => false,
    }
}

fn close_live_ingest(inner: &Rc<RefCell<Inner>>, stream: StreamId, session: SessionEpoch) -> bool {
    {
        let mut state = inner.borrow_mut();
        if !is_current_live_ingest(&state.live_ingest, stream, session) {
            return false;
        }
        retire_live_session_state(&mut state);
    }
    // A terminally closed stream retires its socket too — otherwise the UI
    // keeps offering a live session whose every packet is rejected.
    defer_live_socket_retirement();
    true
}

/// The state half of live retirement, for callers already holding the borrow.
/// Bumping the session epoch fences any callback still in flight.
pub(super) fn retire_live_session_state(state: &mut Inner) {
    state.session_epoch = state.session_epoch.wrapping_add(1).max(1);
    state.live_ingest = LiveIngestState::Closed;
    state.ingest_mailbox = LiveIngestMailbox::default();
    state.ingest_job = None;
    state.live_seedlink_started_at = None;
}

/// Drops the socket handle on the next event-loop turn: retirement can run
/// inside the socket's own callbacks, where dropping the currently executing
/// closure is forbidden.
pub(super) fn defer_live_socket_retirement() {
    #[expect(
        clippy::disallowed_methods,
        reason = "socket callbacks defer dropping their own currently executing closure"
    )]
    spawn_local(async {
        LIVE_SEEDLINK.with(|slot| {
            slot.borrow_mut().take();
        });
    });
}

pub(super) fn retire_live_session(inner: &Rc<RefCell<Inner>>) -> SessionEpoch {
    LIVE_SEEDLINK.with(|slot| {
        slot.borrow_mut().take();
    });
    let mut state = inner.borrow_mut();
    retire_live_session_state(&mut state);
    SessionEpoch(state.session_epoch)
}

fn seedlink_proxy_url(host: &str, port: u16) -> String {
    format!(
        "ws://127.0.0.1:17684/seedlink?host={}&port={}",
        encode_query_component(host),
        port
    )
}

fn encode_query_component(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

fn seedlink_commands(params: &SeedlinkParams) -> Vec<String> {
    let net = params.net.trim();
    let sta = params.sta.trim();
    let selectors = params.selectors.trim();
    let mut commands = vec!["HELLO\r".to_owned(), "SLPROTO 4.0\r".to_owned()];
    if !sta.is_empty() {
        commands.push(if net.is_empty() {
            format!("STATION {sta}\r")
        } else {
            format!("STATION {net}_{sta}\r")
        });
    }
    if !selectors.is_empty() {
        commands.push(format!(
            "SELECT {}\r",
            crate::seedlink::v4_selectors(selectors)
        ));
    }
    commands.push("DATA\r".to_owned());
    commands.push("END\r".to_owned());
    commands
}

use crate::seedlink::drain_records as drain_seedlink_records;

const INGEST_MAILBOX_BYTES: usize = 8 * 1024 * 1024;
const INGEST_MAILBOX_RESIDENCE_S: f64 = 10.0;

fn note_dropped_batch(mailbox: &mut LiveIngestMailbox, batch: LiveIngestBatch) {
    mailbox.bytes = mailbox.bytes.saturating_sub(batch.content.len());
    mailbox.dropped_bytes = mailbox
        .dropped_bytes
        .saturating_add(u64::try_from(batch.content.len()).unwrap_or(u64::MAX));
}

fn enqueue_live_batch(
    inner: &Rc<RefCell<Inner>>,
    stream: StreamId,
    session: SessionEpoch,
    batch: LiveIngestBatch,
) {
    let now_s = js_sys::Date::now() / 1000.0;
    let batch_seq = batch.seq;
    // A sequence fault is terminal for the whole acquisition, not just the
    // ingest state: the socket retires too, or the UI keeps a visibly live
    // session whose every later packet is dropped.
    let sequence_fault = {
        let mut state = inner.borrow_mut();
        let expected = state.ingest_mailbox.batches.back().map_or_else(
            || match state.live_ingest {
                LiveIngestState::ResetInFlight { .. } => 1,
                LiveIngestState::Streaming { next_seq, .. }
                | LiveIngestState::Lagged { next_seq, .. } => next_seq,
                LiveIngestState::Opening { .. } | LiveIngestState::Closed => 0,
            },
            |queued| queued.seq.wrapping_add(1),
        );
        let fault = batch_seq != expected;
        if fault {
            retire_live_session_state(&mut state);
        }
        fault
    };
    if sequence_fault {
        defer_live_socket_retirement();
        return;
    }

    let dropped = {
        let mut state = inner.borrow_mut();
        state.ingest_mailbox.bytes = state
            .ingest_mailbox
            .bytes
            .saturating_add(batch.content.len());
        state.ingest_mailbox.batches.push_back(batch);
        while state
            .ingest_mailbox
            .batches
            .front()
            .is_some_and(|oldest| now_s - oldest.received_s > INGEST_MAILBOX_RESIDENCE_S)
            || state.ingest_mailbox.bytes > INGEST_MAILBOX_BYTES
        {
            let Some(dropped) = state.ingest_mailbox.batches.pop_front() else {
                break;
            };
            note_dropped_batch(&mut state.ingest_mailbox, dropped);
        }
        let dropped = state.ingest_mailbox.dropped_bytes;
        if dropped > 0 {
            let was_lagged = matches!(state.live_ingest, LiveIngestState::Lagged { .. });
            let next_seq = state
                .ingest_mailbox
                .batches
                .back()
                .map_or(batch_seq.wrapping_add(1), |queued| {
                    queued.seq.wrapping_add(1)
                });
            state.live_ingest = LiveIngestState::Lagged {
                stream,
                session,
                next_seq,
                dropped_bytes: dropped,
            };
            // Notify once per lag episode; dropped_bytes keeps accumulating
            // until an acknowledgement drains the mailbox, so repeating the
            // notice on every enqueue would re-bump the snuffling text
            // generation without new information.
            if was_lagged { 0 } else { dropped }
        } else {
            0
        }
    };
    if dropped > 0 {
        set_snuffling_text(
            inner,
            format!("SeedLink lagged: dropped {dropped} buffered bytes"),
            false,
        );
    }
}

fn take_buffered_ingest_request(
    state: &mut Inner,
    stream: StreamId,
    session: SessionEpoch,
    start_time: f64,
    retain_s: f64,
) -> Option<IngestMiniSeedRequest> {
    let mut mailbox = std::mem::take(&mut state.ingest_mailbox);
    let Some(first) = mailbox.batches.front() else {
        state.ingest_mailbox = mailbox;
        return None;
    };
    let Some(last) = mailbox.batches.back() else {
        state.ingest_mailbox = mailbox;
        return None;
    };
    let source = first.source.clone();
    let batch_seq_first = first.seq;
    let batch_seq_last = last.seq;
    let mut content = Vec::with_capacity(mailbox.bytes);
    for batch in mailbox.batches.drain(..) {
        content.extend_from_slice(&batch.content);
    }
    Some(IngestMiniSeedRequest {
        source,
        content,
        start_time,
        reset: false,
        retain_seconds: retain_s,
        stream_id: stream.0,
        session_epoch: session.0,
        batch_seq_first,
        batch_seq_last,
    })
}

fn ingest_live_mseed(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    live: LiveStream,
    content: Vec<u8>,
    seq: u64,
) {
    if content.len() > INGEST_MAILBOX_BYTES {
        set_bridge_error(
            &inner,
            SeedlinkError::from(crate::seedlink::FrameError::Oversized {
                announced: content.len(),
                limit: INGEST_MAILBOX_BYTES,
            })
            .into(),
        );
        close_live_ingest(&inner, live.stream, live.session);
        repaint.request_repaint();
        return;
    }
    let mut sequence_fault = None;
    let enqueue = {
        let state = inner.borrow();
        if !is_current_live_ingest(&state.live_ingest, live.stream, live.session) {
            return;
        }
        if state.ingest_job.is_some() {
            true
        } else {
            let expected = match state.live_ingest {
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
        set_bridge_error(
            &inner,
            BridgeError::browser(
                "ingesting SeedLink data",
                format!("received batch {seq}, expected {expected}"),
            ),
        );
        repaint.request_repaint();
        return;
    }
    if enqueue {
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

    let request = IngestMiniSeedRequest {
        source: live.source.clone(),
        content,
        start_time: live.started_at,
        reset: seq == 0,
        retain_seconds: live.retain_s,
        stream_id: live.stream.0,
        session_epoch: live.session.0,
        batch_seq_first: seq,
        batch_seq_last: seq,
    };
    dispatch_live_ingest(inner, repaint, request, live);
}

enum LiveIngestCompletion {
    Superseded,
    Accepted {
        next_request: Option<IngestMiniSeedRequest>,
        effect: LiveIngestEffect,
    },
    Failed(BridgeError),
}

#[derive(Clone, Copy)]
enum LiveIngestEffect {
    ExtendSession,
    ResetSession,
}

fn complete_live_ingest(
    state: &mut Inner,
    parsed: Result<pb::IngestMiniSeedResult, CallError>,
    job: Option<JobHandle>,
    expected_first: u64,
    expected_last: u64,
    live: &LiveStream,
) -> LiveIngestCompletion {
    if state.ingest_job == job {
        state.ingest_job = None;
    }
    if !is_current_live_ingest(&state.live_ingest, live.stream, live.session) {
        return LiveIngestCompletion::Superseded;
    }
    let ack = match parsed {
        Ok(ack) => ack,
        Err(error) => {
            // Batch N failed terminally; dispatching the buffered successors would
            // mutate the session after the stream's terminal failure.
            state.live_ingest = LiveIngestState::Closed;
            state.ingest_mailbox = LiveIngestMailbox::default();
            return LiveIngestCompletion::Failed(error.into());
        }
    };
    if ack.stream_id != live.stream.0
        || ack.session_epoch != live.session.0
        || ack.ack_seq_first != expected_first
        || ack.ack_seq_last != expected_last
    {
        state.live_ingest = LiveIngestState::Closed;
        state.ingest_mailbox = LiveIngestMailbox::default();
        return LiveIngestCompletion::Failed(BridgeError::browser(
            "acknowledging SeedLink data",
            "stale or out-of-order ingest acknowledgement",
        ));
    }
    let received_next_seq = match state.live_ingest {
        LiveIngestState::Lagged { next_seq, .. } => next_seq,
        _ => expected_last.wrapping_add(1),
    };
    let effect = if expected_first == 0 {
        state.diagnostic = None;
        state.snuffling = None;
        clear_event_state(state);
        state.station_dists.clear();
        state.station_dists_gen = None;
        state.live_seedlink_started_at = Some(live.started_at);
        LiveIngestEffect::ResetSession
    } else {
        LiveIngestEffect::ExtendSession
    };
    state.scene.clone_from(&ack.scene);
    state.scene_gen += 1;
    clear_view(state);
    if let LiveIngestState::Lagged { dropped_bytes, .. } = state.live_ingest {
        log::warn!("SeedLink resumed after acknowledging a gap of {dropped_bytes} bytes");
    }
    state.live_ingest = LiveIngestState::Streaming {
        stream: live.stream,
        session: live.session,
        next_seq: received_next_seq.max(expected_last.wrapping_add(1)),
    };
    let next_request = take_buffered_ingest_request(
        state,
        live.stream,
        live.session,
        live.started_at,
        live.retain_s,
    );
    LiveIngestCompletion::Accepted {
        next_request,
        effect,
    }
}

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
    let reject_repaint = repaint.clone();
    let job_slot = Rc::new(Cell::new(None));
    let work_job_slot = job_slot.clone();
    let work_live = live.clone();
    let submission = submit_job(
        inner.clone(),
        repaint,
        Operation::IngestMiniSeed,
        Some(request.source.clone()),
        Vec::new(),
        async move {
            let parsed = client().ingest_mini_seed(request).await;
            let completion = {
                let mut state = work_inner.borrow_mut();
                complete_live_ingest(
                    &mut state,
                    parsed,
                    work_job_slot.get(),
                    expected_first,
                    expected_last,
                    &work_live,
                )
            };
            match completion {
                LiveIngestCompletion::Superseded => Ok(()),
                LiveIngestCompletion::Accepted {
                    next_request,
                    effect,
                } => {
                    if let Some(next_request) = next_request {
                        dispatch_live_ingest(
                            work_inner,
                            work_repaint,
                            next_request,
                            work_live.clone(),
                        );
                    }
                    if matches!(effect, LiveIngestEffect::ResetSession) {
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
            job_slot.set(Some(handle));
            let mut state = inner.borrow_mut();
            if expected_first == 0 {
                state.live_ingest = LiveIngestState::ResetInFlight {
                    stream: live.stream,
                    session: live.session,
                };
            }
            state.ingest_job = Some(handle);
        }
        Submission::Rejected(_, reason) => {
            close_live_ingest(&inner, live.stream, live.session);
            set_bridge_error(
                &inner,
                BridgeError::browser(
                    "starting SeedLink ingestion",
                    format!("ingest admission was rejected: {reason}"),
                ),
            );
            reject_repaint.request_repaint();
        }
    }
}

impl Bridge {
    /// Reports whether a live `SeedLink` socket is active.
    #[must_use]
    pub fn seedlink_active(&self) -> bool {
        let _scene_gen = self.inner.borrow().scene_gen;
        LIVE_SEEDLINK.with(|slot| slot.borrow().is_some())
    }

    /// Starts a live `SeedLink` source and replaces the current pile.
    pub fn start_seedlink(&self, params: SeedlinkParams) {
        let repaint = self.repaint.clone();
        let (stream, session) = {
            let state = self.inner.borrow();
            (
                StreamId(state.next_stream_id.wrapping_add(1).max(1)),
                SessionEpoch(state.session_epoch.wrapping_add(1).max(1)),
            )
        };
        let started_at = js_sys::Date::now() / 1000.0;
        let follow_s = if params.follow_s > 0.0 {
            params.follow_s
        } else {
            LIVE_FOLLOW_SECONDS
        };
        match LiveSeedlinkHandle::connect(
            self.inner.clone(),
            repaint.clone(),
            params,
            started_at,
            stream,
            session,
        ) {
            Ok(handle) => {
                LIVE_SEEDLINK.with(|slot| {
                    *slot.borrow_mut() = Some(handle);
                });
                let mut i = self.inner.borrow_mut();
                retire_live_session_state(&mut i);
                i.session_epoch = session.0;
                i.next_stream_id = stream.0;
                i.live_ingest = LiveIngestState::Opening { stream, session };
                i.live_seedlink_follow_s = follow_s;
                i.diagnostic = None;
            }
            Err(err) => {
                self.inner.borrow_mut().diagnostic = Some(err.into());
            }
        }
        repaint.request_repaint();
    }

    /// Stops live `SeedLink` acquisition while retaining received samples.
    pub fn stop_seedlink(&self) {
        retire_live_session(&self.inner);
        set_snuffling_text(&self.inner, "SeedLink stopped".to_owned(), false);
    }
}
