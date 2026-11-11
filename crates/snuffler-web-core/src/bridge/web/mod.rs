//! Web target: drive Pyodide through web/bridge.js.

#![allow(
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    reason = "the web bridge re-uses most parent types and marshals owned values \
              (JsValue, protobuf bytes, params) across the JS/async boundary, where \
              by-value parameters keep the call sites clean"
)]

use super::proto::{self, pb};
use super::*;
use crate::activity::RuntimeEpoch;
use snuffler_web_proto::prost::Message as _;
use std::cell::{Cell, RefCell as StdRefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

mod dispatch;
mod live;

use dispatch::*;
use live::*;

// The module path escapes the crate (`/` roots it at the crate directory,
// `../..` walks up to the repo), so wasm-bindgen cannot embed the file as a
// snippet: the emitted import resolves to `web/gen/bridge.js` under the site
// root, and the trunk post-build hook must stage the file there
// (`stage_bridge_module` in scripts/trunk-post-build.sh).
#[wasm_bindgen(module = "/../../web/gen/bridge.js")]
extern "C" {
    // The single transport seam for every unary proto RPC: (method, request bytes)
    // -> { result: BridgeResult bytes, ys: Uint8Array }.
    // bridge.js forwards it to the Pyodide worker, which runs bridge.py and ships the
    // out-of-band ys buffer alongside the proto.
    // This is the custom transport the typed service client rides on.
    #[wasm_bindgen(js_name = call, catch)]
    async fn js_call(method: &str, request: Vec<u8>, job_id: u64) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = takeOperationEvents)]
    fn js_take_operation_events() -> js_sys::Array;

    #[wasm_bindgen(js_name = seisbenchDevice)]
    fn js_seisbench_device() -> JsValue;

    #[wasm_bindgen(js_name = runtimeInfo)]
    fn js_runtime_info() -> JsValue;

    #[wasm_bindgen(js_name = currentRuntimeEpoch)]
    fn js_current_runtime_epoch() -> u32;

    #[wasm_bindgen(js_name = cancelJob, catch)]
    fn js_cancel_job(request: Vec<u8>) -> Result<Vec<u8>, JsValue>;

    #[wasm_bindgen(js_name = restartRuntime)]
    fn js_restart_runtime(reason: &str) -> u32;

    // DOM-only calls keep bespoke externs: they drive browser APIs (file
    // pickers, downloads, the File System Access API, tile fetch+decode) that
    // can't be expressed as a uniform request->response RPC.
    #[wasm_bindgen(js_name = pickCakeModel, catch)]
    async fn js_pick_cake_model() -> Result<JsValue, JsValue>;

    #[cfg(feature = "gf")]
    #[wasm_bindgen(js_name = addGfStore, catch)]
    async fn js_add_gf_store(job_id: u64) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = fetchTiles, catch)]
    async fn js_fetch_tiles(
        urls: js_sys::Array,
        on_tile: js_sys::Function,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = openFiles, catch)]
    async fn js_open_files() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = reloadFiles, catch)]
    async fn js_reload_files() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = readFileLoadPage, catch)]
    async fn js_read_file_load_page(request: Vec<u8>) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = cancelFileLoad)]
    fn js_cancel_file_load(request: Vec<u8>);
    #[wasm_bindgen(js_name = acknowledgeSources)]
    fn js_acknowledge_sources(bytes: Vec<u8>);
    #[wasm_bindgen(js_name = clearOpenSources)]
    fn js_clear_open_sources();

    #[wasm_bindgen(js_name = openDirectory, catch)]
    async fn js_open_directory() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = reloadSnufflings, catch)]
    async fn js_reload_snufflings(job_id: u64) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = downloadFile, catch)]
    async fn js_download_file(
        bytes: Vec<u8>,
        filename: String,
        mime: String,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadMarkers, catch)]
    async fn js_load_markers() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadStationXml, catch)]
    async fn js_load_station_xml() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadStations, catch)]
    async fn js_load_stations() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadEvents, catch)]
    async fn js_load_events() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = savePng, catch)]
    async fn js_save_png(bytes: Vec<u8>, filename: String) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = saveSvg, catch)]
    async fn js_save_svg(svg: String, filename: String) -> Result<JsValue, JsValue>;
}

/// Terminates everything owned by a dead runtime and resets the fences and
/// latches that are per-interpreter: the replacement session numbers its
/// generations from zero, must be re-warmed, and has no live stream state.
fn note_runtime_lost(state: &mut Inner, epoch: RuntimeEpoch) {
    if let Some((_, params)) = state.view_job.take() {
        state.desired = Some(params);
    }
    state.examples_job = None;
    state.map_job = None;
    state.figures.clear();
    // The owning executor futures may still be alive; without this, a retired
    // composite flow would keep resolving child owners against the replacement
    // interpreter while blocking every new job-lane submission.
    clear_lane_registrations();
    state.activity.runtime_lost(epoch);
    state.python_session_generation = 0;
    state.scientific_tools_requested = false;
    // The replacement interpreter has no session for the stream to land in, so
    // the socket retires with the runtime — otherwise the UI keeps a visibly
    // live acquisition whose every packet is rejected against a Closed stream.
    retire_live_session_state(state);
    defer_live_socket_retirement();
}

pub(super) fn sync_runtime(inner: &Rc<RefCell<Inner>>) {
    let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
    let ready = runtime_info().is_some();
    {
        let mut state = inner.borrow_mut();
        if let Some(previous) = state.runtime_epoch
            && previous != epoch
        {
            // An epoch bump with no rejected reply to report it: the worker
            // died or was restarted while idle.
            note_runtime_lost(&mut state, previous);
        }
        state.runtime_epoch = Some(epoch);
        if ready && !matches!(state.activity.runtime(), RuntimeState::Ready) {
            state.activity.set_runtime(RuntimeState::Ready);
        }
    }
    drain_operation_events(inner);
}

pub(super) fn cancel_job(handle: JobHandle) -> Result<pb::CancelDisposition, BridgeError> {
    let request = pb::CancelJobRequest {
        job_id: handle.id().get(),
    }
    .encode_to_vec();
    let bytes = js_cancel_job(request)
        .map_err(|error| BridgeError::browser("cancelling Python work", jsval_to_string(error)))?;
    let reply: pb::CancelJobReply = proto::decode_message(&bytes, "cancelJob")?;
    pb::CancelDisposition::try_from(reply.disposition).map_err(|_| {
        BridgeError::browser("cancelling Python work", "unknown cancellation disposition")
    })
}

pub(super) fn restart_runtime(inner: &Rc<RefCell<Inner>>) {
    let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
    {
        let mut state = inner.borrow_mut();
        note_runtime_lost(&mut state, epoch);
    }
    js_restart_runtime("Python did not stop after cancellation");
    // Record the successor epoch so the next sync poll does not report the
    // restart a second time.
    inner.borrow_mut().runtime_epoch = Some(RuntimeEpoch::new(js_current_runtime_epoch()));
}

