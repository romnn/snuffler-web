// The Pyodide worker: it boots our own Pyodide 314 build (pthread + OpenMP + SIMD;
// py3.14 / emscripten 5.0.3) with every wheel from the locally served py-mt/, and
// relays each bridge request to bridge.py.
// This is the only runtime; the page is always cross-origin isolated (see
// index.html), which SharedArrayBuffer and wasm threads require.
import { create, toBinary } from "@bufbuild/protobuf";
import {
  BootProgressSchema,
  BridgeResultSchema,
  CancelledSchema,
  MlDevice,
  MlDeviceStatusSchema,
  OperationCountProgressSchema,
  OperationEventSchema,
  OperationProgressSchema,
  OperationStartedSchema,
  RuntimeFailureSchema,
  RuntimeInfoSchema,
  StringValueSchema,
  WorkerEventSchema,
  type OperationProgress,
  type WorkerEvent,
} from "./proto/snuffler/bridge/v1/bridge_pb.js";
import {
  beginCommit,
  cancellationRequested,
  executionControl,
  markExecutionDone,
  setExternalWait,
  setInterpreterOwnership,
} from "./execution_control.js";
import { BridgeActivity, BridgeMethod, type ActivityMeta } from "./rpc.js";
import { WORKER_RELOAD_SNUFFLINGS } from "./worker_command.js";
import {
  type WorkerEventMessage,
  type WorkerFailureMessage,
  type WorkerReplyPayload,
  type WorkerRequest,
  type WorkerSuccessMessage,
} from "./worker_protocol.js";

const PYODIDE_BASE = new URL("py-mt/", self.location.href).href;
const epochParameter = Number(new URL(self.location.href).searchParams.get("epoch") || 0);
const RUNTIME_EPOCH =
  Number.isSafeInteger(epochParameter) && epochParameter >= 0 ? epochParameter >>> 0 : 0;

// TextDecoder.decode() throws "The provided ArrayBufferView value must not be
// shared" on a view backed by a SharedArrayBuffer in some engines.
// The threaded Pyodide heap is one, and Pyodide's Python->JS string conversion
// decodes straight off it (loadPackage()'s unpack_buffer, any Python string or
// exception crossing into JS).
// Most decodes do not trip this, so the native decode runs first and only the
// rejected inputs are copied and retried.
// Must run before Pyodide is loaded.
if (typeof SharedArrayBuffer !== "undefined") {
  const proto = TextDecoder.prototype;
  const decode = proto.decode;
  proto.decode = function (input, options) {
    try {
      return decode.call(this, input, options);
    } catch (e) {
      const shared =
        input instanceof SharedArrayBuffer ||
        (ArrayBuffer.isView(input) && input.buffer instanceof SharedArrayBuffer);
      if (!shared || !(e instanceof TypeError)) throw e;
      const bytes =
        input instanceof SharedArrayBuffer
          ? new Uint8Array(input)
          : new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
      const copy = new Uint8Array(bytes.byteLength);
      copy.set(bytes);
      return decode.call(this, copy, options);
    }
  };
}

// py / bridge are Pyodide handles (a runtime + the imported bridge.py module),
// typed `any` — they are dynamic JsProxies with no static types available.
let pyodideReadyPromise: Promise<any> | null = null;
let py: any = null;
let bridge: any = null;
let gfStoreCounter = 0;
let activeSeisbenchJobId = 0n;
// One value, not parallel variables: "a job is active" and "that job excludes
// views" are facts about the same request, and coupling them makes the state
// "blocks views, but no active job" unrepresentable.
let activeJob: { control: Int32Array; blocksView: boolean } | null = null;
let activeViewControl: Int32Array | null = null;
let pyodideInterrupt: Uint8Array | null = null;
type PythonLane = ActivityMeta["lane"];
type MlDeviceInfo = { device: MlDevice; adapter: unknown | null };
type OrtWorkerDevice = "webgpu" | "wasm";
type OrtRequest = {
  type: "infer";
  id: number;
  modelUrl: string;
  input: ArrayBuffer;
  dims: number[];
  label: string;
};
type OrtProgressMessage = {
  type: "progress";
  id: number;
  fraction: number;
  label: string;
};
type OrtResultMessage =
  | {
      type: "result";
      id: number;
      ok: true;
      data: ArrayBuffer;
      dims: number[];
      device: OrtWorkerDevice;
    }
  | {
      type: "result";
      id: number;
      ok: false;
      error: string;
      device: OrtWorkerDevice;
    };
type PendingOrtRequest = {
  resolve: (value: { data: ArrayBuffer; dims: number[]; device: OrtWorkerDevice }) => void;
  reject: (error: Error) => void;
  timer: number | null;
  heartbeatTimer: number | null;
  runTimeoutMs: number;
  sessionReady: boolean;
  setupStartedMs: number | null;
  setupLabel: string;
};
type OrtClient = {
  worker: Worker;
  device: OrtWorkerDevice;
  pending: Map<number, PendingOrtRequest>;
  nextId: number;
};

let mlDeviceInfoPromise: Promise<MlDeviceInfo> | null = null;
let ortWebgpuClient: OrtClient | null = null;
let ortWasmClient: OrtClient | null = null;
const ortWebgpuFailedModels = new Set<string>();
const ORT_WEBGPU_SETUP_TIMEOUT_MS = 120_000;
const ORT_WASM_SETUP_TIMEOUT_MS = 60_000;
const ORT_RUN_TIMEOUT_MS = 180_000;
const nativeFetch = self.fetch.bind(self) as typeof fetch;
const prefetchedResponses = new Map<string, Uint8Array>();
// Requests in flight while prefetching the runtime + wheels. Six matches what
// browsers allow per host over HTTP/1.1, and keeps a high-latency link busy
// instead of paying one round trip per wheel.
const PREFETCH_CONCURRENCY = 6;
const PYODIDE_PACKAGES = [
  "numpy",
  "scipy",
  "matplotlib",
  "pyyaml",
  "micropip",
  "protobuf",
  "requests",
  "pyodide-http",
  "pyrocko",
];

