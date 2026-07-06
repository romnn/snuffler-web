"""Type-checked pyrocko shim that runs inside Pyodide and drives the egui
frontend.

This module is the *only* place that touches pyrocko. It exposes a tiny, stable,
protobuf API to the Pyodide worker (`web/bridge_worker.js`), which in turn is
called from Rust (`src/bridge.rs`) via `web/bridge.js`. The worker-facing
``*_pb`` functions parse and return generated protobuf messages; every request
and response on the live path is protobuf bytes (no JSON crosses the boundary).

pyrocko itself is left completely unmodified; we only call its public API
(`pyrocko.trace.Trace`, `pyrocko.io.load`).
"""

from __future__ import annotations

import io
import math
import os
import re
import time
import traceback
from collections import OrderedDict
from datetime import datetime, timedelta, timezone
from importlib import import_module
from typing import (
    Any,
    Callable,
    Dict,
    Iterator,
    List,
    Mapping,
    NamedTuple,
    Optional,
    Protocol,
    Tuple,
    cast,
)
from urllib.parse import quote

import numpy as np
from pyrocko import io as pio
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import (
    bridge_dispatch,
    bridge_pb2,
    common_pb2,
    data_pb2,
    figure_pb2,
    scene_pb2,
    snuffling_pb2,
    view_pb2,
)

NslcId = Tuple[str, str, str, str]
TraceRangeFunc = Callable[..., Mapping[Any, Tuple[Any, Any]]]


class AutoScalerLike(Protocol):
    def make_scale(self, data_range: Tuple[float, float]) -> Tuple[float, float, float]: ...


# The current set of loaded traces (snuffler's "pile").
_TRACES: List[ptrace.Trace] = []
# Provenance: traces contributed by each on-disk source (File System Access
# handle id), so "reload modified files" can swap just that source's traces.
_SOURCE_TRACES: "OrderedDict[str, List[ptrace.Trace]]" = OrderedDict()

# Trace visibility driven by the ':' command line. `_HIDDEN` is a list of NSLC
# globs to hide; `_REDUCE`, when set, keeps only traces matching it. Matching is
# fnmatch over the "net.sta.loc.cha" string. Data is never dropped, only hidden.
_HIDDEN: List[str] = []
_REDUCE: Optional[str] = None
# Per-pattern amplitude scaling overrides from the ':scaling' command
# (snuffler's scaling hooks). Keyed by n.s.l.c pattern; the '_' key is the global
# (no-pattern) rule. Each value is (vmin, vmax) where None leaves that bound auto.
_SCALING_HOOKS: "OrderedDict[str, Tuple[Optional[float], Optional[float]]]" = OrderedDict()
_DATA_GEN = 0
_RESPONSE_GEN = 0
_VIEW_CACHE_LIMIT = 256
_MIN_DECIMATION_BINS = 16
_MAX_DECIMATION_BINS = 8192
_VIEW_PERF_LOG_INTERVAL_S = 1.0
_VIEW_PERF_SLOW_MS = 25.0
_VIEW_CACHE: "OrderedDict[Tuple[Any, ...], Tuple[float, float, np.ndarray]]" = OrderedDict()
_OLD_DATA_RANGES: Dict[Any, Tuple[float, float]] = {}
_OLD_DATA_RANGE_CONTEXT: Optional[Tuple[Any, ...]] = None
_LAST_VIEW_PERF_LOG_S = 0.0


def _clear_view_cache() -> None:
    global _OLD_DATA_RANGE_CONTEXT
    _VIEW_CACHE.clear()
    _OLD_DATA_RANGES.clear()
    _OLD_DATA_RANGE_CONTEXT = None


def _mark_traces_changed() -> None:
    global _DATA_GEN
    _DATA_GEN += 1
    _clear_view_cache()


def _set_traces(traces: List[ptrace.Trace], append: bool = False) -> None:
    global _TRACES
    _TRACES = (list(_TRACES) + traces) if append else traces
    if not append:
        _SOURCE_TRACES.clear()
    _mark_traces_changed()


def _clear_event_context() -> None:
    global _ACTIVE_EVENT, _ACTIVE_MT, _EVENTS
    _ACTIVE_EVENT = None
    _ACTIVE_MT = None
    _EVENT_REGISTRY.clear()
    _EVENTS = []


def _replace_source(source_id: str, new_traces: List[ptrace.Trace]) -> None:
    """Swap the traces previously loaded from ``source_id`` for freshly-loaded ones
    (the reload path). Removes the old per-source Trace objects by identity and
    appends the new, leaving synthetic/FDSN/other-source traces untouched — the
    faithful analog of pyrocko ``reload_modified`` only touching changed files."""
    global _TRACES
    old = _SOURCE_TRACES.get(source_id)
    if old:
        old_ids = {id(t) for t in old}
        _TRACES = [t for t in _TRACES if id(t) not in old_ids]
    _TRACES = list(_TRACES) + new_traces
    _SOURCE_TRACES[source_id] = new_traces
    _mark_traces_changed()


def _visible_traces() -> List[ptrace.Trace]:
    # pyrocko's canonical NSLC matcher (shell globs, case-insensitive, anchored on
    # the full "net.sta.loc.cha" code), the same one snuffler's hide/quick-filter
    # use — match_nslc takes a list of patterns and returns True if any matches.
    from pyrocko.util import match_nslc

    out = []
    for tr in _TRACES:
        if _REDUCE is not None and not match_nslc(_REDUCE, tr.nslc_id):
            continue
        if _HIDDEN and match_nslc(_HIDDEN, tr.nslc_id):
            continue
        out.append(tr)
    return out


# Station coordinates (net, sta) -> (lat, lon, elevation) and the active event
# (lat, lon, depth_m, time), populated by the FDSN fetch helpers below. Used for
# distance/azimuth sorting. The fetch itself runs here in Python (pyfetch over the
# browser's network); URL building, parsing and geo math are all pyrocko/Python.
_STATIONS: Dict[Tuple[str, str], Tuple[float, float, float]] = {}
_ACTIVE_EVENT: Optional[Tuple[float, float, float, float]] = None
# Moment tensor of the active (largest-magnitude) catalog event, if it has one.
# Used as the reference for the marker table's Kagan-angle column.
_ACTIVE_MT: Any = None
# All loaded events, keyed by their marker event_hash, so the user can pick any of
# them as the active event (distance/Kagan/cake/seismosizer reference).
_EVENT_REGISTRY: "OrderedDict[str, Any]" = OrderedDict()
# Instrument responses (a parsed FDSNStationXML), used by Deconvolution.
_STATIONXML: Any = None
# Catalog events (lat, lon, depth_m, mag, time) for the Map overlay.
_EVENTS: List[data_pb2.MapEvent] = []

# CORS-enabled FDSN web services (verified to allow cross-origin fetch).
_FDSN = {
    "geofon": "https://geofon.gfz.de",
    "earthscope": "https://service.earthscope.org",
    "iris": "https://service.iris.edu",
    "usgs": "https://earthquake.usgs.gov",
    "orfeus": "https://www.orfeus-eu.org",
    "ingv": "https://webservices.ingv.it",
    "resif": "https://ws.resif.fr",
    "bgr": "https://eida.bgr.de",
}


def _fdsn_base(name: str, default: str = "geofon") -> str:
    if name.startswith(("http://", "https://")):
        return name.rstrip("/")
    return _FDSN.get(name, _FDSN[default])


_STAGE_DIR = "/staged"
_STAGE_COUNTER = 0


def _stage_bytes(name: str, content: bytes) -> str:
    """Write request bytes into the Pyodide FS and return the path. pyrocko's
    ``io.load`` reads a file, so file-carrying rpcs stage here in Python — the
    worker is a dumb relay and no longer touches the FS for loads."""
    global _STAGE_COUNTER
    os.makedirs(_STAGE_DIR, exist_ok=True)
    _STAGE_COUNTER += 1
    safe = "".join(c if (c.isalnum() or c in "._-") else "_" for c in (name or "data"))[-96:]
    path = "%s/%d-%s" % (_STAGE_DIR, _STAGE_COUNTER, safe or "data")
    with open(path, "wb") as f:
        f.write(content)
    return path


def _asset_url(path: str) -> str:
    """Resolve a bundled-asset path against the worker's location, matching the
    worker's ``new URL(path, self.location.href)``."""
    import js

    return str(js.URL.new(path, js.location.href).href)


async def _fetch_bytes(url: str) -> bytes:
    """GET raw bytes through the browser's fetch (pyodide-http), which is reachable
    from Python inside the worker — so network rpcs need no JS orchestration."""
    from pyodide.http import pyfetch

    resp = await pyfetch(url)
    if not resp.ok:
        raise RuntimeError("fetch %s: HTTP %s" % (url, resp.status))
    return bytes(await resp.bytes())


async def _fetch_text_or_empty(url: str) -> str:
    """GET text, returning '' on any non-2xx — an FDSN 'no data' reply parses to an
    empty result rather than an error."""
    from pyodide.http import pyfetch

    resp = await pyfetch(url)
    return str(await resp.string()) if resp.ok else ""


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


def _local_fdsn_proxy_url(url: str) -> str:
    return _local_cors_proxy_url(url)


def _seisbench_progress(frac: float, label: str) -> None:
    try:
        import js

        js.__snufflerSeisbenchProgress(float(frac), label)
    except Exception:
        pass


async def _pyfetch_fdsn(url: str) -> Any:
    from pyodide.http import pyfetch

    proxied = _local_fdsn_proxy_url(url)
    try:
        return await pyfetch(proxied)
    except Exception:
        if proxied != url:
            return await pyfetch(url)
        raise


async def _fetch_fdsn_text_or_empty(url: str) -> str:
    resp = await _pyfetch_fdsn(url)
    return str(await resp.string()) if resp.ok else ""


def _trace_meta(msg: scene_pb2.TraceMeta, tr: ptrace.Trace) -> None:
    net, sta, loc, cha = tr.nslc_id
    msg.net, msg.sta, msg.loc, msg.cha = net, sta, loc, cha
    msg.label = ".".join(tr.nslc_id)
    msg.tmin = float(tr.tmin)
    msg.tmax = float(tr.tmax)
    msg.deltat = float(tr.deltat)
    msg.nsamples = int(tr.data_len())


def _scene_meta() -> scene_pb2.SceneMeta:
    msg = scene_pb2.SceneMeta()
    for tr in _visible_traces():
        _trace_meta(msg.tracks.add(), tr)
    msg.tmin = min((t.tmin for t in msg.tracks), default=0.0)
    msg.tmax = max((t.tmax for t in msg.tracks), default=1.0)
    return msg


def _make_example_scene() -> scene_pb2.SceneMeta:
    """Synthesize a small multi-station / 3-component dataset (noise + a
    travelling event packet), so the viewer is populated without any file IO."""
    deltat = 0.02  # 50 Hz
    n = 30_000  # 600 s
    tmin = 1_700_000_000.0
    stations = ["STA01", "STA02", "STA03", "STA04", "STA05", "STA06"]
    channels = ["HHZ", "HHN", "HHE"]
    t = np.arange(n) * deltat
    rng = np.random.default_rng(42)

    traces: List[ptrace.Trace] = []
    for si, sta in enumerate(stations):
        arrival = 180.0 + si * 18.0
        env = np.exp(-((t - arrival) ** 2) / (2.0 * 8.0**2))
        for cha in channels:
            phase = rng.uniform(0.0, 2.0 * np.pi)
            carrier = np.sin(2.0 * np.pi * 3.2 * (t - arrival) + phase) + 0.45 * np.sin(
                2.0 * np.pi * 6.7 * (t - arrival) + 0.3 * phase
            )
            coda = np.exp(-np.maximum(t - arrival, 0.0) / 26.0)
            coda[t < arrival] = 0.0
            burst_noise = rng.standard_normal(n)
            event = env * carrier + 0.22 * coda * burst_noise
            noise = rng.standard_normal(n)
            y = 0.22 * noise + event
            traces.append(
                ptrace.Trace(
                    network="XX",
                    station=sta,
                    location="",
                    channel=cha,
                    deltat=deltat,
                    tmin=tmin,
                    ydata=y.astype(np.float64),
                )
            )

    _set_traces(traces)
    return _scene_meta()


def _load_path_scene(
    name: str, path: str, fmt: str, append: bool = False, source_id: str = ""
) -> scene_pb2.SceneMeta:
    """Load a file already staged in Pyodide's FS with pyrocko's ``io.load``. With
    a ``source_id`` the load replaces that source's previous traces (reload)."""
    del name
    fmt2 = "detect" if fmt in (None, "", "detect") else fmt
    traces = list(pio.load(path, format=fmt2))
    if source_id:
        _replace_source(source_id, traces)
    else:
        _set_traces(traces, append=append)
    return _scene_meta()


def _stable_decimation_stride(samples_per_bin: int) -> int:
    """Choose a stable stride so tiny zoom changes do not constantly re-bin."""
    stride = max(1, int(samples_per_bin))
    if stride <= 1:
        return 1
    value = 1.0
    step = 2.0**0.25
    while value < stride:
        value *= step
    return max(1, int(math.ceil(value)))


def _decimate(
    y: np.ndarray,
    deltat: float,
    npx: int,
    cap_bins: bool = True,
    sample_offset: int = 0,
) -> Tuple[np.ndarray, int, float]:
    """Return raw samples or a signal-preserving min/max envelope.

    ``dt > 0`` means the payload is raw uniformly-spaced samples. ``dt == 0``
    means the payload is interleaved ``(t_rel, y)`` points. The reduced
    representation keeps the first sample, extrema, and last sample of every
    pixel-scale bin in chronological order so the renderer draws a connected
    trace without smoothing over peaks.
    """
    n = len(y)
    npx = max(npx, _MIN_DECIMATION_BINS)
    if cap_bins:
        npx = min(npx, _MAX_DECIMATION_BINS)
    if n <= npx * 4:
        return np.ascontiguousarray(y, dtype="<f4"), n, deltat

    stride = _stable_decimation_stride(int(math.ceil(n / npx)))
    pieces: List[np.ndarray] = []
    sample_offset = max(0, int(sample_offset))
    global_start = sample_offset
    global_stop = sample_offset + n
    first_bin = global_start // stride
    last_bin = (global_stop + stride - 1) // stride

    def edge_indices(bin_idx: int) -> Optional[np.ndarray]:
        bin_start = bin_idx * stride
        start = max(0, bin_start - global_start)
        stop = min(n, bin_start + stride - global_start)
        if stop <= start:
            return None
        block = y[start:stop]
        return np.unique(
            np.array(
                (
                    start,
                    start + int(np.argmin(block)),
                    start + int(np.argmax(block)),
                    stop - 1,
                ),
                dtype=np.int64,
            )
        )

    full_first = first_bin + int(global_start > first_bin * stride)
    full_last = last_bin - int(global_stop < last_bin * stride)

    first_edge = edge_indices(first_bin) if first_bin < full_first else None
    if first_edge is not None:
        pieces.append(first_edge)

    full_bins = max(0, full_last - full_first)
    if full_bins:
        local_start = full_first * stride - global_start
        blocks = y[local_start : local_start + full_bins * stride].reshape(full_bins, stride)
        starts = local_start + np.arange(full_bins, dtype=np.int64) * stride
        idx4 = np.sort(
            np.stack(
                (
                    starts,
                    starts + np.argmin(blocks, axis=1),
                    starts + np.argmax(blocks, axis=1),
                    starts + stride - 1,
                ),
                axis=1,
            ),
            axis=1,
        )
        keep = np.ones(idx4.shape, dtype=bool)
        keep[:, 1:] = idx4[:, 1:] != idx4[:, :-1]
        pieces.append(idx4[keep])

    last_edge = edge_indices(last_bin - 1) if full_last < last_bin else None
    if last_edge is not None and (first_edge is None or last_bin - 1 != first_bin):
        pieces.append(last_edge)

    idx = np.concatenate(pieces) if len(pieces) > 1 else pieces[0]
    out = np.empty(idx.size * 2, dtype="<f4")
    out[0::2] = idx.astype(np.float64) * deltat
    out[1::2] = y[idx].astype("<f4", copy=False)
    return out, int(idx.size), 0.0


def _apply_rotation(traces: List[ptrace.Trace], deg: float) -> List[ptrace.Trace]:
    """Rotate horizontal component pairs (N/E or 1/2) by `deg` degrees, like
    snuffler's rotate control, using pyrocko's own ``trace.rotate`` (verified
    bit-identical to the earlier hand 2x2). Aligned, equal-length pairs only."""
    if not deg:
        return traces
    groups: Dict[Tuple[str, str, str], List[ptrace.Trace]] = {}
    for tr in traces:
        groups.setdefault(tr.nslc_id[:3], []).append(tr)

    out: List[ptrace.Trace] = []
    for grp in groups.values():
        by_comp = {t.channel[-1].upper(): t for t in grp if t.channel}
        rotated: set[int] = set()
        for n, e in (("N", "E"), ("1", "2")):
            tn, te = by_comp.get(n), by_comp.get(e)
            if (
                tn is not None
                and te is not None
                and tn.ydata is not None
                and te.ydata is not None
                and len(tn.ydata) == len(te.ydata)
                and tn.deltat == te.deltat
                and tn.tmin == te.tmin
            ):
                try:
                    pair = ptrace.rotate(
                        [tn, te],
                        deg,
                        in_channels=[tn.channel, te.channel],
                        out_channels=[tn.channel, te.channel],
                    )
                except Exception:  # noqa: BLE001 — leave unrotatable pairs as-is
                    continue
                if len(pair) == 2:
                    out.extend(pair)
                    rotated.update((id(tn), id(te)))
        out.extend(t for t in grp if id(t) not in rotated)
    return out


def _decon_freq_limits(
    deltat: float, data_len: int, hp: Optional[float], lp: Optional[float]
) -> Tuple[float, float, float, float]:
    """Cosine-taper frequency limits for response deconvolution, exactly as
    snuffler's ``PileViewer.get_freq_limits``: anchored to the current highpass /
    lowpass passband, clamped to a Nyquist-safe band and a data-length abs-min."""
    nyquist_safe = (1.0 / (2.0 * deltat)) * 0.9
    abs_min = 0.01 / (max(1, data_len) * deltat)
    taper_width = 0.5
    if hp:
        f2 = min(max(abs_min, hp), nyquist_safe)
        f1 = min(max(abs_min, f2 * (1.0 - taper_width)), nyquist_safe)
    else:
        f1 = f2 = abs_min
    if lp:
        f3 = min(lp, nyquist_safe)
        f4 = min(f3 * (1.0 + taper_width), nyquist_safe)
    else:
        f3 = f4 = nyquist_safe
    return f1, f2, f3, f4


