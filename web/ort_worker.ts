// The onnxruntime-web worker: runs SeisBench's exported ONNX graphs for the
// Pyodide worker's ORT client (web/ort_client.ts).
// One worker per device, chosen by the `?device=` parameter, so the WebGPU build
// and the wasm build never share a process.
import {
  SESSION_CREATING_FRACTION,
  SESSION_READY_FRACTION,
  type OrtDevice,
  type OrtInferFailure,
  type OrtInferRequest,
  type OrtInferSuccess,
  type OrtProgressMessage,
} from "./ort_protocol.js";

type OrtRuntime = typeof import("onnxruntime-web/webgpu");
type OrtSession = Awaited<ReturnType<OrtRuntime["InferenceSession"]["create"]>>;

const device: OrtDevice =
  new URL(self.location.href).searchParams.get("device") === "webgpu" ? "webgpu" : "wasm";
// Model sessions by url, created once per worker.
const sessions = new Map<string, Promise<OrtSession>>();
let ortRuntimePromise: Promise<OrtRuntime> | null = null;
// ORT logs VerifyEachNodeIsAssignedToAnEp as an error whenever some nodes fall back
// from WebGPU to the wasm provider.
// That fallback is expected, so the message is demoted to a warning.
const nativeConsoleError = console.error.bind(console);
console.error = (...args: unknown[]) => {
  const text = args.map((arg) => String(arg)).join(" ");
  if (text.includes("VerifyEachNodeIsAssignedToAnEp")) {
    console.warn(...args);
    return;
  }
  nativeConsoleError(...args);
};

function resolveUrl(path: string): string {
  return new URL(path, self.location.href).href;
}

function postProgress(id: number, fraction: number, label: string) {
  const message: OrtProgressMessage = { type: "progress", id, fraction, label };
  self.postMessage(message);
}

function responseContentLength(response: Response): number {
  const value = Number(response.headers.get("content-length") || 0);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

function progressFraction(start: number, end: number, done: number, total: number): number {
  if (total <= 0) return start;
  return start + (end - start) * Math.max(0, Math.min(1, done / total));
}

async function ortRuntime(): Promise<OrtRuntime> {
  if (!ortRuntimePromise) {
    ortRuntimePromise = (async () => {
      const ort =
        device === "webgpu"
          ? await import("onnxruntime-web/webgpu")
          : ((await import("onnxruntime-web/wasm")) as OrtRuntime);
      ort.env.logLevel = "error";
      ort.env.wasm.proxy = false;
      ort.env.wasm.numThreads = 1;
      ort.env.wasm.wasmPaths = {
        wasm:
          device === "webgpu"
            ? resolveUrl("ort/ort-wasm-simd-threaded.asyncify.wasm")
            : resolveUrl("ort/ort-wasm-simd-threaded.wasm"),
      };
      return ort;
    })();
  }
  return ortRuntimePromise;
}

// The model download's span of the progress bar.
const DOWNLOAD_START_FRACTION = 0.04;
const DOWNLOAD_END_FRACTION = 0.16;
// Without a content-length the bar assumes a 16 MiB model and stops short of the
// span's end.
const ASSUMED_MODEL_BYTES = 16 * 1024 * 1024;

async function fetchModelBytes(id: number, modelUrl: string, label: string): Promise<Uint8Array> {
  postProgress(id, DOWNLOAD_START_FRACTION, `${label} · downloading model`);
  const response = await fetch(modelUrl);
  if (!response.ok) throw new Error(`fetch ${modelUrl}: ${response.status} ${response.statusText}`);

  if (!response.body) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    postProgress(id, DOWNLOAD_END_FRACTION, `${label} · model downloaded`);
    return bytes;
  }

  const totalBytes = responseContentLength(response);
  const chunks: Uint8Array[] = [];
  let loadedBytes = 0;
  let lastFraction = DOWNLOAD_START_FRACTION;
  const report = () => {
    let fraction: number;
    let text: string;
    if (totalBytes > 0) {
      fraction = progressFraction(
        DOWNLOAD_START_FRACTION,
        DOWNLOAD_END_FRACTION,
        loadedBytes,
        totalBytes
      );
      text = `${label} · downloading model ${Math.round((loadedBytes / totalBytes) * 100)}%`;
    } else {
      const assumedShare = Math.min(0.95, loadedBytes / ASSUMED_MODEL_BYTES);
      fraction = Math.min(0.15, 0.04 + 0.12 * assumedShare);
      text = `${label} · downloading model ${(loadedBytes / 1_048_576).toFixed(1)} MB`;
    }
    lastFraction = Math.max(lastFraction, fraction);
    postProgress(id, lastFraction, text);
  };

  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (!value) continue;
    chunks.push(value);
    loadedBytes += value.byteLength;
    report();
  }

  const bytes = new Uint8Array(loadedBytes);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  postProgress(id, DOWNLOAD_END_FRACTION, `${label} · model downloaded`);
  return bytes;
}

// The model's session, created on first use; a failed creation is forgotten so the
// next request retries it.
async function session(id: number, modelUrl: string, label: string): Promise<OrtSession> {
  let pending = sessions.get(modelUrl);
  if (!pending) {
    pending = (async () => {
      const ort = await ortRuntime();
      const providers = device === "webgpu" ? ["webgpu", "wasm"] : ["wasm"];
      console.info(
        `[ort-worker] SeisBench ORT session ${modelUrl} executionProviders=${providers.join(",")}`
      );
      const bytes = await fetchModelBytes(id, modelUrl, label);
      postProgress(id, SESSION_CREATING_FRACTION, `${label} · creating inference session`);
      const created = await ort.InferenceSession.create(bytes, {
        executionProviders: providers,
      });
      postProgress(id, SESSION_READY_FRACTION, `${label} · running inference`);
      return created;
    })().catch((e) => {
      sessions.delete(modelUrl);
      throw e;
    });
    sessions.set(modelUrl, pending);
  }
  return pending;
}

async function infer(request: OrtInferRequest): Promise<OrtInferSuccess> {
  const ort = await ortRuntime();
  const modelSession = await session(
    request.id,
    request.modelUrl,
    request.label || "SeisBench picker"
  );
  const inputData = new Float32Array(request.input);
  const tensor = new ort.Tensor("float32", inputData, request.dims);
  const output = await modelSession.run({ [modelSession.inputNames[0]]: tensor });
  const firstOutput = output[modelSession.outputNames[0]];
  const data =
    firstOutput.data instanceof Float32Array
      ? firstOutput.data
      : new Float32Array(firstOutput.data as any);
  // Copied into a buffer of its own, which is then transferred to the caller.
  const copy = new Float32Array(data.length);
  copy.set(data);
  return {
    id: request.id,
    type: "result",
    ok: true,
    data: copy.buffer,
    dims: Array.from(firstOutput.dims),
    device,
  };
}

self.onmessage = (event) => {
  const request = event.data as OrtInferRequest;
  if (!request || request.type !== "infer") return;
  infer(request)
    .then((result) => self.postMessage(result, [result.data]))
    .catch((e) => {
      const result: OrtInferFailure = {
        id: request.id,
        type: "result",
        ok: false,
        error: e && e.message ? e.message : String(e),
        device,
      };
      self.postMessage(result);
    });
};
