"""Request bytes written into the Pyodide filesystem, for pyrocko readers that take a path."""

from __future__ import annotations

import os

from . import state

# A scratch directory in Pyodide's in-memory filesystem.
STAGE_DIR = "/tmp/snuffler-staged"


def stage_bytes(name: str, content: bytes) -> str:
    """Write request bytes into the Pyodide FS and return the path.

    pyrocko's ``io.load`` reads a file, so file-carrying rpcs stage here.
    """
    os.makedirs(STAGE_DIR, exist_ok=True)
    state.stage_counter += 1
    safe = "".join(c if (c.isalnum() or c in "._-") else "_" for c in (name or "data"))[-96:]
    path = "%s/%d-%s" % (STAGE_DIR, state.stage_counter, safe or "data")
    with open(path, "wb") as f:
        f.write(content)
    return path
