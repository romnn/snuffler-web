// Thin browser-side client for the Pyodide worker. Rust calls this module via
// wasm-bindgen; this module keeps DOM-only work on the main thread and sends
// protobuf request bytes plus transferable buffers to web/bridge_worker.js.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  BridgeResultSchema,
  CancelDisposition,
  CancelJobReplySchema,
  CancelJobRequestSchema,
  MlDevice as ProtoMlDevice,
  MlDeviceStatusSchema,
  MountGfStoreRequestSchema,
  RuntimeInfoSchema,
  WorkerEventSchema,
  type RuntimeInfo,
} from "./proto/snuffler/bridge/v1/bridge_pb.js";
import {
  ExecutionState,
  createExecutionControl,
  requestCancellation,
} from "./execution_control.js";
import {
  AddCakeModelRequestSchema,
  BrowserFileLoadPageRequestSchema,
  BrowserFileLoadPageSchema,
  BrowserFileSchema,
  BrowserReadFailureSchema,
  BrowserSourceAckListSchema,
  LoadEventsRequestSchema,
  LoadMarkersRequestSchema,
  LoadStationXmlRequestSchema,
  LoadStationsRequestSchema,
  type BrowserFile,
  type BrowserReadFailure,
} from "./proto/snuffler/bridge/v1/data_pb.js";
import { rpc, BridgeMethod, type BridgeMethodId } from "./rpc.js";
import { WORKER_RELOAD_SNUFFLINGS } from "./worker_command.js";
import {
  type WorkerEventMessage,
  type WorkerMessage,
  type WorkerRequest,
  type WorkerRequestMethod,
  type WorkerSuccessMessage,
} from "./worker_protocol.js";

const BRIDGE_PERF_LOG = false;
const WORKER_BOOT_TIMEOUT_MS = 300_000;
const NON_CANCELLABLE_REQUEST_TIMEOUT_MS = 30_000;
const MAX_PENDING_REQUESTS = 8;
const GET_VIEW_METHOD = rpc("getView");
const LIST_EXAMPLES_METHOD = rpc("listExamples");

// Cosmetic progress hook (set by index.html) + the File System Access API. Typed
// loosely — optional integration points, not the typed proto boundary.
declare global {
  interface Window {
    __snufflerProgress?: (frac: number, label: string) => void;
    // The Pyodide worker index.html starts at page load, with the messages its
    // placeholder listener parked until ensureWorker() takes over.
    __snufflerWorker?: { worker: Worker; queue: MessageEvent<WorkerMessage>[]; epoch: number };
    showDirectoryPicker?: (opts?: { mode?: string }) => Promise<unknown>;
    showOpenFilePicker?: (opts?: { multiple?: boolean }) => Promise<FileSystemFileHandle[]>;
  }
}

// Re-readable handles for files opened via the File System Access API, keyed by a
// stable source id, so "reload modified files" can re-read them from disk.
type SourceKind = "waveform" | "bundle";
type OpenSource = { handle: FileSystemFileHandle; mtime: number; kind: SourceKind };
const _openSources = new Map<string, OpenSource>();
let _sourceCounter = 0;
const _gfStoreDirectoryHandles = new Map<string, FileSystemDirectoryHandle>();
let _gfStoreDirectoryHandleCounter = 0;
const FILE_PAGE_BYTES = 32 * 1024 * 1024;
const FILE_PAGE_COUNT = 64;
type BrowserFileSource =
  | { case: "snapshot"; sourceId: ""; file: File }
  | {
      case: "handle";
      sourceId: string;
      handle: FileSystemFileHandle;
      file?: File;
      kind?: SourceKind;
      previousMtime?: number;
    };
type BrowserFileLoad = {
  iterator: AsyncIterator<BrowserFileSource>;
  pending?: BrowserFileSource;
  done: boolean;
};
const _fileLoads = new Map<string, BrowserFileLoad>();
let _fileLoadCounter = 0;

function cancelledFileLoadPage(): Uint8Array {
  return toBinary(
    BrowserFileLoadPageSchema,
    create(BrowserFileLoadPageSchema, { cancelled: true })
  );
}

export function clearOpenSources() {
  _openSources.clear();
  _fileLoads.clear();
}

