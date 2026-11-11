"""The polarization snuffling's analysis: windows, channel extraction and PCA."""

from __future__ import annotations

import math
from typing import Dict, List, Optional, Tuple

import numpy as np
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import snuffling_pb2

from . import state


def pca(data: np.ndarray) -> Tuple[np.ndarray, np.ndarray, np.ndarray, float, float]:
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


_COMPONENT_SCHEMES: List[List[str]] = [
    ["E", "N", "Z"],
    ["1", "2", "Z"],
    ["1", "2", "3"],
    ["0", "1", "2"],
    ["R", "T", "Z"],
    ["E", "N"],
    ["1", "2"],
    ["0", "1"],
]


def order_components(by_channel: Dict[str, ptrace.Trace]) -> Optional[List[ptrace.Trace]]:
    """Order a station's channels ``[E-like, N-like(, Z-like)]`` by Qt's component schemes.

    As `polarization.py:250` tries them: the Cube-datalogger names first, then
    the eight last-character schemes.
    ``None`` when the group matches none.
    """
    channels = sorted(by_channel)
    for cube in (["p2", "p1", "p0"], ["p2", "p1"]):
        if channels == sorted(cube):
            return [by_channel[ch] for ch in cube]
    comps = {ch[-1]: ch for ch in by_channel if ch}
    if len(comps) != len(by_channel):
        return None
    for scheme in _COMPONENT_SCHEMES:
        if sorted(comps) == sorted(scheme):
            return [by_channel[comps[c]] for c in scheme]
    return None


def window_channels(
    nsl: Tuple[str, str, str],
    chagroup: str,
    w_tmin: float,
    w_tmax: float,
    hp: Optional[float],
    lp: Optional[float],
    t_length: float,
) -> Dict[str, ptrace.Trace]:
    """One polarization window's filtered channel segments, keyed by channel —
    the extraction both the figure and its text twin start from, so their
    completeness demands cannot drift apart.

    Qt's polarization choppers the raw pile with only its own channel pattern
    (polarization.py:300) — no viewer hide/quick filters, no row window, no
    GUI rotation (the snuffling derives its own R/T rotation from the
    back-azimuth) — and requires data across the window, one window length of
    display margin, and the filter's settling pad, so a hidden component still
    analyses and a gappy or too-short one drops out.
    Each segment comes back filtered and chopped to the display margin
    ``[w_tmin - t_length, w_tmax + t_length]``; callers chop further as their
    output needs."""
    from pyrocko import util as putil

    # The filter needs settling room: three periods of the lowest corner,
    # capped so a slider mid-drag cannot demand an insane cutout
    # (polarization.py:270); tpad is the display margin around the window.
    if hp is not None:
        tpad_filter = 3.0 / float(hp)
    elif lp is not None:
        tpad_filter = 3.0 / float(lp)
    else:
        tpad_filter = 0.0
    tpad_filter = min(tpad_filter, 5.0 * t_length)
    tpad = t_length

    pattern = ".".join(nsl + (chagroup + "?",))

    def match(trace: ptrace.Trace, pattern: str = pattern) -> bool:
        return bool(putil.match_nslc(pattern, trace.nslc_id))

    by_channel: Dict[str, ptrace.Trace] = {}
    for batch in state.pile().chopper(
        tmin=w_tmin,
        tmax=w_tmax,
        tpad=tpad + tpad_filter,
        want_incomplete=False,
        style="batch",
        trace_selector=match,
    ):
        for segment in batch.traces:
            if segment.ydata is None or len(segment.ydata) < 3:
                continue
            segment.ydata = segment.ydata.astype(np.float64)
            # The snuffling's own corners, always as 4th-order lowpass then
            # highpass and only below the trace's Nyquist (polarization.py:308).
            try:
                if lp is not None and float(lp) < 0.5 / segment.deltat:
                    segment.lowpass(4, float(lp))
                if hp is not None and float(hp) < 0.5 / segment.deltat:
                    segment.highpass(4, float(hp))
                segment.chop(w_tmin - tpad, w_tmax + tpad)
            except Exception:  # noqa: BLE001 — window not covered
                continue
            by_channel[segment.channel] = segment
    return by_channel


