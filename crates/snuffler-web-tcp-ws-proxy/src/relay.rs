//! The relay itself: bytes from the WebSocket go to the TCP stream, and back.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::target::TcpTarget;

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
pub(crate) async fn relay_websocket_to_tcp(
    socket: WebSocket,
    connect_timeout: Duration,
    target: TcpTarget,
) {
    let connect = TcpStream::connect((target.host.as_str(), target.port));
    let Ok(Ok(tcp)) = tokio::time::timeout(connect_timeout, connect).await else {
        warn!(%target, "failed to connect websocket tcp target");
        return;
    };
    if let Err(err) = tcp.set_nodelay(true) {
        debug!(%target, %err, "failed to disable tcp nodelay");
    }
    info!(%target, "websocket tcp proxy connected");

    let (mut tcp_reader, mut tcp_writer) = tcp.into_split();
    let (mut websocket_sender, mut websocket_receiver) = socket.split();

    let websocket_to_tcp = async {
        while let Some(message) = websocket_receiver.next().await {
            match message? {
                Message::Binary(bytes) => tcp_writer.write_all(&bytes).await?,
                Message::Text(text) => tcp_writer.write_all(text.as_bytes()).await?,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
            }
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
        }
        // The browser may already have gone away, in which case there is no one to tell.
        if let Err(err) = websocket_sender.send(Message::Close(None)).await {
            debug!(%target, %err, "failed to send websocket close frame");
        }
        Ok::<(), RelayError>(())
    };

    // Whichever direction ends first tears down the whole relay
    tokio::select! {
        result = websocket_to_tcp => {
            if let Err(err) = result {
                debug!(%target, %err, "websocket-to-tcp leg ended with error");
            }
        }
        result = tcp_to_websocket => {
            if let Err(err) = result {
                debug!(%target, %err, "tcp-to-websocket leg ended with error");
            }
        }
    }
    info!(%target, "websocket tcp proxy disconnected");
}
