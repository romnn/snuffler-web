# /// script
# requires-python = ">=3.11"
# dependencies = ["pyshp==2.3.1", "shapely==2.1.2"]
# ///
"""Build the compact, same-origin station-region lookup.

Administrative polygon coordinates are delta-encoded at 1e-4 degree precision.
This is an explicit data-maintenance step, never a network dependency of the app build.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import urllib.request
import zipfile
from itertools import pairwise
from pathlib import Path
from typing import Iterable

import shapefile
from shapely import coverage_simplify
from shapely.geometry import MultiPolygon, Polygon, shape
from shapely.geometry.polygon import orient

SOURCE_URL = "https://naciscdn.org/naturalearth/10m/cultural/ne_10m_admin_1_states_provinces.zip"
SOURCE_SHA256 = "efc59726337323058f9446210adc96673179cd344e053666ee3d28cb58ba2b05"
LUXEMBOURG_URL = "https://download.data.public.lu/resources/limites-administratives-du-grand-duche-de-luxembourg/20261105-040012/limadmin.geojson"
LUXEMBOURG_SHA256 = "497e99914a9c3286254536ac02a9219dfbadbf2a5aa8719e76d48a6305813ccc"
FE_REVISION = "f4b5b7f5aaa2235da86590b696f24713173dae5e"
FE_URL = f"https://codeload.github.com/savage13/flinn_engdahl/zip/{FE_REVISION}"
FE_SHA256 = "9d1f7b36e1be651256640ed3186a82d992ddf3bd515f337d1ee4cafed90603fc"
SCALE = 10_000


def ring_coordinates(coordinates: Iterable[tuple[float, float]]) -> list[int]:
    result: list[int] = []
    previous: list[int] | None = None
    for lon, lat in coordinates:
        point = [round(lon * SCALE), round(lat * SCALE)]
        if point == previous:
            continue
        result.extend(
            point if previous is None else [point[0] - previous[0], point[1] - previous[1]]
        )
        previous = point
    return result if len(result) >= 8 else []


def read_source(path: Path | None, url: str, digest: str) -> bytes:
    if path:
        raw = path.read_bytes()
    else:
        with urllib.request.urlopen(url, timeout=120) as response:
            raw = response.read()
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError(f"Source does not match the pinned SHA-256: {url}")
    return raw


def epicentral_regions(raw: bytes) -> tuple[dict, str]:
    archive = zipfile.ZipFile(io.BytesIO(raw))
    root = f"flinn_engdahl-{FE_REVISION}/"
    names = archive.read(root + "data/names.asc").decode().splitlines()
    counts = [int(value) for value in archive.read(root + "data/quadsidx.asc").split()]
    if len(counts) != 4 * 91:
        raise ValueError("Expected 91 latitude rows in each Flinn–Engdahl quadrant")
    quadrants = {}
    for index, quadrant in enumerate(("ne", "nw", "se", "sw")):
        values = [int(value) for value in archive.read(root + f"data/{quadrant}sect.asc").split()]
        rows = []
        offset = 0
        for count in counts[index * 91 : (index + 1) * 91]:
            row = values[offset : offset + 2 * count]
            if (
                len(row) != 2 * count
                or not row
                or row[0] != 0
                or any(a >= b for a, b in pairwise(row[::2]))
                or any(region < 1 or region > len(names) for region in row[1::2])
            ):
                raise ValueError(f"Invalid Flinn–Engdahl row in {quadrant}")
            rows.append(row)
            offset += 2 * count
        if offset != len(values):
            raise ValueError(f"Unexpected Flinn–Engdahl data in {quadrant}")
        quadrants[quadrant] = rows
    return {"names": names, "quadrants": quadrants}, archive.read(root + "LICENSE").decode()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--luxembourg-source", type=Path)
    parser.add_argument("--fe-source", type=Path)
    parser.add_argument("--tolerance", type=float, default=0.005)
    parser.add_argument("--out", type=Path, default=Path("web/assets/station-regions.json"))
    args = parser.parse_args()
    raw = read_source(args.source, SOURCE_URL, SOURCE_SHA256)
    luxembourg_raw = read_source(args.luxembourg_source, LUXEMBOURG_URL, LUXEMBOURG_SHA256)
    fe_raw = read_source(args.fe_source, FE_URL, FE_SHA256)
    epicenters, fe_license = epicentral_regions(fe_raw)
    archive = zipfile.ZipFile(io.BytesIO(raw))
    stem = "ne_10m_admin_1_states_provinces"
    reader = shapefile.Reader(
        **{
            suffix: io.BytesIO(archive.read(f"{stem}.{suffix}")) for suffix in ("shp", "shx", "dbf")
        },
        encoding="utf-8",
    )
    features = []
    for feature in reader.iterShapeRecords():
        attributes = feature.record.as_dict()
        country = attributes.get("admin") or ""
        name = attributes.get("name") or attributes.get("name_en") or ""
        if not country:
            continue
        features.append(
            (name, country, attributes.get("iso_a2") or "", shape(feature.shape.__geo_interface__))
        )

    # Simplify shared edges together so neighboring regions cannot develop gaps.
    geometries = coverage_simplify([feature[3] for feature in features], args.tolerance)
    luxembourg = json.loads(luxembourg_raw)["cantons"]["features"]
    luxembourg_geometries = coverage_simplify(
        [shape(feature["geometry"]) for feature in luxembourg], args.tolerance
    )
    sources = [
        (feature["properties"]["CANTON"], "Luxembourg", "LU", geometry)
        for feature, geometry in zip(luxembourg, luxembourg_geometries, strict=True)
    ]
    sources.extend(
        (name, country, code, geometry)
        for (name, country, code, _), geometry in zip(features, geometries, strict=True)
        if country != "Luxembourg"
    )
    regions = []
    for name, country, country_code, geometry in sources:
        polygons = geometry.geoms if isinstance(geometry, MultiPolygon) else [geometry]
        rings = []
        for polygon in polygons:
            if not isinstance(polygon, Polygon) or polygon.is_empty:
                continue
            polygon = orient(polygon, sign=1.0)
            outer = ring_coordinates(polygon.exterior.coords)
            if not outer:
                continue
            holes = [ring_coordinates(hole.coords) for hole in polygon.interiors]
            rings.append([outer, *[hole for hole in holes if hole]])
        if rings:
            regions.append(
                {"name": name, "country": country, "country_code": country_code, "polygons": rings}
            )
    metadata = [
        {
            "name": "Natural Earth 5.1.1 Admin 1",
            "license": "public domain",
            "url": SOURCE_URL,
            "sha256": SOURCE_SHA256,
        },
        {
            "name": "Luxembourg administrative boundaries (cantons)",
            "license": "CC0 1.0",
            "url": LUXEMBOURG_URL,
            "sha256": LUXEMBOURG_SHA256,
        },
        {
            "name": "Flinn–Engdahl regions, savage13 mirror",
            "license": "BSD-2-Clause",
            "license_scope": "Mirror repository; original regionalization published without a license",
            "original_source": "U.S. Geological Survey, ftp://hazards.cr.usgs.gov/feregion/fe_1995/",
            "license_text": fe_license,
            "url": FE_URL,
            "sha256": FE_SHA256,
        },
    ]
    lines = [
        "{",
        '  "sources": ' + json.dumps(metadata, ensure_ascii=False) + ",",
        f'  "scale": {SCALE},',
        '  "delta_encoded": true,',
        '  "epicentral_regions": {',
        '    "names": ' + json.dumps(epicenters["names"], ensure_ascii=False) + ",",
        '    "quadrants": {',
    ]
    for index, (quadrant, rows) in enumerate(epicenters["quadrants"].items()):
        lines.append("      " + json.dumps(quadrant) + ": [")
        for row_index, row in enumerate(rows):
            lines.append("        " + json.dumps(row) + ("," if row_index + 1 < len(rows) else ""))
        lines.append("      ]" + ("," if index + 1 < len(epicenters["quadrants"]) else ""))
    lines.extend(
        [
            "    }",
            "  },",
            '  "regions": [',
        ]
    )
    for index, region in enumerate(regions):
        lines.extend(
            [
                "    {",
                '      "name": ' + json.dumps(region["name"], ensure_ascii=False) + ",",
                '      "country": ' + json.dumps(region["country"], ensure_ascii=False) + ",",
                '      "country_code": ' + json.dumps(region["country_code"]) + ",",
                '      "polygons": [',
            ]
        )
        for polygon_index, polygon in enumerate(region["polygons"]):
            lines.append("        [")
            for ring_index, ring in enumerate(polygon):
                lines.append(
                    "          " + json.dumps(ring) + ("," if ring_index + 1 < len(polygon) else "")
                )
            lines.append("        ]" + ("," if polygon_index + 1 < len(region["polygons"]) else ""))
        lines.extend(["      ]", "    }" + ("," if index + 1 < len(regions) else "")])
    lines.extend(["  ]", "}"])
    content = "\n".join(lines) + "\n"
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(content)
    print(f"{len(regions)} regions: {len(content.encode()):,} bytes JSON")


if __name__ == "__main__":
    main()
