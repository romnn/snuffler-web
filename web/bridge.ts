// The browser-side client for the Pyodide worker, and the one module Rust imports
// through wasm-bindgen (bundled to web/gen/bridge.js).
// It owns the worker's lifecycle and sends protobuf request bytes plus
// transferable buffers to web/bridge_worker.ts.
// The DOM-only glue lives beside it and is re-exported here:
// - browser_files.ts reads the user's files.
// - save_file.ts saves exports to disk.
// - map_tiles.ts fetches and decodes map tiles.
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  BridgeResultSchema,
  CancelDisposition,
  CancelJobReplySchema,
  CancelJobRequestSchema,
  MlDevice,
  MlDeviceStatusSchema,
  RuntimeFailureSchema,
  RuntimeInfoSchema,
  WorkerEventSchema,
  type RuntimeInfo,
} from "./proto/snuffler_web_proto/bridge_pb.js";
import { copyBytes } from "./bytes.js";
import {
  ExecutionState,
  createExecutionControl,
  requestCancellation,
} from "./execution_control.js";
import { rpc, BridgeMethod, type BridgeMethodId } from "./rpc.js";
import { WORKER_RELOAD_SNUFFLINGS } from "./worker_command.js";
import {
  type WorkerEventMessage,
  type WorkerMessage,
  type WorkerRequest,
  type WorkerRequestMethod,
  type WorkerSuccessMessage,
} from "./worker_protocol.js";

export {
  acknowledgeSources,
  cancelFileLoad,
  clearOpenSources,
  loadEvents,
  loadMarkers,
  loadStations,
  loadStationXml,
  openDirectory,
  openFiles,
  pickCakeModel,
  readFileLoadPage,
  reloadFiles,
} from "./browser_files.js";
export { fetchTiles } from "./map_tiles.js";
export { downloadFile, savePng, saveSvg } from "./save_file.js";

// A worker that has not reported its runtime by then is presumed hung and replaced.
const WORKER_BOOT_TIMEOUT_MS = 300_000;
// getView and listExamples cannot be cancelled, so a hung one replaces the worker.
const NON_CANCELLABLE_REQUEST_TIMEOUT_MS = 30_000;
const NON_CANCELLABLE_METHODS: WorkerRequestMethod[] = [rpc("getView"), rpc("listExamples")];
const MAX_PENDING_REQUESTS = 8;
// Consecutive boot failures after which the runtime halts instead of retrying.
const MAX_CONSECUTIVE_BOOT_FAILURES = 3;
// Operation events kept for Rust between two takeOperationEvents calls.
const MAX_QUEUED_OPERATION_EVENTS = 256;

// Hooks index.html installs on the window.
declare global {
  interface Window {
    // The loading overlay's progress bar; cosmetic.
    __snufflerProgress?: (fraction: number, label: string) => void;
    // The Pyodide worker index.html starts at page load, with the messages its
    // placeholder listener parked until ensureWorker() takes over.
    // `failure` carries a worker-script load error that fired before adoption.
    __snufflerWorker?: {
      worker: Worker;
      queue: MessageEvent<WorkerMessage>[];
      epoch: number;
      failure?: string | null;
    };
  }
}

// A request waiting for the worker's reply.
type PendingRequest = {
  resolve: (reply: WorkerSuccessMessage) => void;
  reject: (error: Error) => void;
  jobId: bigint;
  control: Int32Array;
  deadlineTimer: number | null;
};

let worker: Worker | null = null;
// Bumped on every worker replacement; replies and events from an older worker
// carry the old epoch and are dropped.
let runtimeEpoch = typeof window === "undefined" ? 0 : (window.__snufflerWorker?.epoch ?? 0);
let nextRequestId = 1;
let bootDeadlineTimer: number | null = null;
// Consecutive worker deaths that never reported runtimeInfo.
// A persistent boot failure (missing/broken worker script) would otherwise
// recreate workers forever without ever reaching the ledger's Failed state.
let consecutiveBootFailures = 0;
// Set once boot failures exhaust the retries; only restartRuntime() lifts it.
let runtimeHaltedReason: string | null = null;
// Pyodide's interrupt buffer, shared with every request of the current worker.
let interruptBuffer = new SharedArrayBuffer(Uint8Array.BYTES_PER_ELEMENT);
const pendingRequests = new Map<string, PendingRequest>();
let seisbenchDeviceStatus = MlDevice.WASM;
// Serialized WorkerEvents for Rust's activity ledger, drained by
// takeOperationEvents.
const queuedOperationEvents: Uint8Array[] = [];
let latestRuntimeInfo: RuntimeInfo | null = null;

