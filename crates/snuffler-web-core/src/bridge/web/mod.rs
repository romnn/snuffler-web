//! The wasm transport: every operation that submits Python work, and the browser glue
//! it needs.
//!
//! Layout:
//!
//! - `js`: the `web/bridge.js` imports and their value helpers;
//! - `dispatch`: ledger submission, lane ownership and the worker transport;
//! - `runtime`: worker events, cancellation, restarts and runtime loss;
//! - `view` and `tiles`: the per-frame view lane and the map's tile fetches;
//! - `datasets`, `file_load`, `metadata`, `analysis`, `exports`: the [`Bridge`]
//!   operations, grouped by what they act on;
//! - `live`: the `SeedLink` client.

#![allow(
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    reason = "the web bridge re-uses most parent types and marshals owned values \
              (JsValue, protobuf bytes, params) across the JS/async boundary, where \
              by-value parameters keep the call sites clean"
)]

use std::cell::Cell;
use std::future::Future;

use snuffler_web_proto::prost::Message as _;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::install::{set_diagnostic, settle_diagnostic, standard_marker_budget};
use super::proto::{self, pb};
use super::*;

mod analysis;
mod datasets;
mod dispatch;
mod exports;
mod file_load;
mod js;
mod live;
mod metadata;
mod proxy;
mod runtime;
mod tiles;
mod view;

use dispatch::*;
use file_load::*;
use js::*;
use live::*;
use runtime::*;

pub(super) use runtime::{
    cancel_job, compute_budget, restart_runtime, runtime_info, seisbench_device, sync_runtime,
};
