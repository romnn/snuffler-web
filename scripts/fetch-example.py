#!/usr/bin/env python3
"""Fetch one example dataset from the datacenters that hold it.

The queries live in the dataset's own ``example.toml`` under ``[source]``, so the
provenance of every example is declarative and sits beside the prose describing it.
This script is the one implementation that reads them, rather than a copy of the
same fifty lines per dataset.

It is provenance, not a reproducible generator. StationXML carries a ``<Created>``
stamp and datacenters revise metadata, so re-fetching does not reproduce the
checked-in bytes: for a dataset the parity suite measures, that drifts
``tests/parity/corpus/manifest.json`` and the two have to move together. Re-fetch
to change a dataset on purpose.

    uv run --project qt python scripts/fetch-example.py <slug> [--dry-run]
"""

from __future__ import annotations

import argparse
import sys
import tomllib
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, List

ROOT = Path(__file__).resolve().parents[1]
EXAMPLES = ROOT / "examples"

# EarthScope retired its FDSN event service in June 2026, so origins come from the
# USGS regardless of who holds the waveforms.
USGS = "https://earthquake.usgs.gov/fdsnws"

REQUIRED = ("datacenter", "networks", "stations", "channels", "start", "end")


def _get(service: str, query: Dict[str, str], dry_run: bool) -> bytes:
    url = f"{service}?{urllib.parse.urlencode(query)}"
    print(f"GET {url}")
    if dry_run:
        return b""
    with urllib.request.urlopen(url, timeout=600) as response:
        return response.read()


def _source(slug: str) -> Dict[str, Any]:
    spec = EXAMPLES / slug / "example.toml"
    if not spec.exists():
        raise SystemExit(f"no such example: examples/{slug}/example.toml")
    source = tomllib.loads(spec.read_text()).get("source")
    if not source:
        raise SystemExit(
            f"examples/{slug}/example.toml has no [source]: its bytes did not come from\n"
            "a query this script can reproduce, so they can only be restored from git."
        )
    missing = [key for key in REQUIRED if not source.get(key)]
    if missing:
        raise SystemExit(f"examples/{slug}: [source] is missing {', '.join(missing)}")
    return source


def fetch(slug: str, dry_run: bool) -> List[Path]:
    source = _source(slug)
    directory = EXAMPLES / slug
    window = {"starttime": source["start"], "endtime": source["end"]}
    selection = {
        "sta": source["stations"],
        "loc": source.get("locations", "*"),
        "cha": source["channels"],
    }
    written: List[Path] = []

    waveforms = _get(
        f"{source['datacenter']}/dataselect/1/query",
        {"net": ",".join(source["networks"]), **selection, **window, "format": "miniseed"},
        dry_run,
    )
    out = directory / f"{slug}.mseed"
    if not dry_run:
        out.write_bytes(waveforms)
        _drop_empty_records(out)
    written.append(out)

    # One StationXML per network at response level, the way a datacenter hands it
    # over and the shape both frontends already accept. The time constraints
    # matter: without them the query returns every epoch a station ever had.
    for network in source["networks"]:
        meta = _get(
            f"{source['datacenter']}/station/1/query",
            {"net": network, **selection, **window, "level": "response", "format": "xml"},
            dry_run,
        )
        out = directory / "meta" / f"{network}.xml"
        if not dry_run:
            out.parent.mkdir(exist_ok=True)
            out.write_bytes(meta)
        written.append(out)

    event_id = source.get("event_id")
    if event_id:
        written.append(_fetch_catalog(directory, str(event_id), dry_run))
    return written