function pendingRequestKey(epoch: number, id: number): string {
  return `${epoch}:${id}`;
}

function workerUrl(epoch: number): string {
  const url = new URL("bridge_worker.js", document.baseURI);
  url.searchParams.set("epoch", String(epoch));
  return url.href;
}

function clearBootDeadline(): void {
  if (bootDeadlineTimer != null) {
    window.clearTimeout(bootDeadlineTimer);
    bootDeadlineTimer = null;
  }
}

function takePendingRequest(key: string): PendingRequest | undefined {
  const pending = pendingRequests.get(key);
  if (!pending) return undefined;
  pendingRequests.delete(key);
  if (pending.deadlineTimer != null) window.clearTimeout(pending.deadlineTimer);
  return pending;
}

function rejectAllPendingRequests(error: Error): void {
  for (const key of [...pendingRequests.keys()]) {
    takePendingRequest(key)?.reject(error);
  }
}

function queueOperationEvent(event: Uint8Array): void {
  queuedOperationEvents.push(event);
  const overflow = queuedOperationEvents.length - MAX_QUEUED_OPERATION_EVENTS;
  if (overflow > 0) queuedOperationEvents.splice(0, overflow);
}

function handleWorkerEvent(message: WorkerEventMessage, epoch: number): void {
  const decoded = fromBinary(WorkerEventSchema, message.event);
  if (decoded.runtimeEpoch !== epoch) return;
  const event = decoded.event;
  switch (event.case) {
    case "boot":
      reportBootProgress(event.value.fraction, event.value.label);
      break;
    case "mlDevice":
      seisbenchDeviceStatus = event.value.device;
      break;
    case "operation":
    case "runtimeFailure":
      queueOperationEvent(message.event.slice());
      break;
    case "runtimeInfo":
      clearBootDeadline();
      latestRuntimeInfo = event.value;
      consecutiveBootFailures = 0;
      break;
  }
}

function handleWorkerMessage(
  instance: Worker,
  epoch: number,
  event: MessageEvent<WorkerMessage>
): void {
  if (worker !== instance || runtimeEpoch !== epoch) return;
  const message = event.data;
  if (message.epoch !== epoch) return;
  if ("type" in message) {
    handleWorkerEvent(message as WorkerEventMessage, epoch);
    return;
  }
  const pending = takePendingRequest(pendingRequestKey(epoch, message.id));
  if (!pending) return;
  if (message.ok) pending.resolve(message);
  else pending.reject(new Error(message.error || "worker bridge request failed"));
}

function armBootDeadline(instance: Worker, epoch: number): void {
  clearBootDeadline();
  bootDeadlineTimer = window.setTimeout(() => {
    if (worker === instance && runtimeEpoch === epoch && latestRuntimeInfo == null) {
      recreateWorker(new Error(`Python runtime epoch ${epoch} timed out during boot`));
    }
  }, WORKER_BOOT_TIMEOUT_MS);
}

// The current worker, started or adopted on first use; null once the runtime has
// halted.
function ensureWorker(): Worker | null {
  if (worker) return worker;
  // Persistent boot failure: recreating yet another worker would only repeat it.
  // The synthetic runtimeFailure event already put the ledger in Failed; an
  // explicit restartRuntime() lifts the halt for a user-initiated retry.
  if (runtimeHaltedReason != null) return null;

  // Adopt the worker index.html started at page load, which has had the whole
  // wasm download to get Python going.
  // Only create one here if that early start was skipped (no cross-origin
  // isolation yet).
  // Pyodide 314 rejects classic workers, so this is always a module worker.
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
    const failure = early.failure;
    window.__snufflerWorker = undefined;
    for (const event of queued) handleWorkerMessage(instance, epoch, event);
    if (failure != null) {
      // The eager worker's module script already failed before adoption; the
      // error event fired with only the placeholder listening.
      // Recreate now, through the bounded path, instead of waiting out the boot
      // deadline.
      recreateWorker(new Error(failure));
      return worker;
    }
  }
  return instance;
}

