#!/usr/bin/env bash
#
# Build the SINGLE-THREADED pyrocko wheel for the CDN (default) path — the
# fallback that loads the stock Pyodide 314.0.0 CDN runtime. Same ABI as the
# threaded build (cp314 / pyemscripten_2026_0_wasm32, emscripten 5.0.3) but with
# NO -pthread / -sSHARED_MEMORY / -msimd128 and PYROCKO_OPENMP=0, so the wheel
# loads into the unmodified single-threaded CDN runtime. pyrocko's #pragma omp +
# <omp.h> are all under #if defined(_OPENMP), so without -fopenmp they compile
# out cleanly; pyrocko uses no setjmp/longjmp, so the SJLJ ABI is irrelevant.
#
# Output: dist-py/pyrocko-*.whl + dist-py/wheels.json (consumed by
# web/bridge_worker.js on the non-threaded path; trunk copies dist-py → dist/py).
# This is the single-threaded counterpart of build-pyrocko.sh — same docker/
# pyodide-mt 314 env, so both wheels share one toolchain and differ ONLY in
# multithreading + SIMD.
#
# Prereq: docker/pyodide-mt/build.sh + packages.sh (numpy) already built (the
# in-tree build-recipes env + host numpy).
# Usage:  docker/pyodide-mt/build-pyrocko-cdn.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
build="$here/.work/pyodide"
image="pyodide/pyodide-env:20260211-chrome145-firefox146-py314"
src="$repo_root/sources/pyrocko"

[ -f "$build/dist/pyodide.asm.mjs" ] || { echo "ERROR: run build.sh first." >&2; exit 1; }
[ -d "$src/src" ] || { echo "ERROR: run 'task fetch:pyrocko' first." >&2; exit 1; }

# 1) Stage pyrocko src + libmseed + setup.py (mirrors build-pyrocko.sh).
b="$build/pyrocko-build"
rm -rf "$b"; mkdir -p "$b"
cp -r "$src/src" "$b/pyrocko"
git_sha="$(git -C "$src" rev-parse --short HEAD 2>/dev/null || echo web)"
printf "version = '2026.06.02'\ngit_sha = '%s'\ninstalled_date = ''\n" "$git_sha" > "$b/pyrocko/info.py"
cp "$src"/libmseed-2.19.6.tar.gz "$src"/libmseed-2.19.6-*.patch "$b/"
( cd "$b" && tar -xzf libmseed-2.19.6.tar.gz --exclude=doc --exclude=test --exclude=example \
    && for p in speedread selection-fallthru fix-blkt-395-bswap speedswap; do
         patch -s -p0 < "libmseed-2.19.6-$p.patch"; done )
cp "$repo_root"/pybuild/setup.py "$repo_root"/pybuild/pyproject.toml "$b/"

mkdir -p "$build/packages/pyrocko"
cp "$here/pyrocko/meta.yaml" "$build/packages/pyrocko/meta.yaml"

# 2) Build single-threaded: drop -pthread / -sSHARED_MEMORY / -msimd128 and set
#    PYROCKO_OPENMP=0. -fwasm-exceptions matches the tree's SUPPORT_LONGJMP=wasm
#    side-module default (harmless for pyrocko).
docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EMCC_CFLAGS="-fwasm-exceptions" \
    -e PYROCKO_OPENMP="0" \
    -e PYROCKO_EXTS="${PYROCKO_EXTS:-full}" \
    "$image" \
    bash -lc '
        export PATH="$HOME/.local/bin:$PATH"
        command -v pyodide >/dev/null 2>&1 || pip install -q -e ./pyodide-build
        pyodide build-recipes pyrocko \
            --cflags="-fwasm-exceptions" \
            --ldflags="-fwasm-exceptions"
    '

# 3) Collect into dist-py (the CDN path's wheel dir) + refresh wheels.json.
dist="$repo_root/dist-py"
mkdir -p "$dist"
wheel="$(ls "$build"/packages/pyrocko/dist/pyrocko-*.whl 2>/dev/null | head -1)"
[ -n "$wheel" ] || { echo "ERROR: no wheel produced." >&2; exit 1; }
rm -f "$dist"/*.whl
cp "$wheel" "$dist/"
( cd "$dist" && ls *.whl | python3 -c "import sys,json; json.dump([l.strip() for l in sys.stdin], open('wheels.json','w'))" )
echo "single-threaded CDN wheel → $dist/$(basename "$wheel")"
cat "$dist/wheels.json"; echo