fn drain_operation_events(inner: &Rc<RefCell<Inner>>) {
    for value in js_take_operation_events().iter() {
        let Ok(bytes) = value.dyn_into::<js_sys::Uint8Array>() else {
            set_bridge_error(
                inner,
                BridgeError::browser("reading Python progress", "event was not bytes"),
            );
            continue;
        };
        let event = match pb::WorkerEvent::decode(bytes.to_vec().as_slice()) {
            Ok(event) => event,
            Err(error) => {
                set_bridge_error(
                    inner,
                    BridgeError::browser("decoding Python progress", error.to_string()),
                );
                continue;
            }
        };
        if event.runtime_epoch != js_current_runtime_epoch() {
            continue;
        }
        let Some(event) = event.event else {
            continue;
        };
        let operation = match event {
            pb::worker_event::Event::RuntimeFailure(failure) => {
                let detail = if failure.kind.is_empty() {
                    failure.message
                } else {
                    format!("{}: {}", failure.kind, failure.message)
                };
                let error = BridgeError::browser("starting Python", detail);
                let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
                let mut state = inner.borrow_mut();
                note_runtime_lost(&mut state, epoch);
                state
                    .activity
                    .set_runtime(RuntimeState::Failed(error.clone()));
                state.diagnostic = Some(error);
                continue;
            }
            pb::worker_event::Event::Operation(operation) => operation,
            pb::worker_event::Event::Boot(_)
            | pb::worker_event::Event::MlDevice(_)
            | pb::worker_event::Event::RuntimeInfo(_) => continue,
        };
        match operation.event {
            Some(pb::operation_event::Event::Started(_)) => {
                inner
                    .borrow_mut()
                    .activity
                    .mark_wire_running(operation.job_id, js_sys::Date::now() / 1000.0);
            }
            Some(pb::operation_event::Event::Progress(progress)) => {
                let phase = progress.phase.into();
                let value = match progress.progress {
                    Some(pb::operation_progress::Progress::Count(count)) => Progress::Count {
                        done: count.done,
                        total: count.total,
                        phase,
                    },
                    Some(pb::operation_progress::Progress::Fraction(fraction)) => {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "wire progress is normalized before conversion to the UI's f32"
                        )]
                        let fraction = fraction.clamp(0.0, 1.0) as f32;
                        Progress::Fraction { fraction, phase }
                    }
                    None => {
                        set_bridge_error(
                            inner,
                            BridgeError::browser(
                                "reading Python progress",
                                "progress event had no count or fraction",
                            ),
                        );
                        continue;
                    }
                };
                inner
                    .borrow_mut()
                    .activity
                    .update_wire_progress(operation.job_id, value);
            }
            None => {}
        }
    }
}

fn jsval_to_string(v: JsValue) -> String {
    v.as_string().unwrap_or_else(|| format!("{v:?}"))
}

pub(super) fn seisbench_device() -> String {
    let status = js_bytes(js_seisbench_device(), "seisbench_device")
        .ok()
        .and_then(|bytes| {
            proto::decode_message::<pb::MlDeviceStatus>(&bytes, "seisbenchDevice").ok()
        })
        .and_then(|status| pb::MlDevice::try_from(status.device).ok());
    match status {
        Some(pb::MlDevice::Webgpu) => "WebGPU".to_owned(),
        Some(pb::MlDevice::Wasm | pb::MlDevice::Unspecified) | None => "CPU (wasm)".to_owned(),
    }
}

pub(super) fn runtime_info() -> Option<pb::RuntimeInfo> {
    let info = js_runtime_info();
    if info.is_null() || info.is_undefined() {
        return None;
    }
    let bytes = js_bytes(info, "runtime_info").ok()?;
    proto::decode_message(&bytes, "runtimeInfo").ok()
}

fn js_bytes(v: JsValue, field: &'static str) -> Result<Vec<u8>, MarshalError> {
    v.dyn_into::<js_sys::Uint8Array>()
        .map_err(|_| MarshalError::NotBytes { field })
        .map(|arr| arr.to_vec())
}

fn decode_browser_file_page(
    result: Result<JsValue, JsValue>,
    field: &'static str,
    action: &'static str,
) -> Result<BrowserFileLoadPage, BridgeError> {
    let value = result.map_err(|error| BridgeError::browser(action, jsval_to_string(error)))?;
    let bytes = js_bytes(value, field)?;
    proto::decode_message(&bytes, field).map_err(Into::into)
}

