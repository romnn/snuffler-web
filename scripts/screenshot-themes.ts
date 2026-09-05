// Capture the served app's palette matrix in isolated browser contexts.
// Run `bun scripts/screenshot-themes.ts --help` for filters and output conventions.
import { appendFile, mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { type Page } from "playwright";
import {
  BASE_URL,
  BOOT_TIMEOUT_MS,
  closeWebMenu,
  launchParityBrowser,
  reportParityRenderer,
  saveWebScreenshot,
  waitForAppReady,
} from "../tests/parity/parity_browser";
import { pressKey } from "../tests/parity/parity_keys";

const { values } = parseArgs({
  options: {
    examples: { type: "string" },
    themes: { type: "string" },
    modes: { type: "string", default: "light,dark" },
    boxes: { type: "string", default: "on,off" },
    traces: { type: "string", default: "on,off" },
    grouping: { type: "string", default: "grouped,separate" },
    framing: { type: "string", default: "fitted,zoomed" },
    url: { type: "string", default: BASE_URL },
    output: { type: "string", default: "tests/parity/.artifacts/screenshots/themes" },
    help: { type: "boolean", default: false },
  },
});

if (values.help) {
  console.log(`Usage: bun scripts/screenshot-themes.ts [options]

Defaults: every served example, every theme, light/dark, boxes on/off,
colour traces on/off, grouped/separate locations, fitted/zoomed framing.
Snuffler Qt is light-only; its unsupported dark combinations are skipped.

Comma-separated filters:
  --examples <slugs>             Default: served examples index
  --themes <names>               Default: names discovered in the View menu
  --modes light,dark             --boxes on,off
  --traces on,off                --grouping grouped,separate
  --framing fitted,zoomed
  --url <base URL>               Default: http://127.0.0.1:17683/
  --output <directory>           Default: tests/parity/.artifacts/screenshots/themes (gitignored)
  SNUFFLER_BROWSER_HEADLESS=0    Show the capture browser (same as parity tests)

Output: <directory>/<theme-name>/<example>__<mode>__boxes-<on|off>__traces-<on|off>__<grouping>__<framing>.png
Control states and URLs are appended to captures.jsonl in the output directory.
The viewport is 1440×1000 at device scale 1; zoomed means a vertical canvas drag
that narrows the time window around 40% of the fitted extent.
Marker kind 0 is selected; all eight swatches remain visible in the bottom bar.
The script uses fresh contexts, leaves personal browser settings alone, and
overwrites matching filenames so repeated runs remain easy to compare.

Example:
  bun scripts/screenshot-themes.ts --examples tohoku-2011-03-11-m9.1-clipping --themes Fieldbook --grouping grouped
`);
  process.exit(0);
}

function choices(input: string, allowed?: string[]): string[] {
  const result = [...new Set(input.split(",").map((s) => s.trim()))];
  if (result.some((s) => !s || (allowed && !allowed.includes(s)))) {
    throw new Error(`Invalid selection ${JSON.stringify(input)}; expected ${allowed?.join(", ")}`);
  }
  return result;
}

function slug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9.-]+/g, "-");
}

const modes = choices(values.modes, ["light", "dark"]);
const boxes = choices(values.boxes, ["on", "off"]);
const traces = choices(values.traces, ["on", "off"]);
const groupings = choices(values.grouping, ["grouped", "separate"]);
const framings = choices(values.framing, ["fitted", "zoomed"]);
const output = resolve(values.output);
const overlay = "#snuffler-a11y";

async function activate(page: Page, name: string): Promise<void> {
  // Synthetic activation uses the AccessKit action without a canvas press leaking
  // through when the overlay rebuilds between pointer-down and pointer-up.
  await page.locator(`${overlay} [aria-label=${JSON.stringify(name)}]`).evaluate((el) => {
    (el as HTMLElement).click();
  });
  await waitForAppReady(page);
}

async function viewChoice(page: Page, name: string): Promise<void> {
  await activate(page, "View");
  await activate(page, name);
  await closeWebMenu(page, "View", "Clip traces");
}

async function checkbox(page: Page, name: string, desired: boolean): Promise<boolean> {
  const control = page.getByRole("checkbox", { name, exact: true });
  if ((await control.getAttribute("aria-checked")) !== String(desired)) {
    await activate(page, name);
  }
  const actual = await control.getAttribute("aria-checked");
  if (actual !== String(desired)) throw new Error(`${name}: wanted ${desired}, got ${actual}`);
  return desired;
}

