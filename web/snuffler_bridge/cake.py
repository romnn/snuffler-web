"""The Cake Phase snuffling: theoretical arrivals through a 1D earth model."""

from __future__ import annotations

import os
from typing import Any, List, Tuple

from snuffler.bridge.v1 import snuffling_pb2

from . import fetch, gf_stores, marker_budget, pyrocko_modules, sorting, state

# Directory holding the cake earth models, fetched from the build assets on first
# use: the minimal wheel has no data dir, so cake.builtin_models() is empty and
# models are loaded by path.
EARTH_MODEL_DIR = "/earthmodels"

# Builtin cake models served under earthmodels/<stem>.nd, matching the Cake Phase
# dropdown.
_BUILTIN_MODELS = [
    "ak135-f-average-no-ocean.f",
    "ak135-f-average-no-ocean.l",
    "ak135-f-average-no-ocean.m",
    "ak135-f-average.f",
    "ak135-f-average.l",
    "ak135-f-average.m",
    "ak135-f-average.vf",
    "ak135-f-continental.f",
    "ak135-f-continental.l",
    "ak135-f-continental.m",
    "prem-no-ocean.f",
    "prem-no-ocean.l",
    "prem-no-ocean.m",
    "prem.f",
    "prem.l",
    "prem.m",
]

_earth_models_staged = False


async def stage_earth_models() -> None:
    """Fetch the builtin cake models into the Pyodide FS once, before a cake run uses one.

    A model that fails to fetch is skipped, so it is just not selectable.
    """
    global _earth_models_staged
    if _earth_models_staged:
        return
    os.makedirs(EARTH_MODEL_DIR, exist_ok=True)
    for stem in _BUILTIN_MODELS:
        path = "%s/%s.nd" % (EARTH_MODEL_DIR, stem)
        if os.path.exists(path):
            continue
        text = await fetch.fetch_text_or_empty(fetch.asset_url("earthmodels/%s.nd" % stem))
        if text:
            with open(path, "w") as f:
                f.write(text)
    _earth_models_staged = True


def model_path(name: str) -> str:
    """Filesystem path of a staged cake model from its dropdown name."""
    return "%s/%s.nd" % (EARTH_MODEL_DIR, name)


def load_model(name: str) -> Any:
    """Load the cake earth model for a Cake Phase model selection.

    A ``GF Store: <id>`` selection uses the mounted store's embedded 1D model
    (``config.earthmodel_1d``, read without ``store_ext``); any other name is a
    staged builtin ``.nd`` model loaded by path.
    """
    if name.startswith(gf_stores.LABEL_PREFIX):
        sid = name[len(gf_stores.LABEL_PREFIX) :]
        store_dir = state.gf_stores.get(sid)
        if store_dir is None:
            raise ValueError("GF store %r is not mounted" % sid)
        gfstore, _gfmeta = pyrocko_modules.gf_store_modules()
        mod = gfstore.Store(store_dir).config.earthmodel_1d
        if mod is None:
            raise ValueError("GF store %r has no 1D earth model" % sid)
        return mod
    from pyrocko import cake

    return cake.load_model(model_path(name))


def settings(
    request: snuffling_pb2.SnufflingRequestParams,
) -> Tuple[str, List[str], float, bool, str]:
    """Extract the Cake Phase knobs from the snuffling params."""
    c = request.snufflings.cake
    model = c.model or "ak135-f-continental.m"
    phases = [str(x) for x in (list(c.phases) or ["~P"])]
    tshift = float(c.time_shift_s)
    use_station_depth = bool(c.use_station_depth)
    mode = c.mode or "run"
    return model, phases, tshift, use_station_depth, mode


def phase_definitions(names: List[str]) -> Tuple[List[Any], List[str]]:
    """Turn the enabled phase names into cake ``PhaseDef`` objects.

    A leading ``~`` selects a raw definition (``cake.PhaseDef``); any other name
    is expanded via the classic-phase table (``cake.PhaseDef.classic``), exactly
    as the Qt Cake Phase snuffling does.
    The friendly name is stashed on each PhaseDef so ``ray.given_phase().name``
    reports it back.
    Parse failures are collected, not raised, so one bad custom phase does not
    sink the whole run.
    """
    from pyrocko import cake

    wanted = []
    errors: List[str] = []
    for name in names:
        try:
            if name.startswith("~"):
                phases = [cake.PhaseDef(name[1:])]
            else:
                phases = cake.PhaseDef.classic(name)
        except (cake.UnknownClassicPhase, cake.PhaseDefParseError) as exc:
            errors.append("%s (%s)" % (name, exc))
            continue
        for pha in phases:
            pha.name = name
        wanted.extend(phases)
    return wanted, errors


