// The messages between the Pyodide worker's ORT client (web/ort_client.ts) and the
// onnxruntime-web worker (web/ort_worker.ts).

// Which build of onnxruntime-web a worker runs; set by its `?device=` parameter.
export type OrtDevice = "webgpu" | "wasm";

export type OrtInferRequest = {
  type: "infer";
  id: number;
  modelUrl: string;
  input: ArrayBuffer;
  dims: number[];
  label: string;
};

export type OrtProgressMessage = {
  type: "progress";
  id: number;
  fraction: number;
  label: string;
};

export type OrtInferSuccess = {
  type: "result";
  id: number;
  ok: true;
  data: ArrayBuffer;
  dims: number[];
  device: OrtDevice;
};

export type OrtInferFailure = {
  type: "result";
  id: number;
  ok: false;
  error: string;
  device: OrtDevice;
};

export type OrtResultMessage = OrtInferSuccess | OrtInferFailure;

// Progress fractions with a meaning for the client.
// Model download reports below SESSION_CREATING, session creation reports
// SESSION_CREATING, and SESSION_READY means the session exists and the run started.
export const SESSION_CREATING_FRACTION = 0.18;
export const SESSION_READY_FRACTION = 0.2;
