"""The web oracle: ``web/bridge.py`` asked the same question, in plain CPython.

``bridge.py`` is the whole web-side processing pipeline, and it imports without a
browser: every ``import js`` and ``pyfetch`` in it is lazy and lives inside a
network-only function. So with ``PYTHONPATH=web/proto:web`` the harness calls
``_get_view_data`` directly — the same entry point the Pyodide worker calls — and
unpacks the flat ``ys`` payload back into per-track arrays.

The bridge keeps module-level state (the loaded pile, stations, responses, the
processed-trace cache, the remembered "fixed" scale ranges). :func:`reset` puts all
of it back so one scenario can never colour the next.
"""

from __future__ import annotations

import importlib
from dataclasses import dataclass, field
from typing import Any, List, Optional, Tuple

import bridge
import numpy as np
from scenario import CorpusFiles, Scenario
from snuffler.bridge.v1 import view_pb2

NslcId = Tuple[str, str, str, str]


@dataclass
class Segment:
    nslc: NslcId
    tmin: float
    deltat: float
    y: np.ndarray
    # False when the payload is interleaved (t_rel, y) envelope points rather than
    # uniformly spaced samples — the web side's display decimation.
    uniform: bool = True


@dataclass
class WebSnapshot:
    meta: view_pb2.ViewMeta
    window: Tuple[float, float]
    segments: List[Segment]
    # One entry per track, in the order the frontend stacks them.
    track_order: List[NslcId] = field(default_factory=list)
    # Which traces landed on each track. More than one only where a row groups
    # several channels: the segments of one gapped channel, or the locations of a
    # channel under the "Grouped by Location" subsorts.
    track_members: List[List[NslcId]] = field(default_factory=list)
    # (ymin, ymax, yscale) per span, keyed by the span's own trace, because a row
    # can carry traces on different scales. Exact duplicates are folded — the
    # segments of one gapped channel share a scale — so a repeated trace here
    # always means two of its spans genuinely disagree.
    span_scales: List[Tuple[NslcId, Tuple[float, float, float]]] = field(default_factory=list)
    sort_annotations: List[str] = field(default_factory=list)
    # Trace boxes per track, in track order: (tmin, tmax, style) as the ViewTrack
    # carries them — the pile's coverage, present even when a track has no spans
    # because the zoom put every channel below `min_deltat_allow`.
    boxes: List[List[Tuple[float, float, int]]] = field(default_factory=list)
    raw_ys: Optional[np.ndarray] = None


def reset() -> None:
    """Return ``bridge`` to a freshly imported state.

    A reload rather than a hand-written list of module globals: the list would have
    to name every piece of state the bridge keeps — pile, stations, caches, scale
    memories, generation counters — and silently stop being complete the day the
    bridge grows another one. Re-executing the module is complete by construction,
    and cheap, since every import it performs is already cached.
    """
    importlib.reload(bridge)


def load(files: CorpusFiles, extras: List[str]) -> None:
    reset()
    bridge._load_path_scene("waveforms", str(files.waveforms), "detect")
    # In the order the scenario lists them, exactly as a user would load them.
    for extra in extras:
        if extra == "stations" and files.stations is not None:
            bridge._load_stations_result(files.stations.read_text())
        elif extra == "events" and files.events is not None:
            bridge._load_events_result(files.events.read_text())
        elif extra == "responses":
            # One call for the whole batch, the way the app receives a multi-file
            # selection from its picker.
            bridge._load_responses_result([path.read_text() for path in files.responses])
        else:
            raise KeyError(extra)


def params(scenario: Scenario, tmin: float, tmax: float) -> view_pb2.ViewParams:
    state = scenario.state
    req = view_pb2.ViewParams(
        tmin=tmin,
        tmax=tmax,
        npx=int(state["npx"]),
        sort=state["sort"],
        subsort=state["subsort"],
        rotate=float(state["rotate"]),
        remove_response=bool(state["remove_response"]),
        mode=state["mode"],
        demean=bool(state["demean"]),
        degap=bool(state["degap"]),
        allow_downsampling=bool(state["allow_downsampling"]),
        fft_filtering=bool(state["fft_filtering"]),
        bandpass_lphp=bool(state["bandpass_lphp"]),
        distances_3d=bool(state["distances_3d"]),
        scaling_base=state["scaling_base"],
        scaling_group=state["scaling_group"],
        gain=float(state["gain"]),
        fix_scale_ranges=bool(state["fix_scale_ranges"]),
        show_scale_ranges=bool(state["show_scale_ranges"]),
        show_scale_axes=bool(state["show_scale_axes"]),
        visible_length=float(state["visible_length"]),
        liberal_fetch=bool(state["liberal_fetch"]),
        # Every track, not a page: a scenario compares the whole stack or none of it.
        track_offset=0,
        track_count=1 << 32,
    )
    if state["highpass"] is not None:
        req.highpass = float(state["highpass"])
    if state["lowpass"] is not None:
        req.lowpass = float(state["lowpass"])
    return req


