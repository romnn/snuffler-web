// Drive the activity lifecycle through the served app and its real Pyodide worker.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import type { Page } from "playwright";
import {
  BridgeResultSchema,
  CancelJobRequestSchema,
  WorkerEventSchema,
} from "../web/proto/snuffler_web_proto/bridge_pb.js";
import { CommandRequestSchema } from "../web/proto/snuffler_web_proto/data_pb.js";
import { FigureRequestSchema } from "../web/proto/snuffler_web_proto/figure_pb.js";
import { BridgeMethod, type BridgeMethodId } from "../web/rpc.js";
import {
  appUrl,
  BOOT_TIMEOUT_MS,
  launchParityBrowser,
  waitForAppReady,
} from "./parity/parity_browser";

interface WireRequest {
  epoch: number;
  id: number;
  method: string;
  request: number[];
}
interface WireReply {
  epoch: number;
  id: number;
  result: number[];
}
interface ActivityProbe {
  requests: WireRequest[];
  replies: WireReply[];
  events: number[][];
  workers: Worker[];
  calls: Record<
    string,
    { settled: boolean; completions: number; result?: number[]; error?: string }
  >;
  directoryWaiting?: boolean;
  releaseDirectory?: () => void;
  retiredHandler?: Worker["onmessage"];
}
interface BrowserBridge {
  call(method: BridgeMethodId, request: Uint8Array, jobId: bigint): Promise<{ result: Uint8Array }>;
  cancelJob(request: Uint8Array): Uint8Array;
  currentRuntimeEpoch(): number;
  restartRuntime(reason: string): number;
  runtimeInfo(): Uint8Array | null;
}
declare global {
  interface Window {
    __activityProbe: ActivityProbe;
  }
}

// This fixture changes handlers only in the test browser's downloaded copy.
// The interrupt probe executes bytecode without cooperative safe points.
const pythonFixture = `
_probe_get_view = BridgeServicer.get_view
_probe_run_command = BridgeServicer.run_command
_probe_view_failed = False
async def _test_get_view(self, request, data):
    global _probe_view_failed
    if not _probe_view_failed:
        _probe_view_failed = True
        raise RuntimeError("injected transient view failure")
    return await _probe_get_view(self, request, data)
async def _test_run_command(self, request, data):
    if request.line == "activity-interrupt-probe":
        import time
        operation.report_progress(0, 1, "activity interrupt probe entered")
        deadline = time.monotonic() + 15.0
        value = 0
        while time.monotonic() < deadline:
            value = (value + 1) % 1000003
        return data_pb2.CommandResult(action="probe completed")
    if request.line == "activity-crash-probe":
        import asyncio
        operation.report_progress(0, 1, "activity crash probe entered")
        await operation.await_external(asyncio.sleep(30))
        return data_pb2.CommandResult(action="probe completed")
    return _probe_run_command(self, request, data)
BridgeServicer.get_view = _test_get_view
BridgeServicer.run_command = _test_run_command
`;

async function installProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe: ActivityProbe = {
      requests: [],
      replies: [],
      events: [],
      workers: [],
      calls: {},
    };
    window.__activityProbe = probe;
    window.Worker = new Proxy(window.Worker, {
      construct(Target, args) {
        const worker = Reflect.construct(Target, args) as Worker;
        if (String(args[0]).includes("bridge_worker.js")) {
          probe.workers.push(worker);
          const post = worker.postMessage;
          Object.defineProperty(worker, "postMessage", {
            value(message: WireRequest & { request?: Uint8Array }, transfer: Transferable[]) {
              if (typeof message.method === "string") {
                probe.requests.push({
                  epoch: message.epoch,
                  id: message.id,
                  method: message.method,
                  request: Array.from(message.request ?? []),
                });
              }
              return Reflect.apply(post, worker, [message, transfer]);
            },
          });
          worker.addEventListener("message", (event) => {
            const message = event.data;
            if (message.type === "workerEvent") probe.events.push(Array.from(message.event));
            else if (message.ok)
              probe.replies.push({
                epoch: message.epoch,
                id: message.id,
                result: Array.from(new Uint8Array(message.result)),
              });
          });
        }
        return worker;
      },
    });
  });
  await page.route("**/snuffler_bridge/service.py", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: (await response.text()) + pythonFixture });
  });
}

async function activate(page: Page, name: string): Promise<void> {
  await page
    .getByRole("button", { name, exact: true })
    .last()
    .evaluate((element) => {
      (element as HTMLElement).click();
    });
  await page.waitForTimeout(100);
}

async function startCall(page: Page, name: string, line: string, jobId: string): Promise<void> {
  const request = toBinary(CommandRequestSchema, create(CommandRequestSchema, { line }));
  await page.evaluate(
    async ({ name, request, jobId }) => {
      const bridge: BrowserBridge = await import(
        new URL("web/gen/bridge.js", document.baseURI).href
      );
      const state = { settled: false, completions: 0 } as ActivityProbe["calls"][string];
      window.__activityProbe.calls[name] = state;
      void bridge
        .call(
          "/snuffler_web_proto.BridgeService/RunCommand",
          new Uint8Array(request),
          BigInt(jobId)
        )
        .then((reply) => {
          state.result = Array.from(reply.result);
        })
        .catch((error: Error) => {
          state.error = error.message;
        })
        .finally(() => {
          state.settled = true;
          state.completions += 1;
        });
    },
    { name, request: Array.from(request), jobId }
  );
}