def _remove_response(
    traces: List[ptrace.Trace],
    enabled: bool,
    hp: Optional[float] = None,
    lp: Optional[float] = None,
) -> List[ptrace.Trace]:
    """Deconvolve the instrument response to displacement using StationXML fetched
    from an FDSN station service (Data ▸ Download ▸ Fetch responses). A no-op until
    responses are loaded; traces without a matching response pass through. The
    frequency limits + cut-off-fading match snuffler's deconvolution exactly."""
    if not enabled or _STATIONXML is None:
        return traces
    out: List[ptrace.Trace] = []
    for tr in traces:
        try:
            resp = _STATIONXML.get_pyrocko_response(
                tr.nslc_id, timespan=(tr.tmin, tr.tmax), fake_input_units="M"
            )
            data_len = len(tr.ydata) if tr.ydata is not None else 0
            freqlimits = _decon_freq_limits(float(tr.deltat), data_len, hp, lp)
            out.append(
                tr.transfer(
                    freqlimits=freqlimits,
                    transfer_function=resp,
                    cut_off_fading=True,
                    demean=True,
                    invert=True,
                )
            )
        except Exception:  # noqa: BLE001 — a channel without a usable response passes through
            out.append(tr)
    return out


# Subsort -> order of nslc elements for the sort/grouping key (snuffler's subsort).
_SUBSORT = {
    "nslc": (0, 1, 2, 3),
    "nscl": (0, 1, 3, 2),
    "sncl": (1, 0, 3, 2),
    "lnsc": (2, 0, 1, 3),
    "cnsl": (3, 0, 1, 2),
    "nsc_l": (0, 1, 3, 2),
    "snc_l": (1, 0, 3, 2),
}


def _dist_deg(alat: float, alon: float, blat: float, blon: float) -> float:
    """Great-circle distance in degrees via pyrocko's pure-Python ``cosdelta``.

    ``orthodrome.distance_accurate50m`` needs ``orthodrome_ext`` (a C extension
    absent from our minimal wheel), so use the analytic cos-delta form, which is
    plenty accurate for sorting and cake travel times.
    """
    from pyrocko import orthodrome as od

    cd = max(-1.0, min(1.0, float(od.cosdelta(alat, alon, blat, blon))))
    return math.degrees(math.acos(cd))


