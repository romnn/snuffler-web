"""Type-checked pyrocko shim that runs inside Pyodide and drives the egui
frontend.

This module is the *only* place that touches pyrocko. It exposes a tiny, stable,
JSON-friendly API to the JavaScript bridge (`web/bridge.js`), which in turn is
called from Rust (`src/bridge.rs`). The TypedDicts below mirror the serde structs
on the Rust side exactly — that mirroring is what makes the boundary type-checked
on both ends.

pyrocko itself is left completely unmodified; we only call its public API
(`pyrocko.trace.Trace`, `pyrocko.io.load`).
"""

from __future__ import annotations

import json
import math
import os
from collections import OrderedDict
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple, TypedDict

import numpy as np

from pyrocko import io as pio
from pyrocko import trace as ptrace


# The TypedDicts below mirror the serde structs in src/bridge.rs.
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
    dt: float       # >0 raw sample spacing, 0 explicit (t_rel, y) points
    count: int
    offset: int
    ymin: float
    ymax: float
    ymean: float
    yscale: float  # display half-range (units→px), grouped per scaling_group
    nf: int       # spectrogram freq bins (0 ⇒ plain wiggle)
    nt: int       # spectrogram time bins
    fmax: float   # spectrogram max frequency (Hz)


class ViewMeta(TypedDict):
    view_tmin: float
    view_tmax: float
    tracks: List[ViewTrack]


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


def _clear_view_cache() -> None:
    _VIEW_CACHE.clear()


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


def _scene_json() -> str:
    metas = [_meta(tr) for tr in _visible_traces()]
    tmin = min((m["tmin"] for m in metas), default=0.0)
    tmax = max((m["tmax"] for m in metas), default=1.0)
    scene: SceneMeta = SceneMeta(tmin=tmin, tmax=tmax, tracks=metas)
    return json.dumps(scene)


def make_example() -> str:
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
        env = np.exp(-((t - arrival) ** 2) / (2.0 * 8.0 ** 2))
        for cha in channels:
            phase = rng.uniform(0.0, 2.0 * np.pi)
            carrier = (
                np.sin(2.0 * np.pi * 3.2 * (t - arrival) + phase)
                + 0.45 * np.sin(2.0 * np.pi * 6.7 * (t - arrival) + 0.3 * phase)
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
    return _scene_json()


def load_path(name: str, path: str, fmt: str, append: bool = False) -> str:
    """Load a file already staged in Pyodide's FS with pyrocko's ``io.load``."""
    del name
    fmt2 = "detect" if fmt in (None, "", "detect") else fmt
    traces = list(pio.load(path, format=fmt2))
    _set_traces(traces, append=append)
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
    groups: dict = {}
    for tr in traces:
        groups.setdefault(tr.nslc_id[:3], []).append(tr)

    out: List[ptrace.Trace] = []
    for grp in groups.values():
        by_comp = {t.channel[-1].upper(): t for t in grp if t.channel}
        rotated: set[int] = set()
        for n, e in (("N", "E"), ("1", "2")):
            tn, te = by_comp.get(n), by_comp.get(e)
            if (
                tn is not None and te is not None
                and tn.ydata is not None and te.ydata is not None
                and len(tn.ydata) == len(te.ydata)
                and tn.deltat == te.deltat and tn.tmin == te.tmin
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
                tr.nslc_id, timespan=(tr.tmin, tr.tmax), fake_input_units="M")
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

    def namekey(tr: ptrace.Trace):
        nslc = tr.nslc_id
        return tuple(nslc[i] for i in perm)

    geo = sort in ("distance", "azimuth", "azimuth_blocks", "backazimuth")
    if geo and _ACTIVE_EVENT is not None and _STATIONS:
        from pyrocko import orthodrome as od

        elat, elon, _edep, _et = _ACTIVE_EVENT

        def geokey(tr: ptrace.Trace):
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


def _group_key(t: "ViewTrack", group: str):
    """Grouping key for shared amplitude scaling (snuffler's Scaling grouping)."""
    if group == "common":
        return ""
    if group == "per_station":
        return (t["net"], t["sta"])
    if group == "per_station_location":
        return (t["net"], t["sta"], t["loc"])
    if group == "per_component":
        return t["cha"]
    return t["label"]  # individual


def _scale_range(y: np.ndarray, base: str) -> Tuple[float, float]:
    """Amplitude (lo, hi) for a trace per snuffler's scaling base statistic."""
    if len(y) == 0:
        return 0.0, 1e-12
    if base == "robust":
        lo, hi = float(np.percentile(y, 1)), float(np.percentile(y, 99))
    elif base == "mean2":
        m, s = float(y.mean()), float(y.std())
        lo, hi = m - 2.0 * s, m + 2.0 * s
    elif base == "mean4":
        m, s = float(y.mean()), float(y.std())
        lo, hi = m - 4.0 * s, m + 4.0 * s
    else:  # minmax
        lo, hi = float(y.min()), float(y.max())
    if hi <= lo:
        hi = lo + 1e-12
    return lo, hi


def _brickwall(y: np.ndarray, deltat: float, hp: Optional[float], lp: Optional[float]) -> np.ndarray:
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


def _filter_seg(seg: ptrace.Trace, hp, lp, fft: bool, lphp: bool) -> None:
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
        _DATA_GEN, _RESPONSE_GEN, track_key, _float_key(proc_min),
        _float_key(proc_max), _float_key(hp), _float_key(lp),
        bool(demean), bool(fft), bool(lphp),
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
    include_sample_support: bool = False,
) -> Optional[Tuple[np.ndarray, float, float]]:
    seg_tmin, deltat, y = entry
    if include_sample_support:
        half = 0.5 * deltat
        start = max(0, int(math.ceil((tmin - half - seg_tmin) / deltat)))
        stop = min(len(y), int(math.floor((tmax + half - seg_tmin) / deltat)) + 1)
    else:
        start = max(0, int(math.floor((tmin - seg_tmin) / deltat)))
        stop = min(len(y), int(math.ceil((tmax - seg_tmin) / deltat)) + 1)
    if stop <= start:
        return None
    return y[start:stop], seg_tmin + start * deltat, deltat


