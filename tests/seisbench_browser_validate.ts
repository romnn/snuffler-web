// Validate SeisBench ONNX inference end to end in the served wasm app: every model
// in the manifest is run against the synthetic example scene, on WebGPU where the
// browser offers it and again on the CPU (wasm) fallback.
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { chromium, type Browser, type Page } from "playwright";
import { readFileSync } from "node:fs";

import {
  BridgeResultSchema,
  EmptySchema,
  MlDevice,
  MlDeviceStatusSchema,
  RuntimeInfoSchema,
  WorkerEventSchema,
  type BridgeResult,
} from "../web/proto/snuffler_web_proto/bridge_pb.js";
import {
  NamedSnufflingRequestSchema,
  SeisbenchSnufflingParamsSchema,
  SnufflingParamsSchema,
  SnufflingRequestParamsSchema,
} from "../web/proto/snuffler_web_proto/snuffling_pb.js";
import { ViewParamsSchema } from "../web/proto/snuffler_web_proto/view_pb.js";

// The oneof cases of `BridgeResult.result`, and the payload each one carries —
// derived from the generated protobuf types so a schema change fails the
// typecheck here rather than surfacing as an undefined field at runtime.
type ResultCase = NonNullable<BridgeResult["result"]["case"]>;
type ResultValue<C extends ResultCase> = Extract<BridgeResult["result"], { case: C }>["value"];
type Scene = ResultValue<"scene">;

interface ModelSpec {
  model: string;
  weights: string;
}

// What the bundled bridge module (web/bridge.ts -> web/gen/bridge.js) exposes of
// itself to this harness.
interface BridgeModule {
  call(method: string, request: Uint8Array, jobId: bigint): Promise<{ result: Uint8Array }>;
  seisbenchDevice(): Uint8Array;
  takeOperationEvents(): Uint8Array[];
  runtimeInfo(): Uint8Array | null;
}

interface CallReply {
  result: number[];
  device: string;
  progress: number;
  progressLabel: string;
  eventDebug: string[];
}

interface ModelResult {
  model: string;
  markers: number;
  phases: Record<string, number>;
  text: string;
  device: string;
  progressLabel: string;
}

interface RunResult {
  hasWebgpu: boolean;
  consoleEntries: string[];
  results: ModelResult[];
}

const ROOT = new URL("..", import.meta.url);
const manifest = JSON.parse(
  readFileSync(new URL("web/assets/seisbench_models.json", ROOT), "utf8")
) as { models: ModelSpec[] };

const BASE_URL = process.env.SNUFFLER_BROWSER_URL || "http://127.0.0.1:8000/";
const MODEL_BASE_URL = process.env.SNUFFLER_ONNX_MODEL_BASE_URL || "assets/models/";
const HEADLESS = process.env.SNUFFLER_BROWSER_HEADLESS !== "0";

// `?demo=1` appended to whatever base URL was configured, which may already carry a
// query string.
function demoUrl(base: string): string {
  const url = new URL(base);
  url.searchParams.set("demo", "1");
  return url.href;
}

const TIMEOUT_MS = Number(process.env.SNUFFLER_BROWSER_TEST_TIMEOUT_MS || 300_000);
const PICK_THRESHOLD = Number(process.env.SNUFFLER_BROWSER_PICK_THRESHOLD || 0.02);
let benchmarkOpenmpThreads: 2 | 4 | null = null;

const METHODS = {
  makeExample: "/snuffler_web_proto.BridgeService/MakeExample",
  runSnuffling: "/snuffler_web_proto.BridgeService/RunSnuffling",
  getView: "/snuffler_web_proto.BridgeService/GetView",
};

// Bytes as a plain array, the form `page.evaluate` can carry across into the page.
function serializableBytes(bytes: Uint8Array): number[] {
  return Array.from(bytes);
}

function bridgeResult<C extends ResultCase>(
  bytes: number[],
  label: string,
  expectedCase: C
): ResultValue<C> {
  const envelope = fromBinary(BridgeResultSchema, new Uint8Array(bytes));
  if (envelope.result.case === "error") {
    const error = envelope.result.value;
    throw new Error(`${label}: ${error.kind}: ${error.message}\n${error.traceback}`);
  }
  if (envelope.result.case !== expectedCase) {
    throw new Error(`${label}: expected ${expectedCase}, got ${envelope.result.case ?? "empty"}`);
  }
  // The guard above proves the case matches.
  // TypeScript cannot narrow a union by comparison against a generic parameter, so
  // the cast carries the proof.
  return envelope.result.value as ResultValue<C>;
}

