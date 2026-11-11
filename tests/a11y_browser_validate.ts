// Validate the accessibility layer in the served wasm app.
//
// The layer projects egui's AccessKit tree into a DOM overlay (the accesskit-dom
// and egui-a11y workspace crates, wired up by crates/snuffler-web-egui/src/a11y.rs).
// Almost everything that can break it is invisible to a unit test.
// Do proxies line up with what is painted, does a keypress reach the widget the
// user thinks is focused, does a resize leave them stranded?
// So the checks here drive a real browser against the real app.
import { chromium, type Page } from "playwright";

const BASE_URL = process.env.SNUFFLER_BROWSER_URL || "http://127.0.0.1:17683/";
const HEADLESS = process.env.SNUFFLER_BROWSER_HEADLESS !== "0";
const TIMEOUT_MS = Number(process.env.SNUFFLER_BROWSER_TEST_TIMEOUT_MS || 300_000);
const OVERLAY = "#snuffler-a11y";

// What `snuffler_a11y_stats` reports (crates/snuffler-web-egui/src/a11y.rs).
interface A11yStats {
  passes: number;
  totalMs: number;
  described: number;
  placed: number;
}

interface FrameStats {
  emaMs: number;
  fps: number;
}

interface ExportProbeWindow extends Window {
  __snufflerExportFrames?: number;
  __snufflerExportLongTasks?: number[];
  __snufflerExportProbeActive?: boolean;
  __snufflerExportObserver?: PerformanceObserver;
}

interface BridgeModule {
  call(
    method: "/snuffler.bridge.v1.BridgeService/ListExamples",
    request: Uint8Array,
    jobId: bigint
  ): Promise<{ result: Uint8Array }>;
  currentRuntimeEpoch(): number;
  restartRuntime(reason?: string): number;
}

// The wasm module exposes its exports on `window.wasmBindings` (trunk's loader).
declare global {
  interface Window {
    wasmBindings?: {
      snuffler_a11y_stats(): string;
      snuffler_frame_stats(): string;
      snuffler_rayon_ready(): boolean;
      snuffler_worker_png_export_count(): number;
    };
  }
}

// What `geometryProbe` measures; the string lists hold proxy names.
interface ProbeData {
  strayedProxies: string[];
  proxyCount: number;
  controlCount: number;
  unnamedControlCount: number;
  occludedControls: string[];
  hitTestable: string[];
  maxDepth: number;
  liveRegionCount: number;
  rangedSliderCount: number;
  sliderCount: number;
}

// Errors the page reported while the suite ran, collected for the final checks.
interface PageErrors {
  console: string[];
  crashes: string[];
}

function watchPageErrors(page: Page): PageErrors {
  const errors: PageErrors = { console: [], crashes: [] };
  page.on("crash", () => errors.crashes.push("page crashed"));
  page.on("pageerror", (error) =>
    errors.console.push((error.stack || String(error)).slice(0, 500))
  );
  page.on("console", (message) => {
    if (message.type() === "error") errors.console.push(message.text().slice(0, 200));
  });
  return errors;
}

const failures: string[] = [];
let checks = 0;

