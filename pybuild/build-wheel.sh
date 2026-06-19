#!/usr/bin/env bash
# `mise run build-wheel` — cross-compile pyrocko (subset) to a Pyodide wheel.
#
# Stages an UNMODIFIED copy of the pyrocko source + extracted/patched libmseed
# into pybuild/build/, then runs `pyodide build` (emscripten) using setup.py,
# emitting dist-py/pyrocko-*.whl + dist-py/wheels.json.
#
# Requires: emcc (mise emsdk) on PATH and the .venv-pyodide created by
# `mise run setup`. Run via `mise run build-wheel` so emcc's env is active.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
PYROCKO="$ROOT/sources/pyrocko"
BUILD="$ROOT/pybuild/build"
VENV="$ROOT/.venv-pyodide"
DIST="$ROOT/dist-py"

command -v emcc >/dev/null || { echo "emcc not on PATH — run via 'mise run build-wheel'"; exit 1; }
[ -x "$VENV/bin/pyodide" ] || { echo "missing $VENV — run 'mise run setup' first"; exit 1; }
[ -d "$PYROCKO/src" ] || { echo "missing $PYROCKO — run 'mise run fetch-pyrocko' first"; exit 1; }

echo "== staging pyrocko source (unmodified copy) =="
rm -rf "$BUILD"
mkdir -p "$BUILD" "$DIST"
cp -r "$PYROCKO/src" "$BUILD/pyrocko"

# Stamp the exact source commit we built from (pyrocko is fetched, not vendored).
GIT_SHA="$(git -C "$PYROCKO" rev-parse --short HEAD 2>/dev/null || echo web)"
cat > "$BUILD/pyrocko/info.py" <<EOF
version = '2026.06.02'
git_sha = '$GIT_SHA'
installed_date = ''
EOF

echo "== extracting + patching libmseed (replicates prerequisites.sh) =="
cp "$PYROCKO"/libmseed-2.19.6.tar.gz "$BUILD/"
cp "$PYROCKO"/libmseed-2.19.6-*.patch "$BUILD/"
(
  cd "$BUILD"
  tar -xzf libmseed-2.19.6.tar.gz --exclude=doc --exclude=test --exclude=example
  for p in speedread selection-fallthru fix-blkt-395-bswap speedswap; do
    patch -s -p0 < "libmseed-2.19.6-$p.patch"
  done
)

cp "$ROOT/pybuild/setup.py" "$ROOT/pybuild/pyproject.toml" "$BUILD/"

echo "== pyodide build (emscripten cross-compile) =="
(
  cd "$BUILD"
  "$VENV/bin/pyodide" build
)

echo "== retag to the Pyodide 0.27.2 platform tag (pyodide_2024_0_wasm32) =="
# pyodide-build 0.35 stamps the newer 'pyemscripten_*' tag; the 0.27.2 runtime
# expects 'pyodide_2024_0_wasm32' (same ABI: emscripten 3.1.58 / abi 2024_0).
( cd "$BUILD/dist" && "$VENV/bin/python" -m wheel tags \
    --platform-tag pyodide_2024_0_wasm32 --remove ./*.whl )

echo "== collecting wheel =="
rm -f "$DIST"/*.whl
WHEEL="$(ls "$BUILD"/dist/*pyodide_2024_0_wasm32.whl | head -1)"
cp "$WHEEL" "$DIST/"
( cd "$DIST" && ls *.whl | python3 -c "import sys,json; json.dump([l.strip() for l in sys.stdin], open('wheels.json','w'))" )
echo "OK -> $DIST/$(basename "$WHEEL")"
cat "$DIST/wheels.json"; echo
