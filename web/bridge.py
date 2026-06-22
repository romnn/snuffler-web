"""Type-checked pyrocko shim that runs inside Pyodide and drives the egui
frontend.

This module is the *only* place that touches pyrocko. It exposes a tiny, stable,
protobuf API to the Pyodide worker (`web/bridge_worker.js`), which in turn is
called from Rust (`src/bridge.rs`) via `web/bridge.js`. The worker-facing
``*_pb`` functions parse and return generated protobuf messages;
legacy JSON wrappers remain only for devtools compatibility.

pyrocko itself is left completely unmodified; we only call its public API
(`pyrocko.trace.Trace`, `pyrocko.io.load`).
"""

from __future__ import annotations

import json
import math
import os
import traceback
from collections import OrderedDict
from dataclasses import dataclass
from datetime import datetime, timezone
from importlib import import_module
from typing import (
    Any,
    Callable,
    Dict,
    Generic,
    Iterator,
    List,
    Mapping,
    Optional,
    Protocol,
    Tuple,
    TypedDict,
    TypeVar,
    cast,
)

import numpy as np
from google.protobuf.message import Message
from pyrocko import io as pio
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import (
    bridge_pb2,
    common_pb2,
    data_pb2,
    scene_pb2,
    snuffling_pb2,
    view_pb2,
)


# The TypedDicts below document the Python-side shape used by pyrocko helpers.
class TraceMeta(TypedDict):
    net: str
    sta: str
    loc: str
    cha: str
    label: str
    tmin: float
    tmax: float
    deltat: float
    nsamples: int


class SceneMeta(TypedDict):
    tmin: float
    tmax: float
    tracks: List[TraceMeta]


class ViewTrack(TypedDict):
    net: str
    sta: str
    loc: str
    cha: str
    label: str
    t0_rel: float
    dt: float  # >0 raw sample spacing, 0 explicit (t_rel, y) points
    count: int
    offset: int
    ymin: float
    ymax: float
    ymean: float
    yscale: float  # gain-adjusted display half-range, grouped per scaling_group
    nf: int  # waterfall raster rows (0 ⇒ plain wiggle, 1 ⇒ Qt-style waterfall)
    nt: int  # waterfall time bins
    fmax: float  # spectrogram max frequency when frequency-resolved


class ViewMeta(TypedDict):
    view_tmin: float
    view_tmax: float
    tracks: List[ViewTrack]


NslcId = Tuple[str, str, str, str]
MessageT = TypeVar("MessageT", bound=Message)
TraceRangeFunc = Callable[..., Mapping[Any, Tuple[Any, Any]]]


class AutoScalerLike(Protocol):
    def make_scale(self, data_range: Tuple[float, float]) -> Tuple[float, float, float]: ...


# The current set of loaded traces (snuffler's "pile").
_TRACES: List[ptrace.Trace] = []

# Trace visibility driven by the ':' command line. `_HIDDEN` is a list of NSLC
# globs to hide; `_REDUCE`, when set, keeps only traces matching it. Matching is
# fnmatch over the "net.sta.loc.cha" string. Data is never dropped, only hidden.
_HIDDEN: List[str] = []
_REDUCE: Optional[str] = None
_DATA_GEN = 0
_RESPONSE_GEN = 0
_VIEW_CACHE_LIMIT = 256
_VIEW_CACHE: "OrderedDict[Tuple[Any, ...], Tuple[float, float, np.ndarray]]" = OrderedDict()
_OLD_DATA_RANGES: Dict[Any, Tuple[float, float]] = {}
_OLD_DATA_RANGE_CONTEXT: Optional[Tuple[Any, ...]] = None


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
    _mark_traces_changed()


def _visible_traces() -> List[ptrace.Trace]:
    import fnmatch

    out = []
    for tr in _TRACES:
        s = ".".join(tr.nslc_id)
        if _REDUCE is not None and not fnmatch.fnmatch(s, _REDUCE):
            continue
        if any(fnmatch.fnmatch(s, pat) for pat in _HIDDEN):
            continue
        out.append(tr)
    return out


# Station coordinates (net, sta) -> (lat, lon, elevation) and the active event
# (lat, lon, depth_m, time), populated by the FDSN fetch helpers below. Used for
# distance/azimuth sorting. The browser does the actual fetch (CORS); all URL
# building, parsing and geo math happen here in pyrocko/Python.
_STATIONS: Dict[Tuple[str, str], Tuple[float, float, float]] = {}
_ACTIVE_EVENT: Optional[Tuple[float, float, float, float]] = None
# Instrument responses (a parsed FDSNStationXML), used by Deconvolution.
_STATIONXML: Any = None
# Catalog events (lat, lon, depth_m, mag, time) for the Map overlay.
_EVENTS: List[Dict[str, float]] = []

# CORS-enabled FDSN web services (verified to allow cross-origin fetch).
_FDSN = {
    "geofon": "https://geofon.gfz.de",
    "earthscope": "https://service.earthscope.org",
    "usgs": "https://earthquake.usgs.gov",
    "orfeus": "https://www.orfeus-eu.org",
}


def _fdsn_base(name: str, default: str = "geofon") -> str:
    return _FDSN.get(name, _FDSN[default])


def _meta(tr: ptrace.Trace) -> TraceMeta:
    net, sta, loc, cha = tr.nslc_id
    return TraceMeta(
        net=net,
        sta=sta,
        loc=loc,
        cha=cha,
        label=".".join(tr.nslc_id),
        tmin=float(tr.tmin),
        tmax=float(tr.tmax),
        deltat=float(tr.deltat),
        nsamples=int(tr.data_len()),
    )


def _scene_dict() -> SceneMeta:
    metas = [_meta(tr) for tr in _visible_traces()]
    tmin = min((m["tmin"] for m in metas), default=0.0)
    tmax = max((m["tmax"] for m in metas), default=1.0)
    return SceneMeta(tmin=tmin, tmax=tmax, tracks=metas)


def _scene_json() -> str:
    return json.dumps(_scene_dict())


def _make_example_scene() -> SceneMeta:
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
    return _scene_dict()


def make_example() -> str:
    _make_example_scene()
    return _scene_json()


def _load_path_scene(name: str, path: str, fmt: str, append: bool = False) -> SceneMeta:
    """Load a file already staged in Pyodide's FS with pyrocko's ``io.load``."""
    del name
    fmt2 = "detect" if fmt in (None, "", "detect") else fmt
    traces = list(pio.load(path, format=fmt2))
    _set_traces(traces, append=append)
    return _scene_dict()


def load_path(name: str, path: str, fmt: str, append: bool = False) -> str:
    _load_path_scene(name, path, fmt, append)
    return _scene_json()


def load_bytes(name: str, data: Any, fmt: str, append: bool = False) -> str:
    """Write the given file bytes into Pyodide's virtual FS and load them with
    pyrocko's `io.load`. `data` arrives as a JS Uint8Array. With `append`, add to
    the current pile instead of replacing it (for multi-file open)."""
    raw = bytes(data.to_py()) if hasattr(data, "to_py") else bytes(data)
    os.makedirs("/data", exist_ok=True)
    path = "/data/" + (name or "data")
    with open(path, "wb") as f:
        f.write(raw)
    fmt2 = "detect" if fmt in (None, "", "detect") else fmt
    return load_path(name, path, fmt2, append=append)


def _decimate(y: np.ndarray, deltat: float, npx: int) -> Tuple[np.ndarray, int, float]:
    """Return raw samples or a signal-preserving min/max envelope.

    ``dt > 0`` means the payload is raw uniformly-spaced samples. ``dt == 0``
    means the payload is interleaved ``(t_rel, y)`` points. The reduced
    representation keeps the first sample, extrema, and last sample of every
    pixel-scale bin in chronological order so the renderer draws a connected
    trace without smoothing over peaks.
    """
    n = len(y)
    npx = max(npx, 16)
    if n <= npx * 4:
        return np.ascontiguousarray(y, dtype="<f4"), n, deltat

    stride = max(1, int(math.ceil(n / npx)))
    bins = int(math.ceil(n / stride))
    out = np.empty(bins * 8, dtype="<f4")
    pos = 0
    for start in range(0, n, stride):
        stop = min(n, start + stride)
        block = y[start:stop]
        if block.size == 0:
            continue
        candidates = [
            start,
            start + int(np.argmin(block)),
            start + int(np.argmax(block)),
            stop - 1,
        ]
        last_idx = -1
        for idx in sorted(candidates):
            if idx == last_idx:
                continue
            out[pos] = idx * deltat
            out[pos + 1] = float(y[idx])
            pos += 2
            last_idx = idx

    out = out[:pos]
    return out, pos // 2, 0.0


def _apply_rotation(traces: List[ptrace.Trace], deg: float) -> List[ptrace.Trace]:
    """Rotate horizontal component pairs (N/E or 1/2) by `deg` degrees, like
    snuffler's rotate control. Aligned, equal-length pairs only."""
    if not deg:
        return traces
    a = math.radians(deg)
    ca, sa = math.cos(a), math.sin(a)
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
                yn = tn.ydata.astype(np.float64)
                ye = te.ydata.astype(np.float64)
                tn2, te2 = tn.copy(), te.copy()
                tn2.set_ydata(ca * yn + sa * ye)
                te2.set_ydata(-sa * yn + ca * ye)
                out.extend((tn2, te2))
                rotated.update((id(tn), id(te)))
        out.extend(t for t in grp if id(t) not in rotated)
    return out


