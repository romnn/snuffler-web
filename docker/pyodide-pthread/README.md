# pthread-enabled Pyodide (experimental, separate, additive)

A reproducible Docker build of a **pthread-enabled Pyodide** so the C extensions
can use real threads. This is kept entirely separate from the working app: the
default build keeps loading the stock single-threaded Pyodide from the CDN, and
cross-origin isolation (the SharedArrayBuffer prerequisite) is already wired as
the opt-in `?threads=1` toggle (`web/coi-serviceworker.js`).

## Build

```sh
docker/pyodide-pthread/build.sh        # → docker/pyodide-pthread/out/
task pyodide:build-pthread             # same, via the task runner
```

It clones `yosefk/pyodide-pthread@mt` (the only known working pthread patch set
for Pyodide) and builds the **core runtime** (`all-but-packages`) in
`pyodide/pyodide-env:20250915-…-py313` with `EXTRA_CFLAGS=-pthread` /
`EXTRA_LDFLAGS="-pthread -g1 -sMAXIMUM_MEMORY=4GB"`. `out/` and `.work/` are
gitignored. The result is Pyodide 0.29.0 / Python 3.13.2 / emscripten 4.0.9 with
`sys._emscripten_info` reporting `pthreads=True, shared_memory=True`.

## Smoke test (run this before any heavy package rebuild)

```sh
docker/pyodide-pthread/smoke/run.sh    # needs build.sh to have run first
```

It compiles two emscripten side modules and `import`s them into the built
pthread Pyodide under node (node has `SharedArrayBuffer` natively, so this tests
the *runtime*, not the browser plumbing):

| module | what it does | result |
| --- | --- | --- |
| `pthreadext.so` | spawns N raw pthreads, tracks peak concurrency | **PASS** — `run(4) = (4, 4, 4)`: 4 threads, all 4 overlapping |
| `ompext.so` | a `#pragma omp parallel` region | **FAIL** — `cannot resolve symbol __kmpc_global_thread_num` |

## Findings (evidence-based, replaces the earlier speculation)

1. **Imported C-extension modules DO run real, concurrent OS threads.** The
   earlier note that the fork "only threads ctypes `CDLL`s, not imported C-ext
   modules" was **wrong**: `import pthreadext` then `pthreadext.run(4)` returns
   `max_concurrent == 4`, i.e. four pthreads created from inside an imported
   `.so` genuinely ran at the same time. The pthread substrate (`pthread_create`
   / `pthread_join` / `<stdatomic.h>`) resolves against the pthread-enabled main
   module and works. This is the capability pyrocko's kernels need.

2. **OpenMP (`#pragma omp`) does NOT work — emscripten ships no OpenMP runtime.**
   clang accepts `-fopenmp` and lowers the pragmas to the libomp ABI
   (`__kmpc_*`), but emscripten 4.0.9 provides no `libomp`: there is no
   `libomp*.a` in the sysroot, `system_libs.py` has no OpenMP entry, and linking
   even a *main* module with omp pragmas fails (`undefined symbol:
   __kmpc_fork_call`, `omp_get_num_threads`). So `import ompext` aborts at
   dynamic-link time. (This emsdk's LLVM also omits `omp.h` from the clang
   resource dir, a separate, smaller gap.)

   This matches the fork author's own note that OpenMP "failed to load" and that
   they hand-rolled a thread pool instead.

3. **Side modules must match for packages**: numpy/scipy/etc. would need
   rebuilding with `-sSHARED_MEMORY=1` to dynamically link into a pthread main
   module — a full-distribution rebuild. But note (2): a `SHARED_MEMORY` rebuild
   alone does **not** parallelize stock numpy/scipy, because their C parallelism
   goes through OpenMP/BLAS, which has no runtime here.

4. **Python threads stay serial** — only C thread pools run in workers; no
   Python code executes in worker threads.

## What this means for parallel pyrocko

The threading *substrate* works in imported C-exts, so a real speedup is
reachable, but **not** by just adding `-fopenmp` to pyrocko's kernels. Two
viable paths, both real work:

- **(a) Provide an emscripten OpenMP runtime.** Cross-compile LLVM's `openmp`
  (libomp) to `wasm32-emscripten` and link it into the *main* module so all
  side modules (numpy/scipy/pyrocko) share one runtime + thread pool. The
  "proper" path; emscripten upstream hasn't shipped this, so expect patching.
- **(b) Hand-thread the hot kernels.** Replace pyrocko's `#pragma omp parallel
  for` with raw pthreads / a small thread pool (proven to work in (1)). Heavy,
  per-kernel porting, but no new toolchain.

**Bottom line:** the cross-origin-isolation foundation and the pthread core are
done and reversible, and imported C-exts can thread. Getting an *actual* OpenMP
speedup for pyrocko is gated on path (a) or (b) above — at/beyond the edge of
current Pyodide tooling, and (per the project's own note) a marginal payoff for
a browser app. This directory exists so the experiment is reproducible and the
exact blocker is documented, not because the end-to-end result works today.

## If/when `out/` is wired into the app

Serve it locally instead of the CDN (additive, gated on `?threads=1`):
`PYODIDE_BASE` in `web/bridge_worker.js` would point at a Trunk-copied `out/`
(e.g. `copy-dir` → `pyodide-rt/`) only when `crossOriginIsolated`, and the
pyrocko wheel would be rebuilt to match (Pyodide 0.29 ABI + `-sSHARED_MEMORY=1`,
and a parallel kernel strategy per path (a)/(b)). Both stay behind the toggle so
the CDN path remains the default.