function resolveUrl(path) {
  return new URL(path, self.location.href).href;
}

function normalizeFetchUrl(input: RequestInfo | URL): string {
  if (typeof input === "string") return new URL(input, self.location.href).href;
  if (input instanceof URL) return input.href;
  return new URL(input.url, self.location.href).href;
}

function responseHeadersFor(url: string, bytes: Uint8Array): Headers {
  const headers = new Headers();
  headers.set("content-length", String(bytes.byteLength));
  if (url.endsWith(".wasm")) headers.set("content-type", "application/wasm");
  else if (url.endsWith(".json")) headers.set("content-type", "application/json");
  else if (url.endsWith(".mjs") || url.endsWith(".js")) {
    headers.set("content-type", "text/javascript");
  } else if (url.endsWith(".zip")) headers.set("content-type", "application/zip");
  else headers.set("content-type", "application/octet-stream");
  return headers;
}

function postWorkerEvent(event: WorkerEvent["event"]) {
  const bytes = toBinary(
    WorkerEventSchema,
    create(WorkerEventSchema, { event, runtimeEpoch: RUNTIME_EPOCH })
  );
  const message: WorkerEventMessage = { type: "workerEvent", epoch: RUNTIME_EPOCH, event: bytes };
  self.postMessage(message, [bytes.buffer]);
}

function postMlDevice(device: MlDevice) {
  postWorkerEvent({
    case: "mlDevice",
    value: create(MlDeviceStatusSchema, { device }),
  });
}

function postBootProgress(fraction: number, label: string) {
  postWorkerEvent({
    case: "boot",
    value: create(BootProgressSchema, { fraction, label }),
  });
}

function postSeisbenchProgress(fraction: number, label: string) {
  if (activeSeisbenchJobId === 0n) return;
  postOperationProgress(activeSeisbenchJobId, String(label || "Running SeisBench picker"), {
    case: "fraction",
    value: Math.max(0, Math.min(1, Number(fraction))),
  });
}

function postCountProgress(jobId: bigint, done: number, total: number | null, phase: string) {
  postOperationProgress(jobId, phase, {
    case: "count",
    value: create(OperationCountProgressSchema, {
      done: BigInt(Math.max(0, Math.trunc(done))),
      total: total == null ? undefined : BigInt(Math.max(0, Math.trunc(total))),
    }),
  });
}

function postOperationProgress(
  jobId: bigint,
  phase: string,
  progress: OperationProgress["progress"]
) {
  if (jobId === 0n) return;
  postWorkerEvent({
    case: "operation",
    value: create(OperationEventSchema, {
      jobId,
      event: {
        case: "progress",
        value: create(OperationProgressSchema, { phase, progress }),
      },
    }),
  });
}

function postOperationStarted(jobId: bigint) {
  postWorkerEvent({
    case: "operation",
    value: create(OperationEventSchema, {
      jobId,
      event: { case: "started", value: create(OperationStartedSchema) },
    }),
  });
}

function postRuntimeFailure(error: unknown) {
  const value = error instanceof Error ? error : new Error(String(error));
  postWorkerEvent({
    case: "runtimeFailure",
    value: create(RuntimeFailureSchema, {
      kind: value.name || "Error",
      message: value.message || "Python runtime boot failed",
    }),
  });
}

function postRuntimeInfo() {
  const threaded = Boolean(
    py.runPython("import sys; bool(getattr(sys._emscripten_info, 'pthreads', False))")
  );
  postWorkerEvent({
    case: "runtimeInfo",
    value: create(RuntimeInfoSchema, {
      threaded,
      python: String(py.runPython("import sys; sys.version.split()[0]")),
      numpy: String(py.runPython("import numpy; numpy.__version__")),
      pyrocko: String(py.runPython("import pyrocko; getattr(pyrocko, '__version__', 'unknown')")),
      cores: (self.navigator && self.navigator.hardwareConcurrency) || 0,
    }),
  });
}

function acquireInterpreter(
  msg: WorkerRequest,
  control: Int32Array,
  interrupt: Uint8Array
): PythonLane {
  const activity = msg.method === WORKER_RELOAD_SNUFFLINGS ? null : BridgeActivity[msg.method];
  if (msg.method !== WORKER_RELOAD_SNUFFLINGS && !activity) {
    throw new Error(`protocol fault: unknown Python method ${String(msg.method)}`);
  }
  const lane = activity?.lane ?? "JOB";
  if (lane === "VIEW") {
    if (activeViewControl) {
      throw new Error("protocol fault: a second view request reached the Python worker");
    }
    if (activeJob?.blocksView) {
      throw new Error("protocol fault: a view request overlapped an exclusive Python job");
    }
    activeViewControl = control;
    if (activeJob) {
      setInterpreterOwnership(activeJob.control, false, interrupt);
      // The job keeps its cancellation state while the view owns the interpreter.
      // Its interrupt is re-armed when the view releases the interpreter.
    }
  } else {
    if (activeJob) {
      throw new Error("protocol fault: a second job request reached the Python worker");
    }
    const blocksView = msg.method === WORKER_RELOAD_SNUFFLINGS;
    if (blocksView && activeViewControl) {
      throw new Error("protocol fault: an exclusive Python job overlapped a view request");
    }
    activeJob = { control, blocksView };
  }
  // Interpreter ownership is NOT granted here. The request's control word stays
  // ...WithoutInterpreter — where cancellation cannot arm SIGINT — until Python
  // itself calls enterProtectedFrame from bridge.call's first protected
  // bytecodes, so no interrupt can be consumed in the coroutine-scheduling
  // plumbing between this point and the target frame.
  return lane;
}

