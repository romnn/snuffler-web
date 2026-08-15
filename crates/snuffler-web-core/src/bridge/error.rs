//! Why a bridge operation did not produce what the frontend asked for.
//!
//! Three sources of failure meet here, and keeping them apart is the point: an
//! rpc that pyrocko rejected ([`snuffler_web_proto::CallError`]), a browser API
//! that threw or handed back the wrong shape, and the `SeedLink` socket. They
//! read alike once formatted, but they are not alike to act on — a rejected
//! request is the user's to fix, a dead worker wants a reload, and a mismatched
//! payload is a schema drift no user can do anything about.
//!
//! Deliberately free of `JsValue`: the error crosses into the egui frontend and
//! is stored in [`crate::bridge::Status`], both of which build on the host.

use snuffler_web_proto::CallError;

/// A failed bridge operation, whatever layer it failed at.
#[derive(Clone, Debug, thiserror::Error)]
pub enum BridgeError {
    /// A `BridgeService` rpc did not return its response message.
    #[error(transparent)]
    Call(#[from] CallError),

    /// A DOM-only browser call — a file picker, a directory read, a download —
    /// rejected. These have no proto envelope to carry a structured error, so
    /// what the browser threw is all there is.
    #[error("{action} failed: {detail}")]
    Browser {
        /// What the user asked for, e.g. `open files`.
        action: &'static str,
        /// The JS exception, stringified at the boundary.
        detail: String,
    },

    /// `web/bridge.js` returned a value that is not the shape its contract
    /// promises. Always a bug in the glue rather than anything the user did.
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
