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
let stagedFileCounter = 0;

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

function stagedDataPath(name) {
  const safe = String(name || "data").replace(/[^A-Za-z0-9._-]+/g, "_").slice(-96) || "data";
  stagedFileCounter += 1;
  return `/data/${stagedFileCounter}-${safe}`;
}

function writeStagedFile(name, data) {
  try { py.FS.mkdir("/data"); } catch (_e) { /* already exists */ }
  const path = stagedDataPath(name);
  py.FS.writeFile(path, data);
  return path;
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

function pyBufferResult(res, label) {
  let ysProxy = null;
  let buffer = null;
  try {
    const meta = pyValueToString(res.get("meta"));
    ysProxy = res.get("ys");
    buffer = ysProxy.getBuffer();
    const data = buffer.data;
    const ys = data instanceof Float32Array
      ? data
      : new Float32Array(data.buffer, data.byteOffset, data.byteLength / 4);
    let released = false;
    return {
      meta,
      ys,
      release() {
        if (released) return;
        released = true;
        try {
          buffer.release();
        } finally {
          destroyMaybe(ysProxy);
          destroyMaybe(res);
        }
      },
    };
  } catch (e) {
    try {
      if (buffer) buffer.release();
    } finally {
      destroyMaybe(ysProxy);
      destroyMaybe(res);
    }
    throw new Error(`${label}: ${e && e.message ? e.message : e}`);
  }
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
  const path = writeStagedFile(name, data);
  return bridge.load_path(name, path, fmt, false);
}

export async function loadUrl(url, fmt) {
  await boot();
  const buf = new Uint8Array(await (await fetchOk(url)).arrayBuffer());
  const name = url.split("/").pop() || "data";
  const path = writeStagedFile(name, buf);
  return bridge.load_path(name, path, fmt, false);
}

export async function getView(paramsJson) {
  await boot();
  const res = bridge.get_view(paramsJson); // PyProxy dict {meta: str, ys: ndarray}
  return pyBufferResult(res, "getView");
}

export async function getSpectrum(paramsJson) {
  await boot();
  const res = bridge.get_spectrum(paramsJson);
  return pyBufferResult(res, "getSpectrum");
}

export async function runSnuffling(name, paramsJson) {
  await boot();
  return bridge.run_snuffling(name, paramsJson); // -> JSON string {text, markers}
}

// --- FDSN network: Python builds the URL + parses; the browser does fetch() ---

export async function downloadWaveforms(paramsJson) {
  await boot();
  const url = bridge.dataselect_url(paramsJson);
  const r = await fetch(url);
  if (!r.ok && r.status !== 204) throw new Error(`dataselect HTTP ${r.status} ${r.statusText}`);
  const buf = new Uint8Array(await r.arrayBuffer());
  if (buf.byteLength === 0) return JSON.stringify({ tmin: 0.0, tmax: 1.0, tracks: [] });
  const path = writeStagedFile("fdsn.mseed", buf);
  return bridge.load_waveform_path(path); // -> scene JSON string
}

export async function catalogSearch(paramsJson) {
  await boot();
  const url = bridge.event_url(paramsJson);
  const r = await fetch(url);
  const text = r.ok ? await r.text() : "";
  return bridge.parse_events(text); // -> JSON string {text, markers}
}

export async function fetchStations(paramsJson) {
  await boot();
  const url = bridge.station_url(paramsJson);
  const r = await fetch(url);
  const text = r.ok ? await r.text() : "";
  return bridge.parse_stations(text); // -> JSON string {text, markers}
}

export async function fetchResponses(paramsJson) {
  await boot();
  const url = bridge.response_url(paramsJson);
  const r = await fetch(url);
  const text = r.ok ? await r.text() : "";
  return bridge.load_responses(text); // -> JSON string {text, markers}
}

export async function mapData() {
  await boot();
  return bridge.map_data(); // -> JSON string {stations, events}
}

// The ak135 model ships as a build asset; stage it into the Pyodide FS once so
// pyrocko.cake.load_model() can read it by path (the minimal wheel has no data
// dir). bridge.py reads it from EARTHMODEL_PATH ("/earthmodel.nd").
let earthModelStaged = false;
async function ensureEarthModel() {
  if (earthModelStaged) return;
  const url = new URL("ak135-f-continental.m.nd", document.baseURI).href;
  const text = await (await fetchOk(url)).text();
  py.FS.writeFile("/earthmodel.nd", text);
  earthModelStaged = true;
}

export async function cakeArrivals() {
  await boot();
  await ensureEarthModel();
  return bridge.cake_arrivals("{}"); // -> JSON string {text, markers}
}

export async function runCommand(line) {
  await boot();
  return bridge.command(line); // -> JSON string {action, ...}
}

export async function rmsTraces(paramsJson) {
  await boot();
  return bridge.rms(paramsJson); // -> scene JSON string (RMS traces appended)
}

// Show a browser file picker; resolves [] if the dialog is cancelled.
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
    inp.addEventListener("change", () => finish(inp.files ? Array.from(inp.files) : []), { once: true });
    // No reliable "cancel" event: when the window regains focus with no files, give up.
    const onFocus = () => setTimeout(() => {
      if (!done && (!inp.files || inp.files.length === 0)) finish([]);
    }, 500);
    window.addEventListener("focus", onFocus);
    document.body.appendChild(inp);
    inp.click();
  });
}

export async function openFiles() {
  await boot();
  const files = await pickFiles(true);
  if (files.length === 0) return ""; // cancelled
  let scene = "";
  for (const f of files) {
    const buf = new Uint8Array(await f.arrayBuffer());
    const path = writeStagedFile(f.name, buf);
    scene = bridge.load_path(f.name, path, "detect", true); // append to the pile
  }
  return scene;
}

export async function saveMarkers(markersJson) {
  await boot();
  const text = bridge.markers_to_text(markersJson);
  const blob = new Blob([text], { type: "text/plain" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "markers.txt";
  document.body.appendChild(a);
  a.click();
  setTimeout(() => { URL.revokeObjectURL(a.href); a.remove(); }, 1000);
  return "";
}

export async function loadMarkers() {
  await boot();
  const files = await pickFiles(false);
  if (files.length === 0) return "";
  const text = await files[0].text();
  return bridge.markers_from_text(text); // -> JSON array
}

// Fetch a map tile and decode it to RGBA in the browser (CORS-clean providers),
// so Rust can upload it as a texture without an image decoder.
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

// Debug hook so the bridge can be exercised from the devtools/tests.
if (typeof window !== "undefined") {
  window.snuffler = {
    makeExample, loadBytes, loadUrl, getView, getSpectrum, runSnuffling,
    downloadWaveforms, catalogSearch, fetchStations, fetchResponses,
    mapData, fetchTile, openFiles, saveMarkers, loadMarkers, cakeArrivals,
    runCommand, rmsTraces,
  };
}
