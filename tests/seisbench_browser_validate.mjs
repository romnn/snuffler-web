import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { chromium } from "playwright";
import { readFileSync } from "node:fs";

import { BridgeResultSchema, EmptySchema } from "../web/proto/snuffler/bridge/v1/bridge_pb.ts";
import {
  NamedSnufflingRequestSchema,
  SeisbenchSnufflingParamsSchema,
  SnufflingParamsSchema,
  SnufflingRequestParamsSchema,
} from "../web/proto/snuffler/bridge/v1/snuffling_pb.ts";
import { ViewParamsSchema } from "../web/proto/snuffler/bridge/v1/view_pb.ts";

const ROOT = new URL("..", import.meta.url);
const manifest = JSON.parse(
  readFileSync(new URL("web/assets/seisbench_models.json", ROOT), "utf8")
);

const BASE_URL = process.env.SNUFFLER_BROWSER_URL || "http://127.0.0.1:17683/";
const MODEL_BASE_URL = process.env.SNUFFLER_ONNX_MODEL_BASE_URL || "assets/models/";
const HEADLESS = process.env.SNUFFLER_BROWSER_HEADLESS !== "0";
const TIMEOUT_MS = Number(process.env.SNUFFLER_BROWSER_TEST_TIMEOUT_MS || 300_000);
const PICK_THRESHOLD = Number(process.env.SNUFFLER_BROWSER_PICK_THRESHOLD || 0.02);

const METHODS = {
  makeExample: "/snuffler.bridge.v1.BridgeService/MakeExample",
  runSnuffling: "/snuffler.bridge.v1.BridgeService/RunSnuffling",
};

function bridgePayload(bytes) {
  return Array.from(bytes);
}

function bridgeResult(bytes, label, expectedCase) {
  const env = fromBinary(BridgeResultSchema, new Uint8Array(bytes));
  if (env.result.case === "error") {
    const error = env.result.value;
    throw new Error(`${label}: ${error.kind}: ${error.message}\n${error.traceback}`);
  }
  if (env.result.case !== expectedCase) {
    throw new Error(`${label}: expected ${expectedCase}, got ${env.result.case || "empty"}`);
  }
  return env.result.value;
}

