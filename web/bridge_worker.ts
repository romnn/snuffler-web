// The app loads our OWN fork-free Pyodide 314 build (pthread + OpenMP + SIMD;
// py3.14 / emscripten 5.0.3) and all wheels from the locally-served py-mt/
// (runtime + pyodide-lock.json + wheels, incl. pyrocko + protobuf). This is the
// ONLY runtime: the page is always cross-origin isolated (see index.html), which
// SharedArrayBuffer / wasm threads require. A separate, non-threaded build can be
// produced later via the `threads` build-time feature and hosted on its own path
// so the two never interfere; there is no runtime CDN fallback / ?threads toggle.
import { create, toBinary } from "@bufbuild/protobuf";
import {
  BootProgressSchema,
  MlDevice,
  MlDeviceStatusSchema,
  SeisbenchProgressSchema,
  WorkerEventSchema,
  type WorkerEvent,
} from "./proto/snuffler/bridge/v1/bridge_pb.js";
import { rpc } from "./rpc.js";
import { WORKER_MOUNT_GF_STORE, WORKER_RELOAD_SNUFFLINGS } from "./worker_command.js";

const PYODIDE_BASE = new URL("py-mt/", self.location.href).href;

// TextDecoder.decode() throws "The provided ArrayBufferView value must not be
// shared" when handed a view backed by a SharedArrayBuffer in this engine. Our
// Pyodide is a threaded (pthread) build, so its WASM heap IS a SharedArrayBuffer,
// and Pyodide's hiwire Python->JS string conversion decodes straight off that heap
// (e.g. loadPackage()'s unpack_buffer -> to_js(get_dynlibs(...)), and any Python
// string/exception crossing into JS). (wasm-bindgen sidesteps this by `.slice()`-ing
// out of shared memory first.) NOTE most decodes — emscripten's own pervasive
// C-string reads — do NOT trip this, so we must not slow them down: try the native
// decode first and only copy-and-retry the rare inputs the engine actually rejects.
// The success path is thus native speed, and on engines that already accept shared
// views no copy ever happens. Must run before Pyodide is loaded.
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
const GET_VIEW_METHOD = rpc("getView");
const BRIDGE_PERF_LOG = false;
type BridgePerf = Record<string, number>;
type MlDeviceInfo = { device: MlDevice; adapter: unknown | null };
type ProgressPost = (fraction: number, label: string) => void;
type FetchProgressContext = {
  start: number;
  end: number;
  label: string;
  post: ProgressPost;
  loadedBytes: number;
  totalBytes: number;
  lastFraction: number;
};
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
type OrtPending = {
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
  pending: Map<number, OrtPending>;
  nextId: number;
};

let mlDeviceInfoPromise: Promise<MlDeviceInfo> | null = null;
let activeFetchProgress: FetchProgressContext | null = null;
let ortWebgpuClient: OrtClient | null = null;
let ortWasmClient: OrtClient | null = null;
const ortWebgpuFailedModels = new Set<string>();
const ORT_WEBGPU_SETUP_TIMEOUT_MS = 120_000;
const ORT_WASM_SETUP_TIMEOUT_MS = 60_000;
const ORT_RUN_TIMEOUT_MS = 180_000;
const nativeFetch = self.fetch.bind(self) as typeof fetch;
const prefetchedResponses = new Map<string, Uint8Array>();
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
  const bytes = toBinary(WorkerEventSchema, create(WorkerEventSchema, { event }));
  self.postMessage({ type: "workerEvent", event: bytes }, [bytes.buffer]);
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
  postWorkerEvent({
    case: "seisbench",
    value: create(SeisbenchProgressSchema, {
      fraction: Math.max(0, Math.min(1, Number(fraction))),
      label: String(label || "Running SeisBench picker"),
    }),
  });
}

function responseContentLength(response: Response): number {
  const value = Number(response.headers.get("content-length") || 0);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

function responseInit(response: Response): ResponseInit {
  return {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  };
}

function progressFraction(start: number, end: number, loaded: number, total: number): number {
  if (total <= 0) return start;
  return start + (end - start) * Math.max(0, Math.min(1, loaded / total));
}

function reportFetchProgress(context: FetchProgressContext, fraction: number) {
  context.lastFraction = Math.max(context.lastFraction, Math.min(context.end, fraction));
  context.post(context.lastFraction, context.label);
}

(self as any).fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = normalizeFetchUrl(input);
  const prefetched = prefetchedResponses.get(url);
  if (prefetched) {
    prefetchedResponses.delete(url);
    return new Response(prefetched.slice(), {
      status: 200,
      headers: responseHeadersFor(url, prefetched),
    });
  }

  const response = await nativeFetch(input, init);
  const context = activeFetchProgress;
  const total = responseContentLength(response);
  if (!context || !response.body || total === 0) {
    return response;
  }

  context.totalBytes += total;
  const [metered, body] = response.body.tee();
  void (async () => {
    const reader = metered.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (!value) continue;
      context.loadedBytes += value.byteLength;
      reportFetchProgress(
        context,
        progressFraction(context.start, context.end, context.loadedBytes, context.totalBytes)
      );
    }
  })().catch((e) => console.warn("[bridge-worker] progress meter failed:", e));
  return new Response(body, responseInit(response));
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