// A SeisBench run over a 200 s window of the synthetic scene.
function seisbenchRequest(scene: Scene, spec: ModelSpec) {
  const centerTmin = scene.tmin + 145.0;
  const centerTmax = scene.tmin + 345.0;
  const view = create(ViewParamsSchema, {
    tmin: Math.max(scene.tmin, centerTmin),
    tmax: Math.min(scene.tmax, centerTmax),
    binsPerTrace: 1400,
  });
  const seisbench = create(SeisbenchSnufflingParamsSchema, {
    model: spec.model,
    weights: spec.weights,
    pThreshold: PICK_THRESHOLD,
    sThreshold: PICK_THRESHOLD,
    modelBaseUrl: MODEL_BASE_URL,
  });
  return create(NamedSnufflingRequestSchema, {
    name: "seisbench_picker",
    params: create(SnufflingRequestParamsSchema, {
      view,
      snufflings: create(SnufflingParamsSchema, { seisbench }),
    }),
  });
}

function browserArgs(disableWebgpu: boolean): string[] {
  const args = [
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-search-engine-choice-screen",
  ];
  if (disableWebgpu) {
    // WebGPU is stable in current Chromium, so the interface itself cannot be
    // removed (--disable-webgpu / --disable-features=WebGPU are silently ignored).
    // What the app actually branches on is `requestAdapter()` handing back an
    // adapter, so deny the adapter instead.
    // With the GPU process disabled it resolves null, in workers too, where the
    // bridge makes its device choice, and the CPU (wasm) provider wins.
    args.push("--disable-gpu");
    return args;
  }
  args.push(
    "--enable-unsafe-webgpu",
    "--enable-features=WebGPUDeveloperFeatures",
    "--ignore-gpu-blocklist",
    "--use-angle=gl"
  );
  return args;
}

async function bridgeModuleUrl(page: Page): Promise<string> {
  return await page.evaluate(() => {
    // Prefer whatever the page itself preloads, but the plain path is a stable
    // contract on its own.
    // The wasm externs import the bundle by exactly this URL (see the module path
    // notes in scripts/trunk-post-build.sh), so it is always staged there.
    // Trunk only sometimes emits a modulepreload for it, depending on how it treats
    // the snippet import.
    const links = Array.from(
      document.querySelectorAll<HTMLLinkElement>('link[rel="modulepreload"]')
    );
    const href = links
      .map((link) => link.href)
      .find((value) => value.includes("/web/gen/bridge.js"));
    return href ?? new URL("web/gen/bridge.js", document.baseURI).href;
  });
}

async function callBridge(
  page: Page,
  moduleUrl: string,
  method: string,
  requestBytes: Uint8Array
): Promise<CallReply> {
  const payload = {
    moduleUrl,
    method,
    request: serializableBytes(requestBytes),
  };
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      const evaluated = await page.evaluate(
        async ({ moduleUrl: href, method: rpcMethod, request }) => {
          const bridge = (await import(href)) as BridgeModule;
          bridge.takeOperationEvents();
          const reply = await bridge.call(rpcMethod, new Uint8Array(request), 999n);
          return {
            result: Array.from(reply.result),
            device: Array.from(bridge.seisbenchDevice()),
            // This harness stubs the egui module and owns the bridge alone.
            // Draining here therefore yields exactly the events this call produced.
            events: bridge.takeOperationEvents().map((bytes) => Array.from(bytes)),
          };
        },
        payload
      );
      const workerEvents = evaluated.events.map((bytes) =>
        fromBinary(WorkerEventSchema, new Uint8Array(bytes))
      );
      const progressEvents = workerEvents
        .flatMap((worker) =>
          worker.event.case === "operation" && worker.event.value.jobId === 999n
            ? [worker.event.value]
            : []
        )
        .flatMap((operation) =>
          operation.event.case === "progress" ? [operation.event.value] : []
        );
      const progress = progressEvents.at(-1);
      const device = fromBinary(MlDeviceStatusSchema, new Uint8Array(evaluated.device));
      return {
        result: evaluated.result,
        device: device.device === MlDevice.WEBGPU ? "WebGPU" : "CPU (wasm)",
        progress: progress?.progress.case === "fraction" ? progress.progress.value : -1,
        progressLabel: progress?.phase ?? "",
        eventDebug: workerEvents.map((worker) => worker.event.case ?? "empty"),
      };
    } catch (error) {
      if (!String(error).includes("Execution context was destroyed") || attempt === 2) throw error;
      await page.waitForLoadState("domcontentloaded");
      await page.waitForTimeout(500);
    }
  }
  throw new Error("unreachable bridge call retry state");
}

