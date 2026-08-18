// Thin browser-side client for the Pyodide worker. Rust calls this module via
// wasm-bindgen; this module keeps DOM-only work on the main thread and sends
// protobuf request bytes plus transferable buffers to web/bridge_worker.js.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  BridgeResultSchema,
  MlDevice as ProtoMlDevice,
  ReloadBatchResultSchema,
  ReloadFailureSchema,
  WorkerEventSchema,
  type BridgeError as ProtoBridgeError,
  type ReloadFailure,
  type SeisbenchProgress,
} from "./proto/snuffler/bridge/v1/bridge_pb.js";
import {
  AddCakeModelRequestSchema,
  BundleResultSchema,
  LoadBundleRequestSchema,
  LoadBytesRequestSchema,
  LoadEventsRequestSchema,
  LoadMarkersRequestSchema,
  LoadStationXmlRequestSchema,
  LoadStationsRequestSchema,
  type BundleResult,
  type LoadBytesResult,
} from "./proto/snuffler/bridge/v1/data_pb.js";
import type { SceneMeta } from "./proto/snuffler/bridge/v1/scene_pb.js";
import { rpc, BridgeMethod, type BridgeMethodId } from "./rpc.js";
import {
  WORKER_MOUNT_GF_STORE,
  WORKER_RELOAD_SNUFFLINGS,
  WORKER_WARM_DEFERRED,
  type WorkerCommand,
} from "./worker_command.js";

// What the worker accepts as a request `method`: any BridgeService rpc id, plus
// the single worker-only command (the GF-store directory mount).
type SeamMethod = BridgeMethodId | WorkerCommand;
const BRIDGE_PERF_LOG = false;
type WorkerEventMessage = { type: "workerEvent"; event: Uint8Array };

// Cosmetic progress hook (set by index.html) + the File System Access API. Typed
// loosely — optional integration points, not the typed proto boundary.
declare global {
  interface Window {
    __snufflerProgress?: (frac: number, label: string) => void;
    // The Pyodide worker index.html starts at page load, with the messages its
    // placeholder listener parked until ensureWorker() takes over.
    __snufflerWorker?: { worker: Worker; queue: MessageEvent[] };
    showDirectoryPicker?: (opts?: { mode?: string }) => Promise<unknown>;
    showOpenFilePicker?: (opts?: { multiple?: boolean }) => Promise<FileSystemFileHandle[]>;
  }
}

// What the worker reports once Python is up, read back from the live interpreter
// rather than assumed. Rust shows it in the About window.
type RuntimeInfo = {
  threaded: boolean;
  simd: boolean;
  pthreads: boolean;
  python: string;
  numpy: string;
  pyrocko: string;
  cores: number;
};

// Re-readable handles for files opened via the File System Access API, keyed by a
// stable source id, so "reload modified files" can re-read them from disk.
type SourceKind = "waveform" | "bundle";
type OpenSource = { handle: FileSystemFileHandle; mtime: number; kind: SourceKind };
const _openSources = new Map<string, OpenSource>();
let _sourceCounter = 0;

export function clearOpenSources() {
  _openSources.clear();
}

type Pending = {
  resolve: (msg: any) => void;
  reject: (err: Error) => void;
  method: SeamMethod;
  requestBytes: number;
  dataBytes: number;
  sentMs: number;
};

let worker: Worker | null = null;
let nextRequestId = 1;
const pending = new Map<number, Pending>();
let mlDevice = ProtoMlDevice.WASM;
let latestSeisbenchProgress: SeisbenchProgress | null = null;
let latestRuntimeInfo: RuntimeInfo | null = null;

function workerUrl() {
  return new URL("bridge_worker.js", document.baseURI).href;
}

