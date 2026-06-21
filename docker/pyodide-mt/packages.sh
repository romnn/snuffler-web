#!/usr/bin/env bash
#
# Build Python packages (numpy, …) for the fork-free pthread+OpenMP+SIMD Pyodide
# 314 runtime. Run AFTER build.sh (which produces the package-capable main module
# with the PIC libc/libc++ whole-archived in). Wheels are built with -pthread
# -sSHARED_MEMORY (so they dynamically link into the threaded main) and -msimd128
# (SIMD; SIMD=0 to disable), via Pyodide's own build-recipes pipeline.
#
# Usage:  docker/pyodide-mt/packages.sh [pkg ...]      # default: numpy
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="$here/.work/pyodide"
image="pyodide/pyodide-env:20260211-chrome145-firefox146-py314"
pkgs="${*:-numpy}"
simd=" -msimd128"; [ "${SIMD:-1}" = "0" ] && simd=""

if [ ! -f "$build/dist/pyodide.asm.mjs" ]; then
    echo "ERROR: run docker/pyodide-mt/build.sh first." >&2
    exit 1
fi

docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EMCC_CFLAGS="-pthread$simd" \
    "$image" \
    bash -lc '
        export PATH="$HOME/.local/bin:$PATH"
        # build-recipes lives in the in-tree pyodide-build (not installed by the
        # all-but-packages core build); install it once.
        command -v pyodide >/dev/null 2>&1 || pip install -q -e ./pyodide-build
        pyodide build-recipes '"$pkgs"' \
            --cflags="-pthread -sSHARED_MEMORY'"$simd"'" \
            --ldflags="-pthread -sSHARED_MEMORY"
    '

echo "built: $pkgs → $build/packages/*/dist/*.whl"