async function webgpuAvailable(page: Page): Promise<boolean> {
  return await page.evaluate(async () => {
    try {
      const gpuNavigator = navigator as { gpu?: { requestAdapter(): Promise<unknown> } };
      if (!gpuNavigator.gpu) return false;
      return Boolean(await gpuNavigator.gpu.requestAdapter());
    } catch {
      return false;
    }
  });
}

// Whether a view request completes while a SeisBench run still holds its job, which
// proves the run yields the interpreter between batches.
async function viewCompletesDuringRun(
  page: Page,
  moduleUrl: string,
  runRequest: Uint8Array,
  viewRequest: Uint8Array
): Promise<boolean> {
  return await page.evaluate(
    async ({ href, runMethod, viewMethod, runBytes, viewBytes }) => {
      const bridge = (await import(href)) as BridgeModule;
      let runFinished = false;
      const run = bridge.call(runMethod, new Uint8Array(runBytes), 2001n).finally(() => {
        runFinished = true;
      });
      await new Promise((resolve) => setTimeout(resolve, 25));
      await bridge.call(viewMethod, new Uint8Array(viewBytes), 2002n);
      const viewFinishedFirst = !runFinished;
      await run;
      return viewFinishedFirst;
    },
    {
      href: moduleUrl,
      runMethod: METHODS.runSnuffling,
      viewMethod: METHODS.getView,
      runBytes: serializableBytes(runRequest),
      viewBytes: serializableBytes(viewRequest),
    }
  );
}

// Messages the page logged, all of them for the provider evidence and the errors to
// fail the pass on.
interface ConsoleLog {
  entries: string[];
  errors: string[];
}

async function openHarnessPage(browser: Browser, log: ConsoleLog): Promise<Page> {
  const page = await browser.newPage();
  if (benchmarkOpenmpThreads !== null) {
    const cores = benchmarkOpenmpThreads === 2 ? 5 : 9;
    await page.addInitScript((cores) => {
      Object.defineProperty(navigator, "hardwareConcurrency", { value: cores });
    }, cores);
    await page.route("**/bridge_worker.js?*", async (route) => {
      const response = await route.fetch();
      await route.fulfill({
        response,
        body: `Object.defineProperty(navigator, "hardwareConcurrency", {value: ${cores}});\n${await response.text()}`,
      });
    });
  }
  // This harness drives the transport directly, so the egui app must not submit its
  // own ledger-owned views into the same worker.
  await page.route(/\/snuffler-web-egui-[^/]+\.js$/, async (route) => {
    await route.fulfill({
      contentType: "text/javascript",
      body: "export default async function init() { return {}; }\n",
    });
  });
  page.setDefaultTimeout(TIMEOUT_MS);
  page.setDefaultNavigationTimeout(60_000);
  page.on("console", (message) => {
    const text = message.text();
    log.entries.push(`[${message.type()}] ${text}`);
    if (message.type() === "error") log.errors.push(text);
  });
  page.on("pageerror", (error) => {
    log.errors.push(`pageerror: ${error.message}`);
  });

  // `?demo=1` keeps the boot load synthetic.
  // The scene under test is built by calling MakeExample, so what the UI holds is
  // irrelevant, but the app opens on a recorded dataset by default, and fetching
  // megabytes of waveforms would take CPU away from the inference.
  await page.goto(demoUrl(BASE_URL), { waitUntil: "domcontentloaded" });
  await page.waitForLoadState("load");
  await page.waitForTimeout(500);
  return page;
}

