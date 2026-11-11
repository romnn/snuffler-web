// Tier 2: extract the web frontend's command surface from its accessibility layer.
//
// The app renders to a canvas, so there is no DOM to read — except the one the
// accessibility layer builds. `?a11y=debug` turns on both halves of it: the DOM
// projection under `#snuffler-a11y`, which carries role, name, state and value range
// for every live node, and the raw AccessKit tree on `window.__snufflerA11y`, which
// additionally says which actions each node supports.
//
// The JSON global is republished as a *delta* on every pass that changes anything, so
// reading it once gives whatever last changed rather than the whole tree. An init
// script therefore intercepts the assignment and accumulates, and the DOM — which is
// the accumulated projection by construction — is what the walk actually reads.
//
// Writes the same JSON shape as inventory_qt.py. run_inventory.py diffs the two.
//
//   bun tests/parity/inventory_web.ts --out tests/parity/.artifacts/inventory-web.json

import { readFileSync, writeFileSync } from "node:fs";

import { type Page } from "playwright";

import {
  BASE_URL,
  BOOT_TIMEOUT_MS,
  launchParityBrowser,
  reportParityRenderer,
  saveWebScreenshots,
  waitForAppReady,
} from "./parity_browser";
import { pressKey } from "./parity_keys";

const OVERLAY = "#snuffler-a11y";

// The menu bar, in the order the app lays it out.
const MENUS = ["File", "Data", "View", "Options", "Scaling", "Sorting", "Snufflings", "Help"];

interface Node {
  id: string;
  role: string;
  name: string;
  checked: string | null;
  disabled: boolean;
  valuemin: number | null;
  valuemax: number | null;
  valuenow: number | null;
  focusable: boolean;
}

interface MenuEntry {
  path: string;
  text: string;
  kind: string;
  checkable: boolean;
  checked: boolean;
  enabled: boolean;
  actions: string[];
}

declare global {
  interface Window {
    __snufflerA11y?: string;
    __parityActions?: Record<string, string[]>;
  }
}

function url(params: Record<string, string>): string {
  const target = new URL(BASE_URL);
  for (const [key, value] of Object.entries(params)) target.searchParams.set(key, value);
  return target.toString();
}

/// Accumulate every published AccessKit delta into one id -> actions map.
async function installTreeAccumulator(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.__parityActions = {};
    let last = "";
    Object.defineProperty(window, "__snufflerA11y", {
      configurable: true,
      get: () => last,
      set: (value: string) => {
        last = value;
        try {
          const update = JSON.parse(value) as { nodes: { id: string; actions?: string[] }[] };
          for (const node of update.nodes) {
            window.__parityActions![node.id] = node.actions ?? [];
          }
        } catch {
          // A malformed delta is a bug in the publisher, not a reason to stop the walk.
        }
      },
    });
  });
}

/// `debug` for reading — it adds the raw AccessKit tree — and `pointer` for driving,
/// since the two flags are mutually exclusive and only `pointer` makes the proxies the
/// hit target. The DOM overlay the walk reads exists in both.
async function boot(page: Page, mode: "debug" | "pointer" = "debug"): Promise<void> {
  await page.goto(url({ a11y: mode, demo: "1" }), { waitUntil: "load" });
  await page
    .getByText("Importing pyrocko...")
    .first()
    .waitFor({ state: "hidden", timeout: BOOT_TIMEOUT_MS });
  await waitForAppReady(page);
}

