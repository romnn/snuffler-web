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

## Status / next steps

1. **Core pthread build** (this script) — in progress.
2. **OpenMP/libomp**: build `libomp.a` from **official LLVM** openmp source
   matching em5.0.3's clang (verify wasm32 `KMP_ARCH` is upstream, ref LLVM
   D142593; if so, only our `kmp_wasm_compat.c` + a wasm `kmp_config.h` are
   needed — no fork), then whole-archive it into the main module (as proven in
   `../pyodide-pthread`).
3. **Packages**: build numpy + the pyrocko wheel (`PYROCKO_OPENMP=1` +
   `-sSHARED_MEMORY`) via Pyodide's own package pipeline (NOT separately against
   an `all-but-packages` main — that exposes libc gaps), then re-run the parstack
   benchmark end-to-end through Python.
4. **Wire into the app** behind the existing `?threads=1` toggle (CDN
   single-threaded stays the default).
