"""The SeisBench picker snuffling: deep-learning P/S picks over the selected spans.

Inference runs in ONNX Runtime in the JS worker; the pre- and post-processing
lives in the top-level ``seisbench_infer`` module.
"""

from __future__ import annotations

import math
from collections import OrderedDict
from typing import Any, Dict, List, Optional, Tuple

import numpy as np
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import snuffling_pb2

from . import fetch, marker_budget, snuffling_scope, state
from .nslc import NslcId


def _report_progress(frac: float, label: str) -> None:
    try:
        import js

        js.__snufflerSeisbenchProgress(float(frac), label)
    except Exception:
        pass


def _join_url(base_url: str, filename: str) -> str:
    return base_url.rstrip("/") + "/" + filename


async def _ensure_model(model: str, weights: str, base_url: str) -> Tuple[str, Dict[str, Any]]:
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
    if cache_key not in state.seisbench_model_urls:
        # An absolute base is fetched directly: the local proxy's allowlist only
        # admits FDSN endpoints, so routing model hosts through it would be refused.
        if base.startswith(("http://", "https://")):
            url = _join_url(base, filename)
        else:
            url = fetch.asset_url(_join_url(base, filename))
        state.seisbench_model_urls[cache_key] = url
    return state.seisbench_model_urls[cache_key], spec


def _apply_filter(segment: ptrace.Trace, spec: Dict[str, Any]) -> None:
    args = spec.get("filter_args")
    kwargs = spec.get("filter_kwargs") or {}
    if not args:
        return
    kind = str(args[0] if isinstance(args, list) else args)
    order = int(kwargs.get("corners", 4))
    try:
        if kind == "highpass":
            segment.highpass(order, float(kwargs["freq"]), nyquist_exception=True)
        elif kind == "lowpass":
            segment.lowpass(order, float(kwargs["freq"]), nyquist_exception=True)
        elif kind == "bandpass":
            segment.bandpass(order, float(kwargs["freqmin"]), float(kwargs["freqmax"]))
    except Exception:
        pass


def _apply_view_filter(
    trace: ptrace.Trace,
    highpass: Optional[float],
    lowpass: Optional[float],
    tcut: float,
) -> ptrace.Trace:
    """Apply Qt's default SeisBench preprocessing from the main viewer passband."""
    segment = trace.copy()
    if lowpass is not None:
        segment.lowpass(4, lowpass, nyquist_exception=False)
    if highpass is not None:
        segment.highpass(4, highpass, nyquist_exception=False)
    try:
        segment.chop(segment.tmin + tcut, segment.tmax - tcut)
    except ptrace.NoData:
        pass
    return segment


def station_groups(
    traces: List[ptrace.Trace],
    tmin: float,
    tmax: float,
    spec: Dict[str, Any],
    scale_factor: float = 1.0,
) -> "OrderedDict[Tuple[str, str, str], Tuple[float, np.ndarray, NslcId]]":
    """Assemble per-(net,sta,loc) component arrays at the model sampling rate.

    Mirrors seisbench's stream_to_array: a zero-filled (3, N) grid with each
    component written at its sample offset (Z->0, N/1->1, E/2->2), missing
    components left as zeros.
    Returns (group_tmin, data, representative_nslc) per group.
    """
    sampling_rate = float(spec["sampling_rate"])
    component_order = str(spec["component_order"])
    comp_row = {comp: i for i, comp in enumerate(component_order)}
    if "N" in comp_row:
        comp_row.setdefault("1", comp_row["N"])
    if "E" in comp_row:
        comp_row.setdefault("2", comp_row["E"])
    dt = 1.0 / sampling_rate
    staged: "OrderedDict[Tuple[str, str, str], Dict[int, List[Tuple[float, np.ndarray]]]]" = (
        OrderedDict()
    )
    rep: Dict[Tuple[str, str, str], NslcId] = {}
    for trace in traces:
        net, sta, loc, cha = trace.nslc_id
        if not cha:
            continue
        row = comp_row.get(cha[-1].upper())
        if row is None:
            continue
        try:
            segment = trace.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:
            continue
        if segment.ydata is None or len(segment.ydata) < 2:
            continue
        segment.ydata = segment.ydata.astype(np.float64)
        if scale_factor != 1.0:
            segment.deltat *= scale_factor
        if abs(segment.deltat - dt) > 1e-9:
            try:
                segment.resample(dt)
            except Exception:
                continue
        _apply_filter(segment, spec)
        key = (net, sta, loc)
        # A component row collects every fragment, as seisbench's stream_to_array
        # writes each trace at its offset.
        staged.setdefault(key, {}).setdefault(row, []).append(
            (float(segment.tmin), np.asarray(segment.ydata, dtype=np.float32))
        )
        if row == 0 or key not in rep:
            rep[key] = (net, sta, loc, cha)  # prefer the vertical channel as the label

    out: "OrderedDict[Tuple[str, str, str], Tuple[float, np.ndarray, NslcId]]" = OrderedDict()
    for key, comps in staged.items():
        g_tmin = min(t for frags in comps.values() for t, _ in frags)
        g_tmax = max(t + (len(y) - 1) * dt for frags in comps.values() for t, y in frags)
        n = int(round((g_tmax - g_tmin) * sampling_rate)) + 1
        if n < 4:
            continue
        data = np.zeros((len(component_order), n), dtype=np.float32)
        for row, frags in comps.items():
            for t0, y in frags:
                off = int(round((t0 - g_tmin) * sampling_rate))
                if off < 0:
                    y = y[-off:]
                    off = 0
                m = min(len(y), n - off)
                if m > 0:
                    data[row, off : off + m] = y[:m]
        out[key] = (g_tmin, data, rep[key])
    return out


