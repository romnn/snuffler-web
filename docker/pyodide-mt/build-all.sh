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
#   4. build-pyrocko-cdn.sh— the SINGLE-THREADED pyrocko wheel into dist-py/ (the
#                            CDN/fallback path; differs only in MT+SIMD).
#   5. stage-web.sh        — assemble the served py-mt/ from dist + dist-mt.
#
# Outputs the two things the app + CI consume: docker/pyodide-mt/served/ (py-mt,
# ~54 MB) and dist-py/ (the single-threaded wheel). CI caches THOSE outputs
# keyed on this dir's build inputs, so the multi-hour step 1 runs only when the
# patch / pinned commit / build scripts change.
#
# Usage:  docker/pyodide-mt/build-all.sh        # reuse an existing runtime build
#         FORCE=1 docker/pyodide-mt/build-all.sh # rebuild the runtime too
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

echo "== [4/5] build-pyrocko-cdn.sh (single-threaded CDN wheel) =="
"$here/build-pyrocko-cdn.sh"

echo "== [5/5] stage-web.sh (assemble served py-mt/) =="
"$here/stage-web.sh"

echo
echo "build-all complete:"
echo "  threaded runtime  -> $here/served/   (trunk stages to dist/py-mt/)"
echo "  single-threaded   -> $(cd "$here/../.." && pwd)/dist-py/  (trunk copies to dist/py/)"
