// Runs the real pyrocko parstackf kernel (compiled as a thin -fopenmp side
// module resolving libomp from the main module) at increasing thread counts and
// reports wall-clock speedup. The checksum must be identical across thread
// counts — that is the parallel-correctness check (same result, more threads).
//
// OpenMP calls go through runPythonAsync (libomp spawns its pool from the main
// thread by unwinding to the event loop). Workload via BENCH_CFG env:
// "narrays,nshifts,nsamp,method,reps" (default below).
import fs from "node:fs";

const cfg = process.env.BENCH_CFG || "40,20000,1000,1,3";
const threadList = (process.env.BENCH_THREADS || "1,2,4,8")
  .split(",")
  .map((x) => parseInt(x, 10));

const { loadPyodide } = await import("../dist/pyodide.mjs");
const py = await loadPyodide({ fullStdLib: false });
const api = py._api || (py._module && py._module.API);
py.FS.writeFile("/parbench.so", fs.readFileSync("./parbench.so"));
await api.loadDynlib("/parbench.so", false);
py.runPython(`
import importlib.machinery, importlib.util, sys
spec = importlib.util.spec_from_file_location(
    "parbench", "/parbench.so",
    loader=importlib.machinery.ExtensionFileLoader("parbench", "/parbench.so"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
sys.modules["parbench"] = m
`);

console.log(`workload: narrays,nshifts,nsamp,method,reps = ${cfg}`);
console.log("threads |  ms/rep | speedup | checksum");
let base = null;
let baseSum = null;
for (const n of threadList) {
  const out = await py.runPythonAsync(
    `import parbench; r = parbench.bench(${n}, ${cfg}); f"{r[0]:.1f} {r[1]:.6e} {r[2]}"`,
  );
  const [msStr, sumStr] = out.split(" ");
  const ms = parseFloat(msStr);
  if (base === null) {
    base = ms;
    baseSum = sumStr;
  }
  const speedup = (base / ms).toFixed(2);
  const ok = sumStr === baseSum ? "" : "  <-- CHECKSUM MISMATCH";
  console.log(
    `${String(n).padStart(7)} | ${msStr.padStart(7)} | ${String(speedup).padStart(6)}x | ${sumStr}${ok}`,
  );
}
