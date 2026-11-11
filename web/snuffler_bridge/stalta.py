"""The STA/LTA snuffling: detections where the short/long-term average ratio crosses a level."""

from __future__ import annotations

from typing import List

import numpy as np
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import snuffling_pb2

from . import marker_budget, operation, snuffling_scope, state


async def run(request: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Qt's STA/LTA detector: picks markers where the ratio crosses the level."""
    stalta = request.snufflings.stalta
    shp = stalta.highpass if stalta.HasField("highpass") else None
    slp = stalta.lowpass if stalta.HasField("lowpass") else None
    tshort = float(stalta.short_window_s or 30.0)
    ratio = float(stalta.long_to_short_window_ratio or 3.0)
    level = float(stalta.trigger_level or 0.5)
    variant = stalta.variant or "centered"
    scaling_name = stalta.scaling_method or "[0-1]"
    apply_to_sum = bool(stalta.apply_to_sum)
    show_level_traces = bool(stalta.show_level_traces)
    scaling_method = {
        "[0-1]": 1,
        "[-1/ratio,1]": 2,
        "[-1/ratio,1] clipped to [0,1]": 3,
    }.get(scaling_name, 1)
    result = snuffling_pb2.SnufflingResult(show_window=False, origin="stalta")
    markers = marker_budget.MarkerAccumulator(request.marker_budget)
    lwin = tshort * ratio
    derived: List[ptrace.Trace] = []
    deltat_min = min((tr.deltat for tr in state.traces if tr.deltat > 0.0), default=0.01)
    tinc = max(lwin * 2.0, 500_000.0 * deltat_min)
    batches_done = 0
    for batch in snuffling_scope.chopper_selected_traces(
        request,
        mode="visible",
        skip_instant=True,
        trace_filter=lambda trace: not snuffling_scope.is_tabu_trace(trace),
        tinc=tinc,
        tpad=lwin,
        want_incomplete=False,
    ):
        batches_done += 1
        await operation.report_progress_and_yield(batches_done, None, "STA/LTA windows")
        sumtrace = None
        isum = 0
        for tr in batch.traces:
            if tr.ydata is None or len(tr.ydata) < 64:
                continue
            tr.ydata = tr.ydata.astype(np.float64)
            if slp is not None:
                tr.lowpass(4, float(slp), nyquist_exception=True)
            if shp is not None:
                tr.highpass(4, float(shp), nyquist_exception=True)
            method = tr.sta_lta_right if variant == "right" else tr.sta_lta_centered
            try:
                method(tshort, lwin, scalingmethod=scaling_method)
            except TypeError:
                method(tshort, lwin)
            tr.chop(batch.tmin, batch.tmax)
            original_nslc = tr.nslc_id
            if not apply_to_sum:
                tpeaks, _apeaks = tr.peaks(level, tshort)
                for tp in tpeaks:
                    marker = snuffling_pb2.SnufMarker()
                    marker.tmin = float(tp)
                    markers.add(marker, [original_nslc])
            # Qt renames the level trace into location `cg` keeping the
            # channel (stalta.py:189) and tags it tabu.
            tr.set_codes(location="cg")
            tr.meta = {"tabu": True}
            if show_level_traces:
                derived.append(tr)
            # The `.SUM.cg.` mean trace at the pile's finest sampling
            # (stalta.py:196), built whether or not it is shown.
            if sumtrace is None:
                ny = int((tr.tmax - tr.tmin) / deltat_min)
                if ny > 0:
                    sumtrace = ptrace.Trace(
                        network="",
                        station="SUM",
                        location="cg",
                        channel="",
                        deltat=deltat_min,
                        tmin=float(tr.tmin),
                        ydata=np.zeros(ny),
                        meta={"tabu": True},
                    )
            if sumtrace is not None:
                sumtrace.add(tr, left=None, right=None)
                isum += 1
        if sumtrace is not None and isum > 0:
            sumtrace.ydata = sumtrace.ydata / float(isum)
            if apply_to_sum:
                tpeaks, _apeaks = sumtrace.peaks(level, tshort)
                for tp in tpeaks:
                    marker = snuffling_pb2.SnufMarker()
                    marker.tmin = float(tp)
                    markers.add(marker, [("*", "*", "*", "*")])
            if show_level_traces:
                derived.append(sumtrace)
    state.replace_derived_traces("stalta", derived if show_level_traces else [])
    result.scene.CopyFrom(state.scene_meta())
    result.text = "STA/LTA: %d detection(s) (short=%.3gs, ratio=%g, level=%g)" % (
        markers.total,
        tshort,
        ratio,
        level,
    )
    return markers.finish(result)
