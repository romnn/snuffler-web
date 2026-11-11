# Threaded Pyodide 314 (pthread + OpenMP + SIMD)

Builds the Pyodide runtime the app runs on from **official sources only**, plus a
small patchset maintained here; no fork is a build dependency.

## What it builds

- **Official Pyodide** at a pinned commit (`build.sh` `ref=`), currently
  Python 3.14.2 / emscripten 5.0.3, in the official
  `pyodide/pyodide-env:20260211-…-py314` image.
- **`patches/0001-pthread.patch`**: the pthread enablement (`Makefile`,
  `src/core/pre.js`, `src/core/stack_switching/stack_switching.mjs`, and a new
  `tools/fixup-threading-support.py`). It guards Pyodide's JS init behind
  `!ENVIRONMENT_IS_PTHREAD` so worker threads don't re-run it, builds with
  `-pthread`, and post-processes the asm for threading.
  The approach follows `yosefk/pyodide-pthread` (MPL-2.0, same as Pyodide),
  ported to current official Pyodide.
- **`libomp/`**: the LLVM OpenMP runtime for wasm32-emscripten, which emscripten
  does not ship. `build.sh` whole-archives it into the main module so every
  `-fopenmp` extension shares one OpenMP runtime.
- **Packages**: `build-packages.sh` builds the threaded package set (numpy, scipy,
  matplotlib, …) from pinned `pyodide-recipes`; `build-pyrocko.sh` builds the
  pyrocko wheel with `PYROCKO_OPENMP=1`, `-sSHARED_MEMORY` and `-msimd128` from
  `pybuild/setup.py`.

## Build

```sh
docker/pyodide-mt/build-all.sh   # runtime, packages, pyrocko, then stage-web.sh
```

`stage-web.sh` assembles `served/`, which Trunk copies to `dist/py-mt/`;
`check-lock.sh` verifies that the staged lock satisfies the app's
`loadPackage()` closure, and `precompile.sh` ships bytecode so a boot does not
recompile every module.
`fetch-web.sh` (`task pyodide:fetch`) downloads the deployed runtime into
`served/` instead, which is all you need unless you change the runtime itself.
`.work/`, `out/` and `served/` are gitignored.

## Gotchas

- The all-but-packages main lacks libc/libc++ symbols packages need; `build.sh`
  whole-archives the PIC variants into main.
- emscripten's clang ships no `omp.h`, so pyrocko's `#include <omp.h>` needs `-I`
  pointing at the libomp source's `include/` (`build-pyrocko.sh`).
- Never pass `-msimd128` to the core CPython build (see `build.sh`); SIMD only
  pays off in the package kernels.
- pyrocko's method-1 `parstack` kernel has no `num_threads(nparallel)` clause, so
  it always uses every core; method 0 scales with `nparallel`.
