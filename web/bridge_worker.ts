// The app loads our OWN fork-free Pyodide 314 build (pthread + OpenMP + SIMD;
// py3.14 / emscripten 5.0.3) and all wheels from the locally-served py-mt/
// (runtime + pyodide-lock.json + wheels, incl. pyrocko + protobuf). This is the
// ONLY runtime: the page is always cross-origin isolated (see index.html), which
// SharedArrayBuffer / wasm threads require. A separate, non-threaded build can be
// produced later via the `threads` build-time feature and hosted on its own path
// so the two never interfere; there is no runtime CDN fallback / ?threads toggle.
import { fromBinary } from "@bufbuild/protobuf";
import {
  LoadBytesRequestSchema,
  LoadUrlRequestSchema,
} from "./proto/snuffler/bridge/v1/data_pb.js";
import { NamedSnufflingRequestSchema } from "./proto/snuffler/bridge/v1/snuffling_pb.js";

const PYODIDE_BASE = new URL("py-mt/", self.location.href).href;

// TextDecoder.decode() throws "The provided ArrayBufferView value must not be
// shared" when handed a view backed by a SharedArrayBuffer in this engine. Our
// Pyodide is a threaded (pthread) build, so its WASM heap IS a SharedArrayBuffer,
// and Pyodide's hiwire Python->JS string conversion decodes straight off that heap
// (e.g. loadPackage()'s unpack_buffer -> to_js(get_dynlibs(...)), and any Python
// string/exception crossing into JS). (wasm-bindgen sidesteps this by `.slice()`-ing
// out of shared memory first.) NOTE most decodes — emscripten's own pervasive
// C-string reads — do NOT trip this, so we must not slow them down: try the native
// decode first and only copy-and-retry the rare inputs the engine actually rejects.
// The success path is thus native speed, and on engines that already accept shared
// views no copy ever happens. Must run before Pyodide is loaded.
if (typeof SharedArrayBuffer !== "undefined") {
  const proto = TextDecoder.prototype;
  const decode = proto.decode;
  proto.decode = function (input, options) {
    try {
      return decode.call(this, input, options);
    } catch (e) {
      const shared =
        input instanceof SharedArrayBuffer ||
        (ArrayBuffer.isView(input) && input.buffer instanceof SharedArrayBuffer);
      if (!shared || !(e instanceof TypeError)) throw e;
      const bytes =
        input instanceof SharedArrayBuffer
          ? new Uint8Array(input)
          : new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
      const copy = new Uint8Array(bytes.byteLength);
      copy.set(bytes);
      return decode.call(this, copy, options);
    }
  };
}

// py / bridge are Pyodide handles (a runtime + the imported bridge.py module),
// typed `any` — they are dynamic JsProxies with no static types available.
let pyodideReadyPromise: Promise<any> | null = null;
let gfReadyPromise: Promise<string> | null = null;
let py: any = null;
let bridge: any = null;
let stagedFileCounter = 0;
let gfStoreCounter = 0;
let earthModelsStaged = false;

function resolveUrl(path) {
  return new URL(path, self.location.href).href;
}