def _remove_response(traces: List[ptrace.Trace], enabled: bool) -> List[ptrace.Trace]:
    """Deconvolve the instrument response to displacement using StationXML fetched
    from an FDSN station service (Data ▸ Download ▸ Fetch responses). A no-op until
    responses are loaded; traces without a matching response pass through."""
    if not enabled or _STATIONXML is None:
        return traces
    out: List[ptrace.Trace] = []
    for tr in traces:
        try:
            resp = _STATIONXML.get_pyrocko_response(
                tr.nslc_id, timespan=(tr.tmin, tr.tmax), fake_input_units="M"
            )
            nyq = 0.5 / float(tr.deltat)
            freqlimits = (0.005, 0.01, 0.8 * nyq, 0.9 * nyq)
            tfade = min(60.0, max(1.0, (tr.tmax - tr.tmin) * 0.25))
            out.append(tr.transfer(tfade, freqlimits, resp, invert=True))
        except Exception:
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


def _sort_order(traces: List[ptrace.Trace], sort: str, subsort: str) -> List[int]:
    """Index order for the traces. Distance/azimuth sorts use station coordinates
    (from a station fetch) + the active event; without them they fall back to name
    order. All geo math is pyrocko's (orthodrome)."""
    perm = _SUBSORT.get(subsort, (0, 1, 2, 3))

    def namekey(tr: ptrace.Trace) -> Tuple[str, str, str, str]:
        nslc = tr.nslc_id
        return (nslc[perm[0]], nslc[perm[1]], nslc[perm[2]], nslc[perm[3]])

    geo = sort in ("distance", "azimuth", "azimuth_blocks", "backazimuth")
    if geo and _ACTIVE_EVENT is not None and _STATIONS:
        from pyrocko import orthodrome as od

        elat, elon, _edep, _et = _ACTIVE_EVENT

        def geokey(tr: ptrace.Trace) -> Tuple[float, Any]:
            st = _STATIONS.get((tr.nslc_id[0], tr.nslc_id[1]))
            if st is None:
                return (1e30, namekey(tr))
            slat, slon, _ele = st
            dist = _dist_deg(elat, elon, slat, slon)
            if sort == "distance":
                return (dist, namekey(tr))
            if sort == "backazimuth":
                return (float(od.azimuth(slat, slon, elat, elon)) % 360.0, dist)
            azi = float(od.azimuth(elat, elon, slat, slon)) % 360.0
            if sort == "azimuth":
                return (azi, dist)
            return (float(int(azi // 30.0)), dist)  # azimuth_blocks: 30° sectors

        return sorted(range(len(traces)), key=lambda i: geokey(traces[i]))

    return sorted(range(len(traces)), key=lambda i: namekey(traces[i]))


def _track_scaling_key(t: "ViewTrack", group: str) -> Any:
    """Grouping key for shared amplitude scaling (snuffler's Scaling menu)."""
    if group == "common":
        return None
    if group == "per_station":
        return (t["net"], t["sta"])
    if group == "per_station_location":
        return (t["net"], t["sta"], t["loc"])
    if group == "per_component":
        return t["cha"]
    return (t["net"], t["sta"], t["loc"], t["cha"])


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


def _scale_context(p: Mapping[str, Any]) -> Tuple[Any, ...]:
    return (
        _DATA_GEN,
        _RESPONSE_GEN,
        p.get("sort", "name"),
        p.get("subsort", "nslc"),
        _float_key(float(p.get("rotate", 0.0) or 0.0)),
        bool(p.get("remove_response", False)),
        bool(p.get("demean", True)),
        bool(p.get("degap", True)),
        _float_key(p.get("highpass")),
        _float_key(p.get("lowpass")),
        bool(p.get("fft_filtering", False)),
        bool(p.get("bandpass_lphp", True)),
        p.get("scaling_base", "minmax"),
        p.get("scaling_group", "individual"),
    )


def _brickwall(
    y: np.ndarray, deltat: float, hp: Optional[float], lp: Optional[float]
) -> np.ndarray:
    """Zero-phase brick-wall band filter in the frequency domain (snuffler's
    'FFT filtering' option)."""
    y = np.asarray(y, dtype=np.float64)
    n = len(y)
    spec = np.fft.rfft(y - y.mean())
    f = np.fft.rfftfreq(n, d=deltat)
    mask = np.ones_like(f)
    if hp is not None:
        mask[f < float(hp)] = 0.0
    if lp is not None:
        mask[f > float(lp)] = 0.0
    return np.fft.irfft(spec * mask, n)


def _filter_seg(
    seg: ptrace.Trace,
    hp: Optional[float],
    lp: Optional[float],
    fft: bool,
    lphp: bool,
) -> None:
    """Apply the main filter to a chopped trace, honoring the FFT-filtering and
    bandpass-as-low+highpass options."""
    try:
        if fft and (hp is not None or lp is not None):
            seg.set_ydata(_brickwall(seg.ydata, float(seg.deltat), hp, lp))
        elif hp is not None and lp is not None and hp < lp:
            if lphp:
                seg.lowpass(4, float(lp))
                seg.highpass(4, float(hp))
            else:
                seg.bandpass(2, float(hp), float(lp))
        elif hp is not None:
            seg.highpass(4, float(hp))
        elif lp is not None:
            seg.lowpass(4, float(lp))
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
) -> Optional[Tuple[np.ndarray, float, float]]:
    seg_tmin, deltat, y = entry
    pad = max(0.0, float(sample_support)) * deltat
    start = max(0, int(math.floor((tmin - pad - seg_tmin) / deltat)))
    stop = min(len(y), int(math.ceil((tmax + pad - seg_tmin) / deltat)) + 1)
    if stop <= start:
        return None
    return y[start:stop], seg_tmin + start * deltat, deltat


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
) -> Tuple[np.ndarray, float]:
    """Snuffler TraceWaterfall data for one trace: normalized amplitude over time."""
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

    return np.ascontiguousarray(data, dtype="<f4"), float(deltat) * float(step)


def _get_view_data(p: Dict[str, Any]) -> Dict[str, Any]:
    """Compute the requested view (wiggle waveforms or a Qt-style waterfall).

    Returns {meta: <ViewMeta dict>, ys: <float32 ndarray>}. For wiggles `ys`
    holds raw uniform samples at close zooms, or explicit `(t_rel, y)` extrema
    points for reduced long-window views. For the waterfall it holds per-track
    normalized amplitude strips for the requested track page.
    """
    tmin = float(p["tmin"])
    tmax = float(p["tmax"])
    npx = int(p["npx"])
    track_offset = max(0, int(p.get("track_offset", 0) or 0))
    track_count = max(0, int(p.get("track_count", 0) or 0))
    track_end = track_offset + track_count
    hp: Optional[float] = p.get("highpass")
    lp: Optional[float] = p.get("lowpass")
    sort = p.get("sort", "name")
    subsort = p.get("subsort", "nslc")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    remove_response = bool(p.get("remove_response", False))
    mode = p.get("mode", "wiggle")
    demean = bool(p.get("demean", True))
    degap = bool(p.get("degap", True))
    allow_ds = bool(p.get("allow_downsampling", True))
    fft = bool(p.get("fft_filtering", False))
    lphp = bool(p.get("bandpass_lphp", True))
    base = p.get("scaling_base", "minmax")
    scaling_group = p.get("scaling_group", "individual")
    gain = float(p.get("gain", 1.0) or 1.0)
    if gain <= 0.0 or not math.isfinite(gain):
        gain = 1.0
    fix_scale_ranges = bool(p.get("fix_scale_ranges", False))
    show_scale_ranges = bool(p.get("show_scale_ranges", False))
    show_scale_axes = bool(p.get("show_scale_axes", False))
    wf_abs = bool(p.get("wf_abs", False))
    wf_integrate = bool(p.get("wf_integrate", False))
    scale_context = _scale_context(p)

    traces = list(_visible_traces())
    if degap:
        try:
            traces = ptrace.degapper(traces)
        except Exception:
            pass
    order = _sort_order(traces, sort, subsort)
    work = [traces[i] for i in order]
    work = _apply_rotation(work, rotate)
    work = _remove_response(work, remove_response)

    ys_parts: List[np.ndarray] = []
    tracks: List[ViewTrack] = []
    range_traces: List[ptrace.Trace] = []
    offset = 0
    view_idx = 0
    for tr_idx, tr in enumerate(work):
        display_tmin, display_tmax = _display_time_span(tr)
        if display_tmax < tmin or display_tmin > tmax:
            continue

        net, sta, loc, cha = tr.nslc_id
        label = ".".join(tr.nslc_id)
        want_samples = track_offset <= view_idx < track_end
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
                    yview, ytmin, deltat = sliced
                    strip, dt = _waterfall_trace(yview, deltat, npx, wf_abs, wf_integrate)
                    if strip.size:
                        flat = strip
                        nf = 1
                        nt = int(strip.size)
                        if wf_abs:
                            vmin = float(strip.min())
                            vmax = float(strip.max())
                        t0_rel = ytmin - tmin
            tracks.append(
                ViewTrack(
                    net=net,
                    sta=sta,
                    loc=loc,
                    cha=cha,
                    label=label,
                    t0_rel=t0_rel,
                    dt=dt,
                    count=int(nf * nt),
                    offset=int(offset),
                    ymin=vmin,
                    ymax=vmax,
                    ymean=0.0,
                    yscale=1.0,
                    nf=int(nf),
                    nt=int(nt),
                    fmax=fmax,
                )
            )
            if want_samples and flat.size:
                ys_parts.append(np.ascontiguousarray(flat, dtype="<f4"))
                offset += int(nf * nt)
        else:
            entry = _processed_trace(tr, track_key, tmin, tmax, hp, lp, demean, fft, lphp)
            if entry is None:
                continue
            sliced = _slice_segment(entry, tmin, tmax, 4.0)
            if sliced is None:
                continue
            yview, ytmin, deltat = sliced
            range_traces.append(_range_trace(tr.nslc_id, ytmin, deltat, yview))
            count = 0
            dt = deltat
            t0_rel = ytmin - tmin
            payload_len = 0
            if want_samples:
                target = npx if allow_ds else len(yview)
                y, count, dt = _decimate(yview, deltat, target)
                ys_parts.append(np.ascontiguousarray(y, dtype="<f4"))
                payload_len = int(y.size)
            tracks.append(
                ViewTrack(
                    net=net,
                    sta=sta,
                    loc=loc,
                    cha=cha,
                    label=label,
                    t0_rel=t0_rel,
                    dt=dt,
                    count=int(count),
                    offset=int(offset),
                    ymin=0.0,
                    ymax=1.0,
                    ymean=0.0,
                    yscale=1.0,
                    nf=0,
                    nt=0,
                    fmax=0.0,
                )
            )
            if want_samples:
                offset += payload_len

    if mode == "waterfall":
        if wf_abs:
            lo = min((t["ymin"] for t in tracks if t["nf"]), default=0.0)
            hi = max((t["ymax"] for t in tracks if t["nf"]), default=1.0)
            if hi <= lo:
                hi = lo + 1e-12
            for t in tracks:
                if t["nf"]:
                    t["ymin"] = lo
                    t["ymax"] = hi
    elif range_traces:
        global _OLD_DATA_RANGE_CONTEXT
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
        data_ranges: Dict[Any, Tuple[float, float]] = {
            key: (float(lo), float(hi)) for key, (lo, hi) in raw_ranges.items()
        }
        if fix_scale_ranges and _OLD_DATA_RANGE_CONTEXT == scale_context:
            data_ranges.update(_OLD_DATA_RANGES)
        elif not fix_scale_ranges:
            _OLD_DATA_RANGES.clear()
            _OLD_DATA_RANGES.update(data_ranges)
            _OLD_DATA_RANGE_CONTEXT = scale_context

        auto_scaler = cast(
            Callable[..., AutoScalerLike], getattr(import_module("pyrocko.plot"), "AutoScaler")
        )
        yscaler = auto_scaler(
            no_exp_interval=(-3, 2),
            approx_ticks=15.0,
            snap=(show_scale_ranges or show_scale_axes) and not show_scale_axes,
        )
        for t in tracks:
            if t["nf"]:
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
            t["ymin"] = ymin
            t["ymax"] = ymax
            t["ymean"] = 0.5 * (ymin + ymax)
            t["yscale"] = max(gain * half, 1e-12)

    ys = (
        np.ascontiguousarray(np.concatenate(ys_parts), dtype="<f4")
        if ys_parts
        else np.zeros(0, dtype="<f4")
    )
    meta: ViewMeta = ViewMeta(view_tmin=tmin, view_tmax=tmax, tracks=tracks)
    return {"meta": meta, "ys": ys}


def get_view(params_json: str) -> Dict[str, Any]:
    res = _get_view_data(json.loads(params_json))
    return {"meta": json.dumps(res["meta"]), "ys": res["ys"]}


def _snuffling_section(params: Dict[str, Any], name: str) -> Dict[str, Any]:
    snufflings = params.get("snufflings") or {}
    section = snufflings.get(name) if isinstance(snufflings, dict) else None
    return section if isinstance(section, dict) else {}


def _cfg(section: Dict[str, Any], params: Dict[str, Any], key: str, default: Any) -> Any:
    return section.get(key, params.get(key, default))


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


def _get_spectrum_data(p: Dict[str, Any]) -> Dict[str, Any]:
    """Amplitude spectrum (|rFFT|) of each visible trace — a built-in snuffling.

    Returns {meta: <dict>, ys: <float32 bytes>} with per-track log10-amplitude
    sampled on a shared log-frequency axis [fmin, fmax]."""
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    spectrum_params = _snuffling_section(p, "spectrum")
    smoothing = bool(_cfg(spectrum_params, p, "spec_smoothing", False))
    smoothing_bandwidth = float(_cfg(spectrum_params, p, "spec_smoothing_bandwidth", 40.0) or 40.0)
    hp, lp = p.get("highpass"), p.get("lowpass")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    nbins = 400

    vis = _visible_traces()
    order = sorted(range(len(vis)), key=lambda i: vis[i].nslc_id)
    work = _apply_rotation([vis[i] for i in order], rotate)
    track_offset = max(0, int(p.get("track_offset", 0) or 0))
    track_count = max(0, int(p.get("track_count", len(work)) or len(work)))
    work = work[track_offset : track_offset + track_count]

    fmin_global, fmax_global = 1e30, 0.0
    specs = []
    for tr in work:
        try:
            seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:
            continue
        if seg.ydata is None or len(seg.ydata) < 8:
            continue
        seg.ydata = seg.ydata.astype(np.float64)
        try:
            if hp is not None and lp is not None and hp < lp:
                seg.bandpass(2, float(hp), float(lp))
            elif hp is not None:
                seg.highpass(4, float(hp))
            elif lp is not None:
                seg.lowpass(4, float(lp))
        except Exception:
            pass
        y = seg.ydata - seg.ydata.mean()
        amp = np.abs(np.fft.rfft(y)) * float(seg.deltat)
        freq = np.fft.rfftfreq(len(y), d=float(seg.deltat))
        if len(freq) > 2:
            fmin_global = min(fmin_global, freq[1])
            fmax_global = max(fmax_global, freq[-1])
        specs.append((".".join(tr.nslc_id), freq, amp))

    if not specs or fmax_global <= 0:
        return {"meta": {"fmin": 0.1, "fmax": 1.0, "tracks": []}, "ys": np.zeros(0, "<f4")}

    fmin_global = max(fmin_global, fmax_global * 1e-4)
    grid = np.logspace(np.log10(fmin_global), np.log10(fmax_global), nbins)
    ys_parts, tracks, offset = [], [], 0
    for label, freq, amp in specs:
        resamp = np.interp(grid, freq, amp, left=0.0, right=0.0)
        if smoothing:
            resamp = _konno_ohmachi_smooth(resamp, grid, smoothing_bandwidth)
        logamp = np.log10(resamp + 1e-20).astype("<f4")
        tracks.append(
            {
                "label": label,
                "offset": int(offset),
                "count": int(nbins),
                "ymin": float(logamp.min()),
                "ymax": float(logamp.max()),
            }
        )
        ys_parts.append(logamp)
        offset += nbins
    ys = np.concatenate(ys_parts) if ys_parts else np.zeros(0, "<f4")
    meta = {"fmin": float(fmin_global), "fmax": float(fmax_global), "tracks": tracks}
    return {"meta": meta, "ys": np.ascontiguousarray(ys, dtype="<f4")}


def get_spectrum(params_json: str) -> Dict[str, Any]:
    res = _get_spectrum_data(json.loads(params_json))
    return {"meta": json.dumps(res["meta"]), "ys": res["ys"]}


def _run_snuffling_result(name: str, p: Dict[str, Any]) -> Dict[str, Any]:
    """Run a built-in snuffling over the current window."""
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    hp, lp = p.get("highpass"), p.get("lowpass")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    fft = bool(p.get("fft_filtering", False))
    lphp = bool(p.get("bandpass_lphp", True))

    if name == "cake":
        return _cake_run(p)

    vis = _visible_traces()
    order = _sort_order(vis, p.get("sort", "name"), p.get("subsort", "nslc"))
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
        return {"text": text, "markers": []}

    if name == "stalta":
        params = _snuffling_section(p, "stalta")
        shp = _cfg(params, p, "stalta_highpass", None)
        slp = _cfg(params, p, "stalta_lowpass", None)
        tshort = float(_cfg(params, p, "stalta_swin", 30.0) or 30.0)
        ratio = float(_cfg(params, p, "stalta_ratio", 3.0) or 3.0)
        level = float(_cfg(params, p, "stalta_level", 0.5) or 0.5)
        variant = str(_cfg(params, p, "stalta_variant", "centered") or "centered")
        scaling_name = str(_cfg(params, p, "stalta_scalingmethod", "[0-1]") or "[0-1]")
        apply_to_sum = bool(_cfg(params, p, "stalta_apply_to_sum", False))
        show_level_traces = bool(_cfg(params, p, "stalta_show_level_traces", False))
        scaling_method = {
            "[0-1]": 1,
            "[-1/ratio,1]": 2,
            "[-1/ratio,1] clipped to [0,1]": 3,
        }.get(scaling_name, 1)
        markers = []
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
                markers.append(
                    {
                        "t": float(tp),
                        "phase": None,
                        "kind": 0,
                        "nslc": list(tr.nslc_id),
                    }
                )
        if apply_to_sum and processed:
            min_len = min(len(c.ydata) for c in processed)
            if min_len >= 64:
                try:
                    csum = processed[0].copy()
                    csum.ydata = np.mean([c.ydata[:min_len] for c in processed], axis=0)
                    tpeaks, _apeaks = csum.peaks(level, tshort)
                    for tp in tpeaks:
                        markers.append({"t": float(tp), "phase": None, "kind": 0})
                except Exception:
                    pass
        scene = None
        if show_level_traces:
            _TRACES.extend(derived)
        if had_level_traces or show_level_traces:
            _mark_traces_changed()
            scene = _scene_dict()
        return {
            "show_window": False,
            "text": "STA/LTA: %d detection(s) (short=%.3gs, ratio=%g, level=%g)"
            % (len(markers), tshort, ratio, level),
            "markers": markers,
            "scene": scene,
        }

    if name == "polarization":
        # Covariance-eigenvalue polarization analysis (Jurkevics, 1988) per
        # 3-component station over the visible window. Pure numpy.
        params = _snuffling_section(p, "polarization")
        php = _cfg(params, p, "polarization_highpass", None)
        plp = _cfg(params, p, "polarization_lowpass", None)
        length = float(_cfg(params, p, "polarization_t_length", tmax - tmin) or (tmax - tmin))
        offset = float(_cfg(params, p, "polarization_ft_offset", 0.0) or 0.0)
        azimuth = float(_cfg(params, p, "polarization_azimuth", 0.0) or 0.0)
        if 0.0 < length < (tmax - tmin):
            center = 0.5 * (tmin + tmax) + offset * length
            ptmin, ptmax = center - 0.5 * length, center + 0.5 * length
        else:
            ptmin, ptmax = tmin, tmax

        groups: Dict[Tuple[str, str, str], Dict[str, Any]] = {}
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
        return {"text": text, "markers": []}

    return {"text": "Unknown snuffling: %s" % name, "markers": []}


def run_snuffling(name: str, params_json: str) -> str:
    return json.dumps(_run_snuffling_result(name, json.loads(params_json)))


# --- FDSN web services (network) -------------------------------------------
# The browser performs the actual fetch (it has CORS); these helpers only build
# the request URLs and parse the responses, so all logic stays in Python.


def _dataselect_url(p: Dict[str, Any]) -> str:
    """FDSN dataselect (waveform) query URL."""
    base = _fdsn_base(p.get("datacenter", "geofon"))
    q = "&".join(
        [
            "net=%s" % (p.get("net") or "*"),
            "sta=%s" % (p.get("sta") or "*"),
            "loc=%s" % (p.get("loc") or "*"),
            "cha=%s" % (p.get("cha") or "*"),
            "starttime=%s" % p["start"],
            "endtime=%s" % p["end"],
            "nodata=204",
        ]
    )
    return "%s/fdsnws/dataselect/1/query?%s" % (base, q)


def dataselect_url(params_json: str) -> str:
    return _dataselect_url(json.loads(params_json))


def load_waveform_bytes(data: Any) -> str:
    """Load fetched MiniSEED bytes into the pile and return the scene JSON."""
    raw = bytes(data.to_py()) if hasattr(data, "to_py") else bytes(data)
    if not raw:
        return json.dumps(SceneMeta(tmin=0.0, tmax=1.0, tracks=[]))
    os.makedirs("/data", exist_ok=True)
    path = "/data/fdsn.mseed"
    with open(path, "wb") as f:
        f.write(raw)
    return load_waveform_path(path)


def _load_waveform_path_scene(path: str) -> SceneMeta:
    """Load staged FDSN MiniSEED bytes into the pile and return the scene."""
    if not path:
        return SceneMeta(tmin=0.0, tmax=1.0, tracks=[])
    traces = list(pio.load(path, format="mseed"))
    _set_traces(traces)
    return _scene_dict()


def load_waveform_path(path: str) -> str:
    """Load staged FDSN MiniSEED bytes into the pile and return the scene JSON."""
    return json.dumps(_load_waveform_path_scene(path))


def _event_url(p: Dict[str, Any]) -> str:
    """FDSN event (catalog) query URL (text format)."""
    base = _fdsn_base(p.get("datacenter", "usgs"))
    q = "&".join(
        [
            "format=text",
            "starttime=%s" % p["start"],
            "endtime=%s" % p["end"],
            "minmagnitude=%s" % p.get("minmag", 5.0),
            "orderby=time",
            "limit=1000",
        ]
    )
    return "%s/fdsnws/event/1/query?%s" % (base, q)


def event_url(params_json: str) -> str:
    return _event_url(json.loads(params_json))


def _ftime(s: str) -> float:
    s = s.strip().rstrip("Z")
    try:
        return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()
    except Exception:
        return 0.0


def _parse_events_result(text: str) -> Dict[str, Any]:
    """Parse FDSN event 'text' format into event markers; set the active event
    (largest magnitude) for distance/azimuth sorting."""
    global _ACTIVE_EVENT, _EVENTS
    markers = []
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
        markers.append(
            {
                "t": t,
                "phase": ("M%.1f %s" % (mag, name)).strip()[:48],
                "kind": 0,
                "is_event": True,
            }
        )
        _EVENTS.append({"lat": lat, "lon": lon, "depth": depth, "mag": mag, "time": t})
        lines.append("M%.1f  %s  %s" % (mag, when, name))
        if best is None or mag > best[0]:
            best = (mag, lat, lon, depth, t)
    if best is not None:
        _ACTIVE_EVENT = (best[1], best[2], best[3], best[4])
        header = (
            "%d event(s) found; active event M%.1f (distance sort enabled).\n"
            "Jumped the view to the events — load waveforms for this time to "
            "see them on data." % (len(markers), best[0])
        )
    else:
        header = "No events found in this time window."
    body = "\n".join(lines[:30])
    return {"text": header + ("\n\n" + body if body else ""), "markers": markers}


def parse_events(text: str) -> str:
    return json.dumps(_parse_events_result(text))


def _station_url(p: Dict[str, Any]) -> str:
    """FDSN station query URL (text format, station level)."""
    base = _fdsn_base(p.get("datacenter", "geofon"))
    q = "&".join(
        [
            "net=%s" % (p.get("net") or "*"),
            "sta=%s" % (p.get("sta") or "*"),
            "cha=%s" % (p.get("cha") or "*"),
            "level=station",
            "format=text",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def station_url(params_json: str) -> str:
    return _station_url(json.loads(params_json))


def _parse_stations_result(text: str) -> Dict[str, Any]:
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
    return {"text": "%d station(s) with coordinates loaded" % n, "markers": []}


def parse_stations(text: str) -> str:
    return json.dumps(_parse_stations_result(text))


def _response_url(p: Dict[str, Any]) -> str:
    """FDSN station query URL at response level (StationXML with instrument
    responses, for deconvolution)."""
    base = _fdsn_base(p.get("datacenter", "geofon"))
    q = "&".join(
        [
            "net=%s" % (p.get("net") or "*"),
            "sta=%s" % (p.get("sta") or "*"),
            "cha=%s" % (p.get("cha") or "*"),
            "level=response",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def response_url(params_json: str) -> str:
    return _response_url(json.loads(params_json))


def _load_responses_result(xml_text: str) -> Dict[str, Any]:
    """Parse fetched StationXML into instrument responses (for Deconvolution) and
    also harvest station coordinates (for distance/azimuth sorting)."""
    global _STATIONXML, _RESPONSE_GEN
    if not xml_text.strip():
        return {"text": "no station metadata returned", "markers": []}
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
    return {
        "text": "%d channel response(s) loaded — enable Options ▸ Deconvolution" % nchan,
        "markers": [],
    }


def load_responses(xml_text: str) -> str:
    return json.dumps(_load_responses_result(xml_text))


def _map_data_result() -> Dict[str, Any]:
    """Station + event coordinates for the Map overlay (no network — reads what
    the FDSN fetches already loaded)."""
    stations = [
        {"label": "%s.%s" % (net, sta), "lat": lat, "lon": lon}
        for (net, sta), (lat, lon, _elev) in _STATIONS.items()
    ]
    events = [
        {"lat": e["lat"], "lon": e["lon"], "mag": e["mag"], "time": e["time"]} for e in _EVENTS
    ]
    return {"stations": stations, "events": events}


def map_data() -> str:
    return json.dumps(_map_data_result())


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


def _gf_store_info_result(store_dir: str) -> Dict[str, Any]:
    """Read a mounted GF store's config (no ``store_ext`` / network needed) and
    register it so its embedded 1D earth model is selectable in Cake Phase.

    Returns ``{id, label, sample_rate, has_earthmodel}`` or ``{error}``.
    """
    try:
        gfstore, _gfmeta = _gf_modules()
        cfg = gfstore.Store(store_dir).config
        sid = str(cfg.id)
        _GF_STORES[sid] = store_dir
        return {
            "id": sid,
            "label": GF_STORE_PREFIX + sid,
            "sample_rate": float(getattr(cfg, "sample_rate", 0.0) or 0.0),
            "has_earthmodel": getattr(cfg, "earthmodel_1d", None) is not None,
        }
    except Exception as e:
        return {"error": "%s: %s" % (type(e).__name__, e)}


def gf_store_label(store_dir: str) -> str:
    info = _gf_store_info_result(store_dir)
    if info.get("error"):
        raise ValueError(str(info["error"]))
    return str(info.get("label", ""))


def gf_store_info(store_dir: str) -> str:
    return json.dumps(_gf_store_info_result(store_dir))


def _markers_to_text(markers: List[Dict[str, Any]]) -> str:
    """Serialize markers to pyrocko's snuffler marker-file format."""
    pm = _marker_module()

    ms = []
    for m in markers:
        tmin = float(m["tmin"])
        tmax = float(m.get("tmax", tmin))
        kind = int(m.get("kind", 0))
        nslc = m.get("nslc")
        ids = [tuple(nslc)] if nslc else []
        phase = m.get("phase")
        polarity = m.get("polarity")
        if polarity is not None:
            polarity = int(polarity)
        # A polarity is only meaningful on a PhaseMarker, so promote any marker
        # that carries one even if it has no phase name yet.
        if phase or polarity is not None:
            ms.append(pm.PhaseMarker(ids, tmin, tmax, kind, phasename=phase, polarity=polarity))
        else:
            ms.append(pm.Marker(ids, tmin, tmax, kind))
    path = "/tmp/markers_out.txt"
    pm.save_markers(ms, path)
    with open(path) as f:
        return f.read()


def markers_to_text(markers_json: str) -> str:
    return _markers_to_text(json.loads(markers_json))


def _markers_from_text(text: str) -> List[Dict[str, Any]]:
    """Parse a pyrocko marker file into the JSON the egui side adds."""
    pm = _marker_module()

    path = "/tmp/markers_in.txt"
    with open(path, "w") as f:
        f.write(text)
    try:
        ms = pm.load_markers(path)
    except Exception:
        return []
    out = []
    for mk in ms:
        ids = mk.get_nslc_ids()
        nslc = list(ids[0]) if ids else None
        phase = None
        polarity = None
        if isinstance(mk, pm.PhaseMarker):
            try:
                phase = mk.get_phasename()
            except Exception:
                phase = None
            try:
                polarity = mk.get_polarity()
            except Exception:
                polarity = None
        out.append(
            {
                "t": float(mk.tmin),
                "tmax": float(mk.tmax),
                "kind": int(mk.kind or 0),
                "phase": phase,
                "polarity": polarity,
                "nslc": nslc,
                "is_event": isinstance(mk, pm.EventMarker),
            }
        )
    return out


def markers_from_text(text: str) -> str:
    return json.dumps(_markers_from_text(text))


# Directory where bridge.js stages the cake earth models (tiny .nd text files
# shipped as build assets and written into the Pyodide FS on first use; the
# minimal wheel has no data dir, so cake.builtin_models() is empty and models
# must be loaded by path). See ensureEarthModels in bridge.js.
MODEL_DIR = "/earthmodels"


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


def _cake_section(p: Dict[str, Any]) -> Tuple[str, List[str], float, bool, str]:
    """Extract the Cake Phase knobs from the snuffling params."""
    c = _snuffling_section(p, "cake")
    model = str(c.get("cake_model", "ak135-f-continental.m"))
    phases = [str(x) for x in (c.get("cake_phases") or ["~P"])]
    tshift = float(c.get("cake_tshift", 0.0) or 0.0)
    use_station_depth = bool(c.get("cake_use_station_depth", False))
    mode = str(c.get("cake_mode", "run") or "run")
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


def _cake_model_geometry(
    cake: Any,
    mod: Any,
) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    """(discontinuities, layers) of a cake model in km, for the egui plots.

    Mirrors :py:func:`pyrocko.plot.cake_plot.sketch_model`: water layers have
    ``vs == 0`` top and bottom; gradient layers are ``cake.GradientLayer``.
    """
    discs = [{"z_km": float(d.z) / 1000.0, "name": d.name} for d in mod.discontinuities()]
    layers = [
        {
            "ztop_km": float(lay.ztop) / 1000.0,
            "zbot_km": float(lay.zbot) / 1000.0,
            "water": bool(lay.mtop.vs == 0.0 and lay.mbot.vs == 0.0),
            "gradient": isinstance(lay, cake.GradientLayer),
            "name": lay.name,
        }
        for lay in mod.layers()
    ]
    return discs, layers


def _cake_run(p: Dict[str, Any]) -> Dict[str, Any]:
    """Theoretical arrivals → phase markers (the Cake Phase 'Run' action).

    All travel-time computation is pyrocko's :py:mod:`pyrocko.cake` (pure
    numpy/scipy ray tracing) and the geometry is :py:func:`_dist_deg`; this only
    marshals the rays into markers and a per-pick angle table.
    """
    model, phase_names, tshift, use_station_depth, _mode = _cake_section(p)
    if _ACTIVE_EVENT is None:
        return {
            "text": "No active event. Run a catalog search first (Data ▸ Catalog search).",
            "markers": [],
        }
    if not _STATIONS:
        return {
            "text": "No station coordinates. Fetch stations first "
            "(Data ▸ Download ▸ Fetch stations).",
            "markers": [],
        }

    from pyrocko import cake

    _elat, _elon, edepth, etime = _ACTIVE_EVENT
    mod = _cake_load_model(cake, model)
    wanted, errors = _cake_wanted_phases(cake, phase_names)

    markers: List[Dict[str, Any]] = []
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
            markers.append(
                {
                    "t": etime + tt,
                    "tmax": etime + tt,
                    "kind": _phase_color_id(name),
                    "phase": name,
                    "nslc": [net, sta, "", "*"],
                }
            )
            lines.append(
                "%-14s %-8s %12.3f %8.1f %8.1f" % ("%s.%s" % (net, sta), name, tt, inc, tko)
            )

    header = "Cake arrivals (%s): %d pick(s) at %d station(s)." % (model, len(markers), nstations)
    if errors:
        header += "\nSkipped phases: " + ", ".join(errors)
    text = header + "\n\n" + "\n".join(lines) if len(markers) else header
    return {"text": text, "markers": markers}


def _cake_plot_result(p: Dict[str, Any]) -> Dict[str, Any]:
    """Numeric data for the Cake Phase 'Plot Model' / 'Plot Rays' windows.

    All geometry is pyrocko's cake ray tracer; this only converts to the km
    units the egui plot expects and serialises polylines — the Rust side renders
    (no matplotlib, unlike the Qt snuffling). Returns a tagged JSON object whose
    ``kind`` is ``"model"`` or ``"rays"``.
    """
    model, phase_names, _tshift, use_station_depth, mode = _cake_section(p)

    from pyrocko import cake

    mod = _cake_load_model(cake, model)
    if mode == "plot_model":
        return _cake_plot_model(cake, mod, model)
    return _cake_plot_rays(cake, mod, model, phase_names, use_station_depth)


def cake_plot(params_json: str) -> str:
    return json.dumps(_cake_plot_result(json.loads(params_json)))


def _cake_plot_model(cake: Any, mod: Any, title: str) -> Dict[str, Any]:
    z = np.asarray(mod.profile("z"), dtype=np.float64) / 1000.0
    vp = np.asarray(mod.profile("vp"), dtype=np.float64) / 1000.0
    vs = np.asarray(mod.profile("vs"), dtype=np.float64) / 1000.0

    def interleave(vals: np.ndarray) -> List[float]:
        out: List[float] = []
        for vi, zi in zip(vals, z, strict=True):
            out.append(float(vi))
            out.append(float(zi))
        return out

    discs, layers = _cake_model_geometry(cake, mod)
    return {
        "kind": "model",
        "title": title,
        "vp": interleave(vp),
        "vs": interleave(vs),
        "discontinuities": discs,
        "layers": layers,
        "vmax_km": float(np.nanmax(vp)) if vp.size else 0.0,
        "zmax_km": float(np.nanmax(z)) if z.size else 0.0,
    }


def _cake_plot_rays(
    cake: Any, mod: Any, title: str, phase_names: List[str], use_station_depth: bool
) -> Dict[str, Any]:
    if _ACTIVE_EVENT is None or not _STATIONS:
        return {
            "kind": "rays",
            "title": "Ray Paths",
            "rays": [],
            "discontinuities": [],
            "layers": [],
            "legend": [],
            "source": {"x_km": 0.0, "z_km": 0.0},
            "receivers": [],
            "xmin_km": 0.0,
            "xmax_km": 1.0,
            "zmax_km": 1.0,
            "text": "No active event / stations for ray paths.",
        }

    _elat, _elon, edepth, _etime = _ACTIVE_EVENT
    wanted, _errors = _cake_wanted_phases(cake, phase_names)
    kmf = cake.d2r * cake.earthradius / cake.km  # epicentral degrees -> km

    allrays = []
    receivers: List[Dict[str, float]] = []
    xmax = 0.0
    zmax = 0.0
    for _net, _sta, dist_deg, elev in _cake_stations():
        rdepth = max(0.0, -float(elev)) if use_station_depth else 0.0
        rays = mod.arrivals(phases=wanted, distances=[dist_deg], zstart=edepth, zstop=rdepth)
        allrays.extend(rays)
        xkm = float(dist_deg) * kmf
        receivers.append({"x_km": xkm, "z_km": float(rdepth) / cake.km})
        xmax = max(xmax, xkm)

    out_rays = []
    legend_names: List[str] = []
    for ray in allrays:
        name = ray.given_phase().name
        if name not in legend_names:
            legend_names.append(name)
        fanz, fanx, _fant = ray.zxt_path_subdivided()
        segs = []
        for zs, xs in zip(fanz, fanx, strict=True):
            seg: List[float] = []
            for xi, zi in zip(np.asarray(xs), np.asarray(zs), strict=True):
                xkm = float(xi) * kmf
                zkm = float(zi) / cake.km
                seg.append(xkm)
                seg.append(zkm)
                xmax = max(xmax, xkm)
                zmax = max(zmax, zkm)
            if seg:
                segs.append(seg)
        if segs:
            out_rays.append({"color": _phase_color_id(name), "phase": name, "segs": segs})

    discs, layers = _cake_model_geometry(cake, mod)
    return {
        "kind": "rays",
        "title": "Ray Paths (%s)" % title,
        "rays": out_rays,
        "discontinuities": discs,
        "layers": layers,
        "source": {"x_km": 0.0, "z_km": float(edepth) / cake.km},
        "receivers": receivers,
        "legend": [{"phase": n, "color": _phase_color_id(n)} for n in legend_names],
        "xmin_km": 0.0,
        "xmax_km": xmax if xmax > 0.0 else 1.0,
        "zmax_km": zmax if zmax > 0.0 else 1.0,
    }


def _command_glob(scope: Optional[str], pattern: str) -> str:
    """Build an "n.s.l.c" fnmatch glob from an optional scope + pattern."""
    idx = {"n": 0, "s": 1, "l": 2, "c": 3}
    if scope in idx:
        fields = ["*", "*", "*", "*"]
        fields[idx[scope]] = pattern
        return ".".join(fields)
    # No scope: a bare token without dots matches anywhere in the id.
    return pattern if "." in pattern else "*%s*" % pattern


def _command_result(line: str) -> Dict[str, Any]:
    """Execute a snuffler ':' command line. Returns JSON describing the action:
    {"action": "goto", "t": <epoch>} | {"action": "scene", "scene": <SceneMeta>}
    | {"action": "error", "message": ...} | {"action": "none"}.

    Supported: ``goto <time>``; ``hide [n|s|l|c] <pattern>``;
    ``unhide [[n|s|l|c] <pattern>]``; ``n|s|l|c <pattern>`` (reduce to matching).
    """
    global _HIDDEN, _REDUCE
    parts = line.split()
    if not parts:
        return {"action": "none"}
    cmd, args = parts[0], parts[1:]

    if cmd == "goto":
        if not args:
            return {"action": "error", "message": "usage: goto <time>"}
        from pyrocko import util

        try:
            t = float(util.str_to_time_fillup(" ".join(args)))
        except Exception as e:
            return {"action": "error", "message": "bad time: %s" % e}
        return {"action": "goto", "t": t}

    if cmd in ("hide", "unhide"):
        scope, rest = None, args
        if args and args[0] in ("n", "s", "l", "c"):
            scope, rest = args[0], args[1:]
        pattern = rest[0] if rest else None
        if cmd == "hide":
            if pattern is None:
                return {
                    "action": "error",
                    "message": "usage: hide [n|s|l|c] <pattern>",
                }
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
        return {"action": "scene", "scene": _scene_dict()}

    if cmd in ("n", "s", "l", "c"):
        _REDUCE = _command_glob(cmd, args[0] if args else "*")
        return {"action": "scene", "scene": _scene_dict()}

    return {"action": "error", "message": "unknown command: %s" % cmd}


def command(line: str) -> str:
    return json.dumps(_command_result(line))


def _rms_scene(p: Dict[str, Any]) -> SceneMeta:
    """Blockwise root-mean-square derived traces (pyrocko's RMS snuffling).

    For each visible trace, RMS is computed over fixed-length blocks across the
    visible window and emitted as a new low-rate trace (channel suffix '-RMS').
    With grouping on, a station's components are combined into one RMS trace.
    Appends the derived traces and returns the updated scene JSON. All numpy.
    """
    global _TRACES
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    params = _snuffling_section(p, "rms")
    hp = _cfg(params, p, "rms_highpass", None)
    lp = _cfg(params, p, "rms_lowpass", None)
    block = _cfg(params, p, "rms_block", None)
    block = float(block) if block else max(1.0, (tmax - tmin) / 100.0)
    group = bool(_cfg(params, p, "rms_group", True))
    logrms = bool(_cfg(params, p, "rms_log", False))

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
        return _scene_dict()

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
        return _scene_dict()

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
    return _scene_dict()


def rms(params_json: str) -> str:
    _rms_scene(json.loads(params_json))
    return _scene_json()


def _raw_bytes(value: Any) -> bytes:
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value)
    if hasattr(value, "to_py"):
        return bytes(value.to_py())
    return bytes(value)


def _parse_pb(cls: type[MessageT], value: Any) -> MessageT:
    msg = cls()
    msg.ParseFromString(_raw_bytes(value))
    return msg


def _nslc_pb(value: Any) -> common_pb2.Nslc:
    msg = common_pb2.Nslc()
    if value:
        msg.net = str(value[0] if len(value) > 0 else "")
        msg.sta = str(value[1] if len(value) > 1 else "")
        msg.loc = str(value[2] if len(value) > 2 else "")
        msg.cha = str(value[3] if len(value) > 3 else "")
    return msg


def _scene_msg(data: Mapping[str, Any]) -> scene_pb2.SceneMeta:
    msg = scene_pb2.SceneMeta()
    msg.tmin = float(data.get("tmin", 0.0) or 0.0)
    msg.tmax = float(data.get("tmax", 1.0) or 1.0)
    for tr in data.get("tracks", []):
        item = msg.tracks.add()
        item.net = str(tr.get("net", ""))
        item.sta = str(tr.get("sta", ""))
        item.loc = str(tr.get("loc", ""))
        item.cha = str(tr.get("cha", ""))
        item.label = str(tr.get("label", ""))
        item.tmin = float(tr.get("tmin", 0.0) or 0.0)
        item.tmax = float(tr.get("tmax", 0.0) or 0.0)
        item.deltat = float(tr.get("deltat", 0.0) or 0.0)
        item.nsamples = int(tr.get("nsamples", 0) or 0)
    return msg


def _scene_pb(data: Mapping[str, Any]) -> bytes:
    return _scene_msg(data).SerializeToString()


def _view_params_dict(msg: view_pb2.ViewParams) -> Dict[str, Any]:
    out = {
        "tmin": float(msg.tmin),
        "tmax": float(msg.tmax),
        "npx": int(msg.npx),
        "sort": str(msg.sort or "name"),
        "subsort": str(msg.subsort or "nslc"),
        "rotate": float(msg.rotate),
        "remove_response": bool(msg.remove_response),
        "mode": str(msg.mode or "wiggle"),
        "demean": bool(msg.demean),
        "degap": bool(msg.degap),
        "allow_downsampling": bool(msg.allow_downsampling),
        "fft_filtering": bool(msg.fft_filtering),
        "bandpass_lphp": bool(msg.bandpass_lphp),
        "distances_3d": bool(msg.distances_3d),
        "scaling_base": str(msg.scaling_base or "minmax"),
        "scaling_group": str(msg.scaling_group or "individual"),
        "gain": float(msg.gain or 1.0),
        "fix_scale_ranges": bool(msg.fix_scale_ranges),
        "show_scale_ranges": bool(msg.show_scale_ranges),
        "show_scale_axes": bool(msg.show_scale_axes),
        "wf_abs": bool(msg.wf_abs),
        "wf_integrate": bool(msg.wf_integrate),
        "wf_color_scale": str(msg.wf_color_scale or "log"),
        "track_offset": int(msg.track_offset),
        "track_count": int(msg.track_count),
    }
    out["highpass"] = float(msg.highpass) if msg.HasField("highpass") else None
    out["lowpass"] = float(msg.lowpass) if msg.HasField("lowpass") else None
    return out


def _snuffling_request_dict(msg: snuffling_pb2.SnufflingRequestParams) -> Dict[str, Any]:
    view = _view_params_dict(msg.view)
    sn = msg.snufflings
    spectrum = sn.spectrum
    stalta = sn.stalta
    polarization = sn.polarization
    rms_params = sn.rms
    cake = sn.cake
    view["snufflings"] = {
        "spectrum": {
            "spec_smoothing": bool(spectrum.spec_smoothing),
            "spec_smoothing_bandwidth": float(spectrum.spec_smoothing_bandwidth),
        },
        "stalta": {
            "stalta_highpass": (
                float(stalta.stalta_highpass) if stalta.HasField("stalta_highpass") else None
            ),
            "stalta_lowpass": (
                float(stalta.stalta_lowpass) if stalta.HasField("stalta_lowpass") else None
            ),
            "stalta_swin": float(stalta.stalta_swin),
            "stalta_ratio": float(stalta.stalta_ratio),
            "stalta_level": float(stalta.stalta_level),
            "stalta_show_level_traces": bool(stalta.stalta_show_level_traces),
            "stalta_variant": str(stalta.stalta_variant or "centered"),
            "stalta_scalingmethod": str(stalta.stalta_scalingmethod or "[0-1]"),
            "stalta_apply_to_sum": bool(stalta.stalta_apply_to_sum),
        },
        "polarization": {
            "polarization_t_length": float(polarization.polarization_t_length),
            "polarization_ft_offset": float(polarization.polarization_ft_offset),
            "polarization_azimuth": float(polarization.polarization_azimuth),
            "polarization_highpass": (
                float(polarization.polarization_highpass)
                if polarization.HasField("polarization_highpass")
                else None
            ),
            "polarization_lowpass": (
                float(polarization.polarization_lowpass)
                if polarization.HasField("polarization_lowpass")
                else None
            ),
            "polarization_dot_position": float(polarization.polarization_dot_position),
            "polarization_rotate_to_rt": bool(polarization.polarization_rotate_to_rt),
            "polarization_show_eigensystem_2d": bool(polarization.polarization_show_eigensystem_2d),
            "polarization_show_eigensystem_3d": bool(polarization.polarization_show_eigensystem_3d),
            "polarization_fix_scale": bool(polarization.polarization_fix_scale),
        },
        "rms": {
            "rms_highpass": (
                float(rms_params.rms_highpass) if rms_params.HasField("rms_highpass") else None
            ),
            "rms_lowpass": (
                float(rms_params.rms_lowpass) if rms_params.HasField("rms_lowpass") else None
            ),
            "rms_block": float(rms_params.rms_block),
            "rms_group": bool(rms_params.rms_group),
            "rms_log": bool(rms_params.rms_log),
        },
        "cake": {
            "cake_model": str(cake.cake_model or "ak135-f-continental.m"),
            "cake_phases": [str(x) for x in cake.cake_phases],
            "cake_tshift": float(cake.cake_tshift),
            "cake_use_station_depth": bool(cake.cake_use_station_depth),
            "cake_mode": str(cake.cake_mode or "run"),
        },
    }
    return view


def _view_meta_pb(data: Mapping[str, Any]) -> bytes:
    msg = view_pb2.ViewMeta()
    msg.view_tmin = float(data.get("view_tmin", 0.0) or 0.0)
    msg.view_tmax = float(data.get("view_tmax", 0.0) or 0.0)
    for tr in data.get("tracks", []):
        item = msg.tracks.add()
        item.net = str(tr.get("net", ""))
        item.sta = str(tr.get("sta", ""))
        item.loc = str(tr.get("loc", ""))
        item.cha = str(tr.get("cha", ""))
        item.label = str(tr.get("label", ""))
        item.t0_rel = float(tr.get("t0_rel", 0.0) or 0.0)
        item.dt = float(tr.get("dt", 0.0) or 0.0)
        item.count = int(tr.get("count", 0) or 0)
        item.offset = int(tr.get("offset", 0) or 0)
        item.ymin = float(tr.get("ymin", 0.0) or 0.0)
        item.ymax = float(tr.get("ymax", 0.0) or 0.0)
        item.ymean = float(tr.get("ymean", 0.0) or 0.0)
        item.yscale = float(tr.get("yscale", 0.0) or 0.0)
        item.nf = int(tr.get("nf", 0) or 0)
        item.nt = int(tr.get("nt", 0) or 0)
        item.fmax = float(tr.get("fmax", 0.0) or 0.0)
    return msg.SerializeToString()


def _spec_meta_pb(data: Mapping[str, Any]) -> bytes:
    msg = view_pb2.SpecMeta()
    msg.fmin = float(data.get("fmin", 0.0) or 0.0)
    msg.fmax = float(data.get("fmax", 0.0) or 0.0)
    for tr in data.get("tracks", []):
        item = msg.tracks.add()
        item.label = str(tr.get("label", ""))
        item.offset = int(tr.get("offset", 0) or 0)
        item.count = int(tr.get("count", 0) or 0)
        item.ymin = float(tr.get("ymin", 0.0) or 0.0)
        item.ymax = float(tr.get("ymax", 0.0) or 0.0)
    return msg.SerializeToString()


def _marker_msg(data: Mapping[str, Any]) -> snuffling_pb2.SnufMarker:
    msg = snuffling_pb2.SnufMarker()
    msg.t = float(data.get("t", data.get("tmin", 0.0)) or 0.0)
    if data.get("tmax") is not None:
        msg.tmax = float(data["tmax"])
    if data.get("phase") is not None:
        msg.phase = str(data["phase"])
    msg.kind = int(data.get("kind", 0) or 0)
    msg.is_event = bool(data.get("is_event", False))
    if data.get("nslc"):
        msg.nslc.CopyFrom(_nslc_pb(data["nslc"]))
    if data.get("polarity") is not None:
        msg.polarity = int(data["polarity"])
    return msg


def _snuffling_result_pb(data: Mapping[str, Any]) -> bytes:
    msg = snuffling_pb2.SnufflingResult()
    msg.show_window = bool(data.get("show_window", True))
    msg.text = str(data.get("text", ""))
    for marker in data.get("markers", []):
        msg.markers.add().CopyFrom(_marker_msg(marker))
    if data.get("scene") is not None:
        msg.scene.CopyFrom(_scene_msg(data["scene"]))
    return msg.SerializeToString()


def _command_result_pb(data: Mapping[str, Any]) -> bytes:
    msg = data_pb2.CommandResult()
    msg.action = str(data.get("action", "none"))
    if data.get("t") is not None:
        msg.t = float(data["t"])
    if data.get("scene") is not None:
        msg.scene.CopyFrom(_scene_msg(data["scene"]))
    if data.get("message") is not None:
        msg.message = str(data["message"])
    return msg.SerializeToString()


def _map_data_pb(data: Mapping[str, Any]) -> bytes:
    msg = data_pb2.MapData()
    for station in data.get("stations", []):
        station_item = msg.stations.add()
        station_item.label = str(station.get("label", ""))
        station_item.lat = float(station.get("lat", 0.0) or 0.0)
        station_item.lon = float(station.get("lon", 0.0) or 0.0)
    for event in data.get("events", []):
        event_item = msg.events.add()
        event_item.lat = float(event.get("lat", 0.0) or 0.0)
        event_item.lon = float(event.get("lon", 0.0) or 0.0)
        event_item.mag = float(event.get("mag", 0.0) or 0.0)
    return msg.SerializeToString()


def _cake_disc_msg(data: Mapping[str, Any]) -> snuffling_pb2.CakeDiscontinuity:
    msg = snuffling_pb2.CakeDiscontinuity()
    msg.z_km = float(data.get("z_km", 0.0) or 0.0)
    if data.get("name") is not None:
        msg.name = str(data["name"])
    return msg


def _cake_layer_msg(data: Mapping[str, Any]) -> snuffling_pb2.CakeLayer:
    msg = snuffling_pb2.CakeLayer()
    msg.ztop_km = float(data.get("ztop_km", 0.0) or 0.0)
    msg.zbot_km = float(data.get("zbot_km", 0.0) or 0.0)
    msg.water = bool(data.get("water", False))
    msg.gradient = bool(data.get("gradient", False))
    return msg


def _cake_point_msg(data: Mapping[str, Any]) -> snuffling_pb2.CakePoint:
    msg = snuffling_pb2.CakePoint()
    msg.x_km = float(data.get("x_km", 0.0) or 0.0)
    msg.z_km = float(data.get("z_km", 0.0) or 0.0)
    return msg


def _cake_plot_result_pb(data: Mapping[str, Any]) -> bytes:
    msg = snuffling_pb2.CakePlotResult()
    if data.get("kind") == "model":
        model = snuffling_pb2.CakeModelPlot()
        model.title = str(data.get("title", ""))
        model.vp.extend(float(x) for x in data.get("vp", []))
        model.vs.extend(float(x) for x in data.get("vs", []))
        model.discontinuities.extend(_cake_disc_msg(x) for x in data.get("discontinuities", []))
        model.layers.extend(_cake_layer_msg(x) for x in data.get("layers", []))
        model.vmax_km = float(data.get("vmax_km", 0.0) or 0.0)
        model.zmax_km = float(data.get("zmax_km", 0.0) or 0.0)
        msg.model.CopyFrom(model)
    else:
        rays = snuffling_pb2.CakeRaysPlot()
        rays.title = str(data.get("title", ""))
        rays.discontinuities.extend(_cake_disc_msg(x) for x in data.get("discontinuities", []))
        rays.layers.extend(_cake_layer_msg(x) for x in data.get("layers", []))
        rays.source.CopyFrom(_cake_point_msg(data.get("source", {})))
        rays.receivers.extend(_cake_point_msg(x) for x in data.get("receivers", []))
        for ray in data.get("rays", []):
            ray_item = rays.rays.add()
            ray_item.color = int(ray.get("color", 0) or 0)
            for seg in ray.get("segs", []):
                series = ray_item.segs.add()
                series.values.extend(float(x) for x in seg)
        for entry in data.get("legend", []):
            legend_item = rays.legend.add()
            legend_item.phase = str(entry.get("phase", ""))
            legend_item.color = int(entry.get("color", 0) or 0)
        rays.xmin_km = float(data.get("xmin_km", 0.0) or 0.0)
        rays.xmax_km = float(data.get("xmax_km", 0.0) or 0.0)
        rays.zmax_km = float(data.get("zmax_km", 0.0) or 0.0)
        msg.rays.CopyFrom(rays)
    return msg.SerializeToString()


def make_example_pb() -> bytes:
    return _scene_pb(_make_example_scene())


def load_staged_file_pb(request: Any, path: str) -> bytes:
    req = _parse_pb(data_pb2.LoadBytesRequest, request)
    return _scene_pb(_load_path_scene(req.name, path, req.fmt, bool(req.append)))


def load_url_staged_pb(request: Any, path: str) -> bytes:
    req = _parse_pb(data_pb2.LoadUrlRequest, request)
    name = req.url.rsplit("/", 1)[-1] or "data"
    return _scene_pb(_load_path_scene(name, path, req.fmt, False))


def empty_scene_pb() -> bytes:
    return _scene_msg(SceneMeta(tmin=0.0, tmax=1.0, tracks=[])).SerializeToString()


def get_view_pb(request: Any) -> Dict[str, Any]:
    req = _parse_pb(view_pb2.ViewParams, request)
    res = _get_view_data(_view_params_dict(req))
    return {"meta": _view_meta_pb(res["meta"]), "ys": res["ys"]}


def get_spectrum_pb(request: Any) -> Dict[str, Any]:
    req = _parse_pb(snuffling_pb2.SnufflingRequestParams, request)
    res = _get_spectrum_data(_snuffling_request_dict(req))
    return {"meta": _spec_meta_pb(res["meta"]), "ys": res["ys"]}


def run_snuffling_pb(request: Any) -> bytes:
    req = _parse_pb(snuffling_pb2.NamedSnufflingRequest, request)
    # Errors travel as proto, never as a raised exception: a raised Python
    # exception's traceback can't be converted to JS on the threaded
    # (SharedArrayBuffer) build — pyodide's TextDecoder rejects the shared view
    # ("ArrayBufferView must not be shared"), masking the real error. So surface
    # it in SnufflingResult.text (which the UI already shows). See AGENTS.md.
    try:
        return _snuffling_result_pb(
            _run_snuffling_result(req.name, _snuffling_request_dict(req.params))
        )
    except Exception as exc:
        return _snuffling_result_pb(
            {
                "show_window": True,
                "text": f"{type(exc).__name__}: {exc}\n\n{traceback.format_exc()}",
            }
        )


def rms_pb(request: Any) -> bytes:
    req = _parse_pb(snuffling_pb2.SnufflingRequestParams, request)
    return _scene_pb(_rms_scene(_snuffling_request_dict(req)))


def cake_plot_pb(request: Any) -> bytes:
    req = _parse_pb(snuffling_pb2.SnufflingRequestParams, request)
    return _cake_plot_result_pb(_cake_plot_result(_snuffling_request_dict(req)))


def command_pb(request: Any) -> bytes:
    req = _parse_pb(data_pb2.StringRequest, request)
    return _command_result_pb(_command_result(req.value))


def dataselect_url_pb(request: Any) -> str:
    req = _parse_pb(data_pb2.WaveformDownloadRequest, request)
    data = {
        "datacenter": req.datacenter,
        "net": req.net,
        "sta": req.sta,
        "loc": req.loc,
        "cha": req.cha,
        "start": req.start,
        "end": req.end,
    }
    return _dataselect_url(data)


def load_waveform_path_pb(path: str) -> bytes:
    return _scene_pb(_load_waveform_path_scene(path))


def event_url_pb(request: Any) -> str:
    req = _parse_pb(data_pb2.CatalogRequest, request)
    data = {
        "datacenter": req.datacenter,
        "start": req.start,
        "end": req.end,
        "minmag": req.minmag,
    }
    return _event_url(data)


def parse_events_pb(text: str) -> bytes:
    return _snuffling_result_pb(_parse_events_result(text))


def station_url_pb(request: Any) -> str:
    req = _parse_pb(data_pb2.StationRequest, request)
    data = {
        "datacenter": req.datacenter,
        "net": req.net,
        "sta": req.sta,
        "cha": req.cha,
    }
    return _station_url(data)


def parse_stations_pb(text: str) -> bytes:
    return _snuffling_result_pb(_parse_stations_result(text))


def response_url_pb(request: Any) -> str:
    req = _parse_pb(data_pb2.StationRequest, request)
    data = {
        "datacenter": req.datacenter,
        "net": req.net,
        "sta": req.sta,
        "cha": req.cha,
    }
    return _response_url(data)


def load_responses_pb(text: str) -> bytes:
    return _snuffling_result_pb(_load_responses_result(text))


def map_data_pb() -> bytes:
    return _map_data_pb(_map_data_result())


def markers_to_text_pb(request: Any) -> str:
    req = _parse_pb(data_pb2.MarkerFile, request)
    out = []
    for marker in req.markers:
        item: Dict[str, Any] = {
            "tmin": float(marker.tmin),
            "tmax": float(marker.tmax),
            "kind": int(marker.kind),
        }
        if marker.HasField("phase"):
            item["phase"] = marker.phase
        if marker.HasField("nslc"):
            item["nslc"] = [
                marker.nslc.net,
                marker.nslc.sta,
                marker.nslc.loc,
                marker.nslc.cha,
            ]
        if marker.HasField("polarity"):
            item["polarity"] = int(marker.polarity)
        out.append(item)
    return _markers_to_text(out)


def markers_from_text_pb(text: str) -> bytes:
    data = _markers_from_text(text)
    msg = data_pb2.SnufMarkerList()
    for marker in data:
        msg.markers.add().CopyFrom(_marker_msg(marker))
    return msg.SerializeToString()


# --- Single dispatch + proto-error failsafe -------------------------------
#
# Every bridge call from the worker goes through call(): it runs the method's
# handler, packs the concrete proto result into the BridgeResult oneof, and —
# crucially — catches ANY exception and returns it as the BridgeError variant. No
# bridge function may ever raise across the JS boundary: on the threaded
# SharedArrayBuffer build a raised exception's traceback can't be converted to JS
# (TextDecoder rejects the shared view). Errors travel as proto bytes like every
# other DTO. See AGENTS.md.
#
# Type safety: each _Method[M] ties its success message type M to the `put` setter
# that writes M into the matching BridgeResult oneof field, so mypy rejects a
# method wired to the wrong field — the one binding the dynamic protobuf API can't
# check on its own. The typed setters also make the field names mypy-checked via
# the generated stubs.
M = TypeVar("M", bound=Message)


@dataclass(frozen=True)
class _Method(Generic[M]):
    # message: the concrete BridgeResult success variant this method produces.
    # run: (request, data) -> (serialized message bytes, optional zero-copy ys buffer).
    # put: copy the parsed message into its BridgeResult oneof field.
    message: type[M]
    run: Callable[[bytes, Any], Tuple[bytes, Any]]
    put: Callable[[bridge_pb2.BridgeResult, M], None]


def _put_scene(r: bridge_pb2.BridgeResult, m: scene_pb2.SceneMeta) -> None:
    r.scene.CopyFrom(m)


def _put_view(r: bridge_pb2.BridgeResult, m: view_pb2.ViewMeta) -> None:
    r.view.CopyFrom(m)


def _put_spectrum(r: bridge_pb2.BridgeResult, m: view_pb2.SpecMeta) -> None:
    r.spectrum.CopyFrom(m)


def _put_snuffling(r: bridge_pb2.BridgeResult, m: snuffling_pb2.SnufflingResult) -> None:
    r.snuffling.CopyFrom(m)


def _put_cake_plot(r: bridge_pb2.BridgeResult, m: snuffling_pb2.CakePlotResult) -> None:
    r.cake_plot.CopyFrom(m)


def _put_map_data(r: bridge_pb2.BridgeResult, m: data_pb2.MapData) -> None:
    r.map_data.CopyFrom(m)


def _put_command(r: bridge_pb2.BridgeResult, m: data_pb2.CommandResult) -> None:
    r.command.CopyFrom(m)


def _put_markers(r: bridge_pb2.BridgeResult, m: data_pb2.SnufMarkerList) -> None:
    r.markers.CopyFrom(m)


def _put_text(r: bridge_pb2.BridgeResult, m: bridge_pb2.StringValue) -> None:
    r.text.CopyFrom(m)


def _run_view(request: bytes, data: Any) -> Tuple[bytes, Any]:
    res = get_view_pb(request)
    return res["meta"], res["ys"]


def _run_spectrum(request: bytes, data: Any) -> Tuple[bytes, Any]:
    res = get_spectrum_pb(request)
    return res["meta"], res["ys"]


def _text_bytes(value: str) -> bytes:
    return bridge_pb2.StringValue(value=value).SerializeToString()


def _method(
    message: type[M],
    run: Callable[[bytes, Any], Tuple[bytes, Any]],
    put: Callable[[bridge_pb2.BridgeResult, M], None],
) -> _Method[Any]:
    # The shared TypeVar M ties `message` to `put`, so mypy rejects a call whose
    # setter's message type doesn't match the declared message class — even though
    # the return is erased to _Method[Any] for the heterogeneous dispatch table
    # (annotating the table _Method[Any] is what would otherwise skip the check).
    return _Method(message, run, put)


_DISPATCH: Dict[str, _Method[Any]] = {
    "makeExample": _method(
        scene_pb2.SceneMeta, lambda request, data: (make_example_pb(), None), _put_scene
    ),
    "loadBytes": _method(
        scene_pb2.SceneMeta,
        lambda request, data: (load_staged_file_pb(request, data), None),
        _put_scene,
    ),
    "loadUrl": _method(
        scene_pb2.SceneMeta,
        lambda request, data: (load_url_staged_pb(request, data), None),
        _put_scene,
    ),
    "emptyScene": _method(
        scene_pb2.SceneMeta, lambda request, data: (empty_scene_pb(), None), _put_scene
    ),
    "loadWaveformPath": _method(
        scene_pb2.SceneMeta, lambda request, data: (load_waveform_path_pb(data), None), _put_scene
    ),
    "getView": _method(view_pb2.ViewMeta, _run_view, _put_view),
    "getSpectrum": _method(view_pb2.SpecMeta, _run_spectrum, _put_spectrum),
    "runSnuffling": _method(
        snuffling_pb2.SnufflingResult,
        lambda request, data: (run_snuffling_pb(request), None),
        _put_snuffling,
    ),
    "rmsTraces": _method(
        scene_pb2.SceneMeta, lambda request, data: (rms_pb(request), None), _put_scene
    ),
    "cakePlot": _method(
        snuffling_pb2.CakePlotResult,
        lambda request, data: (cake_plot_pb(request), None),
        _put_cake_plot,
    ),
    "runCommand": _method(
        data_pb2.CommandResult, lambda request, data: (command_pb(request), None), _put_command
    ),
    "parseEvents": _method(
        snuffling_pb2.SnufflingResult,
        lambda request, data: (parse_events_pb(data), None),
        _put_snuffling,
    ),
    "parseStations": _method(
        snuffling_pb2.SnufflingResult,
        lambda request, data: (parse_stations_pb(data), None),
        _put_snuffling,
    ),
    "loadResponses": _method(
        snuffling_pb2.SnufflingResult,
        lambda request, data: (load_responses_pb(data), None),
        _put_snuffling,
    ),
    "mapData": _method(
        data_pb2.MapData, lambda request, data: (map_data_pb(), None), _put_map_data
    ),
    "markersToText": _method(
        bridge_pb2.StringValue,
        lambda request, data: (_text_bytes(markers_to_text_pb(request)), None),
        _put_text,
    ),
    "loadMarkers": _method(
        data_pb2.SnufMarkerList,
        lambda request, data: (markers_from_text_pb(data or ""), None),
        _put_markers,
    ),
    "dataselectUrl": _method(
        bridge_pb2.StringValue,
        lambda request, data: (_text_bytes(dataselect_url_pb(request)), None),
        _put_text,
    ),
    "eventUrl": _method(
        bridge_pb2.StringValue,
        lambda request, data: (_text_bytes(event_url_pb(request)), None),
        _put_text,
    ),
    "stationUrl": _method(
        bridge_pb2.StringValue,
        lambda request, data: (_text_bytes(station_url_pb(request)), None),
        _put_text,
    ),
    "responseUrl": _method(
        bridge_pb2.StringValue,
        lambda request, data: (_text_bytes(response_url_pb(request)), None),
        _put_text,
    ),
    "gfStoreLabel": _method(
        bridge_pb2.StringValue,
        lambda request, data: (_text_bytes(gf_store_label(data or "")), None),
        _put_text,
    ),
}


def call(method: str, request: Any = b"", data: Any = None) -> Dict[str, Any]:
    """Dispatch a bridge method; always return {"result": <BridgeResult bytes>,
    "buffer"?: ndarray}. Never raises — any exception becomes a BridgeError."""
    result = bridge_pb2.BridgeResult()
    try:
        entry = _DISPATCH[method]
        raw, buffer = entry.run(request, data)
        msg = entry.message()
        msg.MergeFromString(raw)
        entry.put(result, msg)
        env: Dict[str, Any] = {"result": result.SerializeToString()}
        if buffer is not None:
            env["buffer"] = buffer
        return env
    except Exception as exc:  # noqa: BLE001 — the whole point is to catch everything
        err = bridge_pb2.BridgeResult()
        err.error.kind = type(exc).__name__
        err.error.message = str(exc)
        err.error.traceback = traceback.format_exc()
        return {"result": err.SerializeToString()}


def call_text(method: str, request: Any = b"", data: Any = None) -> Dict[str, Any]:
    """For two-step worker flows (URL builders, gf_store_label, markers_to_text)
    that need the string back in the worker. Returns {"text": str, "error": str}
    — never raises (a raised exception over the threaded worker boundary would
    hit the same TextDecoder issue call() guards against)."""
    env = call(method, request, data)
    result = bridge_pb2.BridgeResult()
    result.ParseFromString(env["result"])
    which = result.WhichOneof("result")
    if which == "error":
        return {"text": "", "error": f"{result.error.kind}: {result.error.message}"}
    if which == "text":
        return {"text": result.text.value, "error": ""}
    return {"text": "", "error": f"unexpected bridge result variant: {which}"}
