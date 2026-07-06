type MlDevice = "webgpu" | "wasm";

type InferRequest = {
  id: number;
  type: "infer";
  modelUrl: string;
  input: ArrayBuffer;
  dims: number[];
  label: string;
};

type ProgressMessage = {
  type: "progress";
  id: number;
  fraction: number;
  label: string;
};

type InferResultMessage = {
  id: number;
  type: "result";
  ok: true;
  data: ArrayBuffer;
  dims: number[];
  device: MlDevice;
};

type InferErrorMessage = {
  id: number;
  type: "result";
  ok: false;
  error: string;
  device: MlDevice;
};

type OrtRuntime = typeof import("onnxruntime-web/webgpu");
type OrtSession = Awaited<ReturnType<OrtRuntime["InferenceSession"]["create"]>>;

const device: MlDevice =
  new URL(self.location.href).searchParams.get("device") === "webgpu" ? "webgpu" : "wasm";
const sessions = new Map<string, Promise<OrtSession>>();
let ortRuntimePromise: Promise<OrtRuntime> | null = null;
const nativeConsoleError = console.error.bind(console);
console.error = (...args: unknown[]) => {
  const text = args.map((arg) => String(arg)).join(" ");
  if (text.includes("VerifyEachNodeIsAssignedToAnEp")) {
    console.warn(...args);
    return;
  }
  nativeConsoleError(...args);
};

function resolveUrl(path: string) {
  return new URL(path, self.location.href).href;
}

function postProgress(id: number, fraction: number, label: string) {
  const msg: ProgressMessage = { type: "progress", id, fraction, label };
  self.postMessage(msg);
}

function responseContentLength(response: Response): number {
  const value = Number(response.headers.get("content-length") || 0);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

function progressFraction(start: number, end: number, loaded: number, total: number): number {
  if (total <= 0) return start;
  return start + (end - start) * Math.max(0, Math.min(1, loaded / total));
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

async function fetchModelBytes(id: number, modelUrl: string, label: string): Promise<Uint8Array> {
  postProgress(id, 0.04, `${label} · downloading model`);
  const response = await fetch(modelUrl);
  if (!response.ok) throw new Error(`fetch ${modelUrl}: ${response.status} ${response.statusText}`);

  if (!response.body) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    postProgress(id, 0.16, `${label} · model downloaded`);
    return bytes;
  }

  const total = responseContentLength(response);
  const chunks: Uint8Array[] = [];
  let loaded = 0;
  let lastFraction = 0.04;
  const report = () => {
    let fraction: number;
    let text: string;
    if (total > 0) {
      fraction = progressFraction(0.04, 0.16, loaded, total);
      text = `${label} · downloading model ${Math.round((loaded / total) * 100)}%`;
    } else {
      fraction = Math.min(0.15, 0.04 + 0.12 * Math.min(0.95, loaded / 16_777_216));
      text = `${label} · downloading model ${(loaded / 1_048_576).toFixed(1)} MB`;
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
    loaded += value.byteLength;
    report();
  }

  const bytes = new Uint8Array(loaded);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  postProgress(id, 0.16, `${label} · model downloaded`);
  return bytes;
}

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
      postProgress(id, 0.18, `${label} · creating inference session`);
      const created = await ort.InferenceSession.create(bytes, {
        executionProviders: providers,
      });
      postProgress(id, 0.2, `${label} · running inference`);
      return created;
    })().catch((e) => {
      sessions.delete(modelUrl);
      throw e;
    });
    sessions.set(modelUrl, pending);
  }
  return pending;
}

async function infer(msg: InferRequest): Promise<InferResultMessage> {
  const ort = await ortRuntime();
  const modelSession = await session(msg.id, msg.modelUrl, msg.label || "SeisBench picker");
  const inputData = new Float32Array(msg.input);
  const tensor = new ort.Tensor("float32", inputData, msg.dims);
  const output = await modelSession.run({ [modelSession.inputNames[0]]: tensor });
  const first = output[modelSession.outputNames[0]];
  const data =
    first.data instanceof Float32Array ? first.data : new Float32Array(first.data as any);
  const out = new Float32Array(data.length);
  out.set(data);
  return {
    id: msg.id,
    type: "result",
    ok: true,
    data: out.buffer,
    dims: Array.from(first.dims),
    device,
  };
}

self.onmessage = (event) => {
  const msg = event.data as InferRequest;
  if (!msg || msg.type !== "infer") return;
  infer(msg)
    .then((result) => self.postMessage(result, [result.data]))
    .catch((e) => {
      const result: InferErrorMessage = {
        id: msg.id,
        type: "result",
        ok: false,
        error: e && e.message ? e.message : String(e),
        device,
      };
      self.postMessage(result);
    });
};
