"""FDSN web services: query URLs, and fetches through the local CORS proxy."""

from __future__ import annotations

from typing import Any
from urllib.parse import quote

from snuffler.bridge.v1 import data_pb2

from . import operation, state

# CORS-enabled FDSN web services (verified to allow cross-origin fetch).
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


def _local_cors_proxy_url(url: str) -> str:
    if not (url.startswith("http://") or url.startswith("https://")):
        return url
    try:
        import js

        protocol = str(js.location.protocol)
        host = str(js.location.hostname)
    except Exception:
        return url
    if protocol != "http:" or host not in ("localhost", "127.0.0.1", "0.0.0.0", "::1"):
        return url
    return "http://127.0.0.1:17684/cors?url=%s" % quote(url, safe="")


async def fetch(url: str) -> Any:
    """GET an FDSN query, through the local CORS proxy when the page is served from localhost."""
    from pyodide.http import pyfetch

    proxied = _local_cors_proxy_url(url)
    try:
        response = await operation.await_external(pyfetch(proxied))
    except Exception:
        if proxied != url:
            return await operation.await_external(pyfetch(url))
        raise
    # The local proxy 403s any host off its allowlist, the normal outcome for a
    # custom base URL on localhost, and not an exception.
    # Retry such a host directly: if it sends CORS headers the query still
    # works, and if not the failure surfaces as the network error it is.
    if response.status == 403 and proxied != url:
        return await operation.await_external(pyfetch(url))
    return response


async def fetch_text_or_empty(url: str) -> str:
    """The document at ``url``, or "" for FDSN's own no-data answers.

    Only 204 and 404, the codes the standard assigns to "no matching data", read
    as empty.
    Every other failure raises, so a 403 from the local proxy's allowlist or a
    500 from the service surfaces as the error it is, not as "0 loaded".
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
