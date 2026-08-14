"""Build and verify the pinned parity corpus.

The synthetic corpus is generated rather than checked in: the demo dataset alone
is 4 MB of float64 MiniSEED. What is checked in is ``manifest.json`` — the sha256
of every corpus file — so a corpus that silently changes shape (a different numpy
RNG stream, an edited generator) fails loudly instead of quietly moving the
numbers every parity run is measured against.

Determinism is the whole point. Only the ``demo`` dataset uses an RNG, and only
because it has to: it is byte-for-byte the dataset ``qt/make_demo.py`` writes and
``bridge._make_example_scene`` synthesizes, so the corpus really is "the demo both
frontends already show". Everything else is analytic and stable under any numpy.

The ``eifel`` dataset is the exception to generation: real recordings, checked in
under ``corpus/eifel`` as the bytes came from the datacenter, because no generator
can reproduce the mess a real deployment has — hundreds of stations, mixed rates,
epoch-ridden StationXML. The manifest covers it too, so a locally edited copy
fails the same way a drifted generator does.

Run directly to (re)build::

    python tests/parity/corpus/build.py [--force] [--update-manifest]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sys
from pathlib import Path
from typing import Any, Callable, Dict, List, Tuple

import numpy as np
from pyrocko import io, model, trace
from pyrocko.io import stationxml

HERE = Path(__file__).resolve().parent
DATA = HERE / "data"
MANIFEST = HERE / "manifest.json"

# Every dataset starts here, so no scenario ever needs a wall clock. 2023-11-14
# 22:13:20 UTC — the same epoch qt/make_demo.py uses.
T0 = 1_700_000_000.0

# StationXML carries a creation timestamp that would otherwise be `time.time()`.
STATIONXML_CREATED = T0


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_demo(out: Path) -> List[Path]:
    """The synthetic demo: 6 stations x 3 components, 50 Hz, 600 s.

    Kept character-for-character in step with ``qt/make_demo.py`` and
    ``bridge._make_example_scene`` — this is that dataset, written to a file so
    both sides can load identical bytes.
    """
    deltat = 0.02
    n = 30_000
    stations = ["STA01", "STA02", "STA03", "STA04", "STA05", "STA06"]
    channels = ["HHZ", "HHN", "HHE"]
    t = np.arange(n) * deltat
    rng = np.random.default_rng(42)

    traces = []
    for si, sta in enumerate(stations):
        arrival = 180.0 + si * 18.0
        env = np.exp(-((t - arrival) ** 2) / (2.0 * 8.0**2))
        for cha in channels:
            phase = rng.uniform(0.0, 2.0 * np.pi)
            carrier = np.sin(2.0 * np.pi * 3.2 * (t - arrival) + phase) + 0.45 * np.sin(
                2.0 * np.pi * 6.7 * (t - arrival) + 0.3 * phase
            )
            coda = np.exp(-np.maximum(t - arrival, 0.0) / 26.0)
            coda[t < arrival] = 0.0
            burst_noise = rng.standard_normal(n)
            event = env * carrier + 0.22 * coda * burst_noise
            noise = rng.standard_normal(n)
            y = 0.22 * noise + event
            traces.append(
                trace.Trace(
                    network="XX",
                    station=sta,
                    location="",
                    channel=cha,
                    deltat=deltat,
                    tmin=T0,
                    ydata=y.astype(np.float64),
                )
            )

    path = out / "demo.mseed"
    io.save(traces, str(path))
    return [path]


def _analytic(t: np.ndarray, freq: float, phase: float) -> np.ndarray:
    """A trend plus two tones — enough structure that demeaning, filtering and
    scaling all have something to bite on, with no RNG in sight."""
    return (
        1.0 * np.sin(2.0 * np.pi * freq * t + phase)
        + 0.35 * np.sin(2.0 * np.pi * (freq * 3.7) * t + 0.5 * phase)
        + 0.004 * t
        + 2.5
    )


def _segment(
    nslc: Tuple[str, str, str, str],
    tmin: float,
    deltat: float,
    nsamples: int,
    freq: float,
    phase: float,
) -> trace.Trace:
    net, sta, loc, cha = nslc
    t = tmin - T0 + np.arange(nsamples) * deltat
    return trace.Trace(
        network=net,
        station=sta,
        location=loc,
        channel=cha,
        deltat=deltat,
        tmin=tmin,
        ydata=_analytic(t, freq, phase).astype(np.float64),
    )


def build_gaplap(out: Path) -> List[Path]:
    """Gaps and overlaps at every size the degap logic branches on.

    ``GAP1`` walks: a 2-sample gap (interpolated, both sides fill it), a
    500-sample gap (far past ``maxgap``, stays a gap), a 2-sample overlap
    (trimmed by both), and a 12-sample overlap — past Qt's ``maxlap=5`` but not
    past the web's ``maxlap=None``, which is exactly where the two implementations
    are allowed to disagree.

    ``GAP2`` displaces the sampling instead: its joins land a half sample off the
    grid, which sends ``degapper`` down its "cannot degap traces with displaced
    sampling" branch, where the ``maxlap`` disagreement decides whether the
    overlap is trimmed at all.
    """
    deltat = 0.01
    freq, phase = 2.0, 0.7

    def chain(sta: str, steps: List[float]) -> List[trace.Trace]:
        """One channel built from 1000-sample segments; ``steps`` is the sample
        distance from each segment's start to the next one's."""
        segs: List[trace.Trace] = []
        cursor = T0
        for index, step in enumerate(steps):
            segs.append(
                _segment(("XG", sta, "", "HHZ"), cursor, deltat, 1000, freq, phase + 0.1 * index)
            )
            cursor += step * deltat
        return segs

    aligned = chain(
        "GAP1",
        [
            1000 + 2,  # 2-sample gap: interpolated by both
            1000 + 500,  # 500-sample gap: far past maxgap, stays a gap
            1000 - 2,  # 2-sample overlap: trimmed by both
            1000 - 12,  # 12-sample overlap: past Qt's maxlap=5, not past the web's None
            0,
        ],
    )
    displaced = chain(
        "GAP2",
        [
            1000 + 2.5,  # off-grid gap
            1000 - 8.5,  # off-grid overlap, again straddling maxlap
            0,
        ],
    )

    path = out / "gaplap.mseed"
    # The writer refuses overlapping traces by default; overlaps are precisely
    # what this dataset exists to carry.
    io.save(aligned + displaced, str(path), check_overlaps=False)
    return [path]


_MULTI_STATIONS: List[Tuple[str, str, float, float, float, float]] = [
    # (net, sta, lat, lon, elevation_m, depth_m)
    ("XM", "AAA", 10.0, 20.0, 0.0, 0.0),
    ("XM", "BBB", 11.5, 20.5, 1200.0, 0.0),
    ("XM", "CCC", 9.0, 22.0, 250.0, 100.0),
    ("XM", "DDD", 12.0, 18.0, 3000.0, 0.0),
]

_MULTI_EVENT = dict(lat=10.5, lon=21.0, depth=15_000.0, magnitude=5.4, name="PARITY01")


def _multi_pyrocko_stations() -> List[model.Station]:
    stations = []
    for net, sta, lat, lon, elevation, depth in _MULTI_STATIONS:
        station = model.Station(
            network=net,
            station=sta,
            location="",
            lat=lat,
            lon=lon,
            elevation=elevation,
            depth=depth,
        )
        station.set_channels(
            [
                model.Channel("BHZ", azimuth=0.0, dip=-90.0),
                model.Channel("BHN", azimuth=0.0, dip=0.0),
                model.Channel("BHE", azimuth=90.0, dip=0.0),
            ]
        )
        stations.append(station)
    return stations


def _velocity_response() -> Any:
    """A plain 30 s velocity broadband: two zeros at the origin, one conjugate
    pole pair. Deconvolving it is a real inversion (not a scalar gain), which is
    what makes it worth putting in the corpus."""
    from pyrocko import response as presponse

    corner = 2.0 * math.pi / 30.0
    poles = [
        complex(-corner / math.sqrt(2.0), corner / math.sqrt(2.0)),
        complex(-corner / math.sqrt(2.0), -corner / math.sqrt(2.0)),
    ]
    return presponse.PoleZeroResponse(zeros=[0j, 0j], poles=poles, constant=1.5e9)


def build_multi(out: Path) -> List[Path]:
    """4 stations x 3 components at 20 Hz with coordinates, an event and real
    poles-zeros responses: distance/azimuth sorting, rotation and deconvolution."""
    deltat = 0.05
    n = 6_000  # 300 s
    t = np.arange(n) * deltat

    traces = []
    for si, (net, sta, *_rest) in enumerate(_MULTI_STATIONS):
        arrival = 60.0 + si * 21.0
        env = np.exp(-((t - arrival) ** 2) / (2.0 * 6.0**2))
        for ci, cha in enumerate(("BHZ", "BHN", "BHE")):
            phase = 0.4 * si + 1.1 * ci
            # Counts, as a digitizer would report them, so deconvolution has a
            # gain worth removing.
            y = 4.0e6 * env * np.sin(2.0 * np.pi * 1.7 * (t - arrival) + phase)
            y += 1.5e5 * np.sin(2.0 * np.pi * 0.11 * t + phase)
            traces.append(
                trace.Trace(
                    network=net,
                    station=sta,
                    location="",
                    channel=cha,
                    deltat=deltat,
                    tmin=T0,
                    ydata=y,
                )
            )

    written = [out / "multi.mseed"]
    io.save(traces, str(written[0]))

    stations = _multi_pyrocko_stations()
    written.append(out / "multi.stations")
    model.dump_stations(stations, str(written[-1]))

    event = model.Event(time=T0 + 30.0, **_MULTI_EVENT)
    written.append(out / "multi.events")
    model.dump_events([event], str(written[-1]))

    sx = stationxml.FDSNStationXML.from_pyrocko_stations(stations)
    # from_pyrocko_stations stamps `time.time()`; a corpus file may not carry a
    # wall clock or its hash changes on every rebuild.
    sx.created = STATIONXML_CREATED
    resp = stationxml.Response.from_pyrocko_pz_response(
        _velocity_response(), input_unit="M/S", output_unit="COUNTS"
    )
    for network in sx.network_list:
        for station in network.station_list:
            for channel in station.channel_list:
                channel.sample_rate = stationxml.SampleRate(value=1.0 / deltat)
                channel.response = resp
    written.append(out / "multi.stationxml")
    sx.dump_xml(filename=str(written[-1]))
    return written


def build_oddrate(out: Path) -> List[Path]:
    """Three sampling rates in one dataset, two of them awkward.

    5 Hz puts Nyquist at 2.5 Hz, so a 3 Hz lowpass has to be dropped rather than
    applied; 7 Hz has a period no float represents exactly; 50 Hz is the control.
    A mixed-rate pile is also what makes Qt's ``min_deltat_allow`` selector
    visible — it drops traces that are too coarse for the window.
    """
    specs = [
        ("ODD1", 0.2, 1_500, 0.9),  # 5 Hz, 300 s
        ("ODD2", 1.0 / 7.0, 2_100, 1.3),  # 7 Hz, 300 s
        ("ODD3", 0.02, 15_000, 3.3),  # 50 Hz, 300 s
    ]
    traces = [
        _segment(("XR", sta, "", "BHZ"), T0, deltat, nsamples, freq, 0.25)
        for sta, deltat, nsamples, freq in specs
    ]
    path = out / "oddrate.mseed"
    io.save(traces, str(path))
    return [path]


def build_multiloc(out: Path) -> List[Path]:
    """Co-located sensors: stations recording the same ground motion twice.

    The case snuffler's "Grouped by Location" subsorts exist for — a surface and a
    borehole instrument at one site, or a broadband beside a strong-motion sensor —
    and the one every other dataset here misses, because they all carry a single
    location code and grouping over it changes nothing.

    Two details are load-bearing. The second sensor of each pair is an order of
    magnitude quieter, so a row that scaled its traces together rather than each on
    its own would flatten one of them; and CO03 has a single location, so the same
    stack exercises a grouped row that groups nothing.
    """
    deltat = 0.05
    nsamples = 6_000  # 300 s at 20 Hz
    specs = [
        # station, location, amplitude scale, frequency
        ("CO01", "00", 1.0, 1.3),
        ("CO01", "10", 0.08, 1.3),
        ("CO02", "00", 1.0, 2.1),
        ("CO02", "20", 0.05, 2.1),
        ("CO03", "", 1.0, 0.7),
    ]
    traces = []
    for sta, loc, amplitude, freq in specs:
        for index, cha in enumerate(("BHE", "BHN", "BHZ")):
            segment = _segment(("XL", sta, loc, cha), T0, deltat, nsamples, freq, 0.6 * index)
            segment.ydata = segment.ydata * amplitude
            traces.append(segment)

    path = out / "multiloc.mseed"
    io.save(traces, str(path))
    return [path]


BUILDERS: Dict[str, Callable[[Path], List[Path]]] = {
    "demo": build_demo,
    "gaplap": build_gaplap,
    "multi": build_multi,
    "multiloc": build_multiloc,
    "oddrate": build_oddrate,
}

# Checked-in datasets, as corpus-relative paths. These are not generated and cannot
# be: they are real recordings, and their value is exactly that no generator made
# them. Git carries the bytes; the manifest still pins them so a locally modified
# copy fails the run instead of quietly moving the numbers.
CHECKED_IN: Tuple[str, ...] = (
    "eifel/eifel.mseed",
    "eifel/catalog.yaml",
    "eifel/meta/5X.xml",
    "eifel/meta/6E.xml",
    "eifel/meta/6X.xml",
    "eifel/meta/7X.xml",
    "eifel/meta/8X.xml",
    "eifel/meta/BQ.BGG.xml",
    "eifel/meta/BQ.DREG.xml",
    "eifel/meta/BQ.HILG.xml",
    "eifel/meta/BQ.KLL.xml",
    "eifel/meta/BQ.KOE.xml",
    "eifel/meta/BQ.NAST.xml",
)


def build(force: bool = False) -> Dict[str, str]:
    """Write every generated dataset and return the full ``{path: sha256}`` manifest.

    Generated files are keyed by bare filename (they all live flat in ``data/``),
    checked-in files by their corpus-relative path.
    """
    DATA.mkdir(parents=True, exist_ok=True)
    written: List[Path] = []
    for name, builder in BUILDERS.items():
        marker = DATA / f"{name}.mseed"
        if marker.exists() and not force:
            written.extend(sorted(DATA.glob(f"{name}.*")))
            continue
        written.extend(builder(DATA))
    manifest = {path.name: _sha256(path) for path in sorted(set(written))}
    for rel in CHECKED_IN:
        target = HERE / rel
        if not target.exists():
            raise SystemExit(
                f"checked-in corpus file missing: tests/parity/corpus/{rel}\n"
                "It is not generated — restore it from git."
            )
        manifest[rel] = _sha256(target)
    return manifest


def verify(update: bool = False) -> Dict[str, str]:
    """Ensure the corpus on disk is the pinned one; return its manifest."""
    manifest = build()
    if update or not MANIFEST.exists():
        MANIFEST.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        return manifest

    pinned = json.loads(MANIFEST.read_text())
    if pinned != manifest:
        drifted = sorted(
            name for name in set(pinned) | set(manifest) if pinned.get(name) != manifest.get(name)
        )
        raise SystemExit(
            "parity corpus drifted from tests/parity/corpus/manifest.json: "
            + ", ".join(drifted)
            + "\nThe corpus is the fixed input every parity number is measured "
            "against, so this is never ignorable. Either a generator changed "
            "(intended: rerun with --update-manifest and review the parity "
            "deltas in the same commit), the numpy RNG stream moved under the "
            "demo dataset (unintended: pin numpy), or a checked-in dataset was "
            "edited locally (restore it from git)."
        )
    return manifest


def path(name: str) -> Path:
    """Absolute path to one corpus file, building the corpus if it is missing.

    Checked-in files resolve against the corpus directory itself, generated ones
    against ``data/`` — the same corpus-relative names ``scenario.DATASETS`` uses.
    """
    checked_in = HERE / name
    if checked_in.exists():
        return checked_in
    target = DATA / name
    if not target.exists():
        build()
    if not target.exists():
        raise FileNotFoundError(f"no such corpus file: {name}")
    return target


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--force", action="store_true", help="rebuild existing datasets")
    parser.add_argument("--update-manifest", action="store_true", help="bless the current corpus")
    args = parser.parse_args()
    if args.force:
        build(force=True)
    manifest = verify(update=args.update_manifest)
    for name, digest in sorted(manifest.items()):
        print(f"{digest[:16]}  {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
