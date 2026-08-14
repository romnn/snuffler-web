#!/usr/bin/env bash
#
# Precompile the staged runtime's Python sources to bytecode, in place.
#
# Pyodide runs with sys.dont_write_bytecode=True and the wheels + python_stdlib.zip
# ship sources only, so EVERY boot recompiles every module it imports from source.
# For this app's import graph (numpy -> scipy -> matplotlib -> pyrocko, ~1000
# modules) that is ~3.3 s of the boot, measured in the browser: 2.5 s for the
# wheels and 0.8 s for the stdlib. Shipping .pyc alongside the sources replaces
# that compile with a marshal.loads, which costs ~0.08 s.
#
# The bytecode is written with UNCHECKED_HASH invalidation: the import system then
# trusts the .pyc without stat-ing the source for an mtime/size match, which is
# what makes it survive being repacked into a wheel and unpacked at a fresh mtime.
#
# WHICH LAYOUT depends on whether the source stays. PEP 3147's
# __pycache__/<mod>.cpython-314.pyc is a CACHE for an existing <mod>.py: drop the
# source and the path finder stops seeing the module entirely (a package whose
# __init__.py is gone reads as an empty namespace package). Sourceless imports need
# compileall's legacy -b layout instead — <mod>.pyc exactly where <mod>.py was — so:
#   - stripping sources  -> legacy layout, for wheels and the stdlib zip alike
#   - keeping sources    -> __pycache__ for wheels; the stdlib zip stays legacy
#     either way, because zipimport knows nothing about __pycache__
#
# By default the .py a .pyc supersedes is DROPPED (STRIP_SOURCES=keep to keep it).
# Bytecode compresses worse than source, so keeping both would put ~21 MB on the
# download to save 3.3 s of CPU; replacing sources instead costs ~10 MB. What that
# buys back is paid on every boot, and what it costs is paid once per cache
# lifetime. The price is that library frames in a traceback lose their source line
# (and inspect.getsource stops working) for the archives rewritten here — our own
# bridge.py is staged separately as source and keeps both.
#
# Rewriting a wheel invalidates the sha256 the lock records for it, so the lock is
# updated in the same pass.
#
# Idempotent: an archive that already carries bytecode is left alone, so this can
# run again over an already-staged tree (and stage-web.sh calls it on every stage).
#
# Usage:  docker/pyodide-mt/precompile.sh [staged-dir]     (default: ./served)
#         STRIP_SOURCES=keep docker/pyodide-mt/precompile.sh   # keep the .py too
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/served}"

[ -d "$out" ] || { echo "ERROR: $out does not exist — run stage-web.sh first." >&2; exit 1; }
[ -f "$out/pyodide-lock.json" ] || { echo "ERROR: $out/pyodide-lock.json missing." >&2; exit 1; }

# The interpreter that writes the bytecode must be the same CPython minor version
# as the one in the browser; the magic number (and with it the bytecode format)
# changes between minor versions, and a mismatched .pyc is silently ignored at
# best. mise pins the host python to 3.14 for exactly this.
want="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["info"]["python"])' "$out/pyodide-lock.json")"
minor="${want%.*}"
host="$(command -v "python${minor}" || true)"
[ -n "$host" ] || {
    echo "ERROR: no python${minor} on PATH (the staged runtime is CPython ${want})." >&2
    echo "       \`mise install\` provides it; see the python pin in mise.toml." >&2
    exit 1
}

echo "precompiling $out with $host (CPython $minor, runtime is $want)"

"$host" - "$out" "${STRIP_SOURCES:-strip}" <<'PY'
import compileall
import hashlib
import io
import json
import pathlib
import py_compile
import sys
import tempfile
import zipfile

staged = pathlib.Path(sys.argv[1])
STRIP = sys.argv[2] == "strip"
MODE = py_compile.PycInvalidationMode.UNCHECKED_HASH


def superseded(name: str, compiled: set[str], legacy: bool) -> bool:
    """True if `name` (a .py in the archive) has bytecode that fully replaces it."""
    p = pathlib.PurePosixPath(name)
    if legacy:
        return str(p.with_suffix(".pyc")) in compiled
    tag = sys.implementation.cache_tag
    return str(p.parent / "__pycache__" / f"{p.stem}.{tag}.pyc") in compiled


def rewrite(archive: pathlib.Path, legacy: bool) -> tuple[int, int, int]:
    """Compile every .py in `archive` and repack it with the bytecode included.

    `legacy` selects compileall's -b layout (mod.pyc beside mod.py) for archives
    read by zipimport; wheels get the default __pycache__ layout instead.
    """
    with zipfile.ZipFile(archive) as zf:
        names = zf.namelist()
        if any(n.endswith(".pyc") for n in names):
            return (0, 0, 0)
        if not any(n.endswith(".py") for n in names):
            return (0, 0, 0)
        # Preserve the original compression so a wheel that shipped stored entries
        # (the *.so payloads) is not silently recompressed into a slower install.
        infos = zf.infolist()
        with tempfile.TemporaryDirectory() as tmp:
            work = pathlib.Path(tmp)
            zf.extractall(work)

            ok = compileall.compile_dir(
                work, quiet=2, force=True, legacy=legacy, invalidation_mode=MODE
            )
            if not ok:
                # A single unparsable file (py2 fixtures, template stubs) must not
                # sink the build: the rest still gets bytecode, and anything that
                # failed keeps compiling from source at runtime exactly as before.
                print(f"    note: some files in {archive.name} did not compile", flush=True)

            added = sorted(
                p for p in work.rglob("*.pyc") if str(p.relative_to(work)) not in names
            )
            if not added:
                return (0, 0, 0)

            compiled = {str(p.relative_to(work)) for p in added}
            buf = io.BytesIO()
            with zipfile.ZipFile(buf, "w") as out:
                for info in infos:
                    if STRIP and info.filename.endswith(".py") and superseded(
                        info.filename, compiled, legacy
                    ):
                        continue
                    out.writestr(info, zf.read(info.filename))
                for pyc in added:
                    out.write(pyc, str(pyc.relative_to(work)), zipfile.ZIP_DEFLATED)

    before = archive.stat().st_size
    archive.write_bytes(buf.getvalue())
    return (len(added), before, archive.stat().st_size)


lock_path = staged / "pyodide-lock.json"
lock = json.loads(lock_path.read_text())
by_file = {pkg["file_name"]: pkg for pkg in lock["packages"].values()}

total_before = total_after = total_pyc = 0
for path in sorted(staged.iterdir()):
    if path.suffix not in (".whl", ".zip"):
        continue
    # zipimport reads python_stdlib.zip and never looks in __pycache__; stripped
    # wheels need the same sibling layout for the path finder to see them at all.
    n, before, after = rewrite(path, legacy=STRIP or path.name == "python_stdlib.zip")
    if not n:
        continue
    total_pyc += n
    total_before += before
    total_after += after
    entry = by_file.get(path.name)
    if entry is not None:
        entry["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
    print(f"  {path.name}: +{n} .pyc, {before / 1e6:.1f} -> {after / 1e6:.1f} MB")

if not total_pyc:
    print("  already precompiled — nothing to do")
    raise SystemExit(0)

lock_path.write_text(json.dumps(lock, indent=2, sort_keys=True) + "\n")
delta = total_after - total_before
print(
    f"  precompiled {total_pyc} modules; staged size "
    f"{total_before / 1e6:.1f} -> {total_after / 1e6:.1f} MB "
    f"({'+' if delta >= 0 else ''}{delta / 1e6:.1f} MB); lock checksums updated"
)
PY