function requestFor(scene, spec) {
  const centerTmin = scene.tmin + 145.0;
  const centerTmax = scene.tmin + 345.0;
  const view = create(ViewParamsSchema, {
    tmin: Math.max(scene.tmin, centerTmin),
    tmax: Math.min(scene.tmax, centerTmax),
    npx: 1400,
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

function browserArgs(disableWebgpu) {
  const args = [
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-search-engine-choice-screen",
    "--enable-unsafe-webgpu",
    "--enable-features=WebGPUDeveloperFeatures",
    "--ignore-gpu-blocklist",
    "--use-angle=gl",
  ];
  if (disableWebgpu) {
    args.push("--disable-webgpu", "--disable-features=WebGPU,WebGPUDeveloperFeatures");
  }
  return args;
}

async function bridgeModuleUrl(page) {
  return await page.evaluate(() => {
    const links = Array.from(document.querySelectorAll('link[rel="modulepreload"]'));
    const href = links
      .map((link) => link.href)
      .find((value) => value.includes("/web/gen/bridge.js"));
    if (!href) throw new Error("could not find bundled bridge.js modulepreload");
    return href;
  });
}

async function callBridge(page, moduleUrl, method, requestBytes, clearProgress = false) {
  const payload = {
    moduleUrl,
    method,
    request: bridgePayload(requestBytes),
    clearProgress,
  };
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      return await page.evaluate(
        async ({ moduleUrl: href, method: rpcMethod, request, clearProgress: shouldClear }) => {
          const bridge = await import(href);
          if (shouldClear) bridge.clearSeisbenchProgress();
          const out = await bridge.call(rpcMethod, new Uint8Array(request), new Uint8Array());
          return {
            result: Array.from(out.result),
            device: bridge.seisbenchDevice(),
            progress: bridge.seisbenchProgressFraction(),
            progressLabel: bridge.seisbenchProgressLabel(),
          };
        },
        payload
      );
    } catch (error) {
      if (!String(error).includes("Execution context was destroyed") || attempt === 2) throw error;
      await page.waitForLoadState("domcontentloaded");
      await page.waitForTimeout(500);
    }
  }
  throw new Error("unreachable bridge call retry state");
}

async function webgpuAvailable(page) {
  return await page.evaluate(async () => {
    try {
      if (!navigator.gpu) return false;
      return Boolean(await navigator.gpu.requestAdapter());
    } catch (_e) {
      return false;
    }
  });
}

async function runBrowserPass({ disableWebgpu, models }) {
  const browser = await chromium.launch({
    headless: HEADLESS,
    args: browserArgs(disableWebgpu),
  });
  const consoleEntries = [];
  const consoleErrors = [];
  const results = [];
  let hasWebgpu = false;

  try {
    for (const spec of models) {
      const page = await browser.newPage();
      page.setDefaultTimeout(TIMEOUT_MS);
      page.setDefaultNavigationTimeout(60_000);
      page.on("console", (msg) => {
        const text = msg.text();
        consoleEntries.push(`[${msg.type()}] ${text}`);
        if (msg.type() === "error") consoleErrors.push(text);
      });
      page.on("pageerror", (error) => {
        consoleErrors.push(`pageerror: ${error.message}`);
      });
      try {
        await page.goto(BASE_URL, { waitUntil: "domcontentloaded" });
        await page.waitForLoadState("load");
        await page.waitForTimeout(500);
        const moduleUrl = await bridgeModuleUrl(page);
        hasWebgpu = (await webgpuAvailable(page)) || hasWebgpu;
        const sceneReply = await callBridge(
          page,
          moduleUrl,
          METHODS.makeExample,
          toBinary(EmptySchema, create(EmptySchema))
        );
        const scene = bridgeResult(sceneReply.result, "MakeExample", "scene");
        if (scene.tracks.length < 18) {
          throw new Error(`MakeExample: expected at least 18 traces, got ${scene.tracks.length}`);
        }

        const request = requestFor(scene, spec);
        const reply = await callBridge(
          page,
          moduleUrl,
          METHODS.runSnuffling,
          toBinary(NamedSnufflingRequestSchema, request),
          true
        );
        const result = bridgeResult(
          reply.result,
          `${spec.model}/${spec.weights} RunSnuffling`,
          "snuffling"
        );
        if (reply.progress < 0.99) {
          throw new Error(
            `${spec.model}/${spec.weights}: final progress ${reply.progress}, label "${reply.progressLabel}"`
          );
        }
        if (!reply.progressLabel.includes("complete")) {
          throw new Error(
            `${spec.model}/${spec.weights}: final progress label did not complete: ${reply.progressLabel}`
          );
        }
        if (!reply.device || !["WebGPU", "CPU (wasm)"].includes(reply.device)) {
          throw new Error(`${spec.model}/${spec.weights}: invalid device label "${reply.device}"`);
        }
        if (result.text.includes("no ZNE data")) {
          throw new Error(`${spec.model}/${spec.weights}: did not run on the synthetic ZNE scene`);
        }
        const syntheticShouldProduceMarkers = spec.model === "PhaseNet";
        if (syntheticShouldProduceMarkers && result.markers.length === 0) {
          throw new Error(`${spec.model}/${spec.weights}: no browser markers produced`);
        }
        const phases = result.markers.reduce((acc, marker) => {
          const phase = marker.phase || "?";
          acc[phase] = (acc[phase] || 0) + 1;
          return acc;
        }, {});
        results.push({
          model: `${spec.model}/${spec.weights}`,
          markers: result.markers.length,
          phases,
          text: result.text,
          device: reply.device,
          progressLabel: reply.progressLabel,
        });
      } finally {
        await page.close();
      }
    }

    if (consoleErrors.length > 0) {
      throw new Error(`browser console errors:\n${consoleErrors.join("\n")}`);
    }

    return { hasWebgpu, consoleEntries, results };
  } finally {
    await browser.close();
  }
}

function requireProviderEvidence(run, providerText) {
  if (run.consoleEntries.some((entry) => entry.includes(providerText))) return;
  throw new Error(`missing ORT provider evidence: ${providerText}`);
}

function printResults(title, run) {
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