// Tears the current worker down, fails its requests with `reason`, and starts a
// new one under the next epoch.
// `isIntentionalTeardown` keeps a requested restart from counting as a boot
// failure.
function recreateWorker(reason: Error, isIntentionalTeardown = false): void {
  const previous = worker;
  const bootNeverCompleted = latestRuntimeInfo == null && !isIntentionalTeardown;

  // Tear down
  clearBootDeadline();
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

  // Halt after repeated boot failures
  if (bootNeverCompleted) {
    consecutiveBootFailures += 1;
    if (consecutiveBootFailures >= MAX_CONSECUTIVE_BOOT_FAILURES) {
      // Deterministic boot failure: stop recreating and tell the ledger.
      // The synthetic event takes the same path a live worker's boot failure does,
      // so Rust reaches RuntimeState::Failed instead of looping forever.
      runtimeHaltedReason = `Python runtime failed ${consecutiveBootFailures} times in a row: ${reason.message}`;
      queuedOperationEvents.push(
        toBinary(
          WorkerEventSchema,
          create(WorkerEventSchema, {
            runtimeEpoch,
            event: {
              case: "runtimeFailure",
              value: create(RuntimeFailureSchema, {
                kind: "BootFailure",
                message: runtimeHaltedReason,
              }),
            },
          })
        )
      );
      return;
    }
  } else {
    consecutiveBootFailures = 0;
  }

  ensureWorker();
}

/** Returns the epoch of the active Python runtime. */
export function currentRuntimeEpoch(): number {
  ensureWorker();
  return runtimeEpoch;
}

/** Recreates the Python runtime and returns its new epoch. */
export function restartRuntime(reason = "Python runtime restart requested"): number {
  // A user-initiated restart starts a fresh retry budget and lifts a
  // persistent-boot-failure halt.
  // The teardown itself is intentional, not a boot failure; counting it would
  // leave only two real attempts.
  consecutiveBootFailures = 0;
  runtimeHaltedReason = null;
  recreateWorker(new Error(reason), true);
  return runtimeEpoch;
}

function reportBootProgress(fraction: number, label: string) {
  try {
    if (typeof window !== "undefined" && window.__snufflerProgress) {
      window.__snufflerProgress(fraction, label);
    }
  } catch (_e) {
    // Ignore loading-overlay failures; they are cosmetic.
  }
}

// The device SeisBench inference runs on, as an MlDeviceStatus protobuf.
export function seisbenchDevice(): Uint8Array<ArrayBuffer> {
  ensureWorker();
  return toBinary(
    MlDeviceStatusSchema,
    create(MlDeviceStatusSchema, { device: seisbenchDeviceStatus })
  );
}

// The booted Python runtime, or null until the worker reports it.
export function runtimeInfo(): Uint8Array<ArrayBuffer> | null {
  return latestRuntimeInfo == null ? null : toBinary(RuntimeInfoSchema, latestRuntimeInfo);
}

// Post one request to the worker and resolve when its reply with the same id
// arrives.
// Every request belongs to a ledger job, which is what cancelJob finds it by.
function send(
  method: WorkerRequestMethod,
  {
    request = null,
    handle = null,
    jobId,
  }: {
    request?: Uint8Array | null;
    handle?: FileSystemDirectoryHandle | null;
    jobId: bigint;
  }
) {
  // Admission
  if (jobId <= 0n) {
    return Promise.reject(new Error("Python bridge calls require a ledger job id"));
  }
  if (pendingRequests.size >= MAX_PENDING_REQUESTS) {
    return Promise.reject(
      new Error(`Python bridge pending limit reached (${MAX_PENDING_REQUESTS})`)
    );
  }
  const target = ensureWorker();
  if (!target) {
    return Promise.reject(new Error(runtimeHaltedReason ?? "the Python runtime is unavailable"));
  }

  // Build the message
  const epoch = runtimeEpoch;
  const id = nextRequestId++;
  const key = pendingRequestKey(epoch, id);
  const control = createExecutionControl();
  const message: WorkerRequest = {
    epoch,
    id,
    method,
    jobId,
    control: control.buffer as SharedArrayBuffer,
    interrupt: interruptBuffer,
  };
  const transfer: Transferable[] = [];
  const requestCopy = copyBytes(request);
  if (requestCopy) {
    message.request = requestCopy;
    transfer.push(requestCopy.buffer);
  }
  if (handle != null) message.handle = handle;

  // Register the reply, with a deadline for the methods that cannot be cancelled
  const reply = new Promise<WorkerSuccessMessage>((resolve, reject) => {
    pendingRequests.set(key, {
      resolve,
      reject,
      jobId: BigInt(jobId),
      control,
      deadlineTimer: null,
    });
  });
  if (NON_CANCELLABLE_METHODS.includes(method)) {
    const pending = pendingRequests.get(key);
    if (pending) {
      pending.deadlineTimer = window.setTimeout(() => {
        if (worker === target && runtimeEpoch === epoch && pendingRequests.has(key)) {
          recreateWorker(
            new Error(`${String(method)} exceeded ${NON_CANCELLABLE_REQUEST_TIMEOUT_MS} ms`)
          );
        }
      }, NON_CANCELLABLE_REQUEST_TIMEOUT_MS);
    }
  }

  try {
    target.postMessage(message, transfer);
  } catch (error) {
    takePendingRequest(key)?.reject(error instanceof Error ? error : new Error(String(error)));
  }
  return reply;
}

