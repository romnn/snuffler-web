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
    dt: float
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

# Station coordinates (net, sta) -> (lat, lon, elevation) and the active event
# (lat, lon, depth_m, time), populated by the FDSN fetch helpers below. Used for
# distance/azimuth sorting. The browser does the actual fetch (CORS); all URL
# building, parsing and geo math happen here in pyrocko/Python.
_STATIONS: Dict[Tuple[str, str], Tuple[float, float, float]] = {}
_ACTIVE_EVENT: Optional[Tuple[float, float, float, float]] = None
# Instrument responses (a parsed FDSNStationXML), used by Deconvolution.
_STATIONXML: Any = None

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
    metas = [_meta(tr) for tr in _TRACES]
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
        event = env * np.sin(2.0 * np.pi * 1.5 * (t - arrival))
        for cha in channels:
            # mild "colored" noise
            noise = np.convolve(rng.standard_normal(n), np.ones(8) / 8.0, "same")
            y = 0.3 * noise + event
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

    global _TRACES
    _TRACES = traces
    return _scene_json()


def load_bytes(name: str, data: Any, fmt: str) -> str:
    """Write the given file bytes into Pyodide's virtual FS and load them with
    pyrocko's `io.load`. `data` arrives as a JS Uint8Array."""
    raw = bytes(data.to_py()) if hasattr(data, "to_py") else bytes(data)
    os.makedirs("/data", exist_ok=True)
    path = "/data/" + (name or "data")
    with open(path, "wb") as f:
        f.write(raw)
    fmt2 = "detect" if fmt in (None, "", "detect") else fmt
    traces = list(pio.load(path, format=fmt2))
    global _TRACES
    _TRACES = traces
    return _scene_json()


