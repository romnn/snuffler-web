// Thin browser-side client for the Pyodide worker. Rust calls this module via
// wasm-bindgen; this module keeps DOM-only work on the main thread and sends
// protobuf request bytes plus transferable buffers to web/bridge_worker.js.

let worker = null;
let nextRequestId = 1;
const pending = new Map();

function workerUrl() {
  return new URL("bridge_worker.js", document.baseURI).href;
}

function ensureWorker() {
  if (worker) return worker;
  // Pyodide 314 rejects classic workers on BOTH paths — the threaded build
  // spawns its pthread workers as ES modules, and even the stock single-threaded
  // 314 CDN build refuses a classic worker — so always use a module worker.
  worker = new Worker(workerUrl(), { type: "module" });
  worker.onmessage = (event) => {
    const msg = event.data || {};
    if (msg.type === "progress") {
      prog(msg.frac, msg.label);
      return;
    }
    if (msg.type === "runtime") {
      try {
        if (typeof window !== "undefined" && window.__snufflerRuntime) {
          window.__snufflerRuntime(msg);
        }
      } catch (_e) {
        // The badge is cosmetic; never let it break the bridge.
      }
      return;
    }
    const slot = pending.get(msg.id);
    if (!slot) return;
    pending.delete(msg.id);
    if (msg.ok) {
      slot.resolve(msg);
    } else {
      slot.reject(new Error(msg.error || "worker bridge request failed"));
    }
  };
  worker.onerror = (event) => {
    const error = new Error(event.message || "worker bridge crashed");
    for (const slot of pending.values()) slot.reject(error);
    pending.clear();
  };
  return worker;
}

function prog(frac, label) {
  try {
    if (typeof window !== "undefined" && window.__snufflerProgress) {
      window.__snufflerProgress(frac, label);
    }
  } catch (_e) {
    // Ignore loading-overlay failures; they are cosmetic.
  }
}

function copyBytes(value) {
  if (value == null) return null;
  const src = value instanceof Uint8Array ? value : new Uint8Array(value.buffer || value);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

function rpc(
  method,
  { request = null, data = null, text = null, name = null, handle = null } = {}
) {
  const id = nextRequestId++;
  const payload = { id, method };
  const transfer = [];

  const req = copyBytes(request);
  if (req) {
    payload.request = req;
    transfer.push(req.buffer);
  }

  const bytes = copyBytes(data);
  if (bytes) {
    payload.data = bytes;
    transfer.push(bytes.buffer);
  }

  if (text != null) payload.text = String(text);
  if (name != null) payload.name = String(name);
  if (handle != null) payload.handle = handle;

  const promise = new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
  });
  ensureWorker().postMessage(payload, transfer);
  return promise;
}

// Both data helpers now carry the BridgeResult envelope bytes in `result`
// (success variant OR BridgeError); Rust decodes the oneof. rpcBytes returns the
// envelope bytes; rpcBufferResult additionally carries the zero-copy ys buffer.
async function rpcBytes(method, args) {
  const msg = await rpc(method, args);
  return new Uint8Array(msg.result || new ArrayBuffer(0));
}

async function rpcText(method, args) {
  const msg = await rpc(method, args);
  return msg.text || "";
}

async function rpcBufferResult(method, args) {
  const msg = await rpc(method, args);
  return {
    result: new Uint8Array(msg.result || new ArrayBuffer(0)),
    ys: new Uint8Array(msg.ys || new ArrayBuffer(0)),
  };
}

// The single transport seam Rust (src/bridge.rs) calls for every unary proto
// RPC: forward (method, request, data) to the worker and return the BridgeResult
// envelope bytes plus the optional out-of-band zero-copy ys buffer. The worker
// owns all per-method logic (file staging, FDSN URL->fetch->parse, etc.); this
// is a uniform pipe. Category-B/C browser glue keeps its own exports below.
export async function call(method, request, data) {
  const out = await rpcBufferResult(method, { request, data });
  // Complete the loading overlay once the first scene (the synthetic example,
  // the first call after boot) is in; the boot sequence stops at 0.97.
  if (method === "makeExample") prog(1.0, "Ready");
  return out;
}

function varint(value) {
  let n = Number(value >>> 0);
  const out = [];
  while (n >= 0x80) {
    out.push((n & 0x7f) | 0x80);
    n = Math.floor(n / 128);
  }
  out.push(n);
  return out;
}

function fieldKey(fieldNumber, wireType) {
  return varint((fieldNumber << 3) | wireType);
}

function fieldString(fieldNumber, value) {
  const bytes = new TextEncoder().encode(String(value || ""));
  return [...fieldKey(fieldNumber, 2), ...varint(bytes.length), ...bytes];
}

function fieldBool(fieldNumber, value) {
  return [...fieldKey(fieldNumber, 0), value ? 1 : 0];
}

function encodeLoadBytesRequest(name, fmt, append) {
  return new Uint8Array([...fieldString(1, name), ...fieldString(2, fmt), ...fieldBool(3, append)]);
}

export async function makeExample() {
  const bytes = await rpcBytes("makeExample");
  prog(1.0, "Ready");
  return bytes;
}

export async function loadBytes(request, data) {
  return rpcBytes("loadBytes", { request, data });
}

export async function loadUrl(request) {
  return rpcBytes("loadUrl", { request });
}

export async function getView(request) {
  return rpcBufferResult("getView", { request });
}