async function waitForPhase(page: Page, jobId: bigint, phase: string): Promise<void> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    const events = await page.evaluate(() => window.__activityProbe.events);
    if (
      events.some((bytes) => {
        const event = fromBinary(WorkerEventSchema, new Uint8Array(bytes)).event;
        return (
          event.case === "operation" &&
          event.value.jobId === jobId &&
          event.value.event.case === "progress" &&
          event.value.event.value.phase === phase
        );
      })
    )
      return;
    await page.waitForTimeout(50);
  }
  throw new Error(`Python never entered ${phase}`);
}

async function checkInterrupt(page: Page): Promise<void> {
  await startCall(page, "interrupt", "activity-interrupt-probe", "900001");
  await waitForPhase(page, 900001n, "activity interrupt probe entered");
  const request = toBinary(
    CancelJobRequestSchema,
    create(CancelJobRequestSchema, { jobId: 900001n })
  );
  await page.evaluate(async (bytes) => {
    const bridge: BrowserBridge = await import(new URL("web/gen/bridge.js", document.baseURI).href);
    bridge.cancelJob(new Uint8Array(bytes));
  }, Array.from(request));
  await page.waitForFunction(() => window.__activityProbe.calls.interrupt?.settled, undefined, {
    timeout: 5000,
  });
  const result = await page.evaluate(() => window.__activityProbe.calls.interrupt);
  assert.equal(result.error, undefined);
  assert.ok(result.result);
  assert.equal(
    fromBinary(BridgeResultSchema, new Uint8Array(result.result)).result.case,
    "cancelled"
  );
  assert.equal(result.completions, 1);
  console.log("ok   cancellation interrupts bytecode in the real Python worker");
}

async function checkCompositeCancellation(page: Page): Promise<void> {
  const waveform = await readFile(
    new URL(
      "../examples/sumatra-2004-12-26-m9.1-surface-waves/sumatra-2004-12-26-m9.1-surface-waves.mseed",
      import.meta.url
    )
  );
  let blockette = waveform.readUInt16BE(46);
  while (waveform.readUInt16BE(blockette) !== 1000) {
    blockette = waveform.readUInt16BE(blockette + 2);
    assert.ok(blockette, "the fixture must contain a record-length blockette");
  }
  const record = waveform.subarray(0, 2 ** waveform[blockette + 6]);
  await page.evaluate((content) => {
    const probe = window.__activityProbe;
    const gate = new Promise<void>((resolve) => {
      probe.releaseDirectory = resolve;
    });
    Object.defineProperty(window, "showDirectoryPicker", {
      configurable: true,
      value: async () => ({
        async *entries() {
          for (let index = 0; index < 65; index += 1) {
            if (index === 64) {
              probe.directoryWaiting = true;
              await gate;
            }
            const name = `activity-${index}.mseed`;
            yield [
              name,
              {
                kind: "file",
                getFile: async () => new File([new Uint8Array(content)], name, { lastModified: 1 }),
              },
            ];
          }
        },
      }),
    });
  }, Array.from(record));
  await activate(page, "File");
  await activate(page, "Open waveform directory…");
  await page.waitForFunction(() => window.__activityProbe.directoryWaiting === true);
  await activate(page, "Cancel");
  await page.evaluate(() => window.__activityProbe.releaseDirectory?.());
  await page.getByText(/Opening directory stopped: [1-9]\d* committed/).waitFor();
  await waitForAppReady(page);
  console.log("ok   cancelling a composite load retains its committed prefix and reports Stopped");
}

async function checkFigures(page: Page): Promise<void> {
  for (const name of ["Amplitude spectrum", "Spectrogram (figure)"]) {
    const before = await page.evaluate(() => window.__activityProbe.requests.length);
    await activate(page, "Snufflings");
    await activate(page, name);
    const panel = page.getByRole("dialog", {
      name: name === "Amplitude spectrum" ? "Plot Amplitude Spectrum" : "Plot Spectrogram",
      exact: true,
    });
    const run = panel.getByRole("button", { name: "Run", exact: true });
    await run.waitFor();
    await page.waitForTimeout(250);
    const opened = await page.evaluate(
      (start) => window.__activityProbe.requests.slice(start),
      before
    );
    const kind = name === "Amplitude spectrum" ? "ampspec" : "spectrogram";
    assert.ok(
      opened.every(
        (request) =>
          request.method !== BridgeMethod.renderFigure ||
          fromBinary(FigureRequestSchema, new Uint8Array(request.request)).kind !== kind
      )
    );
    await run.evaluate((element) => (element as HTMLElement).click());
    await page.waitForTimeout(100);
  }
  await page.waitForFunction(
    () => {
      const probe = window.__activityProbe;
      const figures = probe.requests.filter((request) => request.method.endsWith("/RenderFigure"));
      return (
        figures.length >= 2 &&
        figures.every((request) =>
          probe.replies.some((reply) => reply.epoch === request.epoch && reply.id === request.id)
        )
      );
    },
    undefined,
    { timeout: 60_000 }
  );
  const records = await page.evaluate(() => window.__activityProbe);
  const kinds = new Set<string>();
  for (const request of records.requests.filter(
    (request) => request.method === BridgeMethod.renderFigure
  )) {
    const figure = fromBinary(FigureRequestSchema, new Uint8Array(request.request));
    const reply = records.replies.find(
      (reply) => reply.epoch === request.epoch && reply.id === request.id
    );
    assert.ok(reply);
    const result = fromBinary(BridgeResultSchema, new Uint8Array(reply.result)).result;
    assert.equal(result.case, "figure");
    if (result.case === "figure") assert.ok(result.value.png.length > 0, result.value.error);
    kinds.add(figure.kind);
  }
  assert.ok(kinds.has("ampspec") && kinds.has("spectrogram"));
  console.log("ok   opening panels is idle; Run renders two distinct figure kinds");
}