def _decimate(y: np.ndarray, deltat: float, npx: int) -> Tuple[np.ndarray, int, float]:
    """Max-abs decimation to ~`npx` points (keeps the visually dominant sample
    of each bin). Mirrors the native Rust stub. Returns (f32 array, count, dt)."""
    n = len(y)
    npx = max(npx, 16)
    if n <= npx * 2:
        return y.astype(np.float32), n, deltat
    stride = (n + npx - 1) // npx
    nb = (n + stride - 1) // stride
    pad = nb * stride - n
    yp = np.concatenate([y, np.zeros(pad)]) if pad else y
    yp = yp.reshape(nb, stride)
    idx = np.argmax(np.abs(yp), axis=1)
    out = yp[np.arange(nb), idx].astype(np.float32)
    return out, nb, deltat * stride


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
            dist = float(od.distance_accurate50m(elat, elon, slat, slon))
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

    Returns {meta: <ViewMeta JSON>, ys: <float32 bytes>}. For wiggles `ys` holds
    decimated samples; for the waterfall it holds per-track [time][freq] log-power
    (nf*nt values). Rust copies the Float32Array and renders accordingly."""
    p = json.loads(params_json)
    tmin = float(p["tmin"])
    tmax = float(p["tmax"])
    npx = int(p["npx"])
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

    traces = list(_TRACES)
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
    for tr in work:
        try:
            seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:
            continue
        if seg.ydata is None or len(seg.ydata) == 0:
            continue
        seg.ydata = seg.ydata.astype(np.float64)
        if demean:
            seg.ydata = seg.ydata - seg.ydata.mean()
        _filter_seg(seg, hp, lp, fft, lphp)

        net, sta, loc, cha = tr.nslc_id
        label = ".".join(tr.nslc_id)

        if mode == "waterfall":
            spec = _spectrogram(seg, npx, wf_abs, wf_integrate, wf_cscale)
            if spec is None:
                continue
            flat, nf, nt, fmax, vmin, vmax, t0_rel, dt = spec
            tracks.append(ViewTrack(
                net=net, sta=sta, loc=loc, cha=cha, label=label,
                t0_rel=(float(seg.tmin) - tmin) + t0_rel, dt=dt,
                count=int(nf * nt), offset=int(offset),
                ymin=vmin, ymax=vmax, ymean=0.0, yscale=1.0,
                nf=int(nf), nt=int(nt), fmax=fmax,
            ))
            ys_parts.append(flat)
            offset += int(nf * nt)
        else:
            # Amplitude range from the chosen scaling base over the raw samples.
            lo, hi = _scale_range(seg.ydata, base)
            target = npx if allow_ds else len(seg.ydata)
            y, count, dt = _decimate(seg.ydata, float(seg.deltat), target)
            tracks.append(ViewTrack(
                net=net, sta=sta, loc=loc, cha=cha, label=label,
                t0_rel=float(seg.tmin) - tmin, dt=dt,
                count=int(count), offset=int(offset),
                ymin=lo, ymax=hi,
                ymean=float(seg.ydata.mean()) if len(seg.ydata) else 0.0,
                yscale=0.0,
                nf=0, nt=0, fmax=0.0,
            ))
            ys_parts.append(y.astype("<f4"))
            offset += int(count)

    # Grouped display scale (snuffler's Scaling grouping): per group, the largest
    # half-range; egui multiplies by this directly (no scaling math on its side).
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

    ys = np.concatenate(ys_parts) if ys_parts else np.zeros(0, dtype="<f4")
    meta: ViewMeta = ViewMeta(view_tmin=tmin, view_tmax=tmax, tracks=tracks)
    return {"meta": json.dumps(meta), "ys": ys.tobytes()}


def get_spectrum(params_json: str) -> dict:
    """Amplitude spectrum (|rFFT|) of each visible trace — a built-in snuffling.

    Returns {meta: <JSON>, ys: <float32 bytes>} with per-track log10-amplitude
    sampled on a shared log-frequency axis [fmin, fmax]."""
    p = json.loads(params_json)
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    hp, lp = p.get("highpass"), p.get("lowpass")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    nbins = 400

    order = sorted(range(len(_TRACES)), key=lambda i: _TRACES[i].nslc_id)
    work = _apply_rotation([_TRACES[i] for i in order], rotate)

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
                "ys": np.zeros(0, "<f4").tobytes()}

    fmin_global = max(fmin_global, fmax_global * 1e-4)
    grid = np.logspace(np.log10(fmin_global), np.log10(fmax_global), nbins)
    ys_parts, tracks, offset = [], [], 0
    for label, freq, amp in specs:
        resamp = np.interp(grid, freq, amp, left=0.0, right=0.0)
        logamp = np.log10(resamp + 1e-20).astype("<f4")
        tracks.append({"label": label, "offset": int(offset), "count": int(nbins),
                       "ymin": float(logamp.min()), "ymax": float(logamp.max())})
        ys_parts.append(logamp)
        offset += nbins
    ys = np.concatenate(ys_parts) if ys_parts else np.zeros(0, "<f4")
    meta = {"fmin": float(fmin_global), "fmax": float(fmax_global), "tracks": tracks}
    return {"meta": json.dumps(meta), "ys": ys.tobytes()}


def run_snuffling(name: str, params_json: str) -> str:
    """Run a built-in snuffling over the current window. Returns a JSON string
    {text, markers:[{t, phase, kind}]} that the egui side displays / adds."""
    p = json.loads(params_json)
    tmin, tmax = float(p["tmin"]), float(p["tmax"])
    hp, lp = p.get("highpass"), p.get("lowpass")
    rotate = float(p.get("rotate", 0.0) or 0.0)
    fft = bool(p.get("fft_filtering", False))
    lphp = bool(p.get("bandpass_lphp", True))

    order = _sort_order(_TRACES, p.get("sort", "name"), p.get("subsort", "nslc"))
    work = _apply_rotation([_TRACES[i] for i in order], rotate)

    def chopped():
        for tr in work:
            try:
                seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
            except Exception:
                continue
            if seg.ydata is None or len(seg.ydata) < 4:
                continue
            seg.ydata = seg.ydata.astype(np.float64)
            _filter_seg(seg, hp, lp, fft, lphp)
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
        tshort, ratio, level = 1.0, 3.0, 0.7
        markers = []
        for _tr, seg in chopped():
            if len(seg.ydata) < 64:
                continue
            try:
                c = seg.copy()
                c.sta_lta_centered(tshort, tshort * ratio)
                tpeaks, _apeaks = c.peaks(level, tshort)
            except Exception:
                continue
            for tp in tpeaks:
                markers.append({"t": float(tp), "phase": None, "kind": 3})
        return json.dumps({
            "text": "STA/LTA: %d detection(s) (short=%.3gs, ratio=%g, level=%g)"
                    % (len(markers), tshort, ratio, level),
            "markers": markers,
        })

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
    traces = list(pio.load(path, format="mseed"))
    global _TRACES
    _TRACES = traces
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
    global _ACTIVE_EVENT
    markers = []
    lines = []
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
    global _STATIONXML
    if not xml_text.strip():
        return json.dumps({"text": "no station metadata returned", "markers": []})
    from pyrocko.io import stationxml

    _STATIONXML = stationxml.load_xml(string=xml_text)
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
