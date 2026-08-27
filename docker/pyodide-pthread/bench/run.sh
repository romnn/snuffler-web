#!/usr/bin/env bash
#
# Measure whether OpenMP actually speeds up a REAL pyrocko kernel in the pthread
# Pyodide. Extracts parstackf + parstack_config from the fetched pyrocko source
# (not vendored — pyrocko is GPL), compiles them with our benchmark harness as a
# thin -fopenmp side module (libomp resolves from the main module), and runs it
# at 1/2/4/8 threads under node.
#
# Prerequisite: docker/pyodide-pthread/build.sh (libomp linked into main) and a
# fetched sources/pyrocko checkout (`task fetch:pyrocko`).
# Usage: docker/pyodide-pthread/bench/run.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
build="$here/../.work/pyodide-pthread"
image="pyodide/pyodide-env:20250915-chrome140-firefox140-py313"
kernel_src="$repo/sources/pyrocko/src/ext/parstack_ext.c"

if [ ! -f "$build/dist/pyodide.mjs" ]; then
    echo "ERROR: run docker/pyodide-pthread/build.sh first." >&2; exit 1
fi
if [ ! -f "$kernel_src" ]; then
    echo "ERROR: $kernel_src not found — run 'task fetch:pyrocko'." >&2; exit 1
fi

mkdir -p "$build/bench"
# Extract just the pure-C kernel (helpers + parstack_config + parstackf), from
# the i32min definition up to the double-precision parstack definition. Prepend
# our own includes so it builds without Python.h / numpy.
{
    printf '#include <stdint.h>\n#include <stddef.h>\n#include <stdlib.h>\n#include <limits.h>\n#include <math.h>\n'
    awk '/^static int32_t i32min/{p=1} p&&/^int parstack\(/{exit} p{print}' "$kernel_src"
} > "$build/bench/kernel.c"
cp "$here/harness.c" "$here/bench.mjs" "$build/bench/"

docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e BENCH_CFG="${BENCH_CFG:-40,20000,1000,1,3}" \
    -e BENCH_THREADS="${BENCH_THREADS:-1,2,4,8}" \
    "$image" \
    bash -lc '
        source emsdk/emsdk/emsdk_env.sh 2>/dev/null
        INC="$(ls -d cpython/installs/python-*/include/python3.13 | head -1)"
        flags="-O3 -fPIC -sSIDE_MODULE=1 -pthread -sSHARED_MEMORY -fopenmp -Wno-experimental -I $INC"
        echo "--- compiling parbench.so (real pyrocko parstackf + harness) ---"
        emcc bench/kernel.c bench/harness.c -o bench/parbench.so $flags
        echo "--- running OpenMP speedup benchmark ---"
        cd bench && node bench.mjs
    '