async function main(): Promise<void> {
  const browser = await launchParityBrowser();
  let captured = 0;
  try {
    await mkdir(output, { recursive: true });
    const response = await browser.newContext();
    let available: string[];
    try {
      const index = await response.request.get(
        new URL("assets/examples/examples.json", values.url).href
      );
      if (!index.ok()) throw new Error(`Examples index returned ${index.status()}`);
      available = (await index.json()).examples.map((example: { slug: string }) => example.slug);
    } finally {
      await response.close();
    }
    const examples = values.examples ? choices(values.examples, available) : available;
    for (const example of examples) {
      const context = await browser.newContext({
        viewport: { width: 1440, height: 1000 },
        deviceScaleFactor: 1,
      });
      try {
        const page = await context.newPage();
        page.setDefaultTimeout(BOOT_TIMEOUT_MS);
        const errors: string[] = [];
        page.on("pageerror", (error) => errors.push(error.message));
        const url = new URL(values.url);
        url.searchParams.set("a11y", "pointer");
        url.searchParams.set("example", example);
        await page.goto(url.href);
        await reportParityRenderer(page);
        await page.getByText("Importing pyrocko...", { exact: true }).waitFor({ state: "hidden" });
        await waitForAppReady(page);
        const close = page.locator(`${overlay} [aria-label="Close window"]`);
        if (await close.count()) await activate(page, "Close window");
        await activate(page, "View");
        const labels = await page
          .locator(`${overlay} [aria-label]`)
          .evaluateAll((els) => els.map((el) => el.getAttribute("aria-label") ?? ""));
        const themeNames = labels.slice(
          labels.indexOf("Snuffler Qt"),
          labels.indexOf("Clip traces")
        );
        if (!themeNames.length) throw new Error("No themes found in the View menu");
        const themes = values.themes ? choices(values.themes, themeNames) : themeNames;
        await closeWebMenu(page, "View", "Clip traces");

        for (const theme of themes) {
          await viewChoice(page, theme);
          await page.waitForFunction(
            (expected) => localStorage.getItem("snuffler.settings.wiggle_theme") === expected,
            theme
          );
          const directory = resolve(output, slug(theme));
          await mkdir(directory, { recursive: true });
          for (const mode of modes) {
            if (theme === "Snuffler Qt" && mode === "dark") continue;
            if (theme !== "Snuffler Qt")
              await viewChoice(page, mode === "dark" ? "🌙 Dark" : "☀ Light");
            if (theme !== "Snuffler Qt")
              await page.waitForFunction(
                (expected) =>
                  localStorage.getItem("snuffler.settings.theme_preference") === expected,
                mode
              );
            for (const grouping of groupings) {
              await activate(page, "Sorting");
              await activate(
                page,
                grouping === "grouped"
                  ? "Network, Station, Channel (by Location)"
                  : "Network, Station, Location, Channel"
              );
              await closeWebMenu(page, "Sorting", "Network, Station, Location, Channel");
              for (const showBoxes of boxes) {
                for (const colorTraces of traces) {
                  await activate(page, "View");
                  const state = {
                    boxes: await checkbox(page, "Show boxes", showBoxes === "on"),
                    colorTraces: await checkbox(page, "Color traces", colorTraces === "on"),
                  };
                  await closeWebMenu(page, "View", "Clip traces");
                  for (const framing of framings) {
                    await viewChoice(page, "Reset view to full extent");
                    await pressKey(page.keyboard, "=");
                    await waitForAppReady(page);
                    if (framing === "zoomed") {
                      await page.mouse.move(576, 300);
                      await page.mouse.down();
                      await page.mouse.move(576, 540, { steps: 20 });
                      await page.mouse.up();
                      await waitForAppReady(page);
                    }
                    await page.mouse.move(1435, 995);
                    await waitForAppReady(page);
                    if (errors.length) throw new Error(`Browser errors: ${errors.join("; ")}`);
                    const stem = `${slug(example)}__${mode}__boxes-${showBoxes}__traces-${colorTraces}__${grouping}__${framing}`;
                    const path = resolve(directory, `${stem}.png`);
                    await saveWebScreenshot(page, path);
                    await appendFile(
                      resolve(output, "captures.jsonl"),
                      JSON.stringify({
                        path,
                        example,
                        theme,
                        mode,
                        grouping,
                        framing,
                        ...state,
                        url: url.href,
                        viewport: page.viewportSize(),
                        capturedAt: new Date().toISOString(),
                      }) + "\n"
                    );
                    captured += 1;
                    console.log(`[${captured}] ${path}`);
                  }
                }
              }
            }
          }
        }
      } finally {
        await context.close();
      }
    }
    console.log(`Captured ${captured} screenshots in ${output}`);
  } finally {
    await browser.close();
  }
}

await main();
