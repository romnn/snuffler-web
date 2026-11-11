//! Allowlisted WebSocket-to-TCP bridge for browser `SeedLink` clients.

use std::fmt;
use std::num::ParseIntError;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, info, warn};

const TCP_BUFFER_BYTES: usize = 64 * 1024;

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

    /// Handles one upgrade request and starts forwarding after a successful upgrade.
    pub fn handle(&self, ws: WebSocketUpgrade, req: &Request<Body>) -> Response<Body> {
        match tcp_target(req.uri().query()) {
            Ok(target) if self.allowlist.contains(&target) => {
                let connect_timeout = self.connect_timeout;
                ws.on_upgrade(move |socket| {
                    bridge_websocket_to_tcp(socket, connect_timeout, target)
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct TcpTarget {
    host: String,
    port: u16,
}

impl fmt::Display for TcpTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Why a requested or configured TCP target was rejected.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    /// The request named neither a `target` nor a `host`.
    #[error("missing host query parameter")]
    MissingHost,
    /// The request named a `host` without a `port`.
    #[error("missing port query parameter")]
    MissingPort,
    /// A `host:port` target had no `:` separator.
    #[error("target must be host:port")]
    MissingPortSeparator,
    /// The port is not a number in `0..=65535`.
    #[error("invalid port {port:?}")]
    InvalidPort {
        /// The rejected port text.
        port: String,
        /// Why it did not parse.
        #[source]
        source: ParseIntError,
    },
    /// The host is empty after trimming whitespace and IPv6 brackets.
    #[error("host must not be empty")]
    EmptyHost,
    /// The host contains whitespace.
    #[error("host must not contain whitespace")]
    HostContainsWhitespace,
    /// Port `0` cannot be connected to.
    #[error("port must be greater than zero")]
    ZeroPort,
}

/// Why one direction of an established relay stopped.
///
/// Both variants are transparent because the log line already names the direction.
#[derive(Debug, thiserror::Error)]
enum RelayError {
    #[error(transparent)]
    WebSocket(#[from] axum::Error),
    #[error(transparent)]
    Tcp(#[from] std::io::Error),
}

/// TCP targets accepted by a [`Proxy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Allowlist {
    targets: Vec<AllowedTcpTarget>,
}

impl Allowlist {
    /// Extends the built-in `SeedLink` targets with deployment-specific targets.
    pub fn with_default_targets<I>(extra_targets: I) -> Self
    where
        I: IntoIterator<Item = AllowedTcpTarget>,
    {
        let mut targets = default_allowed_targets();
        targets.extend(extra_targets);
        Self { targets }
    }

    fn contains(&self, target: &TcpTarget) -> bool {
        self.targets.iter().any(|allowed| allowed.matches(target))
    }
}

/// One allowed TCP host and port pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedTcpTarget {
    host: String,
    port: u16,
}

impl AllowedTcpTarget {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.trim().to_ascii_lowercase(),
            port,
        }
    }

    /// Parses a `host:port` target rule.
    ///
    /// # Errors
    ///
    /// Returns [`TargetError::MissingPortSeparator`] without a `:`,
    /// [`TargetError::InvalidPort`] or [`TargetError::ZeroPort`] for an unusable port, and
    /// [`TargetError::EmptyHost`] or [`TargetError::HostContainsWhitespace`] for an unusable
    /// host.
    pub fn parse(value: &str) -> Result<Self, TargetError> {
        let target = parse_tcp_target(value)?;
        Ok(Self::new(&target.host, target.port))
    }

    fn matches(&self, target: &TcpTarget) -> bool {
        target.host.eq_ignore_ascii_case(&self.host) && target.port == self.port
    }
}

/// The app's default live `SeedLink` server, plus GEOFON's.
fn default_allowed_targets() -> Vec<AllowedTcpTarget> {
    [
        AllowedTcpTarget::new("rtserve.iris.washington.edu", 18000),
        AllowedTcpTarget::new("geofon.gfz.de", 18000),
    ]
    .into()
}

/// Reads the target from a `host=…&port=…` query.
fn tcp_target(query: Option<&str>) -> Result<TcpTarget, TargetError> {
    let mut host = None;
    let mut port = None;

    for (key, value) in query
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
    {
        match key.as_ref() {
            "host" => host = Some(value.into_owned()),
            "port" => port = Some(value.into_owned()),
            _ => {}
        }
    }

    let host = host.ok_or(TargetError::MissingHost)?;
    let port = port.ok_or(TargetError::MissingPort)?;
    validate_tcp_target(&host, parse_port(&port)?)
}

/// Parses `host:port`, splitting at the last `:` so a bracketed IPv6 host stays whole.
fn parse_tcp_target(target: &str) -> Result<TcpTarget, TargetError> {
    let (host, port) = target
        .rsplit_once(':')
        .ok_or(TargetError::MissingPortSeparator)?;
    validate_tcp_target(host, parse_port(port)?)
}

fn parse_port(port: &str) -> Result<u16, TargetError> {
    port.parse().map_err(|source| TargetError::InvalidPort {
        port: port.to_owned(),
        source,
    })
}

fn validate_tcp_target(host: &str, port: u16) -> Result<TcpTarget, TargetError> {
    let host = host.trim().trim_matches(['[', ']']).to_owned();
    if host.is_empty() {
        return Err(TargetError::EmptyHost);
    }
    if host.chars().any(char::is_whitespace) {
        return Err(TargetError::HostContainsWhitespace);
    }
    if port == 0 {
        return Err(TargetError::ZeroPort);
    }
    Ok(TcpTarget { host, port })
}

async fn bridge_websocket_to_tcp(socket: WebSocket, connect_timeout: Duration, target: TcpTarget) {
    let connect = TcpStream::connect((target.host.as_str(), target.port));
    let Ok(Ok(tcp)) = timeout(connect_timeout, connect).await else {
        warn!(%target, "failed to connect websocket tcp target");
        return;
    };
    if let Err(err) = tcp.set_nodelay(true) {
        debug!(%target, %err, "failed to disable tcp nodelay");
    }
    info!(%target, "websocket tcp proxy connected");

    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut ws_send, mut ws_recv) = socket.split();
    let to_tcp = async {
        while let Some(message) = ws_recv.next().await {
            match message? {
                Message::Binary(bytes) => tcp_write.write_all(&bytes).await?,
                Message::Text(text) => tcp_write.write_all(text.as_bytes()).await?,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
            }
        }
        tcp_write.shutdown().await?;
        Ok::<(), RelayError>(())
    };
    let to_ws = async {
        let mut buf = vec![0_u8; TCP_BUFFER_BYTES];
        loop {
            let nread = tcp_read.read(&mut buf).await?;
            if nread == 0 {
                break;
            }
            let Some(chunk) = buf.get(..nread) else {
                break;
            };
            ws_send
                .send(Message::Binary(Bytes::copy_from_slice(chunk)))
                .await?;
        }
        // The browser may already have gone away, in which case there is no one to tell.
        if let Err(err) = ws_send.send(Message::Close(None)).await {
            debug!(%target, %err, "failed to send websocket close frame");
        }
        Ok::<(), RelayError>(())
    };

    // Whichever direction ends first tears down the whole relay
    tokio::select! {
        result = to_tcp => {
            if let Err(err) = result {
                debug!(%target, %err, "websocket-to-tcp leg ended with error");
            }
        }
        result = to_ws => {
            if let Err(err) = result {
                debug!(%target, %err, "tcp-to-websocket leg ended with error");
            }
        }
    }
    info!(%target, "websocket tcp proxy disconnected");
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre;

    use super::*;

    #[test]
    fn parses_tcp_target_pair() -> eyre::Result<()> {
        let target = tcp_target(Some("host=geofon.gfz.de&port=18000"))?;
        assert_eq!(target.to_string(), "geofon.gfz.de:18000");
        Ok(())
    }

    #[test]
    fn rejects_missing_target() {
        assert!(matches!(tcp_target(None), Err(TargetError::MissingHost)));
    }

    #[test]
    fn rejects_unusable_ports() {
        // Not a number, out of range, and the unconnectable zero port
        assert!(matches!(
            tcp_target(Some("host=geofon.gfz.de&port=seedlink")),
            Err(TargetError::InvalidPort { .. })
        ));
        assert!(matches!(
            AllowedTcpTarget::parse("geofon.gfz.de:70000"),
            Err(TargetError::InvalidPort { .. })
        ));
        assert!(matches!(
            AllowedTcpTarget::parse("geofon.gfz.de:0"),
            Err(TargetError::ZeroPort)
        ));
    }

    #[test]
    fn default_allowlist_accepts_known_seedlink_hosts() -> eyre::Result<()> {
        let target = tcp_target(Some("host=rtserve.iris.washington.edu&port=18000"))?;
        assert!(Allowlist::with_default_targets([]).contains(&target));
        Ok(())
    }

    #[test]
    fn default_allowlist_rejects_other_hosts() -> eyre::Result<()> {
        let target = tcp_target(Some("host=example.org&port=18000"))?;
        assert!(!Allowlist::with_default_targets([]).contains(&target));
        Ok(())
    }

    #[test]
    fn default_allowlist_rejects_other_ports() -> eyre::Result<()> {
        let target = tcp_target(Some("host=rtserve.iris.washington.edu&port=22"))?;
        assert!(!Allowlist::with_default_targets([]).contains(&target));
        Ok(())
    }
}
