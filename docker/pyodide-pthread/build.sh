#!/usr/bin/env bash
#
# Reproducible build of a pthread-enabled Pyodide, EXPERIMENTAL and fully
# SEPARATE from the working app. It clones the yosefk/pyodide-pthread fork
# (the only known patch set that makes Emscripten pthreads work under Pyodide)
# and builds it inside the official pyodide-env Docker image, leaving the
# runtime artifacts in ./out/ — which the app can then serve locally instead of
# the CDN (opt-in; see ../../web/bridge_worker.js and the ?threads toggle).
#
# Nothing here touches the default single-threaded build. See README.md for the
# (sobering) feasibility verdict before spending hours on this.
#
# Usage:  docker/pyodide-pthread/build.sh        # full build into ./out
#         KEEP=1 docker/pyodide-pthread/build.sh # keep the .work clone for reuse
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$here/.work"
out="$here/out"
image="pyodide/pyodide-env:20250915-chrome140-firefox140-py313"
fork="https://github.com/yosefk/pyodide-pthread.git"
ref="mt"
# Parallelism. The host has 24 physical cores but the clang/CPython compiles are
# memory-hungry (~60 GB RAM), so default to the physical-core count rather than
# nproc (48 w/ HT) to avoid OOM. Override with JOBS=N.
jobs="${JOBS:-24}"

mkdir -p "$work" "$out"

# 1) Source: the pthread fork + its submodules (CPython, emsdk, packages — large).
if [ ! -d "$work/pyodide-pthread/.git" ]; then
    git clone --branch "$ref" --depth 1 "$fork" "$work/pyodide-pthread"
fi
cd "$work/pyodide-pthread"
git submodule update --init --recursive --depth 1

# 2) Build the CORE runtime (all-but-packages) inside the pinned pyodide-env
#    image with the pthread flags the fork documents. -g1 keeps pyodide.asm.js
#    readable for tools/fixup-threading-support.py (the Makefile runs it). We
#    build `all-but-packages`, NOT the default `all` (which compiles the entire
#    package set for hours); numpy/scipy with -sSHARED_MEMORY=1 are a separate
#    phase (gated on the threaded-C-ext smoke test — see README). pip puts the
#    pyodide CLI under $HOME/.local/bin, so add that to PATH.
docker run --rm \
    -v "$PWD:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EXTRA_CFLAGS="-pthread" \
    -e EXTRA_LDFLAGS="-pthread -g1 -sMAXIMUM_MEMORY=4GB" \
    "$image" \
    bash -lc 'export PATH="$HOME/.local/bin:$PATH" && make -j'"$jobs"' all-but-packages'

# 3) Copy the runtime out of the build tree into ./out (the bind mount already
#    surfaces dist/ on the host; this just isolates the bits the app serves).
if [ -d dist ]; then
    cp -rv dist/. "$out/"
    echo "pthread Pyodide runtime staged in: $out"
else
    echo "ERROR: dist/ not produced — the build did not complete." >&2
    exit 1
fi

[ "${KEEP:-0}" = "1" ] || echo "(set KEEP=1 to retain the $work clone for incremental rebuilds)"