def ray_stations() -> List[Tuple[str, str, float, float]]:
    """``(net, sta, dist_deg, depth_m)`` for the stations cake gets rays at.

    Qt's scope (`get_active_event_and_stations`, snuffling.py:806): stations
    whose traces hold data within an hour of the event, one per distinct station
    entry, so a borehole and a surface sensor at one site are two.
    """
    if state.active_event is None:
        return []
    elat, elon, _edep, etime = state.active_event
    seen = set()
    out: List[Tuple[str, str, float, float]] = []
    for trace in state.traces:
        if trace.tmax < etime - 3600.0 or trace.tmin > etime + 3600.0:
            continue
        net, sta, loc, _cha = trace.nslc_id
        for key in ((net, sta, loc), (net, sta)):
            entry = state.stations.get(key)
            if entry is None:
                continue
            ident = (net, sta, entry)
            if ident in seen:
                continue
            seen.add(ident)
            slat, slon, _elev, depth = entry
            out.append((net, sta, sorting.distance_deg(elat, elon, slat, slon), depth))
    return out


def run(request: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Theoretical arrivals as phase markers (the Cake Phase 'Run' action), with
    a per-pick angle table."""
    model, phase_names, tshift, use_station_depth, _mode = settings(request)
    if state.active_event is None:
        return snuffling_pb2.SnufflingResult(
            show_window=True,
            text="No active event. Run a catalog search first (Data ▸ Catalog search).",
        )
    if not state.stations:
        return snuffling_pb2.SnufflingResult(
            show_window=True,
            text="No station coordinates. Fetch stations first (Data ▸ Download ▸ Fetch stations).",
        )

    _elat, _elon, edepth, etime = state.active_event
    mod = load_model(model)
    wanted, errors = phase_definitions(phase_names)

    result = snuffling_pb2.SnufflingResult(show_window=True, origin="cake")
    markers = marker_budget.MarkerAccumulator(request.marker_budget)
    lines = ["%-14s %-8s %12s %8s %8s" % ("station", "phase", "t-rel[s]", "inc[deg]", "tko[deg]")]
    nstations = 0
    for net, sta, dist_deg, depth in ray_stations():
        nstations += 1
        # `station.depth`, which is what Qt's "Use station depth" means
        # (cake_phase.py).
        rdepth = float(depth) if use_station_depth else 0.0
        # Qt raytraces the direct distance *and* the major arc (360° minus it,
        # cake_phase.py:146), which is where PKP/SKS-type late arrivals live.
        rays = mod.arrivals(
            phases=wanted,
            distances=[dist_deg, 360.0 - dist_deg],
            zstart=edepth,
            zstop=rdepth,
        )
        for ray in rays:
            name = ray.given_phase().name
            tt = float(ray.t) + tshift
            inc = float(ray.incidence_angle())
            tko = float(ray.takeoff_angle())
            marker = snuffling_pb2.SnufMarker()
            marker.tmin = etime + tt
            marker.tmax = etime + tt
            # Qt's Cake Phase conventions (cake_phase.py:163): a fixed kind 2,
            # `('*', '*')` for "any location, any channel" — an empty location
            # means "the blank location" to pyrocko and matches nothing else —
            # the angles on the marker itself, and the event association.
            marker.kind = 2
            marker.phase = name
            marker.incidence_angle = inc
            marker.takeoff_angle = tko
            if state.active_event_hash:
                marker.event_hash = state.active_event_hash
            if state.active_event is not None:
                marker.event_time = state.active_event[3]
            # The table row rides as the accumulator payload so the text stays
            # bounded by — and in agreement with — the retained picks instead of
            # growing with the total detection count.
            markers.add(marker, [(net, sta, "*", "*")], inc, payload=(net, sta, name, tt, inc, tko))

    for _marker, payload in markers.retained_candidates():
        net, sta, name, tt, inc, tko = payload
        lines.append("%-14s %-8s %12.3f %8.1f %8.1f" % ("%s.%s" % (net, sta), name, tt, inc, tko))
    npicks = markers.total
    dropped = npicks - (len(lines) - 1)
    if dropped > 0:
        lines.append("… %d more arrival(s) beyond the marker budget not shown" % dropped)
    header = "Cake arrivals (%s): %d pick(s) at %d station(s)." % (model, npicks, nstations)
    if errors:
        header += "\nSkipped phases: " + ", ".join(errors)
    result.text = header + "\n\n" + "\n".join(lines) if npicks else header
    return markers.finish(result)


def add_model(file_name: str, content: bytes) -> str:
    """Stage a user's ``.nd`` earth model and return its dropdown name."""
    name = file_name[:-3] if file_name.lower().endswith(".nd") else file_name
    name = name or "model"
    os.makedirs(EARTH_MODEL_DIR, exist_ok=True)
    with open(model_path(name), "wb") as f:
        f.write(content)
    return name
