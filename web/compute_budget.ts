// One policy for the frontend's rayon workers and Python's OpenMP team.
import { create, toBinary } from "@bufbuild/protobuf";
import { ComputeBudgetSchema, type ComputeBudget } from "./proto/snuffler_web_proto/bridge_pb.js";

export function splitComputeBudget(hardwareConcurrency: number | undefined): ComputeBudget {
  const cores =
    typeof hardwareConcurrency === "number" &&
    Number.isFinite(hardwareConcurrency) &&
    hardwareConcurrency >= 1
      ? Math.floor(hardwareConcurrency)
      : 2;
  // Reserve a core for the browser above two cores; keep rayon available on dual-core devices.
  // Cap startup costs on large machines. OpenMP teams larger than four are rarely useful here.
  // OpenMP includes the Python worker itself; rayon counts additional workers.
  // 2026-11-12, Threadripper 9960X, Chromium, median of five warm runs:
  // OMP 2/4: filtered 18-track, 600 s views 12.6/11.4 ms; PhaseNet/instance 420/397 ms.
  // These mostly serial paths do not establish OpenMP scaling (ORT uses one CPU thread).
  // Reproduce with task bench:compute:browser against the served threaded build.
  const total = Math.min(8, cores > 2 ? cores - 1 : cores);
  const openmpThreads = Math.min(4, Math.ceil(total / 2));
  return create(ComputeBudgetSchema, { total, openmpThreads, rayonThreads: total - openmpThreads });
}

export const COMPUTE_BUDGET = splitComputeBudget(globalThis.navigator?.hardwareConcurrency);

/** The same startup budget as the worker, serialized for the Rust frontend. */
export function computeBudget(): Uint8Array {
  return toBinary(ComputeBudgetSchema, COMPUTE_BUDGET);
}
