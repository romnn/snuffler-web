//! What can go wrong on one trip across the bridge.
//!
//! Every `BridgeService` response is a `BridgeResult` envelope carrying either a
//! payload or a `BridgeError`, because pyrocko never raises across the JS boundary.
//! These types are the Rust half of that contract: the generated client from
//! `build/client.rs` produces them, and they are the only way a bridge call reports
//! failure.

use std::fmt;

use crate as pb;

/// The bridge transport itself failed, before any envelope existed.
///
/// The detail is whatever the browser threw, since a JS exception carries no
/// structure worth recovering.
/// The *type* still separates "the worker is gone" from a request pyrocko
/// rejected: one is fatal and wants a reload, the other is the user's to fix.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{method}: {detail}")]
pub struct TransportError {
    /// The rpc that was in flight, as its `/package.Service/Method` id.
    pub method: &'static str,
    /// What the browser reported.
    pub detail: String,
}

impl TransportError {
    /// Records a rejected transport call.
    pub fn new(method: &'static str, detail: impl Into<String>) -> Self {
        Self {
            method,
            detail: detail.into(),
        }
    }
}

/// The exception pyrocko raised, carried back as a DTO rather than thrown.
///
/// The traceback travels with it because it is what makes a bug report
/// actionable, and there is nowhere else to recover it from.
#[derive(Clone, Debug, thiserror::Error)]
pub struct RemoteError {
    /// The Python exception class, e.g. `ValueError`.
    pub kind: String,
    /// `str(exc)`, which an exception raised with no arguments leaves empty.
    pub message: String,
    /// `traceback.format_exc()`, for a details view or the console.
    pub traceback: String,
}

// Handwritten rather than `#[error("{kind}: {message}")]`, which would render a dangling
// separator for `raise ValueError()` — an exception is allowed to carry no message at
// all.
// (thiserror's `fmt = ` escape hatch is enum-only.)
impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.kind, self.message)
        }
    }
}

impl From<pb::BridgeError> for RemoteError {
    fn from(error: pb::BridgeError) -> Self {
        Self {
            kind: error.kind,
            message: error.message,
            traceback: error.traceback,
        }
    }
}

/// One `BridgeService` call that did not come back with its response message.
#[derive(Clone, Debug, thiserror::Error)]
pub enum CallError {
    /// The call never reached pyrocko.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// pyrocko answered, and the answer was an exception.
    #[error(transparent)]
    Remote(#[from] RemoteError),

    /// Cancellation won before the dispatcher committed.
    #[error("{method}: cancelled")]
    Cancelled {
        /// The rpc that was cancelled.
        method: &'static str,
    },

    /// The defensive session generation check refused a commit.
    #[error(
        "{method}: stale session generation (expected {expected_generation}, actual {actual_generation})"
    )]
    Stale {
        /// The rpc whose result could not commit.
        method: &'static str,
        /// Generation captured at dispatch.
        expected_generation: u64,
        /// Generation current at commit.
        actual_generation: u64,
    },

    /// The bytes that came back are not a `BridgeResult` at all.
    #[error("{method}: the response was not a decodable BridgeResult")]
    Decode {
        /// The rpc that was in flight.
        method: &'static str,
        /// Where prost gave up.
        #[source]
        source: prost::DecodeError,
    },

    /// A well-formed envelope with neither a payload nor an error in it.
    #[error("{method}: the response envelope was empty")]
    Empty {
        /// The rpc that was in flight.
        method: &'static str,
    },

    /// The envelope carried a payload, but not the one this rpc is declared to
    /// return — the schema and the generated ends have drifted apart.
    #[error("{method}: the response envelope carried another rpc's payload")]
    UnexpectedPayload {
        /// The rpc that was in flight.
        method: &'static str,
    },
}

