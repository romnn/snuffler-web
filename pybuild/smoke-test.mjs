// Headless validation of the pyrocko Pyodide wheel against the real 0.27.2
// runtime: install the wheel, import pyrocko.trace, run a filter, load MiniSEED.
import { loadPyodide } from "pyodide";
import fs from "node:fs";

const INDEX = "https://cdn.jsdelivr.net/pyodide/v0.27.2/full/";

const py = await loadPyodide({ indexURL: INDEX });
console.log("pyodide", py.version);
await py.loadPackage(["numpy", "scipy", "micropip"]);

const micropip = py.pyimport("micropip");
const wheels = JSON.parse(fs.readFileSync("dist-py/wheels.json", "utf8"));
for (const w of wheels) {
  py.FS.writeFile("/" + w, fs.readFileSync("dist-py/" + w));
  console.log("installing", w);
  await micropip.install("emfs:/" + w);
}

// import + synthetic filter
py.runPython(`
import numpy as np
from pyrocko import trace, io
tr = trace.Trace(network='XX', station='STA', channel='HHZ',
                 deltat=0.01, ydata=np.sin(np.arange(2000)*0.2).astype(float))
tr.highpass(4, 1.0)
tr.lowpass(4, 20.0)
print("trace ok: import + highpass + lowpass, nsamples =", tr.data_len())
`);

// load the bundled MiniSEED file (the real demo data)
const mseed = "sources/pyrocko/doc/source/static/test.mseed";
py.FS.writeFile("/test.mseed", fs.readFileSync(mseed));
py.runPython(`
from pyrocko import io
trs = io.load('/test.mseed', format='mseed')
print("mseed ok:", len(trs), "traces")
for t in trs[:4]:
    print("  ", '.'.join(t.nslc_id), "nsamp=", t.data_len(), "deltat=", t.deltat)
`);

// pyrocko.gf + the GF/synthetics C extensions (store_ext, eikonal_ext,
// okada_ext, ...). `import pyrocko.gf` exercises the whole chain (seismosizer
// imports eikonal_ext + okada_ext at module load); importing a wasm .so proves
// its module-init ran, i.e. it cross-compiled to a valid Emscripten module.
// pyrocko.gf.ws imports `requests`, so install it (+ pyodide-http, which patches
// requests/urllib onto sync XHR) first — the same packages ensureGf() loads in
// the browser bridge.
await micropip.install(["requests", "pyodide-http"]);
await py.runPythonAsync(`
import pyodide_http
pyodide_http.patch_all()
from pyrocko import gf
from pyrocko.gf import store as _gfstore
from pyrocko import (
    eikonal_ext, parstack_ext, ahfullgreen_ext, autopick_ext,
    util_ext, orthodrome_ext)
from pyrocko.modelling import okada_ext, disloc_ext
assert _gfstore.store_ext is not None, "gf.store.store_ext did not load"
eng = gf.LocalEngine(store_dirs=[])
assert eng.get_store_ids() == [], "unexpected GF stores"
print("gf ok: import pyrocko.gf + LocalEngine + store_ext/eikonal_ext/okada_ext"
      "/parstack_ext/ahfullgreen_ext/autopick_ext/util_ext/orthodrome_ext/disloc_ext")
`);

console.log("SMOKE TEST PASSED");
