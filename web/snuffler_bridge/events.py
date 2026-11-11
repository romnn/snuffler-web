"""Events: catalog parsing, the active event, and the event projection the frontend shows."""

from __future__ import annotations

import os
import re
from datetime import datetime, timezone
from typing import Any, List, Optional, Tuple

from snuffler.bridge.v1 import common_pb2, data_pb2, snuffling_pb2

from . import marker_budget, pyrocko_modules, state
from .marker_budget import MarkerBudget, MarkerResultT


def _parse_time(s: str) -> float:
    s = s.strip().rstrip("Z")
    try:
        return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()
    except Exception:
        return 0.0


def parse_fdsn_text(text: str, budget: MarkerBudget = None) -> snuffling_pb2.SnufflingResult:
    """Parse FDSN event 'text' format into event markers and activate the
    largest-magnitude event.

    Every event goes into ``state.event_registry`` with its full origin, so ``e`` on
    one activates the real origin.
    """
    from pyrocko import model

    result = snuffling_pb2.SnufflingResult(show_window=True)
    markers = marker_budget.MarkerAccumulator(budget)
    lines: List[str] = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        c = line.split("|")
        if len(c) < 11:
            continue
        t = _parse_time(c[1])
        lat = float(c[2]) if c[2] else 0.0
        lon = float(c[3]) if c[3] else 0.0
        depth = (float(c[4]) if c[4] else 0.0) * 1000.0
        mag = float(c[10]) if len(c) > 10 and c[10] else 0.0
        # FDSN text columns: EventID | Time | Lat | Lon | Depth | Author |
        # Catalog | ... | Magnitude | MagAuthor | EventLocationName.
        event_id = c[0].strip()
        catalog = c[6].strip() if len(c) > 6 else ""
        region = c[12] if len(c) > 12 else ""
        when = c[1].replace("T", " ")[:19]
        event = model.Event(
            time=t,
            lat=lat,
            lon=lon,
            depth=depth,
            magnitude=mag,
            name=event_id or None,
            region=region or None,
            catalog=catalog or None,
        )
        h = event_id or str(event.get_hash())
        marker = snuffling_pb2.SnufMarker()
        marker.tmin = t
        marker.is_event = True
        marker.event_hash = h
        if event_id:
            marker.event_name = event_id
        if region:
            marker.region = region
        if catalog:
            marker.catalog = catalog
        marker.magnitude = mag
        marker.lat = lat
        marker.lon = lon
        marker.depth = depth
        if len(lines) < 30:
            lines.append("M%.1f  %s  %s" % (mag, when, region))
        markers.add(marker, strength=mag, payload=event)
    best_marker = _activate_largest_event(markers.retained_candidates())
    if best_marker is not None:
        header = (
            "%d event(s) found; active event M%.1f (distance sort enabled).\n"
            "Jumped the view to the events — load waveforms for this time to "
            "see them on data." % (markers.total, float(best_marker.magnitude))
        )
    else:
        header = "No events found in this time window."
    body = "\n".join(lines)
    result.text = header + ("\n\n" + body if body else "")
    return finalize_event_markers(result, markers)


def _activate_largest_event(
    retained: List[Tuple[snuffling_pb2.SnufMarker, Any]],
) -> Optional[snuffling_pb2.SnufMarker]:
    """Register the retained events and activate the largest-magnitude one.

    The active marker is flagged and every marker gets its distance to it for
    the table's Dist [km] column.
    Returns the active marker, or None when nothing was retained."""
    from pyrocko import orthodrome as od

    for marker, event in retained:
        state.register_event(marker.event_hash, event)
    best = max(
        retained,
        key=lambda item: float(item[0].magnitude) if item[0].HasField("magnitude") else 0.0,
        default=None,
    )
    if best is None:
        state.activate_event(None)
        return None
    best_marker = best[0]
    state.activate_event(best_marker.event_hash)
    best_marker.active = True
    for marker, _event in retained:
        distance_m = od.distance_accurate50m(
            float(marker.lat), float(marker.lon), float(best_marker.lat), float(best_marker.lon)
        )
        marker.dist_active_km = float(distance_m) / 1000.0
    return best_marker


