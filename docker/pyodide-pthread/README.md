# pthread + OpenMP Pyodide (experimental, separate, additive)

A reproducible Docker build of a **pthread-enabled Pyodide** plus a **libomp.a
(LLVM OpenMP runtime) built for wasm32-emscripten**, so C extensions can use real
threads *and* `#pragma omp`. Kept entirely separate from the working app: the
default build keeps loading the stock single-threaded Pyodide from the CDN, and
cross-origin isolation (the SharedArrayBuffer prerequisite) is already wired as
the opt-in `?threads=1` toggle (`web/coi-serviceworker.js`).

## Build + smoke test

```sh
docker/pyodide-pthread/build.sh        # → core runtime in .work + out/
docker/pyodide-pthread/smoke/run.sh    # builds libomp.a + proves threading
# task pyodide:build-pthread / task pyodide:smoke-pthread do the same
```

`build.sh` clones `yosefk/pyodide-pthread@mt` (the pthread patch set), builds the
core runtime (`all-but-packages`) in `pyodide/pyodide-env:…-py313` with
`EXTRA_CFLAGS=-pthread`, then **builds `libomp.a` and relinks the main module
with it** (see below) so OpenMP is available to every imported C-ext. Result:
Pyodide 0.29.0 / Python 3.13.2 / emscripten 4.0.9, `sys._emscripten_info`
reporting `pthreads=True, shared_memory=True`, and the main module exporting
`__kmpc_*`/`omp_*`. `smoke/run.sh` then imports thin C-ext side modules (no
bundled libomp) into the runtime under node.

## Smoke test results (`smoke/run.sh`)

| module | what it does | result |
| --- | --- | --- |
| `ompext.so` | `#pragma omp parallel num_threads(4)` + reduction, resolving libomp from main | **PASS** (gating) — `run(4) = (4, 48, 23999980)`: 4 threads, 48 cores detected, reduction correct, 3/3 reliable |
| `pthreadext.so` | spawns N raw pthreads | informational — raw `pthread_create` from the main thread needs the event loop / a pool, which can't complete here once libomp is in main; pyrocko uses OpenMP |

## OpenMP speedup on a REAL pyrocko kernel (`bench/run.sh`)

`bench/run.sh` extracts pyrocko's actual `parstackf` seismic-stacking kernel from
the fetched source (not vendored — pyrocko is GPL), builds it as a thin
`-fopenmp` side module against the libomp-in-main runtime, and times it at
increasing thread counts (workload: 40 traces × 1000 samples, 20000 stacking
shifts, method 1 / semblance-max). Measured on a 24-core host:

| threads | 1 | 2 | 4 | 8 | 16 | 24 |
| --- | --- | --- | --- | --- | --- | --- |
| ms/rep | 189.8 | 92.0 | 45.5 | 23.1 | 14.5 | 8.6 |
| speedup | 1.00× | 2.06× | 4.17× | 8.22× | 13.09× | **22.07×** |

Near-linear scaling, and the result checksum is **identical across all thread
counts** (parallel == serial, bit-for-bit). This validates the premise: OpenMP
gives real, large speedups for actual pyrocko compute in browser-grade Pyodide.

## libomp.a — the OpenMP runtime (`libomp/build.sh`)

emscripten ships **no** OpenMP: PR
[emscripten-core/emscripten#25937](https://github.com/emscripten-core/emscripten/pull/25937),
which vendors libomp, is unmerged (em main is 6.0). But LLVM ≥21 already has
wasm32 OpenMP **codegen** (D142593), so `#pragma omp` compiles to the libomp ABI
(`__kmpc_*`); only the runtime library is missing. `libomp/build.sh` builds it:

1. Fetches the wasm-patched openmp source from `JAicewizard/emscripten@67a6100`
   (the PR #25937 branch), pinned by SHA. The wasm port lives in
   `z_Linux_util.cpp` (a C `__kmp_invoke_microtask`), `kmp_platform.h`
   (`KMP_ARCH_WASM`), `kmp_runtime.cpp`, `kmp_gsupport.cpp`, and a wasm
   `kmp_config.h` (HWLOC/STATS/OMPT/ITT all off).
2. Compiles ~31 sources with emcc per the PR's flags, **plus**
   `kmp_wasm_compat.c` — a 3-line shim defining `__kmp_unnamed_critical_addr`
   (the GOMP critical-lock pointer the native `z_Linux_asm.S` provides but wasm
   omits; without it the `.so` loads-fails with "bad export type … undefined").
3. Archives `libomp.a` (~868 KB, defines `__kmpc_fork_call`,
   `__kmpc_global_thread_num`, `omp_*`, the C `__kmp_invoke_microtask`, …).

`build.sh` then relinks the main module with `-Wl,--whole-archive libomp.a` so the
runtime lives in main and `MAIN_MODULE=1`/`EXPORT_ALL` re-export it; a `-fopenmp`
C-ext is then a *thin* side module whose `__kmpc_*` calls resolve from main.

### Things discovered

- **The compat shim** (above) — one undefined symbol otherwise.
- **No `-sPTHREAD_POOL_SIZE`** — a *pre-spawned* pool breaks side modules
  dlopen'd later: those workers were created before the module loaded, so libomp
  dispatching a microtask to them hits "table index out of bounds". With dynamic
  spawn, libomp creates its workers *after* the module loads, so they have its
  table. OpenMP from main is then reliable (3/3) under both `runPython` and
  `runPythonAsync` in node.
- **Raw `pthread_create` from the main thread** still needs the event loop and
  can't complete synchronously once libomp is in main (it throws `unwind`) — so
  `pthreadext` is informational. Not the pyrocko path (pyrocko uses OpenMP). In
  the browser, OpenMP calls go through the worker bridge / `runPythonAsync`.

## Findings (evidence-based)

1. **Imported C-extension modules run real, concurrent OS threads** (refutes the
   earlier "fork only threads ctypes CDLLs" claim).
2. **OpenMP works, sharing one runtime in the main module** — `ompext.run(4)` ran
   a 4-thread parallel region with a correct reduction (`(4, 48, …)`), resolving
   libomp from main. This is the path pyrocko's kernels use (`#pragma omp`, gated
   behind `PYROCKO_OPENMP`); numpy/scipy/etc. would share the same runtime.
3. **Python threads stay serial** — only C thread pools run in workers.

## Next steps (task #35)

- **Migrate to latest Pyodide** (314.0.0 / Python 3.14.2 / emscripten 5.0.3) and
  maintain the small (~25-line) yosefk pthread patch ourselves, rather than
  pinning the fork's branch. Upstream Pyodide still has no pthread support
  ([#237](https://github.com/pyodide/pyodide/issues/237)), and no released
  emscripten has OpenMP, so both the patch and `libomp.a` stay our responsibility
  — but both are small and now proven.

## If/when wired into the app

Serve the runtime locally instead of the CDN (additive, gated on `?threads=1`):
`PYODIDE_BASE` in `web/bridge_worker.js` would point at a Trunk-copied `out/`
only when `crossOriginIsolated`, and the pyrocko wheel would be rebuilt to match
(matching Pyodide ABI + `-sSHARED_MEMORY=1` + `PYROCKO_OPENMP=1`). Both stay
behind the toggle so the CDN path remains the default.
