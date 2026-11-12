"""Station place names, resolved in batches and retained only in worker memory."""

from __future__ import annotations

import json
import logging
import math
from bisect import bisect_right
from typing import Any, Dict, List, NamedTuple, Optional, Tuple

import numpy as np
from pyrocko import orthodrome
from snuffler_web_proto import view_pb2

from . import fetch, state

logger = logging.getLogger(__name__)


class Place(NamedTuple):
    locality: str = ""
    region: str = ""
    country: str = ""


class SiteDescription(NamedTuple):
    town: str = ""
    name: str = ""
    region: str = ""
    country: str = ""


class RegionPolygon(NamedTuple):
    region: str
    country: str
    outer: np.ndarray
    holes: List[np.ndarray]
    south: float
    north: float
    west: float
    east: float


_regions: Optional[List[RegionPolygon]] = None
_resolved: Dict[Tuple[float, float], Place] = {}
_event_regions: Dict[Tuple[float, float], str] = {}
_seismic_names: List[str] = []
_seismic_quadrants: Dict[str, List[List[int]]] = {}
_display_places: Dict[Tuple[float, float], Place] = {}
_display_coordinates: Tuple[Tuple[float, float], ...] = ()
_display_metadata_generation = -1
_country_names: Dict[str, str] = {}
_metadata_documents: Tuple[Any, ...] = ()
_metadata_places: Dict[Tuple[str, str], SiteDescription] = {}


def decode_regions(raw: bytes) -> List[RegionPolygon]:
    """Decode the bundled, delta-encoded administrative rings into pyrocko coordinates."""
    global _seismic_names, _seismic_quadrants
    document = json.loads(raw)
    seismic = document["epicentral_regions"]
    _seismic_names = [str(name) for name in seismic["names"]]
    _seismic_quadrants = {
        str(quadrant): [[int(value) for value in row] for row in rows]
        for quadrant, rows in seismic["quadrants"].items()
    }
    scale = float(document["scale"])
    polygons = []
    for region in document["regions"]:
        country = str(region["country"])
        _country_names[country.casefold()] = country
        code = str(region["country_code"]).casefold()
        if len(code) == 2 and code.isalpha():
            _country_names[code] = country
        for rings in region["polygons"]:
            decoded = [
                np.cumsum(np.asarray(ring, dtype=np.float64).reshape(-1, 2), axis=0)[:, ::-1]
                / scale
                for ring in rings
            ]
            outer, *holes = decoded
            polygons.append(
                RegionPolygon(
                    str(region["name"]),
                    str(region["country"]),
                    outer,
                    [hole[::-1] for hole in holes],
                    float(outer[:, 0].min()),
                    float(outer[:, 0].max()),
                    float(outer[:, 1].min()),
                    float(outer[:, 1].max()),
                )
            )
    return polygons


def resolve_coordinates(
    coordinates: List[Tuple[float, float]], polygons: List[RegionPolygon]
) -> Dict[Tuple[float, float], Place]:
    """Locate each distinct coordinate by containment, without merging across borders."""
    if not coordinates:
        return {}
    points = np.asarray(coordinates, dtype=np.float64)
    remaining = np.ones(len(points), dtype=bool)
    result = dict.fromkeys(coordinates, Place())
    for polygon in polygons:
        candidates = remaining & (points[:, 0] >= polygon.south) & (points[:, 0] <= polygon.north)
        # Dateline-spanning rings need pyrocko's spherical test without a longitude cut.
        if polygon.east - polygon.west < 180.0:
            candidates &= (points[:, 1] >= polygon.west) & (points[:, 1] <= polygon.east)
        indices = np.flatnonzero(candidates)
        if not len(indices):
            continue
        hits = orthodrome.contains_points(polygon.outer, points[indices])
        for hole in polygon.holes:
            hits &= ~orthodrome.contains_points(hole, points[indices])
        for index in indices[hits]:
            result[coordinates[index]] = Place(region=polygon.region, country=polygon.country)
            remaining[index] = False
        if not remaining.any():
            break
    return result


def seismic_region(latitude: float, longitude: float) -> str:
    """Flinn–Engdahl geographic region, including oceans, from the bundled latitude tiers."""
    if (
        not _seismic_quadrants
        or not math.isfinite(latitude)
        or not math.isfinite(longitude)
        or not -90.0 <= latitude <= 90.0
        or not -180.0 <= longitude <= 180.0
    ):
        return ""
    # The shared meridian has one identity on both sides of the date line.
    if longitude == -180.0:
        longitude = 180.0
    quadrant = ("n" if latitude >= 0 else "s") + ("e" if longitude >= 0 else "w")
    row = _seismic_quadrants[quadrant][int(abs(latitude))]
    interval = bisect_right(row[::2], int(abs(longitude))) - 1
    name = _seismic_names[row[interval * 2 + 1] - 1]
    compass = {
        "W": "West",
        "W.": "West",
        "E": "East",
        "E.": "East",
        "N": "North",
        "N.": "North",
        "S": "South",
        "S.": "South",
    }
    label = " ".join(compass.get(word, word) for word in name.title().split())
    label = label.replace(" Of ", " of ").replace(" And ", " and ").replace(" The ", " the ")
    return ", ".join(country_label(part) for part in label.removesuffix(" Region").split(", "))


