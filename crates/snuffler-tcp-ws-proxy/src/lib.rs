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

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const TCP_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct Proxy {
    connect_timeout: Duration,
    allowlist: Arc<Allowlist>,
}

impl Proxy {
    pub fn new(connect_timeout: Duration) -> Self {
        Self::new_with_allowlist(connect_timeout, Allowlist::default())
    }

    pub fn new_with_allowlist(connect_timeout: Duration, allowlist: Allowlist) -> Self {
        Self {
            connect_timeout,
            allowlist: Arc::new(allowlist),
        }
    }

    pub async fn handle(&self, ws: WebSocketUpgrade, req: Request<Body>) -> Response<Body> {
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
                format!("target is not allowed: {}\n", target.display()),
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

impl TcpTarget {
    fn display(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Allowlist {
    targets: Vec<AllowedTcpTarget>,
}

impl Allowlist {
    pub fn new(targets: Vec<AllowedTcpTarget>) -> Self {
        Self { targets }
    }

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

impl Default for Allowlist {
    fn default() -> Self {
        Self {
            targets: default_allowed_targets(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedTcpTarget {
    host: String,
    port: u16,
}

impl AllowedTcpTarget {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into().trim().to_ascii_lowercase(),
            port,
        }
    }

    pub fn parse(value: &str) -> Result<Self, BoxError> {
        let target = parse_tcp_target(value)?;
        Ok(Self::new(target.host, target.port))
    }

    fn matches(&self, target: &TcpTarget) -> bool {
        target.host.eq_ignore_ascii_case(&self.host) && target.port == self.port
    }
}

fn default_allowed_targets() -> Vec<AllowedTcpTarget> {
    [
        AllowedTcpTarget::new("rtserve.iris.washington.edu", 18000),
        AllowedTcpTarget::new("geofon.gfz.de", 18000),
    ]
    .into()
}

fn tcp_target(query: Option<&str>) -> Result<TcpTarget, BoxError> {
    let mut host = None;
    let mut port = None;
    let mut target = None;

    for (key, value) in query
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
    {
        match key.as_ref() {
            "host" => host = Some(value.into_owned()),
            "port" => port = Some(value.into_owned()),
            "target" => target = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(target) = target {
        parse_tcp_target(&target)
    } else {
        let host = host.ok_or_else(|| "missing host query parameter".to_owned())?;
        let port = port
            .ok_or_else(|| "missing port query parameter".to_owned())?
            .parse::<u16>()?;
        validate_tcp_target(host, port)
    }
}

fn parse_tcp_target(target: &str) -> Result<TcpTarget, BoxError> {
    let Some((host, port)) = target.rsplit_once(':') else {
        return Err("target must be host:port".into());
    };
    validate_tcp_target(host.to_owned(), port.parse::<u16>()?)
}

fn validate_tcp_target(host: String, port: u16) -> Result<TcpTarget, BoxError> {
    let host = host.trim().trim_matches(['[', ']']).to_owned();
    if host.is_empty() {
        return Err("host must not be empty".into());
    }
    if host.chars().any(char::is_whitespace) {
        return Err("host must not contain whitespace".into());
    }
    if port == 0 {
        return Err("port must be greater than zero".into());
    }
    Ok(TcpTarget { host, port })
}

async fn bridge_websocket_to_tcp(socket: WebSocket, connect_timeout: Duration, target: TcpTarget) {
    let label = target.display();
    let connect = TcpStream::connect((target.host.as_str(), target.port));
    let Ok(Ok(tcp)) = timeout(connect_timeout, connect).await else {
        warn!(target = %label, "failed to connect websocket tcp target");
        return;
    };
    if let Err(err) = tcp.set_nodelay(true) {
        debug!(target = %label, %err, "failed to disable tcp nodelay");
    }
    info!(target = %label, "websocket tcp proxy connected");

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
        Ok::<(), BoxError>(())
    };
    let to_ws = async {
        let mut buf = vec![0_u8; TCP_BUFFER_BYTES];
        loop {
            let nread = tcp_read.read(&mut buf).await?;
            if nread == 0 {
                break;
            }
            ws_send
                .send(Message::Binary(Bytes::copy_from_slice(&buf[..nread])))
                .await?;
        }
        let _ = ws_send.send(Message::Close(None)).await;
        Ok::<(), BoxError>(())
    };

    tokio::select! {
        result = to_tcp => {
            if let Err(err) = result {
                debug!(target = %label, %err, "websocket-to-tcp leg ended with error");
            }
        }
        result = to_ws => {
            if let Err(err) = result {
                debug!(target = %label, %err, "tcp-to-websocket leg ended with error");
            }
        }
    }
    info!(target = %label, "websocket tcp proxy disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_target_pair() {
        let parsed =
            tcp_target(Some("host=geofon.gfz.de&port=18000")).map(|target| target.display());
        assert_eq!(parsed.as_deref().ok(), Some("geofon.gfz.de:18000"));
    }

    #[test]
    fn parses_tcp_target_shorthand() {
        let parsed = tcp_target(Some("target=rtserve.iris.washington.edu%3A18000"))
            .map(|target| target.display());
        assert_eq!(
            parsed.as_deref().ok(),
            Some("rtserve.iris.washington.edu:18000")
        );
    }

    #[test]
    fn rejects_missing_target() {
        assert!(tcp_target(None).is_err());
    }

    #[test]
    fn default_allowlist_accepts_known_seedlink_hosts() {
        let target = tcp_target(Some("target=rtserve.iris.washington.edu%3A18000")).unwrap();
        assert!(Allowlist::default().contains(&target));
    }

    #[test]
    fn default_allowlist_rejects_other_hosts() {
        let target = tcp_target(Some("target=example.org%3A18000")).unwrap();
        assert!(!Allowlist::default().contains(&target));
    }

    #[test]
    fn default_allowlist_rejects_other_ports() {
        let target = tcp_target(Some("target=rtserve.iris.washington.edu%3A22")).unwrap();
        assert!(!Allowlist::default().contains(&target));
    }
}
