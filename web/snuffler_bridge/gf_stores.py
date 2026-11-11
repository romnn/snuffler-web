"""Green's-function stores, mounted from the user's disk or fetched over HTTP."""

from __future__ import annotations

import os
import shutil

from snuffler_web_proto import bridge_pb2

from . import operation, pyrocko_modules, state

# Dropdown label prefix for GF-store earth models (mirrors Qt's "GF Store: <id>").
LABEL_PREFIX = "GF Store: "


def register(store_dir: str) -> str:
    """Register a mounted GF store, so its embedded 1D earth model is selectable
    in Cake Phase, and return its dropdown label."""
    gfstore, _gfmeta = pyrocko_modules.gf_store_modules()
    cfg = gfstore.Store(store_dir).config
    sid = str(cfg.id)
    state.gf_stores[sid] = store_dir
    return LABEL_PREFIX + sid


async def load_from_url(base_url: str) -> bridge_pb2.StringValue:
    """Mount a Green's-functions store fetched from an HTTP base URL.

    The base must serve a ``.manifest`` of newline-separated relative file paths.
    Returns the store's "GF Store: <id>" label."""
    from pyodide.http import pyfetch

    base = base_url.rstrip("/")
    manifest = await operation.await_external(pyfetch(base + "/.manifest"))
    if not manifest.ok:
        raise RuntimeError("no .manifest at %s (HTTP %s)" % (base, manifest.status))
    files = [
        line.strip()
        for line in (await operation.await_external(manifest.string())).splitlines()
        if line.strip()
    ]
    # Download into a per-attempt staging tree and move it into place only once
    # every file landed: the session transaction rolls back state.gf_stores but not
    # the filesystem, so a half-downloaded store must never occupy a root a
    # later attempt reuses.
    state.stage_counter += 1
    staging = "/gfstores/.staging%d" % state.stage_counter
    os.makedirs(staging, exist_ok=True)
    try:
        for rel in files:
            response = await operation.await_external(pyfetch(base + "/" + rel))
            if not response.ok:
                raise RuntimeError("fetch %s: HTTP %s" % (rel, response.status))
            dest = "%s/%s" % (staging, rel)
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "wb") as handle:
                handle.write(bytes(await operation.await_external(response.bytes())))
        root = "/gfstores/store%d" % len(state.gf_stores)
        shutil.rmtree(root, ignore_errors=True)
        os.replace(staging, root)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return bridge_pb2.StringValue(value=register(root))