def _display_time_span(tr: ptrace.Trace) -> Tuple[float, float]:
    t0, t1 = float(tr.tmin), float(tr.tmax)
    if tr.nslc_id[3].endswith("-RMS") and int(tr.data_len()) > 0:
        half = 0.5 * float(tr.deltat)
        return t0 - half, t1 + half
    return t0, t1


def _spectrogram(seg: ptrace.Trace, npx: int, want_abs: bool, want_integrate: bool, cscale: str):
    """Return (flat [time][freq] float32, nf, nt, fmax, vmin, vmax, t0_rel, dt)."""
    from scipy import signal as sps

    y = seg.ydata.astype(np.float64)
    if want_abs:
        y = np.abs(y)
    n = len(y)
    if n < 32:
        return None
    fs = 1.0 / float(seg.deltat)
    nperseg = int(min(256, max(32, 2 ** int(np.log2(max(32, n // 64))))))
    f, t, sxx = sps.spectrogram(
        y, fs=fs, nperseg=nperseg, noverlap=nperseg // 2, scaling="spectrum"
    )
    if want_integrate:
        w = np.where(f == 0.0, 1e-9, 2.0 * np.pi * f)
        sxx = sxx / (w[:, None] ** 2)
    if cscale == "lin":
        power = sxx
    elif cscale == "sqrt":
        power = np.sqrt(sxx)
    else:  # log (dB)
        power = 10.0 * np.log10(sxx + 1e-20)
    # cap time columns to ~npx for transport
    nt_target = max(1, min(int(npx), 600))
    if power.shape[1] > nt_target:
        idx = np.linspace(0, power.shape[1] - 1, nt_target).astype(int)
        power = power[:, idx]
        t = t[idx]
    nf, nt = power.shape
    flat = np.ascontiguousarray(power.T, dtype="<f4").ravel()  # [time][freq]
    vmin = float(np.percentile(power, 5))
    vmax = float(np.percentile(power, 99))
    dt = float(t[1] - t[0]) if nt > 1 else float(seg.deltat)
    return flat, nf, nt, float(f[-1]), vmin, vmax, float(t[0]), dt


def get_view(params_json: str) -> dict:
    """Compute the requested view (wiggle waveforms or a waterfall spectrogram).

    Returns {meta: <ViewMeta JSON>, ys: <float32 ndarray>}. For wiggles `ys`
    holds raw uniform samples at close zooms, or explicit `(t_rel, y)` extrema
    points for reduced long-window views. For the waterfall it holds per-track
    [time][freq] log-power for the requested track page.
    """
    p = json.loads(params_json)
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
    wf_abs = bool(p.get("wf_abs", False))
    wf_integrate = bool(p.get("wf_integrate", False))
    wf_cscale = p.get("wf_color_scale", "log")

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

        if mode == "waterfall":
            flat = np.zeros(0, dtype="<f4")
            nf = nt = 0
            fmax = vmin = vmax = 0.0
            t0_rel = max(float(tr.tmin), tmin) - tmin
            dt = float(tr.deltat)
            if want_samples:
                try:
                    seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
                except Exception:
                    seg = None
                if seg is not None and seg.ydata is not None and len(seg.ydata) > 0:
                    seg.ydata = seg.ydata.astype(np.float64)
                    if demean:
                        seg.ydata = seg.ydata - seg.ydata.mean()
                    _filter_seg(seg, hp, lp, fft, lphp)
                    spec = _spectrogram(seg, npx, wf_abs, wf_integrate, wf_cscale)
                    if spec is not None:
                        flat, nf, nt, fmax, vmin, vmax, spec_t0_rel, dt = spec
                        t0_rel = (float(seg.tmin) - tmin) + spec_t0_rel
            tracks.append(ViewTrack(
                net=net, sta=sta, loc=loc, cha=cha, label=label,
                t0_rel=t0_rel, dt=dt,
                count=int(nf * nt), offset=int(offset),
                ymin=vmin, ymax=vmax, ymean=0.0, yscale=1.0,
                nf=int(nf), nt=int(nt), fmax=fmax,
            ))
            if want_samples and flat.size:
                ys_parts.append(np.ascontiguousarray(flat, dtype="<f4"))
                offset += int(nf * nt)
        else:
            track_key = (
                tr_idx, tuple(tr.nslc_id), _float_key(float(tr.tmin)),
                _float_key(float(tr.tmax)), _float_key(float(tr.deltat)),
                int(tr.data_len()), _float_key(rotate), bool(remove_response),
                bool(degap),
            )
            entry = _processed_trace(
                tr, track_key, tmin, tmax, hp, lp, demean, fft, lphp)
            if entry is None:
                continue
            sliced = _slice_segment(entry, tmin, tmax, cha.endswith("-RMS"))
            if sliced is None:
                continue
            yview, ytmin, deltat = sliced
            yscale_source = entry[2]
            lo, hi = _scale_range(yscale_source, base)
            ymean = float(yscale_source.mean()) if len(yscale_source) else 0.0
            count = 0
            dt = deltat
            t0_rel = ytmin - tmin
            payload_len = 0
            if want_samples:
                target = npx if allow_ds else len(yview)
                y, count, dt = _decimate(yview, deltat, target)
                ys_parts.append(np.ascontiguousarray(y, dtype="<f4"))
                payload_len = int(y.size)
            tracks.append(ViewTrack(
                net=net, sta=sta, loc=loc, cha=cha, label=label,
                t0_rel=t0_rel, dt=dt,
                count=int(count), offset=int(offset),
                ymin=lo, ymax=hi,
                ymean=ymean,
                yscale=0.0,
                nf=0, nt=0, fmax=0.0,
            ))
            if want_samples:
                offset += payload_len

    # Grouped display scale (snuffler's Scaling grouping): per group, the largest
    # half-range; egui multiplies by this directly (no scaling math on its side).
    if mode != "waterfall":
        halfs: dict = {}
        for t in tracks:
            if t["nf"]:
                continue
            h = max(t["ymax"] - t["ymean"], t["ymean"] - t["ymin"], 1e-12)
            k = _group_key(t, scaling_group)
            halfs[k] = max(halfs.get(k, 0.0), h)
        for t in tracks:
            if not t["nf"]:
                t["yscale"] = halfs.get(_group_key(t, scaling_group), 1.0)

    ys = (
        np.ascontiguousarray(np.concatenate(ys_parts), dtype="<f4")
        if ys_parts else np.zeros(0, dtype="<f4")
    )
    meta: ViewMeta = ViewMeta(view_tmin=tmin, view_tmax=tmax, tracks=tracks)
    return {"meta": json.dumps(meta), "ys": ys}


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


def get_spectrum(params_json: str) -> dict:
    """Amplitude spectrum (|rFFT|) of each visible trace — a built-in snuffling.

    Returns {meta: <JSON>, ys: <float32 bytes>} with per-track log10-amplitude
    sampled on a shared log-frequency axis [fmin, fmax]."""
    p = json.loads(params_json)
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    spectrum_params = _snuffling_section(p, "spectrum")
    smoothing = bool(_cfg(spectrum_params, p, "spec_smoothing", False))
    smoothing_bandwidth = float(
        _cfg(spectrum_params, p, "spec_smoothing_bandwidth", 40.0) or 40.0)
    hp, lp = p.get("highpass"), p.get("lowpass")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    nbins = 400

    vis = _visible_traces()
    order = sorted(range(len(vis)), key=lambda i: vis[i].nslc_id)
    work = _apply_rotation([vis[i] for i in order], rotate)
    track_offset = max(0, int(p.get("track_offset", 0) or 0))
    track_count = max(0, int(p.get("track_count", len(work)) or len(work)))
    work = work[track_offset:track_offset + track_count]

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
        return {"meta": json.dumps({"fmin": 0.1, "fmax": 1.0, "tracks": []}),
                "ys": np.zeros(0, "<f4")}

    fmin_global = max(fmin_global, fmax_global * 1e-4)
    grid = np.logspace(np.log10(fmin_global), np.log10(fmax_global), nbins)
    ys_parts, tracks, offset = [], [], 0
    for label, freq, amp in specs:
        resamp = np.interp(grid, freq, amp, left=0.0, right=0.0)
        if smoothing:
            resamp = _konno_ohmachi_smooth(resamp, grid, smoothing_bandwidth)
        logamp = np.log10(resamp + 1e-20).astype("<f4")
        tracks.append({"label": label, "offset": int(offset), "count": int(nbins),
                       "ymin": float(logamp.min()), "ymax": float(logamp.max())})
        ys_parts.append(logamp)
        offset += nbins
    ys = np.concatenate(ys_parts) if ys_parts else np.zeros(0, "<f4")
    meta = {"fmin": float(fmin_global), "fmax": float(fmax_global), "tracks": tracks}
    return {"meta": json.dumps(meta), "ys": np.ascontiguousarray(ys, dtype="<f4")}


def run_snuffling(name: str, params_json: str) -> str:
    """Run a built-in snuffling over the current window. Returns a JSON string
    {text, markers:[{t, phase, kind}]} that the egui side displays / adds."""
    p = json.loads(params_json)
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    hp, lp = p.get("highpass"), p.get("lowpass")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    fft = bool(p.get("fft_filtering", False))
    lphp = bool(p.get("bandpass_lphp", True))

    vis = _visible_traces()
    order = _sort_order(vis, p.get("sort", "name"), p.get("subsort", "nslc"))
    work = _apply_rotation([vis[i] for i in order], rotate)

    def chopped(pass_hp: Optional[float] = hp, pass_lp: Optional[float] = lp,
                ctmin: float = tmin, ctmax: float = tmax,
                traces: Optional[List[ptrace.Trace]] = None):
        for tr in (work if traces is None else traces):
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
            lines.append("%-16s %+.4e %+.4e %.4e" % (
                ".".join(tr.nslc_id), float(y.min()), float(y.max()), float(y.max() - y.min())))
        text = "\n".join(lines) if len(lines) > 1 else "No data in the visible window."
        return json.dumps({"text": text, "markers": []})

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
                    derived.append(ptrace.Trace(
                        network=net, station=sta, location=loc,
                        channel=cha + "-STALTA",
                        deltat=float(c.deltat), tmin=float(c.tmin),
                        ydata=np.asarray(c.ydata, dtype=np.float64),
                        meta={"tabu": True}))
                if apply_to_sum:
                    processed.append(c)
                    continue
                tpeaks, _apeaks = c.peaks(level, tshort)
            except Exception:
                continue
            for tp in tpeaks:
                markers.append({
                    "t": float(tp), "phase": None, "kind": 0,
                    "nslc": list(tr.nslc_id),
                })
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
            scene = json.loads(_scene_json())
        return json.dumps({
            "show_window": False,
            "text": "STA/LTA: %d detection(s) (short=%.3gs, ratio=%g, level=%g)"
                    % (len(markers), tshort, ratio, level),
            "markers": markers,
            "scene": scene,
        })

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

        lines = ["%-16s %8s %8s %6s %6s" % (
            "station", "az[°]", "inc[°]", "rect", "plan")]
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
            lines.append("%-16s %8.1f %8.1f %6.2f %6.2f" % (
                "%s.%s.%s" % (net, sta, loc), az, inc, rect, plan))

        text = "\n".join(lines) if len(lines) > 1 else \
            "Need 3-component (Z/N/E) data in the visible window."
        return json.dumps({"text": text, "markers": []})

    return json.dumps({"text": "Unknown snuffling: %s" % name, "markers": []})


# --- FDSN web services (network) -------------------------------------------
# The browser performs the actual fetch (it has CORS); these helpers only build
# the request URLs and parse the responses, so all logic stays in Python.


def dataselect_url(params_json: str) -> str:
    """FDSN dataselect (waveform) query URL."""
    p = json.loads(params_json)
    base = _fdsn_base(p.get("datacenter", "geofon"))
    q = "&".join([
        "net=%s" % (p.get("net") or "*"),
        "sta=%s" % (p.get("sta") or "*"),
        "loc=%s" % (p.get("loc") or "*"),
        "cha=%s" % (p.get("cha") or "*"),
        "starttime=%s" % p["start"],
        "endtime=%s" % p["end"],
        "nodata=204",
    ])
    return "%s/fdsnws/dataselect/1/query?%s" % (base, q)


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


def load_waveform_path(path: str) -> str:
    """Load staged FDSN MiniSEED bytes into the pile and return the scene JSON."""
    if not path:
        return json.dumps(SceneMeta(tmin=0.0, tmax=1.0, tracks=[]))
    traces = list(pio.load(path, format="mseed"))
    _set_traces(traces)
    return _scene_json()


def event_url(params_json: str) -> str:
    """FDSN event (catalog) query URL (text format)."""
    p = json.loads(params_json)
    base = _fdsn_base(p.get("datacenter", "usgs"))
    q = "&".join([
        "format=text",
        "starttime=%s" % p["start"],
        "endtime=%s" % p["end"],
        "minmagnitude=%s" % p.get("minmag", 5.0),
        "orderby=time",
        "limit=1000",
    ])
    return "%s/fdsnws/event/1/query?%s" % (base, q)


def _ftime(s: str) -> float:
    s = s.strip().rstrip("Z")
    try:
        return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()
    except Exception:
        return 0.0


def parse_events(text: str) -> str:
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
        markers.append({
            "t": t,
            "phase": ("M%.1f %s" % (mag, name)).strip()[:48],
            "kind": 0,
            "is_event": True,
        })
        _EVENTS.append({"lat": lat, "lon": lon, "depth": depth, "mag": mag, "time": t})
        lines.append("M%.1f  %s  %s" % (mag, when, name))
        if best is None or mag > best[0]:
            best = (mag, lat, lon, depth, t)
    if best is not None:
        _ACTIVE_EVENT = (best[1], best[2], best[3], best[4])
        header = ("%d event(s) found; active event M%.1f (distance sort enabled).\n"
                  "Jumped the view to the events — load waveforms for this time to "
                  "see them on data." % (len(markers), best[0]))
    else:
        header = "No events found in this time window."
    body = "\n".join(lines[:30])
    return json.dumps({"text": header + ("\n\n" + body if body else ""), "markers": markers})


def station_url(params_json: str) -> str:
    """FDSN station query URL (text format, station level)."""
    p = json.loads(params_json)
    base = _fdsn_base(p.get("datacenter", "geofon"))
    q = "&".join([
        "net=%s" % (p.get("net") or "*"),
        "sta=%s" % (p.get("sta") or "*"),
        "cha=%s" % (p.get("cha") or "*"),
        "level=station",
        "format=text",
    ])
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def parse_stations(text: str) -> str:
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
    return json.dumps({"text": "%d station(s) with coordinates loaded" % n, "markers": []})


def response_url(params_json: str) -> str:
    """FDSN station query URL at response level (StationXML with instrument
    responses, for deconvolution)."""
    p = json.loads(params_json)
    base = _fdsn_base(p.get("datacenter", "geofon"))
    q = "&".join([
        "net=%s" % (p.get("net") or "*"),
        "sta=%s" % (p.get("sta") or "*"),
        "cha=%s" % (p.get("cha") or "*"),
        "level=response",
    ])
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def load_responses(xml_text: str) -> str:
    """Parse fetched StationXML into instrument responses (for Deconvolution) and
    also harvest station coordinates (for distance/azimuth sorting)."""
    global _STATIONXML, _RESPONSE_GEN
    if not xml_text.strip():
        return json.dumps({"text": "no station metadata returned", "markers": []})
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
    return json.dumps({
        "text": "%d channel response(s) loaded — enable Options ▸ Deconvolution" % nchan,
        "markers": [],
    })


def map_data() -> str:
    """Station + event coordinates for the Map overlay (no network — reads what
    the FDSN fetches already loaded)."""
    stations = [
        {"label": "%s.%s" % (net, sta), "lat": lat, "lon": lon}
        for (net, sta), (lat, lon, _elev) in _STATIONS.items()
    ]
    events = [
        {"lat": e["lat"], "lon": e["lon"], "mag": e["mag"], "time": e["time"]}
        for e in _EVENTS
    ]
    return json.dumps({"stations": stations, "events": events})


# --- Marker file I/O (pyrocko marker format) -------------------------------


_marker_mod: Any = None


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
            stub.__path__ = [  # type: ignore[attr-defined]
                os.path.join(os.path.dirname(pyrocko.gui.__file__), "snuffler")
            ]
            stub.__package__ = pkg_name
            stub._snuffler_stub = True  # type: ignore[attr-defined]
            sys.modules[pkg_name] = stub
        _marker_mod = importlib.import_module(pkg_name + ".marker")
    return _marker_mod


def markers_to_text(markers_json: str) -> str:
    """Serialize markers to pyrocko's snuffler marker-file format."""
    pm = _marker_module()

    ms = []
    for m in json.loads(markers_json):
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
            ms.append(pm.PhaseMarker(
                ids, tmin, tmax, kind, phasename=phase, polarity=polarity))
        else:
            ms.append(pm.Marker(ids, tmin, tmax, kind))
    path = "/tmp/markers_out.txt"
    pm.save_markers(ms, path)
    with open(path) as f:
        return f.read()


def markers_from_text(text: str) -> str:
    """Parse a pyrocko marker file into the JSON the egui side adds."""
    pm = _marker_module()

    path = "/tmp/markers_in.txt"
    with open(path, "w") as f:
        f.write(text)
    try:
        ms = pm.load_markers(path)
    except Exception:
        return json.dumps([])
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
        out.append({
            "t": float(mk.tmin),
            "tmax": float(mk.tmax),
            "kind": int(mk.kind or 0),
            "phase": phase,
            "polarity": polarity,
            "nslc": nslc,
            "is_event": isinstance(mk, pm.EventMarker),
        })
    return json.dumps(out)


# Path where bridge.js stages the ak135 earth model (it is shipped as a build
# asset and written into the Pyodide FS on first use; see cakeArrivals there).
EARTHMODEL_PATH = "/earthmodel.nd"


def cake_arrivals(params_json: str) -> str:
    """Theoretical P/S arrival times for the active event at each loaded station.

    All travel-time computation is pyrocko's :py:mod:`pyrocko.cake` (pure
    numpy/scipy ray tracing through the ak135 model) and the geometry is
    :py:mod:`pyrocko.orthodrome`; this only marshals the result into markers.
    """
    if _ACTIVE_EVENT is None:
        return json.dumps({
            "text": "No active event. Run a catalog search first "
                    "(Data ▸ Catalog search).",
            "markers": [],
        })
    if not _STATIONS:
        return json.dumps({
            "text": "No station coordinates. Fetch stations first "
                    "(Data ▸ Download ▸ Fetch stations).",
            "markers": [],
        })

    from pyrocko import cake

    elat, elon, edepth, etime = _ACTIVE_EVENT
    mod = cake.load_model(EARTHMODEL_PATH)

    # Only annotate stations that actually have traces loaded, so the picks line
    # up with visible tracks.
    have = {(tr.nslc_id[0], tr.nslc_id[1]) for tr in _TRACES}
    markers: List[Dict[str, Any]] = []
    nstations = 0
    for (net, sta), (slat, slon, _elev) in _STATIONS.items():
        if have and (net, sta) not in have:
            continue
        dist_deg = _dist_deg(elat, elon, slat, slon)
        nstations += 1
        for phasename, kind in (("P", 1), ("S", 2)):
            try:
                phases = cake.PhaseDef.classic(phasename)
            except Exception:
                phases = [cake.PhaseDef(phasename)]
            rays = mod.arrivals(
                distances=[dist_deg], phases=phases, zstart=edepth, zstop=0.0)
            if rays:
                t = etime + float(rays[0].t)  # earliest arrival of this phase
                markers.append({
                    "t": t,
                    "tmax": t,
                    "kind": kind,
                    "phase": phasename,
                    "nslc": [net, sta, "", "*"],
                })

    text = "Theoretical arrivals (ak135): %i picks at %i stations." % (
        len(markers), nstations)
    return json.dumps({"text": text, "markers": markers})


def _command_glob(scope: Optional[str], pattern: str) -> str:
    """Build an "n.s.l.c" fnmatch glob from an optional scope + pattern."""
    idx = {"n": 0, "s": 1, "l": 2, "c": 3}
    if scope in idx:
        fields = ["*", "*", "*", "*"]
        fields[idx[scope]] = pattern
        return ".".join(fields)
    # No scope: a bare token without dots matches anywhere in the id.
    return pattern if "." in pattern else "*%s*" % pattern


def command(line: str) -> str:
    """Execute a snuffler ':' command line. Returns JSON describing the action:
    {"action": "goto", "t": <epoch>} | {"action": "scene", "scene": <SceneMeta>}
    | {"action": "error", "message": ...} | {"action": "none"}.

    Supported: ``goto <time>``; ``hide [n|s|l|c] <pattern>``;
    ``unhide [[n|s|l|c] <pattern>]``; ``n|s|l|c <pattern>`` (reduce to matching).
    """
    global _HIDDEN, _REDUCE
    parts = line.split()
    if not parts:
        return json.dumps({"action": "none"})
    cmd, args = parts[0], parts[1:]

    if cmd == "goto":
        if not args:
            return json.dumps(
                {"action": "error", "message": "usage: goto <time>"})
        from pyrocko import util
        try:
            t = float(util.str_to_time_fillup(" ".join(args)))
        except Exception as e:
            return json.dumps({"action": "error", "message": "bad time: %s" % e})
        return json.dumps({"action": "goto", "t": t})

    if cmd in ("hide", "unhide"):
        scope, rest = None, args
        if args and args[0] in ("n", "s", "l", "c"):
            scope, rest = args[0], args[1:]
        pattern = rest[0] if rest else None
        if cmd == "hide":
            if pattern is None:
                return json.dumps(
                    {"action": "error",
                     "message": "usage: hide [n|s|l|c] <pattern>"})
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
        return json.dumps({"action": "scene", "scene": json.loads(_scene_json())})

    if cmd in ("n", "s", "l", "c"):
        _REDUCE = _command_glob(cmd, args[0] if args else "*")
        return json.dumps({"action": "scene", "scene": json.loads(_scene_json())})

    return json.dumps(
        {"action": "error", "message": "unknown command: %s" % cmd})


def rms(params_json: str) -> str:
    """Blockwise root-mean-square derived traces (pyrocko's RMS snuffling).

    For each visible trace, RMS is computed over fixed-length blocks across the
    visible window and emitted as a new low-rate trace (channel suffix '-RMS').
    With grouping on, a station's components are combined into one RMS trace.
    Appends the derived traces and returns the updated scene JSON. All numpy.
    """
    global _TRACES
    p = json.loads(params_json)
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
        tr for tr in _visible_traces()
        if not _is_tabu_trace(tr) and not tr.nslc_id[3].endswith("-RMS")
    ]
    if not src or block <= 0.0:
        _mark_traces_changed()
        return _scene_json()

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
        return _scene_json()

    def complete_chop(tr: ptrace.Trace, wmin: float, wmax: float) -> Optional[ptrace.Trace]:
        try:
            seg = tr.chop(
                wmin - tpad,
                wmax + tpad,
                inplace=False,
                include_last=False)
        except ptrace.NoData:
            return None

        emin = float(seg.tmin) - (wmin - tpad)
        emax = float(seg.tmax) + float(seg.deltat) - (wmax + tpad)
        if abs(emin) <= 0.5 * float(seg.deltat) and abs(emax) <= 0.5 * float(seg.deltat):
            return seg

        if (
                0.0 < emin <= 5.0 * float(seg.deltat)
                and -5.0 * float(seg.deltat) <= emax < 0.0):
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
                value = float(np.sqrt(np.sum(y ** 2) / tr.ydata.size))
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
                    meta={"tabu": True})
                targets[key] = target

            value = np.atleast_1d(
                np.sqrt(np.sum(np.array(values, dtype=float) ** 2)))
            if logrms and value != 0.0:
                value = np.log(value)
            targets[key].append(value)

    completed.extend(targets.values())
    _TRACES.extend(completed)

    _mark_traces_changed()
    return _scene_json()
