"""The one scenario format both parity tiers read.

A scenario is a *side-neutral* description of what the viewer is showing: which
corpus dataset is loaded, which time window is on screen, and the value of every
knob that changes the numbers. The two drivers translate it — ``qt_oracle`` into
menu actions and attributes on a live ``PileViewerMain``, ``web_oracle`` into a
``ViewParams`` message — and neither is allowed to invent state the scenario did
not name.

That is why :data:`DEFAULTS` spells out every field instead of letting each side
fall back to its own defaults: Qt's viewer boots with ``Allow Downsampling`` and
``Allow Degapping`` on while a bare ``ViewParams`` request leaves every flag off,
and a comparison that inherited each seam's own defaults would be measuring the
defaults rather than the code. (The web *app* sets its boot state to snuffler's
config defaults on purpose — ``state.rs`` — which tier 3 verifies; the seams the
oracles drive do not.)

Parsing is strict for the same reason. A misspelt key would not break anything;
it would silently measure the defaults instead of the scenario, and the suite
would go on passing. Every unknown key is therefore an error.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

SCENARIO_DIR = Path(__file__).resolve().parent / "scenarios"

# Every knob a scenario may set, with the value used when it does not.
#
# `visible_length` is Qt's `PileViewerMain.visible_length`, which despite the name
# `prepare_cutout2` uses as a target *sample count* (`nmax`) when it decides which
# sampling rates are worth fetching. 20000 is the upstream default.
DEFAULTS: Dict[str, Any] = {
    "highpass": None,
    "lowpass": None,
    "demean": False,
    "degap": False,
    "allow_downsampling": False,
    "fft_filtering": False,
    "bandpass_lphp": False,
    "remove_response": False,
    "liberal_fetch": False,
    "rotate": 0.0,
    "gain": 1.0,
    "sort": "name",
    "subsort": "nslc",
    "scaling_group": "individual",
    "scaling_base": "minmax",
    "fix_scale_ranges": False,
    "show_scale_ranges": False,
    "show_scale_axes": False,
    "distances_3d": False,
    "visible_length": 20_000.0,
    "mode": "wiggle",
    "npx": 1_200,
}


@dataclass(frozen=True)
class Dataset:
    """One corpus dataset: the waveform file and the side-files it carries.

    Paths are relative to the corpus directory, resolved through
    ``corpus.build.path`` — generated datasets live under ``corpus/data``, the
    checked-in real-world ones next to it. ``responses`` is a tuple because a real
    deployment hands StationXML over one file per network or station, and both
    frontends accept them as a batch.
    """

    waveforms: str
    stations: Optional[str] = None
    events: Optional[str] = None
    responses: Tuple[str, ...] = ()

    def extras(self) -> Tuple[str, ...]:
        """The names a scenario may list under ``load``."""
        return tuple(name for name in ("stations", "events", "responses") if getattr(self, name))


DATASETS: Dict[str, Dataset] = {
    "demo": Dataset(waveforms="demo.mseed"),
    "gaplap": Dataset(waveforms="gaplap.mseed"),
    "multi": Dataset(
        waveforms="multi.mseed",
        stations="multi.stations",
        events="multi.events",
        responses=("multi.stationxml",),
    ),
    "multiloc": Dataset(waveforms="multiloc.mseed"),
    "oddrate": Dataset(waveforms="oddrate.mseed"),
    # Real recordings of a small Eifel event: 354 stations across six networks at
    # three sampling rates, a yaml event catalog, and per-network StationXML with
    # full responses. Checked in rather than generated — realness is the point.
    "eifel": Dataset(
        waveforms="eifel/eifel.mseed",
        events="eifel/catalog.yaml",
        responses=(
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
        ),
    ),
    # The GSN recording the 2015-05-30 Bonin Islands Mww 7.8 at 664 km depth: six
    # stations that carry the same channel at two or three location codes, which is
    # the one shape "Grouped by Location" can be observed on. Real co-location brings
    # what the synthetic `multiloc` cannot — 20 Hz beside 40 Hz on one row, gains an
    # order of magnitude apart, sensor depths that differ where the coordinates do
    # not. Checked in rather than generated, for the same reason `eifel` is.
    "bonin-2015-05-30-m7.8-multiloc": Dataset(
        waveforms="bonin-2015-05-30-m7.8-multiloc/bonin-2015-05-30-m7.8-multiloc.mseed",
        events="bonin-2015-05-30-m7.8-multiloc/catalog.yaml",
        responses=(
            "bonin-2015-05-30-m7.8-multiloc/meta/II.xml",
            "bonin-2015-05-30-m7.8-multiloc/meta/IU.xml",
        ),
    ),
}


@dataclass(frozen=True)
class CorpusFiles:
    """One dataset's files resolved to absolute paths, ready for either oracle."""

    waveforms: Path
    stations: Optional[Path] = None
    events: Optional[Path] = None
    responses: Tuple[Path, ...] = ()


@dataclass(frozen=True)
class Compare:
    """Per-scenario comparison policy.

    ``edge_trim_s`` exists because the two pipelines choose different amounts of
    context to filter over: Qt filters a ``tpad``-padded chop of the requested
    window, the web filters the whole trace once and slices. Inside the window the
    difference is a filter transient at the edges and nothing else, so a scenario
    that filters a sub-window says how much of each edge is transient rather than
    inflating the tolerance until the middle passes too.
    """

    rtol: float = 1e-6
    atol: float = 1e-9
    edge_trim_s: float = 0.0
    scale_rtol: float = 1e-6
    # Named entries in known_gaps.toml [[numeric]] that this scenario is expected
    # to trip. A scenario that trips none of them, or trips one that is not listed,
    # fails.
    expect_divergence: List[str] = field(default_factory=list)
    reason: str = ""