type PendingRequest = {
  resolve: (msg: WorkerSuccessMessage) => void;
  reject: (err: Error) => void;
  method: WorkerRequestMethod;
  requestBytes: number;
  sentMs: number;
  jobId: bigint;
  control: Int32Array;
  deadline: number | null;
};

let worker: Worker | null = null;
let nextRequestId = 1;
let runtimeEpoch = typeof window === "undefined" ? 0 : (window.__snufflerWorker?.epoch ?? 0);
let bootTimer: number | null = null;
let interruptBuffer = new SharedArrayBuffer(Uint8Array.BYTES_PER_ELEMENT);
const pendingRequests = new Map<string, PendingRequest>();
let mlDevice = ProtoMlDevice.WASM;
const operationEvents: Uint8Array[] = [];
let latestRuntimeInfo: RuntimeInfo | null = null;

function pendingRequestKey(epoch: number, id: number): string {
  return `${epoch}:${id}`;
}

function workerUrl(epoch: number): string {
  const url = new URL("bridge_worker.js", document.baseURI);
  url.searchParams.set("epoch", String(epoch));
  return url.href;
}

function clearBootTimer(): void {
  if (bootTimer != null) {
    window.clearTimeout(bootTimer);
    bootTimer = null;
  }
}

function takePendingRequest(key: string): PendingRequest | undefined {
  const slot = pendingRequests.get(key);
  if (!slot) return undefined;
  pendingRequests.delete(key);
  if (slot.deadline != null) window.clearTimeout(slot.deadline);
  return slot;
}

function rejectAllPendingRequests(error: Error): void {
  for (const key of [...pendingRequests.keys()]) {
    takePendingRequest(key)?.reject(error);
  }
}

function handleWorkerEvent(msg: WorkerEventMessage, epoch: number): void {
  const decoded = fromBinary(WorkerEventSchema, msg.event);
  if (decoded.runtimeEpoch !== epoch) return;
  const event = decoded.event;
  switch (event.case) {
    case "boot":
      reportBootProgress(event.value.fraction, event.value.label);
      break;
    case "mlDevice":
      mlDevice = event.value.device;
      break;
    case "operation":
    case "runtimeFailure":
      operationEvents.push(msg.event.slice());
      if (operationEvents.length > 256) operationEvents.splice(0, operationEvents.length - 256);
      break;
    case "runtimeInfo":
      clearBootTimer();
      latestRuntimeInfo = event.value;
      break;
  }
}