// Run one model over the synthetic scene and check what came back.
async function runModel(
  page: Page,
  moduleUrl: string,
  spec: ModelSpec,
  probeConcurrentView: boolean
): Promise<ModelResult> {
  const modelName = `${spec.model}/${spec.weights}`;

  // Synthetic scene
  const sceneReply = await callBridge(
    page,
    moduleUrl,
    METHODS.makeExample,
    toBinary(EmptySchema, create(EmptySchema))
  );
  const scene = bridgeResult(sceneReply.result, "MakeExample", "scene");
  if (scene.trackCount < 18n) {
    throw new Error(`MakeExample: expected at least 18 traces, got ${scene.trackCount}`);
  }

  // A view must not wait for the whole run
  const request = seisbenchRequest(scene, spec);
  if (probeConcurrentView) {
    const view = request.params?.view;
    if (!view) throw new Error("SeisBench request has no view params");
    const viewFinishedFirst = await viewCompletesDuringRun(
      page,
      moduleUrl,
      toBinary(NamedSnufflingRequestSchema, request),
      toBinary(ViewParamsSchema, view)
    );
    if (!viewFinishedFirst) {
      throw new Error("GetView did not complete while the SeisBench job yielded the interpreter");
    }
  }

  // The run itself
  const reply = await callBridge(
    page,
    moduleUrl,
    METHODS.runSnuffling,
    toBinary(NamedSnufflingRequestSchema, request)
  );
  const result = bridgeResult(reply.result, `${modelName} RunSnuffling`, "snuffling");
  if (reply.progress < 0.99) {
    throw new Error(
      `${modelName}: final progress ${reply.progress}, label "${reply.progressLabel}", events ${reply.eventDebug.join(",")}`
    );
  }
  if (!reply.progressLabel.includes("complete")) {
    throw new Error(`${modelName}: final progress label did not complete: ${reply.progressLabel}`);
  }
  if (!reply.device || !["WebGPU", "CPU (wasm)"].includes(reply.device)) {
    throw new Error(`${modelName}: invalid device label "${reply.device}"`);
  }
  if (result.text.includes("no ZNE data")) {
    throw new Error(`${modelName}: did not run on the synthetic ZNE scene`);
  }
  const syntheticShouldProduceMarkers = spec.model === "PhaseNet";
  if (syntheticShouldProduceMarkers && result.markers.length === 0) {
    throw new Error(`${modelName}: no browser markers produced`);
  }

  const phases = result.markers.reduce<Record<string, number>>((counts, marker) => {
    const phase = marker.phase || "?";
    counts[phase] = (counts[phase] || 0) + 1;
    return counts;
  }, {});
  return {
    model: modelName,
    markers: result.markers.length,
    phases,
    text: result.text,
    device: reply.device,
    progressLabel: reply.progressLabel,
  };
}

async function runBrowserPass({
  disableWebgpu,
  models,
}: {
  disableWebgpu: boolean;
  models: ModelSpec[];
}): Promise<RunResult> {
  const browser = await chromium.launch({
    headless: HEADLESS,
    args: browserArgs(disableWebgpu),
  });
  const log: ConsoleLog = { entries: [], errors: [] };
  const results: ModelResult[] = [];
  let hasWebgpu = false;

  try {
    for (const spec of models) {
      const page = await openHarnessPage(browser, log);
      try {
        const moduleUrl = await bridgeModuleUrl(page);
        hasWebgpu = (await webgpuAvailable(page)) || hasWebgpu;
        results.push(await runModel(page, moduleUrl, spec, spec === models[0]));
      } finally {
        await page.close();
      }
    }

    if (log.errors.length > 0) {
      throw new Error(`browser console errors:\n${log.errors.join("\n")}`);
    }

    return { hasWebgpu, consoleEntries: log.entries, results };
  } finally {
    await browser.close();
  }
}

function requireProviderEvidence(run: RunResult, providerText: string): void {
  if (run.consoleEntries.some((entry) => entry.includes(providerText))) return;
  throw new Error(`missing ORT provider evidence: ${providerText}`);
}

function printResults(title: string, run: RunResult): void {
  console.log(title);
  console.log(`  webgpu_available=${run.hasWebgpu}`);
  for (const result of run.results) {
    console.log(
      `  ${result.model}: ${result.markers} marker(s), phases=${JSON.stringify(
        result.phases
      )}, device=${result.device}, ${result.text}`
    );
  }
}

