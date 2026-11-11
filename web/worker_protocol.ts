// The messages between the page (web/bridge.ts) and the Pyodide worker
// (web/bridge_worker.ts).
import type { BridgeMethodId } from "./rpc.js";
import type { WorkerCommand } from "./worker_command.js";

export type WorkerRequestMethod = BridgeMethodId | WorkerCommand;

export type WorkerRequest = {
  // The worker generation the request was sent to; a replaced worker rejects it.
  epoch: number;
  // Matches the reply to its request.
  id: number;
  method: WorkerRequestMethod;
  // The activity ledger's job id, which progress events are reported under; 0n
  // for a request outside the ledger.
  jobId: bigint;
  // The request's cancellation control word (see execution_control.ts).
  control: SharedArrayBuffer;
  // Pyodide's interrupt buffer, the same one for every request.
  interrupt: SharedArrayBuffer;
  // The serialized protobuf request.
  request?: Uint8Array;
  // MountGfStore's directory, which cannot be serialized into the protobuf.
  handle?: FileSystemDirectoryHandle;
};

// What the worker hands to postMessage; `transfer` lists the buffers it moves.
export type WorkerReplyPayload = {
  result: ArrayBuffer;
  ys?: ArrayBuffer;
  transfer: Transferable[];
};

// An event the worker posts unprompted (boot and operation progress).
export type WorkerEventMessage = {
  type: "workerEvent";
  epoch: number;
  // A serialized WorkerEvent protobuf.
  event: Uint8Array;
};

export type WorkerSuccessMessage = {
  epoch: number;
  id: number;
  ok: true;
  // A serialized BridgeResult protobuf.
  result: ArrayBuffer;
  // getView's float32 samples, sent beside the protobuf so they are transferred
  // rather than copied.
  ys?: ArrayBuffer;
};

// A failure outside Python's own error envelope: a boot failure, a protocol fault
// or an epoch mismatch.
export type WorkerFailureMessage = {
  epoch: number;
  id: number;
  ok: false;
  error: string;
};

export type WorkerMessage = WorkerEventMessage | WorkerSuccessMessage | WorkerFailureMessage;