function releaseInterpreter(lane: PythonLane, control: Int32Array, interrupt: Uint8Array): void {
  let jobWaitingForInterpreter: Int32Array | null = null;
  if (lane === "VIEW" && activeViewControl === control) {
    activeViewControl = null;
    jobWaitingForInterpreter = activeJob?.control ?? null;
  } else if (lane === "JOB" && activeJob?.control === control) {
    activeJob = null;
  }
  markExecutionDone(control, interrupt);
  if (jobWaitingForInterpreter) {
    setInterpreterOwnership(jobWaitingForInterpreter, true, interrupt);
  }
}

function operationCallbacks(
  msg: WorkerRequest,
  lane: PythonLane,
  control: Int32Array,
  interrupt: Uint8Array
) {
  const reporter = (done: number, total: number, phase: string): boolean => {
    if (cancellationRequested(control)) return false;
    postCountProgress(BigInt(msg.jobId || 0), done, total < 0 ? null : total, phase);
    return true;
  };
  const beginRequestCommit = (): boolean => {
    return beginCommit(control, interrupt);
  };
  const beginExternalWait = (): void => {
    setExternalWait(control, true, false, interrupt);
  };
  // "Does this request own the interpreter" is lane-dependent: a view is the
  // owner exactly while it is the active view, a job exactly while no view
  // displaces it. The job-only predicate would read inverted for a view.
  const ownsInterpreter = (): boolean =>
    lane === "VIEW" ? activeViewControl === control : activeViewControl == null;
  const endExternalWait = (): void => {
    setExternalWait(control, false, ownsInterpreter(), interrupt);
  };
  // Called by bridge.call from its first protected bytecodes; grants ownership
  // (arming the byte if cancellation is already pending) only once the target
  // frame is provably the one executing. See acquireInterpreter.
  const enterProtectedFrame = (): void => {
    setInterpreterOwnership(control, ownsInterpreter(), interrupt);
  };
  return { reporter, beginRequestCommit, beginExternalWait, endExternalWait, enterProtectedFrame };
}

// The typed cancelled envelope, produced worker-side for a request whose
// cancellation is already decided before (or instead of) Python answering: a
// cancel that won while the request was queued, or a KeyboardInterrupt that was
// consumed outside bridge.call's protected frames.
function bridgeCancelledResult(): WorkerReplyPayload {
  const result = toBinary(
    BridgeResultSchema,
    create(BridgeResultSchema, {
      result: { case: "cancelled", value: create(CancelledSchema) },
    })
  );
  const resultBuffer = result.buffer as ArrayBuffer;
  return { result: resultBuffer, transfer: [resultBuffer] };
}

// True when an error thrown by a bridge.call invocation is this request's own
// cancellation surfacing outside the protected frames (Pyodide raises it as a
// PythonError whose `type` names the Python exception class). Gated on the
// control word so an unrelated Python failure that merely mentions
// KeyboardInterrupt can never be misread as a cancellation.
function isEscapedCancellation(control: Int32Array, error: unknown): boolean {
  if (!cancellationRequested(control)) return false;
  const type = (error as { type?: unknown } | null)?.type;
  return type === "KeyboardInterrupt";
}

function responseContentLength(response: Response): number {
  const value = Number(response.headers.get("content-length") || 0);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

function progressFraction(start: number, end: number, loaded: number, total: number): number {
  if (total <= 0) return start;
  return start + (end - start) * Math.max(0, Math.min(1, loaded / total));
}

// Serve loadPyodide()/loadPackage()'s own fetches out of the batch we downloaded
// ahead of them (see startPrefetch); anything they ask for that we did not
// prefetch falls through to the network unchanged.
(self as any).fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = normalizeFetchUrl(input);
  const prefetched = prefetchedResponses.get(url);
  if (!prefetched) return await nativeFetch(input, init);
  prefetchedResponses.delete(url);
  return new Response(prefetched.slice(), {
    status: 200,
    headers: responseHeadersFor(url, prefetched),
  });
};

async function fetchContentLength(url: string): Promise<number> {
  try {
    const response = await nativeFetch(url, { method: "HEAD" });
    return response.ok ? responseContentLength(response) : 0;
  } catch (_e) {
    return 0;
  }
}

async function fetchBytes(url: string, loaded: (bytes: number) => void): Promise<Uint8Array> {
  const response = await nativeFetch(url);
  if (!response.ok) throw new Error(`fetch ${url}: ${response.status} ${response.statusText}`);
  if (!response.body) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    loaded(bytes.byteLength);
    return bytes;
  }

  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (!value) continue;
    chunks.push(value);
    byteLength += value.byteLength;
    loaded(value.byteLength);
  }

  const bytes = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

// The boot phase the progress bar is labelled with. The download meter below runs
// across several phases (the runtime starts while the wheels are still coming in),
// so the fraction it posts is byte-driven while the label tracks whatever boot() is
// actually waiting on.
//
// The spans each phase gets are its share of a real first visit, measured at
// 25 Mbit/s with a cold cache (~20 s end to end).
// The app wasm and the Python assets download concurrently (index.html starts
// this worker at page parse) and the two meters cannot see each other's bytes,
// so the bar shows whichever is further along: the app's span is 0 -> 0.44
// (driven by the Trunk loader; see scripts/patch-trunk-loader.mjs), then
// downloads -> 0.84, installing -> 0.92, importing -> 0.96, first scene the rest.
// The long phases are metered by bytes and modules, so the bar tracks work done.
let bootPhase = "Starting…";
let bootFraction = 0;

function setBootPhase(label: string) {
  bootPhase = label;
  postBootProgress(bootFraction, label);
}

