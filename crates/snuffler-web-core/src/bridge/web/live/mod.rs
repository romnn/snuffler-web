//! The live `SeedLink` client.
//!
//! - `socket`: the WebSocket to the `SeedLink` proxy, its handshake and its callbacks;
//! - `ingest`: the bounded coalescing mailbox and the epoch-fenced `IngestMiniSeed`
//!   dispatch.

use super::*;

mod ingest;
mod socket;

use ingest::ingest_live_mseed;
use socket::LiveSocket;

thread_local! {
    /// The open socket, if a live session runs.
    static LIVE_SOCKET: RefCell<Option<LiveSocket>> = const { RefCell::new(None) };
}

/// What every callback of one socket needs to know about the stream it feeds.
#[derive(Clone)]
struct LiveStream {
    /// The `host:port NET.STA selectors` label the pile shows as the source.
    source: String,
    started_at: f64,
    stream: StreamId,
    session: LiveSessionEpoch,
    retain_s: f64,
}

/// Closes the stream if it is still the current one, and reports whether it was.
/// A terminally closed stream retires its socket too; otherwise the UI keeps
/// offering a live session whose every packet is rejected.
fn close_live_ingest(
    inner: &Rc<RefCell<Inner>>,
    stream: StreamId,
    session: LiveSessionEpoch,
) -> bool {
    {
        let mut state = inner.borrow_mut();
        if !state.live.ingest.is_current(stream, session) {
            return false;
        }
        state.live.retire();
    }
    defer_live_socket_retirement();
    true
}

/// Drops the socket on the next event-loop turn.
/// Retirement can run inside the socket's own callbacks, where dropping the
/// currently executing closure is forbidden.
pub(super) fn defer_live_socket_retirement() {
    #[expect(
        clippy::disallowed_methods,
        reason = "socket callbacks defer dropping their own currently executing closure"
    )]
    spawn_local(async {
        LIVE_SOCKET.with(|slot| {
            slot.borrow_mut().take();
        });
    });
}

/// Closes the socket and ends the live session, before a new source replaces the pile.
pub(super) fn retire_live_session(inner: &Rc<RefCell<Inner>>) {
    LIVE_SOCKET.with(|slot| {
        slot.borrow_mut().take();
    });
    inner.borrow_mut().live.retire();
}

impl Bridge {
    /// Reports whether a live `SeedLink` socket is active.
    #[must_use]
    pub fn seedlink_active(&self) -> bool {
        LIVE_SOCKET.with(|slot| slot.borrow().is_some())
    }

    /// Starts a live `SeedLink` source and replaces the current pile.
    pub fn start_seedlink(&self, params: SeedlinkParams) {
        let (stream, session) = {
            let state = self.inner.borrow();
            (
                StreamId(state.live.last_stream_id.wrapping_add(1).max(1)),
                LiveSessionEpoch(state.live.session_epoch.wrapping_add(1).max(1)),
            )
        };
        let started_at = js_sys::Date::now() / 1000.0;
        let follow_s = if params.follow_s > 0.0 {
            params.follow_s
        } else {
            LIVE_FOLLOW_SECONDS
        };
        match LiveSocket::connect(
            self.inner.clone(),
            self.repaint.clone(),
            params,
            started_at,
            stream,
            session,
        ) {
            Ok(socket) => {
                LIVE_SOCKET.with(|slot| {
                    *slot.borrow_mut() = Some(socket);
                });
                let mut state = self.inner.borrow_mut();
                state.live.retire();
                state.live.session_epoch = session.0;
                state.live.last_stream_id = stream.0;
                state.live.ingest = LiveIngestState::Opening { stream, session };
                state.live.follow_s = follow_s;
                state.diagnostic = None;
            }
            Err(err) => {
                self.inner.borrow_mut().diagnostic = Some(err.into());
            }
        }
        self.repaint.request_repaint();
    }

    /// Stops live `SeedLink` acquisition while retaining received samples.
    pub fn stop_seedlink(&self) {
        retire_live_session(&self.inner);
        self.inner
            .borrow_mut()
            .install_snuffling_text("SeedLink stopped".to_owned(), false);
    }
}
