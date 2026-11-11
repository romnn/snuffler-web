//! Allowlisted WebSocket-to-TCP relay for browser `SeedLink` clients.
//!
//! A browser cannot open a raw TCP socket, so it asks for `/seedlink?host=…&port=…` instead.
//! An allowlisted target is upgraded to a WebSocket whose binary frames carry the TCP bytes.

mod relay;
mod target;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;

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
    pub fn handle(&self, upgrade: WebSocketUpgrade, request: &Request<Body>) -> Response<Body> {
        match target_from_query(request.uri().query()) {
            Ok(target) if self.allowlist.contains(&target) => {
                let connect_timeout = self.connect_timeout;
                upgrade
                    .on_upgrade(move |socket| {
                        relay_websocket_to_tcp(socket, connect_timeout, target)
                    })
                    .into_response()
            }
            Ok(target) => text_response(
                StatusCode::FORBIDDEN,
                format!("target is not allowed: {target}\n"),
            ),
            Err(err) => text_response(StatusCode::BAD_REQUEST, format!("{err}\n")),
        }
    }
}

fn text_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