function check(name: string, ok: boolean, detail = ""): void {
  checks += 1;
  if (ok) {
    console.log(`  ok   ${name}`);
  } else {
    console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ""}`);
    failures.push(name);
  }
}

function appUrl(params: Record<string, string>): string {
  const target = new URL(BASE_URL);
  for (const [key, value] of Object.entries(params)) target.searchParams.set(key, value);
  return target.toString();
}

// No a11y param by default: the layer must come up for a plain visit, so the
// whole suite doubles as the proof that accessibility is on by default.
//
// `demo=1` for the data, though.
// This suite measures the projection: where proxies land, what they are named,
// whether a press reaches the canvas.
// Without it the app opens on a recorded dataset that introduces itself in a
// window, and the canvas-reachability probe would measure that window instead.
async function boot(page: Page, params: Record<string, string> = {}): Promise<void> {
  await page.goto(appUrl({ demo: "1", ...params }), { waitUntil: "load" });
  // The boot overlay covers the app until Pyodide has imported pyrocko.
  await page
    .getByText("Importing pyrocko...")
    .first()
    .waitFor({ state: "hidden", timeout: TIMEOUT_MS });
  await page
    .locator('#the_canvas_id[data-snuffler-ready="true"]')
    .waitFor({ state: "attached", timeout: TIMEOUT_MS });
  await page.waitForTimeout(2000);
}

// Geometry invariants.
//
// The overlay itself has no box: proxies are `position: fixed` in viewport
// coordinates, so nesting them for semantics cannot move them.
// What has to hold is about the proxies.
// Each one lands inside the canvas it mirrors, and each one that takes pointer
// input is the topmost element at its own centre.
// A proxy failing either is one an assistive technology can see and a user cannot
// reach.
//
// Runs inside the page through `page.evaluate`, so it must not reference anything
// outside its own body.
function geometryProbe(): ProbeData {
  const root = document.querySelector("#snuffler-a11y");
  const canvas = document.getElementById("the_canvas_id");
  if (!root || !canvas) throw new Error("overlay or canvas missing");

  const canvasRect = canvas.getBoundingClientRect();
  const proxies = [...root.querySelectorAll("*")];
  const occludedControls: string[] = [];
  const strayedProxies: string[] = [];
  const hitTestable: string[] = [];
  const SLACK_PX = 2;

  for (const proxy of proxies) {
    const rect = proxy.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) continue;
    const centerX = rect.left + rect.width / 2;
    const centerY = rect.top + rect.height / 2;
    const name = proxy.getAttribute("aria-label") || proxy.getAttribute("role") || proxy.tagName;
    // Where a proxy sits is checked for every one of them.
    // Whether it is the topmost element there only matters for the ones that
    // hit-test, and only `?a11y=pointer` produces any of those.
    const takesPointer = getComputedStyle(proxy).pointerEvents !== "none";
    if (takesPointer) hitTestable.push(name);

    if (
      centerX < canvasRect.left - SLACK_PX ||
      centerY < canvasRect.top - SLACK_PX ||
      centerX > canvasRect.right + SLACK_PX ||
      centerY > canvasRect.bottom + SLACK_PX
    ) {
      // egui does paint past the edge when the window is too small for its layout,
      // so only flag a proxy whose centre is nowhere near the canvas at all.
      if (
        centerX < canvasRect.left - canvasRect.width ||
        centerY < canvasRect.top - canvasRect.height
      ) {
        strayedProxies.push(name);
      }
      continue;
    }
    if (!takesPointer) continue;
    const hit = document.elementFromPoint(centerX, centerY);
    if (hit !== proxy && !proxy.contains(hit)) occludedControls.push(name);
  }

  const accessibleName = (element: Element): string => {
    const labelledBy = element.getAttribute("aria-labelledby");
    return (
      element.getAttribute("aria-label") ||
      (labelledBy && document.getElementById(labelledBy)?.textContent) ||
      element.textContent ||
      ""
    ).trim();
  };
  const controls = proxies.filter((proxy) => proxy.getAttribute("tabindex") === "0");

  return {
    strayedProxies,
    proxyCount: proxies.length,
    controlCount: controls.length,
    unnamedControlCount: controls.filter((control) => !accessibleName(control)).length,
    occludedControls,
    hitTestable,
    maxDepth: Math.max(
      ...proxies.map((proxy) => {
        let depth = 0;
        for (let node: Element | null = proxy; node && node !== root; node = node.parentElement) {
          depth += 1;
        }
        return depth;
      })
    ),
    liveRegionCount: root.querySelectorAll("[aria-live]").length,
    rangedSliderCount: root.querySelectorAll(
      "[role=slider][aria-valuemin],[role=spinbutton][aria-valuemin]"
    ).length,
    sliderCount: root.querySelectorAll("[role=slider],[role=spinbutton]").length,
  };
}

async function checkStructure(page: Page): Promise<void> {
  console.log("\nstructure");
  const base = await page.evaluate(geometryProbe);
  check(
    "proxies land on the canvas they mirror",
    base.strayedProxies.length === 0,
    base.strayedProxies.join(", ")
  );
  check("the tree is nested, not flat", base.maxDepth >= 3, `depth ${base.maxDepth}`);
  check("controls are keyboard reachable", base.controlCount > 20, `${base.controlCount} tabbable`);
  check(
    "every control has a name",
    base.unnamedControlCount === 0,
    `${base.unnamedControlCount} unnamed`
  );
  // The layer is on for a plain visit, so it has to be invisible to the mouse:
  // a proxy that hit-tests is handed the press meant for the canvas, and since
  // it can only report a click, the drag that a slider or a sweep is made of
  // never reaches egui at all.
  check(
    "no proxy takes pointer input by default",
    base.hitTestable.length === 0,
    base.hitTestable.slice(0, 8).join(", ")
  );
  check(
    "sliders publish their range",
    base.rangedSliderCount === base.sliderCount,
    `${base.rangedSliderCount}/${base.sliderCount}`
  );
  check("status is a live region", base.liveRegionCount >= 1);
}

async function checkBorrowedNames(page: Page): Promise<void> {
  console.log("\nnames borrowed from adjacent labels");
  const names = await page.evaluate(() =>
    [...document.querySelectorAll("#snuffler-a11y [role=textbox]")].map((textbox) => {
      const labelledBy = textbox.getAttribute("aria-labelledby");
      return (
        textbox.getAttribute("aria-label") ||
        (labelledBy && document.getElementById(labelledBy)?.textContent) ||
        ""
      ).trim();
    })
  );
  check(
    "filter fields are named by the label beside them",
    names.some((name) => name.startsWith("Highpass")),
    names.join(" | ")
  );
}

// Everything from here on drives the app the way an agent does, by resolving an
// element and clicking it, which needs the proxies to be the hit target.
async function checkPointerMode(page: Page): Promise<void> {
  console.log("\npointer (?a11y=pointer)");
  await boot(page, { a11y: "pointer" });
  const pointerMode = await page.evaluate(geometryProbe);
  check(
    "?a11y=pointer makes controls the hit target",
    pointerMode.hitTestable.length > 0,
    `${pointerMode.hitTestable.length} hit-testable`
  );
  check(
    "no control is occluded",
    pointerMode.occludedControls.length === 0,
    pointerMode.occludedControls.join(", ")
  );
  await page.getByRole("button", { name: "Options" }).click();
  await page.waitForTimeout(400);
  const checkboxLabels = await page.$$eval(`${OVERLAY} [role=checkbox]`, (checkboxes) =>
    checkboxes.map((checkbox) => checkbox.getAttribute("aria-label"))
  );
  check(
    "a menu opens from a click",
    checkboxLabels.includes("Deconvolution"),
    checkboxLabels.join(", ")
  );

  const before = await page.$eval(`${OVERLAY} [aria-label="Deconvolution"]`, (checkbox) =>
    checkbox.getAttribute("aria-checked")
  );
  await page.getByRole("checkbox", { name: "Deconvolution" }).click();
  await page.waitForTimeout(400);
  const after = await page.$eval(`${OVERLAY} [aria-label="Deconvolution"]`, (checkbox) =>
    checkbox.getAttribute("aria-checked")
  );
  check(
    "a click toggles state and it flows back",
    before === "false" && after === "true",
    `${before} -> ${after}`
  );
  await page.keyboard.press("Escape");
  await page.waitForTimeout(300);
}

async function checkKeyboard(page: Page): Promise<void> {
  console.log("\nkeyboard");
  await page.$eval(`${OVERLAY} [aria-label="Options"]`, (element) =>
    (element as HTMLElement).focus()
  );
  await page.waitForTimeout(200);
  await page.keyboard.press("Enter");
  await page.waitForTimeout(400);
  const openedCheckboxes = await page.$$eval(
    `${OVERLAY} [role=checkbox]`,
    (checkboxes) => checkboxes.length
  );
  check(
    "Enter activates the focused control",
    openedCheckboxes > 1,
    `${openedCheckboxes} checkboxes`
  );

  await page.$eval(`${OVERLAY} [aria-label="3D distances"]`, (element) =>
    (element as HTMLElement).focus()
  );
  await page.waitForTimeout(200);
  const spaceBefore = await page.$eval(`${OVERLAY} [aria-label="3D distances"]`, (checkbox) =>
    checkbox.getAttribute("aria-checked")
  );
  await page.keyboard.press("Space");
  await page.waitForTimeout(400);
  const spaceAfter = await page.$eval(`${OVERLAY} [aria-label="3D distances"]`, (checkbox) =>
    checkbox.getAttribute("aria-checked")
  );
  check("Space toggles a checkbox", spaceBefore !== spaceAfter, `${spaceBefore} -> ${spaceAfter}`);
  await page.keyboard.press("Escape");
  await page.waitForTimeout(300);

  const sliderValue = () =>
    page.$eval(`${OVERLAY} [role=slider]`, (slider) =>
      Number(slider.getAttribute("aria-valuenow"))
    );
  await page.$eval(`${OVERLAY} [role=slider]`, (element) => (element as HTMLElement).focus());
  await page.waitForTimeout(200);
  const sliderBefore = await sliderValue();
  for (let press = 0; press < 5; press += 1) await page.keyboard.press("ArrowRight");
  await page.waitForTimeout(400);
  check(
    "arrow keys drive a slider",
    (await sliderValue()) !== sliderBefore,
    `stayed at ${sliderBefore}`
  );
}

async function checkResize(page: Page): Promise<void> {
  console.log("\nresize");
  for (const [width, height] of [
    [900, 650],
    [1500, 900],
    [640, 480],
    [1200, 781],
  ] as const) {
    await page.setViewportSize({ width, height });
    await page.waitForTimeout(700);
    const probe = await page.evaluate(geometryProbe);
    check(
      `${width}x${height} keeps proxies on the canvas`,
      probe.strayedProxies.length === 0,
      probe.strayedProxies.join(", ")
    );
    check(
      `${width}x${height} keeps every control reachable`,
      probe.occludedControls.length === 0,
      probe.occludedControls.join(", ")
    );
  }
}

async function checkCanvasStillReachable(page: Page): Promise<void> {
  console.log("\ncanvas is not blocked");
  await page.locator("#the_canvas_id").click({ position: { x: 400, y: 300 } });
  await page.waitForTimeout(400);
  check(
    "clicking the canvas still reaches egui",
    (await page.evaluate(geometryProbe)).proxyCount > 0
  );
}

async function checkProjectionCost(page: Page): Promise<void> {
  console.log("\ncost");
  const stats = await page.evaluate(() => {
    if (!window.wasmBindings) throw new Error("wasmBindings missing");
    return JSON.parse(window.wasmBindings.snuffler_a11y_stats()) as A11yStats;
  });
  const perPass = (stats.totalMs / stats.passes) * 1000;
  console.log(`  ${stats.passes} passes, ${perPass.toFixed(1)}us each`);
  // Generous, and deliberately so: a release build measures ~25us per pass, but
  // `trunk serve` builds the dev profile, where the same work takes ~30x longer.
  // The bar only has to catch a regression that would put the projection into a
  // frame's critical path.
  check("a pass stays well inside the frame budget", perPass < 2000, `${perPass.toFixed(1)}us`);
}

async function checkEifelMarkerBounds(page: Page): Promise<void> {
  console.log("\neifel marker bounds");
  await page.setViewportSize({ width: 1200, height: 781 });
  await boot(page, { a11y: "pointer", example: "eifel" });
  await page.getByText("Ready · 1061 traces").first().waitFor({ timeout: TIMEOUT_MS });
  const defaultTrackHeight = Number(
    await page.getByRole("slider", { name: "Track height:" }).getAttribute("aria-valuenow")
  );
  check(
    "the default layout keeps several tracks visible",
    Number.isFinite(defaultTrackHeight) && defaultTrackHeight < 200,
    `${defaultTrackHeight}px`
  );
  const trackHeight = page.getByRole("slider", { name: "Track height:" });
  await trackHeight.focus();
  await page.keyboard.press("ArrowRight");
  await page.waitForTimeout(100);
  const transientStatuses = await page.locator("#snuffler-a11y [aria-live]").allTextContents();
  check(
    "a short wiggle refresh does not flash activity feedback",
    transientStatuses.every((status) => !status.includes("Updating view")),
    transientStatuses.join(" | ")
  );
  await page.getByText("Ready · 1061 traces").first().waitFor({ timeout: TIMEOUT_MS });
  const datasetClose = page.getByRole("button", { name: "Close window" });
  if (await datasetClose.isVisible()) await datasetClose.click();
  await page.getByRole("button", { name: "Snufflings" }).click();
  await page.getByRole("checkbox", { name: "Markers" }).click();
  await page.getByRole("dialog", { name: "Markers" }).waitFor({ timeout: TIMEOUT_MS });
  await page.waitForTimeout(500);
  const markerProjection = await page.evaluate(() => ({
    nodes: document.querySelectorAll("#snuffler-a11y *").length,
    names: [...document.querySelectorAll("#snuffler-a11y *")].map(
      (node) => node.getAttribute("aria-label") || node.textContent || ""
    ),
  }));
  check(
    "the eifel marker table projects only visible rows",
    markerProjection.nodes < 800 &&
      markerProjection.names.some((name) => name.includes("717 markers")),
    `${markerProjection.nodes} nodes`
  );

  const canvas = page.locator("#the_canvas_id");
  const canvasBox = await canvas.boundingBox();
  if (!canvasBox) throw new Error("canvas has no bounding box");
  for (let step = 0; step < 30; step += 1) {
    await page.mouse.move(
      canvasBox.x + canvasBox.width * (0.2 + (step % 10) * 0.05),
      canvasBox.y + canvasBox.height * (0.35 + (step % 2) * 0.1)
    );
    await page.waitForTimeout(17);
  }
  const frameStats = await page.evaluate(() => {
    if (!window.wasmBindings) throw new Error("wasmBindings missing");
    return JSON.parse(window.wasmBindings.snuffler_frame_stats()) as FrameStats;
  });
  check(
    "the open eifel marker editor stays within the frame budget",
    frameStats.emaMs > 0 && frameStats.emaMs < 40,
    `${frameStats.emaMs.toFixed(1)}ms (${frameStats.fps.toFixed(1)} fps)`
  );
}

async function checkPngExport(page: Page, errors: PageErrors): Promise<void> {
  console.log("\nimage export");
  const closeMarkerDialog = page
    .getByRole("dialog", { name: "Markers" })
    .getByRole("button", { name: "Close window" });
  if (await closeMarkerDialog.isVisible()) await closeMarkerDialog.click();
  // Force the anchor-download fallback because Playwright cannot accept the
  // native File System Access save picker in headless mode.
  await page.evaluate(() => {
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: undefined,
    });
  });
  const workerBefore = await page.evaluate(() => {
    if (!window.wasmBindings) throw new Error("wasmBindings missing");
    return {
      isolated: crossOriginIsolated,
      ready: window.wasmBindings.snuffler_rayon_ready(),
      exports: window.wasmBindings.snuffler_worker_png_export_count(),
    };
  });
  check(
    "the threaded export pool is genuinely available",
    workerBefore.isolated && workerBefore.ready,
    `isolated=${workerBefore.isolated}, ready=${workerBefore.ready}`
  );
  await page.evaluate(() => {
    const target = window as ExportProbeWindow;
    target.__snufflerExportFrames = 0;
    target.__snufflerExportLongTasks = [];
    target.__snufflerExportProbeActive = true;
    const countFrame = () => {
      if (!target.__snufflerExportProbeActive) return;
      target.__snufflerExportFrames = (target.__snufflerExportFrames ?? 0) + 1;
      requestAnimationFrame(countFrame);
    };
    requestAnimationFrame(countFrame);
    if (PerformanceObserver.supportedEntryTypes.includes("longtask")) {
      target.__snufflerExportObserver = new PerformanceObserver((list) => {
        target.__snufflerExportLongTasks?.push(...list.getEntries().map((entry) => entry.duration));
      });
      target.__snufflerExportObserver.observe({ entryTypes: ["longtask"] });
    }
  });
  await page.getByRole("button", { name: "File" }).click();
  const pngDownload = page.waitForEvent("download", { timeout: TIMEOUT_MS });
  await page.getByRole("button", { name: /Save image \(PNG\)/ }).click();
  const download = await pngDownload;
  const downloadStream = await download.createReadStream();
  let pngBytes = 0;
  for await (const chunk of downloadStream) pngBytes += chunk.length;
  const exportErrors = [...errors.crashes, ...errors.console];
  check(
    "PNG export downloads a non-empty image without crashing",
    download.suggestedFilename().endsWith(".png") && pngBytes > 0 && exportErrors.length === 0,
    `${download.suggestedFilename()}, ${pngBytes} bytes${
      exportErrors.length > 0 ? `, ${exportErrors.join(" | ")}` : ""
    }`
  );
  const workerAfter = await page.evaluate(() => {
    if (!window.wasmBindings) throw new Error("wasmBindings missing");
    return window.wasmBindings.snuffler_worker_png_export_count();
  });
  check(
    "PNG rasterization actually completes on rayon",
    workerAfter > workerBefore.exports,
    `${workerBefore.exports} -> ${workerAfter}`
  );
  await page.waitForTimeout(100);
  const exportResponsiveness = await page.evaluate(() => {
    const target = window as ExportProbeWindow;
    target.__snufflerExportProbeActive = false;
    target.__snufflerExportObserver?.disconnect();
    return {
      frames: target.__snufflerExportFrames ?? 0,
      maxLongTaskMs: Math.max(0, ...(target.__snufflerExportLongTasks ?? [])),
    };
  });
  check(
    "the browser keeps painting during PNG export",
    exportResponsiveness.frames > 1 && exportResponsiveness.maxLongTaskMs < 500,
    `${exportResponsiveness.frames} frames, longest task ${exportResponsiveness.maxLongTaskMs.toFixed(1)}ms`
  );
  check(
    "the app remains interactive after PNG export",
    await page.locator('#the_canvas_id[data-snuffler-ready="true"]').isVisible()
  );
}

async function checkSvgExport(page: Page, errors: PageErrors): Promise<void> {
  await page.getByRole("button", { name: "File" }).click();
  const svgDownload = page.waitForEvent("download", { timeout: TIMEOUT_MS });
  await page.getByRole("button", { name: /Save image \(SVG\)/ }).click();
  const svg = await svgDownload;
  const svgStream = await svg.createReadStream();
  let svgBytes = 0;
  for await (const chunk of svgStream) svgBytes += chunk.length;
  check(
    "SVG export also remains on the browser-safe path",
    svg.suggestedFilename().endsWith(".svg") && svgBytes > 0 && errors.console.length === 0,
    `${svg.suggestedFilename()}, ${svgBytes} bytes`
  );
}

// Stub the native picker to cover its write path and a second use of the rayon
// pool without opening browser chrome that Playwright cannot drive.
async function checkPickerExport(page: Page, errors: PageErrors): Promise<void> {
  await page.evaluate(() => {
    const target = window as typeof window & { __snufflerSavedBytes?: number };
    target.__snufflerSavedBytes = 0;
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: async () => ({
        createWritable: async () => ({
          close: async () => undefined,
          write: async (blob: Blob) => {
            target.__snufflerSavedBytes = blob.size;
          },
        }),
      }),
    });
  });
  await page.getByRole("button", { name: "File" }).click();
  await page.getByRole("button", { name: /Save image \(PNG\)/ }).click();
  await page.waitForFunction(
    () =>
      ((window as typeof window & { __snufflerSavedBytes?: number }).__snufflerSavedBytes ?? 0) > 0,
    undefined,
    { timeout: TIMEOUT_MS }
  );
  const pickerBytes = await page.evaluate(
    () => (window as typeof window & { __snufflerSavedBytes?: number }).__snufflerSavedBytes ?? 0
  );
  check(
    "a repeated PNG export writes through the native-picker path",
    pickerBytes > 0 && errors.crashes.length === 0 && errors.console.length === 0,
    `${pickerBytes} bytes`
  );
}

async function checkWorkerRecovery(page: Page): Promise<void> {
  console.log("\nworker recovery");
  const recovery = await page.evaluate(async () => {
    const bridgeUrl = new URL("web/gen/bridge.js", document.baseURI).href;
    const bridge = (await import(bridgeUrl)) as BridgeModule;
    const before = bridge.currentRuntimeEpoch();
    const after = bridge.restartRuntime("browser recovery regression");
    const reply = await bridge.call(
      "/snuffler.bridge.v1.BridgeService/ListExamples",
      new Uint8Array(),
      999n
    );
    return { before, after, replyBytes: reply.result.byteLength };
  });
  check(
    "a recreated Python worker advances its epoch",
    recovery.after === recovery.before + 1,
    `${recovery.before} -> ${recovery.after}`
  );
  check(
    "the recreated Python worker accepts a fresh request",
    recovery.replyBytes > 0,
    `${recovery.replyBytes} reply bytes`
  );
}

async function checkOffSwitch(page: Page): Promise<void> {
  console.log("\noff switch");
  await boot(page, { a11y: "0" });
  const absent = await page.evaluate(() => document.querySelector("#snuffler-a11y") === null);
  check("?a11y=0 builds no overlay at all", absent);
}

async function main(): Promise<void> {
  const browser = await chromium.launch({ headless: HEADLESS });
  const page = await browser.newPage();
  const errors = watchPageErrors(page);

  try {
    console.log(`a11y: ${BASE_URL}`);
    await boot(page);
    await checkStructure(page);
    await checkBorrowedNames(page);
    await checkPointerMode(page);
    await checkKeyboard(page);
    await checkResize(page);
    await checkCanvasStillReachable(page);
    await checkProjectionCost(page);
    await checkEifelMarkerBounds(page);
    await checkPngExport(page, errors);
    await checkSvgExport(page, errors);
    await checkPickerExport(page, errors);
    await checkWorkerRecovery(page);
    await checkOffSwitch(page);

    check("no console errors", errors.console.length === 0, errors.console.join(" | "));
  } finally {
    await browser.close();
  }

  console.log(`\n${checks - failures.length}/${checks} checks passed`);
  if (failures.length) {
    console.error(`FAILED: ${failures.join(", ")}`);
    process.exit(1);
  }
}

await main();