impl CallError {
    /// The pyrocko traceback behind this failure, when there is one.
    ///
    /// Only a [`CallError::Remote`] has one.
    /// Everything else went wrong on this side of the boundary and has a Rust
    /// source chain instead.
    #[must_use]
    pub fn traceback(&self) -> Option<&str> {
        match self {
            Self::Remote(remote) if !remote.traceback.is_empty() => Some(&remote.traceback),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use color_eyre::eyre::{self, bail, ensure};
    use prost::Message as _;

    use super::{CallError, TransportError};
    use crate::{BridgeResult, BridgeServiceClient, BridgeTransport, MapRequest, bridge_result};

    /// A transport that answers every rpc with one canned envelope, so the
    /// generated client's envelope handling can be exercised without a browser.
    struct CannedTransport(Vec<u8>);

    // Answering from memory rather than with `async fn`: these never suspend, and
    // a ready future is what lets `poll_once` below drive them without a runtime.
    impl BridgeTransport for CannedTransport {
        fn unary(
            &self,
            _operation: crate::Operation,
            _method: &'static str,
            _request: Vec<u8>,
        ) -> impl Future<Output = Result<Vec<u8>, TransportError>> {
            std::future::ready(Ok(self.0.clone()))
        }

        fn unary_with_buffer(
            &self,
            _operation: crate::Operation,
            _method: &'static str,
            _request: Vec<u8>,
        ) -> impl Future<Output = Result<(Vec<u8>, Vec<f32>), TransportError>> {
            std::future::ready(Ok((self.0.clone(), Vec::new())))
        }
    }

    /// A transport whose worker is gone — the failure that must never be mistaken
    /// for something pyrocko said.
    struct DeadTransport;

    impl BridgeTransport for DeadTransport {
        fn unary(
            &self,
            _operation: crate::Operation,
            method: &'static str,
            _request: Vec<u8>,
        ) -> impl Future<Output = Result<Vec<u8>, TransportError>> {
            std::future::ready(Err(TransportError::new(method, "the worker is gone")))
        }

        fn unary_with_buffer(
            &self,
            _operation: crate::Operation,
            method: &'static str,
            _request: Vec<u8>,
        ) -> impl Future<Output = Result<(Vec<u8>, Vec<f32>), TransportError>> {
            std::future::ready(Err(TransportError::new(method, "the worker is gone")))
        }
    }

    /// Drives a future that cannot actually suspend: the transports above answer
    /// from memory, so one poll is the whole story and no runtime is needed.
    fn poll_once<F: Future>(future: F) -> eyre::Result<F::Output> {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => Ok(value),
            Poll::Pending => bail!("the fake transport suspended, which it cannot do"),
        }
    }

    fn envelope(result: bridge_result::Result) -> Vec<u8> {
        BridgeResult {
            result: Some(result),
        }
        .encode_to_vec()
    }

    /// The wire id the sample rpc is addressed by, which the errors have to name.
    const MAP_DATA: &str = "/snuffler_web_proto.BridgeService/MapData";

    /// Calls `MapData`, the sample RPC throughout, against a canned reply.
    fn call_with_reply(bytes: Vec<u8>) -> eyre::Result<CallError> {
        match poll_once(
            BridgeServiceClient::new(CannedTransport(bytes)).map_data(MapRequest::default()),
        )? {
            Ok(payload) => bail!("expected a failure, got {payload:?}"),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn a_python_exception_arrives_with_its_traceback() -> eyre::Result<()> {
        let error = call_with_reply(envelope(bridge_result::Result::Error(crate::BridgeError {
            kind: "ValueError".to_owned(),
            message: "no traces in that window".to_owned(),
            traceback: "Traceback (most recent call last):\n  ...".to_owned(),
        })))?;

        let CallError::Remote(remote) = &error else {
            bail!("a BridgeError envelope should be a remote failure, got {error:?}")
        };
        ensure!(
            remote.to_string() == "ValueError: no traces in that window",
            "the exception reads as {remote:?}"
        );
        // The traceback survives the trip alongside the formatted message.
        ensure!(
            error.traceback() == Some("Traceback (most recent call last):\n  ..."),
            "the traceback came through as {:?}",
            error.traceback()
        );
        Ok(())
    }

    #[test]
    fn an_exception_with_no_message_does_not_trail_a_separator() -> eyre::Result<()> {
        let error = call_with_reply(envelope(bridge_result::Result::Error(crate::BridgeError {
            kind: "KeyboardInterrupt".to_owned(),
            message: String::new(),
            traceback: String::new(),
        })))?;
        ensure!(
            error.to_string() == "KeyboardInterrupt",
            "a message-less exception reads as {error:?}"
        );
        // An empty traceback is no traceback, not an empty details pane.
        ensure!(
            error.traceback().is_none(),
            "an empty traceback should not be offered as one"
        );
        Ok(())
    }

    #[test]
    fn an_empty_envelope_is_not_mistaken_for_success() -> eyre::Result<()> {
        let error = call_with_reply(BridgeResult { result: None }.encode_to_vec())?;
        let CallError::Empty { method } = error else {
            bail!("an envelope with no oneof set should be Empty, got {error:?}")
        };
        ensure!(
            method == MAP_DATA,
            "the failure names {method}, not the rpc"
        );
        Ok(())
    }

    #[test]
    fn another_rpcs_payload_is_refused() -> eyre::Result<()> {
        // A SceneMeta where MapData was asked for: the schema and the generated
        // ends have drifted, which is nobody's user error.
        let error = call_with_reply(envelope(bridge_result::Result::Scene(
            crate::SceneMeta::default(),
        )))?;
        let CallError::UnexpectedPayload { method } = error else {
            bail!("the wrong payload should be UnexpectedPayload, got {error:?}")
        };
        ensure!(
            method == MAP_DATA,
            "the failure names {method}, not the rpc"
        );
        Ok(())
    }

    #[test]
    fn bytes_that_are_not_an_envelope_name_the_rpc() -> eyre::Result<()> {
        let error = call_with_reply(vec![0xFF; 8])?;
        let CallError::Decode { method, .. } = &error else {
            bail!("undecodable bytes should be a Decode failure, got {error:?}")
        };
        ensure!(
            *method == MAP_DATA,
            "the failure names {method}, not the rpc"
        );
        ensure!(
            error.to_string()
                == format!("{MAP_DATA}: the response was not a decodable BridgeResult"),
            "unexpected wording: {error}"
        );
        Ok(())
    }

    #[test]
    fn a_dead_worker_never_looks_like_a_remote_failure() -> eyre::Result<()> {
        let Err(error) =
            poll_once(BridgeServiceClient::new(DeadTransport).map_data(MapRequest::default()))?
        else {
            bail!("a dead transport cannot produce a payload")
        };
        let CallError::Transport(transport) = &error else {
            bail!("a rejected transport should be Transport, got {error:?}")
        };
        ensure!(
            transport.to_string() == format!("{MAP_DATA}: the worker is gone"),
            "unexpected wording: {transport}"
        );
        // Nothing to blame pyrocko for — this call never reached it.
        ensure!(
            error.traceback().is_none(),
            "a transport failure has no pyrocko traceback to offer"
        );
        Ok(())
    }
}