def short_event_name(name: Optional[str]) -> str:
    """A compact event code from a QuakeML public id, e.g.
    ``quakeml:earthquake.usgs.gov/...?eventid=us6000jllz&...`` → ``us6000jllz`` or
    ``quakeml:us.anss.org/event/us6000jllz`` → ``us6000jllz``."""
    s = name or ""
    match = re.search(r"eventid=(\w+)", s)
    if match:
        return match.group(1)
    return s.rstrip("/").rsplit("/", 1)[-1].rsplit(":", 1)[-1]


def parse_quakeml(text: str, budget: MarkerBudget = None) -> snuffling_pb2.SnufflingResult:
    """Parse an FDSN QuakeML event response into event markers carrying moment
    tensors, with the Kagan angle of every MT event to the active
    (largest-magnitude) event.

    Also populates the event registry and activates the largest event."""
    from pyrocko import moment_tensor as mtm
    from pyrocko import orthodrome as od
    from pyrocko import util

    # quakeml.py eagerly imports pyrocko.gui.snuffler.marker; install the stub that
    # avoids running the Qt snuffler package init first.
    pyrocko_modules.marker_module()
    from pyrocko.io import quakeml

    result = snuffling_pb2.SnufflingResult(show_window=True)
    markers = marker_budget.MarkerAccumulator(budget)
    qml = quakeml.QuakeML.load_xml(string=text)
    events = qml.get_pyrocko_events()

    entries = []
    best = None
    for ev in events:
        mag = float(ev.magnitude) if ev.magnitude is not None else 0.0
        lat = float(ev.lat)
        lon = float(ev.lon)
        depth = float(ev.depth) if ev.depth is not None else 0.0
        entries.append((ev, mag, lat, lon, depth))
        if best is None or mag > best[1]:
            best = (ev, mag, lat, lon, depth)

    if best is not None:
        best_event = best[0]
        best_hash = str(best_event.name) if best_event.name else str(best_event.get_hash())
        state.register_event(best_hash, best_event)
        state.activate_event(best_hash)
    else:
        state.activate_event(None)

    lines: List[str] = []
    for ev, mag, _lat, _lon, _depth in entries:
        event_hash = str(ev.name) if ev.name else str(ev.get_hash())
        short = short_event_name(ev.name)
        marker = snuffling_pb2.SnufMarker()
        marker.tmin = float(ev.time)
        marker.is_event = True
        marker.event_hash = event_hash
        if short:
            marker.event_name = short
        if getattr(ev, "region", None):
            marker.region = str(ev.region)
        if getattr(ev, "catalog", None):
            marker.catalog = str(ev.catalog)
        marker.magnitude = mag
        marker.lat = float(ev.lat)
        marker.lon = float(ev.lon)
        if ev.depth is not None:
            marker.depth = float(ev.depth)
        if best is not None:
            # The auto-activated event is *visible*: the largest-magnitude
            # marker carries the active flag, and every event row gets its
            # distance to it for the table's Dist [km] column.
            if ev is best[0]:
                marker.active = True
            marker.dist_active_km = (
                float(od.distance_accurate50m(float(ev.lat), float(ev.lon), best[2], best[3]))
                / 1000.0
            )
        moment_tensor = ev.moment_tensor
        if moment_tensor is not None:
            marker.moment_tensor.CopyFrom(moment_tensor_message(moment_tensor))
            if state.active_moment_tensor is not None:
                marker.kagan_deg = float(mtm.kagan_angle(state.active_moment_tensor, moment_tensor))
        when = util.time_to_str(ev.time)[:19]
        if len(lines) < 30:
            lines.append(
                "M%.1f  %s  %s%s"
                % (mag, when, short, "  [MT]" if moment_tensor is not None else "")
            )
        markers.add(marker, strength=mag, payload=ev)

    n_mt = sum(1 for e, *_ in entries if e.moment_tensor is not None)
    retained = markers.retained_candidates()
    for marker, event in retained:
        state.register_event(marker.event_hash, event)
    retained_best = max(
        retained,
        key=lambda item: float(item[0].magnitude) if item[0].HasField("magnitude") else 0.0,
        default=None,
    )
    if retained_best is not None:
        retained_best[0].active = True
        state.activate_event(retained_best[0].event_hash)
    else:
        state.activate_event(None)
    if best is not None:
        header = (
            "%d event(s) found, %d with moment tensors; active event M%.1f "
            "(distance sort + Kagan enabled)." % (markers.total, n_mt, best[1])
        )
    else:
        header = "No events found in this time window."
    body = "\n".join(lines)
    result.text = header + ("\n\n" + body if body else "")
    return finalize_event_markers(result, markers)


