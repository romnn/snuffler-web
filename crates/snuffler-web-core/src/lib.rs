//! Toolkit-neutral session state and transport for snuffler-web frontends.
//!
//! A frontend owns one [`session::Session`], calls its per-frame sync, and draws
//! [`state::AppState`] through `snuffler-web-render` (adr/0005).
//! Every unit of Python work goes through the [`activity::ActivityLedger`] inside
//! [`bridge::Bridge`] (adr/0006).

pub mod activity;
pub mod bridge;
pub mod command;
pub mod deployment;
pub mod markers;
pub mod seedlink;
pub mod session;
pub mod snufflings;
pub mod state;
pub mod theme;
pub mod timeaxis;
