#!/usr/bin/env bash
#
# Assemble the threaded Pyodide 314 runtime + wheels into a clean, web-servable
# directory the app loads from py-mt/ (see PYODIDE_BASE in web/bridge_worker.ts).
# Trunk's post_build hook copies this dir to dist/py-mt/.
# The app is MT-only, so this runtime must be present for the app to boot.
# Build it with docker/pyodide-mt/build-all.sh (a fresh checkout has no Pyodide
# until then), or download the deployed one with fetch-web.sh, which produces this
# same directory without the build.
#
# Sources (both produced by the docker/pyodide-mt build steps):
#   - core runtime (relinked MT + OpenMP + SIMD main) from build.sh's   .work/pyodide/dist/
#   - package wheels + pyodide-lock.json from build-packages.sh/build-pyrocko.sh's .work/pyodide/dist-mt/
# Only artifacts the lock references are copied, minus the *-tests.tar test data
# (not imported at runtime).
# The rest of build-recipes' install-dir clutter is left behind.
#
# Usage:  docker/pyodide-mt/stage-web.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$here/.work/pyodide"
runtime="$work/dist"      # build.sh output
pkgs="$work/dist-mt"      # build-recipes --install-dir output
out="$here/served"       # clean, gitignored staging dir (Trunk → dist/py-mt/)

[ -f "$runtime/pyodide.asm.wasm" ] || { echo "ERROR: $runtime/pyodide.asm.wasm missing — run build.sh first." >&2; exit 1; }
[ -f "$pkgs/pyodide-lock.json" ]   || { echo "ERROR: $pkgs/pyodide-lock.json missing — run build-packages.sh + build-pyrocko.sh first." >&2; exit 1; }

rm -rf "$out"; mkdir -p "$out"

# Core runtime: the ESM loader (used by the module worker), the asm glue + wasm,
# and the stdlib.
# pyodide.js (UMD) is intentionally omitted: the threaded path is a module worker
# and never importScripts() it.
for file in pyodide.mjs pyodide.asm.mjs pyodide.asm.wasm python_stdlib.zip; do
    cp "$runtime/$file" "$out/"
done
cp "$pkgs/pyodide-lock.json" "$out/"

# Every artifact the lock references (wheels + shared-lib zips like libopenblas),
# skipping the *-tests.tar test-data packages that are never imported at runtime.
python3 - "$pkgs" "$out" <<'PY'
import json, shutil, sys, pathlib
source_dir, target_dir = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
lock = json.loads((target_dir / "pyodide-lock.json").read_text())
copied = skipped = 0
missing = []
for package in lock["packages"].values():
    file_name = package["file_name"]
    if file_name.endswith("-tests.tar"):
        skipped += 1
        continue
    source = source_dir / file_name
    if source.exists():
        shutil.copy2(source, target_dir / file_name)
        copied += 1
    else:
        missing.append(file_name)
print(f"  staged {copied} artifacts ({skipped} test tarball(s) skipped)")
# A lock-referenced (non-test) wheel that isn't on disk means a partial build.
# Refuse it here rather than ship a served/ whose lock resolves to a 404 at
# loadPackage().
if missing:
    sys.exit(f"  ERROR: {len(missing)} lock-referenced artifact(s) absent from {source_dir}: "
             f"{sorted(missing)} — refusing to stage a partial build.")
PY

echo "threaded runtime staged → $out"
du -sh "$out"

# Ship bytecode, not sources: Pyodide runs with sys.dont_write_bytecode=True, so
# without this every page load recompiles the ~1000 modules the app imports (~3.3 s,
# measured in the browser).
# This also rewrites the lock's checksums, so it must run before check-lock.sh
# validates it.
bash "$here/precompile.sh" "$out"

# Refuse to ship a partial/stale build: every package the bridge loads at boot must
# be in the staged lock, or loadPackage() fails at runtime.
# This also turns a stale or partial *cached* dist-mt into a loud failure here
# instead of a broken deploy.
# Invoked via `bash` so it works even if the file's exec bit is ever lost.
bash "$here/check-lock.sh" "$out/pyodide-lock.json"
