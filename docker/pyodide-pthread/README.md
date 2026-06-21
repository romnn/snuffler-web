# pthread-enabled Pyodide (experimental, separate, additive)

A reproducible Docker build of a **pthread-enabled Pyodide** so the C extensions
can use real threads (the goal: OpenMP in pyrocko's `*_ext` modules). This is
kept entirely separate from the working app: the default build keeps loading the
stock single-threaded Pyodide from the CDN, and cross-origin isolation (the
SharedArrayBuffer prerequisite) is already wired as the opt-in `?threads=1`
toggle (`web/coi-serviceworker.js`).

## Build

```sh
docker/pyodide-pthread/build.sh        # → docker/pyodide-pthread/out/
task pyodide:build-pthread             # same, via the task runner
```

It clones `yosefk/pyodide-pthread@mt` (the only known working pthread patch set
for Pyodide) and builds it in `pyodide/pyodide-env:20250915-…-py313` with
`EXTRA_CFLAGS=-pthread` / `EXTRA_LDFLAGS="-pthread -g1 -sMAXIMUM_MEMORY=4GB"`.
Expect a **multi-hour** build and tens of GB of disk; `out/` and `.work/` are
gitignored.

## Feasibility verdict (read before investing hours)

Research (pyodide #237, the yosefk fork + blog, the Pyodide FAQ) converges on:

1. **The fork only threads C/C++ loaded as ctypes `CDLL`s** — "probably not much
   else." pyrocko's parallel kernels are *imported C-extension modules*
   (`import pyrocko.signal_ext`), not CDLLs, so this fork does **not** obviously
   make them multithreaded.
2. **OpenMP itself is reported broken under Pyodide** (oneTBB/OpenMP "failed to
   load"; even TaskFlow "gets stuck"). The author hand-rolled a thread pool.
3. **Side modules must match**: numpy/scipy/etc. would need rebuilding with
   `-sSHARED_MEMORY=1`; the stock distribution wheels won't dynamically link into
   a pthread main module. That's a full-distribution rebuild.
4. **Python threads stay serial** — only C thread pools run in workers; "no
   Python code executes in worker threads."
5. Version skew: the fork's env is py313 (Pyodide ≳0.28); our app + wheel target
   0.27.2 (`pyodide_2024_0_wasm32`), so the wheel would need rebuilding too.

**Bottom line:** the cross-origin-isolation foundation is done and reversible.
Getting an *actual* OpenMP speedup for pyrocko through this path is at/beyond the
edge of current Pyodide tooling and, per the project's own note, a marginal
payoff for a browser app. This directory exists so the experiment is reproducible
and ready to run, not because the end-to-end result is expected to work today.

## If/when `out/` is produced

Serve it locally instead of the CDN (additive, gated on `?threads=1`):
`PYODIDE_BASE` in `web/bridge_worker.js` would point at a Trunk-copied
`out/` (e.g. `copy-dir` → `pyodide-rt/`) only when `crossOriginIsolated`, and
the pyrocko wheel would be rebuilt with `PYROCKO_OPENMP=1` (see
`pybuild/setup.py`). Both stay behind the toggle so the CDN path remains default.
