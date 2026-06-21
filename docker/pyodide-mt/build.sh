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
docker run --rm \
    -v "$PWD:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EXTRA_CFLAGS="-pthread" \
    -e EXTRA_LDFLAGS="-pthread -g1 -sMAXIMUM_MEMORY=4GB" \
    "$image" \
    bash -lc 'export PATH="$HOME/.local/bin:$PATH" && make -j'"$jobs"' all-but-packages'

if [ -d dist ]; then
    cp -r dist/. "$out/"
    echo "fork-free pthread Pyodide staged in: $out"
else
    echo "ERROR: dist/ not produced." >&2
    exit 1
fi

[ "${KEEP:-0}" = "1" ] || echo "(set KEEP=1 to retain $work for incremental rebuilds)"
