//! Allowlisted TCP-over-WebSocket proxy for browser clients that speak a raw TCP protocol.
//!
//! A browser cannot open a raw TCP socket, so it asks for `?host=…&port=…` on the route the
//! caller mounts this proxy at.
//! An allowlisted target is upgraded to a WebSocket whose binary frames carry the TCP bytes.

mod metrics;
mod relay;
mod target;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use tracing::Instrument as _;

use self::metrics::{Unrelayed, count_unrelayed};
use self::relay::relay_websocket_to_tcp;
use self::target::target_from_query;
pub use self::target::{AllowedTcpTarget, Allowlist, TargetError};

/// Upgrades accepted requests and forwards frames to an allowed TCP target.
#[derive(Clone)]
pub struct Proxy {
    connect_timeout: Duration,
    allowlist: Arc<Allowlist>,
}

impl Proxy {
    /// Creates a proxy connecting to the targets in `allowlist`.
    #[must_use]
    pub fn new(connect_timeout: Duration, allowlist: Allowlist) -> Self {
        Self {
            connect_timeout,
            allowlist: Arc::new(allowlist),
        }
    }

    /// Handles one upgrade request and starts relaying after a successful upgrade.
    ///
    /// Every request counts towards `tcp_over_ws_proxy_connections`, a relayed one once it has
    /// connected or failed to.
    /// A relay also records how many relays are active, how long they last, and the bytes they
    /// carry.
    pub fn handle(&self, upgrade: WebSocketUpgrade, request: &Request<Body>) -> Response<Body> {
        match target_from_query(request.uri().query()) {
            Ok(target) => {
                if let Some(allowed) = self.allowlist.allowed(&target) {
                    self.relay(upgrade, allowed.clone())
                } else {
                    count_unrelayed(Unrelayed::Refused);
                    tracing::info!(%target, "refused a target outside the allowlist");
                    text_response(
                        StatusCode::FORBIDDEN,
                        format!("target is not allowed: {target}\n"),
                    )
                }
            }
            Err(err) => {
                count_unrelayed(Unrelayed::Invalid);
                tracing::info!(%err, "refused an invalid target");
                text_response(StatusCode::BAD_REQUEST, format!("{err}\n"))
            }
        }
    }

    /// Upgrades to a WebSocket and relays it to `target`.
    fn relay(&self, upgrade: WebSocketUpgrade, target: AllowedTcpTarget) -> Response<Body> {
        // The relay runs as long as the stream, long after the upgrade is answered, so it gets
        // a trace of its own, linked to the request that opened it.
        // As a child it would keep the request's span open until the relay ends.
        // The span is opened here rather than by `#[instrument]` on the relay, which would open
        // it on the first poll, after the upgrade is answered, when the request span it links to
        // may already be closed.
        let relay_span = tracing::info_span!(parent: None, "tcp_relay", upstream = %target);
        relay_span.follows_from(tracing::Span::current());
        let connect_timeout = self.connect_timeout;
        upgrade
            .on_upgrade(move |socket| {
                relay_websocket_to_tcp(socket, connect_timeout, target).instrument(relay_span)
            })
            .into_response()
    }
}

fn text_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
