#!/usr/bin/env bash
#
# Fork-free pthread (+ later OpenMP) Pyodide build. Unlike docker/pyodide-pthread
# (which clones the yosefk fork), this clones OFFICIAL pyodide at a pinned commit
# and applies ONLY our own small patch — patches/0001-pthread.patch — built in
# the OFFICIAL pyodide-env image. We maintain the patch ourselves; no fork is a
# dependency. The OpenMP runtime (libomp) is a separate step (see ../pyodide-pthread/libomp).
#
# Usage:  docker/pyodide-mt/build.sh
#         KEEP=1 docker/pyodide-mt/build.sh   # keep .work for incremental rebuilds
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$here/.work"
out="$here/out"
# Official Pyodide (py3.14.2 / emscripten 5.0.3) + its official build image.
image="pyodide/pyodide-env:20260211-chrome145-firefox146-py314"
repo="https://github.com/pyodide/pyodide.git"
ref="807f16b054b2b0b4d5da3c8d465b502db92fdcaf"
patch="$here/patches/0001-pthread.patch"
jobs="${JOBS:-24}"
# wasm SIMD (-msimd128) ON by default: autovectorizes our OpenMP kernels (which
# carry #pragma omp simd) and numpy's scalar loops — free-or-faster, never
# slower. Raises the browser floor to SIMD-capable engines (~2023+). SIMD=0 off.
simd=" -msimd128"; [ "${SIMD:-1}" = "0" ] && simd=""
# PIC system-lib archives (inside the container). The all-but-packages main only
# pulls *referenced* objects from these, so it lacks the float-math / locale /
# libc++ symbols that packages (numpy, scipy, pyrocko) need. We whole-archive the
# PIC (relocatable) variants into the main module so it is package-capable; the
# non-debug variants avoid pulling debug-only objects (e.g. emscripten_dbg).
pic='/src/emsdk/emsdk/upstream/emscripten/cache/sysroot/lib/wasm32-emscripten/pic'
syslibs="$pic/libc-mt.a $pic/libc++-mt-legacyexcept.a $pic/libc++abi-mt-legacyexcept.a"

mkdir -p "$work" "$out"

# 1) Official source at the pinned commit + submodules (CPython, emsdk, …).
if [ ! -d "$work/pyodide/.git" ]; then
    git clone "$repo" "$work/pyodide"
fi
cd "$work/pyodide"
git fetch -q origin "$ref" 2>/dev/null || true
git checkout -q "$ref"
git submodule update --init --recursive --depth 1

# 2) Apply OUR pthread patch (idempotent: skip if already applied).
if git apply --reverse --check "$patch" 2>/dev/null; then
    echo "pthread patch already applied"
else
    git checkout -q -- Makefile src/core/pre.js src/core/stack_switching/stack_switching.mjs
    rm -f tools/fixup-threading-support.py
    git apply "$patch"
    echo "applied $patch"
fi

# 3) Build the CORE runtime (all-but-packages) with the pthread flags. -g1 keeps
#    pyodide.asm.mjs readable for tools/fixup-threading-support.py (the patch runs
#    it). Packages (numpy/scipy/pyrocko) are a later, separate phase.
#
#    EMCC_CFLAGS=-pthread is REQUIRED in addition to EXTRA_CFLAGS: 314 statically
#    links the stdlib C libs sqlite3/lzma/zstd/ffi (and libhiwire) into the main
#    module, but their cpython sub-build hard-codes/overwrites CFLAGS (e.g.
#    `CFLAGS="-fPIC -Os"`, and libffi's build.sh `export CFLAGS=`), so EXTRA_CFLAGS
#    never reaches them and their objects lack the wasm 'atomics' feature. emcc.py
#    appends EMCC_CFLAGS to EVERY compile regardless, so -pthread there gives those
#    objects atomics+bulk-memory — required for a --shared-memory (pthread) main
#    link. (libpython already gets -pthread via CFLAGS_BASE.)
#
#    -sERROR_ON_UNDEFINED_SYMBOLS=0: 314 dropped this from the dylink main link
#    (it's only in the DISABLE_DYLINK variant now). With -pthread, the $JSEvents
#    JS library references emscripten_run_callback_on_thread (event proxying to
#    threads) which isn't pulled in; without =0 the jsifier hard-errors. 0.29 set
#    =0 here and threads (compute-only, no event proxying) worked fine — the
#    yosefk design stubs such unused threading symbols. So we restore =0.
docker run --rm \
    -v "$PWD:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EXTRA_CFLAGS="-pthread$simd" \
    -e EXTRA_LDFLAGS="-pthread -g1 -sMAXIMUM_MEMORY=4GB -sERROR_ON_UNDEFINED_SYMBOLS=0" \
    -e EMCC_CFLAGS="-pthread$simd" \
    "$image" \
    bash -lc 'export PATH="$HOME/.local/bin:$PATH" && make -j'"$jobs"' all-but-packages'

# 4) OpenMP: build libomp.a (emscripten ships none) for this toolchain, then
#    relink the MAIN module with it whole-archived in so every -fopenmp C-ext
#    (pyrocko) shares one OpenMP runtime — MAIN_MODULE=1/EXPORT_ALL re-export the
#    __kmpc_*/omp_* symbols for side modules. NO -sPTHREAD_POOL_SIZE (a pre-spawned
#    pool can't see side modules dlopen'd later). Reuses the libomp builder with the
#    em5 image; the LLVM openmp source compiles unchanged with em5's clang.
PYODIDE_BUILD_TREE="$PWD" IMAGE="$image" "$here/../pyodide-pthread/libomp/build.sh"
docker run --rm \
    -v "$PWD:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EXTRA_CFLAGS="-pthread$simd" \
    -e EXTRA_LDFLAGS="-pthread -g1 -sMAXIMUM_MEMORY=4GB -sERROR_ON_UNDEFINED_SYMBOLS=0 -Wl,--whole-archive /src/libomp.a $syslibs -Wl,--no-whole-archive" \
    -e EMCC_CFLAGS="-pthread$simd" \
    "$image" \
    bash -lc 'export PATH="$HOME/.local/bin:$PATH" && rm -f dist/pyodide.asm.mjs dist/pyodide.asm.wasm && make -j'"$jobs"' all-but-packages'

if [ -d dist ]; then
    cp -r dist/. "$out/"
    echo "fork-free pthread+OpenMP Pyodide staged in: $out"
else
    echo "ERROR: dist/ not produced." >&2
    exit 1
fi

[ "${KEEP:-0}" = "1" ] || echo "(set KEEP=1 to retain $work for incremental rebuilds)"