function ensureWorker() {
  if (worker) return worker;
  // Adopt the worker index.html started at page load, which has had the whole
  // wasm download to get Python going; only fall back to creating one here if
  // that early start was skipped (no cross-origin isolation yet).
  //
  // Pyodide 314 rejects classic workers on BOTH paths — the threaded build
  // spawns its pthread workers as ES modules, and even the stock single-threaded
  // 314 CDN build refuses a classic worker — so always use a module worker.
  const early = window.__snufflerWorker;
  worker = early ? early.worker : new Worker(workerUrl(), { type: "module" });
  worker.onmessage = (event) => {
    const receivedMs = performance.now();
    const receivedEpochMs = Date.now();
    const msg = event.data || {};
    if (msg.type === "workerEvent") {
      handleWorkerEvent(msg as WorkerEventMessage);
      return;
    }
    if (msg.type === "runtime") {
      latestRuntimeInfo = msg as RuntimeInfo;
      return;
    }
    const slot = pending.get(msg.id);
    if (!slot) return;
    pending.delete(msg.id);
    if (BRIDGE_PERF_LOG && msg.perf && slot.method === BridgeMethod.getView) {
      const workerToMainMs =
        typeof msg.perf.worker_post_epoch_ms === "number"
          ? receivedEpochMs - msg.perf.worker_post_epoch_ms
          : NaN;
      console.log(
        "[bridge.js] " +
          `method=getView total_ms=${(receivedMs - slot.sentMs).toFixed(2)} ` +
          `worker_to_main_ms=${Number.isFinite(workerToMainMs) ? workerToMainMs.toFixed(2) : "n/a"} ` +
          `py_call_ms=${(msg.perf.py_call_ms || 0).toFixed(2)} ` +
          `shape_ms=${(msg.perf.shape_ms || 0).toFixed(2)} ` +
          `result_copy_ms=${(msg.perf.result_copy_ms || 0).toFixed(2)} ` +
          `buffer_view_ms=${(msg.perf.buffer_view_ms || 0).toFixed(2)} ` +
          `ys_copy_ms=${(msg.perf.ys_copy_ms || 0).toFixed(2)} ` +
          `request_bytes=${slot.requestBytes} result_bytes=${msg.perf.result_bytes || 0} ` +
          `ys_bytes=${msg.perf.ys_bytes || 0}`
      );
    }
    if (msg.ok) {
      slot.resolve(msg);
    } else {
      slot.reject(new Error(msg.error || "worker bridge request failed"));
    }
  };
  worker.onerror = (event) => {
    const error = new Error(event.message || "worker bridge crashed");
    for (const slot of pending.values()) slot.reject(error);
    pending.clear();
  };
  if (early) {
    // Replay what the early listener parked while the wasm was still loading —
    // boot progress, and the runtime info posted once Python is up.
    const queued = early.queue;
    window.__snufflerWorker = undefined;
    for (const event of queued) worker.onmessage(event);
  }
  return worker;
}

function handleWorkerEvent(msg: WorkerEventMessage) {
  const event = fromBinary(WorkerEventSchema, msg.event).event;
  switch (event.case) {
    case "boot":
      prog(event.value.fraction, event.value.label);
      break;
    case "seisbench":
      latestSeisbenchProgress = event.value;
      break;
    case "mlDevice":
      mlDevice = event.value.device;
      break;
  }
}

function prog(frac, label) {
  try {
    if (typeof window !== "undefined" && window.__snufflerProgress) {
      window.__snufflerProgress(frac, label);
    }
  } catch (_e) {
    // Ignore loading-overlay failures; they are cosmetic.
  }
}

