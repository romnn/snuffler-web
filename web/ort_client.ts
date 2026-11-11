// SeisBench inference for the Python bridge: runs the exported ONNX graph in a
// dedicated onnxruntime-web worker (web/ort_worker.ts), on WebGPU when the browser
// has an adapter and on CPU wasm otherwise.
// Python calls `snufflerOrtInfer` through the `__snufflerOrtInfer` global.
import { MlDevice } from "./proto/snuffler_web_proto/bridge_pb.js";
import { postMlDevice, postSeisbenchProgress } from "./worker_events.js";
import {
  SESSION_CREATING_FRACTION,
  SESSION_READY_FRACTION,
  type OrtDevice,
  type OrtInferRequest,
  type OrtProgressMessage,
  type OrtResultMessage,
} from "./ort_protocol.js";
import { resolveUrl } from "./pyodide_prefetch.js";

type MlDeviceInfo = { device: MlDevice; adapter: unknown | null };
type OrtInferResult = { data: ArrayBuffer; dims: number[]; device: OrtDevice };

// One inference request awaiting its result.
// Session setup and the run itself get separate timeouts; `isSessionReady` says
// which one `timeoutTimer` is currently counting.
type PendingOrtRequest = {
  resolve: (value: OrtInferResult) => void;
  reject: (error: Error) => void;
  timeoutTimer: number | null;
  heartbeatTimer: number | null;
  runTimeoutMs: number;
  isSessionReady: boolean;
  setupStartedMs: number | null;
  setupLabel: string;
};
type OrtClient = {
  worker: Worker;
  device: OrtDevice;
  pending: Map<number, PendingOrtRequest>;
  nextId: number;
};

const ORT_WEBGPU_SETUP_TIMEOUT_MS = 120_000;
const ORT_WASM_SETUP_TIMEOUT_MS = 60_000;
const ORT_RUN_TIMEOUT_MS = 180_000;

let mlDeviceInfoPromise: Promise<MlDeviceInfo> | null = null;
let ortWebgpuClient: OrtClient | null = null;
let ortWasmClient: OrtClient | null = null;
// Models that failed on WebGPU once run on wasm from then on.
const ortWebgpuFailedModels = new Set<string>();