def snapshot(req: view_pb2.ViewParams) -> WebSnapshot:
    meta, ys = bridge._get_view_data(req)
    segments: List[Segment] = []
    order: List[NslcId] = []
    track_members: List[List[NslcId]] = []
    scales: List[Tuple[NslcId, Tuple[float, float, float]]] = []
    annotations: List[str] = []
    boxes: List[List[Tuple[float, float, int]]] = []

    for track in meta.tracks:
        nslc: NslcId = (track.net, track.sta, track.loc, track.cha)
        order.append(nslc)
        annotations.append(track.sort_annotation)
        boxes.append([(float(b.tmin), float(b.tmax), int(b.style)) for b in track.boxes])
        # One track, one or more spans: a channel that arrives in pieces draws every
        # piece on the same row, and so do the locations of a channel when the
        # subsort groups over them. Each span names the trace it came from, which is
        # what the row's membership and the per-span scales are keyed on.
        members: List[NslcId] = []
        for span in track.spans:
            span_nslc: NslcId = (track.net, track.sta, span.loc, track.cha)
            if span_nslc not in members:
                members.append(span_nslc)
            scale = (span_nslc, (float(span.ymin), float(span.ymax), float(span.yscale)))
            if scale not in scales:
                scales.append(scale)
            payload = np.asarray(ys[span.offset : span.offset + _payload_len(span)])
            if span.dt > 0.0:
                segments.append(
                    Segment(
                        nslc=span_nslc,
                        tmin=float(req.tmin) + float(span.t0_rel),
                        deltat=float(span.dt),
                        y=payload.astype(np.float64),
                    )
                )
            elif payload.size:
                # dt == 0 marks the decimated form: interleaved (t_rel, y) extrema.
                segments.append(
                    Segment(
                        nslc=span_nslc,
                        tmin=float(req.tmin) + float(span.t0_rel),
                        deltat=0.0,
                        y=payload.astype(np.float64),
                        uniform=False,
                    )
                )
        members.sort()
        track_members.append(members)

    return WebSnapshot(
        meta=meta,
        window=(float(meta.view_tmin), float(meta.view_tmax)),
        segments=segments,
        track_order=order,
        track_members=track_members,
        span_scales=scales,
        sort_annotations=annotations,
        boxes=boxes,
        raw_ys=ys,
    )


def _payload_len(span: Any) -> int:
    """How many floats of ``ys`` belong to this span.

    ``count`` is the number of *points*; the decimated form carries two floats per
    point because it interleaves the relative time with the sample.
    """
    return int(span.count) * (1 if span.dt > 0.0 else 2)


def pile_fingerprint() -> List[Tuple[Any, ...]]:
    """What the loaded pile looks like, in enough detail to notice a rewrite."""
    return [
        (
            tr.nslc_id,
            round(float(tr.tmin), 9),
            round(float(tr.deltat), 12),
            int(tr.data_len()),
            float(np.sum(np.asarray(tr.ydata, dtype=np.float64))) if tr.ydata is not None else 0.0,
        )
        for tr in bridge._TRACES
    ]


@dataclass
class WebRun:
    request: view_pb2.ViewParams
    first: WebSnapshot
    second: WebSnapshot
    pile_before: List[Tuple[Any, ...]]
    pile_after: List[Tuple[Any, ...]]


def run(scenario: Scenario, files: CorpusFiles) -> WebRun:
    """Snapshot one scenario twice, and watch the pile while doing it.

    Twice on purpose, and fingerprinted on purpose. ``trace.degapper`` rewrites the
    traces it is handed and the bridge hands it the loaded pile itself, so drawing a
    view can change the data underneath the next one. Neither the returned payload
    nor a second identical request necessarily shows that — degapping an already
    degapped pile is a no-op — which is why the pile itself is checked directly.
    """
    load(files, scenario.load)
    data_tmin = min((tr.tmin for tr in bridge._TRACES), default=0.0)
    data_tmax = max((tr.tmax for tr in bridge._TRACES), default=1.0)
    tmin, tmax = scenario.window(data_tmin, data_tmax)
    req = params(scenario, tmin, tmax)
    before = pile_fingerprint()
    first = snapshot(req)
    second = snapshot(req)
    return WebRun(
        request=req,
        first=first,
        second=second,
        pile_before=before,
        pile_after=pile_fingerprint(),
    )
