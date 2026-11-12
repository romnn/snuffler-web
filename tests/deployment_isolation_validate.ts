// Exercise the deployed COI script with separate controllers and shared origin storage.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";

const source = await readFile(new URL("../web/coi-serviceworker.js", import.meta.url), "utf8");
const origin = "https://example.test";
const stable = `${origin}/snuffler-web/`;
const latest = `${stable}latest/`;

class Controller {
  messages: unknown[] = [];
  constructor(public scriptURL: string) {}
  postMessage(message: unknown): void {
    this.messages.push(message);
  }
}

async function page(options: {
  base: string;
  controller?: Controller;
  active?: Controller;
  isolated?: boolean;
  storage?: Map<string, string>;
  refuseStorage?: boolean;
}) {
  const storage = options.storage ?? new Map<string, string>();
  const registration = Object.assign(new EventTarget(), {
    scope: options.base,
    active: options.active ?? null,
  });
  const registrations: { script: string; scope: string }[] = [];
  const reloads: string[] = [];
  const workers = Object.assign(new EventTarget(), {
    controller: options.controller ?? null,
    async register(script: string, { scope }: { scope: string }) {
      registrations.push({ script, scope });
      return registration;
    },
  });
  const sessionStorage = {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  };
  const window = {
    document: { currentScript: { src: `${options.base}coi-serviceworker.js` } },
    crossOriginIsolated: options.isolated ?? true,
    isSecureContext: true,
    coi: {
      quiet: true,
      shouldRegister: () => true,
      doReload: (reason: string) => reloads.push(reason),
    },
    get sessionStorage() {
      if (options.refuseStorage) throw new Error("storage denied");
      return sessionStorage;
    },
  };
  runInNewContext(source, { window, navigator: { serviceWorker: workers }, URL, console });
  await Promise.resolve();
  return { storage, registration, registrations, reloads, workers };
}

{
  // The stable worker can control the first visit to latest and already supply COI.
  // Latest must still install its own worker without configuring the stable worker.
  const parent = new Controller(`${stable}coi-serviceworker.js`);
  const child = new Controller(`${latest}coi-serviceworker.js`);
  const harness = await page({ base: latest, controller: parent });
  assert.deepEqual(parent.messages, []);
  assert.deepEqual(harness.registrations, [{ script: child.scriptURL, scope: latest }]);
  assert.deepEqual(harness.reloads, []);

  // Discovering an update is too early to reload; activation owns that transition.
  harness.registration.dispatchEvent(new Event("updatefound"));
  assert.deepEqual(harness.reloads, []);
  harness.workers.dispatchEvent(new Event("controllerchange"));
  assert.deepEqual(harness.reloads, []);
  assert.deepEqual(parent.messages, []);
  harness.workers.controller = child;
  harness.workers.dispatchEvent(new Event("controllerchange"));
  assert.equal(child.messages.length, 1);
  assert.equal((child.messages[0] as { deployment: string }).deployment, latest);
  assert.deepEqual(harness.reloads, ["controllerchange"]);
  harness.workers.dispatchEvent(new Event("controllerchange"));
  assert.deepEqual(harness.reloads, ["controllerchange"]);
}

{
  // A server that supplies native isolation needs no service worker at all.
  const harness = await page({ base: latest });
  assert.deepEqual(harness.registrations, []);
}

{
  // First acquisition claims an unisolated response that the new worker never served.
  // It must reload with the requested policy rather than record a false COEP failure.
  const harness = await page({ base: latest, isolated: false });
  const child = new Controller(`${latest}coi-serviceworker.js`);
  harness.workers.controller = child;
  harness.workers.dispatchEvent(new Event("controllerchange"));
  assert.equal((child.messages[0] as { value: boolean }).value, true);
  assert.deepEqual(harness.reloads, ["controllerchange"]);
  assert.equal(harness.storage.has("snuffler:/snuffler-web/latest/:coi.coepHasFailed"), false);
}

{
  const parent = new Controller(`${stable}coi-serviceworker.js`);
  const child = new Controller(`${latest}coi-serviceworker.js`);
  const harness = await page({ base: latest, controller: parent, active: child });
  assert.deepEqual(harness.reloads, ["notcontrolling"]);
  assert.deepEqual(parent.messages, []);
}

{
  // One tab can visit both deployments, so sessionStorage must be scoped too.
  const storage = new Map<string, string>();
  const owner = new Controller(`${stable}coi-serviceworker.js`);
  const first = await page({ base: stable, controller: owner, isolated: false, storage });
  assert.deepEqual(first.reloads, ["coepdegrade"]);
  assert.equal(storage.get("snuffler:/snuffler-web/:coi.coepHasFailed"), "true");
  const child = new Controller(`${latest}coi-serviceworker.js`);
  await page({ base: latest, controller: child, storage });
  assert.equal((child.messages[0] as { value: boolean }).value, true);
  assert.equal(storage.get("snuffler:/snuffler-web/:coi.reloadedBySelf"), "coepdegrade");
  assert.equal(storage.has("snuffler:/snuffler-web/latest/:coi.coepHasFailed"), false);
}

{
  const child = new Controller(`${latest}coi-serviceworker.js`);
  const harness = await page({ base: latest, controller: child, refuseStorage: true });
  assert.equal(harness.registrations.length, 1);
  assert.equal(child.messages.length, 1);
}

{
  const handlers = new Map<string, (event: any) => void>();
  let unregistered = false;
  runInNewContext(source, {
    self: {
      registration: {
        scope: latest,
        unregister: async () => {
          unregistered = true;
        },
      },
      clients: { matchAll: async () => [] },
      addEventListener: (name: string, handler: (event: any) => void) =>
        handlers.set(name, handler),
    },
    Headers,
    Request,
    Response,
    fetch: async () => new Response("asset"),
    console,
  });
  async function policy(): Promise<string | null> {
    let response: Promise<Response> | undefined;
    handlers.get("fetch")?.({
      request: new Request(`${latest}asset`),
      respondWith: (result: Promise<Response>) => {
        response = result;
      },
    });
    assert.ok(response);
    return (await response).headers.get("Cross-Origin-Embedder-Policy");
  }
  // A wrong deployment cannot change this worker's policy or unregister it.
  handlers.get("message")?.({
    data: { deployment: stable, type: "coepCredentialless", value: true },
  });
  assert.equal(await policy(), "require-corp");
  handlers.get("message")?.({ data: { deployment: stable, type: "deregister" } });
  assert.equal(unregistered, false);
  handlers.get("message")?.({
    data: { deployment: latest, type: "coepCredentialless", value: true },
  });
  assert.equal(await policy(), "credentialless");
}

console.log("all deployment-isolation lifecycle checks passed");
