#!/usr/bin/env bash
#
# Threaded-C-ext smoke test for the locally-built pthread Pyodide. Proves the
# capability that gates the heavy numpy/scipy/pyrocko rebuild: an *imported*
# C-extension running OpenMP threads in wasm.
#
# build.sh links libomp into the MAIN module, so the smoke modules here are thin
# (they resolve __kmpc_*/omp_* from main). Imported into the runtime under node
# (node has SharedArrayBuffer natively, so this tests the *runtime*, not the
# browser cross-origin plumbing):
#   ompext.so     — #pragma omp / OpenMP   GATING: must pass (the pyrocko path)
#   pthreadext.so — raw pthreads           informational (see note below)
#
# Note: raw pthread_create from the main thread needs the event loop (or a
# pre-spawned pool, which would break dlopen'd side modules), and once libomp is
# in main it can't complete synchronously here — so pthreadext is informational.
# pyrocko parallelism goes through OpenMP, which works.
#
# Prerequisite: docker/pyodide-pthread/build.sh has populated .work + dist + libomp.
# Usage: docker/pyodide-pthread/smoke/run.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="$here/../.work/pyodide-pthread"
image="pyodide/pyodide-env:20250915-chrome140-firefox140-py313"

if [ ! -f "$build/dist/pyodide.mjs" ]; then
    echo "ERROR: $build/dist not found — run docker/pyodide-pthread/build.sh first." >&2
    exit 1
fi

inc_rel="$(cd "$build" && ls -d cpython/installs/python-*/include/python3.13 | head -1)"
ver="$(grep -m1 '"version"' "$build/dist/package.json" | sed -E 's/.*"version" *: *"([^"]+)".*/\1/')"

# loadPyodide needs a lock file; all-but-packages doesn't emit one, and we load
# no packages here, so a minimal empty-package lock is enough.
if [ ! -f "$build/dist/pyodide-lock.json" ]; then
    cat > "$build/dist/pyodide-lock.json" <<JSON
{ "info": { "arch": "wasm32", "platform": "emscripten_4_0_9_wasm32",
            "version": "$ver", "python": "3.13.2", "abi_version": "2025_0" },
  "packages": {} }
JSON
fi

mkdir -p "$build/smoke"
cp "$here/pthreadext.c" "$here/ompext.c" "$here/test.mjs" "$build/smoke/"

docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e INC="$inc_rel" \
    "$image" \
    bash -lc '
        source emsdk/emsdk/emsdk_env.sh 2>/dev/null
        # thin side modules: -fopenmp emits __kmpc_* calls, resolved from main.
        flags="-O2 -fPIC -sSIDE_MODULE=1 -pthread -sSHARED_MEMORY -Wno-experimental -I $INC"
        echo "--- compiling thin side modules ---"
        emcc smoke/pthreadext.c -o smoke/pthreadext.so $flags
        emcc smoke/ompext.c     -o smoke/ompext.so     $flags -fopenmp
        cd smoke
        echo "--- OpenMP (gating: the pyrocko path) ---"
        node test.mjs ompext
        echo "--- raw pthreads (informational) ---"
        node test.mjs pthreadext \
          || echo "pthreadext: known limitation — raw pthread_create from main needs the event loop; pyrocko uses OpenMP (see README)"
    '
