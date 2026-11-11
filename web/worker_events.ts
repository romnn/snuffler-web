// The events the Pyodide worker posts to the page unprompted: boot progress,
// operation progress, the runtime badge and boot failures.
// Each is a `WorkerEvent` protobuf tagged with this worker's runtime epoch.
import { create, toBinary } from "@bufbuild/protobuf";
import {
  BootProgressSchema,
  MlDeviceStatusSchema,
  OperationCountProgressSchema,
  OperationEventSchema,
  OperationProgressSchema,
  OperationStartedSchema,
  RuntimeFailureSchema,
  RuntimeInfoSchema,
  WorkerEventSchema,
  type MlDevice,
  type OperationProgress,
  type WorkerEvent,
} from "./proto/snuffler_web_proto/bridge_pb.js";
import { type WorkerEventMessage } from "./worker_protocol.js";

// The page starts each worker with `?epoch=N`, and drops every message whose epoch
// is not the current one, so a replaced worker's late events are ignored.
const epochParameter = Number(new URL(self.location.href).searchParams.get("epoch") || 0);
export const RUNTIME_EPOCH =
  Number.isSafeInteger(epochParameter) && epochParameter >= 0 ? epochParameter >>> 0 : 0;

// The job a running SeisBench snuffling reports its progress under.
// The ORT client only knows a fraction and a label, so the worker remembers which
// job is the SeisBench one; 0n means no SeisBench run is active.
let seisbenchJobId = 0n;

export function setSeisbenchJob(jobId: bigint) {
  seisbenchJobId = jobId;
}

export function clearSeisbenchJob(jobId: bigint) {
  if (seisbenchJobId === jobId) seisbenchJobId = 0n;
}

function postWorkerEvent(event: WorkerEvent["event"]) {
  const bytes = toBinary(
    WorkerEventSchema,
    create(WorkerEventSchema, { event, runtimeEpoch: RUNTIME_EPOCH })
  );
  const message: WorkerEventMessage = { type: "workerEvent", epoch: RUNTIME_EPOCH, event: bytes };
  self.postMessage(message, [bytes.buffer]);
}

export function postMlDevice(device: MlDevice) {
  postWorkerEvent({
    case: "mlDevice",
    value: create(MlDeviceStatusSchema, { device }),
  });
}

export function postBootProgress(fraction: number, label: string) {
  postWorkerEvent({
    case: "boot",
    value: create(BootProgressSchema, { fraction, label }),
  });
}

export function postSeisbenchProgress(fraction: number, label: string) {
  if (seisbenchJobId === 0n) return;
  postOperationProgress(seisbenchJobId, String(label || "Running SeisBench picker"), {
    case: "fraction",
    value: Math.max(0, Math.min(1, Number(fraction))),
  });
}

// `total` is null when the operation does not know how much work is left.
export function postCountProgress(
  jobId: bigint,
  done: number,
  total: number | null,
  phase: string
) {
  postOperationProgress(jobId, phase, {
    case: "count",
    value: create(OperationCountProgressSchema, {
      done: BigInt(Math.max(0, Math.trunc(done))),
      total: total == null ? undefined : BigInt(Math.max(0, Math.trunc(total))),
    }),
  });
}

// Job 0n is a request with no ledger entry, so it has nobody to report to.
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

export function postOperationStarted(jobId: bigint) {
  postWorkerEvent({
    case: "operation",
    value: create(OperationEventSchema, {
      jobId,
      event: { case: "started", value: create(OperationStartedSchema) },
    }),
  });
}

export function postRuntimeFailure(error: unknown) {
  const value = error instanceof Error ? error : new Error(String(error));
  postWorkerEvent({
    case: "runtimeFailure",
    value: create(RuntimeFailureSchema, {
      kind: value.name || "Error",
      message: value.message || "Python runtime boot failed",
    }),
  });
}

// Reads the versions back from the live interpreter, so the debug badge shows what
// actually booted.
// `pthreads` confirms threading really came up on the py-mt build.
export function postRuntimeInfo(pyodide: any) {
  const isThreaded = Boolean(
    pyodide.runPython("import sys; bool(getattr(sys._emscripten_info, 'pthreads', False))")
  );
  postWorkerEvent({
    case: "runtimeInfo",
    value: create(RuntimeInfoSchema, {
      threaded: isThreaded,
      python: String(pyodide.runPython("import sys; sys.version.split()[0]")),
      numpy: String(pyodide.runPython("import numpy; numpy.__version__")),
      pyrocko: String(
        pyodide.runPython("import pyrocko; getattr(pyrocko, '__version__', 'unknown')")
      ),
      cores: (self.navigator && self.navigator.hardwareConcurrency) || 0,
    }),
  });
}

// The boot progress bar.
//
// The download meter runs across several phases (the runtime starts while the
// wheels are still coming in), so its fraction is byte-driven while the label
// tracks whatever boot() is actually waiting on.
//
// Each phase's span is its share of a real first visit, measured at 25 Mbit/s with
// a cold cache (~20 s end to end).
// The app wasm and the Python assets download concurrently (index.html starts this
// worker at page parse), and the two meters cannot see each other's bytes, so the
// bar shows whichever is further along.
// The spans:
//
//   0    -> 0.44  the app wasm, driven by the Trunk loader (scripts/patch-trunk-loader.mjs)
//   0.44 -> 0.84  downloads, by bytes
//   0.84 -> 0.92  installing packages, by wheel bytes
//   0.92 -> 0.96  importing the bridge, by modules
//   0.96 -> 1     the first scene
export const BOOT_SPAN = {
  downloadStart: 0.44,
  installStart: 0.84,
  importStart: 0.92,
  importEnd: 0.96,
  almostReady: 0.97,
} as const;

let bootPhase = "Starting…";
// Never moves backwards: concurrent meters each report their own estimate.
let bootFraction = 0;

// `start + (end - start) * done / total`, clamped to the span.
export function progressFraction(start: number, end: number, done: number, total: number): number {
  if (total <= 0) return start;
  return start + (end - start) * Math.max(0, Math.min(1, done / total));
}

// Relabels the bar without moving it.
export function setBootPhase(label: string) {
  bootPhase = label;
  postBootProgress(bootFraction, label);
}

// Advances the bar, keeping the current label.
export function advanceBootFraction(fraction: number) {
  bootFraction = Math.max(bootFraction, fraction);
  postBootProgress(bootFraction, bootPhase);
}

// Advances and relabels the bar in one event.
export function reportBootProgress(fraction: number, label: string) {
  bootFraction = Math.max(bootFraction, fraction);
  bootPhase = label;
  postBootProgress(bootFraction, label);
}