async function prefetchUrls(
  urls: string[],
  start: number,
  end: number,
  label: string,
  cacheResponses: boolean
): Promise<void> {
  const uniqueUrls = Array.from(new Set(urls));
  const lengths = await Promise.all(uniqueUrls.map(fetchContentLength));
  const total = lengths.reduce((sum, value) => sum + value, 0);
  let loaded = 0;
  let lastFraction = start;
  const report = () => {
    const fraction =
      total > 0 ? progressFraction(start, end, loaded, total) : Math.min(end - 0.01, lastFraction);
    lastFraction = Math.max(lastFraction, fraction);
    postBootProgress(lastFraction, label);
  };

  postBootProgress(start, label);
  for (const url of uniqueUrls) {
    const bytes = await fetchBytes(url, (n) => {
      loaded += n;
      report();
    });
    if (cacheResponses) prefetchedResponses.set(url, bytes);
  }
  postBootProgress(end, label);
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

async function prefetchPyodideRuntime(): Promise<void> {
  await prefetchUrls(
    [
      resolveUrl("py-mt/pyodide-lock.json"),
      resolveUrl("py-mt/pyodide.asm.mjs"),
      resolveUrl("py-mt/pyodide.asm.wasm"),
      resolveUrl("py-mt/python_stdlib.zip"),
    ],
    0.22,
    0.45,
    "Downloading Python runtime…",
    true
  );
}

async function prefetchPyodidePackages(): Promise<void> {
  const lockUrl = resolveUrl("py-mt/pyodide-lock.json");
  const lockBytes = prefetchedResponses.get(lockUrl);
  const lock =
    lockBytes != null
      ? JSON.parse(new TextDecoder().decode(lockBytes))
      : await (await nativeFetch(lockUrl)).json();
  const packages = lock.packages || {};
  const urls = pyodidePackageClosure(lock).map((name) =>
    resolveUrl(`py-mt/${packages[name].file_name}`)
  );
  await prefetchUrls(urls, 0.45, 0.86, "Downloading Python packages…", true);
}

async function withFetchProgress<T>(
  start: number,
  end: number,
  label: string,
  post: ProgressPost,
  action: () => Promise<T>
): Promise<T> {
  const previous = activeFetchProgress;
  const context: FetchProgressContext = {
    start,
    end,
    label,
    post,
    loadedBytes: 0,
    totalBytes: 0,
    lastFraction: start,
  };

  try {
    activeFetchProgress = context;
    reportFetchProgress(context, start);
    const result = await action();
    reportFetchProgress(context, end);
    return result;
  } finally {
    activeFetchProgress = previous;
  }
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

function float32Copy(value: any): Float32Array {
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

function intArray(value: any): number[] {
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

function clearOrtTimers(slot: OrtPending) {
  if (slot.timer != null) {
    clearTimeout(slot.timer);
    slot.timer = null;
  }
  stopOrtSetupHeartbeat(slot);
}

function stopOrtSetupHeartbeat(slot: OrtPending) {
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
  const inputData = float32Copy(input);
  const inputDims = intArray(dims);
  return await inferWithFallback(modelUrl, inputData, inputDims, progressLabel(label));
}

(globalThis as any).__snufflerOrtInfer = snufflerOrtInfer;
(globalThis as any).__snufflerSeisbenchProgress = (frac: number, label: string) => {
  postSeisbenchProgress(frac, label);
};

async function fetchOk(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${url}: ${r.status} ${r.statusText}`);
  return r;
}

function prog(frac, label) {
  postBootProgress(frac, label);
}

function destroyMaybe(value) {
  if (value && typeof value.destroy === "function") value.destroy();
}

function ownedBytes(value) {
  const src = value instanceof Uint8Array ? value : new Uint8Array(value.buffer || value);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

function pyBytes(value) {
  try {
    if (value instanceof Uint8Array) return ownedBytes(value);
    if (value && typeof value.toJs === "function") return ownedBytes(value.toJs());
    return ownedBytes(value);
  } finally {
    destroyMaybe(value);
  }
}

async function boot() {
  if (pyodideReadyPromise) return pyodideReadyPromise;
  pyodideReadyPromise = (async () => {
    try {
      const t0 = performance.now();
      prog(0.22, "Downloading Python runtime…");
      await prefetchPyodideRuntime();
      // Pyodide 314 requires a module worker on both paths (it rejects classic
      // workers), so import the ESM build from whichever base is active — our
      // local py-mt/ when threaded, else the stock 314 CDN.
      const { loadPyodide } = await import(PYODIDE_BASE + "pyodide.mjs");
      py = await withFetchProgress(0.43, 0.45, "Starting Python runtime…", postBootProgress, () =>
        loadPyodide({ indexURL: PYODIDE_BASE })
      );
      const tRuntime = performance.now();

      prog(0.45, "Downloading Python packages…");
      await prefetchPyodidePackages();
      prog(0.86, "Installing Python packages…");
      // Everything (incl. pyrocko, protobuf, requests, pyodide-http) is in
      // py-mt/pyodide-lock.json, built for this runtime — load by name.
      await withFetchProgress(0.86, 0.87, "Installing Python packages…", postBootProgress, () =>
        py.loadPackage(PYODIDE_PACKAGES)
      );
      prefetchedResponses.clear();
      const tPackages = performance.now();

      prog(0.88, "Importing pyrocko…");
      for (const dir of ["/snuffler", "/snuffler/bridge", "/snuffler/bridge/v1"]) {
        try {
          py.FS.mkdir(dir);
        } catch (_e) {
          // Already exists.
        }
      }
      for (const file of [
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
      ]) {
        const src = await (await fetchOk(resolveUrl(`proto/snuffler/bridge/v1/${file}`))).text();
        py.FS.writeFile(`/snuffler/bridge/v1/${file}`, src);
      }
      // Helper module bridge.py imports lazily for SeisBench pre/post-processing.
      for (const mod of ["seisbench_infer.py"]) {
        const src = await (await fetchOk(resolveUrl(mod))).text();
        py.FS.writeFile(`/${mod}`, src);
      }
      const bridgeSrc = await (await fetchOk(resolveUrl("bridge.py"))).text();
      py.FS.writeFile("/bridge.py", bridgeSrc);
      bridge = py.runPython('import sys; sys.path.insert(0, "/"); import bridge; bridge');
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
          `total=${ms(t0, tImport)}`
      );

      // Report the booted runtime so the UI can show a truthful debug badge.
      // pthreads is read back from the live interpreter to confirm threading
      // really came up on our py-mt build (the badge then reads "wasm+openmp+simd").
      try {
        self.postMessage({
          type: "runtime",
          threaded: true,
          simd: true,
          pthreads: py.runPython(
            "import sys; bool(getattr(sys._emscripten_info, 'pthreads', False))"
          ),
          python: py.runPython("import sys; sys.version.split()[0]"),
          numpy: py.runPython("import numpy; numpy.__version__"),
          pyrocko: py.runPython("import pyrocko; getattr(pyrocko, '__version__', 'unknown')"),
          cores: (self.navigator && self.navigator.hardwareConcurrency) || 0,
        });
      } catch (_e) {
        // Non-fatal: the badge just won't show.
      }
      try {
        postMlDevice(await mlDevice());
      } catch (_e) {
        postMlDevice(MlDevice.WASM);
      }

      prog(0.97, "Almost ready…");
      return py;
    } catch (e) {
      console.error("[bridge-worker] boot failed:", e);
      throw e;
    }
  })();
  return pyodideReadyPromise;
}

// Shape the {"result": BridgeResult bytes, "buffer"?: ndarray} dict that
// bridge.call() returns into the worker's postMessage payload. The result bytes
// (the proto envelope — success variant OR BridgeError) always flow through;
// the zero-copy ys buffer is transferred only when present (getView).
function bridgeResult(res, perf: BridgePerf | null = null) {
  try {
    const shapeT0 = performance.now();
    const resultT0 = performance.now();
    const result = pyBytes(res.get("result")); // pyBytes owns + destroys the inner proxy
    if (perf) {
      perf.result_copy_ms = performance.now() - resultT0;
      perf.result_bytes = result.byteLength;
    }
    const out: Record<string, any> = { result: result.buffer, transfer: [result.buffer] };
    if (res.has("buffer")) {
      const ysProxy = res.get("buffer");
      let buf: any = null;
      try {
        const viewT0 = performance.now();
        buf = ysProxy.getBuffer();
        if (perf) perf.buffer_view_ms = performance.now() - viewT0;
        const d = buf.data;
        const copyT0 = performance.now();
        const ys = ownedBytes(
          d instanceof Uint8Array ? d : new Uint8Array(d.buffer, d.byteOffset, d.byteLength)
        );
        if (perf) {
          perf.ys_copy_ms = performance.now() - copyT0;
          perf.ys_bytes = ys.byteLength;
        }
        out.ys = ys.buffer;
        out.transfer.push(ys.buffer);
      } finally {
        if (buf) buf.release();
        destroyMaybe(ysProxy);
      }
    }
    if (perf) {
      perf.shape_ms = performance.now() - shapeT0;
      out.perf = perf;
    }
    return out;
  } finally {
    destroyMaybe(res);
  }
}

// Extract the string from a bridge.call_text() {"text", "error"} reply. The
// error variant is surfaced as a plain JS Error; bridge.py returns the message as
// a field (rather than raising) so it crosses as data, and the TextDecoder shim
// above now also covers Python strings/exceptions decoded off the shared heap.
function callText(res) {
  try {
    const error = res.get("error");
    if (error) throw new Error(error);
    return res.get("text") || "";
  } finally {
    destroyMaybe(res);
  }
}

// The worker is a DUMB RELAY: it forwards the generated method id + request bytes
// (+ optional out-of-band data) to bridge.py's async dispatch, which runs the rpc
// — including its own network fetches (pyfetch) — and returns the BridgeResult
// envelope (plus the zero-copy ys buffer for getView). It spells no
// method names of its own, so nothing here can drift from the .proto.
//
// The ONE exception is mounting a live directory handle for a GF store: that is a
// DOM object that can't ride a proto, so the worker mounts it into the Pyodide FS
// and then relays GfStoreLabel for the registered store's label.
async function handleRequest(msg, workerRecvMs: number) {
  await boot();
  const tracePerf: BridgePerf | null =
    BRIDGE_PERF_LOG && msg.method === GET_VIEW_METHOD
      ? {
          worker_recv_ms: workerRecvMs,
          request_bytes: msg.request ? msg.request.byteLength || 0 : 0,
          data_bytes: msg.data ? msg.data.byteLength || 0 : 0,
        }
      : null;
  if (msg.method === WORKER_RELOAD_SNUFFLINGS) {
    // Re-fetch bridge.py (the snuffling logic) and importlib.reload it, preserving
    // the loaded pile + catalog/station/command state across the reload so the
    // session is not lost (snuffler's Shift+R reloads code, not data).
    for (const mod of ["seisbench_infer.py"]) {
      const modSrc = await (await fetchOk(resolveUrl(mod))).text();
      py.FS.writeFile(`/${mod}`, modSrc);
    }
    const src = await (await fetchOk(resolveUrl("bridge.py"))).text();
    py.FS.writeFile("/bridge.py", src);
    py.runPython(`
import importlib, bridge
for _m in ('seisbench_infer',):
    if _m in importlib.sys.modules:
        importlib.reload(importlib.sys.modules[_m])
_keep = ('_TRACES','_EVENTS','_STATIONS','_ACTIVE_EVENT','_ACTIVE_MT','_EVENT_REGISTRY',
         '_STATIONXML','_HIDDEN','_REDUCE','_SCALING_HOOKS','_SOURCE_TRACES','_PLATES',
         '_GF_STORES','_DATA_GEN','_RESPONSE_GEN','_SEISBENCH_MODEL_URLS')
_saved = {k: getattr(bridge, k) for k in _keep if hasattr(bridge, k)}
importlib.invalidate_caches()
importlib.reload(bridge)
for _k, _v in _saved.items():
    setattr(bridge, _k, _v)
bridge._mark_traces_changed()
`);
    return { text: "ok" };
  }
  if (msg.method === WORKER_MOUNT_GF_STORE) {
    gfStoreCounter += 1;
    const mount = "/gfstores/" + gfStoreCounter;
    try {
      py.FS.mkdir("/gfstores");
    } catch (_e) {
      // Already exists.
    }
    py.FS.mkdir(mount);
    await py.mountNativeFS(mount, msg.handle);
    return { text: callText(await bridge.call_text(rpc("gfStoreLabel"), null, mount)) };
  }
  const pyCallT0 = performance.now();
  const res = await bridge.call(msg.method, msg.request, msg.data ?? null);
  if (tracePerf) tracePerf.py_call_ms = performance.now() - pyCallT0;
  return bridgeResult(res, tracePerf);
}

self.onmessage = (event) => {
  const workerRecvMs = performance.now();
  const msg = event.data || {};
  handleRequest(msg, workerRecvMs)
    .then((result: any) => {
      const transfer = result.transfer || [];
      const { transfer: _transfer, ...payload } = result;
      if (payload.perf) payload.perf.worker_post_epoch_ms = Date.now();
      self.postMessage({ id: msg.id, ok: true, ...payload }, transfer);
    })
    .catch((e) => {
      self.postMessage({
        id: msg.id,
        ok: false,
        error: e && e.message ? e.message : String(e),
      });
    });
};