/** Requests cancellation for a ledger job and returns a typed disposition. */
export function cancelJob(requestBytes): Uint8Array {
  const request = fromBinary(CancelJobRequestSchema, copyBytes(requestBytes) || new Uint8Array());
  const pending = [...pendingRequests.values()].find(
    (candidate) => candidate.jobId === request.jobId
  );
  let disposition = CancelDisposition.NOT_FOUND;
  if (pending) {
    const previous = requestCancellation(pending.control, new Uint8Array(interruptBuffer));
    if (previous === ExecutionState.Computing || previous === ExecutionState.Cancelling) {
      disposition = CancelDisposition.CANCELLING;
    } else {
      disposition = CancelDisposition.TOO_LATE;
    }
  }
  return toBinary(CancelJobReplySchema, create(CancelJobReplySchema, { disposition }));
}

// Both helpers return the BridgeResult envelope bytes, which Rust decodes.
// rpcResultWithSamples additionally carries the zero-copy `ys` sample buffer.
async function rpcBytes(
  method: WorkerRequestMethod,
  args: Parameters<typeof send>[1]
): Promise<Uint8Array> {
  const reply = await send(method, args);
  return new Uint8Array(reply.result || new ArrayBuffer(0));
}

async function rpcResultWithSamples(method: BridgeMethodId, args: Parameters<typeof send>[1]) {
  const reply = await send(method, args);
  return {
    result: new Uint8Array(reply.result || new ArrayBuffer(0)),
    ys: new Uint8Array(reply.ys || new ArrayBuffer(0)),
  };
}

// Decode a BridgeResult envelope for the DOM glue that needs a string back for
// a GF store or a snuffling reload.
// The error variant rejects the browser operation, mirroring how Rust surfaces
// a BridgeError.
function resultText(envelope: Uint8Array): string {
  const decoded = fromBinary(BridgeResultSchema, envelope);
  if (decoded.result.case === "error") {
    throw new Error(`${decoded.result.value.kind}: ${decoded.result.value.message}`);
  }
  if (decoded.result.case === "text") return decoded.result.value.value;
  throw new Error(`unexpected bridge result: ${decoded.result.case}`);
}

// The rpcs that mean the app is up and the splash can go: whatever put the first
// scene there, plus the example index.
// The example index is the first thing a `?example=` link asks for, and the one
// that reports a slug nobody has.
// Typed against the generated ids, so renaming an rpc is a tsc error rather than
// a splash that never clears.
let isBootOverlayDone = false;
const BOOT_DONE_METHODS: BridgeMethodId[] = [
  BridgeMethod.makeExample,
  BridgeMethod.loadBundle,
  BridgeMethod.loadUrl,
  BridgeMethod.loadBytes,
  BridgeMethod.listExamples,
];

// The single transport seam Rust calls for every unary proto rpc: forward
// (method, request) to the worker and return the BridgeResult envelope bytes plus
// the optional out-of-band zero-copy `ys` sample buffer.
export async function call(method: BridgeMethodId, request: Uint8Array, jobId: bigint) {
  let reply;
  try {
    reply = await rpcResultWithSamples(method, { request, jobId });
  } finally {
    // Complete the loading overlay once the first post-boot work has settled; the
    // boot sequence itself stops at 0.97.
    // In `finally`, because a failed load must reveal the app too: its error is
    // in the status line behind the splash.
    if (!isBootOverlayDone && BOOT_DONE_METHODS.includes(method)) {
      isBootOverlayDone = true;
      reportBootProgress(1.0, "Ready");
    }
  }
  return reply;
}

export function takeOperationEvents(): Uint8Array[] {
  return queuedOperationEvents.splice(0);
}

// Returns the mounted store's dropdown label, or "" when the user cancels the
// picker.
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
  // The handle rides the postMessage payload itself: browser handles cannot be
  // serialized into the protobuf request, and the rpc needs nothing else.
  return resultText(
    await rpcBytes(BridgeMethod.mountGfStore, {
      handle: handle as FileSystemDirectoryHandle,
      jobId,
    })
  );
}

// Reload the Python snuffling code (the snuffler_bridge package) in the worker,
// preserving the loaded pile (snuffler's Shift+R).
// Returns "ok".
export async function reloadSnufflings(jobId: bigint) {
  return resultText(await rpcBytes(WORKER_RELOAD_SNUFFLINGS, { jobId }));
}
