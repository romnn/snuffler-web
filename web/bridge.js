// Glue between the egui (wasm) frontend and Pyodide (CPython + numpy + scipy +
// pyrocko, all compiled to WASM). The Rust side calls the exported async
// functions via wasm-bindgen; here we drive Pyodide and marshal data.
//
// The data contract (SceneMeta / ViewMeta) is defined in web/bridge.py and
// mirrored by the serde structs in src/bridge.rs.

const PYODIDE_VERSION = "0.27.2";
const PYODIDE_BASE = `https://cdn.jsdelivr.net/pyodide/v${PYODIDE_VERSION}/full/`;

let pyodideReadyPromise = null;
let py = null; // the Pyodide instance
let bridge = null; // the imported Python `bridge` module (PyProxy)

function loadScript(src) {
  return new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = src;
    s.onload = () => resolve();
    s.onerror = () => reject(new Error("failed to load " + src));
    document.head.appendChild(s);
  });
}

// fetch() that turns an HTTP error into a clear exception instead of letting a
// 404 error page flow downstream as bogus JSON/bytes. The wheel + bridge.py are
// fetched on demand, so a missing build artifact should fail loudly.
async function fetchOk(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${url}: ${r.status} ${r.statusText}`);
  return r;
}

// Report boot progress to the HTML loading overlay (index.html).
function prog(frac, label) {
  try {
    if (typeof window !== "undefined" && window.__snufflerProgress) {
      window.__snufflerProgress(frac, label);
    }
  } catch (_e) { /* ignore */ }
}

// Boot Pyodide once (lazy). Loads numpy/scipy, installs the pyrocko wheel,
// and imports the Python bridge module.
function boot() {
  if (pyodideReadyPromise) return pyodideReadyPromise;
  pyodideReadyPromise = (async () => {
    try {
      prog(0.08, "Downloading Python runtime…");
      if (typeof loadPyodide === "undefined") {
        await loadScript(PYODIDE_BASE + "pyodide.js");
      }
      prog(0.18, "Starting Python…");
      py = await loadPyodide({ indexURL: PYODIDE_BASE });

      // pyrocko's import chain needs numpy + scipy (filters), pyyaml (guts
      // serialization) and matplotlib (orthodrome imports matplotlib.path).
      // This is the big download — creep the bar while it runs.
      prog(0.25, "Loading numpy, scipy, matplotlib…");
      let creep = 0.25;
      const timer = setInterval(() => {
        creep = Math.min(0.78, creep + 0.012);
        prog(creep, "Loading numpy, scipy, matplotlib…");
      }, 350);
      try {
        await py.loadPackage(["numpy", "scipy", "micropip", "pyyaml", "matplotlib"]);
      } finally {
        clearInterval(timer);
      }

      // Install the cross-compiled pyrocko wheel(s). deps=False: pyrocko
      // declares matplotlib/requests/tornado which we neither need nor want to
      // pull in. Done in Python so deps=False is passed unambiguously.
      prog(0.80, "Installing pyrocko…");
      const wheels = await (await fetchOk("py/wheels.json")).json();
      for (const w of wheels) {
        const url = new URL("py/" + w, document.baseURI).href;
        // Fetch into the Emscripten FS and install via emfs: — installing
        // straight from an http URL trips a micropip download bug.
        const buf = new Uint8Array(await (await fetchOk(url)).arrayBuffer());
        py.FS.writeFile("/" + w, buf);
        await py.runPythonAsync(
          `import micropip\nawait micropip.install("emfs:/${w}", deps=False)`
        );
      }

      // Load the type-checked Python shim and import it.
      prog(0.92, "Importing pyrocko…");
      const src = await (await fetchOk("bridge.py")).text();
      py.FS.writeFile("/bridge.py", src);
      bridge = py.runPython('import sys; sys.path.insert(0, "/"); import bridge; bridge');
      prog(0.97, "Almost ready…");
      return py;
    } catch (e) {
      console.error("[bridge] boot failed:", e);
      throw e;
    }
  })();
  return pyodideReadyPromise;
}

export async function makeExample() {
  await boot();
  const scene = bridge.make_example(); // -> JSON string (SceneMeta)
  prog(1.0, "Ready");
  return scene;
}

export async function loadBytes(name, data, fmt) {
  await boot();
  return bridge.load_bytes(name, data, fmt); // data: Uint8Array -> bytes
}

export async function loadUrl(url, fmt) {
  await boot();
  const buf = new Uint8Array(await (await fetchOk(url)).arrayBuffer());
  const name = url.split("/").pop() || "data";
  return bridge.load_bytes(name, buf, fmt);
}

export async function getView(paramsJson) {
  await boot();
  const res = bridge.get_view(paramsJson); // PyProxy dict {meta: str, ys: bytes}
  const obj = res.toJs({ dict_converter: Object.fromEntries });
  res.destroy();
  const u8 = obj.ys; // Uint8Array (a copy of the Python bytes)
  const ys = new Float32Array(u8.buffer, u8.byteOffset, u8.byteLength / 4);
  return { meta: obj.meta, ys };
}

export async function getSpectrum(paramsJson) {
  await boot();
  const res = bridge.get_spectrum(paramsJson);
  const obj = res.toJs({ dict_converter: Object.fromEntries });
  res.destroy();
  const u8 = obj.ys;
  const ys = new Float32Array(u8.buffer, u8.byteOffset, u8.byteLength / 4);
  return { meta: obj.meta, ys };
}

// Debug hook so the bridge can be exercised from the devtools/tests.
if (typeof window !== "undefined") {
  window.snuffler = { makeExample, loadBytes, loadUrl, getView, getSpectrum };
}
