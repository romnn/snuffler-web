#![doc = "Generated protobuf messages and service clients for the snuffler web bridge."]

pub use prost;

mod error;

pub use error::{CallError, RemoteError, TransportError};

// The module is private so the generated items have one path, the crate root.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::allow_attributes_without_reason,
    missing_docs,
    reason = "prost generates this API from the protobuf schema"
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/bridge-activity.rs"));
    include!(concat!(env!("OUT_DIR"), "/snuffler_web_proto.rs"));
}

pub use generated::*;
