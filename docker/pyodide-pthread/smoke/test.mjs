// Loads the locally-built pthread Pyodide in node, then imports each smoke
// side-module (.so) and runs it. node has SharedArrayBuffer natively, so no
// COOP/COEP is needed here — this isolates the *runtime* question (can an
// imported C-ext thread?) from the browser cross-origin-isolation plumbing.
//
// Run from the build tree's smoke/ dir (run.sh handles that): dist is at ../dist
// and the prebuilt .so files sit next to this file.
import fs from "node:fs";

const { loadPyodide } = await import("../dist/pyodide.mjs");
const py = await loadPyodide({ fullStdLib: false });
console.log("pyodide", py.version, "loaded");
console.log(
  py.runPython(`
import sys
i = sys._emscripten_info
f"core: pthreads={getattr(i,'pthreads','?')} shared_memory={getattr(i,'shared_memory','?')} emscripten={getattr(i,'emscripten_version','?')}"
`),
);

async function loadSo(path) {
  const api = py._api || (py._module && py._module.API);
  if (api && api.loadDynlib) {
    await api.loadDynlib(path, false);
    return;
  }
  if (py.loadDynlib) {
    await py.loadDynlib(path);
    return;
  }
  throw new Error("no loadDynlib on this pyodide build");
}

for (const mod of ["pthreadext", "ompext"]) {
  const so = `./${mod}.so`;
  if (!fs.existsSync(so)) {
    console.log(`${mod}: not built, skipping`);
    continue;
  }
  try {
    py.FS.writeFile(`/${mod}.so`, fs.readFileSync(so));
    await loadSo(`/${mod}.so`);
    const res = py.runPython(`
import importlib.machinery, importlib.util
spec = importlib.util.spec_from_file_location(
    "${mod}", "/${mod}.so",
    loader=importlib.machinery.ExtensionFileLoader("${mod}", "/${mod}.so"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
str(m.run(4))
`);
    console.log(`${mod}: PASS  run(4) = ${res}`);
  } catch (e) {
    const msg = String(e).split("\n")[0];
    const expected = /__kmpc_|undefined symbol.*omp/.test(msg);
    console.log(
      `${mod}: ${expected ? "FAIL (expected: no emscripten OpenMP runtime)" : "FAIL"}  ${msg}`,
    );
  }
}