function mlDeviceInfo(): Promise<MlDeviceInfo> {
  mlDeviceInfoPromise ??= (async () => {
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
  return mlDeviceInfoPromise;
}

// The device SeisBench inference will try first.
export async function mlDevice(): Promise<MlDevice> {
  return (await mlDeviceInfo()).device;
}

// Copies a Python buffer, typed array or list into a JS-owned Float32Array.
function copyFloat32Array(value: any): Float32Array {
  if (value instanceof Float32Array) {
    const copy = new Float32Array(value.length);
    copy.set(value);
    return copy;
  }
  if (value && typeof value.getBuffer === "function") {
    const buffer = value.getBuffer();
    try {
      const source = buffer.data;
      const view =
        source instanceof Float32Array
          ? source
          : new Float32Array(
              source.buffer,
              source.byteOffset,
              source.byteLength / Float32Array.BYTES_PER_ELEMENT
            );
      const copy = new Float32Array(view.length);
      copy.set(view);
      return copy;
    } finally {
      buffer.release();
    }
  }
  if (ArrayBuffer.isView(value)) {
    const source = new Float32Array(
      value.buffer,
      value.byteOffset,
      value.byteLength / Float32Array.BYTES_PER_ELEMENT
    );
    const copy = new Float32Array(source.length);
    copy.set(source);
    return copy;
  }
  if (Array.isArray(value)) return new Float32Array(value);
  throw new Error("expected Float32Array-compatible ORT input");
}

function copyIntegerArray(value: any): number[] {
  if (Array.isArray(value)) return value.map((x) => Number(x));
  if (value && typeof value.toJs === "function") return Array.from(value.toJs(), (x) => Number(x));
  return Array.from(value || [], (x) => Number(x));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function createOrtClient(device: OrtDevice): OrtClient {
  const worker = new Worker(`${resolveUrl("ort_worker.js")}?device=${device}`, { type: "module" });
  const client: OrtClient = {
    worker,
    device,
    pending: new Map(),
    nextId: 1,
  };
  worker.onmessage = (event) => {
    const message = event.data as OrtProgressMessage | OrtResultMessage;
    if (!message) return;

    // Progress: switch from the setup timeout to the run timeout once the session
    // is ready, and keep a slow setup visibly alive until then.
    if (message.type === "progress") {
      const request = client.pending.get(message.id);
      if (request && message.fraction >= SESSION_READY_FRACTION && !request.isSessionReady) {
        request.isSessionReady = true;
        stopOrtSetupHeartbeat(request);
        armOrtTimeout(client, message.id, request.runTimeoutMs);
      } else if (
        request &&
        message.fraction >= SESSION_CREATING_FRACTION &&
        !request.isSessionReady
      ) {
        armOrtSetupHeartbeat(client, message.id, message.label);
      }
      postSeisbenchProgress(message.fraction, message.label);
      return;
    }

    // Result
    if (message.type !== "result") return;
    const request = client.pending.get(message.id);
    if (!request) return;
    client.pending.delete(message.id);
    clearOrtTimers(request);
    if (message.ok) {
      request.resolve({ data: message.data, dims: message.dims, device: message.device });
    } else {
      request.reject(new Error(message.error || `${device} inference failed`));
    }
  };
  worker.onerror = (event) => {
    const error = new Error(event.message || `${device} inference worker crashed`);
    rejectOrtClient(client, error);
  };
  return client;
}

function ortClientFor(device: OrtDevice): OrtClient {
  if (device === "webgpu") {
    ortWebgpuClient ??= createOrtClient(device);
    return ortWebgpuClient;
  }
  ortWasmClient ??= createOrtClient(device);
  return ortWasmClient;
}

function rejectOrtClient(client: OrtClient, error: Error) {
  for (const request of client.pending.values()) {
    clearOrtTimers(request);
    request.reject(error);
  }
  client.pending.clear();
}

// A timed-out inference cannot be cancelled inside the ORT worker, so the whole
// worker goes; the next request creates a fresh one.
function terminateOrtClient(client: OrtClient, reason: Error) {
  rejectOrtClient(client, reason);
  client.worker.terminate();
  if (client.device === "webgpu" && ortWebgpuClient === client) ortWebgpuClient = null;
  if (client.device === "wasm" && ortWasmClient === client) ortWasmClient = null;
}

function armOrtTimeout(client: OrtClient, id: number, timeoutMs: number) {
  const request = client.pending.get(id);
  if (!request) return;
  if (request.timeoutTimer != null) clearTimeout(request.timeoutTimer);
  if (timeoutMs <= 0) {
    request.timeoutTimer = null;
    return;
  }
  request.timeoutTimer = self.setTimeout(() => {
    const current = client.pending.get(id);
    if (!current) return;
    const phase = current.isSessionReady ? "run" : "setup";
    terminateOrtClient(client, new Error(`${client.device} inference ${phase} timed out`));
  }, timeoutMs);
}

function clearOrtTimers(request: PendingOrtRequest) {
  if (request.timeoutTimer != null) {
    clearTimeout(request.timeoutTimer);
    request.timeoutTimer = null;
  }
  stopOrtSetupHeartbeat(request);
}

function stopOrtSetupHeartbeat(request: PendingOrtRequest) {
  if (request.heartbeatTimer != null) {
    clearInterval(request.heartbeatTimer);
    request.heartbeatTimer = null;
  }
}

// Session creation reports nothing while it compiles the model, which can take a
// minute on WebGPU.
// The heartbeat ticks the elapsed seconds into the label and creeps the bar towards
// SESSION_READY_FRACTION without ever reaching it.
function armOrtSetupHeartbeat(client: OrtClient, id: number, label: string) {
  const request = client.pending.get(id);
  if (!request || request.isSessionReady) return;
  request.setupLabel = label;
  request.setupStartedMs ??= performance.now();
  if (request.heartbeatTimer != null) return;
  request.heartbeatTimer = self.setInterval(() => {
    const current = client.pending.get(id);
    if (!current || current.isSessionReady || current.setupStartedMs == null) {
      if (current) stopOrtSetupHeartbeat(current);
      return;
    }
    const elapsedS = Math.max(1, Math.round((performance.now() - current.setupStartedMs) / 1000));
    const creep = 0.019 * (1 - Math.exp(-elapsedS / 30));
    const fraction = Math.min(0.199, SESSION_CREATING_FRACTION + creep);
    postSeisbenchProgress(fraction, `${current.setupLabel} (${elapsedS}s)`);
  }, 1_000);
}

function sendOrtInfer(
  device: OrtDevice,
  modelUrl: string,
  inputData: Float32Array,
  inputDims: number[],
  label: string,
  setupTimeoutMs: number,
  runTimeoutMs: number
): Promise<OrtInferResult> {
  const client = ortClientFor(device);
  const id = client.nextId++;
  const input = new ArrayBuffer(inputData.byteLength);
  new Float32Array(input).set(inputData);
  return new Promise((resolve, reject) => {
    client.pending.set(id, {
      resolve,
      reject,
      timeoutTimer: null,
      heartbeatTimer: null,
      runTimeoutMs,
      isSessionReady: false,
      setupStartedMs: null,
      setupLabel: `${label} · creating inference session`,
    });
    armOrtTimeout(client, id, setupTimeoutMs);
    const request: OrtInferRequest = {
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

// WebGPU first when the browser has it and this model has not failed on it yet;
// any WebGPU failure retries the same input on wasm.
async function inferWithFallback(
  modelUrl: string,
  inputData: Float32Array,
  inputDims: number[],
  label: string
): Promise<{ data: Float32Array; dims: number[] }> {
  const info = await mlDeviceInfo();
  const normalizedModelUrl = new URL(modelUrl, self.location.href).href;
  const prefersWebgpu =
    info.device === MlDevice.WEBGPU && !ortWebgpuFailedModels.has(normalizedModelUrl);
  const inferOnWasm = () =>
    sendOrtInfer(
      "wasm",
      modelUrl,
      inputData,
      inputDims,
      label,
      ORT_WASM_SETUP_TIMEOUT_MS,
      ORT_RUN_TIMEOUT_MS
    );

  let result: OrtInferResult;
  if (prefersWebgpu) {
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
      postSeisbenchProgress(
        SESSION_CREATING_FRACTION,
        `${label} · WebGPU unavailable; using CPU (wasm)`
      );
      result = await inferOnWasm();
    }
  } else {
    result = await inferOnWasm();
  }
  if (result.device === "wasm") postMlDevice(MlDevice.WASM);
  return { data: new Float32Array(result.data), dims: result.dims };
}

// The entry point Python calls (web/seisbench_infer.py), with Pyodide proxies for
// the input array and its shape.
export async function snufflerOrtInfer(modelUrl: string, input: any, dims: any, label?: string) {
  const inputData = copyFloat32Array(input);
  const inputDims = copyIntegerArray(dims);
  return await inferWithFallback(
    modelUrl,
    inputData,
    inputDims,
    String(label || "SeisBench picker")
  );
}
