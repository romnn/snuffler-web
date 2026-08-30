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
        let on_message = seedlink_on_message(
            inner.clone(),
            repaint.clone(),
            conn.source.clone(),
            started_at,
            stream,
            session,
            next_seq,
            retain_s,
        );
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
        set_snuffling_text(&inner, format!("SeedLink connected: {source}"), false);
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

fn seedlink_on_message(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    source: String,
    started_at: f64,
    stream: StreamId,
    session: SessionEpoch,
    next_seq: Rc<Cell<u64>>,
    retain_s: f64,
) -> Closure<dyn FnMut(web_sys::MessageEvent)> {
    let buffer = Rc::new(RefCell::new(Vec::new()));
    Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        let bytes = seedlink_event_bytes(&event);
        if bytes.is_empty() {
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
                if !close_live_ingest(&inner, stream, session) {
                    return;
                }
                set_bridge_error(&inner, SeedlinkError::from(error).into());
                repaint.request_repaint();
                spawn_local(async {
                    LIVE_SEEDLINK.with(|slot| {
                        slot.borrow_mut().take();
                    });
                });
                return;
            }
        };
        if !records.is_empty() {
            let seq = next_seq.get();
            next_seq.set(seq.wrapping_add(1));
            ingest_live_mseed(
                inner.clone(),
                repaint.clone(),
                source.clone(),
                records,
                started_at,
                stream,
                session,
                seq,
                retain_s,
            );
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
        spawn_local(async {
            LIVE_SEEDLINK.with(|slot| {
                slot.borrow_mut().take();
            });
        });
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
            set_snuffling_text(&inner, format!("SeedLink disconnected: {source}"), false);
            spawn_local(async {
                LIVE_SEEDLINK.with(|slot| {
                    slot.borrow_mut().take();
                });
            });
        }
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

fn seedlink_event_bytes(event: &web_sys::MessageEvent) -> Vec<u8> {
    let data = event.data();
    if data.is_instance_of::<js_sys::ArrayBuffer>() {
        js_sys::Uint8Array::new(&data).to_vec()
    } else if let Some(text) = data.as_string() {
        text.into_bytes()
    } else {
        Vec::new()
    }
}

fn live_ingest_matches(
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

fn close_live_ingest(
    inner: &Rc<RefCell<Inner>>,
    stream: StreamId,
    session: SessionEpoch,
) -> bool {
    {
        let mut state = inner.borrow_mut();
        if !live_ingest_matches(&state.live_ingest, stream, session) {
            return false;
        }
        retire_live_session_state(&mut state);
    }
    // A terminally closed stream retires its socket too — otherwise the UI
    // keeps offering a live session whose every packet is rejected.
    retire_live_socket_deferred();
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
pub(super) fn retire_live_socket_deferred() {
    spawn_local(async {
        LIVE_SEEDLINK.with(|slot| {
            slot.borrow_mut().take();
        });
    });
}

pub(super) fn retire_live_source(inner: &Rc<RefCell<Inner>>) -> SessionEpoch {
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
    mailbox.dropped_seq_first.get_or_insert(batch.seq);
    mailbox.dropped_seq_last = Some(batch.seq);
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
        retire_live_socket_deferred();
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

fn take_live_mailbox(
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
        dropped_seq_first: mailbox.dropped_seq_first.unwrap_or(0),
        dropped_seq_last: mailbox.dropped_seq_last.unwrap_or(0),
        dropped_bytes: mailbox.dropped_bytes,
    })
}

fn ingest_live_mseed(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    source: String,
    content: Vec<u8>,
    start_time: f64,
    stream: StreamId,
    session: SessionEpoch,
    seq: u64,
    retain_s: f64,
) {
    if seq == 0 && content.len() > INGEST_MAILBOX_BYTES {
        set_bridge_error(
            &inner,
            SeedlinkError::from(crate::seedlink::FrameError::Oversized {
                announced: content.len(),
                limit: INGEST_MAILBOX_BYTES,
            })
            .into(),
        );
        close_live_ingest(&inner, stream, session);
        spawn_local(async {
            LIVE_SEEDLINK.with(|slot| {
                slot.borrow_mut().take();
            });
        });
        repaint.request_repaint();
        return;
    }
    let enqueue = {
        let mut state = inner.borrow_mut();
        if !live_ingest_matches(&state.live_ingest, stream, session) {
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
                state.live_ingest = LiveIngestState::Closed;
                return;
            }
            if seq == 0 {
                state.live_ingest = LiveIngestState::ResetInFlight { stream, session };
            }
            false
        }
    };
    if enqueue {
        enqueue_live_batch(
            &inner,
            stream,
            session,
            LiveIngestBatch {
                source,
                content,
                seq,
                received_s: js_sys::Date::now() / 1000.0,
            },
        );
        return;
    }

    let request = IngestMiniSeedRequest {
        source,
        content,
        start_time,
        reset: seq == 0,
        retain_seconds: retain_s,
        stream_id: stream.0,
        session_epoch: session.0,
        batch_seq_first: seq,
        batch_seq_last: seq,
        dropped_seq_first: 0,
        dropped_seq_last: 0,
        dropped_bytes: 0,
    };
    dispatch_live_ingest(
        inner, repaint, request, stream, session, start_time, retain_s,
    );
}

