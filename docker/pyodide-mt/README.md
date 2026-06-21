# Fork-free pthread + OpenMP Pyodide (our own patchset)

Builds a threaded, OpenMP-capable Pyodide from **official sources only**, with a
**small patchset we maintain ourselves** — no fork is a build dependency. This
supersedes `../pyodide-pthread/`, which cloned the `yosefk/pyodide-pthread` fork
to prove the concept (see that directory's README for the validated results,
including the **~22× OpenMP speedup on a real pyrocko kernel**).

## What it builds

- **Official Pyodide** at a pinned commit (`build.sh` `ref=`), currently
  Python 3.14.2 / emscripten 5.0.3, in the **official**
  `pyodide/pyodide-env:20260211-…-py314` image.
- **`patches/0001-pthread.patch`** — our maintained pthread-enablement patch
  (4 files: `Makefile`, `src/core/pre.js`, `src/core/stack_switching/stack_switching.mjs`,
  and a new `tools/fixup-threading-support.py`). It guards Pyodide's JS init
  behind `!ENVIRONMENT_IS_PTHREAD` so worker threads don't re-run it, builds with
  `-pthread`, and post-processes the asm for threading. Verified to apply cleanly
  to the pinned official commit.

  Provenance: the patch follows the approach pioneered by `yosefk/pyodide-pthread`
  (MPL-2.0, same as Pyodide); we re-derived/ported it against current official
  Pyodide so we own and maintain it directly rather than tracking a fork.

## Build

```sh
docker/pyodide-mt/build.sh        # → docker/pyodide-mt/out/
```

`out/` and `.work/` are gitignored.

## Status: threading + OpenMP WORK (validated)

`build.sh` produces a runtime that **builds, loads, threads, and runs OpenMP** —
all from official sources + only our patch:

- `sys._emscripten_info`: `pthreads=True, shared_memory=True`, python 3.14.2,
  emscripten (5, 0, 3).
- raw pthreads in an imported C-ext: `pthreadext.run(4) = (4, 4, 4)` (sync+async).
- OpenMP in an imported C-ext (libomp in main): `ompext.run(4) = (4, 48, …)`.
- **real pyrocko `parstack` kernel** (24-core host): near-linear speedup
  `1 / 2.00 / 3.84 / 7.75 / 14.61 / 18.61×` at 1/2/4/8/16/24 threads, result
  bit-identical to the em4.0.9 baseline.

Six em5.0.3-vs-em4.0.9 differences were fixed in our own tree (see `build.sh`
comments + `patches/0001-pthread.patch`); the decisive one was that our fixup
referenced `_is_sentinel`/`_create_sentinel`, which em4 auto-generated but em5
does not — a dangling ref that crashed every spawned worker at boot and looked
exactly like a dlsync deadlock (now guarded with `typeof`).

To reproduce the smoke/bench on this tree, point the `../pyodide-pthread`
harnesses at it, e.g. `PYODIDE_BUILD_TREE=$PWD/.work/pyodide
IMAGE=pyodide/pyodide-env:20260211-chrome145-firefox146-py314
../pyodide-pthread/libomp/build.sh` (the smoke/bench runners need the same two
overrides + a python3.14 include glob).

## Packages (numpy + pyrocko) — WORKING end-to-end

`packages.sh` builds numpy; `build-pyrocko.sh` builds the pyrocko wheel
(`PYROCKO_OPENMP=1` + `-sSHARED_MEMORY` + `-msimd128`) via Pyodide's in-tree
`build-recipes` pipeline (NOT `pyodide build`, which needs an out-of-tree
xbuildenv). Both load into the threaded runtime and run via Python:

- `numpy 2.4.3` + `pyrocko 2026.06.02` import and compute correctly.
- `pyrocko.parstack` (method 0, through numpy arrays) scales across `nparallel`:
  `1 / 1.87 / 3.26 / 4.37 / 5.14 / 5.53×` at 1/2/4/8/16/24 threads, checksum
  bit-identical — real multithreading through the full numpy→pyrocko→OpenMP path.

Two gotchas worth keeping: (1) the all-but-packages main lacks libc/libc++
symbols packages need, fixed by whole-archiving the PIC variants into main
(build.sh); (2) em5's clang ships no `omp.h`, so pyrocko's `#include <omp.h>`
needs `-I` pointing at the libomp source's `include/` (build-pyrocko.sh).

## Next steps

1. **Make the OpenMP runtime fork-free too**: `build.sh` still fetches the libomp
   source from the PR #25937 branch; vendor it (or build from official LLVM
   openmp, whose wasm32 `KMP_ARCH` support is upstream) so nothing is a fork.
2. **Wire into the app** behind the existing `?threads=1` toggle (serve this
   runtime + the numpy/pyrocko wheels via trunk; CDN single-threaded stays default).
3. **Tune Python-level speedup**: pyrocko's method-1 kernel has no
   `num_threads(nparallel)` clause so it always uses all cores; method-0 scales
   with `nparallel`. Heavier real workloads scale better than the demo above.