// Roughly how many modules `import bridge` pulls in, used only to shape the bar
// across the import phase. The count is measured, not derived, so it drifts as
// pyrocko/numpy change — progressFraction clamps at the end of the span, and the
// final report jumps the bar there regardless, so drift costs smoothness, never
// correctness.
const BOOT_MODULE_ESTIMATE = 700;

// Called from Python as modules come in (see bootstrap.import_bridge).
function bootModuleProgress(count: number) {
  reportBootProgress(
    progressFraction(0.92, 0.96, count, BOOT_MODULE_ESTIMATE),
    count >= BOOT_MODULE_ESTIMATE ? "Importing pyrocko…" : `Importing pyrocko… (${count} modules)`
  );
}

// Download `urls` into `prefetchedResponses` with `PREFETCH_CONCURRENCY` requests
// in flight, metering all of them as one byte-accurate progress span.
//
// `priority` is a prefix of `urls` that boot() needs before the rest (the Pyodide
// runtime, so the interpreter can start while the wheels are still downloading);
// it is fetched first and awaited separately.
function startPrefetch(
  priority: string[],
  rest: string[],
  start: number,
  end: number
): { priority: Promise<void>; all: Promise<void> } {
  // A Set keeps first-insertion order, so the priority urls stay at the front of
  // the queue the workers below pull from.
  const urls = Array.from(new Set([...priority, ...rest]));
  const settled = new Map<string, { resolve: () => void; reject: (error: unknown) => void }>();
  const ready = priority.map(
    (url) =>
      new Promise<void>((resolve, reject) => {
        settled.set(url, { resolve, reject });
      })
  );

  let loaded = 0;
  let total = 0;
  const report = () => {
    if (total <= 0) return;
    bootFraction = Math.max(bootFraction, progressFraction(start, end, loaded, total));
    postBootProgress(bootFraction, bootPhase);
  };

  const all = (async () => {
    const lengths = await Promise.all(urls.map(fetchContentLength));
    total = lengths.reduce((sum, value) => sum + value, 0);
    let next = 0;
    const drain = async () => {
      for (;;) {
        const index = next++;
        if (index >= urls.length) return;
        const url = urls[index];
        try {
          prefetchedResponses.set(
            url,
            await fetchBytes(url, (n) => {
              loaded += n;
              report();
            })
          );
          settled.get(url)?.resolve();
        } catch (e) {
          settled.get(url)?.reject(e);
          throw e;
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(PREFETCH_CONCURRENCY, urls.length) }, drain));
    bootFraction = Math.max(bootFraction, end);
    postBootProgress(bootFraction, bootPhase);
  })();

  // boot() awaits `priority` first and `all` later; without this sink a failure
  // that surfaces through `priority` would also land as an unhandled rejection.
  void all.catch(() => {});
  return { priority: Promise.all(ready).then(() => undefined), all };
}

function pyodidePackageName(name: string): string {
  return name.toLowerCase().replace(/_/g, "-");
}

function pyodidePackageClosure(lock: any): string[] {
  const packages = lock.packages || {};
  const seen = new Set<string>();
  const out: string[] = [];
  const visit = (name: string) => {
    const normalized = pyodidePackageName(name);
    if (seen.has(normalized)) return;
    const entry = packages[normalized];
    if (!entry) throw new Error(`pyodide package ${name} is missing from pyodide-lock.json`);
    seen.add(normalized);
    out.push(normalized);
    for (const dep of entry.depends || []) visit(String(dep));
  };
  for (const name of PYODIDE_PACKAGES) visit(name);
  return out;
}

// Start every download the boot needs — runtime first, then the wheel closure —
// as ONE metered, concurrent batch. The wheels are not needed until loadPackage(),
// so downloading them alongside the runtime start overlaps ~30 MB of network with
// the interpreter's own (CPU-bound) startup instead of queueing behind it.
async function startPyodidePrefetch(): Promise<{
  priority: Promise<void>;
  all: Promise<void>;
  packages: Array<{ name: string; url: string }>;
}> {
  // loadPyodide() and loadPackage() both read the lock again; keep the bytes so
  // they are answered from the prefetch cache rather than a second round trip.
  const lockUrl = resolveUrl("py-mt/pyodide-lock.json");
  const lockBytes = new Uint8Array(await (await fetchOk(lockUrl)).arrayBuffer());
  prefetchedResponses.set(lockUrl, lockBytes);
  const lock = JSON.parse(new TextDecoder().decode(lockBytes));
  const entries = lock.packages || {};
  const packages = pyodidePackageClosure(lock).map((name) => ({
    name,
    url: resolveUrl(`py-mt/${entries[name].file_name}`),
  }));
  return {
    packages,
    ...startPrefetch(
      [
        resolveUrl("py-mt/pyodide.asm.mjs"),
        resolveUrl("py-mt/pyodide.asm.wasm"),
        resolveUrl("py-mt/python_stdlib.zip"),
      ],
      packages.map((pkg) => pkg.url),
      0.44,
      0.84
    ),
  };
}

// The generated proto modules + bridge.py, staged into the Pyodide FS once Python
// is up. They are plain static files with no dependency on the runtime, so fetch
// them in parallel with it rather than one at a time after it.
async function fetchBridgeSources(): Promise<Array<[string, string]>> {
  const protoDir = "proto/snuffler/bridge/v1";
  const files = [
    ...[
      "activity_pb2.py",
      "common_pb2.py",
      "scene_pb2.py",
      "view_pb2.py",
      "snuffling_pb2.py",
      "figure_pb2.py",
      "data_pb2.py",
      "bridge_pb2.py",
      // Generated typed servicer Protocol + dispatch (from BridgeService);
      // bridge.py imports it, so it must be staged alongside the *_pb2 modules.
      "bridge_dispatch.py",
    ].map((file): [string, string] => [`/snuffler/bridge/v1/${file}`, `${protoDir}/${file}`]),
    // Helper module bridge.py imports lazily for SeisBench pre/post-processing.
    ["/seisbench_infer.py", "seisbench_infer.py"] as [string, string],
    // Imported BEFORE bridge.py: it decides what pyrocko's own imports defer.
    ["/bootstrap.py", "bootstrap.py"] as [string, string],
    ["/bridge.py", "bridge.py"] as [string, string],
  ];
  return await Promise.all(
    files.map(
      async ([dest, path]): Promise<[string, string]> => [
        dest,
        await (await fetchOk(resolveUrl(path))).text(),
      ]
    )
  );
}

