"""The marker budget: how many markers one result may carry, and which survive a cut.

Snufflings and catalog loads can produce far more markers than the frontend
should hold, so results keep the strongest candidates and count the rest.
"""

from __future__ import annotations

import heapq
import math
from typing import Any, Dict, Iterable, List, Optional, Tuple, TypeVar

from snuffler.bridge.v1 import data_pb2, snuffling_pb2

from . import nslc
from .nslc import NslcId

# The two results that carry markers and an NSLC table.
MarkerResultT = TypeVar("MarkerResultT", snuffling_pb2.SnufflingResult, data_pb2.SnufMarkerList)
# The frontend's requested cap; None or zero means the default.
MarkerBudget = Optional[snuffling_pb2.MarkerBudget]

DEFAULT_MARKER_BUDGET = 20_000
# Also caps the event registry, since every event is a marker too.
MAX_MARKER_BUDGET = 100_000


def marker_limit(budget: MarkerBudget) -> int:
    """The number of markers a result may keep under `budget`."""
    requested = int(budget.max_markers if budget is not None else 0) or DEFAULT_MARKER_BUDGET
    return max(1, min(requested, MAX_MARKER_BUDGET))


def marker_nslcs(
    result: snuffling_pb2.SnufflingResult | data_pb2.SnufMarkerList,
    marker: snuffling_pb2.SnufMarker,
) -> List[NslcId]:
    """The channels a marker of ``result`` references through its NSLC table."""
    table = result.nslc_table
    return [
        (table[index].net, table[index].sta, table[index].loc, table[index].cha)
        for index in marker.nslc_indexes
        if index < len(table)
    ]


def _replace_marker_rows(
    result: MarkerResultT,
    rows: Iterable[Tuple[snuffling_pb2.SnufMarker, Iterable[NslcId]]],
) -> None:
    del result.markers[:]
    del result.nslc_table[:]
    nslc_table: Dict[NslcId, int] = {}
    for marker, nslcs in rows:
        target = result.markers.add()
        target.CopyFrom(marker)
        del target.nslc_indexes[:]
        for channel in nslcs:
            table_index = nslc_table.get(channel)
            if table_index is None:
                table_index = len(result.nslc_table)
                result.nslc_table.add().CopyFrom(nslc.nslc_message(channel))
                nslc_table[channel] = table_index
            target.nslc_indexes.append(table_index)


def _retention_rank(
    marker: snuffling_pb2.SnufMarker, index: int, strength: Optional[float] = None
) -> Tuple[int, int, float, float, int]:
    """One marker's claim to survive a budget cut; higher tuples are kept.

    The active marker outranks everything, then event markers, then strength:
    events carry no incidence angle, so ranking by strength alone would drop
    every event the retained picks reference.
    """
    if strength is None and marker.HasField("incidence_angle"):
        strength = float(marker.incidence_angle)
    score = float(strength or 0.0)
    if not math.isfinite(score):
        score = 0.0
    return (
        int(marker.active),
        int(marker.is_event),
        score,
        -float(marker.tmin),
        -index,
    )


class MarkerAccumulator:
    """Keeps only the strongest marker candidates while counting every result."""

    def __init__(self, budget: MarkerBudget) -> None:
        self.limit = marker_limit(budget)
        self.total = 0
        self._heap: List[
            Tuple[
                Tuple[int, int, float, float, int],
                int,
                snuffling_pb2.SnufMarker,
                Tuple[NslcId, ...],
                Any,
            ]
        ] = []

    def add(
        self,
        marker: snuffling_pb2.SnufMarker,
        nslcs: Iterable[NslcId] = (),
        strength: Optional[float] = None,
        payload: Any = None,
    ) -> None:
        index = self.total
        self.total += 1
        quality = _retention_rank(marker, index, strength)
        entry = (quality, index, marker, tuple(nslcs), payload)
        if len(self._heap) < self.limit:
            heapq.heappush(self._heap, entry)
        elif quality > self._heap[0][0]:
            heapq.heapreplace(self._heap, entry)

    def finish(self, result: MarkerResultT) -> MarkerResultT:
        _replace_marker_rows(
            result,
            (
                (marker, nslcs)
                for _quality, _index, marker, nslcs, _payload in sorted(
                    self._heap, key=lambda entry: entry[1]
                )
            ),
        )
        result.markers_dropped += self.total - len(self._heap)
        result.marker_budget = self.limit
        return result

    def retained_candidates(self) -> List[Tuple[snuffling_pb2.SnufMarker, Any]]:
        return [
            (marker, payload)
            for _quality, _index, marker, _nslcs, payload in sorted(
                self._heap, key=lambda entry: entry[1]
            )
        ]


def truncate_marker_result(result: MarkerResultT, budget: MarkerBudget) -> MarkerResultT:
    """Cut a finished result down to the budget, keeping the strongest markers in order."""
    limit = marker_limit(budget)
    count = len(result.markers)
    result.marker_budget = limit
    if count <= limit:
        return result
    ranked = sorted(
        enumerate(result.markers),
        key=lambda item: _retention_rank(item[1], item[0]),
        reverse=True,
    )[:limit]
    ranked.sort(key=lambda item: item[0])
    kept: List[Tuple[snuffling_pb2.SnufMarker, List[NslcId]]] = []
    for _, marker in ranked:
        copy_marker = snuffling_pb2.SnufMarker()
        copy_marker.CopyFrom(marker)
        kept.append((copy_marker, marker_nslcs(result, marker)))
    _replace_marker_rows(result, kept)
    result.markers_dropped += count - limit
    return result