export async function getSpectrum(request) {
  return rpcBufferResult("getSpectrum", { request });
}

export async function runSnuffling(request) {
  return rpcBytes("runSnuffling", { request });
}

export async function downloadWaveforms(request) {
  return rpcBytes("downloadWaveforms", { request });
}

export async function catalogSearch(request) {
  return rpcBytes("catalogSearch", { request });
}

export async function fetchStations(request) {
  return rpcBytes("fetchStations", { request });
}

export async function fetchResponses(request) {
  return rpcBytes("fetchResponses", { request });
}

export async function mapData() {
  return rpcBytes("mapData");
}

export async function cakePlot(request) {
  return rpcBytes("cakePlot", { request });
}

export async function addCakeModel() {
  const files = await pickFiles(false);
  if (files.length === 0) return "";
  const f = files[0];
  const bytes = new Uint8Array(await f.arrayBuffer());
  return rpcText("addCakeModel", { name: f.name, data: bytes });
}

export async function addGfStore() {
  if (typeof window.showDirectoryPicker !== "function") {
    throw new Error(
      "This browser has no File System Access API (showDirectoryPicker); " +
        "mounting a GF store directory needs a Chromium-based browser."
    );
  }
  let handle;
  try {
    handle = await window.showDirectoryPicker({ mode: "read" });
  } catch (e) {
    if (e && e.name === "AbortError") return "";
    throw e;
  }
  return rpcText("addGfStore", { handle });
}

export async function runCommand(request) {
  return rpcBytes("runCommand", { request });
}

export async function rmsTraces(request) {
  return rpcBytes("rmsTraces", { request });
}

function pickFiles(multiple) {
  return new Promise((resolve) => {
    const inp = document.createElement("input");
    inp.type = "file";
    inp.multiple = !!multiple;
    inp.style.display = "none";
    let done = false;
    const finish = (files) => {
      if (done) return;
      done = true;
      window.removeEventListener("focus", onFocus);
      inp.remove();
      resolve(files);
    };
    inp.addEventListener("change", () => finish(inp.files ? Array.from(inp.files) : []), {
      once: true,
    });
    const onFocus = () =>
      setTimeout(() => {
        if (!done && (!inp.files || inp.files.length === 0)) finish([]);
      }, 500);
    window.addEventListener("focus", onFocus);
    document.body.appendChild(inp);
    inp.click();
  });
}

export async function openFiles() {
  const files = await pickFiles(true);
  if (files.length === 0) return new Uint8Array(0);
  let scene = new Uint8Array(0);
  for (const f of files) {
    const data = new Uint8Array(await f.arrayBuffer());
    const request = encodeLoadBytesRequest(f.name, "detect", true);
    scene = await rpcBytes("loadBytes", { request, data });
  }
  return scene;
}

export async function saveMarkers(request) {
  const text = await rpcText("saveMarkers", { request });
  const blob = new Blob([text], { type: "text/plain" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "markers.txt";
  document.body.appendChild(a);
  a.click();
  setTimeout(() => {
    URL.revokeObjectURL(a.href);
    a.remove();
  }, 1000);
  return "";
}

export function savePng(bytes, filename) {
  // `bytes` is a wasm-bindgen view over the egui WASM heap, which is a
  // SharedArrayBuffer on this threaded build; the Blob constructor rejects shared
  // views ("must not be shared"). copyBytes() lifts it into a private,
  // non-shared buffer first (same trick wasm-bindgen uses for strings).
  const blob = new Blob([copyBytes(bytes)], { type: "image/png" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = filename || "snuffler.png";
  document.body.appendChild(a);
  a.click();
  setTimeout(() => {
    URL.revokeObjectURL(a.href);
    a.remove();
  }, 1000);
}

export function saveSvg(svg, filename) {
  const blob = new Blob([svg], { type: "image/svg+xml" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = filename || "snuffler.svg";
  document.body.appendChild(a);
  a.click();
  setTimeout(() => {
    URL.revokeObjectURL(a.href);
    a.remove();
  }, 1000);
}

export async function loadMarkers() {
  const files = await pickFiles(false);
  if (files.length === 0) return new Uint8Array(0);
  return rpcBytes("loadMarkers", { text: await files[0].text() });
}

export async function fetchTile(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`tile ${url}: ${r.status}`);
  const bmp = await createImageBitmap(await r.blob());
  const cv = new OffscreenCanvas(bmp.width, bmp.height);
  const cx = cv.getContext("2d");
  cx.drawImage(bmp, 0, 0);
  const img = cx.getImageData(0, 0, bmp.width, bmp.height);
  if (bmp.close) bmp.close();
  return { w: img.width, h: img.height, rgba: new Uint8Array(img.data.buffer) };
}

export async function pyExec(code) {
  return rpcText("pyExec", { text: code });
}

export async function ensureGf() {
  return rpcText("ensureGf");
}

if (typeof window !== "undefined") {
  window.snuffler = {
    makeExample,
    loadBytes,
    loadUrl,
    getView,
    getSpectrum,
    runSnuffling,
    downloadWaveforms,
    catalogSearch,
    fetchStations,
    fetchResponses,
    mapData,
    fetchTile,
    openFiles,
    saveMarkers,
    loadMarkers,
    cakePlot,
    addCakeModel,
    addGfStore,
    runCommand,
    rmsTraces,
    savePng,
    saveSvg,
    pyExec,
    ensureGf,
  };
}