async function mlDeviceInfo(): Promise<MlDeviceInfo> {
  if (!mlDeviceInfoPromise) {
    mlDeviceInfoPromise = (async () => {
      const nav = self.navigator as Navigator & {
        gpu?: { requestAdapter: () => Promise<unknown> };
      };
      if (!nav.gpu) return { device: MlDevice.WASM, adapter: null };
      try {
        const adapter = await nav.gpu.requestAdapter();
        return adapter
          ? { device: MlDevice.WEBGPU, adapter }
          : { device: MlDevice.WASM, adapter: null };
      } catch (_e) {
        return { device: MlDevice.WASM, adapter: null };
      }
    })();
  }
  return mlDeviceInfoPromise;
}

async function mlDevice(): Promise<MlDevice> {
  return (await mlDeviceInfo()).device;
}

function copyFloat32Array(value: any): Float32Array {
  if (value instanceof Float32Array) {
    const out = new Float32Array(value.length);
    out.set(value);
    return out;
  }
  if (value && typeof value.getBuffer === "function") {
    const buf = value.getBuffer();
    try {
      const src = buf.data;
      const view =
        src instanceof Float32Array
          ? src
          : new Float32Array(
              src.buffer,
              src.byteOffset,
              src.byteLength / Float32Array.BYTES_PER_ELEMENT
            );
      const out = new Float32Array(view.length);
      out.set(view);
      return out;
    } finally {
      buf.release();
    }
  }
  if (ArrayBuffer.isView(value)) {
    const src = new Float32Array(
      value.buffer,
      value.byteOffset,
      value.byteLength / Float32Array.BYTES_PER_ELEMENT
    );
    const out = new Float32Array(src.length);
    out.set(src);
    return out;
  }
  if (Array.isArray(value)) return new Float32Array(value);
  throw new Error("expected Float32Array-compatible ORT input");
}

function copyIntegerArray(value: any): number[] {
  if (Array.isArray(value)) return value.map((x) => Number(x));
  if (value && typeof value.toJs === "function") return Array.from(value.toJs(), (x) => Number(x));
  return Array.from(value || [], (x) => Number(x));
}