async function fetchOk(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${url}: ${r.status} ${r.statusText}`);
  return r;
}

function prog(frac, label) {
  self.postMessage({ type: "progress", frac, label });
}

function destroyMaybe(value) {
  if (value && typeof value.destroy === "function") value.destroy();
}

function pyValueToString(value) {
  if (typeof value === "string") return value;
  try {
    if (value && typeof value.toJs === "function") return value.toJs();
    return String(value);
  } finally {
    destroyMaybe(value);
  }
}

function ownedBytes(value) {
  const src = value instanceof Uint8Array ? value : new Uint8Array(value.buffer || value);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

function pyBytes(value) {
  try {
    if (value instanceof Uint8Array) return ownedBytes(value);
    if (value && typeof value.toJs === "function") return ownedBytes(value.toJs());
    return ownedBytes(value);
  } finally {
    destroyMaybe(value);
  }
}

function safeName(name) {
  return (
    String(name || "data")
      .replace(/[^A-Za-z0-9._-]+/g, "_")
      .slice(-96) || "data"
  );
}

function stagedDataPath(name) {
  stagedFileCounter += 1;
  return `/data/${stagedFileCounter}-${safeName(name)}`;
}

function writeStagedFile(name, data) {
  try {
    py.FS.mkdir("/data");
  } catch (_e) {
    // Already exists.
  }
  const path = stagedDataPath(name);
  py.FS.writeFile(path, data);
  return path;
}

async function boot() {
  if (pyodideReadyPromise) return pyodideReadyPromise;
  pyodideReadyPromise = (async () => {
    try {
      const t0 = performance.now();
      prog(0.08, "Downloading Python runtime…");
      // Pyodide 314 requires a module worker on both paths (it rejects classic
      // workers), so import the ESM build from whichever base is active — our
      // local py-mt/ when threaded, else the stock 314 CDN.
      const { loadPyodide } = await import(PYODIDE_BASE + "pyodide.mjs");
      prog(0.18, "Starting Python worker…");
      py = await loadPyodide({ indexURL: PYODIDE_BASE });
      const tRuntime = performance.now();

      prog(0.25, "Loading numpy, scipy, matplotlib…");
      let creep = 0.25;
      const timer = setInterval(() => {
        creep = Math.min(0.74, creep + 0.012);
        prog(creep, "Loading numpy, scipy, matplotlib…");
      }, 350);
      try {
        // Everything (incl. pyrocko, protobuf, requests, pyodide-http) is in
        // py-mt/pyodide-lock.json, built for this runtime — load by name.
        await py.loadPackage([
          "numpy",
          "scipy",
          "matplotlib",
          "pyyaml",
          "micropip",
          "protobuf",
          "requests",
          "pyodide-http",
          "pyrocko",
        ]);
      } finally {
        clearInterval(timer);
      }
      const tPackages = performance.now();

      prog(0.92, "Importing pyrocko…");
      for (const dir of ["/snuffler", "/snuffler/bridge", "/snuffler/bridge/v1"]) {
        try {
          py.FS.mkdir(dir);
        } catch (_e) {
          // Already exists.
        }
      }
      for (const file of [
        "common_pb2.py",
        "scene_pb2.py",
        "view_pb2.py",
        "snuffling_pb2.py",
        "data_pb2.py",
        "bridge_pb2.py",
        // Generated typed servicer Protocol + dispatch (from BridgeService);
        // bridge.py imports it, so it must be staged alongside the *_pb2 modules.
        "bridge_dispatch.py",
      ]) {
        const src = await (await fetchOk(resolveUrl(`proto/snuffler/bridge/v1/${file}`))).text();
        py.FS.writeFile(`/snuffler/bridge/v1/${file}`, src);
      }
      const bridgeSrc = await (await fetchOk(resolveUrl("bridge.py"))).text();
      py.FS.writeFile("/bridge.py", bridgeSrc);
      bridge = py.runPython('import sys; sys.path.insert(0, "/"); import bridge; bridge');
      const tImport = performance.now();

      // Boot-phase timings, logged once so future regressions are visible in the
      // console. runtime = download + start the Pyodide runtime; packages =
      // loadPackage (numpy/scipy/matplotlib/pyrocko/…); import = stage protos +
      // `import bridge` (which imports pyrocko). On this MT build the py-mt runtime
      // also stands up libomp + pthread workers, so these spans include that spin-up.
      const ms = (a, b) => Math.round(b - a);
      console.log(
        `[bridge-worker] boot timing (ms): runtime=${ms(t0, tRuntime)} ` +
          `packages=${ms(tRuntime, tPackages)} import=${ms(tPackages, tImport)} ` +
          `total=${ms(t0, tImport)}`
      );

      // Report the booted runtime so the UI can show a truthful debug badge.
      // pthreads is read back from the live interpreter to confirm threading
      // really came up on our py-mt build (the badge then reads "wasm+openmp+simd").
      try {
        self.postMessage({
          type: "runtime",
          threaded: true,
          simd: true,
          pthreads: py.runPython(
            "import sys; bool(getattr(sys._emscripten_info, 'pthreads', False))"
          ),
          python: py.runPython("import sys; sys.version.split()[0]"),
          numpy: py.runPython("import numpy; numpy.__version__"),
          cores: (self.navigator && self.navigator.hardwareConcurrency) || 0,
        });
      } catch (_e) {
        // Non-fatal: the badge just won't show.
      }

      prog(0.97, "Almost ready…");
      return py;
    } catch (e) {
      console.error("[bridge-worker] boot failed:", e);
      throw e;
    }
  })();
  return pyodideReadyPromise;
}

async function ensureGf() {
  if (gfReadyPromise) return gfReadyPromise;
  gfReadyPromise = (async () => {
    await boot();
    // requests + pyodide-http are preloaded at boot (they are in the py-mt lock).
    await py.runPythonAsync(`
import pyodide_http
pyodide_http.patch_all()
`);
    return "";
  })();
  return gfReadyPromise;
}

const CAKE_MODELS = [
  "ak135-f-average-no-ocean.f",
  "ak135-f-average-no-ocean.l",
  "ak135-f-average-no-ocean.m",
  "ak135-f-average.f",
  "ak135-f-average.l",
  "ak135-f-average.m",
  "ak135-f-average.vf",
  "ak135-f-continental.f",
  "ak135-f-continental.l",
  "ak135-f-continental.m",
  "prem-no-ocean.f",
  "prem-no-ocean.l",
  "prem-no-ocean.m",
  "prem.f",
  "prem.l",
  "prem.m",
];

async function ensureEarthModels() {
  if (earthModelsStaged) return;
  await boot();
  try {
    py.FS.mkdir("/earthmodels");
  } catch (_e) {
    // Already exists.
  }
  await Promise.all(
    CAKE_MODELS.map(async (stem) => {
      const text = await (await fetchOk(resolveUrl(`earthmodels/${stem}.nd`))).text();
      py.FS.writeFile(`/earthmodels/${stem}.nd`, text);
    })
  );
  earthModelsStaged = true;
}

// Shape the {"result": BridgeResult bytes, "buffer"?: ndarray} dict that
// bridge.call() returns into the worker's postMessage payload. The result bytes
// (the proto envelope — success variant OR BridgeError) always flow through;
// the zero-copy ys buffer is transferred only when present (getView/getSpectrum).
function bridgeResult(res) {
  try {
    const result = pyBytes(res.get("result")); // pyBytes owns + destroys the inner proxy
    const out: Record<string, any> = { result: result.buffer, transfer: [result.buffer] };
    if (res.has("buffer")) {
      const ysProxy = res.get("buffer");
      let buf: any = null;
      try {
        buf = ysProxy.getBuffer();
        const d = buf.data;
        const ys = ownedBytes(
          d instanceof Uint8Array ? d : new Uint8Array(d.buffer, d.byteOffset, d.byteLength)
        );
        out.ys = ys.buffer;
        out.transfer.push(ys.buffer);
      } finally {
        if (buf) buf.release();
        destroyMaybe(ysProxy);
      }
    }
    return out;
  } finally {
    destroyMaybe(res);
  }
}

// Extract the string from a bridge.call_text() {"text", "error"} reply. The
// error variant is surfaced as a plain JS Error; bridge.py returns the message as
// a field (rather than raising) so it crosses as data, and the TextDecoder shim
// above now also covers Python strings/exceptions decoded off the shared heap.
function callText(res) {
  try {
    const error = res.get("error");
    if (error) throw new Error(error);
    return res.get("text") || "";
  } finally {
    destroyMaybe(res);
  }
}

// Canonical gRPC id prefix for a BridgeService rpc. The generated Rust client and
// Python dispatch both key on /package.Service/Method, so the worker names the
// leaf rpcs it invokes the same way. Pure-passthrough rpcs already arrive as this
// id (relayed straight through, below); the category-B/C glue here, whose own
// incoming ids are hand-written, translates to it when calling bridge.py.
const SVC = "/snuffler.bridge.v1.BridgeService/";

async function handleRequest(msg) {
  const method = msg.method;
  if (method === "loadBytes") {
    await boot();
    const name = fromBinary(LoadBytesRequestSchema, msg.request).name;
    const path = writeStagedFile(name || "data", msg.data || new Uint8Array(0));
    return bridgeResult(bridge.call(SVC + "LoadBytes", msg.request, path));
  }
  if (method === SVC + "LoadUrl") {
    await boot();
    const url = fromBinary(LoadUrlRequestSchema, msg.request).url || "";
    const name = url.split("/").pop() || "data";
    const buf = new Uint8Array(await (await fetchOk(resolveUrl(url))).arrayBuffer());
    const path = writeStagedFile(name, buf);
    return bridgeResult(bridge.call(method, msg.request, path));
  }
  if (method === SVC + "RunSnuffling") {
    await boot();
    const name = fromBinary(NamedSnufflingRequestSchema, msg.request).name;
    if (name === "cake") await ensureEarthModels();
    return bridgeResult(bridge.call(method, msg.request));
  }
  if (method === "downloadWaveforms") {
    await boot();
    const url = callText(bridge.call_text(SVC + "DataselectUrl", msg.request));
    const r = await fetch(url);
    if (!r.ok && r.status !== 204) {
      throw new Error(`dataselect HTTP ${r.status} ${r.statusText}`);
    }
    const buf = new Uint8Array(await r.arrayBuffer());
    if (buf.byteLength === 0) return bridgeResult(bridge.call(SVC + "EmptyScene"));
    const path = writeStagedFile("fdsn.mseed", buf);
    return bridgeResult(bridge.call(SVC + "LoadWaveformPath", null, path));
  }
  if (method === "catalogSearch") {
    await boot();
    const url = callText(bridge.call_text(SVC + "EventUrl", msg.request));
    const r = await fetch(url);
    const text = r.ok ? await r.text() : "";
    return bridgeResult(bridge.call(SVC + "ParseEvents", null, text));
  }
  if (method === "fetchStations") {
    await boot();
    const url = callText(bridge.call_text(SVC + "StationUrl", msg.request));
    const r = await fetch(url);
    const text = r.ok ? await r.text() : "";
    return bridgeResult(bridge.call(SVC + "ParseStations", null, text));
  }
  if (method === "fetchResponses") {
    await boot();
    const url = callText(bridge.call_text(SVC + "ResponseUrl", msg.request));
    const r = await fetch(url);
    const text = r.ok ? await r.text() : "";
    return bridgeResult(bridge.call(SVC + "LoadResponses", null, text));
  }
  if (method === SVC + "CakePlot") {
    await boot();
    await ensureEarthModels();
    return bridgeResult(bridge.call(method, msg.request));
  }
  if (method === "addCakeModel") {
    await boot();
    const name = String(msg.name || "model").replace(/\.nd$/i, "");
    try {
      py.FS.mkdir("/earthmodels");
    } catch (_e) {
      // Already exists.
    }
    py.FS.writeFile(`/earthmodels/${name}.nd`, msg.data || new Uint8Array(0));
    earthModelsStaged = true;
    return { text: name };
  }
  if (method === "addGfStore") {
    await boot();
    gfStoreCounter += 1;
    const mount = "/gfstores/" + gfStoreCounter;
    try {
      py.FS.mkdir("/gfstores");
    } catch (_e) {
      // Already exists.
    }
    py.FS.mkdir(mount);
    await py.mountNativeFS(mount, msg.handle);
    return { text: callText(bridge.call_text(SVC + "GfStoreLabel", null, mount)) };
  }
  if (method === "saveMarkers") {
    await boot();
    return { text: callText(bridge.call_text(SVC + "MarkersToText", msg.request)) };
  }
  if (method === "loadMarkers") {
    await boot();
    return bridgeResult(bridge.call(SVC + "LoadMarkers", null, String(msg.text || "")));
  }
  if (method === "pyExec") {
    await boot();
    const value = await py.runPythonAsync(String(msg.text || ""));
    return { text: pyValueToString(value) };
  }
  if (method === "ensureGf") {
    await ensureGf();
    return { text: "" };
  }
  // Pure passthrough: any unary rpc that needs no worker-side staging or
  // orchestration (makeExample, getView, getSpectrum, rmsTraces, mapData,
  // runCommand, and any future one). The worker just relays the generated method
  // id + request bytes; bridge.py's dispatch validates and turns an unknown
  // method into a BridgeError, so there is no per-method branch to drift here.
  await boot();
  return bridgeResult(bridge.call(method, msg.request));
}

self.onmessage = (event) => {
  const msg = event.data || {};
  handleRequest(msg)
    .then((result: any) => {
      const transfer = result.transfer || [];
      const { transfer: _transfer, ...payload } = result;
      self.postMessage({ id: msg.id, ok: true, ...payload }, transfer);
    })
    .catch((e) => {
      self.postMessage({
        id: msg.id,
        ok: false,
        error: e && e.message ? e.message : String(e),
      });
    });
};