def moment_tensor_message(moment_tensor: Any) -> common_pb2.MomentTensor:
    mnn, mee, mdd, mne, mnd, med = (float(value) for value in moment_tensor.m6())
    return common_pb2.MomentTensor(mnn=mnn, mee=mee, mdd=mdd, mne=mne, mnd=mnd, med=med)


def _event_snapshot(event_hash: str, event: Any) -> common_pb2.EventSnapshot:
    snapshot = common_pb2.EventSnapshot(
        event_hash=event_hash,
        time=float(event.time),
        lat=float(event.lat),
        lon=float(event.lon),
        depth=float(event.depth or 0.0),
    )
    if event.magnitude is not None:
        snapshot.magnitude = float(event.magnitude)
    for field in ("name", "region", "catalog"):
        value = getattr(event, field, None)
        if value:
            setattr(snapshot, field, str(value))
    moment_tensor = getattr(event, "moment_tensor", None)
    if moment_tensor is not None:
        snapshot.moment_tensor.CopyFrom(moment_tensor_message(moment_tensor))
    return snapshot


def event_state(limit: Optional[int] = None) -> common_pb2.ActiveEventResult:
    """Project the authoritative event registry and its active-event metrics."""
    from pyrocko import moment_tensor as mtm
    from pyrocko import orthodrome as od

    active = state.event_registry.get(state.active_event_hash) if state.active_event_hash else None
    active_mt = getattr(active, "moment_tensor", None) if active is not None else None
    result = common_pb2.ActiveEventResult(has_event=active is not None)
    if active is not None:
        result.active_hash = state.active_event_hash
    bounded = max(
        1, min(limit or marker_budget.DEFAULT_MARKER_BUDGET, marker_budget.MAX_MARKER_BUDGET)
    )
    items = list(state.event_registry.items())
    selected = items[-bounded:]
    if state.active_event_hash and all(
        event_hash != state.active_event_hash for event_hash, _event in selected
    ):
        active_item = next(
            (item for item in items if item[0] == state.active_event_hash),
            None,
        )
        if active_item is not None:
            selected = [active_item, *selected[-(bounded - 1) :]] if bounded > 1 else [active_item]
    for event_hash, event in selected:
        result.events.add().CopyFrom(_event_snapshot(event_hash, event))
        metric = result.kagan.add()
        metric.event_hash = event_hash
        moment_tensor = getattr(event, "moment_tensor", None)
        if active_mt is not None and moment_tensor is not None:
            metric.kagan_deg = float(mtm.kagan_angle(active_mt, moment_tensor))
        if active is not None:
            metric.dist_km = (
                float(
                    od.distance_accurate50m(
                        float(event.lat),
                        float(event.lon),
                        float(active.lat),
                        float(active.lon),
                    )
                )
                / 1000.0
            )
    return result


def set_active_event(request: data_pb2.SetActiveEventRequest) -> common_pb2.ActiveEventResult:
    """Set or clear the user-chosen active event, as snuffler's
    ``set_active_event_marker`` / ``deactivate_event_marker`` do."""
    from pyrocko import model

    if request.deactivate:
        state.activate_event(None)
        return event_state()

    event = state.event_registry.get(request.event_hash) if request.event_hash else None
    if event is None:
        # Hand-made event marker: derive lat/lon from the marker's single station if
        # it is bound to one, else keep the previous active event's location (Qt).
        lat = lon = 0.0
        bound = (
            state.station_at(request.nslc.net, request.nslc.sta, request.nslc.loc)
            if request.HasField("nslc")
            else None
        )
        if bound is not None:
            lat, lon, _elev, _depth = bound
        elif state.active_event is not None:
            lat, lon = state.active_event[0], state.active_event[1]
        event = model.Event(time=float(request.time), lat=lat, lon=lon, depth=0.0)
        if request.event_hash:
            state.register_event(request.event_hash, event)

    state.activate_event(request.event_hash or None)

    return event_state()