def _sort_order(
    traces: List[ptrace.Trace], sort: str, subsort: str, distances_3d: bool = False
) -> List[int]:
    """Index order for the traces. Distance/azimuth sorts use station coordinates
    (from a station fetch) + the active event; without them they fall back to name
    order. With ``distances_3d`` the distance sort adds the source depth and station
    elevation (snuffler's '3D distances' option). All geo math is pyrocko's."""
    perm = _SUBSORT.get(subsort, (0, 1, 2, 3))

    def namekey(tr: ptrace.Trace) -> Tuple[str, str, str, str]:
        nslc = tr.nslc_id
        return (nslc[perm[0]], nslc[perm[1]], nslc[perm[2]], nslc[perm[3]])

    geo = sort in ("distance", "azimuth", "azimuth_blocks", "backazimuth")
    if geo and _ACTIVE_EVENT is not None and _STATIONS:
        from pyrocko import cake
        from pyrocko import orthodrome as od

        elat, elon, edep, _et = _ACTIVE_EVENT

        def geokey(tr: ptrace.Trace) -> Tuple[float, Any]:
            st = _STATIONS.get((tr.nslc_id[0], tr.nslc_id[1]))
            if st is None:
                return (1e30, namekey(tr))
            slat, slon, sele = st
            dist = _dist_deg(elat, elon, slat, slon)
            if sort == "distance":
                if distances_3d:
                    # Great-circle degrees -> surface metres, combined with the
                    # vertical offset (source depth + station elevation).
                    surface_m = dist * cake.d2r * cake.earthradius
                    dist = math.hypot(surface_m, float(edep) + float(sele))
                return (dist, namekey(tr))
            if sort == "backazimuth":
                return (float(od.azimuth(slat, slon, elat, elon)) % 360.0, dist)
            azi = float(od.azimuth(elat, elon, slat, slon)) % 360.0
            if sort == "azimuth":
                return (azi, dist)
            return (float(int(azi // 30.0)), dist)  # azimuth_blocks: 30° sectors

        return sorted(range(len(traces)), key=lambda i: geokey(traces[i]))

    return sorted(range(len(traces)), key=lambda i: namekey(traces[i]))


def _track_scaling_key(t: view_pb2.ViewTrack, group: str) -> Any:
    """Grouping key for shared amplitude scaling (snuffler's Scaling menu)."""
    if group == "common":
        return None
    if group == "per_station":
        return (t.net, t.sta)
    if group == "per_station_location":
        return (t.net, t.sta, t.loc)
    if group == "per_component":
        return t.cha
    return (t.net, t.sta, t.loc, t.cha)


def _trace_scaling_key(tr: ptrace.Trace, group: str) -> Any:
    if group == "common":
        return None
    if group == "per_station":
        return (tr.network, tr.station)
    if group == "per_station_location":
        return (tr.network, tr.station, tr.location)
    if group == "per_component":
        return tr.channel
    return tr.nslc_id


def _scaling_base_modes(base: str) -> Tuple[Any, str]:
    if base == "robust":
        return "minmax", "robust"
    if base == "mean2":
        return 2.0, "minmax"
    if base == "mean4":
        return 4.0, "minmax"
    return "minmax", "minmax"


def _range_trace(
    nslc: NslcId,
    tmin: float,
    deltat: float,
    ydata: np.ndarray,
) -> ptrace.Trace:
    net, sta, loc, cha = nslc
    return ptrace.Trace(
        network=net,
        station=sta,
        location=loc,
        channel=cha,
        tmin=tmin,
        deltat=deltat,
        ydata=np.asarray(ydata, dtype=np.float64),
    )


def _scale_context(req: view_pb2.ViewParams) -> Tuple[Any, ...]:
    return (
        _DATA_GEN,
        _RESPONSE_GEN,
        req.sort or "name",
        req.subsort or "nslc",
        _float_key(float(req.rotate)),
        bool(req.remove_response),
        bool(req.demean),
        bool(req.degap),
        _float_key(req.highpass if req.HasField("highpass") else None),
        _float_key(req.lowpass if req.HasField("lowpass") else None),
        bool(req.fft_filtering),
        bool(req.bandpass_lphp),
        req.scaling_base or "minmax",
        req.scaling_group or "individual",
    )


def _fft_filter(seg: ptrace.Trace, hp: Optional[float], lp: Optional[float]) -> None:
    """snuffler's 'FFT filtering': linear-detrend, multiply the (pow2-padded)
    spectrum by a Butterworth (order 4) low/high response, irfft, then retrend.

    Matches pyrocko.gui.snuffler.pile_viewer.prepare_cutout2 exactly (a smooth
    Butterworth roll-off, not a hard brick-wall mask). detrend/retrend are the
    OLS-line removal those snuffler helpers do (np.polyfit == their linregress)."""
    from pyrocko import response

    it = np.arange(seg.data_len(), dtype=float)
    y = np.asarray(seg.ydata, dtype=np.float64)
    coeffs = np.polyfit(it, y, 1)
    slope, offset = float(coeffs[0]), float(coeffs[1])
    seg.set_ydata(y - slope * it - offset)
    freqs, fdata = seg.spectrum(pad_to_pow2=True, tfade=None)
    resps = []
    if lp is not None:
        resps.append(response.ButterworthResponse(order=4, corner=float(lp), type="low"))
    if hp is not None:
        resps.append(response.ButterworthResponse(order=4, corner=float(hp), type="high"))
    tf = response.MultiplyResponse(resps).evaluate(freqs)
    filtered = np.fft.irfft(fdata * tf)[: seg.data_len()]
    seg.set_ydata(it * slope + filtered + offset)


def _filter_seg(
    seg: ptrace.Trace,
    hp: Optional[float],
    lp: Optional[float],
    fft: bool,
    lphp: bool,
) -> None:
    """Apply the main filter to a chopped trace, honoring the FFT-filtering and
    bandpass-as-low+highpass options. Filter corners at/above Nyquist are skipped
    (snuffler's ``< 0.5/deltat`` guards), not left to a bare try/except."""
    try:
        if fft and (hp is not None or lp is not None):
            _fft_filter(seg, hp, lp)
            return
        fmax = 0.5 / float(seg.deltat)
        if not lphp and hp is not None and lp is not None and hp < lp < fmax and hp < fmax:
            seg.bandpass(2, float(hp), float(lp))
        else:
            if lp is not None and lp < fmax:
                seg.lowpass(4, float(lp))
            if hp is not None and (lp is None or hp < lp) and hp < fmax:
                seg.highpass(4, float(hp))
    except Exception:
        pass


def _cache_get(key: Tuple[Any, ...]) -> Optional[Tuple[float, float, np.ndarray]]:
    entry = _VIEW_CACHE.get(key)
    if entry is not None:
        _VIEW_CACHE.move_to_end(key)
    return entry


def _cache_put(key: Tuple[Any, ...], entry: Tuple[float, float, np.ndarray]) -> None:
    _VIEW_CACHE[key] = entry
    _VIEW_CACHE.move_to_end(key)
    while len(_VIEW_CACHE) > _VIEW_CACHE_LIMIT:
        _VIEW_CACHE.popitem(last=False)


def _float_key(value: Optional[float]) -> Optional[float]:
    return None if value is None else round(float(value), 9)


def _processed_trace(
    tr: ptrace.Trace,
    track_key: Tuple[Any, ...],
    tmin: float,
    tmax: float,
    hp: Optional[float],
    lp: Optional[float],
    demean: bool,
    fft: bool,
    lphp: bool,
) -> Optional[Tuple[float, float, np.ndarray]]:
    del tmin, tmax
    proc_min, proc_max = float(tr.tmin), float(tr.tmax)
    key = (
        _DATA_GEN,
        _RESPONSE_GEN,
        track_key,
        _float_key(proc_min),
        _float_key(proc_max),
        _float_key(hp),
        _float_key(lp),
        bool(demean),
        bool(fft),
        bool(lphp),
    )
    cached = _cache_get(key)
    if cached is not None:
        return cached

    try:
        seg = tr.chop(proc_min, proc_max, inplace=False, include_last=True)
    except Exception:
        return None
    if seg.ydata is None or len(seg.ydata) == 0:
        return None

    seg.ydata = seg.ydata.astype(np.float64)
    if demean:
        seg.ydata = seg.ydata - seg.ydata.mean()
    _filter_seg(seg, hp, lp, fft, lphp)
    entry = (
        float(seg.tmin),
        float(seg.deltat),
        np.ascontiguousarray(seg.ydata, dtype=np.float64),
    )
    _cache_put(key, entry)
    return entry


def _slice_segment(
    entry: Tuple[float, float, np.ndarray],
    tmin: float,
    tmax: float,
    sample_support: float = 0.0,
) -> Optional[Tuple[np.ndarray, float, float, int]]:
    seg_tmin, deltat, y = entry
    pad = max(0.0, float(sample_support)) * deltat
    start = max(0, int(math.floor((tmin - pad - seg_tmin) / deltat)))
    stop = min(len(y), int(math.ceil((tmax + pad - seg_tmin) / deltat)) + 1)
    if stop <= start:
        return None
    return y[start:stop], seg_tmin + start * deltat, deltat, start


def _display_time_span(tr: ptrace.Trace) -> Tuple[float, float]:
    t0, t1 = float(tr.tmin), float(tr.tmax)
    if tr.nslc_id[3].endswith("-RMS") and int(tr.data_len()) > 0:
        half = 0.5 * float(tr.deltat)
        return t0 - half, t1 + half
    return t0, t1


def _waterfall_trace(
    yview: np.ndarray,
    deltat: float,
    npx: int,
    want_abs: bool,
    want_integrate: bool,
    color_scale: str = "lin",
) -> Tuple[np.ndarray, float]:
    """Snuffler TraceWaterfall data for one trace: normalized amplitude over time.

    With the envelope (``want_abs``), ``color_scale`` (``lin`` | ``sqrt`` | ``log``)
    compresses the amplitude before it is colour-mapped, honouring snuffler's
    waterfall colour-scale control (the Rust side only normalises + blits)."""
    from scipy import signal as sps

    y = np.asarray(yview, dtype=np.float64)
    if y.size == 0:
        return np.zeros(0, dtype="<f4"), float(deltat)

    step = max(1, int(y.size // max(1, 2 * int(npx))))
    data = np.ascontiguousarray(y[::step], dtype=np.float64)
    if want_integrate:
        data = np.cumsum(data) * float(deltat)

    peak = float(np.abs(data).max()) if data.size else 0.0
    if peak > 0.0:
        data = data / peak

    if want_abs and data.size:
        hilbert: Callable[[np.ndarray], np.ndarray] = getattr(sps, "hilbert")
        data = np.abs(hilbert(data))
        if color_scale == "sqrt":
            data = np.sqrt(data)
        elif color_scale == "log":
            # map [0, 1] -> [0, 1] with log compression (log10(1)=0, log10(10)=1).
            data = np.log10(data * 9.0 + 1.0)

    return np.ascontiguousarray(data, dtype="<f4"), float(deltat) * float(step)


_WF_LUTS: Dict[str, np.ndarray] = {}


def _wf_colormap_lut(name: str) -> np.ndarray:
    """256x3 uint8 RGB lookup table for a waterfall colormap, sampled from
    matplotlib so the colours are byte-identical to the Qt snuffler (which colours
    its waterfall with the same matplotlib colormaps). Cached per name."""
    key = name or "viridis"
    lut = _WF_LUTS.get(key)
    if lut is None:
        import matplotlib as mpl

        try:
            cmap = mpl.colormaps[key]
        except KeyError:
            cmap = mpl.colormaps["viridis"]
        xs = np.linspace(0.0, 1.0, 256)
        rgb = np.asarray(cmap(xs))[:, :3] * 255.0
        lut = np.ascontiguousarray(rgb.round().clip(0.0, 255.0).astype(np.uint8))
        _WF_LUTS[key] = lut
    return lut


def _wf_rgba(
    strip: np.ndarray,
    lo: float,
    hi: float,
    clip_lo: float,
    clip_hi: float,
    symmetry: bool,
    reverse: bool,
    lut: np.ndarray,
) -> np.ndarray:
    """Colour-map one waterfall amplitude strip to RGBA, packed one pixel per
    float32 (its 4 little-endian bytes ARE the RGBA) so it rides the same
    zero-copy ``ys`` buffer; the frontend just reinterprets and blits. Mirrors the
    former Rust normalisation exactly: clip the ``[lo, hi]`` band by
    ``clip_lo/clip_hi``, optional symmetry recentre + reverse, then LUT lookup."""
    span = max(hi - lo, 1e-6)
    band_lo = lo + clip_lo * span
    band_hi = lo + clip_hi * span
    if symmetry:
        mid = 0.5 * (band_lo + band_hi)
        half = 0.5 * (band_hi - band_lo)
        band_lo = mid - half
        band_hi = mid + half
    rng = max(band_hi - band_lo, 1e-6)
    tn = np.clip((strip.astype(np.float64) - band_lo) / rng, 0.0, 1.0)
    if reverse:
        tn = 1.0 - tn
    idx = np.minimum((tn * 255.0).astype(np.int64), 255)
    rgba = np.empty((idx.size, 4), dtype=np.uint8)
    rgba[:, :3] = lut[idx]
    rgba[:, 3] = 255
    return rgba.reshape(-1).view("<f4")


def _colorize_waterfall(meta: view_pb2.ViewMeta, ys: np.ndarray, req: view_pb2.ViewParams) -> None:
    """Replace each waterfall track's amplitude strip in ``ys`` with matplotlib
    RGBA (packed as float32), in place. Runs after the global min/max is fixed so
    every strip shares the same colour scale."""
    lut = _wf_colormap_lut(req.wf_colormap)
    clip_lo = float(req.wf_clip_lo)
    clip_hi = float(req.wf_clip_hi)
    if not clip_hi > clip_lo:
        clip_lo, clip_hi = 0.0, 1.0
    symmetry = bool(req.wf_symmetry)
    reverse = bool(req.wf_reverse)
    for t in meta.tracks:
        if not t.nf or not t.count:
            continue
        off = int(t.offset)
        cnt = int(t.count)
        strip = ys[off : off + cnt]
        if strip.size != cnt:
            continue
        ys[off : off + cnt] = _wf_rgba(
            strip, float(t.ymin), float(t.ymax), clip_lo, clip_hi, symmetry, reverse, lut
        )


def _maybe_log_view_perf(
    mode: str,
    tmin: float,
    tmax: float,
    npx: int,
    track_offset: int,
    track_count: int,
    total_tracks: int,
    wanted_tracks: int,
    sample_tracks: int,
    y_floats: int,
    t0: float,
    t_visible: float,
    t_prepare: float,
    t_loop: float,
    t_scale: float,
    t_concat: float,
    t_done: float,
) -> None:
    global _LAST_VIEW_PERF_LOG_S
    total_ms = (t_done - t0) * 1000.0
    now = time.monotonic()
    if total_ms < _VIEW_PERF_SLOW_MS and now - _LAST_VIEW_PERF_LOG_S < _VIEW_PERF_LOG_INTERVAL_S:
        return
    _LAST_VIEW_PERF_LOG_S = now
    print(
        "[py.view] "
        f"mode={mode} window_s={tmax - tmin:.3f} npx={npx} "
        f"track_page={track_offset}..{track_offset + track_count} "
        f"total_tracks={total_tracks} wanted_tracks={wanted_tracks} "
        f"sample_tracks={sample_tracks} ys_floats={y_floats} "
        f"ys_mb={y_floats * 4.0 / 1_000_000.0:.3f} "
        f"total_ms={total_ms:.2f} visible_ms={(t_visible - t0) * 1000.0:.2f} "
        f"prep_ms={(t_prepare - t_visible) * 1000.0:.2f} "
        f"loop_ms={(t_loop - t_prepare) * 1000.0:.2f} "
        f"scale_ms={(t_scale - t_loop) * 1000.0:.2f} "
        f"concat_ms={(t_concat - t_scale) * 1000.0:.2f}"
    )


def _get_view_data(req: view_pb2.ViewParams) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
    """Compute the requested view (wiggle waveforms or a Qt-style waterfall).

    Returns (ViewMeta, ys). For wiggles `ys` holds raw uniform samples at close
    zooms, or explicit `(t_rel, y)` extrema points for reduced long-window views.
    For the waterfall it holds per-track normalized amplitude strips for the
    requested track page.
    """
    perf_t0 = time.monotonic()
    tmin = float(req.tmin)
    tmax = float(req.tmax)
    visible_tmin = float(req.visible_tmin) if req.HasField("visible_tmin") else tmin
    visible_tmax = float(req.visible_tmax) if req.HasField("visible_tmax") else tmax
    scale_tmin = float(req.scale_tmin) if req.HasField("scale_tmin") else visible_tmin
    scale_tmax = float(req.scale_tmax) if req.HasField("scale_tmax") else visible_tmax
    npx = int(req.npx)
    track_offset = max(0, int(req.track_offset))
    track_count = max(0, int(req.track_count))
    track_end = track_offset + track_count
    hp: Optional[float] = req.highpass if req.HasField("highpass") else None
    lp: Optional[float] = req.lowpass if req.HasField("lowpass") else None
    sort = req.sort or "name"
    subsort = req.subsort or "nslc"
    rotate = float(req.rotate)
    remove_response = bool(req.remove_response)
    mode = req.mode or "wiggle"
    demean = bool(req.demean)
    degap = bool(req.degap)
    allow_ds = bool(req.allow_downsampling)
    fft = bool(req.fft_filtering)
    lphp = bool(req.bandpass_lphp)
    base = req.scaling_base or "minmax"
    scaling_group = req.scaling_group or "individual"
    gain = float(req.gain or 1.0)
    if gain <= 0.0 or not math.isfinite(gain):
        gain = 1.0
    fix_scale_ranges = bool(req.fix_scale_ranges)
    if (
        fix_scale_ranges
        or scale_tmax <= scale_tmin
        or not math.isfinite(scale_tmin)
        or not math.isfinite(scale_tmax)
    ):
        scale_tmin, scale_tmax = visible_tmin, visible_tmax
    show_scale_ranges = bool(req.show_scale_ranges)
    show_scale_axes = bool(req.show_scale_axes)
    wf_abs = bool(req.wf_abs)
    wf_integrate = bool(req.wf_integrate)
    wf_color_scale = req.wf_color_scale or "lin"
    scale_context = _scale_context(req)

    traces = list(_visible_traces())
    perf_visible = time.monotonic()
    if degap:
        try:
            traces = ptrace.degapper(traces)
        except Exception:
            pass
    order = _sort_order(traces, sort, subsort, bool(req.distances_3d))
    work = [traces[i] for i in order]
    work = _apply_rotation(work, rotate)
    work = _remove_response(work, remove_response, hp, lp)
    perf_prepare = time.monotonic()

    meta = view_pb2.ViewMeta(view_tmin=tmin, view_tmax=tmax)
    ys_parts: List[np.ndarray] = []
    range_traces: List[ptrace.Trace] = []
    data_ranges: Dict[Any, Tuple[float, float]] = {}
    fast_individual_minmax = base == "minmax" and scaling_group == "individual"
    offset = 0
    view_idx = 0
    wanted_tracks = 0
    sample_tracks = 0
    for tr_idx, tr in enumerate(work):
        display_tmin, display_tmax = _display_time_span(tr)
        if display_tmax < visible_tmin or display_tmin > visible_tmax:
            continue
        wanted_tracks += 1

        net, sta, loc, cha = tr.nslc_id
        label = ".".join(tr.nslc_id)
        want_samples = track_offset <= view_idx < track_end
        if want_samples:
            sample_tracks += 1
        view_idx += 1
        track_key = (
            tr_idx,
            tuple(tr.nslc_id),
            _float_key(float(tr.tmin)),
            _float_key(float(tr.tmax)),
            _float_key(float(tr.deltat)),
            int(tr.data_len()),
            _float_key(rotate),
            bool(remove_response),
            bool(degap),
        )

        if mode == "waterfall":
            flat = np.zeros(0, dtype="<f4")
            nf = nt = 0
            fmax = 0.0
            vmin, vmax = (-1.0, 1.0) if not wf_abs else (0.0, 1.0)
            t0_rel = max(float(tr.tmin), tmin) - tmin
            dt = float(tr.deltat)
            if want_samples:
                entry = _processed_trace(tr, track_key, tmin, tmax, hp, lp, demean, fft, lphp)
                if entry is not None:
                    sliced = _slice_segment(entry, tmin, tmax, 4.0)
                else:
                    sliced = None
                if sliced is not None:
                    yview, ytmin, deltat, _sample_offset = sliced
                    strip, dt = _waterfall_trace(
                        yview, deltat, npx, wf_abs, wf_integrate, wf_color_scale
                    )
                    if strip.size:
                        flat = strip
                        nf = 1
                        nt = int(strip.size)
                        if wf_abs:
                            vmin = float(strip.min())
                            vmax = float(strip.max())
                        t0_rel = ytmin - tmin
            t = meta.tracks.add()
            t.net, t.sta, t.loc, t.cha = net, sta, loc, cha
            t.label = label
            t.t0_rel = t0_rel
            t.dt = dt
            t.count = int(nf * nt)
            t.offset = int(offset)
            t.ymin = vmin
            t.ymax = vmax
            t.yscale = 1.0
            t.nf = int(nf)
            t.nt = int(nt)
            t.fmax = fmax
            if want_samples and flat.size:
                ys_parts.append(np.ascontiguousarray(flat, dtype="<f4"))
                offset += int(nf * nt)
        else:
            count = 0
            dt = float(tr.deltat)
            t0_rel = max(display_tmin, tmin) - tmin
            payload_len = 0
            need_group_range = scaling_group != "individual"
            if want_samples or need_group_range:
                entry = _processed_trace(tr, track_key, tmin, tmax, hp, lp, demean, fft, lphp)
                if entry is None:
                    if want_samples:
                        continue
                else:
                    data_sliced = _slice_segment(entry, tmin, tmax, 4.0) if want_samples else None
                    scale_sliced = (
                        _slice_segment(entry, scale_tmin, scale_tmax, 0.0)
                        if fast_individual_minmax or need_group_range
                        else None
                    )
                    if data_sliced is None and want_samples:
                        continue
                    if scale_sliced is not None:
                        yrange, yrmin, rdt, _range_offset = scale_sliced
                        if fast_individual_minmax:
                            data_ranges[tr.nslc_id] = (
                                float(np.min(yrange)),
                                float(np.max(yrange)),
                            )
                        else:
                            range_traces.append(_range_trace(tr.nslc_id, yrmin, rdt, yrange))
                    if data_sliced is not None:
                        yview, ytmin, deltat, sample_offset = data_sliced
                        dt = deltat
                        t0_rel = ytmin - tmin
                        target = npx if allow_ds else len(yview)
                        y, count, dt = _decimate(yview, deltat, target, allow_ds, sample_offset)
                        ys_parts.append(np.ascontiguousarray(y, dtype="<f4"))
                        payload_len = int(y.size)
            t = meta.tracks.add()
            t.net, t.sta, t.loc, t.cha = net, sta, loc, cha
            t.label = label
            t.t0_rel = t0_rel
            t.dt = dt
            t.count = int(count)
            t.offset = int(offset)
            t.ymax = 1.0
            t.yscale = 1.0
            if want_samples:
                offset += payload_len

    perf_loop = time.monotonic()
    if mode == "waterfall":
        if wf_abs:
            lo = min((t.ymin for t in meta.tracks if t.nf), default=0.0)
            hi = max((t.ymax for t in meta.tracks if t.nf), default=1.0)
            if hi <= lo:
                hi = lo + 1e-12
            for t in meta.tracks:
                if t.nf:
                    t.ymin = lo
                    t.ymax = hi
    elif range_traces or data_ranges:
        global _OLD_DATA_RANGE_CONTEXT
        if range_traces:
            mode_param, outer_mode = _scaling_base_modes(base)

            def scaling_key(tr: ptrace.Trace) -> Any:
                return _trace_scaling_key(tr, scaling_group)

            trace_minmax = cast(TraceRangeFunc, getattr(ptrace, "minmax"))
            raw_ranges = trace_minmax(
                range_traces,
                key=scaling_key,
                mode=mode_param,
                outer_mode=outer_mode,
            )
            data_ranges.update(
                {key: (float(lo), float(hi)) for key, (lo, hi) in raw_ranges.items()}
            )
        if fix_scale_ranges and _OLD_DATA_RANGE_CONTEXT == scale_context:
            data_ranges.update(_OLD_DATA_RANGES)
        elif not fix_scale_ranges:
            _OLD_DATA_RANGES.clear()
            _OLD_DATA_RANGES.update(data_ranges)
            _OLD_DATA_RANGE_CONTEXT = scale_context

        # ':scaling' overrides land after minmax + fix-range and before make_scale
        # divides by gain, exactly Qt's order (pile_viewer apply_scaling_hooks).
        _apply_scaling_hooks(data_ranges)

        auto_scaler = cast(
            Callable[..., AutoScalerLike], getattr(import_module("pyrocko.plot"), "AutoScaler")
        )
        yscaler = auto_scaler(
            no_exp_interval=(-3, 2),
            approx_ticks=15.0,
            snap=(show_scale_ranges or show_scale_axes) and not show_scale_axes,
        )
        for t in meta.tracks:
            if t.nf:
                continue
            dymin, dymax = data_ranges.get(_track_scaling_key(t, scaling_group), (0.0, 1.0))
            if not math.isfinite(dymin) or not math.isfinite(dymax) or dymax <= dymin:
                dymin, dymax = 0.0, 1.0
            ymin, ymax, _yinc = yscaler.make_scale((dymin / gain, dymax / gain))
            ymin = float(ymin)
            ymax = float(ymax)
            if ymax <= ymin:
                ymax = ymin + 1e-12
            half = max(0.5 * (ymax - ymin), 1e-12)
            t.ymin = ymin
            t.ymax = ymax
            t.ymean = 0.5 * (ymin + ymax)
            t.yscale = max(gain * half, 1e-12)

    perf_scale = time.monotonic()
    ys = (
        np.ascontiguousarray(np.concatenate(ys_parts), dtype="<f4")
        if ys_parts
        else np.zeros(0, dtype="<f4")
    )
    if mode == "waterfall" and ys.size:
        _colorize_waterfall(meta, ys, req)
    perf_concat = time.monotonic()
    _maybe_log_view_perf(
        mode,
        tmin,
        tmax,
        npx,
        track_offset,
        track_count,
        len(work),
        wanted_tracks,
        sample_tracks,
        int(ys.size),
        perf_t0,
        perf_visible,
        perf_prepare,
        perf_loop,
        perf_scale,
        perf_concat,
        time.monotonic(),
    )
    return meta, ys


def _is_stalta_trace(tr: ptrace.Trace) -> bool:
    return bool(tr.nslc_id[3].endswith("-STALTA"))


def _is_tabu_trace(tr: ptrace.Trace) -> bool:
    meta = getattr(tr, "meta", None)
    return bool(isinstance(meta, dict) and meta.get("tabu"))


def _konno_ohmachi_smooth(values: np.ndarray, freqs: np.ndarray, bandwidth: float) -> np.ndarray:
    bandwidth = max(float(bandwidth or 40.0), 1.0)
    values64 = np.asarray(values, dtype=np.float64)
    freqs64 = np.asarray(freqs, dtype=np.float64)
    out = np.empty_like(values64)
    for i, fc in enumerate(freqs64):
        if fc <= 0.0:
            out[i] = values64[i]
            continue
        with np.errstate(divide="ignore", invalid="ignore"):
            x = bandwidth * np.log10(freqs64 / fc)
            w = (np.sin(x) / x) ** 4
        w[~np.isfinite(w)] = 0.0
        w[i] = 1.0
        denom = float(w.sum())
        out[i] = float(np.sum(w * values64) / denom) if denom > 0.0 else values64[i]
    return out


class _FigureEmpty(Exception):
    """Raised by a figure draw fn when there is nothing to plot (no selected
    traces, missing event, ...). render_figure turns it into a FigureResult whose
    ``error`` the frontend shows as text — mirroring how snuffling errors surface
    in the viewer instead of crashing it."""


_MPL_READY = False


def _ensure_mpl() -> Any:
    """Import matplotlib once with the headless Agg backend (never the Pyodide
    HTML5 canvas backend). Returns the matplotlib module."""
    global _MPL_READY
    import matplotlib

    if not _MPL_READY:
        matplotlib.use("Agg")
        _MPL_READY = True
    return matplotlib


def _render_figure_png(
    req: figure_pb2.FigureRequest, draw: Callable[[Any], None]
) -> figure_pb2.FigureResult:
    """Render a matplotlib figure headlessly and return it as PNG bytes.

    This is the faithfulness seam: ``draw`` runs the *exact* pyrocko/matplotlib
    plotting code, so the frontend shows the same figure the Qt snuffler would —
    no hand-rolled egui reimplementation to drift. Sizing follows the egui window
    (device pixels + dpi); ``dark`` picks matplotlib's dark style so the figure
    matches the app theme. A :class:`_FigureEmpty` becomes an ``error`` result."""
    _ensure_mpl()
    import matplotlib.style as mstyle
    from matplotlib.backends.backend_agg import FigureCanvasAgg
    from matplotlib.figure import Figure

    dpi = float(req.dpi) if req.dpi and req.dpi > 0 else 100.0
    w_px = int(req.width_px) or 640
    h_px = int(req.height_px) or 480
    w_in = max(1.0, w_px / dpi)
    h_in = max(1.0, h_px / dpi)
    style = "dark_background" if req.dark else "default"
    buf = io.BytesIO()
    with mstyle.context(style):
        fig = Figure(figsize=(w_in, h_in), dpi=dpi)
        FigureCanvasAgg(fig)
        try:
            draw(fig)
        except _FigureEmpty as exc:
            return figure_pb2.FigureResult(error=str(exc))
        fig.savefig(buf, format="png", dpi=dpi, facecolor=fig.get_facecolor())
    width, height = fig.canvas.get_width_height()
    return figure_pb2.FigureResult(png=buf.getvalue(), width=int(width), height=int(height))


def _figure_work_traces(view: view_pb2.ViewParams) -> List[ptrace.Trace]:
    """The visible traces a figure snuffling operates on: name-sorted, rotated to
    the current component rotation, restricted to the shown track window — the
    same selection the waveform view shows."""
    rotate = float(view.rotate)
    vis = _visible_traces()
    order = sorted(range(len(vis)), key=lambda i: vis[i].nslc_id)
    work = _apply_rotation([vis[i] for i in order], rotate)
    track_offset = max(0, int(view.track_offset))
    track_count = max(0, int(view.track_count) or len(work))
    return work[track_offset : track_offset + track_count]


def _fig_ampspec(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Amplitude spectra of the visible traces — a faithful port of pyrocko's
    ``snufflings/ampspec.py`` ``AmpSpec.call`` plotting: |spectrum| on log-log
    axes, per-trace ``cm.Accent`` colours, optional Konno-Ohmachi smoothing, and
    the same axis labels/legend/title. The FFT (``Trace.spectrum``) and smoothing
    are pyrocko's, so nothing is reimplemented."""
    from matplotlib import cm

    view = req.view
    tmin, tmax = float(view.tmin), float(view.tmax)
    spectrum = req.snufflings.spectrum
    want_smoothing = bool(spectrum.spec_smoothing)
    bandwidth = float(spectrum.spec_smoothing_bandwidth or 40.0)

    segs: List[ptrace.Trace] = []
    for tr in _figure_work_traces(view):
        try:
            seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:  # noqa: BLE001 — skip traces with no data in the window
            continue
        if seg.ydata is None or len(seg.ydata) < 8:
            continue
        seg.ydata = seg.ydata.astype(np.float64)
        segs.append(seg)

    if not segs:
        raise _FigureEmpty("No data in the visible window.")

    axes = fig.add_subplot(111)
    colors = cm.Accent(np.linspace(0.0, 1.0, len(segs)))
    alpha = 0.2 if want_smoothing else 1.0
    flimits: List[float] = []
    alimits: List[float] = []
    for i, seg in enumerate(segs):
        seg.ydata = seg.ydata - seg.ydata.mean()
        freq, spec = seg.spectrum()
        absa = np.abs(spec)
        if freq.size > 2:
            flimits.append(float(freq[1]))
            flimits.append(float(freq[-1]))
        labsa = np.log(absa + 1e-30)
        std = float(np.std(labsa))
        mean = float(np.mean(labsa))
        alimits.append(math.exp(mean - 5.0 * std))
        alimits.append(math.exp(mean + 5.0 * std))
        color = colors[i]
        label = ".".join(seg.nslc_id)
        axes.plot(freq, absa, c=color, alpha=alpha, label=label)
        if want_smoothing:
            smoothed = _konno_ohmachi_smooth(absa, freq, bandwidth)
            axes.plot(freq, np.abs(smoothed), c=color, alpha=1.0, label=label)

    axes.set_xscale("log")
    axes.set_yscale("log")
    if alimits:
        axes.set_ylim(min(alimits), max(alimits))
    if flimits:
        axes.set_xlim(min(flimits), max(flimits))
    axes.set_xlabel("Frequency [Hz]")
    axes.set_ylabel("Counts")
    handles, labels = axes.get_legend_handles_labels()
    leg = dict(zip(labels, handles, strict=True))
    if len(segs) > 1:
        axes.legend(
            list(leg.values()),
            list(leg.keys()),
            loc=2,
            borderaxespad=0.0,
            bbox_to_anchor=(1.05, 1.0),
        )
        fig.subplots_adjust(right=0.8, left=0.1)
    else:
        axes.set_title(list(leg.keys())[0], fontsize=16)
        fig.subplots_adjust(right=0.9, left=0.1)


def _spectro_cmap(name: str) -> Any:
    """The spectrogram snuffling's colour tables, built exactly as
    ``snufflings/spectrogram.py`` does: a desaturated Tango-colour ramp for
    ``spectro``, else any matplotlib colormap via pyrocko's ``mpl_get_cmap``."""
    from matplotlib.colors import LinearSegmentedColormap
    from pyrocko import plot as pplot

    if name == "spectro":
        cols = [
            pplot.tango_colors[x]
            for x in ["skyblue1", "chameleon1", "butter1", "orange1", "scarletred1", "plum3"]
        ]

        def to01(c: Any) -> Tuple[float, float, float]:
            return (c[0] / 255.0, c[1] / 255.0, c[2] / 255.0)

        def desat(c: Tuple[float, float, float], a: float) -> Tuple[float, ...]:
            mean = (c[0] + c[1] + c[2]) / 3.0
            return tuple(cc * a + mean * (1.0 - a) for cc in c)

        return LinearSegmentedColormap.from_list("spectro", [desat(to01(c), 0.8) for c in cols])
    return pplot.mpl_get_cmap(name)


def _fig_spectrogram(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Spectrogram of each visible channel — a faithful port of pyrocko's
    ``snufflings/spectrogram.py``: a windowed STFT (taper + overlap) plotted as a
    ``pcolormesh`` PSD grid with the spectro/rainbow colour table and log/sqrt/lin
    colour scaling. The FFT and PSD normalisation are pyrocko's."""
    p = req.snufflings.spectrogram
    twin = float(p.spectrogram_twin or 100.0)
    overlap = float(p.spectrogram_overlap or 75.0)
    taper_name = p.spectrogram_taper or "Hanning"
    fmin = float(p.spectrogram_fmin) if p.HasField("spectrogram_fmin") else None
    fmax = float(p.spectrogram_fmax) if p.HasField("spectrogram_fmax") else None
    color_scale = p.spectrogram_color_scale or "log"

    view = req.view
    tmin, tmax = float(view.tmin), float(view.tmax)
    work = _figure_work_traces(view)
    if not work:
        raise _FigureEmpty("No visible traces to compute a spectrogram for.")

    tapers = {
        "Hanning": np.hanning,
        "Hamming": np.hamming,
        "Blackman": np.blackman,
        "Bartlett": np.bartlett,
    }
    taper_fn = tapers.get(taper_name, np.hanning)
    cmap = _spectro_cmap(p.spectrogram_color_table or "spectro")

    tpad = twin * overlap / 100.0 * 0.5
    tinc = twin - 2.0 * tpad
    if tinc <= 0.0:
        raise _FigureEmpty("Overlap is too high for the window length.")

    n = len(work)
    ncols = (n - 1) // 5 + 1
    nrows = (n - 1) // ncols + 1
    plotted = 0
    for i, tr in enumerate(work):
        try:
            base = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:  # noqa: BLE001 — skip channels with no data in the window
            continue
        deltat = float(base.deltat)
        nwin = int(math.floor(twin / deltat))
        if nwin < 4 or base.ydata is None or len(base.ydata) < nwin:
            continue
        nfft = 1 << (nwin - 1).bit_length()  # next power of two
        df = 1.0 / (deltat * nfft)
        nf = nfft // 2 + 1
        ifmin = int(math.ceil(fmin / df)) if fmin else 0
        ifmax = min(int(math.floor(fmax / df)) + 1, nf) if fmax else nf
        if ifmax - ifmin < 2:
            continue
        win = taper_fn(nwin)
        wnorm = deltat * 2.0 / (df * float(np.sum(win**2)))
        step = max(1, int(round(tinc / deltat)))
        ydata = base.ydata.astype(np.float64)
        cols: List[np.ndarray] = []
        times: List[float] = []
        start = 0
        while start + nwin <= len(ydata):
            seg = ydata[start : start + nwin].copy()
            seg -= seg.mean()
            seg *= win
            spec = np.abs(np.fft.rfft(seg, n=nfft))[ifmin:ifmax] ** 2 * wnorm
            cols.append(spec)
            times.append(base.tmin + (start + nwin * 0.5) * deltat)
            start += step
        if len(cols) < 2:
            continue
        amps = np.asarray(cols).T  # [frequency, time]
        freqs = np.arange(ifmin, ifmax) * df
        if color_scale == "log":
            amps = np.log(amps + 1e-30)
        elif color_scale == "sqrt":
            amps = np.sqrt(amps)
        mean_a, std_a = float(np.nanmean(amps)), float(np.nanstd(amps))
        zmin = max(float(np.nanmin(amps)), mean_a - 3.0 * std_a)
        zmax = min(float(np.nanmax(amps)), mean_a + 3.0 * std_a)
        icol, irow = i % ncols, i // ncols
        axes = fig.add_subplot(nrows, ncols, irow * ncols + icol + 1)
        axes.pcolormesh(
            np.asarray(times), freqs, amps, cmap=cmap, shading="gouraud", vmin=zmin, vmax=zmax
        )
        axes.set_title(".".join(x for x in tr.nslc_id if x), fontsize=9)
        axes.set_ylabel("Frequency [Hz]")
        plotted += 1

    if plotted == 0:
        raise _FigureEmpty("Not enough data in the window for the chosen window length.")
    fig.tight_layout()


def _pca(data: np.ndarray) -> Tuple[np.ndarray, np.ndarray, np.ndarray, float, float]:
    """Principal-component analysis of an ``(ncomp, nsamples)`` array — a verbatim
    port of ``snufflings/polarization.py`` ``Polarization.pca`` (covariance ->
    ``eigh`` -> azimuth/incidence of the largest eigenvector)."""
    cov = np.cov(data)
    evals, evecs = np.linalg.eigh(cov)
    pc = evecs[:, -1]
    eh = math.hypot(float(pc[0]), float(pc[1]))
    if data.shape[0] > 2:
        incidence = math.degrees(math.atan2(eh, abs(float(pc[2]))))
    else:
        incidence = 90.0
    azimuth = math.degrees(math.atan2(float(pc[1]), float(pc[0])))
    azimuth = (-azimuth) % 180.0 - 90.0
    return cov, evals, evecs, azimuth, incidence


_POL_COLORS = {
    "line": "#204a87",  # sky blue 3 (tango)
    "rot": "#a40000",  # scarlet red 3
    "bazi": "#4e9a06",  # chameleon 3
    "ell": "#75507b",  # plum 2
    "win": (0.9, 0.9, 0.7),
}


def _fig_polarization(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Per-station polarization figure — a faithful port of
    ``snufflings/polarization.py``: three covariance-hodogram projections
    (N-E / R-Z / T-Z) with a moving dot, optional 2D eigensystem ellipses, a
    backazimuth indicator, and the 3-component trace panel. The eigen-analysis
    (``_pca``) and rotation (``trace.rotate``) are pyrocko's; only the interactive
    dynamic axes placement is replaced by a static grid."""
    from matplotlib import patches

    view = req.view
    tmin, tmax = float(view.tmin), float(view.tmax)
    pol = req.snufflings.polarization
    delta_azi = float(pol.polarization_azimuth)
    rotate_to_rt = bool(pol.polarization_rotate_to_rt)
    dot_position = min(1.0, max(0.0, float(pol.polarization_dot_position)))
    show_2d = bool(pol.polarization_show_eigensystem_2d)
    hp = pol.polarization_highpass if pol.HasField("polarization_highpass") else None
    lp = pol.polarization_lowpass if pol.HasField("polarization_lowpass") else None

    groups: Dict[Tuple[str, str, str], Dict[str, ptrace.Trace]] = {}
    for tr in _figure_work_traces(view):
        if tr.channel:
            groups.setdefault(tr.nslc_id[:3], {})[tr.channel[-1].upper()] = tr

    stations: List[Tuple[Tuple[str, str, str], ptrace.Trace, ptrace.Trace, ptrace.Trace]] = []
    for nsl, by in sorted(groups.items()):
        vertical = by.get("Z")
        horiz = next(
            ((by[a], by[b]) for a, b in (("N", "E"), ("1", "2")) if a in by and b in by), None
        )
        if vertical is not None and horiz is not None:
            stations.append((nsl, horiz[0], horiz[1], vertical))

    if not stations:
        raise _FigureEmpty(
            "Polarization needs 3-component stations (Z + 2 horizontals) in the window."
        )

    bazis: Dict[Tuple[str, str, str], float] = {}
    if _ACTIVE_EVENT is not None and _STATIONS:
        from pyrocko import orthodrome as od

        elat, elon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
        for nsl, *_rest in stations:
            st = _STATIONS.get((nsl[0], nsl[1]))
            if st is not None:
                bazis[nsl] = float(od.azimuth(st[0], st[1], elat, elon))

    def prep(tr: ptrace.Trace) -> Optional[np.ndarray]:
        try:
            seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:  # noqa: BLE001 — no data in the window
            return None
        if seg.ydata is None or len(seg.ydata) < 3:
            return None
        seg.ydata = seg.ydata.astype(np.float64)
        _filter_seg(seg, hp, lp, False, False)
        return seg.ydata

    nst = len(stations)
    drawn = 0
    for i, (nsl, tn, te, tz) in enumerate(stations):
        yn, ye, yz = prep(tn), prep(te), prep(tz)
        if yn is None or ye is None or yz is None:
            continue
        ns = min(len(yn), len(ye), len(yz))
        if ns < 3:
            continue
        yn, ye, yz = yn[:ns], ye[:ns], yz[:ns]

        bazimuth = delta_azi
        if rotate_to_rt and nsl in bazis:
            bazimuth += bazis[nsl] + 90.0
        if bazimuth != 0.0:
            ca, sa = math.cos(math.radians(bazimuth)), math.sin(math.radians(bazimuth))
            yr, yt = ca * yn + sa * ye, -sa * yn + ca * ye
        else:
            yr, yt = yn, ye

        amax = float(np.sqrt(yn**2 + ye**2 + yz**2).max()) or 1.0
        ipos = int(round(dot_position * (ns - 1)))
        line, rot = _POL_COLORS["line"], _POL_COLORS["rot"]

        ax01 = fig.add_subplot(nst, 4, i * 4 + 1)
        ax02 = fig.add_subplot(nst, 4, i * 4 + 2)
        ax12 = fig.add_subplot(nst, 4, i * 4 + 3)
        axtr = fig.add_subplot(nst, 4, i * 4 + 4)

        for axes, xdata, ydata, xlab, ylab in (
            (ax01, ye, yn, te.channel[-1], tn.channel[-1]),
            (ax02, yr, yz, "R", "Z"),
            (ax12, yt, yz, "T", "Z"),
        ):
            axes.set_aspect(1.0)
            axes.plot(xdata, ydata, color=line, lw=1.0)
            axes.plot([xdata[ipos]], [ydata[ipos]], "o", ms=4, color=line)
            axes.set_xlim(-amax * 1.05, amax * 1.05)
            axes.set_ylim(-amax * 1.05, amax * 1.05)
            axes.set_xlabel(xlab)
            axes.set_ylabel(ylab)
            axes.set_xticks([])
            axes.set_yticks([])
            if show_2d and xdata.size >= 3:
                _cov, evals, evecs, _azi, _inc = _pca(np.vstack([xdata, ydata]))
                axes.add_patch(
                    patches.Ellipse(
                        (0.0, 0.0),
                        width=math.sqrt(abs(float(evals[0]))) * 2.0,
                        height=math.sqrt(abs(float(evals[1]))) * 2.0,
                        angle=math.degrees(math.atan2(float(evecs[1, 0]), float(evecs[0, 0]))),
                        zorder=-10,
                        fc=_POL_COLORS["ell"],
                        ec=_POL_COLORS["ell"],
                        alpha=0.3,
                    )
                )

        if nsl in bazis:
            eb = bazis[nsl]
            ax01.plot(
                [0.0, amax * math.cos(math.radians(90.0 - eb))],
                [0.0, amax * math.sin(math.radians(90.0 - eb))],
                "--",
                lw=3,
                zorder=-20,
                color=_POL_COLORS["bazi"],
            )

        deltat = float(tz.deltat)
        times = np.arange(ns) * deltat
        for itr, ydata in enumerate((yr, yt, yz)):
            col = rot if (itr < 2 and bazimuth != 0.0) else line
            axtr.plot(times, ydata / (2.0 * amax) + itr, color=col, lw=1.0)
            axtr.plot([times[ipos]], [ydata[ipos] / (2.0 * amax) + itr], "o", ms=4, color=col)
        axtr.axvspan(times[0], times[-1], color=_POL_COLORS["win"], zorder=-20)
        axtr.set_ylim(-1, 3)
        axtr.set_yticks([0, 1, 2])
        axtr.set_yticklabels(["R", "T", "Z"])
        axtr.set_xlabel("Time [s]")
        ax01.set_title(".".join(x for x in nsl if x) or nsl[1])
        drawn += 1

    if drawn == 0:
        raise _FigureEmpty("No 3-component data in the window.")
    fig.tight_layout()


def _fig_cake_model(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Cake 'Plot Model' — pyrocko's *exact* matplotlib model figure (P/S velocity
    vs depth over the shaded layer cross-section). Reuses
    ``pyrocko.plot.cake_plot.my_model_plot`` (pure matplotlib, no Qt) instead of
    re-drawing it in Rust, so it can never drift from the Qt snuffler."""
    from pyrocko import cake
    from pyrocko.plot import cake_plot

    model, _phases, _tshift, _use_station_depth, _mode = _cake_section(req)
    mod = _cake_load_model(cake, model)
    cake_plot.my_model_plot(mod, fig=fig)


def _fig_cake_rays(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Cake 'Plot Rays' — pyrocko's *exact* ray-path figure (ray fan + source dot +
    receiver triangles over the shaded model). Reuses
    ``pyrocko.plot.cake_plot.my_rays_plot``; all geometry is cake's ray tracer."""
    from pyrocko import cake
    from pyrocko.plot import cake_plot

    model, phase_names, _tshift, use_station_depth, _mode = _cake_section(req)
    if _ACTIVE_EVENT is None or not _STATIONS:
        raise _FigureEmpty(
            "Cake ray paths need an active event and station coordinates.\n"
            "Search a catalog event and fetch station coordinates first."
        )
    wanted, _errors = _cake_wanted_phases(cake, phase_names)
    if not wanted:
        raise _FigureEmpty("No phases selected.")

    mod = _cake_load_model(cake, model)
    _elat, _elon, edepth, _etime = _ACTIVE_EVENT
    allrays: List[Any] = []
    distances: List[float] = []
    zstop = 0.0
    for _net, _sta, dist_deg, elev in _cake_stations():
        rdepth = max(0.0, -float(elev)) if use_station_depth else 0.0
        allrays.extend(
            mod.arrivals(phases=wanted, distances=[dist_deg], zstart=edepth, zstop=rdepth)
        )
        distances.append(float(dist_deg))
        zstop = rdepth
    if not allrays:
        raise _FigureEmpty("No ray arrivals for the selected phases and distances.")

    # zstart/zstop are depths in metres; distances are epicentral degrees — the
    # units my_rays_plot's axis scaling (labels_rays) expects.
    cake_plot.my_rays_plot(mod, None, allrays, edepth, zstop, distances=distances, fig=fig)


def _fig_beachball(fig: Any, req: figure_pb2.FigureRequest) -> None:
    """Focal-mechanism beachball for a catalog event's moment tensor, drawn by
    pyrocko's own matplotlib beachball (deviatoric, scarlet-red compressional
    quadrants, like the Qt snuffler marker editor) — never re-derived in Rust."""
    from pyrocko import moment_tensor as mtm
    from pyrocko.plot import tango_colors
    from pyrocko.plot.beachball import plot_beachball_mpl

    m = req.mt
    mt = mtm.MomentTensor(mnn=m.mnn, mee=m.mee, mdd=m.mdd, mne=m.mne, mnd=m.mnd, med=m.med)
    axes = fig.add_subplot(1, 1, 1, aspect=1.0)
    axes.set_axis_off()
    axes.set_xlim(-1.1, 1.1)
    axes.set_ylim(-1.1, 1.1)
    color_t = tuple(c / 255.0 for c in tango_colors["scarletred3"])
    plot_beachball_mpl(
        mt,
        axes,
        beachball_type="deviatoric",
        size=2.0,
        size_units="data",
        position=(0.0, 0.0),
        color_t=color_t,
        color_p="white",
        edgecolor="white" if req.dark else "black",
        linewidth=1.5,
    )
    strike, dip, rake = mt.both_strike_dip_rake()[0]
    axes.set_title("strike %.0f°  dip %.0f°  rake %.0f°" % (strike, dip, rake))


_FIGURES: Dict[str, Callable[[Any, snuffling_pb2.SnufflingRequestParams], None]] = {
    "ampspec": _fig_ampspec,
    "spectrogram": _fig_spectrogram,
    "polarization": _fig_polarization,
    "cake_model": _fig_cake_model,
    "cake_rays": _fig_cake_rays,
}


def _render_figure(req: figure_pb2.FigureRequest) -> figure_pb2.FigureResult:
    """Dispatch a FigureRequest to the matching pyrocko/matplotlib figure. Any
    error (bad params, no data, plotting failure) becomes an ``error`` result the
    frontend shows as text, so a broken figure never takes down the viewer."""
    if req.kind == "beachball":

        def draw(fig: Any) -> None:
            _fig_beachball(fig, req)

    else:
        draw_kind = _FIGURES.get(req.kind)
        if draw_kind is None:
            return figure_pb2.FigureResult(error="unknown figure: %s" % req.kind)

        def draw(fig: Any) -> None:
            draw_kind(fig, req.params)

    try:
        return _render_figure_png(req, draw)
    except Exception as exc:  # noqa: BLE001 — surface as text, never raise across the worker
        return figure_pb2.FigureResult(
            error="%s: %s\n\n%s" % (type(exc).__name__, exc, traceback.format_exc())
        )


async def _load_gf_store_url(base_url: str) -> bridge_pb2.StringValue:
    """Mount a Green's-functions store by fetching its files from an HTTP base URL
    into the Pyodide filesystem (no File-System-Access-API directory picker). The
    base must serve a ``.manifest`` (newline-separated relative file paths). Then
    register the store like a directory mount and return its "GF Store: <id>" label."""
    from pyodide.http import pyfetch

    base = base_url.rstrip("/")
    manifest = await pyfetch(base + "/.manifest")
    if not manifest.ok:
        raise RuntimeError("no .manifest at %s (HTTP %s)" % (base, manifest.status))
    files = [line.strip() for line in (await manifest.string()).splitlines() if line.strip()]
    root = "/gfstores/store%d" % len(_GF_STORES)
    os.makedirs(root, exist_ok=True)
    for rel in files:
        resp = await pyfetch(base + "/" + rel)
        if not resp.ok:
            raise RuntimeError("fetch %s: HTTP %s" % (rel, resp.status))
        dest = "%s/%s" % (root, rel)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        with open(dest, "wb") as handle:
            handle.write(bytes(await resp.bytes()))
    return bridge_pb2.StringValue(value=_gf_store_info_result(root).label)


def _ensure_real_gf() -> Any:
    """Import the FULL ``pyrocko.gf`` (LocalEngine/DCSource/Target/Request), first
    evicting the lightweight stub that :func:`_gf_modules` installs for the cake
    GF-store path — that stub has only ``gf.store``/``gf.meta`` and would make the
    seismosizer fail. Needs ``ensureGf`` (requests + pyodide-http), which the
    GF-store mount already installs."""
    import importlib
    import sys

    existing = sys.modules.get("pyrocko.gf")
    if existing is not None and getattr(existing, "_gf_stub", False):
        for key in [k for k in sys.modules if k == "pyrocko.gf" or k.startswith("pyrocko.gf.")]:
            del sys.modules[key]
    return importlib.import_module("pyrocko.gf")


def _seismosizer_run(req: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Synthetic seismograms from a double-couple point source via
    ``pyrocko.gf.LocalEngine`` and a mounted Green's-functions store — a faithful
    port of ``snufflings/seismosizer.py`` (DCSource). Adds the synthetic traces to
    the pile and a source event marker. Needs a mounted GF store + `ensureGf`."""
    p = req.snufflings.seismosizer
    store_label = p.seis_store or ""
    if not store_label.startswith(GF_STORE_PREFIX):
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="Select a mounted GF store first (use “Add GF Store…”)."
        )
    sid = store_label[len(GF_STORE_PREFIX) :]
    store_dir = _GF_STORES.get(sid)
    if store_dir is None:
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="GF store %r is not mounted." % sid
        )

    gf = _ensure_real_gf()
    from pyrocko import model

    km = 1000.0
    if _ACTIVE_EVENT is not None:
        elat, elon, _edep, etime = _ACTIVE_EVENT
    else:
        elat, elon, etime = 0.0, 0.0, 0.0

    stf = gf.HalfSinusoidSTF(duration=float(p.seis_stf_duration)) if p.seis_stf_duration else None
    source = gf.DCSource(
        time=etime + float(p.seis_time),
        lat=elat,
        lon=elon,
        north_shift=float(p.seis_north_km) * km,
        east_shift=float(p.seis_east_km) * km,
        depth=float(p.seis_depth_km) * km,
        magnitude=float(p.seis_magnitude),
        strike=float(p.seis_strike),
        dip=float(p.seis_dip),
        rake=float(p.seis_rake),
        stf=stf,
    )
    source.regularize()

    stations = []
    if _STATIONS:
        for (net, sta), (slat, slon, sele) in _STATIONS.items():
            stations.append(
                model.Station(network=net, station=sta, lat=slat, lon=slon, elevation=sele)
            )
    else:  # snuffler's fallback: two symmetric synthetic stations
        for lat, lon in [(5.0, 0.0), (-5.0, 0.0)]:
            stations.append(
                model.Station(network="SY", station="%g_%g" % (lat, lon), lat=lat, lon=lon)
            )

    targets = [
        gf.Target(
            codes=(st.network, st.station, "syn", cha),
            quantity="displacement",
            lat=st.lat,
            lon=st.lon,
            depth=st.depth,
            store_id=sid,
            interpolation="nearest_neighbor",
            optimization="enable",
        )
        for st in stations
        for cha in "NEZ"
    ]

    engine = gf.LocalEngine(store_dirs=[store_dir])
    # Pyodide's mounted (NativeFS/OPFS) filesystem does not support mmap, so open
    # the store without memmap (also fine, just slightly slower, on native builds).
    engine._open_stores[sid] = gf.store.Store(store_dir, use_memmap=False)  # noqa: SLF001
    request = gf.Request(sources=[source], targets=targets)
    request.regularize()
    traces = engine.process(request, nthreads=0).pyrocko_traces()

    waveform_type = p.seis_waveform_type or "Displacement [m]"
    if waveform_type.startswith("Velocity"):
        for tr in traces:
            tr.set_ydata(np.diff(tr.ydata) / tr.deltat)
    elif waveform_type.startswith("Acceleration"):
        for tr in traces:
            tr.set_ydata(np.diff(np.diff(tr.ydata)) / tr.deltat**2)
    if waveform_type.endswith(("[nm]", "[nm/s]", "[nm/s^2]")):
        for tr in traces:
            tr.set_ydata(tr.ydata * 1e9)

    _set_traces(list(_TRACES) + traces)
    result = snuffling_pb2.SnufflingResult(show_window=True, scene=_scene_meta())
    result.text = "%d synthetic trace(s) from a DC source (M%.1f)." % (
        len(traces),
        float(p.seis_magnitude),
    )
    marker = result.markers.add()
    marker.t = float(source.time)
    marker.is_event = True
    marker.phase = "synthetic DC M%.1f" % float(p.seis_magnitude)
    return result


# Resolved SeisBench ONNX model URLs, keyed by "Model/weights/base". The actual
# ORT InferenceSession is cached in the JS worker because that is where WebGPU
# lives; Python caches only the manifest lookup and URL resolution.
_SEISBENCH_MODEL_URLS: Dict[str, str] = {}


def _join_url(base_url: str, filename: str) -> str:
    return base_url.rstrip("/") + "/" + filename


async def _ensure_seisbench_model(
    model: str, weights: str, base_url: str
) -> Tuple[str, Dict[str, Any]]:
    """Resolve and cache the chosen exported ONNX model URL."""
    import seisbench_infer as sbi

    manifest = await sbi.load_manifest()
    spec = sbi.model_spec(manifest, model, weights)
    filename = str(spec["file"])
    base = base_url.strip()
    if not base:
        raise RuntimeError(
            "SeisBench ONNX model base URL is not configured; use the default "
            "embed-data build or set SNUFFLER_ONNX_MODEL_BASE_URL for lean builds"
        )
    cache_key = "%s/%s/%s" % (model, weights, base)
    if cache_key not in _SEISBENCH_MODEL_URLS:
        if base.startswith(("http://", "https://")):
            url = _local_cors_proxy_url(_join_url(base, filename))
        else:
            url = _asset_url(_join_url(base, filename))
        _SEISBENCH_MODEL_URLS[cache_key] = url
    return _SEISBENCH_MODEL_URLS[cache_key], spec


def _apply_seisbench_filter(seg: ptrace.Trace, spec: Dict[str, Any]) -> None:
    args = spec.get("filter_args")
    kwargs = spec.get("filter_kwargs") or {}
    if not args:
        return
    kind = str(args[0] if isinstance(args, list) else args)
    order = int(kwargs.get("corners", 4))
    try:
        if kind == "highpass":
            seg.highpass(order, float(kwargs["freq"]), nyquist_exception=True)
        elif kind == "lowpass":
            seg.lowpass(order, float(kwargs["freq"]), nyquist_exception=True)
        elif kind == "bandpass":
            seg.bandpass(order, float(kwargs["freqmin"]), float(kwargs["freqmax"]))
    except Exception:
        pass


def _seisbench_groups(
    traces: List[ptrace.Trace],
    tmin: float,
    tmax: float,
    spec: Dict[str, Any],
) -> "OrderedDict[Tuple[str, str, str], Tuple[float, np.ndarray, NslcId]]":
    """Assemble per-(net,sta,loc) component arrays at the model sampling rate.

    Mirrors seisbench's stream_to_array: a zero-filled (3, N) grid with each
    component written at its sample offset (Z->0, N/1->1, E/2->2), missing
    components left as zeros. Returns (group_tmin, data, representative_nslc).
    """
    sampling_rate = float(spec["sampling_rate"])
    component_order = str(spec["component_order"])
    comp_row = {comp: i for i, comp in enumerate(component_order)}
    if "N" in comp_row:
        comp_row.setdefault("1", comp_row["N"])
    if "E" in comp_row:
        comp_row.setdefault("2", comp_row["E"])
    dt = 1.0 / sampling_rate
    staged: "OrderedDict[Tuple[str, str, str], Dict[int, Tuple[float, np.ndarray]]]" = OrderedDict()
    rep: Dict[Tuple[str, str, str], NslcId] = {}
    for tr in traces:
        net, sta, loc, cha = tr.nslc_id
        if not cha:
            continue
        row = comp_row.get(cha[-1].upper())
        if row is None:
            continue
        try:
            seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:
            continue
        if seg.ydata is None or len(seg.ydata) < 2:
            continue
        seg.ydata = seg.ydata.astype(np.float64)
        if abs(seg.deltat - dt) > 1e-9:
            try:
                seg.resample(dt)
            except Exception:
                continue
        _apply_seisbench_filter(seg, spec)
        key = (net, sta, loc)
        staged.setdefault(key, {})[row] = (
            float(seg.tmin),
            np.asarray(seg.ydata, dtype=np.float32),
        )
        if row == 0 or key not in rep:
            rep[key] = (net, sta, loc, cha)  # prefer the vertical channel as the label

    out: "OrderedDict[Tuple[str, str, str], Tuple[float, np.ndarray, NslcId]]" = OrderedDict()
    for key, comps in staged.items():
        g_tmin = min(t for t, _ in comps.values())
        g_tmax = max(t + (len(y) - 1) * dt for t, y in comps.values())
        n = int(round((g_tmax - g_tmin) * sampling_rate)) + 1
        if n < 4:
            continue
        data = np.zeros((len(component_order), n), dtype=np.float32)
        for row, (t0, y) in comps.items():
            off = int(round((t0 - g_tmin) * sampling_rate))
            if off < 0:
                y = y[-off:]
                off = 0
            m = min(len(y), n - off)
            if m > 0:
                data[row, off : off + m] = y[:m]
        out[key] = (g_tmin, data, rep[key])
    return out


async def _seisbench_pick_result(
    req: snuffling_pb2.SnufflingRequestParams,
) -> snuffling_pb2.SnufflingResult:
    """SeisBench deep-learning P/S picker over the visible window.

    The ONNX forward pass runs in the JS worker through onnxruntime-web; numpy
    pre/post-processing mirrors the pinned SeisBench 0.11.7 source.
    """
    import seisbench_infer as sbi

    params = req.snufflings.seisbench
    model = params.model or "PhaseNet"
    weights = params.weights or "instance"
    progress_prefix = "SeisBench %s/%s" % (model, weights)
    _seisbench_progress(0.0, "%s · loading model" % progress_prefix)
    model_url, spec = await _ensure_seisbench_model(model, weights, params.model_base_url or "")
    view = req.view
    component_order = str(spec["component_order"])
    _seisbench_progress(0.01, "%s · preparing waveform chunks" % progress_prefix)
    groups = _seisbench_groups(_visible_traces(), float(view.tmin), float(view.tmax), spec)
    result = snuffling_pb2.SnufflingResult(show_window=True)
    n_p = n_s = 0
    total_batches = max(
        1, sum(sbi.batch_count(data, spec) for _g_tmin, data, _nslc in groups.values())
    )
    completed_batches = 0

    def batch_done() -> None:
        nonlocal completed_batches
        completed_batches += 1
        frac = min(0.99, 0.20 + 0.79 * completed_batches / total_batches)
        _seisbench_progress(frac, "%s · running inference" % progress_prefix)

    for g_tmin, data, nslc in groups.values():
        ann = await sbi.annotate(model_url, data, spec, batch_done, progress_prefix)
        thresholds = {
            "P": params.p_threshold
            if params.HasField("p_threshold")
            else sbi.default_threshold(spec, "P"),
            "S": params.s_threshold
            if params.HasField("s_threshold")
            else sbi.default_threshold(spec, "S"),
        }
        for phase, kind in (("P", 1), ("S", 2)):
            if phase not in sbi.labels(spec):
                continue
            thr = float(thresholds[phase])
            for idx, _val in sbi.extract_picks(ann.data[:, sbi.phase_channel(spec, phase)], thr):
                marker = result.markers.add()
                marker.t = g_tmin + ann.start_offset + idx / ann.sampling_rate
                marker.kind = kind
                marker.phase = phase
                marker.nslc.CopyFrom(_nslc_pb(list(nslc)))
                n_p, n_s = (n_p + 1, n_s) if phase == "P" else (n_p, n_s + 1)
    if not groups:
        result.text = "SeisBench picker: no %s data in the visible window." % component_order
    else:
        result.text = "SeisBench %s/%s: %d P and %d S pick(s) across %d station(s)." % (
            model,
            weights,
            n_p,
            n_s,
            len(groups),
        )
    _seisbench_progress(1.0, "%s · complete" % progress_prefix)
    return result


def _run_snuffling_result(
    name: str, req: snuffling_pb2.SnufflingRequestParams
) -> snuffling_pb2.SnufflingResult:
    """Run a built-in snuffling over the current window."""
    view = req.view
    tmin, tmax = float(view.tmin), float(view.tmax)
    hp = view.highpass if view.HasField("highpass") else None
    lp = view.lowpass if view.HasField("lowpass") else None
    rotate = float(view.rotate)
    fft = bool(view.fft_filtering)
    lphp = bool(view.bandpass_lphp)

    if name == "cake":
        return _cake_run(req)

    if name == "seismosizer":
        return _seismosizer_run(req)

    vis = _visible_traces()
    order = _sort_order(vis, view.sort or "name", view.subsort or "nslc", bool(view.distances_3d))
    work = _apply_rotation([vis[i] for i in order], rotate)

    def chopped(
        pass_hp: Optional[float] = hp,
        pass_lp: Optional[float] = lp,
        ctmin: float = tmin,
        ctmax: float = tmax,
        traces: Optional[List[ptrace.Trace]] = None,
    ) -> Iterator[Tuple[ptrace.Trace, ptrace.Trace]]:
        for tr in work if traces is None else traces:
            try:
                seg = tr.chop(ctmin, ctmax, inplace=False, include_last=True)
            except Exception:
                continue
            if seg.ydata is None or len(seg.ydata) < 4:
                continue
            seg.ydata = seg.ydata.astype(np.float64)
            _filter_seg(seg, pass_hp, pass_lp, fft, lphp)
            yield tr, seg

    if name == "minmax":
        lines = ["%-16s %13s %13s %13s" % ("channel", "min", "max", "peak-to-peak")]
        for tr, seg in chopped():
            y = seg.ydata
            lines.append(
                "%-16s %+.4e %+.4e %.4e"
                % (".".join(tr.nslc_id), float(y.min()), float(y.max()), float(y.max() - y.min()))
            )
        text = "\n".join(lines) if len(lines) > 1 else "No data in the visible window."
        return snuffling_pb2.SnufflingResult(show_window=True, text=text)

    if name == "stalta":
        stalta = req.snufflings.stalta
        shp = stalta.stalta_highpass if stalta.HasField("stalta_highpass") else None
        slp = stalta.stalta_lowpass if stalta.HasField("stalta_lowpass") else None
        tshort = float(stalta.stalta_swin or 30.0)
        ratio = float(stalta.stalta_ratio or 3.0)
        level = float(stalta.stalta_level or 0.5)
        variant = stalta.stalta_variant or "centered"
        scaling_name = stalta.stalta_scalingmethod or "[0-1]"
        apply_to_sum = bool(stalta.stalta_apply_to_sum)
        show_level_traces = bool(stalta.stalta_show_level_traces)
        scaling_method = {
            "[0-1]": 1,
            "[-1/ratio,1]": 2,
            "[-1/ratio,1] clipped to [0,1]": 3,
        }.get(scaling_name, 1)
        result = snuffling_pb2.SnufflingResult(show_window=False)
        processed = []
        derived = []
        source_work = [tr for tr in work if not _is_stalta_trace(tr)]
        had_level_traces = any(_is_stalta_trace(tr) for tr in _TRACES)
        if had_level_traces:
            _TRACES[:] = [tr for tr in _TRACES if not _is_stalta_trace(tr)]
        for tr, seg in chopped(shp, slp, traces=source_work):
            if len(seg.ydata) < 64:
                continue
            try:
                c = seg.copy()
                method = c.sta_lta_right if variant == "right" else c.sta_lta_centered
                try:
                    method(tshort, tshort * ratio, scalingmethod=scaling_method)
                except TypeError:
                    method(tshort, tshort * ratio)
                if show_level_traces:
                    net, sta, loc, cha = tr.nslc_id
                    derived.append(
                        ptrace.Trace(
                            network=net,
                            station=sta,
                            location=loc,
                            channel=cha + "-STALTA",
                            deltat=float(c.deltat),
                            tmin=float(c.tmin),
                            ydata=np.asarray(c.ydata, dtype=np.float64),
                            meta={"tabu": True},
                        )
                    )
                if apply_to_sum:
                    processed.append(c)
                    continue
                tpeaks, _apeaks = c.peaks(level, tshort)
            except Exception:
                continue
            for tp in tpeaks:
                marker = result.markers.add()
                marker.t = float(tp)
                marker.nslc.CopyFrom(_nslc_pb(list(tr.nslc_id)))
        if apply_to_sum and processed:
            min_len = min(len(c.ydata) for c in processed)
            if min_len >= 64:
                try:
                    csum = processed[0].copy()
                    csum.ydata = np.mean([c.ydata[:min_len] for c in processed], axis=0)
                    tpeaks, _apeaks = csum.peaks(level, tshort)
                    for tp in tpeaks:
                        result.markers.add().t = float(tp)
                except Exception:
                    pass
        if show_level_traces:
            _TRACES.extend(derived)
        if had_level_traces or show_level_traces:
            _mark_traces_changed()
            result.scene.CopyFrom(_scene_meta())
        result.text = "STA/LTA: %d detection(s) (short=%.3gs, ratio=%g, level=%g)" % (
            len(result.markers),
            tshort,
            ratio,
            level,
        )
        return result

    if name == "polarization":
        # Covariance-eigenvalue polarization analysis (Jurkevics, 1988) per
        # 3-component station over the visible window. Pure numpy.
        polarization = req.snufflings.polarization
        php = (
            polarization.polarization_highpass
            if polarization.HasField("polarization_highpass")
            else None
        )
        plp = (
            polarization.polarization_lowpass
            if polarization.HasField("polarization_lowpass")
            else None
        )
        length = float(polarization.polarization_t_length or (tmax - tmin))
        offset = float(polarization.polarization_ft_offset)
        azimuth = float(polarization.polarization_azimuth)
        if 0.0 < length < (tmax - tmin):
            center = 0.5 * (tmin + tmax) + offset * length
            ptmin, ptmax = center - 0.5 * length, center + 0.5 * length
        else:
            ptmin, ptmax = tmin, tmax

        groups: Dict[Tuple[str, str, str], Dict[str, np.ndarray]] = {}
        comp_of = {"Z": "Z", "N": "N", "1": "N", "E": "E", "2": "E"}
        for tr, seg in chopped(php, plp, ptmin, ptmax):
            net, sta, loc, cha = tr.nslc_id
            comp = comp_of.get(cha[-1].upper()) if cha else None
            if comp is not None:
                groups.setdefault((net, sta, loc), {})[comp] = seg.ydata

        lines = ["%-16s %8s %8s %6s %6s" % ("station", "az[°]", "inc[°]", "rect", "plan")]
        for (net, sta, loc), comps in groups.items():
            if not all(k in comps for k in ("Z", "N", "E")):
                continue
            m = min(len(comps[k]) for k in ("Z", "N", "E"))
            if m < 8:
                continue
            if azimuth != 0.0:
                theta = np.radians(azimuth)
                n = comps["N"][:m].copy()
                e = comps["E"][:m].copy()
                comps["N"] = np.cos(theta) * n - np.sin(theta) * e
                comps["E"] = np.sin(theta) * n + np.cos(theta) * e
            data = np.vstack([comps["Z"][:m], comps["N"][:m], comps["E"][:m]])
            data = data - data.mean(axis=1, keepdims=True)
            evals, evecs = np.linalg.eigh(np.cov(data))  # ascending
            order_ev = np.argsort(evals)[::-1]
            l1, l2, l3 = (float(evals[i]) for i in order_ev)
            vz, vn, ve = evecs[:, order_ev[0]]
            rect = 1.0 - (l2 + l3) / (2.0 * l1) if l1 > 0.0 else 0.0
            plan = 1.0 - 2.0 * l3 / (l1 + l2) if (l1 + l2) > 0.0 else 0.0
            az = float(np.degrees(np.arctan2(ve, vn))) % 360.0
            inc = float(np.degrees(np.arccos(min(1.0, abs(float(vz))))))
            lines.append(
                "%-16s %8.1f %8.1f %6.2f %6.2f"
                % ("%s.%s.%s" % (net, sta, loc), az, inc, rect, plan)
            )

        text = (
            "\n".join(lines)
            if len(lines) > 1
            else "Need 3-component (Z/N/E) data in the visible window."
        )
        return snuffling_pb2.SnufflingResult(show_window=True, text=text)

    return snuffling_pb2.SnufflingResult(show_window=True, text="Unknown snuffling: %s" % name)


# --- FDSN web services (network) -------------------------------------------
# These helpers build the FDSN request URLs and parse the responses. The fetch
# runs in Python too (pyfetch, in the async servicer methods above), so the whole
# workflow is one rpc and the worker stays a dumb relay.


def _dataselect_url(req: data_pb2.WaveformDownloadRequest) -> str:
    """FDSN dataselect (waveform) query URL."""
    base = _fdsn_base(req.datacenter)
    q = "&".join(
        [
            "net=%s" % (req.net or "*"),
            "sta=%s" % (req.sta or "*"),
            "loc=%s" % (req.loc or "*"),
            "cha=%s" % (req.cha or "*"),
            "starttime=%s" % req.start,
            "endtime=%s" % req.end,
            "nodata=204",
        ]
    )
    return "%s/fdsnws/dataselect/1/query?%s" % (base, q)


def _load_waveform_path_scene(path: str) -> scene_pb2.SceneMeta:
    """Load staged FDSN MiniSEED bytes into the pile and return the scene."""
    if not path:
        return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
    traces = list(pio.load(path, format="mseed"))
    _set_traces(traces)
    return _scene_meta()


def _chop_trace_from(tr: ptrace.Trace, tmin: float) -> Optional[ptrace.Trace]:
    if not math.isfinite(tmin) or tmin <= 0.0 or tr.tmin >= tmin:
        return tr
    if tr.tmax < tmin:
        return None
    try:
        return tr.chop(tmin, tr.tmax, inplace=False, include_last=True)
    except ptrace.NoData:
        return None


def _trim_live_traces(traces: List[ptrace.Trace], tmin: float) -> List[ptrace.Trace]:
    out = []
    for tr in traces:
        chopped = _chop_trace_from(tr, tmin)
        if chopped is not None:
            out.append(chopped)
    return out


def _append_mseed_scene(
    source: str,
    raw: bytes,
    start_time: float = 0.0,
    reset: bool = False,
    retain_seconds: float = 0.0,
) -> scene_pb2.SceneMeta:
    """Append live MiniSEED records and merge adjacent segments by NSLC/time."""
    del source
    global _TRACES
    if reset:
        _TRACES = []
        _SOURCE_TRACES.clear()
        _clear_event_context()
    if not raw:
        if reset:
            _mark_traces_changed()
        return _scene_meta()

    traces = list(pio.load(_stage_bytes("live.mseed", raw), format="mseed"))
    if start_time > 0.0:
        traces = _trim_live_traces(traces, start_time)
    if not traces:
        if reset:
            _mark_traces_changed()
        return _scene_meta()

    merged_input = list(_TRACES) + traces
    if retain_seconds > 0.0 and merged_input:
        latest = max(tr.tmax for tr in merged_input)
        retain_from = latest - retain_seconds
        if start_time > 0.0:
            retain_from = max(start_time, retain_from)
        merged_input = _trim_live_traces(merged_input, retain_from)
    elif start_time > 0.0:
        merged_input = _trim_live_traces(merged_input, start_time)

    merged = sorted(
        merged_input,
        key=lambda tr: (
            tr.network,
            tr.station,
            tr.location,
            tr.channel,
            tr.tmin,
        ),
    )
    _TRACES = ptrace.degapper(merged)
    _mark_traces_changed()
    return _scene_meta()


def _event_url(req: data_pb2.CatalogRequest) -> str:
    """FDSN event (catalog) query URL. Text format by default; full QuakeML with
    all origins (so focal-mechanism moment tensors are included) when the request
    asks for MTs — this is the browser-feasible MT source (GlobalCMT/GEOFON-mt are
    blocked by mixed-content/CORS)."""
    base = _fdsn_base(req.datacenter)
    if req.with_mt:
        q = "&".join(
            [
                "format=xml",
                "starttime=%s" % req.start,
                "endtime=%s" % req.end,
                "minmagnitude=%s" % req.minmag,
                "includeallorigins=true",
                "orderby=time",
            ]
        )
    else:
        q = "&".join(
            [
                "format=text",
                "starttime=%s" % req.start,
                "endtime=%s" % req.end,
                "minmagnitude=%s" % req.minmag,
                "orderby=time",
                "limit=1000",
            ]
        )
    return "%s/fdsnws/event/1/query?%s" % (base, q)


def _ftime(s: str) -> float:
    s = s.strip().rstrip("Z")
    try:
        return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()
    except Exception:
        return 0.0


def _parse_events_result(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse FDSN event 'text' format into event markers; set the active event
    (largest magnitude) for distance/azimuth sorting."""
    global _ACTIVE_EVENT, _EVENTS
    result = snuffling_pb2.SnufflingResult(show_window=True)
    lines = []
    _EVENTS = []
    best = None
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        c = line.split("|")
        if len(c) < 11:
            continue
        t = _ftime(c[1])
        lat = float(c[2]) if c[2] else 0.0
        lon = float(c[3]) if c[3] else 0.0
        depth = (float(c[4]) if c[4] else 0.0) * 1000.0
        mag = float(c[10]) if len(c) > 10 and c[10] else 0.0
        name = c[12] if len(c) > 12 else ""
        when = c[1].replace("T", " ")[:19]
        marker = result.markers.add()
        marker.t = t
        marker.phase = ("M%.1f %s" % (mag, name)).strip()[:48]
        marker.is_event = True
        _EVENTS.append(
            data_pb2.MapEvent(
                lat=lat, lon=lon, mag=mag, depth=depth, name=name or "(Event)", time=when
            )
        )
        lines.append("M%.1f  %s  %s" % (mag, when, name))
        if best is None or mag > best[0]:
            best = (mag, lat, lon, depth, t)
    if best is not None:
        _ACTIVE_EVENT = (best[1], best[2], best[3], best[4])
        header = (
            "%d event(s) found; active event M%.1f (distance sort enabled).\n"
            "Jumped the view to the events — load waveforms for this time to "
            "see them on data." % (len(result.markers), best[0])
        )
    else:
        header = "No events found in this time window."
    body = "\n".join(lines[:30])
    result.text = header + ("\n\n" + body if body else "")
    return result


def _short_event_name(name: str) -> str:
    """A compact event code from a QuakeML public id, e.g.
    ``quakeml:earthquake.usgs.gov/...?eventid=us6000jllz&...`` → ``us6000jllz`` or
    ``quakeml:us.anss.org/event/us6000jllz`` → ``us6000jllz``."""
    s = name or ""
    match = re.search(r"eventid=(\w+)", s)
    if match:
        return match.group(1)
    return s.rstrip("/").rsplit("/", 1)[-1].rsplit(":", 1)[-1]


def _parse_events_quakeml(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse an FDSN QuakeML event response into event markers carrying moment
    tensors, and precompute the Kagan angle of every MT event to the active
    (largest-magnitude) event. Reuses pyrocko's own QuakeML reader + MomentTensor
    (no seismology reimplemented in Rust). Also populates ``_EVENTS`` and
    ``_ACTIVE_EVENT`` so the Map overlay and distance sort keep working."""
    global _ACTIVE_EVENT, _ACTIVE_MT, _EVENTS
    from pyrocko import moment_tensor as mtm
    from pyrocko import util

    # quakeml.py eagerly imports pyrocko.gui.snuffler.marker; install the stub that
    # avoids running the Qt snuffler package init first (same trick as markers IO).
    _marker_module()
    from pyrocko.io import quakeml

    result = snuffling_pb2.SnufflingResult(show_window=True)
    _EVENTS = []
    _EVENT_REGISTRY.clear()
    qml = quakeml.QuakeML.load_xml(string=text)
    events = qml.get_pyrocko_events()

    entries = []
    best = None
    for ev in events:
        mag = float(ev.magnitude) if ev.magnitude is not None else 0.0
        lat = float(ev.lat)
        lon = float(ev.lon)
        depth = float(ev.depth) if ev.depth is not None else 0.0
        _EVENTS.append(
            data_pb2.MapEvent(
                lat=lat,
                lon=lon,
                mag=mag,
                depth=depth,
                name=_short_event_name(ev.name) or "(Event)",
                time=util.time_to_str(ev.time)[:19],
            )
        )
        entries.append((ev, mag, lat, lon, depth))
        if best is None or mag > best[1]:
            best = (ev, mag, lat, lon, depth)

    if best is not None:
        best_ev = best[0]
        _ACTIVE_EVENT = (best[2], best[3], best[4], float(best_ev.time))
        _ACTIVE_MT = best_ev.moment_tensor
    else:
        _ACTIVE_MT = None

    lines = []
    for ev, mag, _lat, _lon, _depth in entries:
        short = _short_event_name(ev.name)
        marker = result.markers.add()
        marker.t = float(ev.time)
        marker.phase = ("M%.1f %s" % (mag, short)).strip()[:48]
        marker.is_event = True
        marker.event_hash = ev.name or ""
        marker.magnitude = mag
        marker.lat = float(ev.lat)
        marker.lon = float(ev.lon)
        if ev.depth is not None:
            marker.depth = float(ev.depth)
        if ev.name:
            _EVENT_REGISTRY[ev.name] = ev
        mt = ev.moment_tensor
        if mt is not None:
            mnn, mee, mdd, mne, mnd, med = (float(x) for x in mt.m6())
            marker.moment_tensor.mnn = mnn
            marker.moment_tensor.mee = mee
            marker.moment_tensor.mdd = mdd
            marker.moment_tensor.mne = mne
            marker.moment_tensor.mnd = mnd
            marker.moment_tensor.med = med
            if _ACTIVE_MT is not None:
                marker.kagan_deg = float(mtm.kagan_angle(_ACTIVE_MT, mt))
        when = util.time_to_str(ev.time)[:19]
        lines.append("M%.1f  %s  %s%s" % (mag, when, short, "  [MT]" if mt is not None else ""))

    n_mt = sum(1 for e, *_ in entries if e.moment_tensor is not None)
    if best is not None:
        header = (
            "%d event(s) found, %d with moment tensors; active event M%.1f "
            "(distance sort + Kagan enabled)." % (len(result.markers), n_mt, best[1])
        )
    else:
        header = "No events found in this time window."
    body = "\n".join(lines[:30])
    result.text = header + ("\n\n" + body if body else "")
    return result


def _set_active_event(req: data_pb2.SetActiveEventRequest) -> data_pb2.ActiveEventResult:
    """Set (or clear) the user-chosen active event — the single reference every
    downstream consumer (station distances, Kagan column, cake/seismosizer origin,
    map centre) reads from ``_ACTIVE_EVENT``/``_ACTIVE_MT``. Faithful port of
    snuffler ``set_active_event_marker`` / ``deactivate_event_marker``; recomputes
    the Kagan angle of every registered event against the new active MT."""
    global _ACTIVE_EVENT, _ACTIVE_MT
    from pyrocko import model
    from pyrocko import moment_tensor as mtm

    if req.deactivate:
        _ACTIVE_EVENT = None
        _ACTIVE_MT = None
        return data_pb2.ActiveEventResult(
            has_event=False,
            kagan=[data_pb2.EventKagan(event_hash=h) for h in _EVENT_REGISTRY],
        )

    ev = _EVENT_REGISTRY.get(req.event_hash) if req.event_hash else None
    if ev is None:
        # Hand-made event marker: derive lat/lon from the marker's single station if
        # it is bound to one, else keep the previous active event's location (Qt).
        lat = lon = 0.0
        if req.HasField("nslc") and (req.nslc.net, req.nslc.sta) in _STATIONS:
            lat, lon, _elev = _STATIONS[(req.nslc.net, req.nslc.sta)]
        elif _ACTIVE_EVENT is not None:
            lat, lon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
        ev = model.Event(time=float(req.t), lat=lat, lon=lon, depth=0.0)
        if req.event_hash:
            _EVENT_REGISTRY[req.event_hash] = ev

    _ACTIVE_EVENT = (float(ev.lat), float(ev.lon), float(ev.depth or 0.0), float(ev.time))
    _ACTIVE_MT = ev.moment_tensor

    out = data_pb2.ActiveEventResult(has_event=True, active_hash=req.event_hash or "")
    for h, e in _EVENT_REGISTRY.items():
        entry = out.kagan.add()
        entry.event_hash = h
        mt = getattr(e, "moment_tensor", None)
        if _ACTIVE_MT is not None and mt is not None:
            entry.kagan_deg = float(mtm.kagan_angle(_ACTIVE_MT, mt))
    return out


def _station_url(req: data_pb2.StationRequest) -> str:
    """FDSN station query URL (text format, station level)."""
    base = _fdsn_base(req.datacenter)
    q = "&".join(
        [
            "net=%s" % (req.net or "*"),
            "sta=%s" % (req.sta or "*"),
            "cha=%s" % (req.cha or "*"),
            "level=station",
            "format=text",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def _parse_stations_result(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse FDSN station 'text' format (#Net|Sta|Lat|Lon|Elev|Site|Start|End)
    and remember coordinates for distance/azimuth sorting."""
    global _STATIONS
    n = 0
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        c = line.split("|")
        if len(c) < 5:
            continue
        try:
            _STATIONS[(c[0], c[1])] = (float(c[2]), float(c[3]), float(c[4]))
            n += 1
        except Exception:
            pass
    return snuffling_pb2.SnufflingResult(
        show_window=True, text="%d station(s) with coordinates loaded" % n
    )


def _load_stations_result(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse a local pyrocko station file into the ``_STATIONS`` coordinate table
    (distance/azimuth sorting, cake, map), via pyrocko's own ``model.load_stations``."""
    global _STATIONS
    if not text.strip():
        return snuffling_pb2.SnufflingResult(show_window=True, text="empty station file")
    import tempfile

    from pyrocko import model

    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as handle:
        handle.write(text)
        path = handle.name
    try:
        stations = model.load_stations(path)
    except Exception as exc:  # noqa: BLE001 — surface a parse failure as text
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="could not parse station file: %s" % exc
        )
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
    n = 0
    for st in stations:
        try:
            _STATIONS[(st.network, st.station)] = (
                float(st.lat),
                float(st.lon),
                float(st.elevation),
            )
            n += 1
        except Exception:  # noqa: BLE001 — skip malformed entries
            pass
    return snuffling_pb2.SnufflingResult(
        show_window=True, text="%d station(s) with coordinates loaded" % n
    )


def _station_distances_result() -> data_pb2.StationDistanceList:
    """Great-circle distance/azimuth/backazimuth (degrees) from the active event
    to each known station, via pyrocko's ``orthodrome`` — for the marker/station
    distance columns. Empty (``has_event=False``) until an event is active."""
    result = data_pb2.StationDistanceList(has_event=_ACTIVE_EVENT is not None)
    if _ACTIVE_EVENT is None or not _STATIONS:
        return result
    from pyrocko import orthodrome as od

    elat, elon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
    for (net, sta), (slat, slon, _elev) in _STATIONS.items():
        entry = result.stations.add()
        entry.net = net
        entry.sta = sta
        entry.distance_deg = _dist_deg(elat, elon, slat, slon)
        entry.azimuth_deg = float(od.azimuth(elat, elon, slat, slon)) % 360.0
        entry.backazimuth_deg = float(od.azimuth(slat, slon, elat, elon)) % 360.0
    return result


def _load_events_result(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse a local pyrocko event file into event markers and set the active
    event (largest magnitude, for distance/azimuth sorting), via pyrocko's own
    ``model.load_events``. Mirrors the FDSN catalog path."""
    global _ACTIVE_EVENT, _EVENTS
    if not text.strip():
        return snuffling_pb2.SnufflingResult(show_window=True, text="empty event file")
    import tempfile

    from pyrocko import model, util

    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as handle:
        handle.write(text)
        path = handle.name
    try:
        events = model.load_events(path)
    except Exception as exc:  # noqa: BLE001 — surface a parse failure as text
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="could not parse event file: %s" % exc
        )
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass

    result = snuffling_pb2.SnufflingResult(show_window=True)
    _EVENTS = []
    best: Optional[Tuple[float, float, float, float, float]] = None
    for ev in events:
        lat = float(ev.lat)
        lon = float(ev.lon)
        depth = float(ev.depth or 0.0)
        mag = float(ev.magnitude) if ev.magnitude is not None else 0.0
        marker = result.markers.add()
        marker.t = float(ev.time)
        marker.phase = ("M%.1f %s" % (mag, ev.name or "")).strip()[:48]
        marker.is_event = True
        _EVENTS.append(
            data_pb2.MapEvent(
                lat=lat,
                lon=lon,
                mag=mag,
                depth=depth,
                name=ev.name or "(Event)",
                time=util.time_to_str(ev.time)[:19],
            )
        )
        if best is None or mag > best[0]:
            best = (mag, lat, lon, depth, float(ev.time))
    if best is not None:
        _ACTIVE_EVENT = (best[1], best[2], best[3], best[4])
        result.text = "%d event(s) loaded; active event M%.1f (distance sort enabled)." % (
            len(result.markers),
            best[0],
        )
    else:
        result.text = "No events in file."
    return result


def _response_url(req: data_pb2.StationRequest) -> str:
    """FDSN station query URL at response level (StationXML with instrument
    responses, for deconvolution)."""
    base = _fdsn_base(req.datacenter)
    q = "&".join(
        [
            "net=%s" % (req.net or "*"),
            "sta=%s" % (req.sta or "*"),
            "cha=%s" % (req.cha or "*"),
            "level=response",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def _load_responses_result(xml_text: str) -> snuffling_pb2.SnufflingResult:
    """Parse fetched StationXML into instrument responses (for Deconvolution) and
    also harvest station coordinates (for distance/azimuth sorting)."""
    global _STATIONXML, _RESPONSE_GEN
    if not xml_text.strip():
        return snuffling_pb2.SnufflingResult(show_window=True, text="no station metadata returned")
    from pyrocko.io import stationxml

    _STATIONXML = stationxml.load_xml(string=xml_text)
    _RESPONSE_GEN += 1
    _clear_view_cache()
    nchan = 0
    for net in _STATIONXML.network_list:
        for st in net.station_list:
            nchan += len(st.channel_list)
            try:
                _STATIONS[(net.code, st.code)] = (
                    float(st.latitude.value),
                    float(st.longitude.value),
                    float(st.elevation.value),
                )
            except Exception:
                pass
    return snuffling_pb2.SnufflingResult(
        show_window=True,
        text="%d channel response(s) loaded — enable Options ▸ Deconvolution" % nchan,
    )


_PLATES: Optional[List[data_pb2.PlateBoundary]] = None


async def _plate_boundaries() -> List[data_pb2.PlateBoundary]:
    """Parse pyrocko's shipped Bird (2003) ``plates.kml`` into lon/lat polylines
    (memoized). Pure geometry passthrough of pyrocko's own data via stdlib XML —
    the map overlays exactly what the Qt map snuffling draws."""
    global _PLATES
    if _PLATES is not None:
        return _PLATES
    import xml.etree.ElementTree as ET

    plates: List[data_pb2.PlateBoundary] = []
    text = await _fetch_text_or_empty(_asset_url("plates.kml"))
    if text:

        def local(tag: str) -> str:
            return tag.rsplit("}", 1)[-1]

        root = ET.fromstring(text)
        for pm in root.iter():
            if local(pm.tag) != "Placemark":
                continue
            name = ""
            lons: List[float] = []
            lats: List[float] = []
            for el in pm.iter():
                lt = local(el.tag)
                if lt == "name" and el.text:
                    name = el.text.strip()
                elif lt == "coordinates" and el.text:
                    for tok in el.text.split():
                        parts = tok.split(",")
                        if len(parts) >= 2:
                            lons.append(float(parts[0]))
                            lats.append(float(parts[1]))
            if len(lons) >= 2:
                plates.append(data_pb2.PlateBoundary(name=name, lons=lons, lats=lats))
    _PLATES = plates
    return _PLATES


def _map_data_result(plates: Optional[List[data_pb2.PlateBoundary]] = None) -> data_pb2.MapData:
    """Station + event coordinates for the Map overlay (no network — reads what
    the FDSN fetches already loaded), plus optional plate boundaries."""
    msg = data_pb2.MapData()
    for (net, sta), (lat, lon, _elev) in _STATIONS.items():
        station = msg.stations.add()
        station.label = "%s.%s" % (net, sta)
        station.lat = lat
        station.lon = lon
    msg.events.extend(_EVENTS)
    if plates:
        msg.plate_boundaries.extend(plates)
    return msg


# --- Marker file I/O (pyrocko marker format) -------------------------------


_marker_mod: Any = None
_gf_store_mod: Any = None
_gf_meta_mod: Any = None
# Mounted Green's-function stores: dropdown id -> filesystem dir (Pyodide FS).
_GF_STORES: Dict[str, str] = {}
# Dropdown label prefix for GF-store earth models (mirrors Qt's "GF Store: <id>").
GF_STORE_PREFIX = "GF Store: "


def _marker_module() -> Any:
    """Load pyrocko's ``marker`` module without running the snuffler package init.

    ``import pyrocko.gui.snuffler.marker`` would first execute
    ``pyrocko/gui/snuffler/__init__.py``, which eagerly imports the full Qt
    snuffler application (``pile`` -> ``avl``, PyQt) — none of which exists in our
    minimal Pyodide wheel. marker.py itself only needs util/plot/model/trace, so
    install a lightweight stub for the ``snuffler`` package (carrying the real
    directory on ``__path__``) and import the submodule through it.
    """
    global _marker_mod
    if _marker_mod is None:
        import importlib
        import sys
        import types

        import pyrocko.gui

        pkg_name = "pyrocko.gui.snuffler"
        existing = sys.modules.get(pkg_name)
        if existing is None or not getattr(existing, "_snuffler_stub", False):
            stub = types.ModuleType(pkg_name)
            stub.__path__ = [os.path.join(os.path.dirname(pyrocko.gui.__file__), "snuffler")]
            stub.__package__ = pkg_name
            setattr(stub, "_snuffler_stub", True)
            sys.modules[pkg_name] = stub
        _marker_mod = importlib.import_module(pkg_name + ".marker")
    return _marker_mod


def _gf_modules() -> Tuple[Any, Any]:
    """Load ``pyrocko.gf.store`` + ``pyrocko.gf.meta`` WITHOUT running
    ``pyrocko/gf/__init__.py``.

    ``import pyrocko.gf`` pulls seismosizer -> ws -> ``import requests`` (only
    needed by RemoteEngine). Reading a local store's config + 1D earth model needs
    neither requests nor ``store_ext``, so install a stub ``pyrocko.gf`` package
    (real dir on ``__path__``) and import just the two submodules — the same trick
    as :py:func:`_marker_module`.
    """
    global _gf_store_mod, _gf_meta_mod
    if _gf_store_mod is None or _gf_meta_mod is None:
        import importlib
        import sys
        import types

        import pyrocko

        pkg = "pyrocko.gf"
        existing = sys.modules.get(pkg)
        if existing is None or not getattr(existing, "_gf_stub", False):
            stub = types.ModuleType(pkg)
            stub.__path__ = [os.path.join(os.path.dirname(pyrocko.__file__), "gf")]
            stub.__package__ = pkg
            setattr(stub, "_gf_stub", True)
            sys.modules[pkg] = stub
        _gf_meta_mod = importlib.import_module(pkg + ".meta")
        _gf_store_mod = importlib.import_module(pkg + ".store")
    return _gf_store_mod, _gf_meta_mod


class GfStoreInfo(NamedTuple):
    id: str
    label: str
    sample_rate: float
    has_earthmodel: bool


def _gf_store_info_result(store_dir: str) -> GfStoreInfo:
    """Read a mounted GF store's config (no ``store_ext`` / network needed) and
    register it so its embedded 1D earth model is selectable in Cake Phase.

    Raises on failure; the ``call()`` failsafe turns that into a BridgeError.
    """
    gfstore, _gfmeta = _gf_modules()
    cfg = gfstore.Store(store_dir).config
    sid = str(cfg.id)
    _GF_STORES[sid] = store_dir
    return GfStoreInfo(
        id=sid,
        label=GF_STORE_PREFIX + sid,
        sample_rate=float(getattr(cfg, "sample_rate", 0.0) or 0.0),
        has_earthmodel=getattr(cfg, "earthmodel_1d", None) is not None,
    )


def gf_store_label(store_dir: str) -> str:
    return _gf_store_info_result(store_dir).label


def _markers_to_text(req: data_pb2.MarkerFile) -> str:
    """Serialize markers to pyrocko's snuffler marker-file format."""
    pm = _marker_module()
    from pyrocko import model
    from pyrocko import moment_tensor as mtm

    ms = []
    for m in req.markers:
        tmin = float(m.tmin)
        tmax = float(m.tmax)
        kind = int(m.kind)
        ids = [(m.nslc.net, m.nslc.sta, m.nslc.loc, m.nslc.cha)] if m.HasField("nslc") else []
        phase = m.phase if m.HasField("phase") else None
        polarity = int(m.polarity) if m.HasField("polarity") else None
        unc = float(m.uncertainty) if m.HasField("uncertainty") else None
        # Event markers save as real EventMarkers (origin + focal mechanism), not
        # downgraded to plain markers; catalog events carry a mag label in `phase`,
        # so this branch MUST precede the phase-marker check.
        if m.is_event:
            mt = None
            if m.HasField("moment_tensor"):
                t = m.moment_tensor
                mt = mtm.MomentTensor(
                    mnn=t.mnn, mee=t.mee, mdd=t.mdd, mne=t.mne, mnd=t.mnd, med=t.med
                )
            ev = model.Event(
                time=tmin,
                lat=float(m.lat) if m.HasField("lat") else 0.0,
                lon=float(m.lon) if m.HasField("lon") else 0.0,
                depth=float(m.depth) if m.HasField("depth") else 0.0,
                magnitude=float(m.magnitude) if m.HasField("magnitude") else None,
                name=phase or "Event",
                moment_tensor=mt,
            )
            ms.append(pm.EventMarker(ev, kind, event_hash=m.event_hash or None))
        # A polarity or uncertainty is only meaningful on a PhaseMarker, so promote
        # any marker that carries one even if it has no phase name yet.
        elif phase or polarity is not None or unc is not None or m.HasField("event_hash"):
            ms.append(
                pm.PhaseMarker(
                    ids,
                    tmin,
                    tmax,
                    kind,
                    phasename=phase,
                    polarity=polarity,
                    uncertainty=unc,
                    # Phase-to-event association (snuffler set_event): the linked
                    # event's hash rides the phase marker so it survives the file.
                    event_hash=m.event_hash or None,
                )
            )
        else:
            ms.append(pm.Marker(ids, tmin, tmax, kind))
    path = "/tmp/markers_out.txt"
    pm.save_markers(ms, path)
    with open(path) as f:
        return f.read()


def _markers_from_text(text: str) -> data_pb2.SnufMarkerList:
    """Parse a pyrocko marker file into the SnufMarker messages the egui side adds."""
    pm = _marker_module()

    result = data_pb2.SnufMarkerList()
    path = "/tmp/markers_in.txt"
    with open(path, "w") as f:
        f.write(text)
    try:
        ms = pm.load_markers(path)
    except Exception:
        return result
    for mk in ms:
        ids = mk.get_nslc_ids()
        marker = result.markers.add()
        marker.t = float(mk.tmin)
        marker.tmax = float(mk.tmax)
        marker.kind = int(mk.kind or 0)
        marker.is_event = isinstance(mk, pm.EventMarker)
        if ids:
            marker.nslc.CopyFrom(_nslc_pb(list(ids[0])))
        if isinstance(mk, pm.EventMarker):
            ev = mk.get_event()
            if ev is not None:
                if ev.name:
                    marker.phase = str(ev.name)[:48]
                if ev.magnitude is not None:
                    marker.magnitude = float(ev.magnitude)
                if ev.depth is not None:
                    marker.depth = float(ev.depth)
                marker.lat = float(ev.lat)
                marker.lon = float(ev.lon)
                if ev.moment_tensor is not None:
                    mnn, mee, mdd, mne, mnd, med = (float(x) for x in ev.moment_tensor.m6())
                    marker.moment_tensor.mnn = mnn
                    marker.moment_tensor.mee = mee
                    marker.moment_tensor.mdd = mdd
                    marker.moment_tensor.mne = mne
                    marker.moment_tensor.mnd = mnd
                    marker.moment_tensor.med = med
            try:
                marker.event_hash = mk.get_event_hash() or ""
            except Exception:
                pass
        if isinstance(mk, pm.PhaseMarker):
            try:
                phase = mk.get_phasename()
            except Exception:
                phase = None
            if phase is not None:
                marker.phase = phase
            try:
                polarity = mk.get_polarity()
            except Exception:
                polarity = None
            if polarity is not None:
                marker.polarity = int(polarity)
            # PhaseMarker stores uncertainty privately (no public getter).
            unc = getattr(mk, "_uncertainty", None)
            if unc is not None:
                marker.uncertainty = float(unc)
            # The associated event's hash (phase-to-event link), if any.
            try:
                ehash = mk.get_event_hash()
            except Exception:
                ehash = None
            if ehash:
                marker.event_hash = str(ehash)
    return result


# Directory holding the cake earth models (tiny .nd text files shipped as build
# assets and fetched into the Pyodide FS on first cake use; the minimal wheel has
# no data dir, so cake.builtin_models() is empty and models must be loaded by
# path). They are fetched here in Python (pyfetch) — see _ensure_earth_models.
MODEL_DIR = "/earthmodels"

# Builtin cake models served under earthmodels/<stem>.nd (matches the Cake Phase
# dropdown). Fetched lazily on first cake run so the boot path stays light.
_CAKE_MODELS = [
    "ak135-f-average-no-ocean.f",
    "ak135-f-average-no-ocean.l",
    "ak135-f-average-no-ocean.m",
    "ak135-f-average.f",
    "ak135-f-average.l",
    "ak135-f-average.m",
    "ak135-f-average.vf",
    "ak135-f-continental.f",
    "ak135-f-continental.l",
    "ak135-f-continental.m",
    "prem-no-ocean.f",
    "prem-no-ocean.l",
    "prem-no-ocean.m",
    "prem.f",
    "prem.l",
    "prem.m",
]
_EARTH_MODELS_STAGED = False


async def _ensure_earth_models() -> None:
    """Fetch the builtin cake models into the Pyodide FS once, before a cake run
    that uses one. A model that fails to fetch is skipped (just not selectable)."""
    global _EARTH_MODELS_STAGED
    if _EARTH_MODELS_STAGED:
        return
    os.makedirs(MODEL_DIR, exist_ok=True)
    for stem in _CAKE_MODELS:
        path = "%s/%s.nd" % (MODEL_DIR, stem)
        if os.path.exists(path):
            continue
        text = await _fetch_text_or_empty(_asset_url("earthmodels/%s.nd" % stem))
        if text:
            with open(path, "w") as f:
                f.write(text)
    _EARTH_MODELS_STAGED = True


def _cake_model_path(name: str) -> str:
    """Filesystem path of a staged cake model from its dropdown name."""
    return "%s/%s.nd" % (MODEL_DIR, name)


def _cake_load_model(cake: Any, name: str) -> Any:
    """Load the cake earth model for a Cake Phase model selection.

    A ``GF Store: <id>`` selection uses the mounted store's embedded 1D model
    (``config.earthmodel_1d``, read without ``store_ext``); any other name is a
    staged builtin ``.nd`` model loaded by path.
    """
    if name.startswith(GF_STORE_PREFIX):
        sid = name[len(GF_STORE_PREFIX) :]
        store_dir = _GF_STORES.get(sid)
        if store_dir is None:
            raise ValueError("GF store %r is not mounted" % sid)
        gfstore, _gfmeta = _gf_modules()
        mod = gfstore.Store(store_dir).config.earthmodel_1d
        if mod is None:
            raise ValueError("GF store %r has no 1D earth model" % sid)
        return mod
    return cake.load_model(_cake_model_path(name))


def _phase_color_id(name: str) -> int:
    """Stable per-phase index into the egui marker palette (8 Tango colors).

    In the spirit of :py:func:`pyrocko.plot.cake_plot.path2colorint`, but kept
    deterministic and without the collision-avoidance pass, so the Rust side can
    reuse the same color for a phase's markers, ray paths and the ray legend
    with no extra bookkeeping crossing the boundary.
    """
    return sum(ord(ch) for ch in name) % 8


def _cake_section(
    req: snuffling_pb2.SnufflingRequestParams,
) -> Tuple[str, List[str], float, bool, str]:
    """Extract the Cake Phase knobs from the snuffling params."""
    c = req.snufflings.cake
    model = c.cake_model or "ak135-f-continental.m"
    phases = [str(x) for x in (list(c.cake_phases) or ["~P"])]
    tshift = float(c.cake_tshift)
    use_station_depth = bool(c.cake_use_station_depth)
    mode = c.cake_mode or "run"
    return model, phases, tshift, use_station_depth, mode


def _cake_wanted_phases(cake: Any, names: List[str]) -> Tuple[List[Any], List[str]]:
    """Turn the enabled phase names into cake ``PhaseDef`` objects.

    A leading ``~`` selects a raw definition (``cake.PhaseDef``); any other name
    is expanded via the classic-phase table (``cake.PhaseDef.classic``), exactly
    as the Qt Cake Phase snuffling does. The friendly name is stashed on each
    PhaseDef so ``ray.given_phase().name`` reports it back. Parse failures are
    collected (not raised) so one bad custom phase does not sink the whole run.
    """
    wanted = []
    errors: List[str] = []
    for name in names:
        try:
            if name.startswith("~"):
                phases = [cake.PhaseDef(name[1:])]
            else:
                phases = cake.PhaseDef.classic(name)
        except (cake.UnknownClassicPhase, cake.PhaseDefParseError) as exc:
            errors.append("%s (%s)" % (name, exc))
            continue
        for pha in phases:
            pha.name = name
        wanted.extend(phases)
    return wanted, errors


def _cake_stations() -> List[Tuple[str, str, float, float]]:
    """``(net, sta, dist_deg, elevation_m)`` for stations that have loaded traces.

    Distances are great-circle degrees from the active event (which is what
    ``cake.arrivals`` wants). Requires ``_ACTIVE_EVENT`` + ``_STATIONS``.
    """
    if _ACTIVE_EVENT is None:
        return []
    elat, elon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
    have = {(tr.nslc_id[0], tr.nslc_id[1]) for tr in _TRACES}
    out: List[Tuple[str, str, float, float]] = []
    for (net, sta), (slat, slon, elev) in _STATIONS.items():
        if have and (net, sta) not in have:
            continue
        out.append((net, sta, _dist_deg(elat, elon, slat, slon), elev))
    return out


def _cake_run(req: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Theoretical arrivals → phase markers (the Cake Phase 'Run' action).

    All travel-time computation is pyrocko's :py:mod:`pyrocko.cake` (pure
    numpy/scipy ray tracing) and the geometry is :py:func:`_dist_deg`; this only
    marshals the rays into markers and a per-pick angle table.
    """
    model, phase_names, tshift, use_station_depth, _mode = _cake_section(req)
    if _ACTIVE_EVENT is None:
        return snuffling_pb2.SnufflingResult(
            show_window=True,
            text="No active event. Run a catalog search first (Data ▸ Catalog search).",
        )
    if not _STATIONS:
        return snuffling_pb2.SnufflingResult(
            show_window=True,
            text="No station coordinates. Fetch stations first (Data ▸ Download ▸ Fetch stations).",
        )

    from pyrocko import cake

    _elat, _elon, edepth, etime = _ACTIVE_EVENT
    mod = _cake_load_model(cake, model)
    wanted, errors = _cake_wanted_phases(cake, phase_names)

    result = snuffling_pb2.SnufflingResult(show_window=True)
    lines = ["%-14s %-8s %12s %8s %8s" % ("station", "phase", "t-rel[s]", "inc[deg]", "tko[deg]")]
    nstations = 0
    for net, sta, dist_deg, elev in _cake_stations():
        nstations += 1
        # _STATIONS stores elevation, not instrument depth; approximate a
        # borehole depth from a negative elevation when the user opts in (Qt
        # uses station.depth, which the web app does not track).
        rdepth = max(0.0, -float(elev)) if use_station_depth else 0.0
        rays = mod.arrivals(phases=wanted, distances=[dist_deg], zstart=edepth, zstop=rdepth)
        for ray in rays:
            name = ray.given_phase().name
            tt = float(ray.t) + tshift
            inc = float(ray.incidence_angle())
            tko = float(ray.takeoff_angle())
            marker = result.markers.add()
            marker.t = etime + tt
            marker.tmax = etime + tt
            marker.kind = _phase_color_id(name)
            marker.phase = name
            marker.nslc.CopyFrom(_nslc_pb([net, sta, "", "*"]))
            lines.append(
                "%-14s %-8s %12.3f %8.1f %8.1f" % ("%s.%s" % (net, sta), name, tt, inc, tko)
            )

    npicks = len(result.markers)
    header = "Cake arrivals (%s): %d pick(s) at %d station(s)." % (model, npicks, nstations)
    if errors:
        header += "\nSkipped phases: " + ", ".join(errors)
    result.text = header + "\n\n" + "\n".join(lines) if npicks else header
    return result


def _command_glob(scope: Optional[str], pattern: str) -> str:
    """Build an "n.s.l.c" fnmatch glob from an optional scope + pattern."""
    idx = {"n": 0, "s": 1, "l": 2, "c": 3}
    if scope in idx:
        fields = ["*", "*", "*", "*"]
        fields[idx[scope]] = pattern
        return ".".join(fields)
    # No scope: a bare token without dots matches anywhere in the id.
    return pattern if "." in pattern else "*%s*" % pattern


def _command_scene() -> data_pb2.CommandResult:
    result = data_pb2.CommandResult(action="scene")
    result.scene.CopyFrom(_scene_meta())
    return result


def _apply_scaling_hooks(data_ranges: Dict[Any, Tuple[float, float]]) -> None:
    """Apply the ':scaling' command's per-pattern amplitude overrides to the
    computed data ranges (snuffler's ``apply_scaling_hooks``): for each hook in
    sorted pattern order, replace lo/hi where the bound is not None. The '_' key is
    the global rule applied to every track; a real pattern is matched via
    ``match_nslc`` against the scaling key (an nslc tuple or a channel string).
    ``None`` keys (common-scale mode) have no channel identity and are skipped."""
    if not _SCALING_HOOKS:
        return
    from pyrocko.util import match_nslc

    for pattern in sorted(_SCALING_HOOKS):
        vmin, vmax = _SCALING_HOOKS[pattern]
        for key in list(data_ranges):
            if pattern != "_":
                if key is None or not match_nslc(pattern, key):
                    continue
            lo, hi = data_ranges[key]
            if vmin is not None:
                lo = vmin
            if vmax is not None:
                hi = vmax
            data_ranges[key] = (lo, hi)


def _goto_window_len(spec: str) -> Optional[float]:
    """The visible window (seconds) matching how specific a ``goto`` date is
    (snuffler): a bare year → ~1yr, year-month → ~1 month, a full date → 1 day,
    +hour → 1h, +minute → 1min; a fully-specified time keeps the current window."""
    s = spec.strip()
    if re.fullmatch(r"\d{4}", s):
        return 12 * 32 * 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}", s):
        return 32 * 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}", s):
        return 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}[ tT]\d{1,2}", s):
        return 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}[ tT]\d{1,2}:\d{1,2}", s):
        return 60.0
    return None


def _command_result(line: str) -> data_pb2.CommandResult:
    """Execute a snuffler ':' command line into a CommandResult whose ``action``
    is ``goto`` (with ``t``), ``scene`` (with ``scene``), ``error`` (with
    ``message``), or ``none``.

    Supported: ``goto <time>`` (also ``goto now|today|yesterday``);
    ``hide [n|s|l|c] <pattern>``; ``unhide [[n|s|l|c] <pattern>]``;
    ``n|s|l|c <pattern>`` (reduce to matching).
    """
    global _HIDDEN, _REDUCE
    parts = line.split()
    if not parts:
        return data_pb2.CommandResult(action="none")
    cmd, args = parts[0], parts[1:]

    if cmd == "goto":
        if not args:
            return data_pb2.CommandResult(action="error", message="usage: goto <time>")
        spec = " ".join(args).strip().lower()
        if spec in ("now", "today", "yesterday"):
            when = datetime.now(timezone.utc)
            if spec == "yesterday":
                when = when - timedelta(days=1)
            if spec in ("today", "yesterday"):
                when = when.replace(hour=0, minute=0, second=0, microsecond=0)
            return data_pb2.CommandResult(action="goto", t=when.timestamp())
        from pyrocko import util

        try:
            joined = " ".join(args)
            t = float(util.str_to_time_fillup(joined))
            res = data_pb2.CommandResult(action="goto", t=t)
            tlen = _goto_window_len(joined)
            if tlen is not None:
                res.tlen = tlen
            return res
        except Exception:
            pass
        # Not a time — fall back to a loaded event by name (snuffler
        # go_to_event_by_name): jump the view to that event's origin time.
        target = " ".join(args).strip().lower()
        for h, ev in _EVENT_REGISTRY.items():
            names = {
                (ev.name or "").lower(),
                _short_event_name(ev.name).lower(),
                h.lower(),
            }
            if target in names:
                return data_pb2.CommandResult(action="goto", t=float(ev.time))
        return data_pb2.CommandResult(
            action="error", message="no event or time matching: %s" % " ".join(args)
        )

    if cmd in ("hide", "unhide"):
        scope, rest = None, args
        if args and args[0] in ("n", "s", "l", "c"):
            scope, rest = args[0], args[1:]
        pattern = rest[0] if rest else None
        if cmd == "hide":
            if pattern is None:
                return data_pb2.CommandResult(
                    action="error", message="usage: hide [n|s|l|c] <pattern>"
                )
            glob = _command_glob(scope, pattern)
            if glob not in _HIDDEN:
                _HIDDEN.append(glob)
        else:  # unhide (no args clears everything)
            if pattern is None:
                _HIDDEN = []
                _REDUCE = None
            else:
                glob = _command_glob(scope, pattern)
                _HIDDEN = [h for h in _HIDDEN if h != glob]
        return _command_scene()

    if cmd in ("n", "s", "l", "c"):
        _REDUCE = _command_glob(cmd, args[0] if args else "*")
        return _command_scene()

    if cmd == "scaling":
        # snuffler pile_viewer.py 'scaling' arm: no args clears all hooks; one arg
        # is a usage error; 2 args set the global rule; 3 args set a per-pattern
        # rule; 4+ args are a silent no-op. 'nan' leaves that bound auto (None).
        if len(args) == 0:
            _SCALING_HOOKS.clear()
            return _command_scene()
        if len(args) == 1:
            return data_pb2.CommandResult(
                action="error", message="usage: scaling [n.s.l.c] vmin vmax"
            )
        if len(args) in (2, 3):

            def _bound(tok: str) -> Optional[float]:
                return None if tok.lower() == "nan" else float(tok)

            try:
                vmin, vmax = _bound(args[-2]), _bound(args[-1])
            except ValueError:
                return data_pb2.CommandResult(
                    action="error",
                    message="scaling: vmin/vmax must be numbers or 'nan'",
                )
            if len(args) == 2:
                _SCALING_HOOKS["_"] = (vmin, vmax)
            else:
                _SCALING_HOOKS[args[0]] = (vmin, vmax)
        return _command_scene()

    return data_pb2.CommandResult(action="error", message="unknown command: %s" % cmd)


def _rms_scene(req: snuffling_pb2.SnufflingRequestParams) -> scene_pb2.SceneMeta:
    """Blockwise root-mean-square derived traces (pyrocko's RMS snuffling).

    For each visible trace, RMS is computed over fixed-length blocks across the
    visible window and emitted as a new low-rate trace (channel suffix '-RMS').
    With grouping on, a station's components are combined into one RMS trace.
    Appends the derived traces and returns the updated scene. All numpy.
    """
    global _TRACES
    tmin, tmax = float(req.view.tmin), float(req.view.tmax)
    rms = req.snufflings.rms
    hp = rms.rms_highpass if rms.HasField("rms_highpass") else None
    lp = rms.rms_lowpass if rms.HasField("rms_lowpass") else None
    block = float(rms.rms_block) if rms.rms_block else max(1.0, (tmax - tmin) / 100.0)
    group = bool(rms.rms_group)
    logrms = bool(rms.rms_log)

    # Drop previous RMS output, then work only on original traces. Qt marks
    # snuffling-produced traces as tabu and RMS skips them.
    _TRACES = [tr for tr in _TRACES if not tr.nslc_id[3].endswith("-RMS")]
    src = [
        tr
        for tr in _visible_traces()
        if not _is_tabu_trace(tr) and not tr.nslc_id[3].endswith("-RMS")
    ]
    if not src or block <= 0.0:
        _mark_traces_changed()
        return _scene_meta()

    tpad = 0.0
    for freq in (hp, lp):
        if freq is not None and float(freq) > 0.0:
            tpad = max(tpad, 1.0 / float(freq))

    def grouping(nslc: Tuple[str, str, str, str]) -> Tuple[str, str, str, str]:
        return nslc[:3] + (nslc[3][:-1],) if group else nslc

    ptmin = min(float(tr.tmin) for tr in src)
    if ptmin > tmin:
        tmin = ptmin + tpad
    ptmax = max(float(tr.tmax) for tr in src)
    if ptmax < tmax:
        tmax = ptmax - tpad

    targets: Dict[Tuple[str, str, str, str], ptrace.Trace] = {}
    completed: List[ptrace.Trace] = []
    eps = block * 1e-6
    nwin = int(((tmax - eps) - tmin) / block) + 1
    if nwin <= 0:
        _mark_traces_changed()
        return _scene_meta()

    def complete_chop(tr: ptrace.Trace, wmin: float, wmax: float) -> Optional[ptrace.Trace]:
        try:
            seg = tr.chop(wmin - tpad, wmax + tpad, inplace=False, include_last=False)
        except ptrace.NoData:
            return None

        emin = float(seg.tmin) - (wmin - tpad)
        emax = float(seg.tmax) + float(seg.deltat) - (wmax + tpad)
        if abs(emin) <= 0.5 * float(seg.deltat) and abs(emax) <= 0.5 * float(seg.deltat):
            return seg

        if 0.0 < emin <= 5.0 * float(seg.deltat) and -5.0 * float(seg.deltat) <= emax < 0.0:
            seg.extend(wmin - tpad, wmax + tpad - float(seg.deltat), fillmethod="repeat")
            return seg

        return None

    for iwin in range(nwin):
        wmin = tmin + iwin * block
        wmax = min(tmin + (iwin + 1) * block, tmax)
        tcur = wmin + 0.5 * block
        rms_by_key: Dict[Tuple[str, str, str, str], List[float]] = {}
        for source in src:
            tr = complete_chop(source, wmin, wmax)
            if tr is None or _is_tabu_trace(tr):
                continue
            if lp is not None:
                tr.lowpass(4, float(lp), nyquist_exception=True)
            if hp is not None:
                tr.highpass(4, float(hp), nyquist_exception=True)
            try:
                tr.chop(wmin, wmax)
            except ptrace.NoData:
                continue
            value = 0.0
            if tr.ydata is not None and tr.ydata.size != 0:
                y = tr.ydata.astype(float)
                value = float(np.sqrt(np.sum(y**2) / tr.ydata.size))
            rms_by_key.setdefault(grouping(tr.nslc_id), []).append(value)

        for key, values in rms_by_key.items():
            net, sta, loc, key_cha = key
            target = targets.get(key)
            if target is None or abs((target.tmax + block) - tcur) > 0.01 * block:
                if target is not None:
                    completed.append(target)
                target = ptrace.Trace(
                    network=net,
                    station=sta,
                    location=loc,
                    channel=key_cha + "-RMS",
                    tmin=tcur,
                    deltat=block,
                    ydata=np.zeros(0, dtype=float),
                    meta={"tabu": True},
                )
                targets[key] = target

            sample = np.atleast_1d(np.sqrt(np.sum(np.array(values, dtype=float) ** 2)))
            if logrms and bool(sample[0] != 0.0):
                sample = np.log(sample)
            targets[key].append(sample)

    completed.extend(targets.values())
    _TRACES.extend(completed)

    _mark_traces_changed()
    return _scene_meta()


def _raw_bytes(value: Any) -> bytes:
    if value is None:
        return b""
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value)
    if hasattr(value, "to_py"):
        return bytes(value.to_py())
    return bytes(value)


def _nslc_pb(value: Any) -> common_pb2.Nslc:
    msg = common_pb2.Nslc()
    if value:
        msg.net = str(value[0] if len(value) > 0 else "")
        msg.sta = str(value[1] if len(value) > 1 else "")
        msg.loc = str(value[2] if len(value) > 2 else "")
        msg.cha = str(value[3] if len(value) > 3 else "")
    return msg


class _BridgeServicer:
    """Implements the generated `BridgeServicer`: one typed method per rpc (proto
    request in, proto response out), each delegating to the pyrocko-backed compute
    functions above. The generated `dispatch()` parses the request, packs the
    BridgeResult oneof, and converts any exception to a BridgeError — so there is
    no hand-written dispatch table or per-method serialize/parse boilerplate.
    mypy structurally checks these signatures against the generated Protocol."""

    def make_example(self, request: bridge_pb2.Empty, data: Any) -> scene_pb2.SceneMeta:
        return _make_example_scene()

    def empty_scene(self, request: bridge_pb2.Empty, data: Any) -> scene_pb2.SceneMeta:
        _set_traces([])
        _clear_event_context()
        return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)

    def load_bytes(self, request: data_pb2.LoadBytesRequest, data: Any) -> scene_pb2.SceneMeta:
        path = _stage_bytes(request.name, bytes(request.content))
        return _load_path_scene(
            request.name, path, request.fmt, bool(request.append), request.source_id
        )

    async def load_url(self, request: data_pb2.LoadUrlRequest, data: Any) -> scene_pb2.SceneMeta:
        name = request.url.rsplit("/", 1)[-1] or "data"
        raw = await _fetch_bytes(_asset_url(request.url))
        return _load_path_scene(name, _stage_bytes(name, raw), request.fmt, False)

    def get_view(
        self, request: view_pb2.ViewParams, data: Any
    ) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
        return _get_view_data(request)

    async def render_figure(
        self, request: figure_pb2.FigureRequest, data: Any
    ) -> figure_pb2.FigureResult:
        # Cake figures need the earth-model data files staged first (same as the
        # cake 'Run' snuffling); other figures compute from loaded traces.
        if request.kind in ("cake_model", "cake_rays"):
            model = request.params.snufflings.cake.cake_model or ""
            if not model.startswith(GF_STORE_PREFIX):
                await _ensure_earth_models()
        return _render_figure(request)

    async def run_snuffling(
        self, request: snuffling_pb2.NamedSnufflingRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        # Snuffling errors (e.g. cake) surface in the result text — which the UI
        # shows — rather than as a BridgeError, so the user sees the traceback.
        try:
            if request.name == "cake":
                model = request.params.snufflings.cake.cake_model or ""
                if not model.startswith(GF_STORE_PREFIX):
                    await _ensure_earth_models()
            elif request.name == "seisbench_picker":
                return await _seisbench_pick_result(request.params)
            return _run_snuffling_result(request.name, request.params)
        except Exception as exc:  # noqa: BLE001 — surface, don't raise
            return snuffling_pb2.SnufflingResult(
                show_window=True,
                text=f"{type(exc).__name__}: {exc}\n\n{traceback.format_exc()}",
            )

    def rms_traces(
        self, request: snuffling_pb2.SnufflingRequestParams, data: Any
    ) -> scene_pb2.SceneMeta:
        return _rms_scene(request)

    def run_command(self, request: data_pb2.StringRequest, data: Any) -> data_pb2.CommandResult:
        return _command_result(request.value)

    async def map_data(self, request: bridge_pb2.Empty, data: Any) -> data_pb2.MapData:
        return _map_data_result(await _plate_boundaries())

    def station_distances(
        self, request: bridge_pb2.Empty, data: Any
    ) -> data_pb2.StationDistanceList:
        return _station_distances_result()

    def set_active_event(
        self, request: data_pb2.SetActiveEventRequest, data: Any
    ) -> data_pb2.ActiveEventResult:
        return _set_active_event(request)

    async def load_gf_store_url(
        self, request: data_pb2.StringRequest, data: Any
    ) -> bridge_pb2.StringValue:
        return await _load_gf_store_url(request.value)

    def markers_to_text(self, request: data_pb2.MarkerFile, data: Any) -> bridge_pb2.StringValue:
        return bridge_pb2.StringValue(value=_markers_to_text(request))

    def load_markers(
        self, request: data_pb2.LoadMarkersRequest, data: Any
    ) -> data_pb2.SnufMarkerList:
        return _markers_from_text(request.text)

    def load_station_xml(
        self, request: data_pb2.LoadStationXmlRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        # Same parsing as fetched FDSN responses: sets _STATIONXML (deconvolution)
        # and harvests station coordinates (distance/azimuth sorting, cake).
        return _load_responses_result(request.text)

    def load_stations(
        self, request: data_pb2.LoadStationsRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _load_stations_result(request.text)

    def load_events(
        self, request: data_pb2.LoadEventsRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _load_events_result(request.text)

    def add_cake_model(
        self, request: data_pb2.AddCakeModelRequest, data: Any
    ) -> bridge_pb2.StringValue:
        name = request.name[:-3] if request.name.lower().endswith(".nd") else request.name
        name = name or "model"
        os.makedirs(MODEL_DIR, exist_ok=True)
        with open(_cake_model_path(name), "wb") as f:
            f.write(bytes(request.content))
        return bridge_pb2.StringValue(value=name)

    async def download_waveforms(
        self, request: data_pb2.WaveformDownloadRequest, data: Any
    ) -> scene_pb2.SceneMeta:
        resp = await _pyfetch_fdsn(_dataselect_url(request))
        if resp.status == 204:
            return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
        if not resp.ok:
            raise RuntimeError("dataselect: HTTP %s" % resp.status)
        raw = bytes(await resp.bytes())
        if not raw:
            return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
        return _load_waveform_path_scene(_stage_bytes("fdsn.mseed", raw))

    def ingest_mini_seed(
        self, request: data_pb2.IngestMiniSeedRequest, data: Any
    ) -> scene_pb2.SceneMeta:
        return _append_mseed_scene(
            request.source,
            bytes(request.content),
            request.start_time,
            request.reset,
            request.retain_seconds,
        )

    async def catalog_search(
        self, request: data_pb2.CatalogRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        text = await _fetch_fdsn_text_or_empty(_event_url(request))
        if request.with_mt and text.lstrip().startswith("<"):
            return _parse_events_quakeml(text)
        return _parse_events_result(text)

    async def fetch_stations(
        self, request: data_pb2.StationRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _parse_stations_result(await _fetch_fdsn_text_or_empty(_station_url(request)))

    async def fetch_responses(
        self, request: data_pb2.StationRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _load_responses_result(await _fetch_fdsn_text_or_empty(_response_url(request)))

    def gf_store_label(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.StringValue:
        return bridge_pb2.StringValue(value=gf_store_label(data or ""))


_SERVICER = _BridgeServicer()


async def call(method: str, request: Any = b"", data: Any = None) -> Dict[str, Any]:
    """Dispatch a bridge method through the generated typed servicer; always
    return {"result": <BridgeResult bytes>, "buffer"?: ndarray}. Async because some
    rpcs do network I/O (pyfetch); never raises — any exception becomes a
    BridgeError (the request is decoded off a JS proxy here, before dispatch)."""
    return await bridge_dispatch.dispatch(_SERVICER, method, _raw_bytes(request), data)


async def call_text(method: str, request: Any = b"", data: Any = None) -> Dict[str, Any]:
    """For the worker's GF-store mount, which needs the gfStoreLabel string back:
    dispatch, then unwrap the StringValue (or the error). Never raises — see
    AGENTS.md."""
    env = await call(method, request, data)
    result = bridge_pb2.BridgeResult()
    result.ParseFromString(env["result"])
    which = result.WhichOneof("result")
    if which == "error":
        return {"text": "", "error": f"{result.error.kind}: {result.error.message}"}
    if which == "text":
        return {"text": result.text.value, "error": ""}
    return {"text": "", "error": f"unexpected bridge result variant: {which}"}
