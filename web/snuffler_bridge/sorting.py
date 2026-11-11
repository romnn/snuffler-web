"""Track order and grouping: snuffler's sort modes, subsorts and gather keys."""

from __future__ import annotations

import math
from collections import OrderedDict
from typing import Any, List, Optional, Tuple

from pyrocko import trace as ptrace

from . import state

# Subsort -> order of nslc elements for the sort/grouping key (snuffler's subsort).
_SUBSORT_PERMUTATIONS = {
    "nslc": (0, 1, 2, 3),
    "nscl": (0, 1, 3, 2),
    "sncl": (1, 0, 3, 2),
    "lnsc": (2, 0, 1, 3),
    "cnsl": (3, 0, 1, 2),
    # Three elements: snuffler's "Grouped by Location" pair leaves the location out
    # of the key, so every location of a channel lands on the same row.
    "nsc_l": (0, 1, 3),
    "snc_l": (1, 0, 3),
}


def subsort_permutation(subsort: str) -> Tuple[int, ...]:
    """The gather permutation of a subsort; nslc order for an unknown name."""
    return _SUBSORT_PERMUTATIONS.get(subsort, (0, 1, 2, 3))


def distance_m(alat: float, alon: float, blat: float, blon: float) -> float:
    """Surface distance in metres on the WGS84 spheroid, as snuffler measures it.

    ``Location.distance_to`` measures via ``orthodrome_ext``, the compiled
    extension the minimal wheel does not ship.
    ``distance_accurate50m(implementation='python')`` evaluates the same Meeus
    formula with plain ``math``, agreeing with the C path bit-for-bit.
    The scalar entry point on purpose: the ``_numpy`` one broadcasts scalars to
    0-d arrays and its Python branch then dies in ``num.where``.
    """
    from pyrocko import orthodrome as od

    return float(od.distance_accurate50m(alat, alon, blat, blon, implementation="python"))


def distance_deg(alat: float, alon: float, blat: float, blon: float) -> float:
    """Epicentral distance in degrees, converted the way ``Station.dist_deg`` is:
    spheroid metres over the *equatorial* radius, not the mean radius."""
    from pyrocko import orthodrome as od

    return distance_m(alat, alon, blat, blon) / od.earthradius_equator * od.r2d


def event_distance_m(net: str, sta: str, loc: str, distances_3d: bool) -> Optional[float]:
    """Distance from the active event to a station in metres.

    Three-dimensional distance folds in the source depth and the station's depth.
    ``None`` means there is no active event or no coordinates for the station.
    """
    if state.active_event is None:
        return None
    station = state.station_at(net, sta, loc)
    if station is None:
        return None
    elat, elon, edep, _et = state.active_event
    slat, slon, _sele, sdep = station
    # Snuffler sorts on `Station.dist_m`, which is the spheroid distance itself, not
    # a degree value converted back with some radius.
    surface_m = distance_m(elat, elon, slat, slon)
    if distances_3d:
        # `event.depth - station.depth`, exactly as `set_event_relative_data` forms
        # it (station.py:184).
        # It ignores the station elevation, which is an upstream quirk kept for
        # parity rather than corrected here.
        #
        # `sqrt(dd**2 + ...)` rather than `math.hypot`, as upstream: a distance
        # within an ulp of a 0.5 km rounding boundary (IU.ANMO in the bonin corpus,
        # 10152.5 km) prints a different kilometre under the two.
        dd = float(edep) - float(sdep)
        return math.sqrt(dd**2 + surface_m**2)
    return surface_m


def _format_distance(metres: float) -> str:
    """snuffler's `m_float.__str__` (pile_viewer.py:75): kilometres to 0 decimals
    above 10 km, to 1 below, `%.5g` metres under a kilometre."""
    if abs(metres) >= 10000.0:
        return "%g km" % round(metres / 1000.0, 0)
    if abs(metres) >= 1000.0:
        return "%g km" % round(metres / 1000.0, 1)
    return "%.5g m" % metres


def sort_annotation(net: str, sta: str, loc: str, sort: str, distances_3d: bool) -> str:
    """The sort-value prefix of snuffler's track label.

    The stringified ssort members of the gather key (`plabel`,
    pile_viewer.py:3521), in snuffler's own formats: `m_float` for distances,
    `deg_float`'s bare `%4.0f` for azimuths, and `sector_int`'s `[%i]` plus a
    distance for the 12-block sort.
    Empty for the name sort and for a station without coordinates, whose `None`
    members snuffler leaves out of the label.
    """
    if state.active_event is None or sort not in (
        "distance",
        "azimuth",
        "azimuth_blocks",
        "backazimuth",
    ):
        return ""
    station = state.station_at(net, sta, loc)
    if station is None:
        return ""
    if sort == "distance":
        dist = event_distance_m(net, sta, loc, distances_3d)
        return "" if dist is None else _format_distance(dist)
    from pyrocko import orthodrome as od

    elat, elon = state.active_event[:2]
    slat, slon = station[0], station[1]
    if sort == "backazimuth":
        return "%4.0f" % float(od.azimuth(slat, slon, elat, elon))
    azi = float(od.azimuth(elat, elon, slat, slon))
    if sort == "azimuth":
        return "%4.0f" % azi
    dist = event_distance_m(net, sta, loc, distances_3d)
    sector = "[%i]" % int(round((azi + 15.0) / 30.0))
    return sector if dist is None else "%s %s" % (sector, _format_distance(dist))


