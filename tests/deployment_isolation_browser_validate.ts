// Test real nested service-worker lifecycles and app persistence in one browser origin.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { type AddressInfo } from "node:net";
import { type Page } from "playwright";
import {
  BASE_URL,
  BOOT_TIMEOUT_MS,
  launchParityBrowser,
  waitForAppReady,
} from "./parity/parity_browser";

const workerSource = await readFile(
  new URL("../web/coi-serviceworker.js", import.meta.url),
  "utf8"
);
const stable = "/snuffler-web/";
const latest = `${stable}latest/`;
const appStable = "/app/";
const appLatest = `${appStable}latest/`;
const revisions = new Map([
  [stable, 1],
  [latest, 1],
]);

// This bounded fixture proxies the already-running app; it never starts a second
// Trunk service or modifies the development origin's worker registrations.
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url ?? "/", "http://fixture.localhost");
    const fixtureBase = url.pathname.startsWith(latest) ? latest : stable;
    if (url.pathname.startsWith(stable)) {
      response.setHeader("Cache-Control", "no-store");
      if (url.pathname.endsWith("coi-serviceworker.js")) {
        response.setHeader("Content-Type", "text/javascript");
        response.end(
          workerSource +
            `\nself.addEventListener("message", event => {
          if (event.data.type === "fixture-revision") event.ports[0].postMessage(${revisions.get(fixtureBase)});
        });`
        );
      } else if (url.pathname === stable || url.pathname === latest) {
        response.setHeader("Content-Type", "text/html");
        response.end(`<!doctype html><base href="${fixtureBase}">
          <script>window.coi = {
            quiet: true, shouldRegister: () => true,
            coepCredentialless: () => ${fixtureBase === latest},
            doReload: () => {
              const key = "fixture-reloads:${fixtureBase}";
              sessionStorage.setItem(key, String(Number(sessionStorage.getItem(key)) + 1));
              location.reload();
            }
          };</script><script src="coi-serviceworker.js"></script><p>fixture</p>`);
      } else {
        response.end("asset");
      }
      return;
    }

    const appBase = url.pathname.startsWith(appLatest) ? appLatest : appStable;
    const upstreamPath = url.pathname.startsWith(appBase)
      ? url.pathname.slice(appBase.length)
      : url.pathname.slice(1);
    const upstream = await fetch(new URL(upstreamPath + url.search, BASE_URL), {
      signal: AbortSignal.timeout(60_000),
    });
    response.statusCode = upstream.status;
    response.setHeader(
      "Content-Type",
      upstream.headers.get("Content-Type") ?? "application/octet-stream"
    );
    response.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    response.setHeader("Cross-Origin-Embedder-Policy", "credentialless");
    response.setHeader("Cross-Origin-Resource-Policy", "cross-origin");
    if (!upstreamPath) {
      const html = (await upstream.text()).replace(
        /<base href="[^"]*"\s*\/>/,
        `<base href="${appBase}" />`
      );
      response.end(html);
    } else {
      response.end(Buffer.from(await upstream.arrayBuffer()));
    }
  } catch (err) {
    response.statusCode = 502;
    response.end(String(err));
  }
});
await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;

async function controlled(page: Page, base: string): Promise<void> {
  // Registration and its automatic reload can outlive the initial DOMContentLoaded.
  await page.waitForFunction(
    (script) => navigator.serviceWorker.controller?.scriptURL === script && crossOriginIsolated,
    `${origin}${base}coi-serviceworker.js`,
    { timeout: 30_000 }
  );
}

async function revision(page: Page): Promise<number> {
  return page.evaluate(async () => {
    const channel = new MessageChannel();
    const result = new Promise<number>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error("worker revision handshake timed out")),
        5000
      );
      channel.port1.onmessage = (event: MessageEvent<number>) => {
        clearTimeout(timer);
        resolve(event.data);
      };
    });
    navigator.serviceWorker.controller?.postMessage({ type: "fixture-revision" }, [channel.port2]);
    return result;
  });
}

async function activate(page: Page, name: string): Promise<void> {
  await page
    .getByRole("button", { name, exact: true })
    .first()
    .evaluate((element) => (element as HTMLElement).click());
  await waitForAppReady(page);
}

async function openApp(page: Page, base: string): Promise<void> {
  console.log(`boot ${base}`);
  await page.goto(`${origin}${base}?a11y=pointer&example=bonin-2015-05-30-m7.8-multiloc`);
  await page.getByText("Importing pyrocko...", { exact: true }).waitFor({ state: "hidden" });
  await waitForAppReady(page);
  if (await page.getByRole("button", { name: "Close window", exact: true }).count()) {
    await activate(page, "Close window");
  }
  console.log(`ready ${base}`);
}

async function setClip(page: Page, desired: boolean): Promise<void> {
  await activate(page, "View");
  const checkbox = page.getByRole("checkbox", { name: "Clip traces", exact: true });
  if ((await checkbox.getAttribute("aria-checked")) !== String(desired)) {
    await checkbox.evaluate((element) => (element as HTMLElement).click());
    await waitForAppReady(page);
  }
  assert.equal(await checkbox.getAttribute("aria-checked"), String(desired));
  await activate(page, "View");
}

async function command(page: Page, text: string): Promise<void> {
  await page.keyboard.press(":");
  await page.getByRole("textbox", { name: ":", exact: true }).waitFor();
  await waitForAppReady(page);
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
  await waitForAppReady(page);
}