/// Every named node currently projected into the DOM.
///
/// A name is `aria-label`, or the label the node borrows through `aria-labelledby` —
/// which is how the control strip's text fields and sliders are named, since their
/// caption is a separate widget beside them. `textContent` is deliberately *not* a
/// fallback: on a container it yields the concatenation of everything inside, which
/// reads like a command and is not one.
function readOverlay(): Node[] {
  const root = document.querySelector("#snuffler-a11y");
  if (!root) throw new Error("a11y overlay missing — is ?a11y=debug on?");
  const num = (el: Element, attr: string): number | null => {
    const raw = el.getAttribute(attr);
    return raw === null ? null : Number(raw);
  };
  const labelledBy = (el: Element): string => {
    const via = el.getAttribute("aria-labelledby");
    if (!via) return "";
    const label = document.getElementById(via);
    return (label?.getAttribute("aria-label") ?? label?.textContent ?? "").trim();
  };
  const nameOf = (el: Element): string => {
    // A combo box's own aria-label is its *selected value* (egui labels the
    // widget with its text), so the caption it borrows through labelledby is
    // the name — "Colormap", not whichever entry happens to be chosen.
    if (el.getAttribute("role") === "combobox") {
      const caption = labelledBy(el);
      if (caption) return caption;
    }
    const direct = el.getAttribute("aria-label");
    if (direct) return direct.trim();
    return labelledBy(el);
  };
  return [...root.querySelectorAll("[data-ak]")]
    .map((el) => ({
      id: el.getAttribute("data-ak") ?? "",
      role: el.getAttribute("role") ?? el.tagName.toLowerCase(),
      name: nameOf(el),
      checked: el.getAttribute("aria-checked"),
      disabled: el.hasAttribute("aria-disabled") || el.hasAttribute("disabled"),
      valuemin: num(el, "aria-valuemin"),
      valuemax: num(el, "aria-valuemax"),
      valuenow: num(el, "aria-valuenow"),
      focusable: el.getAttribute("tabindex") === "0",
    }))
    .filter((node) => node.name.length > 0);
}

async function snapshot(page: Page): Promise<{ nodes: Node[]; actions: Record<string, string[]> }> {
  const nodes = await page.evaluate(readOverlay);
  const actions = await page.evaluate(() => window.__parityActions ?? {});
  return { nodes, actions };
}

function kindOf(node: Node): string {
  if (node.checked !== null) return "check";
  if (node.role === "radio" || node.role === "menuitemradio") return "radio";
  return "action";
}

/// Toggle a menu open or closed through its own button.
///
/// Keyboard rather than mouse, because `?a11y=debug` and `?a11y=pointer` are mutually
/// exclusive: with the debug tree on, the proxies do not take pointer input, so a
/// click would land on the canvas. And Enter rather than Escape to close, because
/// Escape reaches egui and puts the viewer into batch-picking mode.
async function toggleMenu(page: Page, menu: string): Promise<boolean> {
  const selector = `${OVERLAY} [aria-label="${menu}"]`;
  if (!(await page.$(selector))) return false;
  await page.$eval(selector, (el) => (el as HTMLElement).focus());
  await page.waitForTimeout(120);
  await page.keyboard.press("Enter");
  await page.waitForTimeout(450);
  return true;
}

/// Focus a named control and activate it with the keyboard.
async function activate(page: Page, name: string): Promise<void> {
  const handles = await page.$$(`${OVERLAY} [aria-label="${name}"][tabindex="0"]`);
  const target = handles.at(-1);
  if (!target) throw new Error(`no focusable control named ${name}`);
  await target.evaluate((el) => (el as HTMLElement).focus());
  await page.waitForTimeout(120);
  await page.keyboard.press("Enter");
  await page.waitForTimeout(400);
}

async function collectMenus(page: Page, base: Node[]): Promise<MenuEntry[]> {
  const known = new Set(base.map((node) => node.id));
  const entries: MenuEntry[] = [];
  for (const menu of MENUS) {
    entries.push({
      path: menu,
      text: menu,
      kind: "menu",
      checkable: false,
      checked: false,
      enabled: true,
      actions: [],
    });
    if (!(await toggleMenu(page, menu))) {
      throw new Error(`menu ${menu} is not in the accessibility tree`);
    }
    const { nodes, actions } = await snapshot(page);
    // A menu item shows up twice while the menu is opening: once as the plain
    // clickable the previous frame had, once carrying its state. Keep the stateful
    // one — it is the item, and the other is the same command mid-repaint.
    const best = new Map<string, Node>();
    for (const node of nodes) {
      if (known.has(node.id)) continue;
      const previous = best.get(node.name);
      if (!previous || (previous.checked === null && node.checked !== null)) {
        best.set(node.name, node);
      }
    }
    for (const node of best.values()) {
      entries.push({
        path: `${menu}/${node.name}`,
        text: node.name,
        kind: kindOf(node),
        checkable: node.checked !== null,
        checked: node.checked === "true",
        enabled: !node.disabled,
        actions: actions[node.id] ?? [],
      });
    }
    await toggleMenu(page, menu);
  }
  return entries;
}