def _drop_empty_records(path: Path) -> None:
    """Rewrite ``path`` without the MiniSEED records that carry no samples.

    Datacenters ship them — the Sumatra window came back with nine, spread over three
    LHZ channels — and they are not waveforms: zero samples, and a sample rate of zero
    with them. pyrocko reads each as a trace, which costs a viewer a blank row per
    record, and pyrocko's ``pile`` cannot index them at all: ``dispatch_key`` takes
    ``log(deltatmin)``, and a deltat of zero raises. Snuffler therefore cannot open a
    file that contains one, which is not a state to hand a reader an example in.

    Records are copied, not re-encoded: dropping a record is deleting its bytes and
    concatenating the rest, so every sample and every start time in what remains is
    the datacenter's own. Re-encoding through ``io.save`` would have been three lines,
    but MiniSEED keeps times in units of 0.0001 s and these start on the 47th
    microsecond, so a round trip moves them — a small lie in a file whose point is
    that it is real.

    Each record declares its own length in blockette 1000, which is how a file mixing
    4096-byte data records with 512-byte empty ones is walked at all. A record without
    that blockette leaves the next boundary unknowable, so this refuses rather than
    guesses. Only rewrites when there is something to drop.
    """
    raw = path.read_bytes()
    keep: List[bytes] = []
    dropped = 0
    offset = 0
    while offset < len(raw):
        nsamples = int.from_bytes(raw[offset + 30 : offset + 32], "big")
        nblockettes = raw[offset + 39]
        cursor = int.from_bytes(raw[offset + 46 : offset + 48], "big")
        length = None
        for _ in range(nblockettes):
            if cursor == 0 or offset + cursor + 7 > len(raw):
                break
            kind = int.from_bytes(raw[offset + cursor : offset + cursor + 2], "big")
            if kind == 1000:
                length = 1 << raw[offset + cursor + 6]
                break
            cursor = int.from_bytes(raw[offset + cursor + 2 : offset + cursor + 4], "big")
        if length is None:
            raise SystemExit(
                f"{path.name}: record at byte {offset} has no blockette 1000, so its "
                "length is unknown and the file cannot be walked safely"
            )
        if nsamples:
            keep.append(raw[offset : offset + length])
        else:
            dropped += 1
        offset += length

    if not dropped:
        return
    if not keep:
        raise SystemExit(f"{path.name}: every record is empty; nothing to write")
    path.write_bytes(b"".join(keep))
    print(f"          dropped {dropped} empty record(s), {len(keep)} kept")


def _fetch_catalog(directory: Path, event_id: str, dry_run: bool) -> Path:
    """The origin, converted to the event yaml pyrocko reads.

    Converted rather than copied because that is the format both frontends load; the
    numbers are the USGS's, untouched. Only the name is rewritten: QuakeML's is a
    resource URI, where the event id is what a reader can look up.

    QuakeML first, because it carries the moment tensor and so a beachball. It does
    not always parse: a USGS event aggregates origins contributed by several
    agencies, and pyrocko's reader is strict about ones that omit a longitude or a
    nodal-plane strike — true of Tohoku, Sumatra and Ridgecrest, among others. The
    GeoJSON summary is the same origin without the mechanism, so it is the fallback
    rather than a reason to ship an example with no catalogue. (Not the text format:
    it answers an `eventid` query with an empty body.)
    """
    out = directory / "catalog.yaml"
    raw = _get(f"{USGS}/event/1/query", {"eventid": event_id, "format": "xml"}, dry_run)
    if dry_run:
        return out

    from pyrocko import model

    try:
        events = _events_from_quakeml(raw, event_id)
        print("          catalog: QuakeML, with moment tensor")
    except Exception as exc:  # noqa: BLE001 — any parse failure falls back
        print(f"          catalog: QuakeML unreadable ({type(exc).__name__}), using the summary")
        summary = _get(f"{USGS}/event/1/query", {"eventid": event_id, "format": "geojson"}, dry_run)
        events = _events_from_geojson(summary, event_id)
    model.dump_events(events, str(out), format="yaml")
    return out


def _events_from_quakeml(raw: bytes, event_id: str) -> List[Any]:
    from pyrocko.io import quakeml

    qml = quakeml.QuakeML.load_xml(string=raw.decode("utf-8"))
    events = qml.get_pyrocko_events()
    descriptions = qml.event_parameters.event_list[0].description_list
    for event in events:
        event.name = event_id
        if descriptions:
            event.region = descriptions[0].text
    return list(events)


def _events_from_geojson(raw: bytes, event_id: str) -> List[Any]:
    """One USGS event: `[lon, lat, depth_km]` plus properties, time in epoch ms."""
    import json

    from pyrocko import model

    doc = json.loads(raw)
    properties = doc.get("properties") or {}
    lon, lat, depth_km = (doc.get("geometry") or {}).get("coordinates", [None, None, None])
    if lon is None or properties.get("time") is None:
        raise SystemExit(f"the USGS returned no usable origin for {event_id}")
    return [
        model.Event(
            name=event_id,
            time=float(properties["time"]) / 1000.0,
            lat=float(lat),
            lon=float(lon),
            depth=float(depth_km or 0.0) * 1000.0,
            magnitude=float(properties["mag"]),
            magnitude_type=str(properties.get("magType") or ""),
            catalog="USGS",
            region=str(properties.get("place") or ""),
        )
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("slug", help="directory name under examples/")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the queries without fetching or writing anything",
    )
    args = parser.parse_args()
    for path in fetch(args.slug, args.dry_run):
        if args.dry_run:
            print(f"          would write {path.relative_to(ROOT)}")
        else:
            print(f"{path.stat().st_size:>10d}  {path.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