fn dispatch_live_ingest(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    request: IngestMiniSeedRequest,
    stream: StreamId,
    session: SessionEpoch,
    start_time: f64,
    retain_s: f64,
) {
    let expected_first = request.batch_seq_first;
    let expected_last = request.batch_seq_last;

    let work_inner = inner.clone();
    let work_repaint = repaint.clone();
    let job_slot = Rc::new(Cell::new(None));
    let work_job_slot = job_slot.clone();
    let submission = submit_job(
        inner.clone(),
        repaint,
        Operation::IngestMiniSeed,
        Some(request.source.clone()),
        Vec::new(),
        async move {
            let parsed = client().ingest_mini_seed(request).await;
            let failed = parsed.is_err();
            let mut acknowledgement_error = None;
            let pending = {
                let mut state = work_inner.borrow_mut();
                if state.ingest_job == work_job_slot.get() {
                    state.ingest_job = None;
                }
                if !live_ingest_matches(&state.live_ingest, stream, session) {
                    return Ok(());
                }
                if let Ok(ack) = &parsed {
                    if ack.stream_id != stream.0
                        || ack.session_epoch != session.0
                        || ack.ack_seq_first != expected_first
                        || ack.ack_seq_last != expected_last
                    {
                        state.live_ingest = LiveIngestState::Closed;
                        state.ingest_mailbox = LiveIngestMailbox::default();
                        acknowledgement_error = Some(BridgeError::browser(
                            "acknowledging SeedLink data",
                            "stale or out-of-order ingest acknowledgement",
                        ));
                    } else {
                        state.scene = ack.scene.clone();
                        state.scene_gen += 1;
                        clear_view(&mut state);
                        if let LiveIngestState::Lagged { dropped_bytes, .. } = state.live_ingest
                        {
                            log::warn!(
                                "SeedLink resumed after acknowledging a gap of {dropped_bytes} bytes"
                            );
                        }
                        state.live_ingest = LiveIngestState::Streaming {
                            stream,
                            session,
                            next_seq: expected_last.wrapping_add(1),
                        };
                    }
                } else {
                    // Batch N failed terminally; dispatching the buffered
                    // successors would mutate the session after the stream's
                    // terminal failure.
                    state.live_ingest = LiveIngestState::Closed;
                    state.ingest_mailbox = LiveIngestMailbox::default();
                }
                if matches!(state.live_ingest, LiveIngestState::Streaming { .. }) {
                    take_live_mailbox(&mut state, stream, session, start_time, retain_s)
                } else {
                    None
                }
            };
            if let Some(pending) = pending {
                dispatch_live_ingest(
                    work_inner,
                    work_repaint,
                    pending,
                    stream,
                    session,
                    start_time,
                    retain_s,
                );
            }
            if let Some(error) = acknowledgement_error {
                spawn_local(async {
                    LIVE_SEEDLINK.with(|slot| {
                        slot.borrow_mut().take();
                    });
                });
                return Err(error);
            }
            if failed {
                spawn_local(async {
                    LIVE_SEEDLINK.with(|slot| {
                        slot.borrow_mut().take();
                    });
                });
            }
            parsed.map(|_| ()).map_err(BridgeError::from)
        },
    );
    if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
        job_slot.set(Some(handle));
        inner.borrow_mut().ingest_job = Some(handle);
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
        let session = retire_live_source(&self.inner);
        let stream = {
            let mut state = self.inner.borrow_mut();
            state.next_stream_id = state.next_stream_id.wrapping_add(1).max(1);
            let stream = StreamId(state.next_stream_id);
            state.live_ingest = LiveIngestState::Opening { stream, session };
            stream
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
                js_clear_open_sources();
                LIVE_SEEDLINK.with(|slot| {
                    *slot.borrow_mut() = Some(handle);
                });
                let mut i = self.inner.borrow_mut();
                i.live_seedlink_started_at = Some(started_at);
                i.live_seedlink_follow_s = follow_s;
                i.scene = Some(SceneMeta {
                    tmin: started_at,
                    tmax: started_at + follow_s,
                    tracks: Vec::new(),
                });
                i.scene_gen += 1;
                i.diagnostic = None;
                i.snuffling = None;
                clear_event_state(&mut i);
                i.station_dists.clear();
                i.station_dists_gen = None;
                clear_view(&mut i);
            }
            Err(err) => {
                let mut i = self.inner.borrow_mut();
                i.live_seedlink_started_at = None;
                i.live_ingest = LiveIngestState::Closed;
                i.diagnostic = Some(err.into());
            }
        }
        repaint.request_repaint();
    }

    /// Stops live `SeedLink` acquisition while retaining received samples.
    pub fn stop_seedlink(&self) {
        retire_live_source(&self.inner);
        set_snuffling_text(&self.inner, "SeedLink stopped".to_owned(), false);
    }
}
