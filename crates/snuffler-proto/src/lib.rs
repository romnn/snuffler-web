#![doc = "Generated protobuf types for the snuffler web bridge."]

pub use prost;

pub mod bridge {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/snuffler.bridge.v1.rs"));
    }
}
