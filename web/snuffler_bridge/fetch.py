"""Bundled assets fetched through the browser, from Python inside the worker."""

from __future__ import annotations

from . import operation


def asset_url(path: str) -> str:
    """Resolve a bundled-asset path against the worker's location.

    Matches the worker's own ``new URL(path, self.location.href)``.
    """
    import js

    return str(js.URL.new(path, js.location.href).href)


async def fetch_bytes(url: str) -> bytes:
    """GET raw bytes through the browser's fetch (pyodide-http).

    It is reachable from Python inside the worker, so network rpcs need no
    JavaScript orchestration.
    """
    from pyodide.http import pyfetch

    response = await operation.await_external(pyfetch(url))
    if not response.ok:
        raise RuntimeError("fetch %s: HTTP %s" % (url, response.status))
    return bytes(await operation.await_external(response.bytes()))


async def fetch_text_or_empty(url: str) -> str:
    """GET text, or "" on any non-2xx reply, so a missing optional asset reads as empty."""
    from pyodide.http import pyfetch

    response = await operation.await_external(pyfetch(url))
    return str(await operation.await_external(response.string())) if response.ok else ""
