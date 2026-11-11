"""Snuffler marker files, read and written with pyrocko's own marker module."""

from __future__ import annotations

import math
from typing import Any, Dict

from snuffler_web_proto import data_pb2, snuffling_pb2

from . import events, marker_budget, pyrocko_modules, state
from .marker_budget import MarkerBudget


def _time_fractional_digits() -> int:
    """Sub-second digits for saved marker times, derived from the data's sample
    interval exactly as Qt derives them (``time_fractional_digits``,
    `pile_viewer.py:1855`): 50 Hz data gets four decimals, not a fixed three."""
    deltatmin = min(
        (float(trace.deltat) for trace in state.traces if trace.deltat > 0.0), default=0.0
    )
    if deltatmin <= 0.0:
        deltatmin = 0.001
    return min(9, max(1, int(-math.floor(math.log10(deltatmin))) + 2))


def markers_to_text(request: data_pb2.MarkerFile) -> str:
    """Serialize markers to pyrocko's snuffler marker-file format."""
    pm = pyrocko_modules.marker_module()
    from pyrocko import model

    # Build the event objects first, keyed by the web-side identity hash: a
    # phase pick associated with an event *in the same file* is linked through
    # the shared ``model.Event`` object, so both rows write the same
    # ``Event.get_hash()`` and ``associate_phases_to_events`` re-links them on
    # any pyrocko side.
    events_by_hash: Dict[str, Any] = {}
    for m in request.markers:
        if not m.is_event:
            continue
        event_hash = m.event_hash if m.HasField("event_hash") and m.event_hash else ""
        canonical = state.event_registry.get(event_hash) if event_hash else None
        if canonical is not None:
            events_by_hash[event_hash] = canonical
            continue
        moment_tensor = (
            events.pyrocko_moment_tensor(m.moment_tensor) if m.HasField("moment_tensor") else None
        )
        event: Any = model.Event(
            time=float(m.tmin),
            lat=float(m.lat) if m.HasField("lat") else 0.0,
            lon=float(m.lon) if m.HasField("lon") else 0.0,
            depth=float(m.depth) if m.HasField("depth") else 0.0,
            magnitude=float(m.magnitude) if m.HasField("magnitude") else None,
            # The event's own name — never the display label; a hand-made event
            # is Qt's ``name='Event'`` (`marker.py:503`).
            name=(m.event_name if m.HasField("event_name") and m.event_name else "Event"),
            region=(m.region if m.HasField("region") and m.region else None),
            catalog=(m.catalog if m.HasField("catalog") and m.catalog else None),
            moment_tensor=moment_tensor,
        )
        if event_hash:
            events_by_hash[event_hash] = event

    ms = []
    for m in request.markers:
        tmin = float(m.tmin)
        tmax = float(m.tmax)
        kind = int(m.kind)
        ids = [(n.net, n.sta, n.loc, n.cha) for n in m.nslc_ids]
        phase = m.phase if m.HasField("phase") else None
        polarity = int(m.polarity) if m.HasField("polarity") else None
        unc = float(m.uncertainty) if m.HasField("uncertainty") else None
        # Event markers save as real EventMarkers (origin + focal mechanism), not
        # downgraded to plain markers.
        if m.is_event:
            event = events_by_hash.get(m.event_hash) if m.HasField("event_hash") else None
            if event is None:
                # An event without a web-side hash still saves; rebuild inline.
                moment_tensor = (
                    events.pyrocko_moment_tensor(m.moment_tensor)
                    if m.HasField("moment_tensor")
                    else None
                )
                event = model.Event(
                    time=tmin,
                    lat=float(m.lat) if m.HasField("lat") else 0.0,
                    lon=float(m.lon) if m.HasField("lon") else 0.0,
                    depth=float(m.depth) if m.HasField("depth") else 0.0,
                    magnitude=float(m.magnitude) if m.HasField("magnitude") else None,
                    name=(m.event_name if m.HasField("event_name") and m.event_name else "Event"),
                    moment_tensor=moment_tensor,
                )
            ms.append(pm.EventMarker(event, kind))
        # A polarity, uncertainty, angle or association is only meaningful on a
        # PhaseMarker, so promote any marker that carries one even if it has no
        # phase name yet.
        elif (
            phase
            or polarity is not None
            or unc is not None
            or m.HasField("event_hash")
            or m.HasField("automatic")
            or m.HasField("incidence_angle")
            or m.HasField("takeoff_angle")
        ):
            event = events_by_hash.get(m.event_hash) if m.HasField("event_hash") else None
            ms.append(
                pm.PhaseMarker(
                    ids,
                    tmin,
                    tmax,
                    kind,
                    phasename=phase,
                    polarity=polarity,
                    uncertainty=unc,
                    automatic=bool(m.automatic) if m.HasField("automatic") else None,
                    incidence_angle=(
                        float(m.incidence_angle) if m.HasField("incidence_angle") else None
                    ),
                    takeoff_angle=(float(m.takeoff_angle) if m.HasField("takeoff_angle") else None),
                    # Association: through the event object when its row is in
                    # this file (both rows then share one hash); otherwise the
                    # stored hash and event time ride along raw, pyrocko's own
                    # fallback keys.
                    event=event,
                    event_hash=(None if event is not None else (m.event_hash or None)),
                    event_time=(
                        float(m.event_time) if event is None and m.HasField("event_time") else None
                    ),
                )
            )
        else:
            ms.append(pm.Marker(ids, tmin, tmax, kind))
    path = "/tmp/markers_out.txt"
    pm.save_markers(ms, path, fdigits=_time_fractional_digits())
    with open(path) as f:
        return f.read()


