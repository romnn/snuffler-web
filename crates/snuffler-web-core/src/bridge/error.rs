//! Why a bridge operation did not produce what the frontend asked for.
//!
//! The error variants preserve whether pyrocko rejected an RPC, a browser API
//! failed, a payload violated its protocol, or a `SeedLink` socket stopped.
//! They read alike once formatted, but callers must act on them differently.
//!
//! Deliberately free of `JsValue`: the error crosses into the egui frontend and is stored
//! in a job outcome or [`crate::bridge::Bridge::diagnostic`].
//! Both consumers build on the host.

use std::fmt;

use snuffler_web_proto::{CallError, RemoteError};

/// Why one source in a file-reload batch failed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ReloadFailureReason {
    /// The browser could not read the selected file handle or its bytes.
    #[error("browser read failed: {detail}")]
    BrowserRead {
        /// The exception reported by the File System Access API.
        detail: String,
    },
    /// The worker transport rejected before a protobuf response arrived.
    #[error("transport failed: {detail}")]
    Transport {
        /// The exception reported by the worker bridge.
        detail: String,
    },
    /// Pyrocko returned its typed exception and traceback.
    #[error(transparent)]
    Remote(#[from] RemoteError),
    /// The JavaScript side could not decode or classify the protobuf response.
    #[error("protocol failure: {detail}")]
    Protocol {
        /// The decode or unexpected-payload detail.
        detail: String,
    },
}

/// One independently failed source in a file-reload batch.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{name}: {reason}")]
pub struct ReloadSourceError {
    /// Stable identity of the open browser file handle.
    pub source_id: String,
    /// Latest filename reported by the handle.
    pub name: String,
    /// Layer that rejected this source.
    #[source]
    pub reason: ReloadFailureReason,
}

/// Independently failed sources from one otherwise usable reload batch.
#[derive(Clone, Debug)]
pub struct ReloadError {
    /// Failures in open-source iteration order.
    pub failures: Vec<ReloadSourceError>,
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, failure) in self.failures.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{failure}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ReloadError {}

/// A failed bridge operation, whatever layer it failed at.
#[derive(Clone, Debug, thiserror::Error)]
pub enum BridgeError {
    /// A `BridgeService` rpc did not return its response message.
    #[error(transparent)]
    Call(#[from] CallError),

    /// One or more sources failed in a batch whose other results remain usable.
    #[error("reloading files failed: {0}")]
    Reload(#[from] ReloadError),

    /// A DOM-only browser call — a file picker, a directory read, a download — rejected.
    /// These have no proto envelope to carry a structured error, so what the browser
    /// threw is all there is.
    #[error("{action} failed: {detail}")]
    Browser {
        /// What the user asked for, e.g. `open files`.
        action: &'static str,
        /// The JS exception, stringified at the boundary.
        detail: String,
    },

    /// `web/bridge.js` returned a value that is not the shape its contract promises.
    /// Always a bug in the glue rather than anything the user did.
    #[error(transparent)]
    Marshal(#[from] MarshalError),

    /// Live acquisition could not be established or kept up.
    #[error(transparent)]
    Seedlink(#[from] SeedlinkError),

    /// A `?example=<slug>` deep link named a dataset this build does not ship.
    #[error("no example dataset called {slug:?}")]
    UnknownExample {
        /// The slug that was asked for.
        slug: String,
    },
}

impl BridgeError {
    /// Records a rejected DOM-only browser call.
    pub fn browser(action: &'static str, detail: impl Into<String>) -> Self {
        Self::Browser {
            action,
            detail: detail.into(),
        }
    }

    /// The pyrocko traceback behind this failure, when the failure was pyrocko's.
    ///
    /// Everything else went wrong on this side of the boundary and carries a Rust
    /// source chain instead.
    #[must_use]
    pub fn traceback(&self) -> Option<&str> {
        match self {
            Self::Call(call) => call.traceback(),
            Self::Reload(reload) => reload.failures.iter().find_map(|failure| {
                if let ReloadFailureReason::Remote(remote) = &failure.reason
                    && !remote.traceback.is_empty()
                {
                    return Some(remote.traceback.as_str());
                }
                None
            }),
            _ => None,
        }
    }
}

/// A value handed back by `web/bridge.js` did not match its declared shape.
#[derive(Clone, Debug, thiserror::Error)]
pub enum MarshalError {
    /// The field is absent from the result object.
    #[error("the bridge result has no {field:?} field")]
    Missing {
        /// The key that was read.
        field: &'static str,
    },

    /// The field is present but is not the `Uint8Array` it is declared to be.
    #[error("the bridge result's {field:?} is not a Uint8Array")]
    NotBytes {
        /// The key that was read.
        field: &'static str,
    },

    /// A byte buffer that has to reinterpret as `f32` does not divide evenly.
    #[error("the bridge result's {field:?} is {len} bytes, not a whole number of f32 samples")]
    UnalignedSamples {
        /// The key that was read.
        field: &'static str,
        /// How many bytes actually arrived.
        len: u32,
    },
}

/// Live `SeedLink` acquisition could not be set up or driven.
#[derive(Clone, Debug, thiserror::Error)]
pub enum SeedlinkError {
    /// Packet framing exceeded the bounded incomplete-record buffer.
    #[error(transparent)]
    Frame(#[from] crate::seedlink::FrameError),

    /// The port box holds something that is not a TCP port.
    #[error("SeedLink port {port:?} is not a number between 1 and 65535")]
    Port {
        /// What the user typed.
        port: String,
        /// Why it did not parse.
        #[source]
        source: std::num::ParseIntError,
    },

    /// The `WebSocket` to the proxy could not be opened.
    #[error("could not open a SeedLink WebSocket to {url}: {detail}")]
    Connect {
        /// The proxy URL that was dialled.
        url: String,
        /// The JS exception, stringified at the boundary.
        detail: String,
    },

    /// The socket opened but rejected a handshake command.
    #[error("could not send {command:?} to the SeedLink server: {detail}")]
    Send {
        /// The `SeedLink` command that was refused.
        command: String,
        /// The JS exception, stringified at the boundary.
        detail: String,
    },

    /// The live socket reported an error after it had come up.
    #[error("the SeedLink connection failed: {detail}")]
    Socket {
        /// What the `ErrorEvent` carried; browsers often say very little here.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{BridgeError, ReloadError, ReloadFailureReason, ReloadSourceError};
    use snuffler_web_proto::RemoteError;

    #[test]
    fn reload_errors_surface_the_first_remote_traceback() {
        let error = BridgeError::from(ReloadError {
            failures: vec![
                ReloadSourceError {
                    source_id: "source-a".to_owned(),
                    name: "a.mseed".to_owned(),
                    reason: ReloadFailureReason::BrowserRead {
                        detail: "permission revoked".to_owned(),
                    },
                },
                ReloadSourceError {
                    source_id: "source-b".to_owned(),
                    name: "b.mseed".to_owned(),
                    reason: ReloadFailureReason::Remote(RemoteError {
                        kind: "ValueError".to_owned(),
                        message: "bad samples".to_owned(),
                        traceback: "python traceback".to_owned(),
                    }),
                },
            ],
        });
        assert_eq!(error.traceback(), Some("python traceback"));
    }
}