async def pick(
    request: snuffling_pb2.SnufflingRequestParams,
) -> snuffling_pb2.SnufflingResult:
    """SeisBench deep-learning P/S picker over the selected marker spans, or
    the visible window when nothing is selected.

    The ONNX forward pass runs in the JS worker through onnxruntime-web; numpy
    pre/post-processing mirrors the pinned SeisBench 0.11.7 source.
    """
    import seisbench_infer as sbi

    params = request.snufflings.seisbench
    model = params.model or "PhaseNet"
    weights = params.weights or "instance"
    scale_factor = min(10.0, max(0.1, float(params.scale_factor or 1.0)))
    state.replace_derived_traces("seisbench", [])
    progress_prefix = "SeisBench %s/%s" % (model, weights)
    _report_progress(0.0, "%s · loading model" % progress_prefix)
    model_url, spec = await _ensure_model(model, weights, params.model_base_url or "")
    component_order = str(spec["component_order"])
    _report_progress(0.01, "%s · preparing waveform chunks" % progress_prefix)
    # Qt's picker runs the marker-span scope through the pile chopper in
    # 300-second batches padded by one second plus the main high-pass filter's
    # settling margin (seisbench_picker.py:248), in `mode='visible'`: one pass
    # per selected span, over that span's channels only.
    tinc, model_pad = 300.0, 1.0
    use_view_filter = bool(params.use_predefined_filters)
    view_highpass = (
        float(request.view.highpass)
        if use_view_filter and request.view.HasField("highpass")
        else None
    )
    view_lowpass = (
        float(request.view.lowpass)
        if use_view_filter and request.view.HasField("lowpass")
        else None
    )
    filter_pad = 2.0 / view_highpass if view_highpass is not None else 0.0
    tpad = model_pad + filter_pad
    result = snuffling_pb2.SnufflingResult(show_window=True, origin="seisbench")
    markers = marker_budget.MarkerAccumulator(request.marker_budget)
    n_p = n_s = 0
    annotation_traces: List[ptrace.Trace] = []
    # The progress total comes from span arithmetic alone: windows advance it in
    # equal shares and the model's mini-batches fill the current share.
    # Each chopper batch streams through inference and is released, as Qt's loop
    # does, so the whole selection is never held in memory at once.
    total_windows = max(
        1,
        sum(
            max(1, math.ceil((s_tmax - s_tmin) / tinc))
            for s_tmin, s_tmax, _sel in snuffling_scope.spans(request)
        ),
    )
    completed_windows = 0
    window_batches = 1
    done_in_window = 0

    def batch_done() -> None:
        nonlocal done_in_window
        done_in_window += 1
        share = min(1.0, done_in_window / window_batches)
        frac = min(0.99, 0.20 + 0.79 * (completed_windows + share) / total_windows)
        _report_progress(frac, "%s · running inference" % progress_prefix)

    thresholds = {
        "P": params.p_threshold
        if params.HasField("p_threshold")
        else sbi.default_threshold(spec, "P"),
        "S": params.s_threshold
        if params.HasField("s_threshold")
        else sbi.default_threshold(spec, "S"),
    }
    # Distinct stations across all batches, for the summary line — overlapping
    # markers can run the same station twice, as Qt's per-marker chopper does.
    stations_picked = set()
    for batch in snuffling_scope.chopper_selected_traces(
        request, mode="visible", tinc=tinc, tpad=tpad
    ):
        # Qt drops every `tabu` batch trace before inference
        # (seisbench_picker.py:257): RMS curves and STA/LTA level traces are
        # derived data the model must never pick on.
        batch_traces = [trace for trace in batch.traces if not snuffling_scope.is_tabu_trace(trace)]
        if not batch_traces:
            completed_windows += 1
            continue
        wmin, wmax = float(batch.tmin), float(batch.tmax)
        if use_view_filter:
            batch_traces = [
                _apply_view_filter(trace, view_highpass, view_lowpass, filter_pad)
                for trace in batch_traces
            ]
        groups = station_groups(
            batch_traces,
            wmin - model_pad,
            wmax + model_pad,
            spec,
            scale_factor,
        )
        window_batches = max(
            1, sum(sbi.batch_count(data, spec) for _g_tmin, data, _nslc in groups.values())
        )
        done_in_window = 0
        for key, (g_tmin, data, nslc) in groups.items():
            stations_picked.add(key)
            ann = await sbi.annotate(model_url, data, spec, batch_done, progress_prefix)
            if params.show_annotation_traces and ann.data.size:
                labels = sbi.labels(spec)
                start = g_tmin + ann.start_offset / scale_factor
                if scale_factor != 1.0:
                    blinding = spec.get("blinding") or [0, 0]
                    blind_samples = max(int(value) for value in blinding)
                    start -= (blind_samples / 100.0) * (1.0 - 1.0 / scale_factor)
                for label_index, label in enumerate(labels):
                    if label == "N":
                        continue
                    annotation = ptrace.Trace(
                        network=key[0],
                        station=key[1],
                        location=key[2],
                        channel="%s_%s" % (model, label),
                        tmin=start,
                        deltat=1.0 / (ann.sampling_rate * scale_factor),
                        ydata=np.asarray(ann.data[:, label_index], dtype=np.float64),
                        meta={"tabu": True, "annotation": True},
                    )
                    try:
                        annotation.chop(wmin, wmax)
                    except ptrace.NoData:
                        continue
                    annotation_traces.append(annotation)
            # Qt's SeisBench picker uses kind 0 for P and 1 for S, marks the
            # picks automatic, and rides the pick probability on the marker's
            # incidence angle (seisbench_picker.py:341).
            for phase, kind in (("P", 0), ("S", 1)):
                if phase not in sbi.labels(spec):
                    continue
                threshold = float(thresholds[phase])
                for idx, val in sbi.extract_picks(
                    ann.data[:, sbi.phase_channel(spec, phase)], threshold
                ):
                    model_time = g_tmin + ann.start_offset + idx / ann.sampling_rate
                    t_pick = g_tmin + (model_time - g_tmin) / scale_factor
                    # Qt keeps only picks inside the batch window
                    # (seisbench_picker.py:334): the padding exists for model
                    # context, and a pick in it would duplicate across
                    # neighbouring batches.
                    if not wmin <= t_pick < wmax:
                        continue
                    marker = snuffling_pb2.SnufMarker()
                    marker.tmin = t_pick
                    marker.kind = kind
                    marker.phase = phase
                    marker.automatic = True
                    marker.incidence_angle = float(val)
                    markers.add(marker, [nslc], float(val))
                    n_p, n_s = (n_p + 1, n_s) if phase == "P" else (n_p, n_s + 1)
        completed_windows += 1
    if not stations_picked:
        result.text = "SeisBench picker: no %s data in the visible window." % component_order
    else:
        result.text = "SeisBench %s/%s: %d P and %d S pick(s) across %d station(s)." % (
            model,
            weights,
            n_p,
            n_s,
            len(stations_picked),
        )
    state.replace_derived_traces("seisbench", annotation_traces)
    result.scene.CopyFrom(state.scene_meta())
    _report_progress(1.0, "%s · complete" % progress_prefix)
    return markers.finish(result)
