"""FDSN web services: query URLs, and fetches that fall back to snuffler-web-proxy."""

from __future__ import annotations

import functools
from typing import Any, Optional
from urllib.parse import quote

from snuffler_web_proto import data_pb2

from . import operation, state

# The FDSN web services the app knows by name, all of which send CORS headers.
# The proxy allows these hosts; keep them in sync with
# `crates/snuffler-web-proxy/src/defaults.rs`.
_SERVICE_BASE_URLS = {
    "geofon": "https://geofon.gfz.de",
    "earthscope": "https://service.earthscope.org",
    "iris": "https://service.iris.edu",
    "usgs": "https://earthquake.usgs.gov",
    "orfeus": "https://www.orfeus-eu.org",
    "ingv": "https://webservices.ingv.it",
    "resif": "https://ws.resif.fr",
    "bgr": "https://eida.bgr.de",
}


def _service_base_url(name: str, default: str = "geofon") -> str:
    if name.startswith(("http://", "https://")):
        return name.rstrip("/")
    return _SERVICE_BASE_URLS.get(name, _SERVICE_BASE_URLS[default])


@functools.cache
def _proxy_base() -> Optional[str]:
    """snuffler-web-proxy's base URL, ending in ``/``, or None without a usable one.

    The worker imports the deployment's ``config.js`` before the bridge, which sets
    ``snufflerConfig`` (``web/config.js`` documents it).
    It is read once, like the Rust side reads it for live SeedLink streams.
    """
    try:
        import js
    except ImportError:
        return None
    proxy = getattr(getattr(js, "snufflerConfig", None), "proxyUrl", None)
    if not isinstance(proxy, str) or not proxy.strip():
        return None
    try:
        url = js.URL.new(proxy.strip(), js.location.href)
    except Exception:
        print("[py.fdsn] ignoring an invalid proxyUrl %r" % proxy)
        return None
    if url.protocol not in ("http:", "https:"):
        print("[py.fdsn] ignoring a non-HTTP proxyUrl %r" % proxy)
        return None
    base = str(url.href)
    return base if base.endswith("/") else base + "/"


async def fetch(url: str) -> Any:
    """GET an FDSN query, directly or, when the browser cannot, through the proxy.

    The data centers the app knows send CORS headers, so the proxy is a fallback for what a
    browser refuses: a data center without CORS headers, one that sends them twice
    (eida.bgr.de), or plain HTTP from an HTTPS page.
    The browser reports each of those as the same network error as an unreachable host, so
    any failure retries through the proxy.
    An HTTP error status is the data center's answer, never retried.
    """
    from pyodide.http import pyfetch

    try:
        return await operation.await_external(pyfetch(url))
    except Exception:
        proxy = _proxy_base()
        if proxy is None or not url.startswith(("http://", "https://")):
            raise
        # Retried inside the handler, so a failure here carries the direct one as its context.
        # The proxy answers 403 for a data center off its allowlist, which surfaces like any
        # other HTTP error.
        proxied = "%scors?url=%s" % (proxy, quote(url, safe=""))
        return await operation.await_external(pyfetch(proxied))


async def fetch_text_or_empty(url: str) -> str:
    """The document at ``url``, or "" for FDSN's own no-data answers.

    Only 204 and 404, the codes the standard assigns to "no matching data", read
    as empty.
    Every other failure raises, so a 403 from the proxy's allowlist or a 500
    from the service surfaces as the error it is, not as "0 loaded".
    """
    response = await fetch(url)
    if response.ok:
        return str(await operation.await_external(response.string()))
    if response.status in (204, 404):
        return ""
    raise RuntimeError("HTTP %s from %s" % (response.status, url))


def dataselect_url(request: data_pb2.WaveformDownloadRequest) -> str:
    """FDSN dataselect (waveform) query URL."""
    base = _service_base_url(request.datacenter)
    q = "&".join(
        [
            "net=%s" % (request.net or "*"),
            "sta=%s" % (request.sta or "*"),
            "loc=%s" % (request.loc or "*"),
            "cha=%s" % (request.cha or "*"),
            "starttime=%s" % request.start,
            "endtime=%s" % request.end,
            "nodata=204",
        ]
    )
    return "%s/fdsnws/dataselect/1/query?%s" % (base, q)


def event_url(request: data_pb2.CatalogRequest) -> str:
    """FDSN event (catalog) query URL.

    Text format by default; QuakeML with all origins when the request asks for
    moment tensors, the MT source a browser can reach (GlobalCMT and GEOFON-mt
    are blocked by mixed content or CORS)."""
    base = _service_base_url(request.datacenter)
    if request.with_moment_tensors:
        q = "&".join(
            [
                "format=xml",
                "starttime=%s" % request.start,
                "endtime=%s" % request.end,
                "minmagnitude=%s" % request.min_magnitude,
                "includeallorigins=true",
                "orderby=time",
            ]
        )
    else:
        q = "&".join(
            [
                "format=text",
                "starttime=%s" % request.start,
                "endtime=%s" % request.end,
                "minmagnitude=%s" % request.min_magnitude,
                "orderby=time",
                "limit=1000",
            ]
        )
    return "%s/fdsnws/event/1/query?%s" % (base, q)


def station_url(request: data_pb2.StationRequest) -> str:
    """FDSN station query URL — channel level, as Qt's download resolves them
    (download.py:78), with the location, the time span, and the optional
    radius search around the active event's epicentre."""
    base = _service_base_url(request.datacenter)
    extra = []
    if request.start:
        extra.append("starttime=%s" % request.start)
    if request.end:
        extra.append("endtime=%s" % request.end)
    if request.HasField("radius_deg") and state.active_event is not None:
        extra.append("latitude=%.5f" % state.active_event[0])
        extra.append("longitude=%.5f" % state.active_event[1])
        extra.append("maxradius=%.4f" % float(request.radius_deg))
    q = "&".join(
        [
            "net=%s" % (request.net or "*"),
            "sta=%s" % (request.sta or "*"),
            "loc=%s" % (request.loc or "*"),
            "cha=%s" % (request.cha or "*"),
            *extra,
            "level=channel",
            "format=text",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def response_url(request: data_pb2.StationRequest) -> str:
    """FDSN station query URL at response level (StationXML with instrument
    responses, for deconvolution)."""
    base = _service_base_url(request.datacenter)
    q = "&".join(
        [
            "net=%s" % (request.net or "*"),
            "sta=%s" % (request.sta or "*"),
            "cha=%s" % (request.cha or "*"),
            "level=response",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)