function handleWorkerMessage(
  instance: Worker,
  epoch: number,
  event: MessageEvent<WorkerMessage>
): void {
  if (worker !== instance || runtimeEpoch !== epoch) return;
  const receivedMs = performance.now();
  const receivedEpochMs = Date.now();
  const msg = event.data;
  if (msg.epoch !== epoch) return;
  if ("type" in msg) {
    handleWorkerEvent(msg as WorkerEventMessage, epoch);
    return;
  }
  const key = pendingRequestKey(epoch, msg.id);
  const slot = takePendingRequest(key);
  if (!slot) return;
  if (msg.ok && BRIDGE_PERF_LOG && msg.perf && slot.method === BridgeMethod.getView) {
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
  if (msg.ok) slot.resolve(msg);
  else slot.reject(new Error(msg.error || "worker bridge request failed"));
}

function armBootDeadline(instance: Worker, epoch: number): void {
  clearBootTimer();
  bootTimer = window.setTimeout(() => {
    if (worker === instance && runtimeEpoch === epoch && latestRuntimeInfo == null) {
      recreateWorker(new Error(`Python runtime epoch ${epoch} timed out during boot`));
    }
  }, WORKER_BOOT_TIMEOUT_MS);
}

function ensureWorker(): Worker {
  if (worker) return worker;
  // Adopt the worker index.html started at page load, which has had the whole
  // wasm download to get Python going; only fall back to creating one here if
  // that early start was skipped (no cross-origin isolation yet).
  //
  // Pyodide 314 rejects classic workers on BOTH paths — the threaded build
  // spawns its pthread workers as ES modules, and even the stock single-threaded
  // 314 CDN build refuses a classic worker — so always use a module worker.
  const early = window.__snufflerWorker;
  if (early) runtimeEpoch = early.epoch;
  const epoch = runtimeEpoch;
  const instance = early?.worker ?? new Worker(workerUrl(epoch), { type: "module" });
  worker = instance;
  instance.onmessage = (event) => handleWorkerMessage(instance, epoch, event);
  instance.onerror = (event) => {
    if (worker === instance && runtimeEpoch === epoch) {
      recreateWorker(new Error(event.message || `Python runtime epoch ${epoch} crashed`));
    }
  };
  instance.onmessageerror = () => {
    if (worker === instance && runtimeEpoch === epoch) {
      recreateWorker(new Error(`Python runtime epoch ${epoch} sent an undecodable message`));
    }
  };
  armBootDeadline(instance, epoch);
  if (early) {
    // Replay what the early listener parked while the wasm was still loading.
    // Its epoch was minted before the app module existed, so those messages use
    // the same fence as messages received after adoption.
    const queued = early.queue;
    window.__snufflerWorker = undefined;
    for (const event of queued) handleWorkerMessage(instance, epoch, event);
  }
  return instance;
}

function recreateWorker(reason: Error): void {
  const previous = worker;
  clearBootTimer();
  worker = null;
  latestRuntimeInfo = null;
  rejectAllPendingRequests(reason);
  if (previous) {
    previous.onmessage = null;
    previous.onerror = null;
    previous.onmessageerror = null;
    previous.terminate();
  }
  window.__snufflerWorker = undefined;
  runtimeEpoch = (runtimeEpoch + 1) >>> 0;
  nextRequestId = 1;
  interruptBuffer = new SharedArrayBuffer(Uint8Array.BYTES_PER_ELEMENT);
  ensureWorker();
}

/** Returns the epoch of the active Python runtime. */
export function currentRuntimeEpoch(): number {
  ensureWorker();
  return runtimeEpoch;
}

/** Recreates the Python runtime and returns its new epoch. */
export function restartRuntime(reason = "Python runtime restart requested"): number {
  recreateWorker(new Error(reason));
  return runtimeEpoch;
}

function reportBootProgress(fraction, label) {
  try {
    if (typeof window !== "undefined" && window.__snufflerProgress) {
      window.__snufflerProgress(fraction, label);
    }
  } catch (_e) {
    // Ignore loading-overlay failures; they are cosmetic.
  }
}

function copyBytes(
  value: Uint8Array | ArrayBufferLike | null | undefined
): Uint8Array<ArrayBuffer> | null {
  if (value == null) return null;
  const src = value instanceof Uint8Array ? value : new Uint8Array(value);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

export function seisbenchDevice(): Uint8Array<ArrayBuffer> {
  ensureWorker();
  return toBinary(MlDeviceStatusSchema, create(MlDeviceStatusSchema, { device: mlDevice }));
}

// The booted Python runtime, or null until the worker reports it.
export function runtimeInfo(): Uint8Array<ArrayBuffer> | null {
  return latestRuntimeInfo == null ? null : toBinary(RuntimeInfoSchema, latestRuntimeInfo);
}

// Post one request to the worker and resolve when its reply with the same id
// arrives. `rpc(name)` (imported) names the method; this just pipes bytes.
function send(
  method: WorkerRequestMethod,
  {
    request = null,
    data = null,
    handle = null,
    jobId,
  }: {
    request?: Uint8Array | null;
    data?: Uint8Array | null;
    handle?: FileSystemDirectoryHandle | null;
    jobId: bigint;
  }
) {
  if (jobId <= 0n) {
    return Promise.reject(new Error("Python bridge calls require a ledger job id"));
  }
  if (pendingRequests.size >= MAX_PENDING_REQUESTS) {
    return Promise.reject(
      new Error(`Python bridge pending limit reached (${MAX_PENDING_REQUESTS})`)
    );
  }
  const target = ensureWorker();
  const epoch = runtimeEpoch;
  const id = nextRequestId++;
  const key = pendingRequestKey(epoch, id);
  const control = createExecutionControl();
  const payload: WorkerRequest = {
    epoch,
    id,
    method,
    jobId,
    control: control.buffer as SharedArrayBuffer,
    interrupt: interruptBuffer,
  };
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

  const promise = new Promise<WorkerSuccessMessage>((resolve, reject) => {
    pendingRequests.set(key, {
      resolve,
      reject,
      method,
      requestBytes: req ? req.byteLength : 0,
      sentMs: performance.now(),
      jobId: BigInt(jobId),
      control,
      deadline: null,
    });
  });
  if (method === GET_VIEW_METHOD || method === LIST_EXAMPLES_METHOD) {
    const slot = pendingRequests.get(key);
    if (slot) {
      slot.deadline = window.setTimeout(() => {
        if (worker === target && runtimeEpoch === epoch && pendingRequests.has(key)) {
          recreateWorker(
            new Error(`${String(method)} exceeded ${NON_CANCELLABLE_REQUEST_TIMEOUT_MS} ms`)
          );
        }
      }, NON_CANCELLABLE_REQUEST_TIMEOUT_MS);
    }
  }
  try {
    target.postMessage(payload, transfer);
  } catch (error) {
    takePendingRequest(key)?.reject(error instanceof Error ? error : new Error(String(error)));
  }
  return promise;
}

/** Requests cancellation for a ledger job and returns a typed disposition. */
export function cancelJob(requestBytes): Uint8Array {
  const request = fromBinary(CancelJobRequestSchema, copyBytes(requestBytes) || new Uint8Array());
  const slot = [...pendingRequests.values()].find((candidate) => candidate.jobId === request.jobId);
  let disposition = CancelDisposition.NOT_FOUND;
  if (slot) {
    const previous = requestCancellation(slot.control, new Uint8Array(interruptBuffer));
    if (previous === ExecutionState.Computing || previous === ExecutionState.Cancelling) {
      disposition = CancelDisposition.CANCELLING;
    } else {
      disposition = CancelDisposition.TOO_LATE;
    }
  }
  return toBinary(CancelJobReplySchema, create(CancelJobReplySchema, { disposition }));
}

// Both data helpers now carry the BridgeResult envelope bytes in `result`
// (success variant OR BridgeError); Rust decodes the oneof. rpcBytes returns the
// envelope bytes; rpcResultWithBuffer additionally carries the zero-copy ys buffer.
async function rpcBytes(
  method: WorkerRequestMethod,
  args: Parameters<typeof send>[1]
): Promise<Uint8Array> {
  const msg = await send(method, args);
  return new Uint8Array(msg.result || new ArrayBuffer(0));
}

async function rpcResultWithBuffer(method: BridgeMethodId, args: Parameters<typeof send>[1]) {
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
export async function call(
  method: BridgeMethodId,
  request: Uint8Array,
  data: Uint8Array,
  jobId: bigint
) {
  let out;
  try {
    out = await rpcResultWithBuffer(method, { request, data, jobId });
  } finally {
    // Complete the loading overlay once the first post-boot work has settled; the
    // boot sequence itself stops at 0.97. Not just the synthetic example: an app
    // opened on `?example=<slug>` loads a bundle instead, and keying the splash on
    // one rpc left it up at 97% forever with the waveforms drawn underneath.
    // In `finally`, because a load that *failed* must reveal the app too — the
    // error it produced is in the status line behind the splash.
    if (!bootOverlayDone && BOOT_DONE_METHODS.includes(method)) {
      bootOverlayDone = true;
      reportBootProgress(1.0, "Ready");
    }
  }
  // After the FIRST view, not the first scene: the scene is only data, the view is
  // what gets painted, and warming ahead of it just moves the wait in front of the
  // frame the user is waiting for.
  return out;
}

export function takeOperationEvents(): Uint8Array[] {
  return operationEvents.splice(0);
}

export async function addCakeModel(jobId: bigint) {
  const files = await pickFiles(false);
  if (files.length === 0) return "";
  const f = files[0];
  const content = new Uint8Array(await f.arrayBuffer());
  const request = toBinary(
    AddCakeModelRequestSchema,
    create(AddCakeModelRequestSchema, { name: f.name, content })
  );
  return resultText(await rpcBytes(rpc("addCakeModel"), { request, jobId }));
}

export async function addGfStore(jobId: bigint) {
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
  const handleId = `directory:${++_gfStoreDirectoryHandleCounter}`;
  _gfStoreDirectoryHandles.set(handleId, handle as FileSystemDirectoryHandle);
  try {
    const request = toBinary(
      MountGfStoreRequestSchema,
      create(MountGfStoreRequestSchema, { handleId })
    );
    return resultText(
      await rpcBytes(BridgeMethod.mountGfStore, {
        request,
        handle: _gfStoreDirectoryHandles.get(handleId),
        jobId,
      })
    );
  } finally {
    _gfStoreDirectoryHandles.delete(handleId);
  }
}

// Reload the Python snuffling code (bridge.py) in the worker, preserving the
// loaded pile (snuffler's Shift+R). Returns "ok".
export async function reloadSnufflings(jobId: bigint) {
  return resultText(await rpcBytes(WORKER_RELOAD_SNUFFLINGS, { jobId }));
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

// A picked file may be a whole dataset rather than one waveform file. Zips go to
// LoadBundle, which loads every member and returns the station/event results
// alongside the scene; everything else keeps the plain per-file path and is
// wrapped so `openFiles` has one return type either way.
const ZIP_MAGIC = [0x50, 0x4b, 0x03, 0x04];

function isZip(data: Uint8Array) {
  return data.length >= 4 && ZIP_MAGIC.every((byte, i) => data[i] === byte);
}

async function* fileSources(files: File[]): AsyncGenerator<BrowserFileSource> {
  for (const file of files) yield { case: "snapshot", sourceId: "", file };
}

async function* handleSources(handles: FileSystemFileHandle[]): AsyncGenerator<BrowserFileSource> {
  for (const handle of handles) {
    yield { case: "handle", sourceId: `src${_sourceCounter++}`, handle };
  }
}

async function readNextFileLoadPage(loadId: string): Promise<Uint8Array> {
  const load = _fileLoads.get(loadId);
  if (!load) throw new Error(`unknown browser file load ${loadId}`);
  const files: BrowserFile[] = [];
  const failures: BrowserReadFailure[] = [];
  let pageBytes = 0;
  while (files.length + failures.length < FILE_PAGE_COUNT) {
    let source = load.pending;
    load.pending = undefined;
    if (!source) {
      const next = await load.iterator.next();
      if (next.done) {
        load.done = true;
        break;
      }
      source = next.value;
    }
    let file: File;
    try {
      file =
        source.case === "snapshot" ? source.file : (source.file ?? (await source.handle.getFile()));
      if (
        source.case === "handle" &&
        source.previousMtime != null &&
        file.lastModified === source.previousMtime
      ) {
        continue;
      }
      if (file.size > FILE_PAGE_BYTES) {
        failures.push(
          create(BrowserReadFailureSchema, {
            handleId: source.sourceId,
            name: file.name,
            detail: `file is ${file.size} bytes; the per-file limit is ${FILE_PAGE_BYTES}`,
          })
        );
        continue;
      }
      if (pageBytes > 0 && pageBytes + file.size > FILE_PAGE_BYTES) {
        load.pending = { ...source, file };
        break;
      }
      const data = new Uint8Array(await file.arrayBuffer());
      const bundle = (source.case === "handle" && source.kind === "bundle") || isZip(data);
      if (source.case === "handle" && source.previousMtime == null) {
        _openSources.set(source.sourceId, {
          handle: source.handle,
          mtime: 0,
          kind: bundle ? "bundle" : "waveform",
        });
      }
      pageBytes += data.byteLength;
      files.push(
        create(BrowserFileSchema, {
          handleId: source.sourceId,
          name: file.name,
          content: data,
          lastModified: BigInt(file.lastModified),
          bundle,
        })
      );
    } catch (error) {
      if (source.case === "handle" && source.previousMtime == null) {
        _openSources.delete(source.sourceId);
      }
      failures.push(
        create(BrowserReadFailureSchema, {
          handleId: source.sourceId,
          name: source.case === "handle" ? source.handle.name : source.file.name,
          detail: error instanceof Error ? error.message : String(error),
        })
      );
    }
  }
  const hasMore = !load.done || load.pending != null;
  if (!hasMore) _fileLoads.delete(loadId);
  return toBinary(
    BrowserFileLoadPageSchema,
    create(BrowserFileLoadPageSchema, { files, failures, loadId, hasMore })
  );
}

async function startFileLoad(iterator: AsyncIterator<BrowserFileSource>): Promise<Uint8Array> {
  const loadId = `load${_fileLoadCounter++}`;
  _fileLoads.set(loadId, { iterator, done: false });
  return await readNextFileLoadPage(loadId);
}

export async function readFileLoadPage(requestBytes: Uint8Array): Promise<Uint8Array> {
  const request = fromBinary(BrowserFileLoadPageRequestSchema, requestBytes);
  return await readNextFileLoadPage(request.loadId);
}

export function cancelFileLoad(requestBytes: Uint8Array): void {
  const request = fromBinary(BrowserFileLoadPageRequestSchema, requestBytes);
  _fileLoads.delete(request.loadId);
}

export async function openFiles() {
  // Prefer showOpenFilePicker (Chromium): it yields re-readable handles so the
  // files can later be reloaded from disk.
  // Fall back to a hidden <input> (File snapshots, no reload) on browsers
  // without the File System Access API.
  if (typeof window.showOpenFilePicker === "function") {
    let handles: FileSystemFileHandle[];
    try {
      handles = await window.showOpenFilePicker({ multiple: true });
    } catch {
      return cancelledFileLoadPage();
    }
    return await startFileLoad(handleSources(handles));
  }
  const files = await pickFiles(true);
  if (files.length === 0) {
    return cancelledFileLoadPage();
  }
  return await startFileLoad(fileSources(files));
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
    return cancelledFileLoadPage();
  }
  async function* walk(d: any): AsyncGenerator<any> {
    for await (const [, h] of d.entries()) {
      if (h.kind === "file") yield h;
      else if (h.kind === "directory") yield* walk(h);
    }
  }
  async function* directorySources(): AsyncGenerator<BrowserFileSource> {
    for await (const handle of walk(dir)) {
      yield { case: "handle", sourceId: `src${_sourceCounter++}`, handle };
    }
  }
  return await startFileLoad(directorySources());
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
  async function* reloadSources(): AsyncGenerator<BrowserFileSource> {
    for (const [sourceId, source] of Array.from(_openSources)) {
      yield {
        case: "handle",
        sourceId,
        handle: source.handle,
        kind: source.kind,
        previousMtime: source.mtime,
      };
    }
  }
  return await startFileLoad(reloadSources());
}

export function acknowledgeSources(bytes: Uint8Array) {
  const acknowledgements = fromBinary(BrowserSourceAckListSchema, bytes);
  for (const source of acknowledgements.sources) {
    const current = _openSources.get(source.handleId);
    if (!current) continue;
    if (source.bundle) _openSources.clear();
    _openSources.set(source.handleId, {
      handle: current.handle,
      mtime: Number(source.lastModified),
      kind: source.bundle ? "bundle" : "waveform",
    });
  }
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

export async function savePng(bytes, filename) {
  // `bytes` is a wasm-bindgen view over the egui WASM heap, which is a
  // SharedArrayBuffer on this threaded build; the Blob constructor rejects shared
  // views ("must not be shared"). copyBytes() lifts it into a private,
  // non-shared buffer first (same trick wasm-bindgen uses for strings).
  const blob = new Blob([copyBytes(bytes) ?? new Uint8Array()], { type: "image/png" });
  await saveBlob(blob, filename || "snuffler.png");
}

export async function saveSvg(svg, filename) {
  const blob = new Blob([svg], { type: "image/svg+xml" });
  await saveBlob(blob, filename || "snuffler.svg");
}

export async function loadMarkers() {
  const files = await pickFiles(false);
  return pickedFilePage(files);
}

export async function loadStationXml() {
  const files = await pickFiles(true);
  return pickedFilePage(files);
}

export async function loadStations() {
  // Any number of station files in one gesture, as Qt's picker takes them; the
  // format is line-based, so the documents concatenate into one table.
  const files = await pickFiles(true);
  return pickedFilePage(files);
}

export async function loadEvents() {
  const files = await pickFiles(false);
  return pickedFilePage(files);
}

async function pickedFilePage(files: File[]) {
  if (files.length === 0) {
    return cancelledFileLoadPage();
  }
  return toBinary(
    BrowserFileLoadPageSchema,
    create(BrowserFileLoadPageSchema, {
      files: await Promise.all(
        files.map(async (file) =>
          create(BrowserFileSchema, {
            name: file.name,
            content: new Uint8Array(await file.arrayBuffer()),
            lastModified: BigInt(file.lastModified),
          })
        )
      ),
    })
  );
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
