// Validate the accessibility layer in the served wasm app.
//
// The layer projects egui's AccessKit tree into a DOM overlay (the accesskit-dom
// and egui-a11y workspace crates, wired up by crates/snuffler-web-egui/src/a11y.rs).
// Almost everything that can break it is invisible to a unit test: whether proxies
// line up with what is painted, whether a keypress reaches the widget the user
// thinks is focused, whether a resize leaves them stranded. So the checks here
// drive a real browser against the real app.
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

interface BridgeModule {
  call(
    method: "/snuffler.bridge.v1.BridgeService/ListExamples",
    request: Uint8Array,
    data: Uint8Array,
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
    };
  }
}

interface ProbeData {
  strayed: string[];
  proxies: number;
  controls: number;
  unnamed: number;
  occluded: string[];
  hitTestable: string[];
  maxDepth: number;
  liveRegions: number;
  rangedSliders: number;
  sliders: number;
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

function url(params: Record<string, string>): string {
  const target = new URL(BASE_URL);
  for (const [key, value] of Object.entries(params)) target.searchParams.set(key, value);
  return target.toString();
}

// No a11y param by default: the layer must come up for a plain visit, so the
// whole suite doubles as the proof that accessibility is on by default.
//
// `demo=1` for the data, though. What this suite measures is the projection —
// where proxies land, what they are named, whether a press reaches the canvas —
// and the app otherwise opens on a recorded dataset that introduces itself in a
// window, which is a legitimate thing for a window to do and would make the
// canvas-reachability probe a measurement of that window instead.
async function boot(page: Page, params: Record<string, string> = {}): Promise<void> {
  await page.goto(url({ demo: "1", ...params }), { waitUntil: "load" });
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

/// Geometry invariants.
///
/// The overlay itself has no box — proxies are `position: fixed` in viewport
/// coordinates precisely so that nesting them for semantics cannot move them — so
/// what has to hold is about the proxies: each one lands inside the canvas it
/// mirrors, and each one that takes pointer input is the topmost element at its own
/// centre. A proxy failing either is one an assistive technology can see and a user
/// cannot reach.
function geometryProbe(): ProbeData {
  const root = document.querySelector("#snuffler-a11y");
  const canvas = document.getElementById("the_canvas_id");
  if (!root || !canvas) throw new Error("overlay or canvas missing");

  const c = canvas.getBoundingClientRect();
  const all = [...root.querySelectorAll("*")];
  const bad: string[] = [];
  const strayed: string[] = [];
  const hitTestable: string[] = [];
  const SLACK = 2;

  for (const el of all) {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const cx = r.left + r.width / 2;
    const cy = r.top + r.height / 2;
    const name = el.getAttribute("aria-label") || el.getAttribute("role") || el.tagName;
    // Where a proxy sits is checked for every one of them; whether it is the
    // topmost element there is only a question for the ones that hit-test, and
    // only `?a11y=pointer` produces any of those.
    const takesPointer = getComputedStyle(el).pointerEvents !== "none";
    if (takesPointer) hitTestable.push(name);

    if (
      cx < c.left - SLACK ||
      cy < c.top - SLACK ||
      cx > c.right + SLACK ||
      cy > c.bottom + SLACK
    ) {
      // egui does paint past the edge when the window is too small for its layout,
      // so only flag a proxy whose centre is nowhere near the canvas at all.
      if (cx < c.left - c.width || cy < c.top - c.height) strayed.push(name);
      continue;
    }
    if (!takesPointer) continue;
    const hit = document.elementFromPoint(cx, cy);
    if (hit !== el && !el.contains(hit)) bad.push(name);
  }

  const nameOf = (el: Element): string => {
    const via = el.getAttribute("aria-labelledby");
    return (
      el.getAttribute("aria-label") ||
      (via && document.getElementById(via)?.textContent) ||
      el.textContent ||
      ""
    ).trim();
  };
  const controls = all.filter((el) => el.getAttribute("tabindex") === "0");

  return {
    strayed,
    proxies: all.length,
    controls: controls.length,
    unnamed: controls.filter((el) => !nameOf(el)).length,
    occluded: bad,
    hitTestable,
    maxDepth: Math.max(
      ...all.map((el) => {
        let depth = 0;
        for (let p: Element | null = el; p && p !== root; p = p.parentElement) depth += 1;
        return depth;
      })
    ),
    liveRegions: root.querySelectorAll("[aria-live]").length,
    rangedSliders: root.querySelectorAll(
      "[role=slider][aria-valuemin],[role=spinbutton][aria-valuemin]"
    ).length,
    sliders: root.querySelectorAll("[role=slider],[role=spinbutton]").length,
  };
}

async function main(): Promise<void> {
  const browser = await chromium.launch({ headless: HEADLESS });
  const page = await browser.newPage();
  const consoleErrors: string[] = [];
  page.on("pageerror", (e) => consoleErrors.push((e.stack || String(e)).slice(0, 500)));
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text().slice(0, 200));
  });

  try {
    console.log(`a11y: ${BASE_URL}`);
    await boot(page);

    console.log("\nstructure");
    const base = await page.evaluate(geometryProbe);
    check(
      "proxies land on the canvas they mirror",
      base.strayed.length === 0,
      base.strayed.join(", ")
    );
    check("the tree is nested, not flat", base.maxDepth >= 3, `depth ${base.maxDepth}`);
    check("controls are keyboard reachable", base.controls > 20, `${base.controls} tabbable`);
    check("every control has a name", base.unnamed === 0, `${base.unnamed} unnamed`);
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
      base.rangedSliders === base.sliders,
      `${base.rangedSliders}/${base.sliders}`
    );
    check("status is a live region", base.liveRegions >= 1);

    console.log("\nnames borrowed from adjacent labels");
    const named = await page.evaluate(() =>
      [...document.querySelectorAll("#snuffler-a11y [role=textbox]")].map((el) => {
        const via = el.getAttribute("aria-labelledby");
        return (
          el.getAttribute("aria-label") ||
          (via && document.getElementById(via)?.textContent) ||
          ""
        ).trim();
      })
    );
    check(
      "filter fields are named by the label beside them",
      named.some((n) => n.startsWith("Highpass")),
      named.join(" | ")
    );

    // Everything from here on drives the app the way an agent does — by resolving
    // an element and clicking it — which needs the proxies to be the hit target.
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
      pointerMode.occluded.length === 0,
      pointerMode.occluded.join(", ")
    );
    await page.getByRole("button", { name: "Options" }).click();
    await page.waitForTimeout(400);
    const boxes = await page.$$eval(`${OVERLAY} [role=checkbox]`, (els) =>
      els.map((e) => e.getAttribute("aria-label"))
    );
    check("a menu opens from a click", boxes.includes("Deconvolution"), boxes.join(", "));

    const before = await page.$eval(`${OVERLAY} [aria-label="Deconvolution"]`, (e) =>
      e.getAttribute("aria-checked")
    );
    await page.getByRole("checkbox", { name: "Deconvolution" }).click();
    await page.waitForTimeout(400);
    const after = await page.$eval(`${OVERLAY} [aria-label="Deconvolution"]`, (e) =>
      e.getAttribute("aria-checked")
    );
    check(
      "a click toggles state and it flows back",
      before === "false" && after === "true",
      `${before} -> ${after}`
    );
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);

    console.log("\nkeyboard");
    await page.$eval(`${OVERLAY} [aria-label="Options"]`, (e) => (e as HTMLElement).focus());
    await page.waitForTimeout(200);
    await page.keyboard.press("Enter");
    await page.waitForTimeout(400);
    const opened = await page.$$eval(`${OVERLAY} [role=checkbox]`, (els) => els.length);
    check("Enter activates the focused control", opened > 1, `${opened} checkboxes`);

    await page.$eval(`${OVERLAY} [aria-label="3D distances"]`, (e) => (e as HTMLElement).focus());
    await page.waitForTimeout(200);
    const spaceBefore = await page.$eval(`${OVERLAY} [aria-label="3D distances"]`, (e) =>
      e.getAttribute("aria-checked")
    );
    await page.keyboard.press("Space");
    await page.waitForTimeout(400);
    const spaceAfter = await page.$eval(`${OVERLAY} [aria-label="3D distances"]`, (e) =>
      e.getAttribute("aria-checked")
    );
    check(
      "Space toggles a checkbox",
      spaceBefore !== spaceAfter,
      `${spaceBefore} -> ${spaceAfter}`
    );
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);

    const sliderValue = () =>
      page.$eval(`${OVERLAY} [role=slider]`, (e) => Number(e.getAttribute("aria-valuenow")));
    await page.$eval(`${OVERLAY} [role=slider]`, (e) => (e as HTMLElement).focus());
    await page.waitForTimeout(200);
    const vBefore = await sliderValue();
    for (let i = 0; i < 5; i += 1) await page.keyboard.press("ArrowRight");
    await page.waitForTimeout(400);
    check("arrow keys drive a slider", (await sliderValue()) !== vBefore, `stayed at ${vBefore}`);

    console.log("\nresize");
    for (const [w, h] of [
      [900, 650],
      [1500, 900],
      [640, 480],
      [1200, 781],
    ] as const) {
      await page.setViewportSize({ width: w, height: h });
      await page.waitForTimeout(700);
      const probe = await page.evaluate(geometryProbe);
      check(
        `${w}x${h} keeps proxies on the canvas`,
        probe.strayed.length === 0,
        probe.strayed.join(", ")
      );
      check(
        `${w}x${h} keeps every control reachable`,
        probe.occluded.length === 0,
        probe.occluded.join(", ")
      );
    }

    console.log("\ncanvas is not blocked");
    await page.locator("#the_canvas_id").click({ position: { x: 400, y: 300 } });
    await page.waitForTimeout(400);
    check(
      "clicking the canvas still reaches egui",
      (await page.evaluate(geometryProbe)).proxies > 0
    );

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
    for (let i = 0; i < 30; i += 1) {
      await page.mouse.move(
        canvasBox.x + canvasBox.width * (0.2 + (i % 10) * 0.05),
        canvasBox.y + canvasBox.height * (0.35 + (i % 2) * 0.1)
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

    console.log("\nworker recovery");
    const recovery = await page.evaluate(async () => {
      const bridgeUrl = new URL("web/gen/bridge.js", document.baseURI).href;
      const bridge = (await import(bridgeUrl)) as BridgeModule;
      const before = bridge.currentRuntimeEpoch();
      const after = bridge.restartRuntime("browser recovery regression");
      const reply = await bridge.call(
        "/snuffler.bridge.v1.BridgeService/ListExamples",
        new Uint8Array(),
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

    console.log("\noff switch");
    await boot(page, { a11y: "0" });
    const absent = await page.evaluate(() => document.querySelector("#snuffler-a11y") === null);
    check("?a11y=0 builds no overlay at all", absent);

    check("no console errors", consoleErrors.length === 0, consoleErrors.join(" | "));
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
