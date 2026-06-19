#!/usr/bin/env bash
# `mise run fetch-pyrocko` — fetch the pyrocko source on demand.
#
# pyrocko is NOT vendored in this repo (sources/ is gitignored). This clones the
# exact, known-good commit (PYROCKO_REF) of pyrocko into sources/pyrocko, which
# the wheel build (build-wheel.sh) and the bundled demo data (test.mseed,
# referenced by index.html) need. The pin lives in mise.toml ([env]); run via
# `mise run fetch-pyrocko` so PYROCKO_REPO/PYROCKO_REF are set.
#
# Idempotent: a no-op once the pinned commit is already checked out.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"

REPO="${PYROCKO_REPO:?PYROCKO_REPO not set — run via 'mise run fetch-pyrocko'}"
REF="${PYROCKO_REF:?PYROCKO_REF not set — run via 'mise run fetch-pyrocko'}"
DEST="$ROOT/sources/pyrocko"

if [ -d "$DEST/.git" ] && [ "$(git -C "$DEST" rev-parse HEAD 2>/dev/null)" = "$REF" ]; then
  echo "pyrocko already at pinned commit ${REF:0:12} — nothing to do"
  exit 0
fi

echo "== fetching pyrocko @ ${REF:0:12} from $REPO =="
mkdir -p "$DEST"
git -C "$DEST" init -q
git -C "$DEST" remote add origin "$REPO" 2>/dev/null \
  || git -C "$DEST" remote set-url origin "$REPO"

# The pyrocko gitea server allows fetching an unadvertised commit shallowly
# (allowAnySHA1InWant), so we grab just that one commit (~27 MB) instead of the
# full history. Fall back to a full main fetch if a future server disables it.
if git -C "$DEST" fetch -q --depth 1 origin "$REF"; then
  git -C "$DEST" checkout -q --force "$REF"
else
  echo "shallow single-commit fetch unavailable; fetching main and checking out the pin"
  git -C "$DEST" fetch -q origin main
  git -C "$DEST" checkout -q --force "$REF"
fi

HEAD="$(git -C "$DEST" rev-parse HEAD)"
[ "$HEAD" = "$REF" ] || { echo "ERROR: checked-out HEAD $HEAD != pinned $REF"; exit 1; }
echo "OK -> sources/pyrocko @ $(git -C "$DEST" describe --tags 2>/dev/null || echo "$HEAD")"