def _coordinates() -> List[Tuple[float, float]]:
    positions = [(lat, lon) for lat, lon, _elevation, _depth in state.stations.values()]
    positions.extend(
        (float(event.lat), float(event.lon)) for event in state.event_registry.values()
    )
    return sorted(
        {
            (lat, lon)
            for lat, lon in positions
            if math.isfinite(lat)
            and math.isfinite(lon)
            and -90.0 <= lat <= 90.0
            and -180.0 <= lon <= 180.0
        }
    )


async def prepare() -> None:
    """Resolve newly loaded station and event positions once in the worker.

    Hovering reads the resulting ViewTrack fields; it cannot invoke this lookup.
    Identical positions across components, locations, and stations share one entry.
    """
    global _regions, _display_coordinates, _display_places, _display_metadata_generation
    coordinates = _coordinates()
    missing = [coordinate for coordinate in coordinates if coordinate not in _resolved]
    if missing:
        if _regions is None:
            try:
                raw = await fetch.fetch_bytes(fetch.asset_url("assets/station-regions.json"))
                _regions = decode_regions(raw)
            except Exception as err:  # noqa: BLE001 — optional geography must not prevent waveform loading.
                logger.warning("station region lookup unavailable: %s", err)
                _regions = []
        # A metadata import may have completed while the asset was in flight.
        coordinates = _coordinates()
        missing = [coordinate for coordinate in coordinates if coordinate not in _resolved]
        _resolved.update(resolve_coordinates(missing, _regions))
        _event_regions.update({coordinate: seismic_region(*coordinate) for coordinate in missing})
    if (
        tuple(coordinates) != _display_coordinates
        or state.response_generation != _display_metadata_generation
    ):
        _display_coordinates = tuple(coordinates)
        _display_metadata_generation = state.response_generation
        places = {coordinate: _resolved[coordinate] for coordinate in coordinates}
        supplied = _metadata()
        for (net, sta, *_location), position in state.stations.items():
            coordinate = (position[0], position[1])
            if coordinate in places and not places[coordinate].country:
                country = supplied.get((net, sta), SiteDescription()).country
                country = _country_names.get(country.casefold(), country)
                if country:
                    places[coordinate] = Place(country=country)
        _display_places = places


def _metadata() -> Dict[Tuple[str, str], SiteDescription]:
    global _metadata_documents, _metadata_places
    documents = tuple(state.station_xmls)
    if len(documents) != len(_metadata_documents) or any(
        current is not previous
        for current, previous in zip(documents, _metadata_documents, strict=True)
    ):
        _metadata_documents = documents
        _metadata_places = {}
        for document in state.station_xmls:
            for network in document.network_list:
                for station in network.station_list:
                    site = station.site
                    if site is not None:
                        _metadata_places[(network.code, station.code)] = SiteDescription(
                            str(site.town or "").strip(),
                            str(site.name or "").strip(),
                            str(site.region or "").strip(),
                            str(site.country or "").strip(),
                        )
    return _metadata_places


_COUNTRY_LABELS = {
    "united states of america": "USA",
    "united states": "USA",
    "united kingdom": "UK",
    "united kingdom of great britain and northern ireland": "UK",
    "united arab emirates": "UAE",
    "russian federation": "Russia",
    "czech republic": "Czechia",
    "republic of korea": "South Korea",
    "democratic people's republic of korea": "North Korea",
    "democratic republic of the congo": "DR Congo",
    "federated states of micronesia": "Micronesia",
    "united republic of tanzania": "Tanzania",
}


def country_label(country: str) -> str:
    """A compact display name, without changing the source's country identities."""
    return _COUNTRY_LABELS.get(country.casefold(), country)


def for_station(net: str, sta: str, loc: str) -> Optional[view_pb2.StationPlace]:
    """Combine cached geographic regions with the station's supplied site description."""
    supplied = _metadata().get((net, sta), SiteDescription())
    position = state.station_at(net, sta, loc)
    geographic = _display_places.get((position[0], position[1]), Place()) if position else Place()
    if geographic.country:
        region, country, locality = geographic.region, geographic.country, ""
    else:
        region = ""
        country = _country_names.get(supplied.country.casefold(), supplied.country)
        locality = supplied.town or (supplied.name if not country else "")
    for suffix in (region, country):
        if suffix and locality.casefold().endswith(", " + suffix.casefold()):
            locality = locality[: -(len(suffix) + 2)].strip()
        if locality.casefold() == suffix.casefold():
            locality = ""
    if not (locality or region or country):
        return None
    return view_pb2.StationPlace(locality=locality, region=region, country=country_label(country))


def event_locations() -> Dict[str, str]:
    """Cached epicentral regions, with geographic seismic regions for offshore events."""
    result = {}
    for event_hash, event in state.event_registry.items():
        place = _resolved.get((float(event.lat), float(event.lon)), Place())
        if place.country:
            label = ", ".join(
                dict.fromkeys(part for part in (place.region, country_label(place.country)) if part)
            )
        else:
            label = _event_regions.get((float(event.lat), float(event.lon)), "")
        if label:
            result[event_hash] = label
    return result