@dataclass(frozen=True)
class Step:
    """One semantic action, in the vocabulary both tier-3 drivers implement.

    ``menu`` targets are written in Qt's wording and resolved to the web's through
    the same ``[[alias]]`` table tier 2 uses — one map between the two vocabularies,
    not two.
    """

    action: str
    target: str = ""
    highpass: Optional[float] = None
    lowpass: Optional[float] = None
    # Pick position as a fraction of the waveform area, so the two layouts can each
    # turn it into their own pixels.
    x: float = 0.5
    y: float = 0.5

    ACTIONS = ("menu", "key", "filter", "pick")


@dataclass(frozen=True)
class Scenario:
    name: str
    title: str
    dataset: str
    start_s: float
    length_s: Optional[float]
    state: Dict[str, Any]
    compare: Compare
    load: List[str]
    tiers: List[int]
    source: Path
    steps: List[Step]
    # Which menus the tier-3 snapshot reads checkable state out of. Reading them all
    # after every step means opening eight menus in a browser per step; the ones a
    # scenario actually touches are the ones worth watching.
    watch_menus: List[str]

    def window(self, data_tmin: float, data_tmax: float) -> tuple[float, float]:
        """Absolute view window, always derived from the dataset's own extent —
        never from a clock."""
        tmin = data_tmin + self.start_s
        if self.length_s is None:
            return data_tmin, data_tmax
        return tmin, tmin + self.length_s


# What a scenario file may contain, per table. Anything else is a typo, and a typo
# here would not fail — it would quietly measure something other than what the
# file claims to measure.
_TOP_LEVEL_KEYS = frozenset(
    {
        "name",
        "title",
        "dataset",
        "tiers",
        "load",
        "window",
        "state",
        "compare",
        "step",
        "watch_menus",
    }
)
_WINDOW_KEYS = frozenset({"start_s", "length_s"})
_STEP_KEYS = frozenset(Step.__dataclass_fields__)


def _load_one(path: Path) -> Scenario:
    raw = tomllib.loads(path.read_text())

    unknown_top = set(raw) - _TOP_LEVEL_KEYS
    if unknown_top:
        raise ValueError(f"{path.name}: unknown keys {sorted(unknown_top)}")

    name = raw.get("name") or path.stem
    dataset = raw["dataset"]
    if dataset not in DATASETS:
        raise ValueError(f"{path.name}: unknown dataset {dataset!r}")

    unknown = set(raw.get("state", {})) - set(DEFAULTS)
    if unknown:
        raise ValueError(f"{path.name}: unknown state keys {sorted(unknown)}")

    state = dict(DEFAULTS)
    state.update(raw.get("state", {}))

    load = list(raw.get("load", []))
    available = DATASETS[dataset].extras()
    missing = [item for item in load if item not in available]
    if missing:
        raise ValueError(f"{path.name}: dataset {dataset!r} has no {missing}")

    window = raw.get("window", {})
    unknown_window = set(window) - _WINDOW_KEYS
    if unknown_window:
        raise ValueError(f"{path.name}: unknown window keys {sorted(unknown_window)}")
    length = window.get("length_s")
    if length is None and "start_s" in window:
        # `window()` returns the whole extent when no length is given, so a lone
        # start would be silently ignored.
        raise ValueError(f"{path.name}: window.start_s without window.length_s does nothing")

    compare_raw = raw.get("compare", {})
    unknown_compare = set(compare_raw) - set(Compare.__dataclass_fields__)
    if unknown_compare:
        raise ValueError(f"{path.name}: unknown compare keys {sorted(unknown_compare)}")

    tiers = list(raw.get("tiers", [1]))
    unknown_tiers = set(tiers) - {1, 3}
    if unknown_tiers:
        raise ValueError(f"{path.name}: unknown tiers {sorted(unknown_tiers)}")

    steps = []
    for index, raw_step in enumerate(raw.get("step", [])):
        unknown_step = set(raw_step) - _STEP_KEYS
        if unknown_step:
            raise ValueError(f"{path.name}: step {index} has unknown keys {sorted(unknown_step)}")
        action = raw_step.get("action")
        if action not in Step.ACTIONS:
            raise ValueError(f"{path.name}: step {index} has unknown action {action!r}")
        steps.append(Step(**raw_step))
    if steps and 3 not in tiers:
        # Only the tier-3 drivers execute steps, so steps on a tier-1-only scenario
        # would sit there looking covered while nothing ever runs them.
        raise ValueError(f"{path.name}: has steps but does not run in tier 3")

    return Scenario(
        name=name,
        title=raw.get("title", name),
        dataset=dataset,
        start_s=float(window.get("start_s", 0.0)),
        length_s=None if length is None else float(length),
        state=state,
        compare=Compare(**compare_raw),
        load=load,
        tiers=tiers,
        source=path,
        steps=steps,
        watch_menus=list(raw.get("watch_menus", ["View", "Options"])),
    )


def load_all(tier: Optional[int] = None, only: Optional[List[str]] = None) -> List[Scenario]:
    scenarios = [_load_one(path) for path in sorted(SCENARIO_DIR.glob("*.toml"))]
    if tier is not None:
        scenarios = [s for s in scenarios if tier in s.tiers]
    if only:
        wanted = set(only)
        unknown = wanted - {s.name for s in scenarios}
        if unknown:
            raise SystemExit(f"no such scenario: {', '.join(sorted(unknown))}")
        scenarios = [s for s in scenarios if s.name in wanted]
    return scenarios
