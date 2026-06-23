// The app loads our OWN fork-free Pyodide 314 build (pthread + OpenMP + SIMD;
// py3.14 / emscripten 5.0.3) and all wheels from the locally-served py-mt/
// (runtime + pyodide-lock.json + wheels, incl. pyrocko + protobuf). This is the
// ONLY runtime: the page is always cross-origin isolated (see index.html), which
// SharedArrayBuffer / wasm threads require. A separate, non-threaded build can be
// produced later via the `threads` build-time feature and hosted on its own path
// so the two never interfere; there is no runtime CDN fallback / ?threads toggle.
import { rpc } from "./rpc.js";
import { WORKER_MOUNT_GF_STORE } from "./worker_command.js";

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
let py: any = null;
let bridge: any = null;
let gfStoreCounter = 0;

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

// The worker is a DUMB RELAY: it forwards the generated method id + request bytes
// (+ optional out-of-band data) to bridge.py's async dispatch, which runs the rpc
// — including its own network fetches (pyfetch) — and returns the BridgeResult
// envelope (plus the zero-copy ys buffer for getView/getSpectrum). It spells no
// method names of its own, so nothing here can drift from the .proto.
//
// The ONE exception is mounting a live directory handle for a GF store: that is a
// DOM object that can't ride a proto, so the worker mounts it into the Pyodide FS
// and then relays GfStoreLabel for the registered store's label.
async function handleRequest(msg) {
  await boot();
  if (msg.method === WORKER_MOUNT_GF_STORE) {
    gfStoreCounter += 1;
    const mount = "/gfstores/" + gfStoreCounter;
    try {
      py.FS.mkdir("/gfstores");
    } catch (_e) {
      // Already exists.
    }
    py.FS.mkdir(mount);
    await py.mountNativeFS(mount, msg.handle);
    return { text: callText(await bridge.call_text(rpc("gfStoreLabel"), null, mount)) };
  }
  return bridgeResult(await bridge.call(msg.method, msg.request, msg.data ?? null));
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