async function benchmarkCompute(): Promise<void> {
  const model = manifest.models.find((spec) => spec.model === "PhaseNet");
  if (!model) throw new Error("benchmark requires PhaseNet");
  const median = (values: number[]) =>
    [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
  const browser = await chromium.launch({ headless: HEADLESS, args: browserArgs(true) });
  try {
    for (const threads of [2, 4] as const) {
      benchmarkOpenmpThreads = threads;
      const log: ConsoleLog = { entries: [], errors: [] };
      const page = await openHarnessPage(browser, log);
      try {
        const moduleUrl = await bridgeModuleUrl(page);
        const scene = bridgeResult(
          (
            await callBridge(
              page,
              moduleUrl,
              METHODS.makeExample,
              toBinary(EmptySchema, create(EmptySchema))
            )
          ).result,
          "MakeExample",
          "scene"
        );
        const runtimeBytes = await page.evaluate(async (href) => {
          const bridge = (await import(href)) as BridgeModule;
          return Array.from(bridge.runtimeInfo() ?? []);
        }, moduleUrl);
        const runtime = fromBinary(RuntimeInfoSchema, new Uint8Array(runtimeBytes));
        if (runtime.computeBudget?.openmpThreads !== threads || !runtime.threaded) {
          throw new Error(`benchmark did not boot a threaded ${threads}-thread OpenMP budget`);
        }
        const filterMs: number[] = [];
        const seisbenchMs: number[] = [];
        for (let iteration = 0; iteration < 6; iteration += 1) {
          // Change the passband so no cached processed cutout can satisfy this run.
          const view = create(ViewParamsSchema, {
            tmin: scene.tmin,
            tmax: scene.tmax,
            binsPerTrace: 1400,
            highpass: 0.5,
            lowpass: 4 + iteration * 0.01,
          });
          const filterStart = performance.now();
          bridgeResult(
            (await callBridge(page, moduleUrl, METHODS.getView, toBinary(ViewParamsSchema, view)))
              .result,
            "filtered GetView",
            "view"
          );
          const filterElapsed = performance.now() - filterStart;
          const seisbenchStart = performance.now();
          const result = bridgeResult(
            (
              await callBridge(
                page,
                moduleUrl,
                METHODS.runSnuffling,
                toBinary(NamedSnufflingRequestSchema, seisbenchRequest(scene, model))
              )
            ).result,
            "PhaseNet",
            "snuffling"
          );
          const seisbenchElapsed = performance.now() - seisbenchStart;
          if (!result.markers.length) throw new Error("benchmark produced no PhaseNet picks");
          // The first iteration warms Python imports and the ONNX session.
          if (iteration > 0) {
            filterMs.push(filterElapsed);
            seisbenchMs.push(seisbenchElapsed);
          }
        }
        if (log.errors.length) throw new Error(log.errors.join("\n"));
        console.log(
          JSON.stringify({
            openmp: threads,
            rayon: runtime.computeBudget.rayonThreads,
            traces: String(scene.trackCount),
            duration: scene.tmax - scene.tmin,
            model: `${model.model}/${model.weights}`,
            provider: "CPU wasm (1 thread)",
            filterMedianMs: median(filterMs),
            seisbenchMedianMs: median(seisbenchMs),
            filterMs,
            seisbenchMs,
          })
        );
      } finally {
        await page.close();
      }
    }
  } finally {
    await browser.close();
  }
}

if (process.env.SNUFFLER_BENCH_COMPUTE === "1") {
  await benchmarkCompute();
  process.exit(0);
}

const requestedModels = (process.env.SNUFFLER_BROWSER_MODELS || "")
  .split(",")
  .map((name) => name.trim())
  .filter(Boolean);
const models =
  requestedModels.length === 0
    ? manifest.models
    : manifest.models.filter(
        (spec) =>
          requestedModels.includes(`${spec.model}/${spec.weights}`) ||
          requestedModels.includes(spec.model)
      );
if (!Array.isArray(models) || models.length === 0) {
  throw new Error("seisbench manifest has no models");
}

const webgpuRun = await runBrowserPass({ disableWebgpu: false, models });
printResults("browser WebGPU/default pass", webgpuRun);
if (webgpuRun.hasWebgpu) {
  requireProviderEvidence(webgpuRun, "executionProviders=webgpu,wasm");
  for (const result of webgpuRun.results) {
    if (result.device !== "WebGPU") {
      throw new Error(`${result.model}: expected WebGPU device, got ${result.device}`);
    }
  }
  const wasmRun = await runBrowserPass({ disableWebgpu: true, models: [models[0]] });
  printResults("browser CPU fallback pass", wasmRun);
  requireProviderEvidence(wasmRun, "executionProviders=wasm");
  for (const result of wasmRun.results) {
    if (result.device !== "CPU (wasm)") {
      throw new Error(`${result.model}: expected CPU (wasm) fallback, got ${result.device}`);
    }
  }
} else {
  requireProviderEvidence(webgpuRun, "executionProviders=wasm");
  console.log("browser CPU fallback pass covered by default pass (WebGPU unavailable)");
}
