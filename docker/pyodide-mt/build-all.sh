#!/usr/bin/env bash
#
# One-shot build of the custom Pyodide 314 web artifacts, in dependency order:
#
#   1. build.sh            — the threaded + OpenMP + SIMD runtime (the expensive,
#                            multi-hour step: CPython 3.14 + Pyodide + libomp +
#                            relink). Skipped if dist/ already exists (FORCE=1 to
#                            rebuild from scratch).
#   2. build-packages.sh   — the full THREADED package set into dist-mt/.
#   3. build-pyrocko.sh    — the THREADED pyrocko wheel (+ into dist-mt's lock).
#   4. build-pyrocko-cdn.sh— the SINGLE-THREADED pyrocko wheel into dist-py/.
#                            Off by default (BUILD_CDN=1 to build it): the app is
#                            MT-only now. The script is kept for a future
#                            non-threaded build hosted on a separate path.
#   5. stage-web.sh        — assemble the served py-mt/ from dist + dist-mt.
#
# Output the app + CI consume: docker/pyodide-mt/served/ (py-mt, ~27 MB), assembled
# by step 5 from the runtime (dist/) + wheels (dist-mt/). CI caches those two
# pre-stage artifacts (NOT served/) keyed on the builder inputs, and re-runs only
# stage-web.sh on a hit — so the expensive steps 1-3 run only when the patch /
# pinned commits / builder scripts change.
#
# Usage:  docker/pyodide-mt/build-all.sh         # reuse an existing runtime build
#         FORCE=1 docker/pyodide-mt/build-all.sh # rebuild the runtime too
#         BUILD_CDN=1 docker/pyodide-mt/build-all.sh # also build the CDN wheel
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$here/.work/pyodide"

if [ "${FORCE:-0}" = "1" ] || [ ! -f "$work/dist/pyodide.asm.wasm" ]; then
    echo "== [1/5] build.sh (runtime — the expensive step) =="
    KEEP=1 "$here/build.sh"
else
    echo "== [1/5] runtime already built ($work/dist) — skipping (FORCE=1 to rebuild) =="
fi

echo "== [2/5] build-packages.sh (full threaded package set) =="
"$here/build-packages.sh"

echo "== [3/5] build-pyrocko.sh (threaded wheel) =="
"$here/build-pyrocko.sh"

if [ "${BUILD_CDN:-0}" = "1" ]; then
    echo "== [4/5] build-pyrocko-cdn.sh (single-threaded CDN wheel — BUILD_CDN=1) =="
    "$here/build-pyrocko-cdn.sh"
else
    echo "== [4/5] CDN single-threaded wheel — skipped (BUILD_CDN=1 for the future non-threaded build) =="
fi

echo "== [5/5] stage-web.sh (assemble served py-mt/) =="
"$here/stage-web.sh"

echo
echo "build-all complete:"
echo "  threaded runtime  -> $here/served/   (trunk stages to dist/py-mt/)"
# An `if` (not `[ ... ] && echo`): as the script's LAST command the && form returns
# the test's exit status, so with BUILD_CDN unset it would exit 1 and fail the build
# under `set -e` even though everything succeeded.
if [ "${BUILD_CDN:-0}" = "1" ]; then
    echo "  single-threaded   -> $(cd "$here/../.." && pwd)/dist-py/  (BUILD_CDN=1)"
fi
