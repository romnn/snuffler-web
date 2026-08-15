#!/usr/bin/env python3
"""Pack ``examples/<slug>/`` into the bundles the app fetches on demand.

One zip per dataset plus an ``examples.json`` index, written where the trunk
post-build hook can stage them into ``assets/examples/``. Nothing here is bundled
into the wasm: the index is a few hundred bytes fetched when the Examples menu is
first opened, and a dataset's bytes cross the network only when someone asks for
that dataset.

Why a zip rather than the loose files. A dataset is never one file — waveforms plus
per-network StationXML plus an event catalogue — so eifel would be thirteen fetches
to load one example. It also compresses: eifel's 44 MB of files ship as 20 MB,
almost all of it wrung out of the StationXML. And a bundle is a thing a lecturer can
hand out, which is why the loader accepts any zip of data files, not only these.

The output is deterministic — fixed member timestamps, fixed permissions, sorted
member order — so rebuilding from unchanged inputs produces byte-identical zips and
nothing downstream re-uploads or re-downloads. Files are only rewritten when their
contents actually change, so the staging hook's copy-if-newer stays quiet too.

    python3 scripts/build-examples.py [--out DIR] [--force]
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import sys
import tomllib
import zipfile
from pathlib import Path
from typing import Any, Dict, List

ROOT = Path(__file__).resolve().parents[1]
EXAMPLES = ROOT / "examples"
DEFAULT_OUT = ROOT / "target" / "examples"

# The file that describes a bundle rather than belonging in it — including the
# `[source]` queries the data came from, which are provenance for whoever maintains
# the dataset, not data for whoever loads it.
NOT_DATA = frozenset({"example.toml"})

# The zip epoch. Member mtimes are the one thing that would otherwise make two
# builds of identical bytes differ, and a dataset's real timestamps are already
# recorded in the data itself.
FIXED_TIME = (1980, 1, 1, 0, 0, 0)

# rw-r--r--, as a zip external attribute.
FIXED_MODE = 0o644 << 16

# The metadata every bundle carries as its first member, so a downloaded zip still
# says what it is and where it came from.
BUNDLE_MANIFEST = "snuffler.json"


def _data_files(directory: Path) -> List[Path]:
    """Every file that belongs in the bundle, sorted, as paths relative to it."""
    files = [
        path.relative_to(directory)
        for path in directory.rglob("*")
        if path.is_file()
        and path.name not in NOT_DATA
        and not path.name.startswith(".")
        and "__pycache__" not in path.parts
    ]
    return sorted(files)


def _read_metadata(directory: Path) -> Dict[str, Any]:
    spec = directory / "example.toml"
    if not spec.exists():
        raise SystemExit(f"{directory.relative_to(ROOT)}: no example.toml")
    meta = tomllib.loads(spec.read_text())
    missing = [key for key in ("title", "blurb") if not str(meta.get(key, "")).strip()]
    if missing:
        raise SystemExit(f"{spec.relative_to(ROOT)}: missing {', '.join(missing)}")
    return meta


def _pack(directory: Path, meta: Dict[str, Any], members: List[Path]) -> bytes:
    """The bundle, as bytes. Written to disk only if it differs from what is there."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, payload in [(BUNDLE_MANIFEST, json.dumps(meta, indent=2).encode())] + [
            (str(rel), (directory / rel).read_bytes()) for rel in members
        ]:
            info = zipfile.ZipInfo(name, date_time=FIXED_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = FIXED_MODE
            # ZipInfo() defaults to whatever platform wrote it; pin it so a build on
            # macOS and a build on the CI runner agree byte for byte.
            info.create_system = 3
            archive.writestr(info, payload)
    return buffer.getvalue()


def _write_if_changed(path: Path, payload: bytes) -> bool:
    if path.exists() and path.read_bytes() == payload:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(payload)
    return True


def _up_to_date(target: Path, directory: Path, members: List[Path]) -> bool:
    """Whether an existing bundle is newer than everything that goes into it.

    Compressing 44 MB of eifel at level 9 costs two and a half seconds, and this
    runs from the trunk post-build hook — on every rebuild in the watch loop, where
    that is pure latency. Deciding on mtimes turns it into a few file stats.

    Mtimes can lie (a checkout of an older revision, a tool that preserves them),
    so `--force` repacks unconditionally; the deterministic output means a needless
    repack still writes nothing.
    """
    if not target.exists():
        return False
    inputs = [directory / "example.toml", *(directory / rel for rel in members)]
    return target.stat().st_mtime >= max(path.stat().st_mtime for path in inputs)


def build(out: Path, force: bool = False) -> List[Dict[str, Any]]:
    directories = sorted(p for p in EXAMPLES.iterdir() if p.is_dir())
    if not directories:
        raise SystemExit(f"no example datasets under {EXAMPLES.relative_to(ROOT)}")

    index: List[Dict[str, Any]] = []
    for directory in directories:
        slug = directory.name
        meta = _read_metadata(directory)
        members = _data_files(directory)
        if not members:
            raise SystemExit(f"examples/{slug}: example.toml but no data files")

        entry = {
            "slug": slug,
            "title": str(meta["title"]).strip(),
            "blurb": str(meta["blurb"]).strip(),
            "characteristics": [str(item) for item in meta.get("characteristics", [])],
            "citation": str(meta.get("citation", "")).strip(),
            # The view that shows this dataset's point, applied on load. Only the
            # bundle carries it — the index describes what an example is, not how
            # to open it.
            "state": dict(meta.get("state", {})),
            "files": [str(rel) for rel in members],
        }
        target = out / f"{slug}.zip"
        if not force and _up_to_date(target, directory, members):
            payload, changed = target.read_bytes(), False
        else:
            payload = _pack(directory, entry, members)
            changed = _write_if_changed(target, payload)
        index.append(
            {
                **{
                    key: entry[key]
                    for key in ("slug", "title", "blurb", "characteristics", "citation")
                },
                "url": f"assets/examples/{slug}.zip",
                "bytes": len(payload),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
        )
        print(
            f"[examples] {slug}: {len(members)} files -> {len(payload) / 1e6:.2f} MB"
            f"{'' if changed else ' (unchanged)'}"
        )

    _write_if_changed(
        out / "examples.json", (json.dumps({"examples": index}, indent=2) + "\n").encode()
    )
    return index


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT, help="output directory")
    parser.add_argument("--force", action="store_true", help="repack even if nothing changed")
    args = parser.parse_args()
    build(args.out.resolve(), args.force)
    return 0


if __name__ == "__main__":
    sys.exit(main())