async function closeServer(): Promise<void> {
  server.closeAllConnections();
  await new Promise<void>((resolve, reject) =>
    server.close((err) => (err ? reject(err) : resolve()))
  );
}

const browser = await launchParityBrowser().catch(async (err: unknown) => {
  await closeServer();
  throw err;
});
try {
  const context = await browser.newContext();
  try {
    const parent = await context.newPage();
    const child = await context.newPage();
    await parent.goto(origin + stable);
    await controlled(parent, stable);
    await child.goto(origin + latest);
    await controlled(child, latest);
    assert.equal(await revision(parent), 1);
    assert.equal(await revision(child), 1);

    // Distinct policies make accidental controller handshakes observable.
    assert.equal(
      await parent.evaluate(async () =>
        (await fetch("asset")).headers.get("Cross-Origin-Embedder-Policy")
      ),
      "require-corp"
    );
    assert.equal(
      await child.evaluate(async () =>
        (await fetch("asset")).headers.get("Cross-Origin-Embedder-Policy")
      ),
      "credentialless"
    );
    const parentReloads = await parent.evaluate(() =>
      sessionStorage.getItem(`fixture-reloads:${location.pathname}`)
    );
    const parentController = await parent.evaluate(
      () => navigator.serviceWorker.controller?.scriptURL
    );

    // Updating latest must activate latest's new worker and leave stable untouched.
    revisions.set(latest, 2);
    await child.evaluate(async () => (await navigator.serviceWorker.getRegistration())?.update());
    await child.waitForFunction(async () => {
      const response = await fetch("asset");
      return crossOriginIsolated && response.ok;
    });
    await controlled(child, latest);
    // The controller's script URL stays the same through an update, so wait for
    // the new worker's revision rather than merely observing a controller.
    await child.waitForFunction(
      () =>
        new Promise<boolean>((resolve) => {
          const channel = new MessageChannel();
          const timer = setTimeout(() => resolve(false), 1000);
          channel.port1.onmessage = (event) => {
            clearTimeout(timer);
            resolve(event.data === 2);
          };
          navigator.serviceWorker.controller?.postMessage({ type: "fixture-revision" }, [
            channel.port2,
          ]);
        }),
      null,
      { timeout: 30_000 }
    );
    assert.equal(await revision(child), 2);
    assert.equal(await revision(parent), 1);
    assert.equal(
      await parent.evaluate(() => navigator.serviceWorker.controller?.scriptURL),
      parentController
    );
    assert.equal(
      await parent.evaluate(() => sessionStorage.getItem(`fixture-reloads:${location.pathname}`)),
      parentReloads
    );
    assert.deepEqual(
      await parent.evaluate(async () =>
        (await navigator.serviceWorker.getRegistrations())
          .map((entry) => new URL(entry.scope).pathname)
          .sort()
      ),
      [stable, latest]
    );
    console.log("ok   nested worker installation, handshake, and update isolation");
  } finally {
    await context.close();
  }

  const appContext = await browser.newContext();
  try {
    // Origin-global pre-isolation values must never seed either deployment.
    await appContext.addInitScript(() => {
      localStorage.setItem("snuffler.settings.clip_traces", "false");
      localStorage.setItem("snuffler.command_history", "markers 9");
    });
    const page = await appContext.newPage();
    page.setDefaultTimeout(BOOT_TIMEOUT_MS);
    page.on("pageerror", (err) => console.error(`app error: ${err.message}`));
    await openApp(page, appStable);
    assert.equal(
      await page.evaluate(() => localStorage.getItem("snuffler:/app/:settings.clip_traces")),
      "true"
    );
    await setClip(page, false);
    await command(page, "markers 0");
    await page.waitForFunction(
      () => localStorage.getItem("snuffler:/app/:command_history") === "markers 0"
    );

    await openApp(page, appLatest);
    await setClip(page, true);
    await command(page, "markers 1");
    await page.waitForFunction(
      () => localStorage.getItem("snuffler:/app/latest/:command_history") === "markers 1"
    );

    for (const { base, clip, history } of [
      { base: appStable, clip: false, history: "markers 0" },
      { base: appLatest, clip: true, history: "markers 1" },
    ]) {
      await openApp(page, base);
      await activate(page, "View");
      assert.equal(
        await page
          .getByRole("checkbox", { name: "Clip traces", exact: true })
          .getAttribute("aria-checked"),
        String(clip)
      );
      await activate(page, "View");
      await page.keyboard.press(":");
      const textbox = page.getByRole("textbox", { name: ":", exact: true });
      await textbox.waitFor();
      await waitForAppReady(page);
      await page.keyboard.press("ArrowUp");
      await page.waitForFunction(
        (expected) =>
          [...document.querySelectorAll('#snuffler-a11y [role="textbox"]')].some(
            (element) => element.textContent === expected
          ),
        history
      );
      await page.keyboard.press("Escape");
    }
    assert.deepEqual(
      await page.evaluate(() =>
        Object.fromEntries(
          Object.entries(localStorage).filter(([key]) => key.endsWith(":command_history"))
        )
      ),
      {
        "snuffler:/app/:command_history": "markers 0",
        "snuffler:/app/latest/:command_history": "markers 1",
      }
    );
    console.log("ok   same-origin deployment settings and history survive independent reloads");
  } finally {
    await appContext.close();
  }
} finally {
  await browser.close();
  await closeServer();
}
