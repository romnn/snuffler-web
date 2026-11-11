"""The RMS snuffling: blockwise root-mean-square traces."""

from __future__ import annotations

from typing import Dict, List, Tuple

import numpy as np
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import scene_pb2, snuffling_pb2

from . import operation, snuffling_scope, state


async def run(request: snuffling_pb2.SnufflingRequestParams) -> scene_pb2.SceneMeta:
    """Blockwise root-mean-square derived traces (pyrocko's RMS snuffling).

    For each visible trace, RMS is computed over fixed-length blocks across the
    visible window and emitted as a new low-rate trace (channel suffix '-RMS').
    With grouping on, a station's components are combined into one RMS trace.
    Replaces the derived traces and returns the updated scene.
    """
    rms = request.snufflings.rms
    hp = rms.highpass if rms.HasField("highpass") else None
    lp = rms.lowpass if rms.HasField("lowpass") else None
    block = float(rms.block_length_s or 100.0)
    group = bool(rms.group_components)
    logrms = bool(rms.logarithmic)

    if block <= 0.0:
        return state.scene_meta()

    tpad = 0.0
    for freq in (hp, lp):
        if freq is not None and float(freq) > 0.0:
            tpad = max(tpad, 1.0 / float(freq))

    def grouping(nslc: Tuple[str, str, str, str]) -> Tuple[str, str, str, str]:
        return nslc[:3] + (nslc[3][:-1],) if group else nslc

    targets: Dict[Tuple[str, str, str, str], ptrace.Trace] = {}
    completed: List[ptrace.Trace] = []
    for batch in snuffling_scope.chopper_selected_traces(
        request,
        mode="visible",
        tinc=block,
        tpad=tpad,
        want_incomplete=False,
    ):
        await operation.report_progress_and_yield(
            len(completed) + len(targets), None, "RMS windows"
        )
        tcur = float(batch.tmin) + 0.5 * block
        rms_by_key: Dict[Tuple[str, str, str, str], List[float]] = {}
        for trace in batch.traces:
            if snuffling_scope.is_tabu_trace(trace):
                continue
            if lp is not None:
                trace.lowpass(4, float(lp), nyquist_exception=True)
            if hp is not None:
                trace.highpass(4, float(hp), nyquist_exception=True)
            try:
                trace.chop(batch.tmin, batch.tmax)
            except ptrace.NoData:
                continue
            value = 0.0
            if trace.ydata is not None and trace.ydata.size != 0:
                y = trace.ydata.astype(float)
                value = float(np.sqrt(np.sum(y**2) / trace.ydata.size))
            rms_by_key.setdefault(grouping(trace.nslc_id), []).append(value)

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
    state.replace_derived_traces("rms", completed)
    return state.scene_meta()
