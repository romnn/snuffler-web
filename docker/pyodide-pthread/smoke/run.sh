#!/usr/bin/env bash
#
# Threaded-C-ext smoke test for the locally-built pthread Pyodide. Answers the
# question that gates the heavy numpy/scipy/pyrocko SHARED_MEMORY rebuild:
# can an *imported* C-extension module actually run threads in wasm?
#
# Builds two emscripten side modules against the build tree's emsdk + CPython
# headers and imports them into the pthread Pyodide under node:
#   pthreadext.so — raw pthreads        (expected: PASS, real concurrency)
#   ompext.so     — #pragma omp / OpenMP (expected: FAIL, no emscripten libomp)
#
# Prerequisite: docker/pyodide-pthread/build.sh has populated .work + dist.
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
        flags="-O2 -fPIC -sSIDE_MODULE=1 -pthread -sSHARED_MEMORY -Wno-experimental -I $INC"
        echo "--- compiling side modules ---"
        emcc smoke/pthreadext.c -o smoke/pthreadext.so $flags
        # ompext links the OpenMP ABI; with no emscripten libomp the __kmpc_*
        # symbols stay unresolved and surface at import time (the point of the test).
        emcc smoke/ompext.c -o smoke/ompext.so $flags -fopenmp
        echo "--- importing into pthread pyodide (node) ---"
        cd smoke && node test.mjs
    '
