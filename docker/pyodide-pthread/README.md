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

`build.sh` clones `yosefk/pyodide-pthread@mt` (the pthread patch set) and builds
the core runtime (`all-but-packages`) in `pyodide/pyodide-env:…-py313` with
`EXTRA_CFLAGS=-pthread`. Result: Pyodide 0.29.0 / Python 3.13.2 / emscripten
4.0.9 with `sys._emscripten_info` reporting `pthreads=True, shared_memory=True`.
`smoke/run.sh` then builds `libomp.a` (see below) and imports two C-ext side
modules into the runtime under node.

## Smoke test results (`smoke/run.sh`)

| module | what it does | result |
| --- | --- | --- |
| `pthreadext.so` | spawns N raw pthreads, tracks peak concurrency | **PASS** — `run(4) = (4, 4, 4)`: 4 threads, all overlapping |
| `ompext.so` | `#pragma omp parallel num_threads(4)` + reduction | **PASS** — `run(4) = (4, 48, 23999980)`: 4 threads, 48 cores detected, reduction correct |

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

A `-fopenmp` C-ext then links `libomp.a` and the `__kmpc_*` calls resolve.

### Two requirements discovered

- **The compat shim** (above) — one undefined symbol otherwise.
- **`runPythonAsync`** for the OpenMP call: libomp spawns its worker pool lazily
  on the first parallel region, and emscripten can only create pthreads from the
  main thread by unwinding to the JS event loop (Asyncify / stack switching). A
  synchronous `runPython` throws `unwind`. For robust mixing of OpenMP with other
  threading in one interpreter, the main module needs a pre-sized pool
  (`-sPTHREAD_POOL_SIZE`) — see "next steps".

## Findings (evidence-based)

1. **Imported C-extension modules run real, concurrent OS threads** (refutes the
   earlier "fork only threads ctypes CDLLs" claim): `pthreadext.run(4)` →
   `max_concurrent == 4`.
2. **OpenMP works** once a wasm libomp is present — `ompext.run(4)` ran a 4-thread
   parallel region with a correct reduction. This is the path pyrocko's kernels
   use (`#pragma omp`, gated behind `PYROCKO_OPENMP`).
3. **Python threads stay serial** — only C thread pools run in workers.

## Next steps (tasks #34, #35)

- **libomp into the main module** + `-sPTHREAD_POOL_SIZE`: one shared OpenMP
  runtime + pre-spawned pool for all side modules (numpy/scipy/pyrocko), so
  `-fopenmp` works everywhere and the spawn-from-main unwind goes away.
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
