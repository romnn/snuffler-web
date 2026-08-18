#!/usr/bin/env bash
#
# Download the deployed py-mt runtime into served/, instead of building it.
#
# The Pages deploy publishes exactly what stage-web.sh assembles — Trunk copies
# served/ to dist/py-mt/ — so the live site is a ready-made runtime: the same
# relinked core, the same wheels, already precompiled to bytecode. A fresh
# checkout becomes servable in a minute rather than the hour build-all.sh takes,
# which is the only way to get a runtime without Docker and an emsdk toolchain.
#
# What you get is whatever main last deployed. Build with build-all.sh instead
# when the runtime itself is what you are changing: a patch, a package pin, or
# pybuild/setup.py.
#
# Usage:  docker/pyodide-mt/fetch-web.sh
#         PY_MT_URL=http://127.0.0.1:8080/py-mt docker/pyodide-mt/fetch-web.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/served"
# Inside .work so a failed fetch leaves nothing git has to be told to ignore.
staging="$here/.work/served-fetch"
base="${PY_MT_URL:-https://romnn.github.io/snuffler-web/py-mt}"
base="${base%/}"

rm -rf "$staging"
mkdir -p "$staging"
trap 'rm -rf "$staging"' EXIT

echo "fetching $base"
curl -fsS -o "$staging/pyodide-lock.json" "$base/pyodide-lock.json"

# The four runtime files stage-web.sh copies, plus every artifact the lock
# references — the same set it stages, read from the other side of the wire.
files=$(
    python3 - "$staging/pyodide-lock.json" <<'PY'
import json, sys

lock = json.load(open(sys.argv[1]))
for name in ("pyodide.mjs", "pyodide.asm.mjs", "pyodide.asm.wasm", "python_stdlib.zip"):
    print(name)
for pkg in lock["packages"].values():
    if not pkg["file_name"].endswith("-tests.tar"):
        print(pkg["file_name"])
PY
)

urls=""
for f in $files; do
    urls="$urls $base/$f"
done
# Unquoted on purpose: the list is one URL per word, and no artifact name has
# ever carried a space.
# shellcheck disable=SC2086
(cd "$staging" && curl -fsS --parallel --parallel-max 8 --remote-name-all $urls)
echo "  fetched $(echo "$files" | wc -l | tr -d ' ') artifacts"

# The guard the build path runs, against the same question in a different shape:
# a deploy caught mid-update, or a URL that is not a py-mt directory at all,
# fails here instead of at loadPackage() in the browser.
bash "$here/check-lock.sh" "$staging/pyodide-lock.json"

rm -rf "$out"
mv "$staging" "$out"
trap - EXIT
echo "deployed runtime staged → $out"
du -sh "$out"