fn file_page_files(page: BrowserFileLoadPage) -> Result<Vec<pb::BrowserFile>, BridgeError> {
    if page.cancelled {
        return Ok(Vec::new());
    }
    // These aggregate imports are all-or-error: silently loading four of five
    // station files would leave the user with an incomplete table and no signal,
    // so any per-file failure (oversized, unreadable) rejects the operation.
    if !page.failures.is_empty() {
        return Err(BridgeError::browser(
            "reading selected files",
            page.failures
                .iter()
                .map(|failure| format!("{}: {}", failure.name, failure.detail))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    Ok(page.files)
}

fn file_page_texts(page: BrowserFileLoadPage) -> Result<Vec<String>, BridgeError> {
    file_page_files(page)?
        .into_iter()
        .map(|file| {
            String::from_utf8(file.content).map_err(|error| {
                BridgeError::browser("decoding a selected text file", error.to_string())
            })
        })
        .collect()
}

/// Stops a composite flow at a child boundary.
/// Every flow re-checks after an unbounded browser-local await (a picker, a directory
/// walk): the flow may have been cancelled, or retired outright by a runtime loss, while
/// the dialog was open, and its next child must not run.
fn ensure_flow_live(
    inner: &Rc<RefCell<Inner>>,
    flow: JobHandle,
    method: &'static str,
) -> Result<(), BridgeError> {
    if job_flow_interrupted(inner, flow) {
        return Err(BridgeError::Call(CallError::Cancelled { method }));
    }
    Ok(())
}

fn picked_file_texts(
    result: Result<JsValue, JsValue>,
    field: &'static str,
    action: &'static str,
) -> Result<Vec<String>, BridgeError> {
    file_page_texts(decode_browser_file_page(result, field, action)?)
}

async fn download_bytes(
    bytes: Vec<u8>,
    filename: String,
    mime: String,
    action: &'static str,
) -> Result<(), BridgeError> {
    js_download_file(bytes, filename, mime)
        .await
        .map_err(|error| BridgeError::browser(action, jsval_to_string(error)))?;
    Ok(())
}

async fn download_blob(
    result: Result<pb::FileBlob, CallError>,
    action: &'static str,
) -> Result<(), BridgeError> {
    let blob = result?;
    download_bytes(blob.content, blob.filename, blob.mime, action).await
}

fn object_uint8(obj: &JsValue, field: &'static str) -> Result<Vec<u8>, MarshalError> {
    let val = js_sys::Reflect::get(obj, &JsValue::from_str(field))
        .map_err(|_| MarshalError::Missing { field })?;
    js_bytes(val, field)
}

fn object_f32(obj: &JsValue, field: &'static str) -> Result<Vec<f32>, MarshalError> {
    let val = js_sys::Reflect::get(obj, &JsValue::from_str(field))
        .map_err(|_| MarshalError::Missing { field })?;
    let arr = val
        .dyn_into::<js_sys::Uint8Array>()
        .map_err(|_| MarshalError::NotBytes { field })?;
    if arr.byte_length() % 4 != 0 {
        return Err(MarshalError::UnalignedSamples {
            field,
            len: arr.byte_length(),
        });
    }
    let values = arr.byte_length() / 4;
    Ok(js_sys::Float32Array::new_with_byte_offset_and_length(
        &arr.buffer(),
        arr.byte_offset(),
        values,
    )
    .to_vec())
}

fn marker_budget() -> pb::MarkerBudget {
    crate::marker_store::MarkerBudget::standard().into()
}

fn set_bridge_error(inner: &Rc<RefCell<Inner>>, error: BridgeError) {
    inner.borrow_mut().diagnostic = Some(error);
}

/// Settles the recoverable diagnostic slot from one export's result: success
/// clears it, failure takes it — and still fails the job, so the ledger and
/// the status line agree.
fn settle_diagnostic(
    inner: &Rc<RefCell<Inner>>,
    result: Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    match result {
        Ok(()) => {
            inner.borrow_mut().diagnostic = None;
            Ok(())
        }
        Err(error) => {
            inner.borrow_mut().diagnostic = Some(error.clone());
            Err(error)
        }
    }
}

fn set_snuffling_text(inner: &Rc<RefCell<Inner>>, text: String, show_window: bool) {
    let mut i = inner.borrow_mut();
    install_snuffling_result(
        &mut i,
        SnufflingResult {
            show_window,
            text,
            markers: Vec::new(),
            scene: None,
            origin: String::new(),
            event_state: None,
            markers_dropped: 0,
            marker_budget: crate::marker_store::MarkerBudget::standard().get(),
            nslc_table: Vec::new(),
        },
    );
}

fn clear_map_state(inner: &mut Inner) {
    inner.map = None;
    inner.map_request = None;
    inner.map_job = None;
    inner.map_request_gen = inner.map_request_gen.wrapping_add(1);
}

fn clear_event_state(inner: &mut Inner) {
    inner.event_state = None;
    inner.active_event_gen = inner.active_event_gen.wrapping_add(1);
    clear_map_state(inner);
}

fn install_event_state(inner: &mut Inner, state: ActiveEventResult) {
    inner.event_state = Some(state);
    inner.active_event_gen = inner.active_event_gen.wrapping_add(1);
    clear_map_state(inner);
    invalidate_view_request(inner);
}

fn install_snuffling_result(inner: &mut Inner, mut result: SnufflingResult) {
    if let Some(event_state) = result.event_state.take() {
        install_event_state(inner, event_state);
    }
    inner.snuffling = Some(result);
    inner.snuffling_gen = inner.snuffling_gen.wrapping_add(1);
}

fn replace_scene(inner: &mut Inner, scene: SceneMeta) {
    inner.scene = Some(scene);
    inner.scene_gen = inner.scene_gen.wrapping_add(1);
    clear_view(inner);
}

fn install_scene(inner: &mut Inner, scene: Option<SceneMeta>) {
    inner.diagnostic = None;
    replace_scene(inner, scene.unwrap_or_default());
}

fn install_waveform_result(inner: &mut Inner, mut result: pb::LoadBytesResult) {
    install_scene(inner, result.scene.take());
    if let Some(event_state) = result.event_state.take() {
        install_event_state(inner, event_state);
    }
}

fn install_bundle_result(inner: &mut Inner, mut result: pb::BundleResult) {
    install_scene(inner, result.scene.take());
    install_bundle_presentation(inner, result.view.take(), result.about.take());
    if let Some(info) = result.info.take() {
        install_snuffling_result(inner, info);
    }
}

fn acknowledge_source(file: &pb::BrowserFile) {
    if file.handle_id.is_empty() {
        return;
    }
    js_acknowledge_sources(
        BrowserSourceAckList {
            sources: vec![BrowserSourceAck {
                handle_id: file.handle_id.clone(),
                last_modified: file.last_modified,
                bundle: file.bundle,
            }],
        }
        .encode_to_vec(),
    );
}

async fn load_browser_file(
    inner: &Rc<RefCell<Inner>>,
    file: &mut pb::BrowserFile,
    mode: SourceLoadMode,
) -> Result<BrowserFileOutcome, CallError> {
    let content = std::mem::take(&mut file.content);
    if file.bundle {
        retire_live_session(inner);
        return client()
            .load_bundle(pb::LoadBundleRequest {
                url: String::new(),
                sha256: String::new(),
                content,
                name: file.name.clone(),
                marker_budget: Some(marker_budget()),
            })
            .await
            .map(|bundle| {
                install_bundle_result(&mut inner.borrow_mut(), bundle);
                BrowserFileOutcome::Installed
            });
    }
    client()
        .load_bytes(pb::LoadBytesRequest {
            name: file.name.clone(),
            fmt: "detect".to_owned(),
            append: true,
            content,
            source_id: file.handle_id.clone(),
            replace_source: mode == SourceLoadMode::Replace && !file.handle_id.is_empty(),
        })
        .await
        .map(|result| {
            let outcome = if result.loaded {
                BrowserFileOutcome::Installed
            } else {
                BrowserFileOutcome::Duplicate
            };
            install_waveform_result(&mut inner.borrow_mut(), result);
            outcome
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceLoadMode {
    Append,
    Replace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserFileOutcome {
    Installed,
    Duplicate,
}

enum FilePageOutcome {
    More,
    Complete,
    Cancelled {
        remaining: u32,
    },
    /// A child failed below the rpc layer (transport, protocol drift, stale
    /// generation), so the flow itself is broken and must not issue further
    /// children — after a runtime loss they would commit into the replacement
    /// interpreter's fresh session under an already-retired job.
    Aborted(CallError),
}

#[derive(Default)]
struct FileLoadProgress {
    committed: u32,
    processed: u32,
    failures: Vec<ReloadSourceError>,
}

async fn load_file_page(
    inner: &Rc<RefCell<Inner>>,
    flow: JobHandle,
    mut page: BrowserFileLoadPage,
    mode: SourceLoadMode,
    progress: &mut FileLoadProgress,
) -> FilePageOutcome {
    progress.failures.extend(
        std::mem::take(&mut page.failures)
            .into_iter()
            .map(|failure| ReloadSourceError {
                source_id: failure.handle_id,
                name: failure.name,
                reason: ReloadFailureReason::BrowserRead {
                    detail: failure.detail,
                },
            }),
    );
    let page_count = u32::try_from(page.files.len()).unwrap_or(u32::MAX);
    let known_total = progress
        .processed
        .saturating_add(page_count)
        .saturating_add(u32::from(page.has_more));
    for (index, mut file) in page.files.into_iter().enumerate() {
        let remaining = page_count
            .saturating_sub(u32::try_from(index).unwrap_or(u32::MAX))
            .saturating_add(u32::from(page.has_more));
        if job_flow_interrupted(inner, flow) {
            return FilePageOutcome::Cancelled { remaining };
        }
        update_flow_progress(
            inner,
            flow,
            u64::from(progress.processed),
            u64::from(known_total),
            format!("Loading {}", file.name),
        );
        match load_browser_file(inner, &mut file, mode).await {
            Ok(BrowserFileOutcome::Installed) => {
                progress.committed = progress.committed.saturating_add(1);
                acknowledge_source(&file);
            }
            Ok(BrowserFileOutcome::Duplicate) => {}
            Err(CallError::Cancelled { .. }) => {
                return FilePageOutcome::Cancelled { remaining };
            }
            // Only a pyrocko rejection is a per-file verdict worth continuing past;
            // anything below the rpc layer is a broken flow.
            // The abort still joins the failure list, so the diagnostic keeps the
            // per-file failures that accumulated before the flow broke.
            Err(CallError::Remote(remote)) => progress.failures.push(ReloadSourceError {
                source_id: file.handle_id.clone(),
                name: file.name.clone(),
                reason: ReloadFailureReason::Remote(remote),
            }),
            Err(error) => {
                progress.failures.push(ReloadSourceError {
                    source_id: file.handle_id.clone(),
                    name: file.name.clone(),
                    reason: match &error {
                        CallError::Transport(transport) => ReloadFailureReason::Transport {
                            detail: transport.to_string(),
                        },
                        other => ReloadFailureReason::Protocol {
                            detail: other.to_string(),
                        },
                    },
                });
                return FilePageOutcome::Aborted(error);
            }
        }
        progress.processed = progress.processed.saturating_add(1);
    }
    if page.has_more {
        FilePageOutcome::More
    } else {
        FilePageOutcome::Complete
    }
}

fn cancel_browser_file_load(load_id: &str) {
    if !load_id.is_empty() {
        js_cancel_file_load(
            pb::BrowserFileLoadPageRequest {
                load_id: load_id.to_owned(),
            }
            .encode_to_vec(),
        );
    }
}

/// Failures that survive to a terminal outcome surface as one typed per-source
/// diagnostic, so a pyrocko rejection keeps its traceback.
fn set_reload_diagnostic(inner: &Rc<RefCell<Inner>>, failures: &mut Vec<ReloadSourceError>) {
    if !failures.is_empty() {
        let error = BridgeError::from(ReloadError {
            failures: std::mem::take(failures),
        });
        inner.borrow_mut().diagnostic = Some(error);
    }
}

fn stop_file_load(
    inner: &Rc<RefCell<Inner>>,
    progress: &mut FileLoadProgress,
    skipped: u32,
    load_id: &str,
) -> Result<Outcome, BridgeError> {
    cancel_browser_file_load(load_id);
    set_reload_diagnostic(inner, &mut progress.failures);
    if progress.committed == 0 {
        Err(BridgeError::Call(CallError::Cancelled {
            method: "browser file load",
        }))
    } else {
        Ok(Outcome::Stopped {
            committed: progress.committed,
            skipped,
        })
    }
}

async fn run_file_load(
    inner: Rc<RefCell<Inner>>,
    flow: JobHandle,
    mut page: BrowserFileLoadPage,
    mode: SourceLoadMode,
) -> Result<Outcome, BridgeError> {
    let load_id = page.load_id.clone();
    if let Err(error) = ensure_flow_live(&inner, flow, "browser file load") {
        cancel_browser_file_load(&load_id);
        return Err(error);
    }
    if page.cancelled {
        return Ok(Outcome::Succeeded);
    }
    let mut progress = FileLoadProgress::default();
    loop {
        match load_file_page(&inner, flow, page, mode, &mut progress).await {
            FilePageOutcome::More => {}
            FilePageOutcome::Complete => break,
            FilePageOutcome::Cancelled { remaining } => {
                return stop_file_load(&inner, &mut progress, remaining, &load_id);
            }
            FilePageOutcome::Aborted(error) => {
                cancel_browser_file_load(&load_id);
                // The failure list already carries the abort plus everything
                // accumulated before it; the flow's terminal outcome carries
                // the systemic error itself.
                set_reload_diagnostic(&inner, &mut progress.failures);
                return Err(BridgeError::Call(error));
            }
        }
        if job_flow_interrupted(&inner, flow) {
            return stop_file_load(&inner, &mut progress, 1, &load_id);
        }
        let page_request = pb::BrowserFileLoadPageRequest {
            load_id: load_id.clone(),
        }
        .encode_to_vec();
        page = match decode_browser_file_page(
            js_read_file_load_page(page_request).await,
            "read_file_load_page",
            "reading the next file page",
        ) {
            Ok(next) => next,
            Err(error) => {
                cancel_browser_file_load(&load_id);
                return Err(error);
            }
        };
    }
    update_flow_progress(
        &inner,
        flow,
        u64::from(progress.processed),
        u64::from(progress.processed),
        "Finishing file load".to_owned(),
    );
    if job_flow_interrupted(&inner, flow) {
        // Cancellation arrived after the final commit; nothing was skipped,
        // but per-file failures accumulated on the way must still surface.
        set_reload_diagnostic(&inner, &mut progress.failures);
        return Ok(Outcome::TooLate);
    }
    if progress.failures.is_empty() {
        inner.borrow_mut().diagnostic = None;
        return Ok(Outcome::Succeeded);
    }
    let committed = progress.committed;
    let error = BridgeError::from(ReloadError {
        failures: progress.failures,
    });
    inner.borrow_mut().diagnostic = Some(error.clone());
    if committed == 0 {
        Err(error)
    } else {
        Ok(Outcome::Succeeded)
    }
}

fn invalidate_view_request(i: &mut Inner) {
    i.applied = None;
    i.desired = None;
    i.view_job = None;
    i.view_request_gen = i.view_request_gen.wrapping_add(1);
}

fn clear_view(i: &mut Inner) {
    i.view = None;
    invalidate_view_request(i);
}

impl Bridge {
    fn prepare_scientific_tools(&self) -> Option<Submission> {
        {
            let mut state = self.inner.borrow_mut();
            if state.scientific_tools_requested {
                return None;
            }
            state.scientific_tools_requested = true;
        }
        let inner = self.inner.clone();
        Some(submit_job(
            inner,
            self.repaint.clone(),
            Operation::PrepareScientificTools,
            None,
            Vec::new(),
            async move {
                client()
                    .prepare_scientific_tools()
                    .await
                    .map(|_| ())
                    .map_err(BridgeError::from)
            },
        ))
    }

    /// Replaces the current pile with the bundled synthetic example.
    pub fn load_example(&self) -> Submission {
        retire_live_session(&self.inner);
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::MakeExample,
            None,
            Vec::new(),
            async move {
                let parsed = client().make_example().await;
                let scene = parsed.map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                clear_event_state(&mut i);
                replace_scene(&mut i, scene);
                Ok(())
            },
        )
    }

    /// Empties the pile and the event context (`File ▸ Close all`), leaving
    /// the blank viewer Qt starts with when given no arguments.
    pub fn empty_scene(&self) -> Submission {
        let repaint = self.repaint.clone();
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            repaint,
            Operation::EmptyScene,
            None,
            Vec::new(),
            async move {
                let scene = client().empty_scene().await.map_err(BridgeError::from)?;
                retire_live_session(&work_inner);
                js_clear_open_sources();
                let mut i = work_inner.borrow_mut();
                i.diagnostic = None;
                i.snuffling = None;
                clear_event_state(&mut i);
                i.live_seedlink_started_at = None;
                replace_scene(&mut i, scene);
                Ok(())
            },
        )
    }

    /// Appends waveform bytes supplied by the frontend to the pile —
    /// snuffler's `open_waveforms` appends too (`pile_viewer.py:1651`); the
    /// boot example is dropped first by the bridge's example-pile rule.
    pub fn load_file(&self, name: String, data: Vec<u8>, fmt: String) -> Submission {
        // A dropped file may be a whole dataset.
        // Deciding on the bytes rather than the extension means a bundle behaves the same
        // however it arrives — dropped, picked, or fetched from the Examples menu.
        if data.starts_with(ZIP_MAGIC) {
            return self.load_bundle_bytes(name, data);
        }
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let detail = Some(name.clone());
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::LoadBytes,
            detail,
            Vec::new(),
            async move {
                let result = client()
                    .load_bytes(pb::LoadBytesRequest {
                        name,
                        fmt,
                        append: true,
                        content: data,
                        source_id: String::new(),
                        replace_source: false,
                    })
                    .await
                    .map_err(BridgeError::from)?;
                install_waveform_result(&mut work_inner.borrow_mut(), result);
                Ok(())
            },
        )
    }

    /// Replaces the current pile with a waveform fetched from a URL.
    pub fn load_url(&self, url: String, fmt: String) -> Submission {
        retire_live_session(&self.inner);
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let detail = Some(url.clone());
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::LoadUrl,
            detail,
            Vec::new(),
            async move {
                let scene = client()
                    .load_url(pb::LoadUrlRequest { url, fmt })
                    .await
                    .map_err(BridgeError::from)?;
                js_clear_open_sources();
                let mut i = work_inner.borrow_mut();
                clear_event_state(&mut i);
                replace_scene(&mut i, scene);
                Ok(())
            },
        )
    }

    /// Fetch the example index, once.
    ///
    /// The menu calls this on every frame it is open, so "once" has to mean once: a
    /// *failed* fetch is remembered too, or a datacenter that is down would be retried at
    /// the frame rate.
    /// `force` is the user asking again, from the retry the menu offers in its place.
    #[must_use]
    pub fn list_examples(&self, force: bool) -> Option<Submission> {
        {
            let i = self.inner.borrow();
            if let Some(job) = i.examples_job {
                return Some(Submission::Joined(job));
            }
            if i.examples.is_some() && !force {
                return None;
            }
        }
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let submission = submit_job(
            inner,
            self.repaint.clone(),
            Operation::ListExamples,
            None,
            Vec::new(),
            async move {
                let parsed = client().list_examples().await;
                let mut i = work_inner.borrow_mut();
                i.examples_job = None;
                match parsed {
                    Ok(list) => {
                        i.examples = Some(Ok(list.examples));
                        Ok(())
                    }
                    Err(error) => {
                        let error = BridgeError::from(error);
                        i.examples = Some(Err(error.clone()));
                        Err(error)
                    }
                }
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            self.inner.borrow_mut().examples_job = Some(handle);
        }
        Some(submission)
    }

    /// Replaces the current pile with a whole dataset fetched as one archive:
    /// waveforms, station metadata and an event catalogue in a single request.
    ///
    /// The station and event results come back with the scene and are applied
    /// through the same path a separate metadata load would use, so the event
    /// markers appear exactly as if the catalogue had been opened by hand.
    pub fn load_bundle(&self, url: String, sha256: String, title: String) -> Submission {
        let request = pb::LoadBundleRequest {
            url,
            sha256,
            content: Vec::new(),
            name: title.clone(),
            marker_budget: Some(marker_budget()),
        };
        self.dispatch_bundle(request, title)
    }

    /// Load an example by slug, fetching the index first if it has not been read yet.
    /// This is what `?example=<slug>` resolves to: a link someone can hand out that opens
    /// the app on a named dataset.
    ///
    /// An unknown slug is an error the user has to see — they followed a link
    /// that promised data — so it lands in the status line rather than only in
    /// the console.
    pub fn open_example(&self, slug: String) -> Submission {
        let bridge = self.clone();
        let inner = self.inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::ListExamples,
            Some(slug.clone()),
            slug.as_bytes().to_vec(),
            async move {
                let known = bridge.inner.borrow().examples.clone();
                let listed = match known {
                    Some(Ok(examples)) => Ok(examples),
                    _ => client().list_examples().await.map(|list| list.examples),
                }
                .map_err(BridgeError::from)?;
                let found = listed.iter().find(|example| example.slug == slug).cloned();
                bridge.inner.borrow_mut().examples = Some(Ok(listed));
                let example = found.ok_or(BridgeError::UnknownExample { slug })?;
                let _submission = bridge.load_bundle(example.url, example.sha256, example.title);
                Ok(())
            },
        )
    }

    /// The same load for an archive that is already in hand — dropped on the
    /// window, or picked from disk — rather than fetched.
    fn load_bundle_bytes(&self, name: String, data: Vec<u8>) -> Submission {
        let request = pb::LoadBundleRequest {
            url: String::new(),
            sha256: String::new(),
            content: data,
            name: name.clone(),
            marker_budget: Some(marker_budget()),
        };
        self.dispatch_bundle(request, name)
    }

    fn dispatch_bundle(&self, request: pb::LoadBundleRequest, label: String) -> Submission {
        retire_live_session(&self.inner);
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::LoadBundle,
            Some(label),
            Vec::new(),
            async move {
                let bundle = client()
                    .load_bundle(request)
                    .await
                    .map_err(BridgeError::from)?;
                js_clear_open_sources();
                install_bundle_result(&mut work_inner.borrow_mut(), bundle);
                Ok(())
            },
        )
    }

    /// Render a real pyrocko/matplotlib figure (amplitude spectrum, cake, …)
    /// in Pyodide and store the resulting PNG for the matching figure window.
    /// Keyed by `req.kind`, so several figure windows can be open at once.
    pub fn render_figure(&self, req: FigureRequest) -> Submission {
        // No slot-level short circuit here: the ledger's DEDUP policy on the
        // encoded request already joins identical work, and a same-kind
        // request with different params must queue, not silently join —
        // joining it would leave the window showing the old figure forever.
        let kind = req.kind.clone();
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let dedup_key = req.encode_to_vec();
        let work_kind = kind.clone();
        let job_slot = Rc::new(Cell::new(None));
        let work_job_slot = job_slot.clone();
        let submission = submit_job(
            inner,
            self.repaint.clone(),
            Operation::RenderFigure,
            Some(kind.clone()),
            dedup_key,
            async move {
                let parsed = client().render_figure(req).await;
                let mut i = work_inner.borrow_mut();
                let slot = i.figures.entry(work_kind).or_default();
                // A queued successor may already own the slot's spinner.
                if slot.job == work_job_slot.get() {
                    slot.job = None;
                }
                let fr = parsed.map_err(BridgeError::from)?;
                slot.data = FigureData {
                    png: fr.png,
                    error: fr.error,
                };
                slot.generation = slot.generation.wrapping_add(1);
                Ok(())
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            job_slot.set(Some(handle));
            self.inner.borrow_mut().figures.entry(kind).or_default().job = Some(handle);
        }
        submission
    }

    /// Run a built-in snuffling (`MinMax`, `STA/LTA`, …) and store its result.
    pub fn run_snuffling(&self, name: String, params: SnufflingRequestParams) -> Submission {
        let request = pb::NamedSnufflingRequest {
            name,
            params: Some(params),
        };
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let dedup_key = request.encode_to_vec();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::RunSnuffling,
            Some(request.name.clone()),
            dedup_key,
            async move {
                let parsed = client().run_snuffling(request).await;
                let mut i = work_inner.borrow_mut();
                let mut result = parsed.map_err(BridgeError::from)?;
                if let Some(scene) = result.scene.take() {
                    replace_scene(&mut i, scene);
                }
                install_snuffling_result(&mut i, result);
                Ok(())
            },
        )
    }

    /// Downloads waveforms from a CORS-enabled FDSN dataselect service and
    /// append them to the pile (browser fetch; pyrocko parses).
    pub fn download_waveforms(&self, params: WaveformDownloadRequest) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::DownloadWaveforms,
            None,
            Vec::new(),
            async move {
                let scene = client()
                    .download_waveforms(params)
                    .await
                    .map_err(BridgeError::from)?;
                replace_scene(&mut work_inner.borrow_mut(), scene);
                Ok(())
            },
        )
    }

    /// Computes blockwise RMS traces, replacing the previous RMS run.
    pub fn rms_traces(&self, params: SnufflingRequestParams) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let dedup_key = params.encode_to_vec();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::RmsTraces,
            None,
            dedup_key,
            async move {
                let mut result = client()
                    .rms_traces(params)
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                if let Some(scene) = result.scene.take() {
                    replace_scene(&mut i, scene);
                }
                install_snuffling_result(&mut i, result);
                Ok(())
            },
        )
    }

    /// Search a CORS-enabled FDSN event service; results become event markers.
    /// The largest-magnitude hit becomes the active event, so this forces a
    /// re-dispatch for the distance/azimuth sort to recompute against it.
    pub fn catalog_search(&self, params: CatalogRequest) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::CatalogSearch,
            None,
            Vec::new(),
            async move {
                let result = client()
                    .catalog_search(params)
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                install_snuffling_result(&mut i, result);
                invalidate_view_request(&mut i);
                Ok(())
            },
        )
    }

    /// Let the user pick a custom `.nd` model file ("Add Model"); pyrocko
    /// stores it and the returned name is added to the Cake Phase dropdown.
    pub fn add_cake_model(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow(
            inner,
            self.repaint.clone(),
            Operation::AddCakeModel,
            None,
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_pick_cake_model().await,
                    "cake_model",
                    "adding an earth model",
                )?;
                ensure_flow_live(&work_inner, flow, "adding an earth model")?;
                let Some(file) = file_page_files(page)?.into_iter().next() else {
                    return Ok(());
                };
                let label = client()
                    .add_cake_model(pb::AddCakeModelRequest {
                        name: file.name,
                        content: file.content,
                    })
                    .await
                    .map_err(BridgeError::from)?;
                if !label.value.is_empty() {
                    work_inner.borrow_mut().pending_cake_model = Some(label.value);
                }
                Ok(())
            },
        )
    }

    /// Mount a Green's-functions store by fetching its files from an HTTP base URL into
    /// the Pyodide filesystem (no directory picker) and register it as `GF Store: <id>`.
    /// Errors surface in the snuffling text window.
    pub fn load_gf_store_url(&self, url: String) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::LoadGfStoreUrl,
            Some(url.clone()),
            Vec::new(),
            async move {
                let label = client()
                    .load_gf_store_url(pb::StringRequest { value: url })
                    .await
                    .map_err(BridgeError::from)?;
                if !label.value.is_empty() {
                    work_inner.borrow_mut().pending_cake_model = Some(label.value);
                }
                Ok(())
            },
        )
    }

    /// Mount a Green's-functions store directory (`NativeFS`, via the File System Access
    /// API) and add its embedded 1D earth model to the Cake Phase dropdown as
    /// `GF Store: <id>`.
    /// Errors (unsupported browser, not a store) surface in the snuffling text window.
    #[cfg(feature = "gf")]
    pub fn add_gf_store(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::MountGfStore,
            None,
            Vec::new(),
            async move {
                let job = require_registered_job(Operation::MountGfStore, "addGfStore")
                    .map_err(CallError::from)?;
                let value = js_add_gf_store(job.id().get()).await.map_err(|error| {
                    BridgeError::browser("adding a GF store", jsval_to_string(error))
                })?;
                if let Some(name) = value.as_string()
                    && !name.is_empty()
                {
                    work_inner.borrow_mut().pending_cake_model = Some(name);
                }
                Ok(())
            },
        )
    }

    /// Execute a ':' command line (goto / hide / unhide / reduce / scaling).
    /// A goto stores a pending time the session jumps to; a visibility change swaps the
    /// scene in place (no refit) and forces the view to re-dispatch.
    /// Either way the outcome lands in `command_outcome` for the command line to act on.
    pub fn run_command(&self, request: CommandRequest) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::RunCommand,
            Some(request.line.clone()),
            Vec::new(),
            async move {
                let result = client()
                    .run_command(request)
                    .await
                    .map_err(BridgeError::from)?;
                // Scoped so the borrow ends before `request_repaint()`: `Inner` is
                // the same `RefCell` the render loop reads through its own `Rc`, and
                // this is an async block, so a guard still alive at the repaint — or
                // at any `.await` a later edit puts below — is a `BorrowMutError`
                // panic on whichever frame or bridge result lands next.
                {
                    let mut i = work_inner.borrow_mut();
                    match result.action.as_str() {
                        "goto" => i.pending_goto = result.t.map(|t| (t, result.tlen)),
                        "scene" => {
                            if let Some(scene) = result.scene {
                                i.scene = Some(scene);
                                invalidate_view_request(&mut i); // re-dispatch without refitting time
                            }
                        }
                        _ => {}
                    }
                    i.command_outcome = Some(match (result.action.as_str(), result.message) {
                        ("error", Some(message)) => CommandOutcome::Failed(message),
                        ("error", None) => CommandOutcome::Failed("command failed".to_owned()),
                        _ => CommandOutcome::Succeeded,
                    });
                }
                Ok(())
            },
        )
    }

    /// Fetch station coordinates (FDSN station service) to enable distance /
    /// azimuth sorting; forces the view to re-dispatch so the sort recomputes.
    pub fn fetch_stations(&self, params: StationRequest) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::FetchStations,
            None,
            Vec::new(),
            async move {
                let result = client()
                    .fetch_stations(params)
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                install_snuffling_result(&mut i, result);
                invalidate_view_request(&mut i); // re-dispatch so distance/azimuth sort recomputes
                Ok(())
            },
        )
    }

    /// Fetch instrument responses (`StationXML`) so Deconvolution can run;
    /// forces a re-dispatch so the deconvolved view recomputes.
    pub fn fetch_responses(&self, params: StationRequest) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::FetchResponses,
            None,
            Vec::new(),
            async move {
                let result = client()
                    .fetch_responses(params)
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                install_snuffling_result(&mut i, result);
                invalidate_view_request(&mut i); // re-dispatch so deconvolution applies
                Ok(())
            },
        )
    }

    /// Open local waveform files via a browser picker and append them.
    pub fn open_files(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow_with_outcome(
            inner,
            self.repaint.clone(),
            Operation::OpenSelectedFiles,
            Some("selected files".to_owned()),
            Vec::new(),
            move |flow| async move {
                let page =
                    decode_browser_file_page(js_open_files().await, "open_files", "opening files")?;
                run_file_load(work_inner, flow, page, SourceLoadMode::Append).await
            },
        )
    }

    /// Recursively load every waveform file under a picked directory
    /// (Chromium File System Access); appends them to the pile.
    pub fn open_directory(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow_with_outcome(
            inner,
            self.repaint.clone(),
            Operation::OpenDirectory,
            Some("selected directory".to_owned()),
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_open_directory().await,
                    "open_directory",
                    "opening a directory",
                )?;
                run_file_load(work_inner, flow, page, SourceLoadMode::Append).await
            },
        )
    }

    /// Reload the Python snuffling code (bridge.py) in the worker, preserving the loaded
    /// pile (snuffler's Shift+R).
    /// Forces a view re-fetch since the reload resets Python's view cache; the scene
    /// (track list) is unchanged.
    pub fn reload_snufflings(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::ReloadSnufflings,
            None,
            Vec::new(),
            async move {
                let job = require_registered_job(Operation::ReloadSnufflings, "reloadSnufflings")
                    .map_err(CallError::from)?;
                js_reload_snufflings(job.id().get())
                    .await
                    .map_err(|error| {
                        BridgeError::browser("reloading snufflings", jsval_to_string(error))
                    })?;
                invalidate_view_request(&mut work_inner.borrow_mut());
                Ok(())
            },
        )
    }

    /// Re-read any opened files whose on-disk mtime changed and replace their
    /// traces (snuffler's "r").
    ///
    /// Successful sources are installed even when another source in the same
    /// batch fails.
    /// Those failures remain visible through the recoverable diagnostic slot.
    pub fn reload_files(&self) -> Submission {
        let inner = self.inner.clone();
        inner.borrow_mut().diagnostic = None;
        let work_inner = inner.clone();
        submit_flow_with_outcome(
            inner,
            self.repaint.clone(),
            Operation::ReloadOpenFiles,
            Some("modified files".to_owned()),
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_reload_files().await,
                    "reload_files",
                    "reloading files",
                )?;
                run_file_load(work_inner, flow, page, SourceLoadMode::Replace).await
            },
        )
    }

    /// Qt's download Save trigger: the loaded waveforms as one `MiniSEED`
    /// download plus the station table as a pyrocko station file.
    /// A refusal remains a typed recoverable diagnostic, so either successful
    /// half still downloads and the viewer stays operational.
    pub fn save_waveforms_and_stations(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow(
            inner,
            self.repaint.clone(),
            Operation::ExportWaveforms,
            Some("waveforms and stations".to_owned()),
            Vec::new(),
            move |flow| async move {
                // Both halves run even when the first fails: one refused file
                // must not cost the user the other download.
                // The first failure takes the diagnostic slot.
                let waveforms =
                    download_blob(client().export_waveforms().await, "downloading waveforms").await;
                ensure_flow_live(&work_inner, flow, "waveform and station export")?;
                let failed = [
                    waveforms,
                    download_blob(client().export_stations().await, "downloading stations").await,
                ]
                .into_iter()
                .find_map(Result::err);
                work_inner.borrow_mut().diagnostic.clone_from(&failed);
                if let Some(error) = failed {
                    return Err(error);
                }
                Ok(())
            },
        )
    }

    /// The waveform half alone — the live window's "save the buffer as
    /// `MiniSEED`" (a stream rarely has a station table to save with it).
    /// A refusal surfaces as a recoverable diagnostic.
    pub fn save_waveforms(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::ExportWaveforms,
            Some("waveforms".to_owned()),
            Vec::new(),
            async move {
                let result =
                    download_blob(client().export_waveforms().await, "downloading waveforms").await;
                settle_diagnostic(&work_inner, result)
            },
        )
    }

    /// Serializes markers through pyrocko and downloads the resulting text.
    /// A failure surfaces as a typed recoverable diagnostic.
    pub fn save_markers(&self, markers: MarkerFile) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            Operation::MarkersToText,
            Some("markers.txt".to_owned()),
            Vec::new(),
            async move {
                let result = match client().markers_to_text(markers).await {
                    Ok(text) => {
                        download_bytes(
                            text.value.into_bytes(),
                            "markers.txt".to_owned(),
                            "text/plain".to_owned(),
                            "downloading markers",
                        )
                        .await
                    }
                    Err(error) => Err(error.into()),
                };
                settle_diagnostic(&work_inner, result)
            },
        )
    }

    /// Trigger a browser download of encoded image bytes (PNG export).
    pub fn save_image_png(&self, bytes: Vec<u8>, filename: String) -> Submission {
        let detail = Some(filename.clone());
        let inner = self.inner.clone();
        submit_local_future(
            inner,
            self.repaint.clone(),
            Operation::ExportPng,
            detail,
            async move {
                js_save_png(bytes, filename).await.map_err(|error| {
                    BridgeError::browser("saving a PNG", jsval_to_string(error))
                })?;
                Ok(())
            },
        )
    }

    /// Trigger a browser download of an SVG document (vector image export).
    pub fn save_image_svg(&self, svg: String, filename: String) -> Submission {
        let detail = Some(filename.clone());
        let inner = self.inner.clone();
        submit_local_future(
            inner,
            self.repaint.clone(),
            Operation::ExportSvg,
            detail,
            async move {
                js_save_svg(svg, filename).await.map_err(|error| {
                    BridgeError::browser("saving an SVG", jsval_to_string(error))
                })?;
                Ok(())
            },
        )
    }

    /// Load a marker file via a browser picker; results land as markers.
    pub fn load_markers(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow(
            inner,
            self.repaint.clone(),
            Operation::LoadMarkers,
            None,
            Vec::new(),
            move |flow| async move {
                let texts =
                    picked_file_texts(js_load_markers().await, "markers", "loading markers")?;
                ensure_flow_live(&work_inner, flow, "loading markers")?;
                let Some(text) = texts.into_iter().next() else {
                    return Ok(());
                };
                let loaded = client()
                    .load_markers(pb::LoadMarkersRequest {
                        text,
                        marker_budget: Some(marker_budget()),
                    })
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                i.diagnostic = None;
                let n = loaded.markers.len();
                install_snuffling_result(
                    &mut i,
                    SnufflingResult {
                        show_window: true,
                        text: format!("loaded {n} marker(s)"),
                        markers: loaded.markers,
                        scene: None,
                        origin: String::new(),
                        event_state: loaded.event_state,
                        markers_dropped: loaded.markers_dropped,
                        marker_budget: loaded.marker_budget,
                        nslc_table: loaded.nslc_table,
                    },
                );
                Ok(())
            },
        )
    }

    /// Pick and merge local `StationXML` files (instrument responses for
    /// deconvolution + station coordinates for sorting) using the same parsing
    /// as fetched `FDSN` responses.
    pub fn load_station_xml(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow(
            inner,
            self.repaint.clone(),
            Operation::LoadStationXml,
            None,
            Vec::new(),
            move |flow| async move {
                let texts = picked_file_texts(
                    js_load_station_xml().await,
                    "stationxml",
                    "loading StationXML",
                )?;
                ensure_flow_live(&work_inner, flow, "loading StationXML")?;
                if texts.is_empty() {
                    return Ok(());
                }
                let result = client()
                    .load_station_xml(pb::LoadStationXmlRequest { texts })
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                i.diagnostic = None;
                install_snuffling_result(&mut i, result);
                Ok(())
            },
        )
    }

    /// Pick a local pyrocko station file and load its station coordinates
    /// (distance/azimuth sorting, cake, map) via `model.load_stations`.
    pub fn load_stations(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow(
            inner,
            self.repaint.clone(),
            Operation::LoadStations,
            None,
            Vec::new(),
            move |flow| async move {
                let texts =
                    picked_file_texts(js_load_stations().await, "stations", "loading stations")?;
                ensure_flow_live(&work_inner, flow, "loading stations")?;
                if texts.is_empty() {
                    return Ok(());
                }
                let result = client()
                    .load_stations(pb::LoadStationsRequest {
                        text: texts.join("\n"),
                    })
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                i.diagnostic = None;
                install_snuffling_result(&mut i, result);
                Ok(())
            },
        )
    }

    /// Pick a local pyrocko event file and load it: event markers + the active
    /// event (largest magnitude) for distance/azimuth sorting, which forces a
    /// re-dispatch so that sort recomputes against the new active event.
    pub fn load_events(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow(
            inner,
            self.repaint.clone(),
            Operation::LoadEvents,
            None,
            Vec::new(),
            move |flow| async move {
                let texts = picked_file_texts(js_load_events().await, "events", "loading events")?;
                ensure_flow_live(&work_inner, flow, "loading events")?;
                let Some(text) = texts.into_iter().next() else {
                    return Ok(());
                };
                let result = client()
                    .load_events(pb::LoadEventsRequest {
                        text,
                        marker_budget: Some(marker_budget()),
                    })
                    .await
                    .map_err(BridgeError::from)?;
                let mut i = work_inner.borrow_mut();
                i.diagnostic = None;
                install_snuffling_result(&mut i, result);
                invalidate_view_request(&mut i);
                Ok(())
            },
        )
    }

    /// Fetch station + event coordinates for the Map overlay.
    pub fn fetch_map_data(&self, request: MapRequest) -> Submission {
        let inner = self.inner.clone();
        let request_gen = {
            let mut i = inner.borrow_mut();
            if let Some(job) = i.map_job
                && i.map_request.as_ref() == Some(&request)
            {
                return Submission::Joined(job);
            }
            let changed = i.map_request.as_ref() != Some(&request);
            i.map_request = Some(request.clone());
            i.map_request_gen = i.map_request_gen.wrapping_add(1);
            if changed {
                i.map = None;
            }
            i.map_request_gen
        };
        let work_inner = inner.clone();
        let dedup_key = request.encode_to_vec();
        let submission = submit_job(
            inner,
            self.repaint.clone(),
            Operation::MapData,
            None,
            dedup_key,
            async move {
                let result = client().map_data(request).await;
                let mut i = work_inner.borrow_mut();
                if i.map_request_gen == request_gen {
                    i.map_job = None;
                    i.map = Some(result.map_err(BridgeError::from)?);
                }
                Ok(())
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            self.inner.borrow_mut().map_job = Some(handle);
        }
        submission
    }

    /// Polls watched files without showing a loading status.
    ///
    /// Successful changes install immediately; source failures still surface
    /// because otherwise their earlier batch peers would change invisibly.
    pub fn poll_watched_files(&self) -> Submission {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_flow_with_outcome(
            inner,
            self.repaint.clone(),
            Operation::PollWatchedFiles,
            None,
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_reload_files().await,
                    "poll_watched_files",
                    "watching files",
                )?;
                run_file_load(work_inner, flow, page, SourceLoadMode::Replace).await
            },
        )
    }

    /// One canonical-event mutation: run the rpc, then install Python's registry as the
    /// new event state.
    /// The three event editors below differ only in the rpc they run.
    fn submit_event_state_call<F>(&self, operation: Operation, call: F) -> Submission
    where
        F: Future<Output = Result<ActiveEventResult, CallError>> + 'static,
    {
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        submit_job(
            inner,
            self.repaint.clone(),
            operation,
            None,
            Vec::new(),
            async move {
                let state = call.await.map_err(BridgeError::from)?;
                let mut current = work_inner.borrow_mut();
                current.diagnostic = None;
                install_event_state(&mut current, state);
                Ok(())
            },
        )
    }

    /// Sets or clears the active event and installs Python's canonical registry.
    pub fn set_active_event(&self, req: SetActiveEventRequest) -> Submission {
        self.submit_event_state_call(Operation::SetActiveEvent, async move {
            client().set_active_event(req).await
        })
    }

    /// Replaces one canonical event after an inline marker-table edit.
    pub fn update_event(&self, event: EventSnapshot) -> Submission {
        self.submit_event_state_call(Operation::UpdateEvent, async move {
            client().update_event(event).await
        })
    }

    /// Removes canonical events whose final frontend marker was deleted.
    pub fn remove_events(&self, request: RemoveEventsRequest) -> Submission {
        self.submit_event_state_call(Operation::RemoveEvents, async move {
            client().remove_events(request).await
        })
    }

    /// Submit every tile the map wants on screen right now.
    ///
    /// This replaces the pending queue rather than adding to it, which is what makes a
    /// zoom or a pan cheap: tiles the view has moved away from and that no batch has
    /// started yet are simply dropped, and the survivors are re-ordered by the priority
    /// the view just scored them with.
    /// Tiles already fetched, in flight, or known to have failed are skipped, so calling
    /// this every frame costs a filter over a screenful of keys.
    pub fn request_tiles(&self, wanted: Vec<TileRequest>) {
        {
            let mut i = self.inner.borrow_mut();
            let mut queue: Vec<TileRequest> = wanted
                .into_iter()
                .filter(|t| !i.tiles.contains_key(&(t.z, t.x, t.y)))
                .collect();
            queue.sort_by_key(|t| t.priority);
            i.tile_queue = queue;
        }
        self.pump_tiles();
    }

    /// Start batches until the queue empties or the in-flight cap is reached.
    fn pump_tiles(&self) {
        loop {
            let batch = {
                let mut i = self.inner.borrow_mut();
                if i.tile_batches >= TILE_BATCHES_IN_FLIGHT || i.tile_queue.is_empty() {
                    return;
                }
                let take = i.tile_queue.len().min(TILE_BATCH);
                let batch: Vec<TileRequest> = i.tile_queue.drain(..take).collect();
                for tile in &batch {
                    i.tiles
                        .insert((tile.z, tile.x, tile.y), TileState::InFlight);
                }
                i.tile_batches += 1;
                batch
            };
            self.spawn_tile_batch(batch);
        }
    }

    /// Fetch and decode one batch in the browser, filing each tile as it lands.
    fn spawn_tile_batch(&self, batch: Vec<TileRequest>) {
        let inner = self.inner.clone();
        let repaint = self.repaint.clone();
        let me = self.clone();
        let epoch = self.inner.borrow().tile_epoch;
        let keys: Vec<(u8, u32, u32)> = batch.iter().map(|t| (t.z, t.x, t.y)).collect();
        let urls = js_sys::Array::new();
        for tile in &batch {
            urls.push(&JsValue::from_str(&tile.url));
        }

        #[expect(
            clippy::disallowed_methods,
            reason = "the bounded tile-batch counter owns this deferred browser fetch"
        )]
        spawn_local(async move {
            let on_tile = {
                let inner = inner.clone();
                let repaint = repaint.clone();
                let keys = keys.clone();
                // Tile dimensions arrive as JS numbers that are small
                // non-negative pixel counts.
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "tile width/height are small non-negative integers"
                )]
                Closure::<dyn FnMut(f64, f64, f64, js_sys::Uint8Array)>::new(
                    move |index: f64, width: f64, height: f64, rgba: js_sys::Uint8Array| {
                        let Some(&key) = keys.get(index as usize) else {
                            return;
                        };
                        let (width, height) = (width as u32, height as u32);
                        let bytes = rgba.to_vec();
                        let decoded = width > 0
                            && height > 0
                            && bytes.len() == (width as usize) * (height as usize) * 4;
                        {
                            let mut i = inner.borrow_mut();
                            // The basemap changed while this tile was in
                            // flight; it now holds the wrong provider's
                            // imagery for its key.
                            if i.tile_epoch != epoch {
                                return;
                            }
                            i.tiles.insert(
                                key,
                                if decoded {
                                    TileState::Ready(width, height, bytes)
                                } else {
                                    TileState::Failed
                                },
                            );
                        }
                        repaint.request_repaint();
                    },
                )
            };
            let delivered = js_fetch_tiles(
                urls,
                on_tile.as_ref().unchecked_ref::<js_sys::Function>().clone(),
            )
            .await;
            drop(on_tile);
            {
                let mut i = inner.borrow_mut();
                i.tile_batches = i.tile_batches.saturating_sub(1);
                // The batch as a whole can reject before every callback fired (the module
                // failed to load, say).
                // Anything still marked in-flight has no other way to be retired, and
                // would block its key from ever being requested again.
                if i.tile_epoch == epoch && delivered.is_err() {
                    for key in keys {
                        if matches!(i.tiles.get(&key), Some(TileState::InFlight)) {
                            i.tiles.insert(key, TileState::Failed);
                        }
                    }
                }
            }
            repaint.request_repaint();
            me.pump_tiles();
        });
    }

    /// Request a view for the given params, coalescing rapid changes so that
    /// at most one Python call is in flight at a time.
    #[must_use]
    pub fn ensure_view(&self, params: ViewParams) -> Option<Submission> {
        let repaint = self.repaint.clone();
        {
            let mut i = self.inner.borrow_mut();
            if i.applied.as_ref() == Some(&params) {
                if i.view_job.is_none() {
                    i.desired = None;
                } else {
                    i.desired = Some(params);
                    repaint.request_repaint_after(std::time::Duration::from_millis(17));
                }
                return i.view_job.as_ref().map(|(job, _)| Submission::Joined(*job));
            }
            let joined = i
                .view_job
                .as_ref()
                .filter(|(_, active)| active == &params)
                .map(|(job, _)| *job);
            if let Some(job) = joined {
                i.desired = None;
                repaint.request_repaint_after(std::time::Duration::from_millis(17));
                return Some(Submission::Joined(job));
            }
            i.desired = Some(params);
            if let Some((job, _)) = i.view_job {
                repaint.request_repaint_after(std::time::Duration::from_millis(17));
                return Some(Submission::Joined(job));
            }
            let now_s = js_sys::Date::now() / 1000.0;
            if now_s < i.view_retry_after_s {
                repaint.request_repaint_after(std::time::Duration::from_millis(500));
                return None;
            }
            // While the runtime is down, submitting would only mint one
            // rejected ledger record per frame; the runtime state itself is
            // already on screen, and `desired` re-dispatches on recovery.
            // (Booting still submits: those jobs queue until Ready.)
            if matches!(
                i.activity.runtime(),
                RuntimeState::Restarting | RuntimeState::Failed(_)
            ) {
                repaint.request_repaint_after(std::time::Duration::from_millis(200));
                return None;
            }
        }
        self.dispatch_view()
    }

    fn dispatch_view(&self) -> Option<Submission> {
        let repaint = self.repaint.clone();
        let (params, canonical_params, request_gen) = {
            let mut i = self.inner.borrow_mut();
            if let Some((job, _)) = i.view_job {
                return Some(Submission::Joined(job));
            }
            let mut params = i.desired.take()?;
            if i.applied.as_ref() == Some(&params) {
                return None;
            }
            let canonical_params = params.clone();
            i.next_view_sequence = i.next_view_sequence.wrapping_add(1).max(1);
            params.view_sequence = i.next_view_sequence;
            (params, canonical_params, i.view_request_gen)
        };
        repaint.request_repaint_after(std::time::Duration::from_millis(17));
        let inner = self.inner.clone();
        let work_inner = inner.clone();
        let me = self.clone();
        let work_params = params.clone();
        let work_canonical_params = canonical_params.clone();
        let submission = submit_job(
            inner,
            repaint,
            Operation::GetView,
            None,
            params.encode_to_vec(),
            async move {
                let parsed = client()
                    .get_view(work_params.clone())
                    .await
                    .map(|(meta, ys)| ViewData { meta, ys });
                let outcome = {
                    let mut i = work_inner.borrow_mut();
                    if i.view_request_gen == request_gen {
                        match parsed {
                            Ok(vd) => {
                                if vd.meta.view_sequence != work_params.view_sequence
                                    || vd.meta.session_generation < i.python_session_generation
                                {
                                    i.desired = Some(work_canonical_params.clone());
                                    Err(BridgeError::browser(
                                        "applying a waveform view",
                                        "stale view reply",
                                    ))
                                } else {
                                    i.python_session_generation = vd.meta.session_generation;
                                    i.view_retry_after_s = 0.0;
                                    // Every reply that passed the sequence and
                                    // generation fences is applied, even when a
                                    // newer window is already desired: during a
                                    // continuous drag the desired params are
                                    // always newer than any reply, and skipping
                                    // the fresh reply froze the canvas at the
                                    // pre-gesture window for the whole gesture.
                                    // The newer request still goes out —
                                    // `desired` stays set and maybe_redispatch
                                    // compares it against `applied` below.
                                    i.view = Some(std::sync::Arc::new(vd));
                                    i.view_epoch += 1;
                                    i.applied = Some(work_canonical_params.clone());
                                    if i.desired.as_ref() == Some(&work_canonical_params) {
                                        i.desired = None;
                                    }
                                    Ok(())
                                }
                            }
                            Err(error) => {
                                let error = BridgeError::from(error);
                                i.view_retry_after_s = js_sys::Date::now() / 1000.0 + 0.5;
                                i.desired = Some(work_canonical_params.clone());
                                Err(error)
                            }
                        }
                    } else {
                        Ok(())
                    }
                };
                if work_inner
                    .borrow()
                    .view_job
                    .as_ref()
                    .is_some_and(|(_, active)| active == &work_canonical_params)
                {
                    work_inner.borrow_mut().view_job = None;
                }
                let should_warm = outcome.is_ok();
                me.maybe_redispatch();
                if should_warm {
                    let _submission = me.prepare_scientific_tools();
                }
                outcome
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            self.inner.borrow_mut().view_job = Some((handle, canonical_params));
        }
        Some(submission)
    }

    fn maybe_redispatch(&self) {
        let need = {
            let mut i = self.inner.borrow_mut();
            if i.view_job.is_some() {
                false
            } else if js_sys::Date::now() / 1000.0 < i.view_retry_after_s {
                self.repaint
                    .request_repaint_after(std::time::Duration::from_millis(500));
                false
            } else if i.desired.is_some() && i.applied.as_ref() != i.desired.as_ref() {
                true
            } else {
                i.desired = None;
                false
            }
        };
        if need {
            let _submission = self.dispatch_view();
        }
    }
}