/// The bottom control strip: named sliders, spin buttons, fields and checkboxes.
function controlsFrom(nodes: Node[]): Record<string, unknown>[] {
  const interactive = new Set(["slider", "spinbutton", "textbox", "checkbox", "combobox"]);
  const out: Record<string, unknown>[] = [];
  const seen = new Set<string>();
  for (const node of nodes) {
    if (!interactive.has(node.role) || seen.has(node.name)) continue;
    seen.add(node.name);
    out.push({
      name: node.name,
      role: node.role,
      minimum: node.valuemin,
      maximum: node.valuemax,
      value: node.valuenow,
      checked: node.checked === null ? null : node.checked === "true",
    });
  }
  return out.sort((a, b) => String(a.name).localeCompare(String(b.name)));
}

/// The control strip carries different widgets in the two view modes — the colorbar
/// row only exists in the waterfall, and gain only outside it, exactly as snuffler
/// shows and hides them. Capture both and take the union, or half the strip reads as
/// missing for no better reason than which mode the walk happened to start in.
async function collectControls(page: Page, base: Node[]): Promise<Record<string, unknown>[]> {
  const wiggle = controlsFrom(base);

  await toggleMenu(page, "View");
  await activate(page, "Waterfall");
  await toggleMenu(page, "View");
  await page.waitForTimeout(800);
  const waterfall = controlsFrom((await snapshot(page)).nodes);

  await toggleMenu(page, "View");
  await activate(page, "Wiggle plot");
  await toggleMenu(page, "View");
  await page.waitForTimeout(600);

  const merged = new Map<string, Record<string, unknown>>();
  for (const control of [...wiggle, ...waterfall]) {
    if (!merged.has(String(control.name))) merged.set(String(control.name), control);
  }
  return [...merged.values()].sort((a, b) => String(a.name).localeCompare(String(b.name)));
}

/// The Key / Mouse / Command reference, read out of the app's own help window.
///
/// Opened with `?`, the shortcut the window itself documents, rather than through
/// the menu — one fewer piece of machinery between the harness and the table, and it
/// proves the shortcut works as a side effect.
///
/// The rows are label pairs in an egui grid, so they arrive as a flat run of leaf
/// nodes: a heading, then shortcut, description, shortcut, description. Only the
/// headings tell one table from the next.
async function collectHelp(
  page: Page
): Promise<Record<string, { key: string; description: string }[]>> {
  // The walk left focus on a menu button, and a key pressed there never reaches
  // egui. Hand focus back to the canvas first — with the debug tree on, the proxies
  // take no pointer input, so a click on the canvas lands where it looks like it does.
  await page.locator("#the_canvas_id").click({ position: { x: 500, y: 400 } });
  await page.waitForTimeout(300);
  await page.keyboard.press("Shift+Slash");
  await page.waitForTimeout(1200);

  const rows = await page.evaluate(() => {
    const dialogs = [...document.querySelectorAll("#snuffler-a11y [role=dialog]")];
    const help = dialogs.find((d) => d.getAttribute("aria-label") === "Snuffler controls");
    if (!help) return [] as string[];
    return [...help.querySelectorAll("[data-ak]")]
      .filter((el) => el.querySelector("[data-ak]") === null)
      .map((el) => (el.textContent ?? "").trim())
      .filter((text) => text.length > 0);
  });
  if (!rows.length) throw new Error("the 'Snuffler controls' window did not open");

  const sections: Record<string, { key: string; description: string }[]> = {};
  let current = "";
  let pending: string | null = null;
  for (const text of rows) {
    if (["Key", "Mouse", "Command"].includes(text)) {
      current = text;
      sections[current] = [];
      pending = null;
      continue;
    }
    if (!current) continue;
    if (pending === null) {
      pending = text;
    } else {
      sections[current]!.push({ key: pending, description: text });
      pending = null;
    }
  }
  await page.keyboard.press("Shift+Slash");
  await page.waitForTimeout(300);
  return sections;
}

/// Keys whose effect this probe deliberately does not trigger.
///
/// `Ctrl+P` belongs to the browser, and Qt's `q`/`x` pair closes its viewer while a
/// page must not close its host tab.
/// `r` and `R` re-read files and re-import the Python bridge, which is slow and leaves
/// the page in a state later probes would have to recover from.
/// These deliberate differences are enforced through the parity register instead.
const UNPROBED_KEYS = new Set(["<ctrl> + p", "q", "r", "R", "x"]);