async function checkWorkerDeath(page: Page): Promise<void> {
  await startCall(page, "crash", "activity-crash-probe", "900002");
  await waitForPhase(page, 900002n, "activity crash probe entered");
  const previous = await page.evaluate(async () => {
    window.__activityProbe.retiredHandler = window.__activityProbe.workers.at(-1)?.onmessage;
    const bridge: BrowserBridge = await import(new URL("web/gen/bridge.js", document.baseURI).href);
    return bridge.currentRuntimeEpoch();
  });
  const worker = page.workers().find((worker) => worker.url().includes("bridge_worker.js"));
  assert.ok(worker);
  await worker.evaluate(() => {
    setTimeout(() => {
      throw new Error("injected worker crash");
    }, 0);
  });
  await page.waitForFunction(() => window.__activityProbe.calls.crash?.settled);
  // This transport probe has no Rust ledger job to repaint on completion.
  // Wake a frame so the idle frontend observes the changed runtime epoch.
  await page.mouse.move(900, 400);
  assert.equal((await page.evaluate(() => window.__activityProbe.calls.crash)).completions, 1);
  await page.waitForFunction(
    async (epoch) => {
      const bridge: BrowserBridge = await import(
        new URL("web/gen/bridge.js", document.baseURI).href
      );
      return bridge.currentRuntimeEpoch() > epoch && bridge.runtimeInfo() !== null;
    },
    previous,
    { timeout: BOOT_TIMEOUT_MS }
  );
  // A queued old-epoch reply must not resolve a request belonging to the replacement.
  await page.evaluate((epoch) => {
    const probe = window.__activityProbe;
    probe.retiredHandler?.call(probe.workers[0], {
      data: {
        epoch,
        id: 1,
        ok: true,
        result: new ArrayBuffer(0),
      },
    } as MessageEvent);
  }, previous);
  await page.waitForFunction(
    (epoch) =>
      window.__activityProbe.requests.some(
        (request) => request.epoch > epoch && request.method.endsWith("/RenderFigure")
      ),
    previous,
    { timeout: 30_000 }
  );
  console.log("ok   worker death settles once, ignores old replies, and retries open figures");
}

const browser = await launchParityBrowser();
let page: Page | undefined;
try {
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  page.setDefaultTimeout(BOOT_TIMEOUT_MS);
  page.on("pageerror", (error) => console.error("browser error:", error.message));
  page.on("console", (message) => {
    if (message.type() === "error") console.error("browser console:", message.text());
  });
  await installProbe(page);
  const loaded = await page.goto(
    appUrl({ a11y: "pointer", example: "sumatra-2004-12-26-m9.1-surface-waves" })
  );
  assert.ok(loaded?.ok(), `app navigation failed with HTTP ${loaded?.status()}`);
  await page.getByText("Importing pyrocko...", { exact: true }).waitFor({ state: "hidden" });
  await waitForAppReady(page);
  const replies = await page.evaluate(() => window.__activityProbe.replies);
  assert.ok(
    replies.some(
      (reply) =>
        fromBinary(BridgeResultSchema, new Uint8Array(reply.result)).result.case === "error"
    )
  );
  assert.ok(
    replies.some(
      (reply) => fromBinary(BridgeResultSchema, new Uint8Array(reply.result)).result.case === "view"
    )
  );
  assert.equal(await page.getByText(/injected transient view failure/).count(), 0);
  console.log("ok   a transient view failure recovers and clears its error");
  await checkInterrupt(page);
  await checkCompositeCancellation(page);
  await checkFigures(page);
  await checkWorkerDeath(page);
  console.log("all real-worker activity checks passed");
} catch (error) {
  if (page) {
    console.error(
      "activity diagnostics:",
      await page
        .evaluate(() => ({
          text: document.body.innerText.slice(-5000),
          probe: {
            requests: window.__activityProbe.requests.length,
            replies: window.__activityProbe.replies.length,
            events: window.__activityProbe.events.length,
          },
        }))
        .catch(() => "page unavailable")
    );
  }
  throw error;
} finally {
  await browser.close();
}
