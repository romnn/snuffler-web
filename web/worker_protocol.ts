import type { BridgeMethodId } from "./rpc.js";
import type { WorkerCommand } from "./worker_command.js";

export type WorkerRequestMethod = BridgeMethodId | WorkerCommand;

export type WorkerRequest = {
  epoch: number;
  id: number;
  method: WorkerRequestMethod;
  jobId: bigint;
  control: SharedArrayBuffer;
  interrupt: SharedArrayBuffer;
  request?: Uint8Array;
  data?: Uint8Array;
  handle?: FileSystemDirectoryHandle;
};

export type BridgeTimings = Record<string, number>;

export type WorkerReplyPayload = {
  result: ArrayBuffer;
  ys?: ArrayBuffer;
  perf?: BridgeTimings;
  transfer: Transferable[];
};

export type WorkerEventMessage = {
  type: "workerEvent";
  epoch: number;
  event: Uint8Array;
};

export type WorkerSuccessMessage = {
  epoch: number;
  id: number;
  ok: true;
  result: ArrayBuffer;
  ys?: ArrayBuffer;
  perf?: BridgeTimings;
};

export type WorkerFailureMessage = {
  epoch: number;
  id: number;
  ok: false;
  error: string;
};

export type WorkerMessage = WorkerEventMessage | WorkerSuccessMessage | WorkerFailureMessage;