NO_MARKERS_TEXT = (
    "No selected markers.\n\nCreate and select markers at points of interest. "
    "Normal and phase markers are accepted."
)


def windows(
    request: snuffling_pb2.SnufflingRequestParams,
) -> List[Tuple[Tuple[str, str, str], str, float, float]]:
    """Qt's polarization windows (`get_selected` + `transform_time_span`,
    polarization.py:205): one per selected channel-scoped marker, starting
    Offset·Length after the marker start, Length long.

    An unscoped marker contributes nothing, as in Qt."""
    pol = request.snufflings.polarization
    t_length = float(pol.window_length_s or 1.0)
    ft_offset = float(pol.window_offset_fraction)
    windows: List[Tuple[Tuple[str, str, str], str, float, float]] = []
    for s in request.selected_spans:
        w_tmin = float(s.tmin) + t_length * ft_offset
        w_tmax = w_tmin + t_length
        for n in s.nslc_ids:
            windows.append(((n.net, n.sta, n.loc), n.cha[:-1] if n.cha else "", w_tmin, w_tmax))
    return windows


def text_result(request: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Covariance-eigenvalue polarization analysis (Jurkevics, 1988) per 3-component station.

    The figure's analysis, reported as numbers.
    """
    polarization = request.snufflings.polarization
    php = polarization.highpass if polarization.HasField("highpass") else None
    plp = polarization.lowpass if polarization.HasField("lowpass") else None
    t_length = float(polarization.window_length_s or 1.0)
    azimuth = float(polarization.azimuth_deg)

    analysis_windows = windows(request)
    if not analysis_windows:
        return snuffling_pb2.SnufflingResult(show_window=True, text=NO_MARKERS_TEXT)

    lines = ["%-16s %8s %8s %6s %6s" % ("station", "az[°]", "inc[°]", "rect", "plan")]
    for nsl, chagroup, w_tmin, w_tmax in analysis_windows:
        by_channel: Dict[str, np.ndarray] = {}
        trs: Dict[str, ptrace.Trace] = {}
        # The figure's extraction, chopped to the bare analysis window.
        for cha, segment in window_channels(
            nsl, chagroup, w_tmin, w_tmax, php, plp, t_length
        ).items():
            try:
                segment = segment.chop(w_tmin, w_tmax, inplace=False)
            except Exception:  # noqa: BLE001 — window not covered
                continue
            if segment.ydata is None or len(segment.ydata) < 8:
                continue
            trs[cha] = segment
        group = order_components(trs) if trs else None
        if group is None or len(group) != 3:
            continue
        te_, tn_, tz_ = group
        for key, segment in (("E", te_), ("N", tn_), ("Z", tz_)):
            y = np.asarray(segment.ydata, dtype=np.float64)
            by_channel[key] = y - float(np.mean(y))
        m = min(len(by_channel[k]) for k in ("Z", "N", "E"))
        if m < 8:
            continue
        yz = by_channel["Z"][:m]
        yn = by_channel["N"][:m]
        ye = by_channel["E"][:m]
        if azimuth != 0.0:
            # pyrocko's `trace.rotate` matrix, the same convention the
            # figure draws with: n' = cos·n + sin·e, e' = −sin·n + cos·e.
            theta = np.radians(azimuth)
            yn, ye = (
                np.cos(theta) * yn + np.sin(theta) * ye,
                -np.sin(theta) * yn + np.cos(theta) * ye,
            )
        data = np.vstack([yz, yn, ye])
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
            "%-16s %8.1f %8.1f %6.2f %6.2f" % (".".join(x for x in nsl if x), az, inc, rect, plan)
        )

    text = (
        "\n".join(lines)
        if len(lines) > 1
        else "Need a selected marker over 3-component (Z + 2 horizontal) data."
    )
    return snuffling_pb2.SnufflingResult(show_window=True, text=text)
