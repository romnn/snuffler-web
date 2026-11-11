"""What a snuffling operates on: the selected time spans and their chopped traces."""

from __future__ import annotations

from typing import Any, Callable, Iterator, List, Optional, Tuple

from pyrocko import trace as ptrace
from snuffler_web_proto import snuffling_pb2, view_pb2

from . import sorting, state


def is_tabu_trace(trace: ptrace.Trace) -> bool:
    """Whether a snuffling derived this trace, so snufflings and exports skip it."""
    meta = getattr(trace, "meta", None)
    return bool(isinstance(meta, dict) and meta.get("tabu"))


def _inview_work_traces(view: view_pb2.ViewParams) -> List[ptrace.Trace]:
    """The traces a `mode='inview'` snuffling operates on.

    Every visible trace whose gather-key row lies in the shown track window.
    Qt's `trace_selector` tests row membership (pile_viewer.py:1798), so a
    grouped row contributes *all* its members, not one list slot per track.
    Returned in the pile chopper's `full_id` order (`_process_chopped`,
    pile.py:1213), not the viewer's sort.
    Never rotated: `chopper_selected_traces` hands snufflings the pile's data,
    with the GUI rotation explicitly not respected (snuffling.py:985).
    """
    sort = view.sort or "name"
    subsort = view.subsort or "nslc"
    distances_3d = bool(view.distances_3d)
    rows = sorting.row_universe(sort, subsort, distances_3d)
    row_index = {key: i for i, (key, _tr) in enumerate(rows)}
    track_offset = max(0, int(view.track_offset))
    track_count = max(0, int(view.track_count) or len(rows))
    lo, hi = track_offset, track_offset + track_count
    perm = sorting.subsort_permutation(subsort)
    inview = [
        trace
        for trace in state.visible_traces()
        if lo <= row_index.get(sorting.gather_key(trace, perm, sort, distances_3d), -1) < hi
    ]
    return sorted(inview, key=lambda trace: trace.full_id)


def spans(
    request: snuffling_pb2.SnufflingRequestParams,
    skip_instant: bool = False,
) -> List[Tuple[float, float, Optional[Any]]]:
    """The processing scope Qt gives every snuffling (`chopper_selected_traces`,
    snuffling.py:1000): one ``(tmin, tmax, trace_selector)`` per selected
    non-event marker — the selector is the marker's channel patterns — or the
    visible window with no extra filter when nothing is selected.

    ``skip_instant`` reproduces STA/LTA's ``marker_selector`` (stalta.py:163),
    which drops zero-length picks from the scope."""
    from pyrocko import util as putil

    view = request.view
    spans: List[Tuple[float, float, Optional[Any]]] = []
    for s in request.selected_spans:
        if skip_instant and float(s.tmax) <= float(s.tmin):
            continue
        pats = ["%s.%s.%s.%s" % (n.net, n.sta, n.loc, n.cha) for n in s.nslc_ids]
        sel: Optional[Any] = None
        if pats:

            def sel(trace: ptrace.Trace, pats: List[str] = pats) -> bool:
                return bool(putil.match_nslc(pats, trace.nslc_id))

        spans.append((float(s.tmin), float(s.tmax), sel))
    if not spans:
        spans.append((float(view.tmin), float(view.tmax), None))
    return spans


def chopper_selected_traces(
    request: snuffling_pb2.SnufflingRequestParams,
    mode: str = "inview",
    skip_instant: bool = False,
    trace_filter: Optional[Callable[[ptrace.Trace], bool]] = None,
    **chopper_kwargs: Any,
) -> Iterator[Any]:
    """Qt's `Snuffling.chopper_selected_traces` (snuffling.py:952) over the
    real pile: one `Pile.chopper` run per `spans` entry, yielding its
    batches.

    The viewer's `mode` selector is intersected with the marker's, as Qt does at
    snuffling.py:1072: the shown track rows for ``'inview'``, the hide and quick
    filters for ``'visible'``.
    """
    viewer_sel: Callable[[ptrace.Trace], bool]
    if mode == "inview":
        rows = {id(trace) for trace in _inview_work_traces(request.view)}

        def viewer_sel(trace: ptrace.Trace) -> bool:
            return id(trace) in rows
    elif mode == "visible":
        viewer_sel = state.is_trace_visible
    else:
        # Qt raises on an unknown mode too; 'all' has no consumer here yet.
        raise ValueError("invalid mode argument: %r" % mode)

    p = state.pile()
    for s_tmin, s_tmax, sel in spans(request, skip_instant=skip_instant):

        def selector(trace: ptrace.Trace, sel: Optional[Any] = sel) -> bool:
            return (
                viewer_sel(trace)
                and (trace_filter is None or trace_filter(trace))
                and (sel is None or sel(trace))
            )

        yield from p.chopper(
            tmin=s_tmin, tmax=s_tmax, trace_selector=selector, style="batch", **chopper_kwargs
        )
