import { readFileSync, writeFileSync } from "node:fs";

const paths = process.argv.slice(2);
for (const path of paths) {
  patch(path);
}

function patch(path) {
  let html = readFileSync(path, "utf8");
  html = html.replace(/<link rel="preload"[^>]+_bg\.wasm[^>]*>/g, "");

  const loader =
    /<script type="module">\s*import init, \* as bindings from '([^']+)';\s*const wasm = await init\(\{ module_or_path: '([^']+_bg\.wasm)' \}\);\s*window\.wasmBindings = bindings;\s*dispatchEvent\(new CustomEvent\("TrunkApplicationStarted", \{detail: \{wasm\}\}\)\);\s*<\/script>/;
  const match = html.match(loader);
  if (!match) {
    if (!html.includes("snufflerWasmResponse(")) {
      console.warn(`[loader] no Trunk wasm loader matched in ${path}`);
    }
    writeFileSync(path, html);
    return;
  }

  const [, jsPath, wasmPath] = match;
  html = html.replace(loader, patchedLoader(jsPath, wasmPath));
  writeFileSync(path, html);
  console.log(`[loader] patched wasm download progress in ${path}`);
}

function patchedLoader(jsPath, wasmPath) {
  return `<script type="module">
import init, * as bindings from '${jsPath}';

function snufflerReport(fraction, label) {
    if (window.__snufflerProgress) window.__snufflerProgress(fraction, label);
}

async function snufflerWasmResponse(url) {
    snufflerReport(0.01, "Downloading app...");
    const response = await fetch(url);
    if (!response.ok) {
        throw new Error("fetch " + url + ": " + response.status + " " + response.statusText);
    }
    const total = Number(response.headers.get("content-length") || 0);
    if (!response.body || !Number.isFinite(total) || total <= 0) {
        snufflerReport(0.42, "Downloading app...");
        return response;
    }

    const [metered, body] = response.body.tee();
    let loaded = 0;
    void (async () => {
        const reader = metered.getReader();
        for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            if (!value) continue;
            loaded += value.byteLength;
            const fraction = 0.01 + 0.41 * Math.max(0, Math.min(1, loaded / total));
            snufflerReport(fraction, "Downloading app...");
        }
    })().catch((e) => console.warn("[loader] progress meter failed:", e));

    return new Response(body, {
        status: response.status,
        statusText: response.statusText,
        headers: response.headers,
    });
}

// Report a failed instantiate instead of leaving the bar frozen.
// The app wasm is linked --shared-memory, so an engine that will not grant shared
// memory rejects here — WebKit without cross-origin isolation does exactly that — and
// the page otherwise just stops with no explanation.
// Rethrown so the failure still shows up as an error, rather than being swallowed into
// a silent no-op.
let wasm;
try {
    wasm = await init({ module_or_path: snufflerWasmResponse('${wasmPath}') });
} catch (e) {
    if (window.__snufflerLoadFailed) window.__snufflerLoadFailed(e);
    throw e;
}
snufflerReport(0.44, "Starting app...");

window.wasmBindings = bindings;

dispatchEvent(new CustomEvent("TrunkApplicationStarted", {detail: {wasm}}));

</script>`;
}