/// A fingerprint of everything the app currently publishes about itself.
///
/// Coarse on purpose: the question a key probe asks is only "did anything change",
/// and any change at all — a checkbox flipping, a window opening, a marker gaining a
/// colour — answers it.
function overlaySignature(): string {
  const root = document.querySelector("#snuffler-a11y");
  if (!root) return "";
  return [...root.querySelectorAll("[data-ak]")]
    .map((el) =>
      [
        el.getAttribute("role") ?? "",
        el.getAttribute("aria-label") ?? (el.children.length === 0 ? el.textContent : "") ?? "",
        el.getAttribute("aria-checked") ?? "",
        el.getAttribute("aria-valuenow") ?? "",
      ].join("|")
    )
    .join("\n");
}

/// Wait for the accessibility projection to stop changing between animation frames.
///
/// A key is credited only for a settled before/after difference, so an unrelated
/// asynchronous rebuild cannot turn an intentionally unbound key into a false match.
async function settledOverlaySignature(page: Page): Promise<string> {
  let previous = await page.evaluate(overlaySignature);
  let consecutiveMatches = 0;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.waitForTimeout(100);
    const current = await page.evaluate(overlaySignature);
    if (current === previous) {
      consecutiveMatches += 1;
      if (consecutiveMatches === 2) return current;
    } else {
      consecutiveMatches = 0;
      previous = current;
    }
  }
  throw new Error("the accessibility overlay did not settle within two seconds");
}

/// Focus a proxy and press Enter, sidestepping playwright's hit-target
/// stability wait: under machine load the overlay rebuilds faster than a
/// pointer click can prove the element still, and the keyboard path needs only
/// presence — the same reason `toggleMenu` is keyboard-driven.
async function pressProxy(page: Page, selector: string): Promise<void> {
  await page.$eval(selector, (el) => (el as HTMLElement).focus());
  await page.waitForTimeout(120);
  await page.keyboard.press("Enter");
}

/// Which keys the app answers to, asked by pressing them.
///
/// The mirror of the Qt side, which asks its widget the same question through its own
/// event handler. A key that changes nothing observable is reported as `inert` rather
/// than unbound — panning the view and scrolling tracks are real commands that the
/// accessibility layer simply does not describe, so "no effect seen here" is as far as
/// this evidence goes and the diff treats it that way.
/// Data, a marker table to watch, and one selected marker to act on.
///
/// Every part of that is a precondition, not scenery. Most of snuffler's key
/// vocabulary edits markers, and a key with nothing to edit does nothing — so probing
/// from an empty viewer would report two thirds of the alphabet as unbound. Selecting
/// the marker explicitly rather than relying on the pick having selected it keeps the
/// probe measuring the keys and not the picking path, which tier 3 covers.
async function probeSetup(page: Page): Promise<void> {
  // The synthetic pile arrives with the page (`?demo=1`), since the File menu does
  // not offer it.
  // Readiness comes from the application lifecycle rather than the status wording.
  await waitForAppReady(page);
  await pressProxy(page, `${OVERLAY} [aria-label="Snufflings"]`);
  await page.waitForTimeout(400);
  await pressProxy(page, `${OVERLAY} [role=checkbox][aria-label="Markers"]`);
  await page.waitForTimeout(600);
  await pressProxy(page, `${OVERLAY} [aria-label="Snufflings"]`);
  await page.waitForTimeout(400);
  await page.locator("#the_canvas_id").dblclick({ position: { x: 800, y: 400 }, force: true });
  await page.waitForTimeout(900);
  await page.locator("#the_canvas_id").click({ position: { x: 1200, y: 200 }, force: true });
  await page.keyboard.press("Shift+A");
  await page.waitForTimeout(500);
}

