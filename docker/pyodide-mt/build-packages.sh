#!/usr/bin/env bash
#
# Build the full THREADED package set for the custom Pyodide 314 runtime and
# install it into dist-mt/ (wheels + pyodide-lock.json) — the set that
# stage-web.sh assembles into the served py-mt/. Everything is built with
# -pthread -sSHARED_MEMORY -msimd128 -fwasm-exceptions so the side modules link
# into the threaded main (the SAME flags as build-pyrocko.sh; see that file +
# build.sh for why each is needed).
#
# Non-core packages (scipy/matplotlib/protobuf/requests/... and their deps) are
# NOT in the pyodide source tree; their recipes live in the separate
# pyodide-recipes repo, which we clone at a pinned commit and drop into the
# tree's packages/ so the in-tree `build-recipes` can build them. build-recipes
# resolves transitive deps, so only the top-level set is listed.
#
# Prereq: docker/pyodide-mt/build.sh (runtime + in-tree cross-build env).
# Usage:  docker/pyodide-mt/build-packages.sh [pkg ...]
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="$here/.work/pyodide"
image="pyodide/pyodide-env:20260211-chrome145-firefox146-py314"
simd=" -msimd128"; [ "${SIMD:-1}" = "0" ] && simd=""
# pyodide-recipes pinned to the commit whose recipes match this 314 tree
# (release 314-20260521; see Makefile.envs PYODIDE_PREBUILT_PACKAGES_BASE).
recipes_repo="https://github.com/pyodide/pyodide-recipes.git"
recipes_ref="003aab5308245aaf5eacd992d79fa339a9c13013"
# Top-level set the bridge loads; build-recipes pulls in the transitive deps.
pkgs="${*:-numpy scipy matplotlib pyyaml micropip protobuf requests pyodide-http}"

[ -f "$build/dist/pyodide.asm.mjs" ] || { echo "ERROR: run build.sh first." >&2; exit 1; }

# 1) Fetch the recipes + drop them into the tree's packages/ (don't clobber the
#    in-tree core recipes or our pyrocko recipe — cp -n).
rec="$build/.pyodide-recipes"
if [ ! -d "$rec/.git" ]; then
    git clone --filter=blob:none "$recipes_repo" "$rec"
fi
git -C "$rec" fetch -q origin "$recipes_ref" 2>/dev/null || true
git -C "$rec" checkout -q "$recipes_ref"
mkdir -p "$build/packages"
cp -rn "$rec"/packages/* "$build/packages/" 2>/dev/null || true

# 2) Build + install the set into dist-mt (wheels + pyodide-lock.json). -fPIC is
#    forced because Pillow's setup drops it (else 'R_WASM_MEMORY_ADDR ... requires
#    -fPIC'); matplotlib imports PIL at import time so Pillow is required.
docker run --rm \
    -v "$build:/src" -w /src \
    --shm-size 2g \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/.docker_home \
    -e EMCC_CFLAGS="-fPIC -pthread$simd -fwasm-exceptions" \
    "$image" \
    bash -lc '
        export PATH="$HOME/.local/bin:$PATH"
        command -v pyodide >/dev/null 2>&1 || pip install -q -e ./pyodide-build
        pyodide build-recipes '"$pkgs"' \
            --install --install-dir dist-mt \
            --cflags="-pthread -sSHARED_MEMORY'"$simd"' -fwasm-exceptions" \
            --ldflags="-pthread -sSHARED_MEMORY -fwasm-exceptions"
    '

n="$(python3 -c "import json;print(len(json.load(open('$build/dist-mt/pyodide-lock.json'))['packages']))" 2>/dev/null || echo '?')"
echo "threaded package set installed -> $build/dist-mt ($n packages in lock)"
