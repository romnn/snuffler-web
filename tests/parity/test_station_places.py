"""Station places use batched polygon lookup and a memory-only coordinate cache."""

from __future__ import annotations

import harness  # isort: skip # noqa: F401

import asyncio
from collections import OrderedDict
from pathlib import Path
from types import SimpleNamespace
from typing import List
from xml.etree import ElementTree

import pytest
from snuffler_bridge import state, station_places


@pytest.fixture(scope="module")
def regions() -> List[station_places.RegionPolygon]:
    path = Path(__file__).resolve().parents[2] / "web/assets/station-regions.json"
    return station_places.decode_regions(path.read_bytes())


@pytest.fixture(autouse=True)
def clean_places(
    monkeypatch: pytest.MonkeyPatch, regions: List[station_places.RegionPolygon]
) -> None:
    monkeypatch.setattr(state, "stations", {})
    monkeypatch.setattr(state, "event_registry", OrderedDict())
    monkeypatch.setattr(state, "station_xmls", [])
    monkeypatch.setattr(station_places, "_regions", regions)
    monkeypatch.setattr(station_places, "_resolved", {})
    monkeypatch.setattr(station_places, "_event_regions", {})
    monkeypatch.setattr(station_places, "_display_places", {})
    monkeypatch.setattr(station_places, "_display_coordinates", ())
    monkeypatch.setattr(station_places, "_display_metadata_generation", -1)
    monkeypatch.setattr(station_places, "_metadata_documents", ())
    monkeypatch.setattr(station_places, "_metadata_places", {})


def test_station_place_regions_and_ocean(regions: List[station_places.RegionPolygon]) -> None:
    munich = (48.1372, 11.5756)
    montgomery = (32.3668, -86.3)
    ocean = (0.0, -130.0)
    places = station_places.resolve_coordinates([munich, montgomery, ocean], regions)
    assert places[munich].region == "Bayern"
    assert places[munich].country == "Germany"
    assert places[montgomery].region == "Alabama"
    assert places[montgomery].country == "United States of America"
    assert places[ocean] == station_places.Place()