/// Put the app back the way `probeSetup` left it, without paying for a reload.
///
/// Close whatever opened, clear whatever the last key did to the markers, and pick a
/// fresh one. Returns false if it could not — the caller then reloads, which always
/// works and is only slow.
async function restoreBaseline(page: Page): Promise<boolean> {
  try {
    for (const close of await page.$$(`${OVERLAY} [aria-label="Close window"]`)) {
      await close.click({ timeout: 2000 }).catch(() => {});
      await page.waitForTimeout(200);
    }
    if (!(await page.$(`${OVERLAY} [role=dialog][aria-label="Markers"]`))) {
      await pressProxy(page, `${OVERLAY} [aria-label="Snufflings"]`);
      await page.waitForTimeout(300);
      await pressProxy(page, `${OVERLAY} [role=checkbox][aria-label="Markers"]`);
      await page.waitForTimeout(400);
      await pressProxy(page, `${OVERLAY} [aria-label="Snufflings"]`);
      await page.waitForTimeout(300);
    }
    const clear = await page.$(`${OVERLAY} [aria-label="Clear all"]`);
    if (clear) {
      await clear.click({ timeout: 2000 });
      const confirm = page.locator(`${OVERLAY} [aria-label="Really clear all?"]`).first();
      await confirm.waitFor({ state: "attached", timeout: 2000 });
      await confirm.click({ timeout: 2000 });
      await page.waitForTimeout(400);
    }
    await page.locator("#the_canvas_id").dblclick({ position: { x: 800, y: 400 }, force: true });
    await page.waitForTimeout(700);
    await page.locator("#the_canvas_id").click({ position: { x: 1200, y: 200 }, force: true });
    await page.keyboard.press("Shift+A");
    await page.waitForTimeout(400);
    return true;
  } catch {
    return false;
  }
}

async function probeKeys(page: Page, tokens: string[]): Promise<Record<string, string>> {
  await probeSetup(page);
  let baseline = await settledOverlaySignature(page);

  const out: Record<string, string> = {};
  for (const token of tokens) {
    if (UNPROBED_KEYS.has(token)) {
      out[token] = "unprobed";
      continue;
    }
    // A pointer click on the canvas is what hands the app back its shortcuts after
    // anything else has had focus; see scenario_web.ts.
    await page.locator("#the_canvas_id").click({ position: { x: 1200, y: 200 }, force: true });
    await page.waitForTimeout(200);
    const before = await settledOverlaySignature(page);
    await pressKey(page.keyboard, token);
    await page.waitForTimeout(450);
    const after = await settledOverlaySignature(page);
    out[token] = before === after ? "inert" : "changed";

    // Every key is measured from the same starting state, or the answer depends on
    // what the previous key happened to leave behind: press backspace and the marker
    // the next key needed is gone.
    if (before !== after) {
      const restored = await restoreBaseline(page);
      if (!restored || (await settledOverlaySignature(page)) !== baseline) {
        await boot(page, "pointer");
        await probeSetup(page);
        baseline = await settledOverlaySignature(page);
      }
    }
  }
  return out;
}

async function main(): Promise<void> {
  const outIndex = process.argv.indexOf("--out");
  const outPath = outIndex >= 0 ? process.argv[outIndex + 1] : undefined;
  const screenshotsIndex = process.argv.indexOf("--screenshots");
  const screenshotsPath = screenshotsIndex >= 0 ? process.argv[screenshotsIndex + 1] : undefined;
  const probeIndex = process.argv.indexOf("--probe-keys");
  // The tokens to press, handed in by run_inventory.py: whatever Qt turned out to
  // bind, so the probe asks about exactly the keys the diff is about to ask about.
  const probeTokens: string[] =
    probeIndex >= 0 ? JSON.parse(readFileSync(process.argv[probeIndex + 1]!, "utf8")) : [];

  const browser = await launchParityBrowser();
  const page = await browser.newPage();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await installTreeAccumulator(page);
  try {
    await boot(page);
    await reportParityRenderer(page);
    const base = await snapshot(page);
    if (screenshotsPath) await saveWebScreenshots(page, screenshotsPath);
    const menus = await collectMenus(page, base.nodes);
    const controls = await collectControls(page, base.nodes);
    const help = await collectHelp(page);
    // Last, and after a fresh boot: the probe presses real keys, so everything read
    // off an undisturbed app is already in hand by the time it runs.
    let keys: Record<string, string> = {};
    if (probeTokens.length) {
      await boot(page, "pointer");
      keys = await probeKeys(page, probeTokens);
    }
    const inventory = {
      side: "web",
      menus,
      controls,
      help,
      keys,
    };
    const text = `${JSON.stringify(inventory, null, 2)}\n`;
    if (outPath) {
      writeFileSync(outPath, text);
      const changed = Object.values(keys).filter((v) => v === "changed").length;
      console.log(
        `web inventory: ${menus.length} menu entries, ` +
          `${inventory.controls.length} controls, ` +
          `${(help.Key ?? []).length} documented keys, ` +
          `${changed}/${probeTokens.length} keys observed to do something -> ${outPath}`
      );
    } else {
      process.stdout.write(text);
    }
  } finally {
    await browser.close();
  }
}

await main();
