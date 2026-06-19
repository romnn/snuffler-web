#!/usr/bin/env bash
# `mise run setup` — create the repo-local Python venv that holds the
# pyodide-build CLI used to cross-compile pyrocko's C extensions.
#
# Toolchain versions live in mise.toml ([tools] + [env]); this script only
# provisions the venv. Python (3.12) and emscripten (emsdk) come from mise.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/bin:$HOME/.local/share/mise/shims:$PATH"

PYODIDE_VERSION="${PYODIDE_VERSION:-0.27.2}"
PYODIDE_BUILD_VERSION="${PYODIDE_BUILD_VERSION:-0.35.0}"
VENV=".venv-pyodide"

PY="$(mise which python)"
echo "== [1/3] uv venv ($VENV) with mise Python: $("$PY" --version) =="
uv venv --python "$PY" "$VENV"

echo "== [2/3] install pyodide-build==$PYODIDE_BUILD_VERSION =="
# --prerelease=allow: pyodide-build pins some 0.x.y pre-release deps exactly.
VIRTUAL_ENV="$VENV" uv pip install --prerelease=allow \
  "pyodide-build==$PYODIDE_BUILD_VERSION"

echo "== [3/3] install cross-build env for Pyodide $PYODIDE_VERSION =="
"$VENV/bin/pyodide" xbuildenv install "$PYODIDE_VERSION"

EMVER="$("$VENV/bin/pyodide" config get emscripten_version)"
echo
echo "OK. pyodide-build $PYODIDE_BUILD_VERSION, xbuildenv $PYODIDE_VERSION, needs emscripten $EMVER"
echo "   (mise.toml pins emsdk to this version; run 'mise install' if emcc is missing)"