function progressLabel(label?: string): string {
  return String(label || "SeisBench picker");
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function createOrtClient(device: OrtWorkerDevice): OrtClient {
  const worker = new Worker(`${resolveUrl("ort_worker.js")}?device=${device}`, { type: "module" });
  const client: OrtClient = {
    worker,
    device,
    pending: new Map(),
    nextId: 1,
  };
  worker.onmessage = (event) => {
    const msg = event.data as OrtProgressMessage | OrtResultMessage;
    if (!msg) return;
    if (msg.type === "progress") {
      const slot = client.pending.get(msg.id);
      if (slot && msg.fraction >= 0.2 && !slot.sessionReady) {
        slot.sessionReady = true;
        stopOrtSetupHeartbeat(slot);
        armOrtTimeout(client, msg.id, slot.runTimeoutMs);
      } else if (slot && msg.fraction >= 0.18 && !slot.sessionReady) {
        armOrtSetupHeartbeat(client, msg.id, msg.label);
      }
      postSeisbenchProgress(msg.fraction, msg.label);
      return;
    }
    if (msg.type !== "result") return;
    const slot = client.pending.get(msg.id);
    if (!slot) return;
    client.pending.delete(msg.id);
    clearOrtTimers(slot);
    if (msg.ok) {
      slot.resolve({ data: msg.data, dims: msg.dims, device: msg.device });
    } else {
      slot.reject(new Error(msg.error || `${device} inference failed`));
    }
  };
  worker.onerror = (event) => {
    const error = new Error(event.message || `${device} inference worker crashed`);
    rejectOrtClient(client, error);
  };
  return client;
}

function ortClient(device: OrtWorkerDevice): OrtClient {
  if (device === "webgpu") {
    ortWebgpuClient ??= createOrtClient(device);
    return ortWebgpuClient;
  }
  ortWasmClient ??= createOrtClient(device);
  return ortWasmClient;
}

function rejectOrtClient(client: OrtClient, error: Error) {
  for (const slot of client.pending.values()) {
    clearOrtTimers(slot);
    slot.reject(error);
  }
  client.pending.clear();
}

function terminateOrtClient(client: OrtClient, reason: Error) {
  rejectOrtClient(client, reason);
  client.worker.terminate();
  if (client.device === "webgpu" && ortWebgpuClient === client) ortWebgpuClient = null;
  if (client.device === "wasm" && ortWasmClient === client) ortWasmClient = null;
}

function armOrtTimeout(client: OrtClient, id: number, timeoutMs: number) {
  const slot = client.pending.get(id);
  if (!slot) return;
  if (slot.timer != null) clearTimeout(slot.timer);
  if (timeoutMs <= 0) {
    slot.timer = null;
    return;
  }
  slot.timer = self.setTimeout(() => {
    const current = client.pending.get(id);
    if (!current) return;
    const phase = current.sessionReady ? "run" : "setup";
    terminateOrtClient(client, new Error(`${client.device} inference ${phase} timed out`));
  }, timeoutMs);
}

function clearOrtTimers(slot: PendingOrtRequest) {
  if (slot.timer != null) {
    clearTimeout(slot.timer);
    slot.timer = null;
  }
  stopOrtSetupHeartbeat(slot);
}

function stopOrtSetupHeartbeat(slot: PendingOrtRequest) {
  if (slot.heartbeatTimer != null) {
    clearInterval(slot.heartbeatTimer);
    slot.heartbeatTimer = null;
  }
}

function armOrtSetupHeartbeat(client: OrtClient, id: number, label: string) {
  const slot = client.pending.get(id);
  if (!slot || slot.sessionReady) return;
  slot.setupLabel = label;
  slot.setupStartedMs ??= performance.now();
  if (slot.heartbeatTimer != null) return;
  slot.heartbeatTimer = self.setInterval(() => {
    const current = client.pending.get(id);
    if (!current || current.sessionReady || current.setupStartedMs == null) {
      if (current) stopOrtSetupHeartbeat(current);
      return;
    }
    const elapsedS = Math.max(1, Math.round((performance.now() - current.setupStartedMs) / 1000));
    const fraction = Math.min(0.199, 0.18 + 0.019 * (1 - Math.exp(-elapsedS / 30)));
    postSeisbenchProgress(fraction, `${current.setupLabel} (${elapsedS}s)`);
  }, 1_000);
}

function sendOrtInfer(
  device: OrtWorkerDevice,
  modelUrl: string,
  inputData: Float32Array,
  inputDims: number[],
  label: string,
  setupTimeoutMs: number,
  runTimeoutMs: number
): Promise<{ data: ArrayBuffer; dims: number[]; device: OrtWorkerDevice }> {
  const client = ortClient(device);
  const id = client.nextId++;
  const input = new ArrayBuffer(inputData.byteLength);
  new Float32Array(input).set(inputData);
  return new Promise((resolve, reject) => {
    client.pending.set(id, {
      resolve,
      reject,
      timer: null,
      heartbeatTimer: null,
      runTimeoutMs,
      sessionReady: false,
      setupStartedMs: null,
      setupLabel: `${label} · creating inference session`,
    });
    armOrtTimeout(client, id, setupTimeoutMs);
    const request: OrtRequest = {
      type: "infer",
      id,
      modelUrl,
      input,
      dims: inputDims,
      label,
    };
    client.worker.postMessage(request, [input]);
  });
}

async function inferWithFallback(
  modelUrl: string,
  inputData: Float32Array,
  inputDims: number[],
  label: string
): Promise<{ data: Float32Array; dims: number[] }> {
  const info = await mlDeviceInfo();
  const normalizedModelUrl = new URL(modelUrl, self.location.href).href;
  const preferWebgpu =
    info.device === MlDevice.WEBGPU && !ortWebgpuFailedModels.has(normalizedModelUrl);
  let result: { data: ArrayBuffer; dims: number[]; device: OrtWorkerDevice };
  if (preferWebgpu) {
    try {
      result = await sendOrtInfer(
        "webgpu",
        modelUrl,
        inputData,
        inputDims,
        label,
        ORT_WEBGPU_SETUP_TIMEOUT_MS,
        ORT_RUN_TIMEOUT_MS
      );
    } catch (e) {
      ortWebgpuFailedModels.add(normalizedModelUrl);
      postMlDevice(MlDevice.WASM);
      const message = errorMessage(e);
      console.warn(`[bridge-worker] WebGPU inference unavailable, using CPU wasm: ${message}`);
      postSeisbenchProgress(0.18, `${label} · WebGPU unavailable; using CPU (wasm)`);
      result = await sendOrtInfer(
        "wasm",
        modelUrl,
        inputData,
        inputDims,
        label,
        ORT_WASM_SETUP_TIMEOUT_MS,
        ORT_RUN_TIMEOUT_MS
      );
    }
  } else {
    result = await sendOrtInfer(
      "wasm",
      modelUrl,
      inputData,
      inputDims,
      label,
      ORT_WASM_SETUP_TIMEOUT_MS,
      ORT_RUN_TIMEOUT_MS
    );
  }
  if (result.device === "wasm") postMlDevice(MlDevice.WASM);
  return { data: new Float32Array(result.data), dims: result.dims };
}

async function snufflerOrtInfer(modelUrl: string, input: any, dims: any, label?: string) {
  const inputData = copyFloat32Array(input);
  const inputDims = copyIntegerArray(dims);
  return await inferWithFallback(modelUrl, inputData, inputDims, progressLabel(label));
}

(globalThis as any).__snufflerOrtInfer = snufflerOrtInfer;
(globalThis as any).__snufflerYieldToMacrotask = (() => {
  const channel = new MessageChannel();
  const pending: Array<() => void> = [];
  channel.port1.onmessage = () => pending.shift()?.();
  return () =>
    new Promise<void>((resolve) => {
      pending.push(resolve);
      channel.port2.postMessage(0);
    });
})();
(globalThis as any).__snufflerSeisbenchProgress = (frac: number, label: string) => {
  postSeisbenchProgress(frac, label);
};
async function fetchOk(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${url}: ${r.status} ${r.statusText}`);
  return r;
}

function reportBootProgress(fraction, label) {
  bootFraction = Math.max(bootFraction, fraction);
  bootPhase = label;
  postBootProgress(bootFraction, label);
}

function destroyPyProxy(value) {
  if (value && typeof value.destroy === "function") value.destroy();
}

// Copy a Python list of strings into JS and release the proxy behind it.
function takePythonStringList(value): string[] {
  try {
    return value.toJs() as string[];
  } finally {
    destroyPyProxy(value);
  }
}

function copyBytes(value) {
  const src = value instanceof Uint8Array ? value : new Uint8Array(value.buffer || value);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

// Copies bytes into JS-owned memory and destroys any Python proxy wrapper.
function takePythonBytes(value) {
  try {
    if (value instanceof Uint8Array) return copyBytes(value);
    if (value && typeof value.toJs === "function") return copyBytes(value.toJs());
    return copyBytes(value);
  } finally {
    destroyPyProxy(value);
  }
}

async function boot() {
  if (pyodideReadyPromise) return pyodideReadyPromise;
  pyodideReadyPromise = (async () => {
    try {
      const t0 = performance.now();
      setBootPhase("Downloading Python runtime…");

      // Everything the boot downloads starts here, concurrently: the runtime, the
      // wheel closure, and our own Python sources. Only the runtime is awaited
      // before the interpreter starts; the rest lands while it is coming up.
      const sources = fetchBridgeSources();
      void sources.catch(() => {});
      const prefetch = await startPyodidePrefetch();
      await prefetch.priority;

      // Pyodide 314 rejects classic workers, so this is the ESM build, imported
      // from the only base there is: our locally served py-mt/.
      setBootPhase("Starting Python runtime…");
      const { loadPyodide } = await import(PYODIDE_BASE + "pyodide.mjs");
      py = await loadPyodide({ indexURL: PYODIDE_BASE });
      const tRuntime = performance.now();

      setBootPhase("Downloading Python packages…");
      await prefetch.all;
      setBootPhase("Installing Python packages…");
      // Everything (incl. pyrocko, protobuf, requests, pyodide-http) is in
      // py-mt/pyodide-lock.json, built for this runtime — load by name. Installed
      // one at a time so the bar advances per package (~60 ms slower than handing
      // loadPackage the whole list at once, measured); the wheels are already in
      // memory by now, so this is unpack + dlopen with no network left to overlap.
      //
      // Progress is weighted by wheel size, not package count: unpacking costs
      // ~50 ms/MB, so scipy alone is over half this phase while 19 of the 23
      // packages together are under 5% of it.
      const installed = prefetch.packages.map((pkg) => ({
        name: pkg.name,
        bytes: prefetchedResponses.get(pkg.url)?.byteLength ?? 0,
      }));
      const installTotal = installed.reduce((sum, pkg) => sum + pkg.bytes, 0);
      let installDone = 0;
      for (const pkg of installed) {
        await py.loadPackage(pkg.name);
        installDone += pkg.bytes;
        reportBootProgress(
          progressFraction(0.84, 0.92, installDone, installTotal),
          "Installing Python packages…"
        );
      }
      prefetchedResponses.clear();
      const tPackages = performance.now();

      reportBootProgress(0.92, "Importing pyrocko…");
      for (const dir of ["/snuffler", "/snuffler/bridge", "/snuffler/bridge/v1"]) {
        try {
          py.FS.mkdir(dir);
        } catch (_e) {
          // Already exists.
        }
      }
      for (const [dest, src] of await sources) py.FS.writeFile(dest, src);
      // bootstrap.py defers the scipy imports pyrocko does at module scope but only
      // uses inside functions, then imports the bridge while counting modules so the
      // longest phase of the boot has a moving bar instead of a frozen one.
      py.runPython('import sys; sys.path.insert(0, "/"); import bootstrap');
      const deferred = takePythonStringList(py.runPython("bootstrap.defer()"));
      const importBridge = py.runPython("bootstrap.import_bridge");
      try {
        bridge = importBridge(bootModuleProgress);
      } finally {
        importBridge.destroy();
      }
      const tImport = performance.now();

      // Boot-phase timings, logged once so future regressions are visible in the
      // console. runtime = download + start the Pyodide runtime; packages =
      // loadPackage (numpy/scipy/matplotlib/pyrocko/…); import = stage protos +
      // `import bridge` (which imports pyrocko). On this MT build the py-mt runtime
      // also stands up libomp + pthread workers, so these spans include that spin-up.
      const ms = (a, b) => Math.round(b - a);
      console.log(
        `[bridge-worker] boot timing (ms): runtime=${ms(t0, tRuntime)} ` +
          `packages=${ms(tRuntime, tPackages)} import=${ms(tPackages, tImport)} ` +
          `total=${ms(t0, tImport)} deferred=${deferred.join(",") || "none"}`
      );

      // Report the booted runtime so the UI can show a truthful debug badge.
      // pthreads is read back from the live interpreter to confirm threading
      // really came up on our py-mt build (the badge then reads "wasm+openmp+simd").
      try {
        postRuntimeInfo();
      } catch (_e) {
        // Non-fatal: the badge just won't show.
      }
      try {
        postMlDevice(await mlDevice());
      } catch (_e) {
        postMlDevice(MlDevice.WASM);
      }

      reportBootProgress(0.97, "Almost ready…");
      return py;
    } catch (e) {
      console.error("[bridge-worker] boot failed:", e);
      throw e;
    }
  })();
  return pyodideReadyPromise;
}

// Shape the {"result": BridgeResult bytes, "buffer"?: ndarray} dict that
// bridge.call() returns into the worker's postMessage payload.
// The zero-copy ys buffer is transferred only when present (getView).
function bridgeResult(res): WorkerReplyPayload {
  try {
    const result = takePythonBytes(res.get("result"));
    const resultBuffer = result.buffer as ArrayBuffer;
    const out: WorkerReplyPayload = { result: resultBuffer, transfer: [resultBuffer] };
    if (res.has("buffer")) {
      const ysProxy = res.get("buffer");
      let buf: any = null;
      try {
        buf = ysProxy.getBuffer();
        const d = buf.data;
        const ys = copyBytes(
          d instanceof Uint8Array ? d : new Uint8Array(d.buffer, d.byteOffset, d.byteLength)
        );
        out.ys = ys.buffer;
        out.transfer.push(ys.buffer);
      } finally {
        if (buf) buf.release();
        destroyPyProxy(ysProxy);
      }
    }
    return out;
  } finally {
    destroyPyProxy(res);
  }
}

function bridgeTextResult(value: string): WorkerReplyPayload {
  const result = toBinary(
    BridgeResultSchema,
    create(BridgeResultSchema, {
      result: { case: "text", value: create(StringValueSchema, { value }) },
    })
  );
  const resultBuffer = result.buffer as ArrayBuffer;
  return { result: resultBuffer, transfer: [resultBuffer] };
}

// The worker is a relay: it forwards the generated method id, the request bytes
// and optional out-of-band data to bridge.py's async dispatch, which runs the rpc
// and returns the BridgeResult envelope.
// MountGfStore's directory handle rides beside the protobuf request because
// browser handles cannot be serialized into it.
async function handleRequest(msg: WorkerRequest): Promise<WorkerReplyPayload> {
  await boot();
  if (!pyodideInterrupt) {
    pyodideInterrupt = new Uint8Array(msg.interrupt as SharedArrayBuffer);
    py.setInterruptBuffer(pyodideInterrupt);
  }
  const interrupt = pyodideInterrupt;
  if (!interrupt) throw new Error("Pyodide interrupt buffer is unavailable");
  const control = executionControl(msg.control as SharedArrayBuffer);
  const lane = acquireInterpreter(msg, control, interrupt);
  const jobId = BigInt(msg.jobId || 0);
  if (msg.method === BridgeMethod.runSnuffling) activeSeisbenchJobId = jobId;
  postOperationStarted(jobId);
  const { reporter, beginRequestCommit, beginExternalWait, endExternalWait, enterProtectedFrame } =
    operationCallbacks(msg, lane, control, interrupt);
  // The single seam into bridge.py for this request, shared by the GF-store mount
  // and the plain rpc so both pass the same callbacks and cancellation backstop.
  const callPython = async (data: string | null): Promise<WorkerReplyPayload> => {
    let res;
    try {
      res = await bridge.call(
        msg.method,
        msg.request,
        data,
        reporter,
        beginRequestCommit,
        beginExternalWait,
        endExternalWait,
        enterProtectedFrame
      );
    } catch (error) {
      if (isEscapedCancellation(control, error)) return bridgeCancelledResult();
      throw error;
    }
    return bridgeResult(res);
  };
  try {
    // A request cancelled while it was queued never reaches Python: dispatching
    // it would need SIGINT armed before its first protected frame, where the
    // interrupt could land in coroutine-scheduling plumbing instead.
    if (cancellationRequested(control)) {
      return bridgeCancelledResult();
    }
    if (msg.method === WORKER_RELOAD_SNUFFLINGS) {
      // Re-fetch the Python sources and importlib.reload them, keeping the session
      // (snuffler's Shift+R reloads code, not data).
      // What survives is listed in bridge.py's `_RELOAD_KEEP`.
      for (const file of ["seisbench_infer.py", "bridge.py"]) {
        py.FS.writeFile(`/${file}`, await (await fetchOk(resolveUrl(file))).text());
      }
      py.runPython(`
import importlib, sys, bridge
if 'seisbench_infer' in sys.modules:
    importlib.reload(sys.modules['seisbench_infer'])
_saved = bridge._reload_snapshot()
importlib.invalidate_caches()
try:
    importlib.reload(bridge)
finally:
    # The snapshot detached the session state from the module dict, so restore
    # must run even when the reload raised.
    bridge._reload_restore(_saved)
`);
      return bridgeTextResult("ok");
    }
    if (msg.method === BridgeMethod.mountGfStore) {
      if (!msg.handle) {
        throw new Error("mounting a GF store requires a directory handle");
      }
      gfStoreCounter += 1;
      const mount = "/gfstores/" + gfStoreCounter;
      try {
        py.FS.mkdir("/gfstores");
      } catch (_e) {
        // Already exists.
      }
      py.FS.mkdir(mount);
      await py.mountNativeFS(mount, msg.handle);
      return await callPython(mount);
    }
    return await callPython(null);
  } finally {
    releaseInterpreter(lane, control, interrupt);
  }
}

// Booting Python is this worker's whole purpose and takes seconds, so start it on
// module load rather than waiting for the first rpc: the page creates the worker
// while the app wasm is still downloading (see index.html), and that head start is
// the entire Pyodide download. handleRequest awaits the same memoized promise, so
// a request that arrives mid-boot still queues behind it and surfaces its failure.
void boot().catch(postRuntimeFailure);

self.onmessage = (event: MessageEvent<WorkerRequest>) => {
  const msg = event.data || {};
  if (msg.epoch !== RUNTIME_EPOCH) {
    const response: WorkerFailureMessage = {
      epoch: RUNTIME_EPOCH,
      id: msg.id,
      ok: false,
      error: `runtime epoch mismatch: worker=${RUNTIME_EPOCH} request=${String(msg.epoch)}`,
    };
    self.postMessage(response);
    return;
  }
  const jobId = typeof msg.jobId === "bigint" ? msg.jobId : BigInt(msg.jobId || 0);
  handleRequest(msg)
    .then((result) => {
      const { transfer, ...payload } = result;
      const response: WorkerSuccessMessage = {
        epoch: RUNTIME_EPOCH,
        id: msg.id,
        ok: true,
        ...payload,
      };
      self.postMessage(response, transfer);
    })
    .catch((e) => {
      const response: WorkerFailureMessage = {
        epoch: RUNTIME_EPOCH,
        id: msg.id,
        ok: false,
        error: e && e.message ? e.message : String(e),
      };
      self.postMessage(response);
    })
    .finally(() => {
      if (activeSeisbenchJobId === jobId) activeSeisbenchJobId = 0n;
    });
};
