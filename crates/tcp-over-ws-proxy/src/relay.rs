//! The relay itself: bytes from the WebSocket go to the TCP stream, and back.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::metrics::{ActiveRelay, ConnectFailure, Direction, RelayedBytes, count_failed};
use crate::target::AllowedTcpTarget;

/// Size of one TCP read, and so the largest WebSocket frame the relay sends.
const TCP_READ_BUFFER_BYTES: usize = 64 * 1024;

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

/// Connects to `target` and relays bytes both ways until either side closes.
///
/// Counts the connection, the relay among the active ones while it runs, and the bytes as they
/// pass; see [`crate::metrics`].
pub(crate) async fn relay_websocket_to_tcp(
    socket: WebSocket,
    connect_timeout: Duration,
    target: AllowedTcpTarget,
) {
    let connect = TcpStream::connect((target.host(), target.port()));
    let tcp = match tokio::time::timeout(connect_timeout, connect).await {
        Ok(Ok(tcp)) => tcp,
        Ok(Err(err)) => {
            count_failed(&target, ConnectFailure::Connect);
            warn!(%err, "failed to connect to the tcp target");
            return;
        }
        Err(_elapsed) => {
            count_failed(&target, ConnectFailure::Timeout);
            warn!(?connect_timeout, "timed out connecting to the tcp target");
            return;
        }
    };
    if let Err(err) = tcp.set_nodelay(true) {
        debug!(%err, "failed to disable tcp nodelay");
    }
    // Counts the relay as active until this function returns or its task is dropped.
    let _active = ActiveRelay::open(&target);
    info!("connected to the tcp target");

    let (mut tcp_reader, mut tcp_writer) = tcp.into_split();
    let (mut websocket_sender, mut websocket_receiver) = socket.split();

    let mut bytes_to_tcp = RelayedBytes::new(&target, Direction::ToTcp);
    let mut bytes_to_websocket = RelayedBytes::new(&target, Direction::ToWebSocket);

    let websocket_to_tcp = async {
        while let Some(message) = websocket_receiver.next().await {
            let message = message?;
            let bytes: &[u8] = match &message {
                Message::Binary(bytes) => bytes,
                Message::Text(text) => text.as_bytes(),
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            tcp_writer.write_all(bytes).await?;
            bytes_to_tcp.add(bytes.len());
        }
        tcp_writer.shutdown().await?;
        Ok::<(), RelayError>(())
    };

    let tcp_to_websocket = async {
        let mut buffer = vec![0_u8; TCP_READ_BUFFER_BYTES];
        loop {
            let bytes_read = tcp_reader.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            let Some(chunk) = buffer.get(..bytes_read) else {
                break;
            };
            websocket_sender
                .send(Message::Binary(Bytes::copy_from_slice(chunk)))
                .await?;
            bytes_to_websocket.add(bytes_read);
        }
        // The browser may already have gone away, in which case there is no one to tell.
        if let Err(err) = websocket_sender.send(Message::Close(None)).await {
            debug!(%err, "failed to send websocket close frame");
        }
        Ok::<(), RelayError>(())
    };

    // Whichever direction ends first tears down the whole relay
    tokio::select! {
        result = websocket_to_tcp => {
            if let Err(err) = result {
                debug!(%err, "websocket-to-tcp leg ended with error");
            }
        }
        result = tcp_to_websocket => {
            if let Err(err) = result {
                debug!(%err, "tcp-to-websocket leg ended with error");
            }
        }
    }
    info!(
        bytes_to_tcp = bytes_to_tcp.total(),
        bytes_to_websocket = bytes_to_websocket.total(),
        "disconnected from the tcp target"
    );
}
