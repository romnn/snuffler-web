#!/usr/bin/env bash
#
# Build the pyrocko wheel for the fork-free pthread+OpenMP+SIMD Pyodide 314
# runtime. Stages the (fetched, unmodified) pyrocko source + extracted/patched
# libmseed + our minimal pybuild/setup.py, then builds it via Pyodide's in-tree
# build-recipes pipeline with PYROCKO_OPENMP=1 + -pthread -sSHARED_MEMORY
# -msimd128. The C extensions' #pragma omp lowers to libomp (whole-archived into
# main by build.sh); the parstack inner loops also carry #pragma omp simd.
#
# Prerequisites: build.sh (main + libomp + libomp-src for omp.h), numpy built
# (packages.sh), and a fetched sources/pyrocko (mise run fetch-pyrocko).
# Usage:  docker/pyodide-mt/build-pyrocko.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
build="$here/.work/pyodide"
image="pyodide/pyodide-env:20260211-chrome145-firefox146-py314"
src="$repo_root/sources/pyrocko"
simd=" -msimd128"; [ "${SIMD:-1}" = "0" ] && simd=""

[ -f "$build/dist/pyodide.asm.mjs" ] || { echo "ERROR: run build.sh first." >&2; exit 1; }
[ -d "$src/src" ] || { echo "ERROR: run 'mise run fetch-pyrocko' first." >&2; exit 1; }
# emscripten ships no omp.h; pyrocko's exts #include <omp.h>, so point at the
# copy in the libomp source fetched by build.sh/libomp.
ompinc_host="$build/libomp-src/system/lib/libomp/include"
[ -f "$ompinc_host/omp.h" ] || { echo "ERROR: libomp source missing — run build.sh (libomp step) first." >&2; exit 1; }
ompinc="/src/libomp-src/system/lib/libomp/include"

# 1) Stage pyrocko src + libmseed + setup.py (same layout build-pyrocko-cdn.sh uses).
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

# 2) Drop our recipe in place (build-recipes resolves source.path relative to it).
mkdir -p "$build/packages/pyrocko"
cp "$here/pyrocko/meta.yaml" "$build/packages/pyrocko/meta.yaml"

# 3) Build via the in-tree pipeline (pyodide build needs an xbuildenv; build-recipes
#    uses the in-tree cross-build env directly, like numpy).
docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EMCC_CFLAGS="-pthread$simd -fwasm-exceptions -I$ompinc" \
    -e PYROCKO_OPENMP="1" \
    -e PYROCKO_EXTS="${PYROCKO_EXTS:-full}" \
    "$image" \
    bash -lc '
        export PATH="$HOME/.local/bin:$PATH"
        command -v pyodide >/dev/null 2>&1 || pip install -q -e ./pyodide-build
        # -fwasm-exceptions -> wasm-SJLJ, matching the main (the wasm-EH main
        # cannot link emulated-SJLJ side modules). Install into a SEPARATE dir:
        # build-recipes REWRITES <install-dir>/pyodide-lock.json with only the
        # packages it processed (pyrocko + its one declared dep numpy + core), so
        # installing into dist-mt would CLOBBER build-packages.sh full lock down to
        # ~5 entries. We merge pyrocko into dist-mt afterwards (below) instead.
        pyodide build-recipes pyrocko \
            --install --install-dir dist-pyrocko \
            --cflags="-pthread -sSHARED_MEMORY'"$simd"' -fwasm-exceptions -I'"$ompinc"'" \
            --ldflags="-pthread -sSHARED_MEMORY -fwasm-exceptions"
    '

# Merge pyrocko into dist-mt WITHOUT clobbering build-packages.sh's full package set:
# copy pyrocko's wheel in (cp -n keeps the existing builds of shared deps), then add
# pyrocko's lock entry to dist-mt's lock. dist-mt thus = full set + pyrocko, which is
# what stage-web.sh reads to assemble served py-mt/.
[ -f "$build/dist-mt/pyodide-lock.json" ] || { echo "ERROR: $build/dist-mt/pyodide-lock.json missing — run build-packages.sh first." >&2; exit 1; }
[ -f "$build/dist-pyrocko/pyodide-lock.json" ] || { echo "ERROR: pyrocko install ($build/dist-pyrocko) not produced." >&2; exit 1; }
# Force-copy pyrocko's freshly built wheel: a local KEEP=1 re-run may already hold an
# older pyrocko wheel whose bytes no longer match the new sha256 the merge records
# below (the wheels aren't reproducible), which would trip loadPackage integrity.
# Then add any other dist-pyrocko wheel only if absent, so shared-dep wheels (whose
# sha256 dist-mt's lock already records) are never overwritten with a differing build.
cp -f "$build/dist-pyrocko/"pyrocko-*.whl "$build/dist-mt/"
cp -n "$build/dist-pyrocko/"*.whl "$build/dist-mt/"
python3 - "$build/dist-mt/pyodide-lock.json" "$build/dist-pyrocko/pyodide-lock.json" <<'PY'
import json, sys
mt_path, sub_path = sys.argv[1], sys.argv[2]
mt = json.load(open(mt_path))
sub = json.load(open(sub_path))
added = []
for name, pkg in sub["packages"].items():
    if name not in mt["packages"]:
        mt["packages"][name] = pkg
        added.append(name)
mt["packages"]["pyrocko"] = sub["packages"]["pyrocko"]  # the freshly built wheel
json.dump(mt, open(mt_path, "w"), indent=2)
print(f"merged pyrocko -> dist-mt lock now has {len(mt['packages'])} packages"
      + (f"; newly added: {sorted(added)}" if added else ""))
PY

echo "pyrocko wheel → $build/packages/pyrocko/dist/"
ls "$build/packages/pyrocko/dist/"*.whl 2>/dev/null | sed 's#.*/##'
