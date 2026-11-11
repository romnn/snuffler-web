//! The WebSocket to the TCP proxy that carries the `SeedLink` stream: the handshake it
//! sends on open, the framing of the packets it receives, and how its errors and closing end
//! the stream.

use crate::seedlink::{FrameError, MAX_FRAME_BUFFER_BYTES, drain_records, v4_selectors};

use super::*;

/// The live socket plus the JS callbacks it invokes.
///
/// The closures are owned here because the socket calls them until [`Drop`]
/// detaches them; dropping one earlier would leave the socket a dangling callback.
pub(super) struct LiveSocket {
    socket: web_sys::WebSocket,
    #[expect(
        dead_code,
        reason = "keeps the socket's open callback alive until drop"
    )]
    on_open: Closure<dyn FnMut(web_sys::Event)>,
    #[expect(
        dead_code,
        reason = "keeps the socket's message callback alive until drop"
    )]
    on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    #[expect(
        dead_code,
        reason = "keeps the socket's error callback alive until drop"
    )]
    on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
    #[expect(
        dead_code,
        reason = "keeps the socket's close callback alive until drop"
    )]
    on_close: Closure<dyn FnMut(web_sys::Event)>,
}

impl Drop for LiveSocket {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        if let Err(err) = self.socket.close() {
            tracing::warn!(
                err = js_value_to_string(err),
                "closing the SeedLink socket failed"
            );
        }
    }
}

impl LiveSocket {
    /// Opens the socket and wires its callbacks to the stream they feed.
    pub(super) fn connect(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        params: SeedlinkParams,
        started_at: f64,
        stream: StreamId,
        session: LiveSessionEpoch,
    ) -> Result<Self, SeedlinkError> {
        let OpenedSocket {
            socket,
            source,
            handshake,
        } = open_socket(&params)?;
        let retain_s = if params.retain_s > 0.0 {
            params.retain_s
        } else {
            LIVE_RETAIN_SECONDS
        };
        let live = LiveStream {
            source: source.clone(),
            started_at,
            stream,
            session,
            retain_s,
        };

        let on_open = on_open(
            socket.clone(),
            inner.clone(),
            repaint.clone(),
            source.clone(),
            handshake,
        );
        let on_message = on_message(inner.clone(), repaint.clone(), live);
        let on_error = on_error(inner.clone(), repaint.clone(), stream, session);
        let on_close = on_close(inner, repaint, source, stream, session);

        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            socket,
            on_open,
            on_message,
            on_error,
            on_close,
        })
    }
}

struct OpenedSocket {
    socket: web_sys::WebSocket,
    /// The `host:port NET.STA selectors` label the pile shows as the source.
    source: String,
    /// The commands sent once the socket opens.
    handshake: Vec<String>,
}

fn open_socket(params: &SeedlinkParams) -> Result<OpenedSocket, SeedlinkError> {
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
    let Some(proxy) = super::proxy::base() else {
        return Err(SeedlinkError::Unavailable);
    };
    let url = super::proxy::tcp_url(&proxy, params.host.trim(), port).map_err(|err| {
        SeedlinkError::ProxyUrl {
            detail: js_value_to_string(err),
        }
    })?;
    let socket = web_sys::WebSocket::new(&url).map_err(|err| SeedlinkError::Connect {
        url,
        detail: js_value_to_string(err),
    })?;
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
    Ok(OpenedSocket {
        socket,
        source,
        handshake: handshake_commands(params),
    })
}

/// The `SeedLink` 4 handshake: select the station and channels, then start streaming.
fn handshake_commands(params: &SeedlinkParams) -> Vec<String> {
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
        commands.push(format!("SELECT {}\r", v4_selectors(selectors)));
    }
    commands.push("DATA\r".to_owned());
    commands.push("END\r".to_owned());
    commands
}