def pyrocko_moment_tensor(tensor: common_pb2.MomentTensor) -> Any:
    from pyrocko import moment_tensor as mtm

    return mtm.MomentTensor(
        mnn=tensor.mnn,
        mee=tensor.mee,
        mdd=tensor.mdd,
        mne=tensor.mne,
        mnd=tensor.mnd,
        med=tensor.med,
    )


def update_event(snapshot: common_pb2.EventSnapshot) -> common_pb2.ActiveEventResult:
    """Replace one registry event from the frontend's complete edited snapshot."""
    from pyrocko import model

    if not snapshot.event_hash:
        raise ValueError("event update requires an event_hash")
    moment_tensor = None
    if snapshot.HasField("moment_tensor"):
        moment_tensor = pyrocko_moment_tensor(snapshot.moment_tensor)
    event = model.Event(
        time=float(snapshot.time),
        lat=float(snapshot.lat),
        lon=float(snapshot.lon),
        depth=float(snapshot.depth),
        magnitude=(float(snapshot.magnitude) if snapshot.HasField("magnitude") else None),
        name=(snapshot.name if snapshot.HasField("name") else None),
        region=(snapshot.region if snapshot.HasField("region") else None),
        catalog=(snapshot.catalog if snapshot.HasField("catalog") else None),
        moment_tensor=moment_tensor,
    )
    state.register_event(snapshot.event_hash, event)
    if snapshot.event_hash == state.active_event_hash:
        state.activate_event(snapshot.event_hash)
    return event_state()


def remove_events(request: data_pb2.RemoveEventsRequest) -> common_pb2.ActiveEventResult:
    """Drop events from the registry, deactivating the active one if it goes."""
    event_hashes = set(request.event_hashes)
    for event_hash in event_hashes:
        state.event_registry.pop(event_hash, None)
    if state.active_event_hash in event_hashes:
        state.activate_event(None)
    return event_state()


def load_event_file(text: str, budget: MarkerBudget = None) -> snuffling_pb2.SnufflingResult:
    """Parse a local pyrocko event file into event markers and activate the
    largest-magnitude event, like the FDSN catalog path."""
    if not text.strip():
        state.activate_event(None)
        result = snuffling_pb2.SnufflingResult(show_window=True, text="empty event file")
        result.event_state.CopyFrom(event_state())
        return result
    import tempfile

    from pyrocko import model

    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as handle:
        handle.write(text)
        path = handle.name
    try:
        events = model.load_events(path)
    except Exception as exc:  # noqa: BLE001 — surface a parse failure as text
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="could not parse event file: %s" % exc
        )
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass

    result = snuffling_pb2.SnufflingResult(show_window=True)
    markers = marker_budget.MarkerAccumulator(budget)
    for event in events:
        lat = float(event.lat)
        lon = float(event.lon)
        depth = float(event.depth or 0.0)
        mag = float(event.magnitude) if event.magnitude is not None else 0.0
        h = str(event.name) if event.name else str(event.get_hash())
        marker = snuffling_pb2.SnufMarker()
        marker.tmin = float(event.time)
        marker.is_event = True
        marker.event_hash = h
        if event.name:
            marker.event_name = str(event.name)[:96]
        if getattr(event, "region", None):
            marker.region = str(event.region)
        if getattr(event, "catalog", None):
            marker.catalog = str(event.catalog)
        marker.magnitude = mag
        marker.lat = lat
        marker.lon = lon
        marker.depth = depth
        markers.add(marker, strength=mag, payload=event)
    best_marker = _activate_largest_event(markers.retained_candidates())
    if best_marker is not None:
        result.text = "%d event(s) loaded; active event M%.1f (distance sort enabled)." % (
            markers.total,
            float(best_marker.magnitude),
        )
    else:
        result.text = "No events in file."
    return finalize_event_markers(result, markers)


def finalize_event_markers(
    result: MarkerResultT, markers: marker_budget.MarkerAccumulator
) -> MarkerResultT:
    result.event_state.CopyFrom(event_state(markers.limit))
    return markers.finish(result)