def test_station_place_cache_resolves_only_new_distinct_coordinates(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = []
    original = station_places.resolve_coordinates

    def record(coordinates, polygons):
        calls.append(coordinates)
        return original(coordinates, polygons)

    monkeypatch.setattr(station_places, "resolve_coordinates", record)
    state.add_station("XX", "ONE", "", 48.1372, 11.5756, 0.0, 0.0)
    state.add_station("XX", "ONE", "01", 48.1372, 11.5756, 0.0, 0.0)
    state.add_station("XX", "TWO", "", 48.1372, 11.5756, 0.0, 0.0)
    asyncio.run(station_places.prepare())
    asyncio.run(station_places.prepare())
    assert calls == [[(48.1372, 11.5756)]]
    state.add_station("XX", "THREE", "", 32.3668, -86.3, 0.0, 0.0)
    asyncio.run(station_places.prepare())
    assert calls[-1] == [(32.3668, -86.3)]
    assert len(station_places._resolved) == 2


def test_station_place_metadata_does_not_override_the_geographic_region() -> None:
    site = SimpleNamespace(
        name="Local Observatory", town="Local village", region="Eifel", country="DE"
    )
    station = SimpleNamespace(code="ONE", site=site)
    state.station_xmls.append(
        SimpleNamespace(network_list=[SimpleNamespace(code="XX", station_list=[station])])
    )
    state.add_station("XX", "ONE", "", 48.1372, 11.5756, 0.0, 0.0)
    asyncio.run(station_places.prepare())
    place = station_places.for_station("XX", "ONE", "")
    assert place is not None
    assert (place.locality, place.region, place.country) == ("", "Bayern", "Germany")


def test_station_place_boundary_and_luxembourg_cantons(
    regions: List[station_places.RegionPolygon],
) -> None:
    # TD383 lies near a German regional boundary; the other stations are in Luxembourg.
    expected = {
        (50.72929, 7.53147): ("Nordrhein-Westfalen", "Germany"),
        (49.99233, 6.00481): ("Wiltz", "Luxembourg"),
        (49.473276, 6.081468): ("Esch-sur-Alzette", "Luxembourg"),  # spellcheck:ignore-line
    }
    places = station_places.resolve_coordinates(list(expected), regions)
    for coordinate, (region, country) in expected.items():
        assert (places[coordinate].region, places[coordinate].country) == (region, country)


def test_sumatra_offshore_epicenter_has_no_inferred_region(
    regions: List[station_places.RegionPolygon],
) -> None:
    epicenter = (3.295, 95.982)
    assert (
        station_places.resolve_coordinates([epicenter], regions)[epicenter]
        == station_places.Place()
    )


def test_station_place_missing_coordinates_do_not_invent_a_location() -> None:
    assert station_places.for_station("XX", "UNKNOWN", "") is None


def test_station_place_eifel_coordinates_have_regional_coverage(
    regions: List[station_places.RegionPolygon],
) -> None:
    directory = Path(__file__).resolve().parents[2] / "examples/eifel/meta"
    coordinates = set()
    for path in sorted(directory.glob("*.xml")):
        for station in ElementTree.parse(path).getroot().findall(".//{*}Station"):
            latitude = station.find("{*}Latitude")
            longitude = station.find("{*}Longitude")
            if latitude is not None and longitude is not None:
                coordinates.add((float(latitude.text), float(longitude.text)))
    places = station_places.resolve_coordinates(sorted(coordinates), regions)
    located = sum(bool(place.region) for place in places.values())
    assert len(coordinates) > 50
    assert located / len(coordinates) > 0.95, (located, len(coordinates))


def test_event_locations_use_geography_instead_of_catalog_titles() -> None:
    state.event_registry["munich"] = SimpleNamespace(
        lat=48.1372, lon=11.5756, region="Wrong freeform name"
    )
    state.event_registry["sumatra"] = SimpleNamespace(
        lat=3.295, lon=95.982, region="2004 Sumatra - Andaman Islands Earthquake"
    )
    asyncio.run(station_places.prepare())
    assert station_places.event_locations() == {
        "munich": "Bayern, Germany",
        "sumatra": "Off West Coast of Northern Sumatra",
    }
    assert len(station_places._resolved) == 2
    asyncio.run(station_places.prepare())
    assert len(station_places._resolved) == 2


@pytest.mark.parametrize(
    ("country", "label"),
    [
        ("United States of America", "USA"),
        ("UNITED STATES OF AMERICA", "USA"),
        ("United States", "USA"),
        ("uNiTeD KiNgDoM", "UK"),
        ("United Arab Emirates", "UAE"),
        ("Russian Federation", "Russia"),
        ("Czech Republic", "Czechia"),
        ("Germany", "Germany"),
    ],
)
def test_country_labels_use_common_short_names(country: str, label: str) -> None:
    assert station_places.country_label(country) == label


@pytest.mark.parametrize(
    ("latitude", "longitude", "expected"),
    [
        (3.295, 95.982, "Off West Coast of Northern Sumatra"),
        (27.831, 140.493, "Bonin Islands, Japan"),
        (-30.0, -60.0, "Northeastern Argentina"),
        (-77.845753, 166.675927, "Victoria Land, Antarctica"),
    ],
)
def test_seismic_regions_cover_offshore_events_and_both_hemispheres(
    latitude: float, longitude: float, expected: str
) -> None:
    assert station_places.seismic_region(latitude, longitude) == expected


def test_seismic_regions_handle_poles_dateline_and_invalid_coordinates() -> None:
    assert station_places.seismic_region(90.0, 0.0)
    assert station_places.seismic_region(-90.0, 0.0) == "Antarctica"
    assert station_places.seismic_region(0.0, -180.0) == station_places.seismic_region(0.0, 180.0)
    assert station_places.seismic_region(float("nan"), 0.0) == ""
    assert station_places.seismic_region(0.0, 181.0) == ""
