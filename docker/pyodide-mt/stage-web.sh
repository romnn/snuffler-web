#!/usr/bin/env bash
#
# Assemble the threaded Pyodide 314 runtime + wheels into a clean, web-servable
# directory the app loads from py-mt/ (see web/bridge_worker.js, which sets
# PYODIDE_BASE to py-mt/). Trunk's post_build hook copies this dir to dist/py-mt/.
# The app is MT-only, so this runtime must be present for the app to boot — build
# it with docker/pyodide-mt/build-all.sh (a fresh checkout has no Pyodide until then).
#
# Sources (both produced by the docker/pyodide-mt build steps):
#   - core runtime (relinked MT + OpenMP + SIMD main) from build.sh's   .work/pyodide/dist/
#   - package wheels + pyodide-lock.json from packages.sh/build-pyrocko's .work/pyodide/dist-mt/
# Only artifacts the lock references are copied, minus the *-tests.tar test data
# (not imported at runtime) — the rest of build-recipes' install-dir clutter is
# left behind.
#
# Usage:  docker/pyodide-mt/stage-web.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$here/.work/pyodide"
runtime="$work/dist"      # build.sh output
pkgs="$work/dist-mt"      # build-recipes --install-dir output
out="$here/served"       # clean, gitignored staging dir (Trunk → dist/py-mt/)

[ -f "$runtime/pyodide.asm.wasm" ] || { echo "ERROR: $runtime/pyodide.asm.wasm missing — run build.sh first." >&2; exit 1; }
[ -f "$pkgs/pyodide-lock.json" ]   || { echo "ERROR: $pkgs/pyodide-lock.json missing — run packages.sh + build-pyrocko.sh first." >&2; exit 1; }

rm -rf "$out"; mkdir -p "$out"

# Core runtime: the ESM loader (used by the module worker), the asm glue + wasm,
# and the stdlib. pyodide.js (UMD) is intentionally omitted — the threaded path
# is a module worker and never importScripts() it.
for f in pyodide.mjs pyodide.asm.mjs pyodide.asm.wasm python_stdlib.zip; do
    cp "$runtime/$f" "$out/"
done
cp "$pkgs/pyodide-lock.json" "$out/"

# Every artifact the lock references (wheels + shared-lib zips like libopenblas),
# skipping the *-tests.tar test-data packages that are never imported at runtime.
python3 - "$pkgs" "$out" <<'PY'
import json, shutil, sys, pathlib
src, dst = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
lock = json.loads((dst / "pyodide-lock.json").read_text())
copied = skipped = 0
missing = []
for pkg in lock["packages"].values():
    fn = pkg["file_name"]
    if fn.endswith("-tests.tar"):
        skipped += 1
        continue
    s = src / fn
    if s.exists():
        shutil.copy2(s, dst / fn)
        copied += 1
    else:
        missing.append(fn)
print(f"  staged {copied} artifacts ({skipped} test tarball(s) skipped)")
# A lock-referenced (non-test) wheel that isn't on disk = a partial build; refuse
# it here rather than ship a served/ whose lock resolves to a 404 at loadPackage().
if missing:
    sys.exit(f"  ERROR: {len(missing)} lock-referenced artifact(s) absent from {src}: "
             f"{sorted(missing)} — refusing to stage a partial build.")
PY

echo "threaded runtime staged → $out"
du -sh "$out"

# Refuse to ship a partial/stale build: every package the bridge loads at boot must
# be in the staged lock, or loadPackage() fails at runtime. This also turns a stale
# or partial *cached* dist-mt into a loud failure here instead of a broken deploy.
# Invoked via `bash` so it works even if the file's exec bit is ever lost.
bash "$here/check-lock.sh" "$out/pyodide-lock.json"