def markers_from_text(text: str, budget: MarkerBudget = None) -> data_pb2.SnufMarkerList:
    """Parse a pyrocko marker file into the SnufMarker messages the egui side adds.

    Re-links phase picks to their events (``associate_phases_to_events``, which
    Qt's own loader calls) and registers the loaded events so ``e`` activates
    the real origin rather than rebuilding a zero-depth stand-in."""
    pm = pyrocko_modules.marker_module()
    from pyrocko import orthodrome as od

    result = data_pb2.SnufMarkerList()
    markers = marker_budget.MarkerAccumulator(budget)
    path = "/tmp/markers_in.txt"
    with open(path, "w") as f:
        f.write(text)
    # A malformed file raises, which the dispatch turns into the error dialog
    # Qt shows.
    ms = pm.load_markers(path)
    pm.associate_phases_to_events(ms)
    for mk in ms:
        # One bad row must not lose the whole file: parse each marker
        # defensively and skip only the one that fails.
        try:
            ids = mk.get_nslc_ids()
            nslcs = [
                (
                    str(nslc_id[0]) if len(nslc_id) > 0 else "",
                    str(nslc_id[1]) if len(nslc_id) > 1 else "",
                    str(nslc_id[2]) if len(nslc_id) > 2 else "",
                    str(nslc_id[3]) if len(nslc_id) > 3 else "",
                )
                for nslc_id in ids
            ]
            # Built off-list and appended at the end so a half-parsed marker is
            # dropped whole rather than kept truncated.
            marker = snuffling_pb2.SnufMarker()
            marker.tmin = float(mk.tmin)
            marker.tmax = float(mk.tmax)
            # Qt accepts any integer kind; the transport is unsigned 32-bit and
            # the frontend colours 0..7, so fold negatives the way Python's
            # modulo already does.
            marker.kind = int(mk.kind or 0) % 256
            marker.is_event = isinstance(mk, pm.EventMarker)
            event = None
            if isinstance(mk, pm.EventMarker):
                ev = mk.get_event()
                event = ev
                if ev is not None:
                    if ev.name:
                        marker.event_name = str(ev.name)[:96]
                    if getattr(ev, "region", None):
                        marker.region = str(ev.region)
                    if getattr(ev, "catalog", None):
                        marker.catalog = str(ev.catalog)
                    if ev.magnitude is not None:
                        marker.magnitude = float(ev.magnitude)
                    if ev.depth is not None:
                        marker.depth = float(ev.depth)
                    marker.lat = float(ev.lat)
                    marker.lon = float(ev.lon)
                    if ev.moment_tensor is not None:
                        marker.moment_tensor.CopyFrom(
                            events.moment_tensor_message(ev.moment_tensor)
                        )
                try:
                    ehash = mk.get_event_hash()
                except Exception:
                    ehash = None
                known_hash = state.known_event_hash(ev) if ev is not None else None
                if known_hash or ehash:
                    marker.event_hash = known_hash or str(ehash)
                if ev is not None and state.active_event is not None:
                    marker.dist_active_km = (
                        float(
                            od.distance_accurate50m(
                                float(ev.lat),
                                float(ev.lon),
                                state.active_event[0],
                                state.active_event[1],
                            )
                        )
                        / 1000.0
                    )
            if isinstance(mk, pm.PhaseMarker):
                try:
                    phase = mk.get_phasename()
                except Exception:
                    phase = None
                if phase is not None:
                    marker.phase = phase
                try:
                    polarity = mk.get_polarity()
                except Exception:
                    polarity = None
                if polarity is not None:
                    marker.polarity = int(polarity)
                # PhaseMarker stores these privately (no public getters).
                unc = getattr(mk, "_uncertainty", None)
                if unc is not None:
                    marker.uncertainty = float(unc)
                automatic = getattr(mk, "_automatic", None)
                if automatic is not None:
                    marker.automatic = bool(automatic)
                inc = getattr(mk, "_incidence_angle", None)
                if inc is not None:
                    marker.incidence_angle = float(inc)
                tko = getattr(mk, "_takeoff_angle", None)
                if tko is not None:
                    marker.takeoff_angle = float(tko)
                # The associated event's hash and time (phase-to-event link) —
                # after association the hash comes off the shared event object,
                # so it matches the event row's own.
                try:
                    ehash = mk.get_event_hash()
                except Exception:
                    ehash = None
                if ehash:
                    marker.event_hash = str(ehash)
                etime = None
                try:
                    etime = mk.get_event_time()
                except Exception:
                    etime = None
                if etime is not None:
                    marker.event_time = float(etime)
            markers.add(marker, nslcs, payload=event)
        except Exception:
            continue
    for marker, event in markers.retained_candidates():
        if event is not None and marker.event_hash:
            state.register_event(marker.event_hash, event)
    return events.finalize_event_markers(result, markers)
