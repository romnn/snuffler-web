"""Station metadata: coordinate tables and StationXML instrument responses."""

from __future__ import annotations

import os
from typing import List, Tuple

from snuffler.bridge.v1 import bridge_pb2, snuffling_pb2

from . import state


def parse_fdsn_text(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse FDSN station text and remember the coordinates.

    The query asks for channel level, whose rows carry the location code and the
    sensor depth (`#Net|Sta|Loc|Cha|Lat|Lon|Elev|Depth|…`).
    Station-level rows (`#Net|Sta|Lat|Lon|Elev|Site|…`) still parse, for servers
    that answer coarser than asked.
    """
    n = 0
    seen = set()
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        c = line.split("|")
        try:
            # A channel-level row has its latitude in column 4; a station-level
            # row has it in column 2 (where a channel row holds the location
            # code, which never parses as a float).
            if len(c) >= 8:
                try:
                    lat, lon = float(c[4]), float(c[5])
                    is_channel = True
                except ValueError:
                    is_channel = False
            else:
                is_channel = False
            key: Tuple[str, ...]
            if is_channel:
                elev = float(c[6]) if c[6] else 0.0
                depth = float(c[7]) if c[7] else 0.0
                key = (c[0], c[1], c[2])
                state.add_station(c[0], c[1], c[2], lat, lon, elev, depth)
            else:
                if len(c) < 5:
                    continue
                key = (c[0], c[1])
                state.stations[key] = (float(c[2]), float(c[3]), float(c[4]), 0.0)
            if key not in seen:
                seen.add(key)
                n += 1
        except Exception:
            pass
    return snuffling_pb2.SnufflingResult(
        show_window=True, text="%d station(s) with coordinates loaded" % n
    )


def load_station_file(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse a local pyrocko station file into the ``state.stations`` coordinate table
    with pyrocko's own ``model.load_stations``."""
    if not text.strip():
        return snuffling_pb2.SnufflingResult(show_window=True, text="empty station file")
    import tempfile

    from pyrocko import model

    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as handle:
        handle.write(text)
        path = handle.name
    try:
        stations = model.load_stations(path)
    except Exception as exc:  # noqa: BLE001 — surface a parse failure as text
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="could not parse station file: %s" % exc
        )
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
    n = 0
    for station in stations:
        try:
            state.add_station(
                station.network,
                station.station,
                station.location,
                float(station.lat),
                float(station.lon),
                float(station.elevation),
                float(station.depth),
            )
            n += 1
        except Exception:  # noqa: BLE001 — skip malformed entries
            pass
    return snuffling_pb2.SnufflingResult(
        show_window=True, text="%d station(s) with coordinates loaded" % n
    )


def load_station_xml(xml_texts: List[str]) -> snuffling_pb2.SnufflingResult:
    """Parse StationXML documents into instrument responses and station
    coordinates.

    Appended to the document list in load order, as snuffler keeps its
    ``station_xmls`` (pile_viewer.py:4470).
    """
    xml_texts = [text for text in xml_texts if text.strip()]
    if not xml_texts:
        return snuffling_pb2.SnufflingResult(show_window=True, text="no station metadata returned")
    from pyrocko.io import stationxml

    station_xmls = [stationxml.load_xml(string=text) for text in xml_texts]
    state.station_xmls.extend(station_xmls)
    state.response_generation += 1
    state.warned_no_response.clear()
    state.clear_view_cache()
    nchan = sum(
        len(st.channel_list)
        for station_xml in station_xmls
        for net in station_xml.network_list
        for st in net.station_list
    )
    # Coordinates via `get_pyrocko_stations`, as snuffler loads `--stationxml`,
    # not the station element's own lat/lon: real files carry channel positions
    # ~100 m away from the station element.
    for st in (
        station for station_xml in station_xmls for station in station_xml.get_pyrocko_stations()
    ):
        try:
            state.add_station(
                st.network,
                st.station,
                st.location,
                float(st.lat),
                float(st.lon),
                float(st.elevation),
                float(st.depth),
            )
        except Exception:  # noqa: BLE001 — skip malformed entries
            pass
    source_label = (
        "1 StationXML file" if len(xml_texts) == 1 else "%d StationXML files" % len(xml_texts)
    )
    return snuffling_pb2.SnufflingResult(
        show_window=True,
        text="%d channel response(s) loaded from %s" % (nchan, source_label),
    )


def export_station_file() -> bridge_pb2.FileBlob:
    """Qt's download Save trigger, station half: the coordinate table as a pyrocko station file.

    Written by pyrocko's own `model.dump_stations`.
    """
    from pyrocko import model

    pyrocko_stations = [
        model.Station(
            network=key[0],
            station=key[1],
            location=key[2],
            lat=lat,
            lon=lon,
            elevation=elev,
            depth=depth,
        )
        for key, (lat, lon, elev, depth) in state.stations.items()
        if len(key) == 3
    ]
    if not pyrocko_stations:
        raise ValueError("No station coordinates loaded to save.")
    path = "/tmp/stations_out.txt"
    model.dump_stations(pyrocko_stations, path)
    with open(path) as f:
        return bridge_pb2.FileBlob(
            filename="stations.txt", content=f.read().encode(), mime="text/plain"
        )
