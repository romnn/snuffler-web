#!/usr/bin/env bash
#
# Build libomp.a — the LLVM OpenMP runtime — for wasm32-emscripten, so that
# `-fopenmp` C extensions can link a real OpenMP runtime in the pthread Pyodide.
#
# emscripten ships NO OpenMP (PR emscripten-core/emscripten#25937, which vendors
# libomp, is unmerged as of em 6.0). LLVM itself already has wasm32 OpenMP
# CODEGEN (D142593), so `#pragma omp` compiles to the libomp ABI; only the
# runtime library is missing. We compile it from the wasm-patched openmp source
# on the PR's branch (pinned by SHA) plus a tiny compat shim (kmp_wasm_compat.c)
# for the one GOMP symbol the native asm provides but wasm doesn't.
#
# Output: <build tree>/libomp.a  (defines __kmpc_*, omp_*, __kmp_invoke_microtask)
# Usage:  docker/pyodide-pthread/libomp/build.sh
#         PYODIDE_BUILD_TREE=/path/to/tree docker/pyodide-pthread/libomp/build.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="${PYODIDE_BUILD_TREE:-$here/../.work/pyodide-pthread}"
image="pyodide/pyodide-env:20250915-chrome140-firefox140-py313"
# JAicewizard/emscripten@libomp — the source behind emscripten PR #25937.
repo="https://github.com/JAicewizard/emscripten.git"
sha="67a610023bafdb84fa08c6b6e658036fb7dde43a"

if [ ! -d "$build/emsdk" ]; then
    echo "ERROR: $build has no emsdk — run docker/pyodide-pthread/build.sh first." >&2
    exit 1
fi

src="$build/libomp-src"
if [ ! -e "$src/system/lib/libomp/src/kmp_runtime.cpp" ]; then
    rm -rf "$src"
    mkdir -p "$src"
    git -C "$src" init -q
    git -C "$src" remote add origin "$repo"
    git -C "$src" config core.sparseCheckout true
    echo "system/lib/libomp/*" > "$src/.git/info/sparse-checkout"
    git -C "$src" fetch -q --depth 1 origin "$sha"
    git -C "$src" checkout -q FETCH_HEAD
fi
cp "$here/kmp_wasm_compat.c" "$src/system/lib/libomp/src/"

# Compile flags mirror emscripten PR #25937's libomp Library (minus the per-warning
# flags → -w). -pthread implies atomics/bulk-memory + shared memory; -fPIC so the
# objects relocate into a MAIN_MODULE/SIDE_MODULE. z_Linux_asm.S is empty on wasm
# (all blocks are KMP_ARCH_<native>-guarded) but is kept for parity with the recipe.
docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    "$image" \
    bash -lc '
        source emsdk/emsdk/emsdk_env.sh 2>/dev/null
        cd libomp-src/system/lib/libomp/src
        cflags="-O3 -DNDEBUG -pthread -sSHARED_MEMORY -fPIC -w \
            -D_GNU_SOURCE -U_GLIBCXX_ASSERTIONS -D_GLIBCXX_NO_ASSERTIONS \
            -fno-exceptions -fno-rtti -I. -Ii18n -Ithirdparty/ittnotify"
        srcs="kmp_alloc kmp_atomic kmp_csupport kmp_debug kmp_itt kmp_environment \
            kmp_error kmp_global kmp_i18n kmp_io kmp_runtime kmp_settings kmp_str \
            kmp_tasking kmp_threadprivate kmp_utility kmp_barrier kmp_wait_release \
            kmp_affinity kmp_dispatch kmp_lock kmp_sched kmp_collapse z_Linux_util \
            kmp_gsupport kmp_taskdeps kmp_cancel kmp_ftn_cdecl kmp_ftn_extra kmp_version"
        rm -f ./*.o
        for s in $srcs; do emcc -c $cflags "$s.cpp" -o "$s.o"; done
        emcc -c $cflags z_Linux_asm.S -o z_Linux_asm.o
        emcc -c $cflags kmp_wasm_compat.c -o kmp_wasm_compat.o
        emar rcs /src/libomp.a ./*.o
    '

echo "libomp.a → $build/libomp.a ($(du -h "$build/libomp.a" | cut -f1))"