fn on_open(
    socket: web_sys::WebSocket,
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    source: String,
    handshake: Vec<String>,
) -> Closure<dyn FnMut(web_sys::Event)> {
    Closure::wrap(Box::new(move |_event: web_sys::Event| {
        for command in &handshake {
            if let Err(err) = socket.send_with_str(command) {
                set_diagnostic(
                    &inner,
                    SeedlinkError::Send {
                        command: command.clone(),
                        detail: js_value_to_string(err),
                    }
                    .into(),
                );
                repaint.request_repaint();
                return;
            }
        }
        tracing::info!(%source, "SeedLink connected");
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

/// Frames each message's packets and hands complete `miniSEED` records to ingest.
fn on_message(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    live: LiveStream,
) -> Closure<dyn FnMut(web_sys::MessageEvent)> {
    // An incomplete trailing packet waits here for the next message.
    let frame_buffer = Rc::new(RefCell::new(Vec::new()));
    let next_seq = Rc::new(Cell::new(0_u64));
    // Closes the stream on a framing failure and reports it, once.
    let fail = {
        let inner = inner.clone();
        let repaint = repaint.clone();
        let live = live.clone();
        move |error: FrameError| {
            if close_live_ingest(&inner, live.stream, live.session) {
                set_diagnostic(&inner, SeedlinkError::from(error).into());
                repaint.request_repaint();
            }
        }
    };
    Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        let bytes = match message_bytes(&event) {
            Ok(bytes) if bytes.is_empty() => return,
            Ok(bytes) => bytes,
            Err(error) => return fail(error),
        };
        let buffered = frame_buffer.borrow().len().saturating_add(bytes.len());
        if buffered > MAX_FRAME_BUFFER_BYTES {
            return fail(FrameError::Oversized {
                announced: buffered,
                limit: MAX_FRAME_BUFFER_BYTES,
            });
        }
        let records = {
            let mut frame_buffer = frame_buffer.borrow_mut();
            frame_buffer.extend_from_slice(&bytes);
            drain_records(&mut frame_buffer)
        };
        let records = match records {
            Ok(records) => records,
            Err(error) => return fail(error),
        };
        if !records.is_empty() {
            let seq = next_seq.get();
            next_seq.set(seq.wrapping_add(1));
            ingest_live_mseed(inner.clone(), repaint.clone(), live.clone(), records, seq);
        }
    }) as Box<dyn FnMut(_)>)
}

fn on_error(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    stream: StreamId,
    session: LiveSessionEpoch,
) -> Closure<dyn FnMut(web_sys::ErrorEvent)> {
    Closure::wrap(Box::new(move |event: web_sys::ErrorEvent| {
        if !close_live_ingest(&inner, stream, session) {
            return;
        }
        let message = event.message();
        let detail = if message.is_empty() {
            "the browser gave no reason".to_owned()
        } else {
            message
        };
        set_diagnostic(&inner, SeedlinkError::Socket { detail }.into());
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

fn on_close(
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
    source: String,
    stream: StreamId,
    session: LiveSessionEpoch,
) -> Closure<dyn FnMut(web_sys::Event)> {
    Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if close_live_ingest(&inner, stream, session) {
            tracing::info!(%source, "SeedLink disconnected");
        }
        repaint.request_repaint();
    }) as Box<dyn FnMut(_)>)
}

/// The bytes of one socket message; an unknown payload type is an empty message.
fn message_bytes(event: &web_sys::MessageEvent) -> Result<Vec<u8>, FrameError> {
    let data = event.data();
    if data.is_instance_of::<js_sys::ArrayBuffer>() {
        let bytes = js_sys::Uint8Array::new(&data);
        let length = usize::try_from(bytes.byte_length()).unwrap_or(usize::MAX);
        if length > MAX_FRAME_BUFFER_BYTES {
            return Err(FrameError::Oversized {
                announced: length,
                limit: MAX_FRAME_BUFFER_BYTES,
            });
        }
        Ok(bytes.to_vec())
    } else if let Some(text) = data.as_string() {
        let length = text.len();
        if length > MAX_FRAME_BUFFER_BYTES {
            return Err(FrameError::Oversized {
                announced: length,
                limit: MAX_FRAME_BUFFER_BYTES,
            });
        }
        Ok(text.into_bytes())
    } else {
        Ok(Vec::new())
    }
}
