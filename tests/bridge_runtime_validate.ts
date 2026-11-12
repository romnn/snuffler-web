// Exercise the real bridge transport with a deterministic worker and clock.
import assert from "node:assert/strict";
import { create, toBinary } from "@bufbuild/protobuf";
import { WorkerEventSchema } from "../web/proto/snuffler_web_proto/bridge_pb.js";
import { BridgeMethod, type BridgeMethodId } from "../web/rpc.js";
import type { WorkerMessage, WorkerRequest } from "../web/worker_protocol.js";

class Clock {
  now = 0;
  nextId = 1;
  timers = new Map<number, { due: number; callback: () => void }>();

  setTimeout = (callback: () => void, delay: number): number => {
    const id = this.nextId++;
    this.timers.set(id, { due: this.now + delay, callback });
    return id;
  };

  clearTimeout = (id: number): void => {
    this.timers.delete(id);
  };

  advance(milliseconds: number): void {
    const end = this.now + milliseconds;
    for (;;) {
      const next = [...this.timers].sort((left, right) => left[1].due - right[1].due)[0];
      if (!next || next[1].due > end) break;
      const [id, timer] = next;
      this.timers.delete(id);
      this.now = timer.due;
      timer.callback();
    }
    this.now = end;
  }
}

class WorkerStub {
  static instances: WorkerStub[] = [];
  onmessage: ((event: MessageEvent<WorkerMessage>) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  requests: WorkerRequest[] = [];
  terminated = false;
  readonly epoch: number;

  constructor(url: string) {
    this.epoch = Number(new URL(url).searchParams.get("epoch"));
    WorkerStub.instances.push(this);
  }

  postMessage(request: WorkerRequest): void {
    this.requests.push(request);
  }

  terminate(): void {
    this.terminated = true;
  }

  ready(): void {
    this.emit({
      type: "workerEvent",
      epoch: this.epoch,
      event: toBinary(
        WorkerEventSchema,
        create(WorkerEventSchema, {
          runtimeEpoch: this.epoch,
          event: { case: "runtimeInfo", value: { python: "test" } },
        })
      ),
    });
  }

  emit(message: WorkerMessage): void {
    this.onmessage?.({ data: message } as MessageEvent<WorkerMessage>);
  }

  reply(request: WorkerRequest): void {
    this.emit({ epoch: this.epoch, id: request.id, ok: true, result: new ArrayBuffer(0) });
  }
}

const clock = new Clock();
Object.assign(globalThis, {
  window: { setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout },
  document: { baseURI: "https://example.test/snuffler/" },
  Worker: WorkerStub,
});
// Import after installing the browser environment; the bridge captures the initial epoch.
const bridge: {
  call(method: BridgeMethodId, request: Uint8Array, jobId: bigint): Promise<unknown>;
  currentRuntimeEpoch(): number;
} = await import(new URL("../web/bridge.ts", import.meta.url).href);
const currentWorker = (): WorkerStub => {
  const worker = WorkerStub.instances.at(-1);
  assert.ok(worker);
  return worker;
};
let nextJobId = 1n;

{
  // Both non-cancellable methods can be posted while a cold boot exceeds 30 seconds.
  const view = bridge.call(BridgeMethod.getView, new Uint8Array(), nextJobId++);
  const examples = bridge.call(BridgeMethod.listExamples, new Uint8Array(), nextJobId++);
  const worker = currentWorker();
  const epoch = bridge.currentRuntimeEpoch();
  clock.advance(40_000);
  assert.equal(bridge.currentRuntimeEpoch(), epoch);
  assert.equal(worker.terminated, false);
  worker.ready();
  clock.advance(29_999);
  assert.equal(worker.terminated, false);
  for (const request of worker.requests) worker.reply(request);
  await Promise.all([view, examples]);
  clock.advance(60_000);
  assert.equal(bridge.currentRuntimeEpoch(), epoch);
  assert.equal(clock.timers.size, 0, "replies clear both request timers and the boot timer");
}

{
  // A ready worker that fails to answer still times out, even if it repeats runtimeInfo.
  const worker = currentWorker();
  const epoch = bridge.currentRuntimeEpoch();
  const request = bridge.call(BridgeMethod.getView, new Uint8Array(), nextJobId++);
  const failure = assert.rejects(request, /GetView exceeded 30000 ms/);
  clock.advance(20_000);
  worker.ready();
  clock.advance(9_999);
  assert.equal(worker.terminated, false);
  clock.advance(1);
  await failure;
  assert.equal(worker.terminated, true);
  assert.equal(bridge.currentRuntimeEpoch(), epoch + 1);
}

{
  // Replacement epochs wait for their own boot, then get the full response deadline.
  const worker = currentWorker();
  const epoch = bridge.currentRuntimeEpoch();
  const request = bridge.call(BridgeMethod.listExamples, new Uint8Array(), nextJobId++);
  const failure = assert.rejects(request, /ListExamples exceeded 30000 ms/);
  clock.advance(45_000);
  assert.equal(worker.terminated, false);
  worker.ready();
  clock.advance(29_999);
  assert.equal(bridge.currentRuntimeEpoch(), epoch);
  clock.advance(1);
  await failure;
  assert.equal(worker.terminated, true);
}

{
  // Deferring request deadlines must not remove the independent five-minute boot bound.
  const worker = currentWorker();
  const request = bridge.call(BridgeMethod.getView, new Uint8Array(), nextJobId++);
  const failure = assert.rejects(request, /timed out during boot/);
  clock.advance(299_999);
  assert.equal(worker.terminated, false);
  clock.advance(1);
  await failure;
  assert.equal(worker.terminated, true);
}

// The stub has no background worker; clear its remaining synthetic boot timer.
clock.timers.clear();
console.log("all bridge-runtime deadline checks passed");
