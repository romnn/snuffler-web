// Thin browser-side client for the Pyodide worker. Rust calls this module via
// wasm-bindgen; this module keeps DOM-only work on the main thread and sends
// protobuf request bytes plus transferable buffers to web/bridge_worker.js.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { BridgeResultSchema } from "./proto/snuffler/bridge/v1/bridge_pb.js";
import {
  AddCakeModelRequestSchema,
  LoadBytesRequestSchema,
  LoadMarkersRequestSchema,
} from "./proto/snuffler/bridge/v1/data_pb.js";
import { rpc, BridgeMethod, type BridgeMethodId } from "./rpc.js";
import { WORKER_MOUNT_GF_STORE, type WorkerCommand } from "./worker_command.js";

// What the worker accepts as a request `method`: any BridgeService rpc id, plus
// the single worker-only command (the GF-store directory mount).
type SeamMethod = BridgeMethodId | WorkerCommand;

// Cosmetic progress/runtime hooks (set by index.html) + the File System Access
// API. Typed loosely — optional integration points, not the typed proto boundary.
declare global {
  interface Window {
    __snufflerProgress?: (frac: number, label: string) => void;
    __snufflerRuntime?: (info: unknown) => void;
    showDirectoryPicker?: (opts?: { mode?: string }) => Promise<unknown>;
  }
}

type Pending = { resolve: (msg: any) => void; reject: (err: Error) => void };

let worker: Worker | null = null;
let nextRequestId = 1;
const pending = new Map<number, Pending>();

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

// Post one request to the worker and resolve when its reply with the same id
// arrives. `rpc(name)` (imported) names the method; this just pipes bytes.
function send(method: SeamMethod, { request = null, data = null, handle = null } = {}) {
  const id = nextRequestId++;
  const payload: Record<string, any> = { id, method };
  const transfer: Transferable[] = [];

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

  if (handle != null) payload.handle = handle;

  const promise = new Promise<any>((resolve, reject) => {
    pending.set(id, { resolve, reject });
  });
  ensureWorker().postMessage(payload, transfer);
  return promise;
}

// Both data helpers now carry the BridgeResult envelope bytes in `result`
// (success variant OR BridgeError); Rust decodes the oneof. rpcBytes returns the
// envelope bytes; rpcBufferResult additionally carries the zero-copy ys buffer.
async function rpcBytes(method: BridgeMethodId, args?) {
  const msg = await send(method, args);
  return new Uint8Array(msg.result || new ArrayBuffer(0));
}

async function rpcText(method: SeamMethod, args?) {
  const msg = await send(method, args);
  return msg.text || "";
}

async function rpcBufferResult(method: BridgeMethodId, args?) {
  const msg = await send(method, args);
  return {
    result: new Uint8Array(msg.result || new ArrayBuffer(0)),
    ys: new Uint8Array(msg.ys || new ArrayBuffer(0)),
  };
}

// Decode a BridgeResult envelope to its StringValue text, for the DOM glue that
// needs a string back (marker export, cake model, GF store). Throws on the error
// variant, mirroring how Rust surfaces a BridgeError.
function resultText(env: Uint8Array): string {
  const r = fromBinary(BridgeResultSchema, env);
  if (r.result.case === "error") {
    throw new Error(`${r.result.value.kind}: ${r.result.value.message}`);
  }
  if (r.result.case === "text") return r.result.value.value;
  throw new Error(`unexpected bridge result: ${r.result.case}`);
}

// The single transport seam Rust (src/bridge.rs) calls for every unary proto rpc:
// forward (method, request) to the worker and return the BridgeResult envelope
// bytes plus the optional out-of-band zero-copy ys buffer. The worker is a dumb
// relay; bridge.py runs the rpc. The DOM-only browser glue keeps its own exports
// below.
export async function call(method: BridgeMethodId, request, data) {
  const out = await rpcBufferResult(method, { request, data });
  // Complete the loading overlay once the first scene (the synthetic example,
  // the first call after boot) is in; the boot sequence stops at 0.97. Typed
  // compare against the generated id so a rename of MakeExample is a tsc error.
  if (method === BridgeMethod.makeExample) prog(1.0, "Ready");
  return out;
}

export async function addCakeModel() {
  const files = await pickFiles(false);
  if (files.length === 0) return "";
  const f = files[0];
  const content = new Uint8Array(await f.arrayBuffer());
  const request = toBinary(
    AddCakeModelRequestSchema,
    create(AddCakeModelRequestSchema, { name: f.name, content })
  );
  return resultText(await rpcBytes(rpc("addCakeModel"), { request }));
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
    if (e instanceof DOMException && e.name === "AbortError") return "";
    throw e;
  }
  return rpcText(WORKER_MOUNT_GF_STORE, { handle });
}

function pickFiles(multiple): Promise<File[]> {
  return new Promise<File[]>((resolve) => {
    const inp = document.createElement("input");
    inp.type = "file";
    inp.multiple = !!multiple;
    inp.style.display = "none";
    let done = false;
    const finish = (files: File[]) => {
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
    const request = toBinary(
      LoadBytesRequestSchema,
      create(LoadBytesRequestSchema, { name: f.name, fmt: "detect", append: true, content: data })
    );
    scene = await rpcBytes(rpc("loadBytes"), { request });
  }
  return scene;
}

export async function saveMarkers(request) {
  const text = resultText(await rpcBytes(rpc("markersToText"), { request }));
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
  const blob = new Blob([copyBytes(bytes) ?? new Uint8Array()], { type: "image/png" });
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
  const request = toBinary(
    LoadMarkersRequestSchema,
    create(LoadMarkersRequestSchema, { text: await files[0].text() })
  );
  return rpcBytes(rpc("loadMarkers"), { request });
}

export async function fetchTile(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`tile ${url}: ${r.status}`);
  const bmp = await createImageBitmap(await r.blob());
  const cv = new OffscreenCanvas(bmp.width, bmp.height);
  const cx = cv.getContext("2d");
  if (!cx) throw new Error("tile: 2d canvas context unavailable");
  cx.drawImage(bmp, 0, 0);
  const img = cx.getImageData(0, 0, bmp.width, bmp.height);
  if (bmp.close) bmp.close();
  return { w: img.width, h: img.height, rgba: new Uint8Array(img.data.buffer) };
}
