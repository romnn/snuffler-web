"""The map snuffling's data: plate boundaries, stations and events."""

from __future__ import annotations

from typing import List, Optional, Tuple

from snuffler_web_proto import data_pb2

from . import events, fetch, state


async def plate_boundaries() -> List[data_pb2.PlateBoundary]:
    """Parse pyrocko's shipped Bird (2003) ``plates.kml`` into lon/lat polylines,
    the boundaries the Qt map snuffling draws (memoized)."""
    if state.cached_plate_boundaries is not None:
        return state.cached_plate_boundaries
    import xml.etree.ElementTree as ET

    plates: List[data_pb2.PlateBoundary] = []
    text = await fetch.fetch_text_or_empty(fetch.asset_url("plates.kml"))
    if text:

        def local(tag: str) -> str:
            return tag.rsplit("}", 1)[-1]

        root = ET.fromstring(text)
        for pm in root.iter():
            if local(pm.tag) != "Placemark":
                continue
            name = ""
            lons: List[float] = []
            lats: List[float] = []
            for el in pm.iter():
                lt = local(el.tag)
                if lt == "name" and el.text:
                    name = el.text.strip()
                elif lt == "coordinates" and el.text:
                    for tok in el.text.split():
                        parts = tok.split(",")
                        if len(parts) >= 2:
                            lons.append(float(parts[0]))
                            lats.append(float(parts[1]))
            if len(lons) >= 2:
                plates.append(data_pb2.PlateBoundary(name=name, lons=lons, lats=lats))
    state.cached_plate_boundaries = plates
    return state.cached_plate_boundaries


def map_data(
    request: data_pb2.MapRequest,
    plates: Optional[List[data_pb2.PlateBoundary]] = None,
) -> data_pb2.MapData:
    """Resolve the map's marker projection through the event/station registries."""
    msg = data_pb2.MapData()
    from pyrocko.util import match_nslc

    active_stations: Optional["set[Tuple[str, str]]"] = None
    if request.only_active_event:
        active_stations = set()
        active_event = (
            state.event_registry.get(request.active_event_hash)
            if request.HasField("active_event_hash")
            else None
        )
        active_time = (
            float(active_event.time)
            if active_event is not None
            else (
                float(request.active_event_time) if request.HasField("active_event_time") else None
            )
        )
        if active_time is not None:
            active_stations = {
                (trace.nslc_id[0], trace.nslc_id[1])
                for trace in state.traces
                if trace.tmax >= active_time - 3600.0 and trace.tmin <= active_time + 3600.0
            }

    for (net, sta), (lat, lon, _elev, _depth) in state.stations_by_name().items():
        if active_stations is not None and (net, sta) not in active_stations:
            continue
        # Qt's map shows the viewer's stations *minus the blacklist*
        # (map/__init__.py:242): a station the user hid is hidden everywhere.
        if state.hidden_patterns and match_nslc(state.hidden_patterns, (net, sta, "", "")):
            continue
        station = msg.stations.add()
        station.label = "%s.%s" % (net, sta)
        station.lat = lat
        station.lon = lon
    from pyrocko import util

    for event_hash in request.event_hashes:
        ev = state.event_registry.get(event_hash)
        if ev is None:
            continue
        event = msg.events.add()
        event.lat = float(ev.lat)
        event.lon = float(ev.lon)
        event.magnitude = float(ev.magnitude) if ev.magnitude is not None else 0.0
        event.depth = float(ev.depth) if ev.depth is not None else 0.0
        event.name = (
            str(getattr(ev, "region", "") or "")
            or events.short_event_name(getattr(ev, "name", None))
            or "(Event)"
        )
        event.time = util.time_to_str(float(ev.time))[:19]
        event.event_hash = event_hash
    if plates:
        msg.plate_boundaries.extend(plates)
    return msg
