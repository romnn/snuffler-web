import { loadPyodide } from "./pyodide.mjs";
const post = (m) => self.postMessage(String(m));
try {
  post("crossOriginIsolated=" + self.crossOriginIsolated);
  const py = await loadPyodide();
  post("pyodide " + py.version + " loaded, pthreads=" + py.runPython("import sys; str(sys._emscripten_info.pthreads)"));
  await py.loadPackage("pyrocko");
  post("packages loaded: " + py.runPython("import numpy,scipy,matplotlib,pyrocko; 'np'+numpy.__version__+' sp'+scipy.__version__+' mpl'+matplotlib.__version__+' pyrocko'+pyrocko.__version__"));
  py.runPython(`
import numpy as np, time
from pyrocko.parstack import parstack
narrays,nsamp,nshifts=30,1000,4000
base=(np.arange(nsamp*narrays,dtype=np.float64).reshape(narrays,nsamp)%7)
arrays=[base[i].copy() for i in range(narrays)]
offsets=np.zeros(narrays,dtype=np.int32)
shifts=(np.arange(nshifts*narrays,dtype=np.int32).reshape(nshifts,narrays)%50).astype(np.int32)
weights=np.ones((nshifts,narrays))
def run(n):
    parstack(arrays,offsets,shifts,weights,0,nparallel=n)
    t=time.time()
    for _ in range(3): r,off=parstack(arrays,offsets,shifts,weights,0,nparallel=n)
    return (time.time()-t)*1000/3, float(r.sum())
`);
  const r1 = await py.runPythonAsync("ms,s=run(1); f'{ms:.1f} {s:.5e}'");
  const r8 = await py.runPythonAsync("ms,s=run(8); f'{ms:.1f} {s:.5e}'");
  post("parstack nparallel=1: " + r1 + " ms");
  post("parstack nparallel=8: " + r8 + " ms");
  const b1 = parseFloat(r1), b8 = parseFloat(r8);
  post("SPEEDUP=" + (b1/b8).toFixed(2) + "x");
  post("DONE");
} catch (e) { post("ERROR: " + (e && e.message || e)); }
