#!/usr/bin/env bash
#
# Fail loudly if a staged Pyodide lock can't actually satisfy the app's boot-time
# loadPackage().
# A truncated/stale py-mt build silently breaks the deploy: pyodide-lock.json (or
# its wheels) ends up incomplete, so loadPackage() throws "No known package" or
# 404s a wheel at runtime, which a browser with a warm cache does not notice.
#
# loadPackage() resolves the TRANSITIVE dependency closure of the requested names
# and fetches every wheel, so this guard validates that whole closure — names AND
# the wheel files next to the lock — not just the top-level names. It runs in
# stage-web.sh (local builds + every CI path) AND on the final deployed artifact in
# the Pages workflow, so a broken lock can never reach users unnoticed.
#
# Keep `required` in sync with PYODIDE_PACKAGES in web/bridge_worker.ts.
#
# Usage:  check-lock.sh <path/to/pyodide-lock.json>
set -euo pipefail

lock="${1:?usage: check-lock.sh <pyodide-lock.json>}"
required="numpy scipy matplotlib pyyaml micropip protobuf requests pyodide-http pyrocko"

# Unquoted on purpose: one package name per word.
# shellcheck disable=SC2086
python3 - "$lock" $required <<'PY'
import json, os, sys

lock_path, required = sys.argv[1], sys.argv[2:]
lockdir = os.path.dirname(os.path.abspath(lock_path))
try:
    packages = json.load(open(lock_path))["packages"]
except (OSError, ValueError, KeyError) as e:
    sys.exit(f"ERROR: cannot read packages from {lock_path}: {e}")

# Transitive closure of the boot packages over the lock's dependency graph —
# loadPackage() resolves and fetches exactly this set.
closure, stack, missing_names = set(), list(required), []
while stack:
    name = stack.pop()
    if name in closure:
        continue
    closure.add(name)
    pkg = packages.get(name)
    if pkg is None:
        missing_names.append(name)
        continue
    stack.extend(pkg.get("depends", []))

# Every closure member's wheel must actually be staged next to the lock.
missing_files = []
for name in sorted(closure):
    pkg = packages.get(name)
    if pkg is None:
        continue  # already counted in missing_names
    fn = pkg.get("file_name")
    if not fn or not os.path.exists(os.path.join(lockdir, fn)):
        missing_files.append(fn or f"<no file_name for {name!r}>")

print(
    f"{lock_path}: {len(packages)} packages in lock; "
    f"boot closure = {len(closure)} ({len(required)} requested + deps)"
)
if missing_names:
    print(f"ERROR: lock is missing packages needed at boot: {sorted(missing_names)}", file=sys.stderr)
if missing_files:
    print(f"ERROR: lock references wheels not present beside it: {sorted(missing_files)}", file=sys.stderr)
if missing_names or missing_files:
    sys.exit(
        "  Truncated or stale py-mt build — refusing to stage/ship it.\n"
        "  Re-fetch with docker/pyodide-mt/fetch-web.sh, or rebuild with\n"
        "  build-all.sh (or bust the CI cache)."
    )
print(f"OK: boot closure of {len(closure)} packages present, with wheels")
PY