def _sort_order(
    traces: List[ptrace.Trace], sort: str, subsort: str, distances_3d: bool = False
) -> List[int]:
    """Index order for the traces.

    Distance and azimuth sorts use station coordinates and the active event, and
    fall back to name order without them."""
    perm = subsort_permutation(subsort)

    def namekey(trace: ptrace.Trace) -> Tuple[str, ...]:
        # The gather key itself, which is what snuffler sorts its rows by
        # (`sorted(keys)` over `set_gathering`'s keys).
        # Three elements wide for the location-grouped subsorts, so the traces
        # sharing a row tie here and the stable sort keeps their arrival order.
        nslc = trace.nslc_id
        return tuple(nslc[i] for i in perm)

    geo = sort in ("distance", "azimuth", "azimuth_blocks", "backazimuth")
    if geo and state.active_event is not None and state.stations:
        from pyrocko import orthodrome as od

        elat, elon, _edep, _et = state.active_event

        # Mirrors snuffler's gather key exactly (`set_gathering`, pile_viewer.py):
        # the sort-mode value first, then the subsorted nslc as the tie-break —
        # never distance, except inside the azimuthal blocks where snuffler
        # subsorts each sector by distance.
        # Stations without coordinates order last, which is what snuffler's
        # None-aware float subclasses do; a leading flag stands in for that here.
        def geokey(trace: ptrace.Trace) -> Tuple[Any, ...]:
            net, sta, loc = trace.nslc_id[0], trace.nslc_id[1], trace.nslc_id[2]
            station = state.station_at(net, sta, loc)
            if station is None:
                return (1, 0.0, 0.0, namekey(trace))
            slat, slon, _sele, _sdep = station
            if sort == "distance":
                # Same helper the track annotation shows, so the track order
                # always matches the distance printed on them.
                # It only returns None without coordinates, which this branch
                # excluded.
                return (0, event_distance_m(net, sta, loc, distances_3d), 0.0, namekey(trace))
            if sort == "backazimuth":
                return (0, float(od.azimuth(slat, slon, elat, elon)), 0.0, namekey(trace))
            # In azibazi's own (-180, 180] wrap, so the ring is cut where snuffler
            # cuts it — wrapping to [0, 360) starts the same cyclic order at a
            # different track.
            azi = float(od.azimuth(elat, elon, slat, slon))
            if sort == "azimuth":
                return (0, azi, 0.0, namekey(trace))
            # azimuth_blocks: twelve 30° sectors *centred* on multiples of 30, as
            # snuffler bins them (`sector_int(round((azimuth+15.)/30.))`), with
            # the boundary sector split across ±180 exactly as snuffler splits it
            # (`sector_int` never wraps) and each sector subsorted by distance.
            return (
                0,
                float(round((azi + 15.0) / 30.0)),
                event_distance_m(net, sta, loc, distances_3d),
                namekey(trace),
            )

        return sorted(range(len(traces)), key=lambda i: geokey(traces[i]))

    return sorted(range(len(traces)), key=lambda i: namekey(traces[i]))


def _gather_sort_value(trace: ptrace.Trace, sort: str, distances_3d: bool) -> Tuple[Any, ...]:
    """The sort-mode value snuffler prepends to a track's gather key.

    ``set_gathering`` builds the key as ``ssort(tr) + <subsorted nslc>``
    (pile_viewer.py:1741), so the geo sort value is part of the row's identity:
    two traces the subsort would merge are split when they disagree here.
    Sorting by name prepends nothing.
    ``None`` for a trace with no station, matching each geo entry's
    ``default_getter``.
    """
    if sort not in ("distance", "azimuth", "azimuth_blocks", "backazimuth"):
        return ()
    net, sta, loc = trace.nslc_id[0], trace.nslc_id[1], trace.nslc_id[2]
    station = state.station_at(net, sta, loc)
    if sort == "distance":
        return (event_distance_m(net, sta, loc, distances_3d),)
    if station is None:
        return (None, None) if sort == "azimuth_blocks" else (None,)
    from pyrocko import orthodrome as od

    elat, elon = (state.active_event or (0.0, 0.0, 0.0, 0.0))[:2]
    slat, slon = station[0], station[1]
    if sort == "backazimuth":
        return (float(od.azimuth(slat, slon, elat, elon)),)
    azi = float(od.azimuth(elat, elon, slat, slon))
    if sort == "azimuth":
        return (azi,)
    return (float(round((azi + 15.0) / 30.0)), event_distance_m(net, sta, loc, distances_3d))


def gather_key(
    trace: ptrace.Trace, perm: Tuple[int, ...], sort: str, distances_3d: bool
) -> Tuple[Any, ...]:
    """A trace's row identity: the sort value followed by the subsort permutation
    of the nslc id, exactly snuffler's gather key (`set_gathering`,
    pile_viewer.py:1741)."""
    return _gather_sort_value(trace, sort, distances_3d) + tuple(trace.nslc_id[i] for i in perm)


def row_universe(
    sort: str, subsort: str, distances_3d: bool
) -> List[Tuple[Tuple[Any, ...], ptrace.Trace]]:
    """The track rows of the whole pile, in draw order.

    Snuffler's stack is one row per gather key of every selected trace
    (pile_viewer.py:1750), whether or not it has data in the window, so panning
    across a gap cannot reshuffle the stack.
    Each row carries a representative trace for the label and the annotation.
    """
    selected = state.visible_traces()
    order = _sort_order(selected, sort, subsort, distances_3d)
    perm = subsort_permutation(subsort)
    rows: "OrderedDict[Tuple[Any, ...], ptrace.Trace]" = OrderedDict()
    for i in order:
        trace = selected[i]
        rows.setdefault(gather_key(trace, perm, sort, distances_3d), trace)
    return list(rows.items())
