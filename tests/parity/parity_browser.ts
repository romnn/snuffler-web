import { mkdirSync } from "node:fs";
import { join } from "node:path";

import { chromium, type Browser, type Page } from "playwright";

export const BASE_URL = process.env.SNUFFLER_BROWSER_URL || "http://127.0.0.1:17683/";
export const BOOT_TIMEOUT_MS = Number(process.env.SNUFFLER_BROWSER_TEST_TIMEOUT_MS || 300_000);

const HEADLESS = process.env.SNUFFLER_BROWSER_HEADLESS !== "0";

export async function launchParityBrowser(): Promise<Browser> {
  if (!HEADLESS) return chromium.launch({ headless: false });

  try {
    // The default headless-shell binary forces SwiftShader.
    // Playwright's Chromium channel uses the full browser's modern headless mode,
    // which can retain hardware acceleration without changing browser semantics.
    return await chromium.launch({ channel: "chromium", headless: true });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.warn(
      `modern headless Chromium failed to launch; falling back to the software-rendered ` +
        `headless shell, so browser parity tests may be slow:\n${message}`
    );
    return chromium.launch({ headless: true });
  }
}

export async function reportParityRenderer(page: Page): Promise<void> {
  const renderer = await page.evaluate(() => {
    const canvas = document.createElement("canvas");
    const context = canvas.getContext("webgl2") ?? canvas.getContext("webgl");
    if (!context) return null;

    const info = context.getExtension("WEBGL_debug_renderer_info");
    return String(context.getParameter(info ? info.UNMASKED_RENDERER_WEBGL : context.RENDERER));
  });

  if (!renderer || /lavapipe|llvmpipe|software|swiftshader/i.test(renderer)) {
    console.warn(
      `browser parity renderer: ${renderer ?? "unavailable"}; ` +
        "software rendering is active, so the browser tiers may be slow"
    );
  } else {
    console.log(`browser parity renderer: ${renderer}`);
  }
}

async function waitForUiFrames(page: Page, count: number): Promise<void> {
  await page.evaluate(async (frames) => {
    for (let frame = 0; frame < frames; frame += 1) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    }
  }, count);
}

export async function waitForAppReady(page: Page): Promise<void> {
  // Let an action reach egui before checking the attribute it may invalidate.
  // Browser frames naturally stretch under load, unlike a fixed millisecond delay.
  await waitForUiFrames(page, 2);
  await page
    .locator('#the_canvas_id[data-snuffler-ready="true"]')
    .waitFor({ state: "attached", timeout: BOOT_TIMEOUT_MS });
}

export async function saveWebScreenshots(page: Page, directory: string): Promise<void> {
  await waitForAppReady(page);
  mkdirSync(directory, { recursive: true });
  await page.screenshot({ path: join(directory, "web-full.png"), fullPage: true });

  // Force the application's normal anchor-download fallback so Playwright can
  // capture the exact PNG bytes without an operating-system file picker.
  await page.evaluate(() => {
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: undefined,
    });
  });
  const fileMenu = page.getByRole("button", { name: "File", exact: true }).first();
  await fileMenu.evaluate((element) => (element as HTMLElement).click());

  const exportButton = page.getByRole("button", { name: "Save image (PNG)…", exact: true }).first();
  await exportButton.waitFor({ state: "visible" });
  const download = page.waitForEvent("download", { timeout: BOOT_TIMEOUT_MS });
  await exportButton.evaluate((element) => (element as HTMLElement).click());
  await exportButton.waitFor({ state: "hidden", timeout: BOOT_TIMEOUT_MS });
  await (await download).saveAs(join(directory, "web-waveforms.png"));
}
