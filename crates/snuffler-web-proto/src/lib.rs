#![doc = "Generated protobuf types for the snuffler web bridge."]

pub use prost;

mod error;

pub use error::{CallError, RemoteError, TransportError};

/// Versioned protobuf messages and generated service clients.
pub mod bridge {
    /// Version 1 of the Rust-to-Python bridge contract.
    #[allow(
        clippy::all,
        clippy::pedantic,
        clippy::allow_attributes_without_reason,
        missing_docs,
        reason = "prost generates this API from the protobuf schema"
    )]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/snuffler.bridge.v1.rs"));
    }
}
