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
#   4. stage-web.sh        — assemble the served py-mt/ from dist + dist-mt.
#
# Output the app + CI consume: docker/pyodide-mt/served/ (py-mt, ~27 MB), assembled
# by step 4 from the runtime (dist/) + wheels (dist-mt/).
# CI caches those two pre-stage artifacts (not served/) keyed on the builder
# inputs, and re-runs only stage-web.sh on a hit.
# So the expensive steps 1-3 run only when the patch, the pinned commits or the
# builder scripts change.
#
# Usage:  docker/pyodide-mt/build-all.sh         # reuse an existing runtime build
#         FORCE=1 docker/pyodide-mt/build-all.sh # rebuild the runtime too
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$here/.work/pyodide"

if [ "${FORCE:-0}" = "1" ] || [ ! -f "$work/dist/pyodide.asm.wasm" ]; then
    echo "== [1/4] build.sh (runtime — the expensive step) =="
    KEEP=1 "$here/build.sh"
else
    echo "== [1/4] runtime already built ($work/dist) — skipping (FORCE=1 to rebuild) =="
fi

echo "== [2/4] build-packages.sh (full threaded package set) =="
"$here/build-packages.sh"

echo "== [3/4] build-pyrocko.sh (threaded wheel) =="
"$here/build-pyrocko.sh"

echo "== [4/4] stage-web.sh (assemble served py-mt/) =="
"$here/stage-web.sh"

echo
echo "build-all complete:"
echo "  threaded runtime  -> $here/served/   (trunk stages to dist/py-mt/)"