function copyBytes(value) {
  if (value == null) return null;
  const src = value instanceof Uint8Array ? value : new Uint8Array(value.buffer || value);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

export function seisbenchDevice() {
  ensureWorker();
  return mlDevice === ProtoMlDevice.WEBGPU ? "WebGPU" : "CPU (WASM)";
}

export function seisbenchProgressFraction() {
  return latestSeisbenchProgress ? latestSeisbenchProgress.fraction : -1;
}

export function seisbenchProgressLabel() {
  return latestSeisbenchProgress ? latestSeisbenchProgress.label : "";
}

export function clearSeisbenchProgress() {
  latestSeisbenchProgress = null;
}

// The booted Python runtime, or null until the worker reports it.
export function runtimeInfo() {
  return latestRuntimeInfo;
}

// Post one request to the worker and resolve when its reply with the same id
// arrives. `rpc(name)` (imported) names the method; this just pipes bytes.
function send(method: SeamMethod, { request = null, data = null, handle = null } = {}) {
  const id = nextRequestId++;
  const payload: Record<string, any> = { id, method };
  const transfer: Transferable[] = [];

  const req = copyBytes(request);
  if (req) {
    payload.request = req;
    transfer.push(req.buffer);
  }

  const bytes = copyBytes(data);
  if (bytes) {
    payload.data = bytes;
    transfer.push(bytes.buffer);
  }

  if (handle != null) payload.handle = handle;

  const promise = new Promise<any>((resolve, reject) => {
    pending.set(id, {
      resolve,
      reject,
      method,
      requestBytes: req ? req.byteLength : 0,
      dataBytes: bytes ? bytes.byteLength : 0,
      sentMs: performance.now(),
    });
  });
  ensureWorker().postMessage(payload, transfer);
  return promise;
}

// Both data helpers now carry the BridgeResult envelope bytes in `result`
// (success variant OR BridgeError); Rust decodes the oneof. rpcBytes returns the
// envelope bytes; rpcBufferResult additionally carries the zero-copy ys buffer.
async function rpcBytes(method: BridgeMethodId, args?) {
  const msg = await send(method, args);
  return new Uint8Array(msg.result || new ArrayBuffer(0));
}

async function rpcText(method: SeamMethod, args?) {
  const msg = await send(method, args);
  return msg.text || "";
}

async function rpcBufferResult(method: BridgeMethodId, args?) {
  const msg = await send(method, args);
  return {
    result: new Uint8Array(msg.result || new ArrayBuffer(0)),
    ys: new Uint8Array(msg.ys || new ArrayBuffer(0)),
  };
}

// Decode a BridgeResult envelope for the DOM glue that needs a string back for
// a cake model or GF store.
// The error variant rejects the browser operation, mirroring how Rust surfaces
// a BridgeError.
function resultText(env: Uint8Array): string {
  const r = fromBinary(BridgeResultSchema, env);
  if (r.result.case === "error") {
    throw new Error(`${r.result.value.kind}: ${r.result.value.message}`);
  }
  if (r.result.case === "text") return r.result.value.value;
  throw new Error(`unexpected bridge result: ${r.result.case}`);
}

// The rpcs that mean the app is up and the splash can go: whatever put the first
// scene there, plus the example index, which is the first thing a `?example=` link
// asks for and the one that reports a slug nobody has. Typed against the generated
// ids, so renaming an rpc is a tsc error rather than a splash that never clears.
let bootOverlayDone = false;
const BOOT_DONE_METHODS: BridgeMethodId[] = [
  BridgeMethod.makeExample,
  BridgeMethod.loadBundle,
  BridgeMethod.loadUrl,
  BridgeMethod.loadBytes,
  BridgeMethod.listExamples,
];

// The single transport seam Rust (src/bridge.rs) calls for every unary proto rpc:
// forward (method, request) to the worker and return the BridgeResult envelope
// bytes plus the optional out-of-band zero-copy ys buffer. The worker is a dumb
// relay; bridge.py runs the rpc. The DOM-only browser glue keeps its own exports
// below.
export async function call(method: BridgeMethodId, request, data) {
  let out;
  try {
    out = await rpcBufferResult(method, { request, data });
  } finally {
    // Complete the loading overlay once the first post-boot work has settled; the
    // boot sequence itself stops at 0.97. Not just the synthetic example: an app
    // opened on `?example=<slug>` loads a bundle instead, and keying the splash on
    // one rpc left it up at 97% forever with the waveforms drawn underneath.
    // In `finally`, because a load that *failed* must reveal the app too — the
    // error it produced is in the status line behind the splash.
    if (!bootOverlayDone && BOOT_DONE_METHODS.includes(method)) {
      bootOverlayDone = true;
      prog(1.0, "Ready");
    }
  }
  // After the FIRST view, not the first scene: the scene is only data, the view is
  // what gets painted, and warming ahead of it just moves the wait in front of the
  // frame the user is waiting for.
  if (method === BridgeMethod.getView) warmDeferredImports();
  return out;
}

// Pay for the imports the boot deferred (scipy) now that there are waveforms on
// screen, so the first filter does not. Fire-and-forget: it is an optimization,
// and anything that needs those modules imports them itself if it gets there first.
//
// Deliberately queued at idle rather than sent straight away: the worker runs it
// on the one Python thread it has, so an rpc that arrives meanwhile waits for it —
// sending it immediately put the whole warm-up in front of the first getView.
let warmRequested = false;
function warmDeferredImports() {
  if (warmRequested) return;
  warmRequested = true;
  const warm = () => void send(WORKER_WARM_DEFERRED).catch(() => {});
  const idle = (window as Window & { requestIdleCallback?: (cb: () => void) => void })
    .requestIdleCallback;
  if (idle) idle.call(window, warm);
  else window.setTimeout(warm, 1000);
}

export async function addCakeModel() {
  const files = await pickFiles(false);
  if (files.length === 0) return "";
  const f = files[0];
  const content = new Uint8Array(await f.arrayBuffer());
  const request = toBinary(
    AddCakeModelRequestSchema,
    create(AddCakeModelRequestSchema, { name: f.name, content })
  );
  return resultText(await rpcBytes(rpc("addCakeModel"), { request }));
}

export async function addGfStore() {
  if (typeof window.showDirectoryPicker !== "function") {
    throw new Error(
      "This browser has no File System Access API (showDirectoryPicker); " +
        "mounting a GF store directory needs a Chromium-based browser."
    );
  }
  let handle;
  try {
    handle = await window.showDirectoryPicker({ mode: "read" });
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") return "";
    throw e;
  }
  return rpcText(WORKER_MOUNT_GF_STORE, { handle });
}

// Reload the Python snuffling code (bridge.py) in the worker, preserving the
// loaded pile (snuffler's Shift+R). Returns "ok".
export async function reloadSnufflings() {
  return rpcText(WORKER_RELOAD_SNUFFLINGS, {});
}

function pickFiles(multiple): Promise<File[]> {
  return new Promise<File[]>((resolve) => {
    const inp = document.createElement("input");
    inp.type = "file";
    inp.multiple = !!multiple;
    inp.style.display = "none";
    let done = false;
    const finish = (files: File[]) => {
      if (done) return;
      done = true;
      window.removeEventListener("focus", onFocus);
      inp.remove();
      resolve(files);
    };
    inp.addEventListener("change", () => finish(inp.files ? Array.from(inp.files) : []), {
      once: true,
    });
    const onFocus = () =>
      setTimeout(() => {
        if (!done && (!inp.files || inp.files.length === 0)) finish([]);
      }, 500);
    window.addEventListener("focus", onFocus);
    document.body.appendChild(inp);
    inp.click();
  });
}

type LoadOutcome = {
  envelope: Uint8Array;
  loaded: boolean;
  kind: SourceKind;
  scene: SceneMeta | undefined;
  waveform: LoadBytesResult | null;
  bundle: BundleResult | null;
};

class RemoteLoadError extends Error {
  constructor(readonly remote: ProtoBridgeError) {
    super(remote.message ? `${remote.kind}: ${remote.message}` : remote.kind);
  }
}

class ProtocolLoadError extends Error {}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function checkedLoadOutcome(envelope: Uint8Array, kind: SourceKind): LoadOutcome {
  let result;
  try {
    result = fromBinary(BridgeResultSchema, envelope);
  } catch (error) {
    throw new ProtocolLoadError(errorDetail(error));
  }
  if (result.result.case === "error") {
    throw new RemoteLoadError(result.result.value);
  }
  if (kind === "waveform" && result.result.case === "loadBytes") {
    return {
      envelope,
      loaded: result.result.value.loaded,
      kind,
      scene: result.result.value.scene,
      waveform: result.result.value,
      bundle: null,
    };
  }
  if (kind === "bundle" && result.result.case === "bundle") {
    return {
      envelope,
      loaded: true,
      kind,
      scene: result.result.value.scene,
      waveform: null,
      bundle: result.result.value,
    };
  }
  throw new ProtocolLoadError(`unexpected bridge result: ${result.result.case}`);
}

function reloadFailure(
  sourceId: string,
  name: string,
  error: unknown,
  browserRead: boolean
): ReloadFailure {
  const failure = create(ReloadFailureSchema, { sourceId, name });
  if (browserRead) {
    failure.reason = { case: "browserRead", value: errorDetail(error) };
  } else if (error instanceof RemoteLoadError) {
    failure.reason = { case: "remote", value: error.remote };
  } else if (error instanceof ProtocolLoadError) {
    failure.reason = { case: "protocol", value: error.message };
  } else {
    failure.reason = { case: "transport", value: errorDetail(error) };
  }
  return failure;
}

function mergeOpenOutcome(previousBundle: BundleResult | null, outcome: LoadOutcome) {
  if (outcome.bundle) {
    return { envelope: outcome.envelope, bundle: outcome.bundle };
  }
  if (!previousBundle || !outcome.scene) {
    return { envelope: outcome.envelope, bundle: previousBundle };
  }
  const bundle = create(BundleResultSchema, { ...previousBundle, scene: outcome.scene });
  const envelope = toBinary(
    BridgeResultSchema,
    create(BridgeResultSchema, { result: { case: "bundle", value: bundle } })
  );
  return { envelope, bundle };
}

async function loadSource(
  name: string,
  data: Uint8Array,
  append: boolean,
  sourceId: string,
  replaceSource: boolean
) {
  const request = toBinary(
    LoadBytesRequestSchema,
    create(LoadBytesRequestSchema, {
      name,
      fmt: "detect",
      append,
      content: data,
      sourceId,
      replaceSource,
    })
  );
  const envelope = await rpcBytes(rpc("loadBytes"), { request });
  return checkedLoadOutcome(envelope, "waveform");
}

// A picked file may be a whole dataset rather than one waveform file. Zips go to
// LoadBundle, which loads every member and returns the station/event results
// alongside the scene; everything else keeps the plain per-file path and is
// wrapped so `openFiles` has one return type either way.
const ZIP_MAGIC = [0x50, 0x4b, 0x03, 0x04];

function isZip(data: Uint8Array) {
  return data.length >= 4 && ZIP_MAGIC.every((byte, i) => data[i] === byte);
}

async function loadOne(
  name: string,
  data: Uint8Array,
  append: boolean,
  sourceId: string,
  replaceSource = false
) {
  if (isZip(data)) {
    const request = toBinary(
      LoadBundleRequestSchema,
      create(LoadBundleRequestSchema, { name, content: data })
    );
    const envelope = await rpcBytes(rpc("loadBundle"), { request });
    return checkedLoadOutcome(envelope, "bundle");
  }
  return loadSource(name, data, append, sourceId, replaceSource);
}

function rememberOpenSource(
  sourceId: string,
  handle: FileSystemFileHandle,
  mtime: number,
  outcome: LoadOutcome
) {
  if (!outcome.loaded) return;
  if (outcome.kind === "bundle") {
    // A bundle replaces the whole dataset, so handles for the displaced pile no
    // longer name sources Python could reload.
    _openSources.clear();
  }
  _openSources.set(sourceId, { handle, mtime, kind: outcome.kind });
}

// One unreadable file must not abort the rest of a multi-file open: snuffler
// logs it, skips it, and reports the set at the end (pile.py:502). Here every
// failure is logged as it happens; when *nothing* loaded the collected set is
// raised so the status line explains what went wrong instead of saying Ready.
function reportSkipped(failed: string[], loadedAny: boolean) {
  if (failed.length === 0) return;
  if (!loadedAny) {
    throw new Error(`could not read: ${failed.join(", ")}`);
  }
  console.warn(`skipped unreadable file(s): ${failed.join(", ")}`);
}

export async function openFiles() {
  // Prefer showOpenFilePicker (Chromium): it yields re-readable handles so the
  // files can later be reloaded from disk. Fall back to a hidden <input> (File
  // snapshots, no reload) on browsers without the File System Access API.
  if (typeof window.showOpenFilePicker === "function") {
    let handles: FileSystemFileHandle[];
    try {
      handles = await window.showOpenFilePicker({ multiple: true });
    } catch {
      return new Uint8Array(0); // user cancelled
    }
    let result: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
    let bundle: BundleResult | null = null;
    let loadedAny = false;
    const failed: string[] = [];
    for (const handle of handles) {
      const sourceId = `src${_sourceCounter++}`;
      try {
        const f = await handle.getFile();
        const data = new Uint8Array(await f.arrayBuffer());
        const outcome = await loadOne(f.name, data, true, sourceId);
        const merged = mergeOpenOutcome(bundle, outcome);
        result = merged.envelope;
        bundle = merged.bundle;
        rememberOpenSource(sourceId, handle, f.lastModified, outcome);
        loadedAny ||= outcome.loaded;
      } catch (e) {
        _openSources.delete(sourceId);
        failed.push(handle.name || "?");
        console.warn(`skipping ${handle.name}:`, e);
      }
    }
    reportSkipped(failed, loadedAny);
    return result;
  }
  const files = await pickFiles(true);
  if (files.length === 0) return new Uint8Array(0);
  let result: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
  let bundle: BundleResult | null = null;
  let loadedAny = false;
  const failed: string[] = [];
  for (const f of files) {
    try {
      const data = new Uint8Array(await f.arrayBuffer());
      const outcome = await loadOne(f.name, data, true, "");
      const merged = mergeOpenOutcome(bundle, outcome);
      result = merged.envelope;
      bundle = merged.bundle;
      loadedAny ||= outcome.loaded;
    } catch (e) {
      failed.push(f.name || "?");
      console.warn(`skipping ${f.name}:`, e);
    }
  }
  reportSkipped(failed, loadedAny);
  return result;
}

// Recursively load every file under a picked directory (snuffler's "Open
// waveform directory"). No extension filter: snuffler hands every file to
// format detection, which is what lets the extensionless SDS and day-file
// layouts (GE.WLF..BHZ.D.2023.037, 2009.123) load; whatever detection rejects
// is skipped per file. Chromium-only (showDirectoryPicker); handles are
// retained so the loaded files also participate in reload.
export async function openDirectory() {
  const pick = (window as unknown as { showDirectoryPicker?: (o?: unknown) => Promise<any> })
    .showDirectoryPicker;
  if (typeof pick !== "function") {
    throw new Error(
      "This browser has no File System Access API (showDirectoryPicker); " +
        "opening a waveform directory needs a Chromium-based browser."
    );
  }
  let dir: any;
  try {
    dir = await pick({ mode: "read" });
  } catch {
    return new Uint8Array(0); // cancelled
  }
  async function* walk(d: any): AsyncGenerator<any> {
    for await (const [, h] of d.entries()) {
      if (h.kind === "file") yield h;
      else if (h.kind === "directory") yield* walk(h);
    }
  }
  let scene: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
  let bundle: BundleResult | null = null;
  let loadedAny = false;
  const failed: string[] = [];
  for await (const handle of walk(dir)) {
    const sourceId = `src${_sourceCounter++}`;
    try {
      const f = await handle.getFile();
      const data = new Uint8Array(await f.arrayBuffer());
      const outcome = await loadOne(f.name, data, true, sourceId);
      const merged = mergeOpenOutcome(bundle, outcome);
      scene = merged.envelope;
      bundle = merged.bundle;
      rememberOpenSource(sourceId, handle, f.lastModified, outcome);
      loadedAny ||= outcome.loaded;
    } catch (e) {
      _openSources.delete(sourceId); // unreadable, or not a waveform format
      failed.push(handle.name || "?");
      console.warn(`skipping ${handle.name}:`, e);
    }
  }
  reportSkipped(failed, loadedAny);
  return scene;
}

// Re-read any opened files whose on-disk mtime changed and replace their traces
// (snuffler's "r" reload).
// Each changed source commits independently, so a later failure cannot hide an
// earlier successful scene or make that success impossible to observe.
export async function reloadFiles() {
  if (typeof window.showOpenFilePicker !== "function") {
    throw new Error(
      "This browser has no File System Access API (showOpenFilePicker); " +
        "reloading files from disk needs a Chromium-based browser."
    );
  }
  let latest: LoadOutcome | null = null;
  const failures: ReloadFailure[] = [];
  for (const [sourceId, src] of Array.from(_openSources)) {
    let name = src.handle.name || sourceId;
    let file: File;
    let data: Uint8Array;
    try {
      file = await src.handle.getFile();
      name = file.name;
      if (file.lastModified === src.mtime) continue;
      data = new Uint8Array(await file.arrayBuffer());
    } catch (error) {
      failures.push(reloadFailure(sourceId, name, error, true));
      continue;
    }

    let outcome: LoadOutcome;
    try {
      outcome = await loadOne(
        file.name,
        data,
        src.kind !== "bundle",
        sourceId,
        src.kind === "waveform"
      );
    } catch (error) {
      failures.push(reloadFailure(sourceId, name, error, false));
      continue;
    }

    const updated: OpenSource = {
      handle: src.handle,
      mtime: file.lastModified,
      kind: outcome.kind,
    };
    const replacedDataset = src.kind === "bundle" || outcome.kind === "bundle";
    if (replacedDataset) {
      _openSources.clear();
    }
    _openSources.set(sourceId, updated);
    if (outcome.loaded) latest = outcome;
    if (replacedDataset) break;
  }
  const result = create(ReloadBatchResultSchema, { failures });
  if (latest?.bundle) {
    result.outcome = { case: "bundle", value: latest.bundle };
  } else if (latest?.waveform) {
    result.outcome = { case: "waveform", value: latest.waveform };
  }
  return toBinary(ReloadBatchResultSchema, result);
}

// Every export goes through here — the web's stand-in for the save dialog Qt
// pops for each of its exports. showSaveFilePicker (Chromium, same API family
// as the open pickers used above) lets the user name and place the file; on
// engines without it, or when the click's transient activation has expired,
// the download attribute still carries the suggested name.
async function saveBlob(blob: Blob, suggestedName: string) {
  const picker = (window as unknown as { showSaveFilePicker?: (o?: unknown) => Promise<any> })
    .showSaveFilePicker;
  if (typeof picker === "function") {
    try {
      const handle = await picker.call(window, { suggestedName });
      const writable = await handle.createWritable();
      await writable.write(blob);
      await writable.close();
      return;
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return; // user cancelled
      // NotAllowedError (no user activation left) and the rest: fall through.
    }
  }
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = suggestedName;
  document.body.appendChild(a);
  a.click();
  setTimeout(() => {
    URL.revokeObjectURL(a.href);
    a.remove();
  }, 1000);
}

// Rust has already decoded the typed protobuf result before this DOM-only step.
export async function downloadFile(bytes, filename, mime) {
  // The bytes may be a wasm-bindgen view over shared memory, which `Blob`
  // rejects, so the browser receives a private copy.
  const blob = new Blob([copyBytes(bytes) ?? new Uint8Array()], {
    type: mime || "application/octet-stream",
  });
  await saveBlob(blob, filename || "download.bin");
}

export function savePng(bytes, filename) {
  // `bytes` is a wasm-bindgen view over the egui WASM heap, which is a
  // SharedArrayBuffer on this threaded build; the Blob constructor rejects shared
  // views ("must not be shared"). copyBytes() lifts it into a private,
  // non-shared buffer first (same trick wasm-bindgen uses for strings).
  const blob = new Blob([copyBytes(bytes) ?? new Uint8Array()], { type: "image/png" });
  void saveBlob(blob, filename || "snuffler.png");
}

export function saveSvg(svg, filename) {
  const blob = new Blob([svg], { type: "image/svg+xml" });
  void saveBlob(blob, filename || "snuffler.svg");
}

export async function loadMarkers() {
  const files = await pickFiles(false);
  if (files.length === 0) return new Uint8Array(0);
  const request = toBinary(
    LoadMarkersRequestSchema,
    create(LoadMarkersRequestSchema, { text: await files[0].text() })
  );
  return rpcBytes(rpc("loadMarkers"), { request });
}

export async function loadStationXml() {
  const files = await pickFiles(true);
  if (files.length === 0) return new Uint8Array(0);
  const request = toBinary(
    LoadStationXmlRequestSchema,
    create(LoadStationXmlRequestSchema, {
      texts: await Promise.all(files.map((file) => file.text())),
    })
  );
  return rpcBytes(rpc("loadStationXml"), { request });
}

export async function loadStations() {
  // Any number of station files in one gesture, as Qt's picker takes them; the
  // format is line-based, so the documents concatenate into one table.
  const files = await pickFiles(true);
  if (files.length === 0) return new Uint8Array(0);
  const texts = await Promise.all(files.map((file) => file.text()));
  const request = toBinary(
    LoadStationsRequestSchema,
    create(LoadStationsRequestSchema, { text: texts.join("\n") })
  );
  return rpcBytes(rpc("loadStations"), { request });
}

export async function loadEvents() {
  const files = await pickFiles(false);
  if (files.length === 0) return new Uint8Array(0);
  const request = toBinary(
    LoadEventsRequestSchema,
    create(LoadEventsRequestSchema, { text: await files[0].text() })
  );
  return rpcBytes(rpc("loadEvents"), { request });
}

async function decodeTile(url: string): Promise<ImageData> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`tile ${url}: ${r.status}`);
  const bmp = await createImageBitmap(await r.blob());
  const cv = new OffscreenCanvas(bmp.width, bmp.height);
  const cx = cv.getContext("2d");
  if (!cx) throw new Error("tile: 2d canvas context unavailable");
  cx.drawImage(bmp, 0, 0);
  const img = cx.getImageData(0, 0, bmp.width, bmp.height);
  if (bmp.close) bmp.close();
  return img;
}

/**
 * Fetch and decode a whole batch of map tiles at once.
 *
 * One call crosses the wasm boundary and starts every fetch in the batch, but
 * each tile is handed back through `onTile` the moment it decodes rather than at
 * the end — so a single slow tile delays only itself, and the map paints as its
 * pieces land. A tile that fails (404 past a provider's max zoom, offline) is
 * reported with zero dimensions so the caller can retire the slot instead of
 * waiting on it forever; the promise settles only once every tile is accounted
 * for, which is the caller's signal to start the next batch.
 */
export async function fetchTiles(
  urls: string[],
  onTile: (index: number, w: number, h: number, rgba: Uint8Array) => void
): Promise<void> {
  const empty = new Uint8Array(0);
  await Promise.all(
    urls.map(async (url, index) => {
      try {
        const img = await decodeTile(url);
        onTile(index, img.width, img.height, new Uint8Array(img.data.buffer));
      } catch {
        onTile(index, 0, 0, empty);
      }
    })
  );
}
