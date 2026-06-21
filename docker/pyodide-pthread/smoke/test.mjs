// Loads the locally-built pthread Pyodide in node and imports ONE smoke
// side-module (.so), passed as argv, then runs it. node has SharedArrayBuffer
// natively, so this isolates the *runtime* question (can an imported C-ext
// thread?) from the browser cross-origin-isolation plumbing.
//
// One module per process on purpose: pthreadext's synchronous pthread
// create/join and libomp's lazy worker-pool spawn don't coexist cleanly in a
// single interpreter without a pre-sized pool (-sPTHREAD_POOL_SIZE in the main
// module — the eventual fix), and mixing them mid-run throws "unwind". Separate
// processes keep each test deterministic.
//
//   node test.mjs pthreadext  → raw pthreads, sync runPython, expect (4,4,4)
//   node test.mjs ompext      → #pragma omp, async runPython (libomp spawns its
//                               pool on the first region, which emscripten can
//                               only do from the main thread by unwinding to the
//                               event loop), expect (4, <cores>, <checksum>)
import fs from "node:fs";

const mod = process.argv[2];
if (!mod) {
  console.error("usage: node test.mjs <pthreadext|ompext>");
  process.exit(2);
}

const { loadPyodide } = await import("../dist/pyodide.mjs");
const py = await loadPyodide({ fullStdLib: false });
if (mod === "pthreadext") {
  console.log("pyodide", py.version, "—", py.runPython(`
import sys
i = sys._emscripten_info
f"core: pthreads={getattr(i,'pthreads','?')} shared_memory={getattr(i,'shared_memory','?')} emscripten={getattr(i,'emscripten_version','?')}"
`));
}

const api = py._api || (py._module && py._module.API);
py.FS.writeFile(`/${mod}.so`, fs.readFileSync(`./${mod}.so`));
await api.loadDynlib(`/${mod}.so`, false);
py.runPython(`
import importlib.machinery, importlib.util, sys
spec = importlib.util.spec_from_file_location(
    "${mod}", "/${mod}.so",
    loader=importlib.machinery.ExtensionFileLoader("${mod}", "/${mod}.so"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
sys.modules["${mod}"] = m
`);

try {
  if (mod === "ompext") {
    const r = await py.runPythonAsync(`import ompext; str(ompext.run(4))`);
    console.log(`ompext: PASS  run(4) = ${r}  [num_threads, max_threads, checksum]`);
  } else {
    const r = py.runPython(`import pthreadext; str(pthreadext.run(4))`);
    console.log(`pthreadext: PASS  run(4) = ${r}  [created, completed, max_concurrent]`);
  }
} catch (e) {
  console.log(`${mod}: FAIL  ${String(e.message || e).split("\n")[0]}`);
  process.exit(1);
}
