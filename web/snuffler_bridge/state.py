"""The bridge's session state: the loaded data, viewer settings and caches.

Every other module reads and writes these globals as attributes of this module
(``state.traces``), never through a ``from state import`` alias, because
`install_dataset_state` and the hot reload replace whole containers.
`RELOAD_KEEP` names what survives a Shift+R code reload.
"""

from __future__ import annotations

import copy
from collections import OrderedDict
from typing import Any, Dict, Iterable, List, Mapping, NamedTuple, Optional, Tuple

import numpy as np
from pyrocko import trace as ptrace
from snuffler_web_proto import data_pb2, scene_pb2

from . import marker_budget
from .nslc import NslcId

# The loaded traces: snuffler's "pile".
traces: List[ptrace.Trace] = []

# Traces each on-disk source (File System Access handle id) contributed, so
# "reload modified files" can swap just that source's traces.
source_traces: "OrderedDict[str, List[ptrace.Trace]]" = OrderedDict()
# Content digest of each re-readable source, so a reload keeps duplicate
# detection coherent when a file changes.
source_digests: Dict[str, str] = {}
# Parsed traces of sources hidden because another source owns the same content.
# Keeping them parsed lets such a source take over without another filesystem
# notification when the visible owner changes.
suppressed_source_traces: Dict[str, List[ptrace.Trace]] = {}
# Owners of each content digest in the pile.
# A source can keep its last digest while owning no traces after converging on
# another source, so duplicate detection counts owners, not source ids.
digest_owners: Dict[str, "set[str]"] = {}

# Traces each snuffling added, keyed by its origin tag, so a rerun replaces its
# own previous output (Qt's `cleanup()` ticket contract) and nothing else.
derived_traces: Dict[str, List[ptrace.Trace]] = {}
# Event-registry objects each snuffling origin added.
# Cleanup matches them by identity, so it never deletes a newer event that
# reused an owned hash through a catalog load, marker load or frontend edit.
derived_events: Dict[str, Dict[str, Any]] = {}

# True while the pile holds only the boot demo: the synthetic example, or a
# checksummed example bundle nobody asked to keep.
# The first real data replaces it instead of appending, because Qt opens empty
# and never appends onto a pre-seeded pile.
pile_is_example = False

# The ':' command line's two independent trace filters, as in snuffler.
# Both match fnmatch globs over "net.sta.loc.cha"; data is hidden, never dropped.
# The blacklist `hide`/`unhide` maintain.
hidden_patterns: List[str] = []
# The quick filter the `n|s|l|c` commands set; when not None, only traces
# matching one of its globs are shown.
quick_filter_patterns: Optional[List[str]] = None

# The ':scaling' command's per-pattern amplitude overrides (snuffler's scaling
# hooks), keyed by n.s.l.c pattern.
# The '_' key is the rule without a pattern.
# Each value is (vmin, vmax), where None leaves that bound automatic.
scaling_hooks: "OrderedDict[str, Tuple[Optional[float], Optional[float]]]" = OrderedDict()

# Frozen scale ranges ('Fix Scale Ranges').
# Snuffler remembers them with no notion of what produced them, so they survive
# filter, sort and even pile changes (pile_viewer.py:3281), and so do these.
fixed_data_ranges: Dict[Any, Tuple[float, float]] = {}

# Station coordinates as (lat, lon, elevation, depth), for distance and azimuth
# sorting.
# Keyed as snuffler keys them (`station_keys`, pile_viewer.py:1375): every
# station is filed under both `(net, sta, loc)` and `(net, sta)`, and looked up
# in that order.
# Co-located instruments can differ in position (IU.ANMO's two broadbands hang
# 145 m and 90 m down the same borehole), so the location code matters.
# The two-element key is snuffler's fallback for a trace whose location code no
# station file mentions, and, as there, the last station added wins it.
stations: Dict[Tuple[str, ...], Tuple[float, float, float, float]] = {}

# The active event as (lat, lon, depth_m, time), the reference for distance
# sorting, cake and the seismosizer.
active_event: Optional[Tuple[float, float, float, float]] = None
# Registry key of the active event, so machine-made picks (cake, F-keys through
# the frontend) can be associated with it (snuffler `set_event`).
active_event_hash: str = ""
# Moment tensor of the active event, if it has one: the reference for the
# marker table's Kagan-angle column.
active_moment_tensor: Any = None
# Every loaded event, keyed by its marker event_hash, so the user can pick any
# of them as the active event.
event_registry: "OrderedDict[str, Any]" = OrderedDict()

# Instrument responses for Deconvolution: the parsed FDSNStationXML documents
# in load order.
# A list rather than one merged document, because that is how snuffler keeps
# them (`station_xmls`, pile_viewer.py:4470): lookups take the first document
# that resolves, so a channel described twice still restitutes instead of
# raising on the ambiguity a merge would create.
station_xmls: List[Any] = []
# Channels already reported as having no usable response, so the warning fires
# once per response generation rather than once per paint.
warned_no_response: "set[NslcId]" = set()

# Mounted Green's-function stores: dropdown id -> directory in the Pyodide FS.
gf_stores: Dict[str, str] = {}
# The parsed plate boundaries of the map, fetched once.
cached_plate_boundaries: Optional[List[data_pb2.PlateBoundary]] = None
# Resolved SeisBench ONNX model URLs, keyed by "Model/weights/base".
# The ONNX Runtime session itself is cached in the JS worker, where WebGPU
# lives; Python caches only the manifest lookup and URL resolution.
seisbench_model_urls: Dict[str, str] = {}
# Numbers the files staged into the Pyodide FS, so no two stagings collide.
stage_counter = 0

# Bumped on every change to the traces; caches built from them key on it.
data_generation = 0
# Bumped when loaded responses change; the transfer-function cache keys on it.
response_generation = 0

# The processed-cutout cache, as snuffler's: one slot holding the last
# `prepare_cutout2` state vector and its processed traces, reused whenever the
# requested window is contained in the cached one and the rest of the vector
# matches (pile_viewer.py:3640).
cutout_key: Optional[Tuple[Any, ...]] = None
cutout_traces: Optional[List[ptrace.Trace]] = None
# The notices the cached cutout's processing produced, re-emitted on every cache
# hit because the drawn data still carries the failed filter or restitution.
cutout_notices: List[str] = []

# snuffler's `tf_cache`: the combined FFT-filtering transfer function per
# (deltat, nfreqs, nslc-or-None).
# Also keyed on the corners and the response generation so it can never go
# stale, where Qt clears it from the filter-change slots instead.
transfer_function_cache: Dict[Tuple[Any, ...], np.ndarray] = {}

# Snuffler's `trace_styles` (`determine_box_styles`, pile_viewer.py:2947): the
# box tint index per (full_id, deltat), recomputed when the pile changes.
box_styles: Dict[Tuple[Any, ...], int] = {}
box_styles_generation: Optional[int] = None

# Processing failures collected while building one view, delivered on its meta.
view_notices: List[str] = []

# The real `pyrocko.pile.Pile` the view chops through, rebuilt lazily when the
# traces change, so `pile.chopper` — the exact code Qt calls — does the
# chopping, degapping and rate selection.
cached_pile: Optional[Any] = None
cached_pile_generation: Optional[int] = None

# The session state a Shift+R code reload carries over: everything the user
# loaded, derived or configured, plus the generation counters that keep the
# frontend's caches coherent.
# Pure caches (cutouts, transfer functions, box styles, waterfall LUTs) are
# absent because they rebuild from the restored generations.
# tests/parity/test_reload_keep.py catches a name here that stops existing.
RELOAD_KEEP = (
    "traces",
    "source_traces",
    "source_digests",
    "suppressed_source_traces",
    "derived_traces",
    "derived_events",
    "pile_is_example",
    "digest_owners",
    "hidden_patterns",
    "quick_filter_patterns",
    "scaling_hooks",
    "data_generation",
    "response_generation",
    "fixed_data_ranges",
    "stations",
    "active_event",
    "active_event_hash",
    "active_moment_tensor",
    "event_registry",
    "station_xmls",
    "gf_stores",
    "cached_plate_boundaries",
    "seisbench_model_urls",
    "stage_counter",
    # The session store carries the generation Rust fences view replies with; a
    # reload that reset it to zero would leave every later view stale-rejected.
    "session_store",
)


def reload_snapshot() -> Dict[str, Any]:
    """Detach the session state a code reload carries over from the old module.

    The names are popped, not read: ``importlib.reload`` reuses the module dict,
    so a name left behind would survive even when the new source no longer
    defines it.
    The caller pairs this with `reload_restore` in a ``finally``, so a failed
    reload cannot strand the detached state.
    A global renamed between the two versions resets to its fresh default, an
    accepted loss for a dev-loop tool."""
    return {name: globals().pop(name) for name in RELOAD_KEEP if name in globals()}


def reload_restore(saved: Dict[str, Any]) -> None:
    """Reinstall a `reload_snapshot` into the freshly reloaded module.

    Only names the new module's `RELOAD_KEEP` still claims are installed, so a
    retired name is not parked as a global nothing reads.
    The generation bump makes every cache rebuild against the restored pile."""
    globals().update({name: value for name, value in saved.items() if name in RELOAD_KEEP})
    mark_traces_changed()


def mark_traces_changed() -> None:
    """Bump the data generation after any change to the traces, dropping the view caches."""
    global data_generation
    data_generation += 1
    clear_view_cache()


def clear_view_cache() -> None:
    """Drop the processed cutout, the transfer functions and the cached pile."""
    global cutout_key, cutout_traces, cached_pile, cached_pile_generation
    cutout_key = None
    cutout_traces = None
    cutout_notices.clear()
    transfer_function_cache.clear()
    cached_pile = None
    cached_pile_generation = None


def invalidate_interrupted_view() -> None:
    """Forget everything a cancelled view build may have left half-written."""
    clear_view_cache()
    view_notices.clear()
    fixed_data_ranges.clear()


def add_view_notice(message: str) -> None:
    """Record a processing failure for the view being built.

    Delivered on `ViewMeta.notices` so the frontend can say what was drawn
    unprocessed, where snuffler lets these exceptions reach the user.
    Deduplicated per view.
    """
    if message not in view_notices:
        view_notices.append(message)


def pile() -> Any:
    """The loaded traces as a real `pyrocko.pile.Pile`, rebuilt when they change.

    The Trace objects are shared, not copied, which is safe because the chopper
    hands out chopped copies.
    """
    global cached_pile, cached_pile_generation
    if cached_pile is None or cached_pile_generation != data_generation:
        from pyrocko import pile as ppile

        p = ppile.Pile()
        if traces:
            p.add_file(ppile.MemTracesFile(None, list(traces)))
        cached_pile = p
        cached_pile_generation = data_generation
    return cached_pile


def set_traces(new_traces: List[ptrace.Trace], append: bool = False) -> None:
    """Replace the pile with `new_traces`, or append them to it."""
    global traces, pile_is_example
    # Real data replaces the boot example rather than joining it, so the user's
    # channels never mix with unmarked synthetic noise.
    if append and pile_is_example:
        append = False
    traces = (list(traces) + new_traces) if append else new_traces
    if not append:
        source_traces.clear()
        source_digests.clear()
        suppressed_source_traces.clear()
        digest_owners.clear()
        derived_traces.clear()
        # The replaced scene's events, stations and responses go with its
        # waveforms; user loads only ever append, so only the boot-example
        # handover above reaches here with user data.
        _clear_dataset_metadata()
    pile_is_example = False
    mark_traces_changed()


def replace_source(source_id: str, new_traces: List[ptrace.Trace]) -> None:
    """Swap the traces previously loaded from ``source_id`` for freshly loaded ones.

    The old traces are removed by identity, leaving every other source untouched,
    as pyrocko's ``reload_modified`` only touches changed files."""
    global traces
    old = source_traces.get(source_id)
    if old is None:
        raise ValueError("waveform source %r was not registered by an initial load" % source_id)
    if old:
        old_ids = {id(t) for t in old}
        traces = [t for t in traces if id(t) not in old_ids]
    traces = list(traces) + new_traces
    source_traces[source_id] = new_traces
    mark_traces_changed()


def replace_derived_traces(origin: str, new_traces: List[ptrace.Trace]) -> None:
    """Swap the traces a snuffling's previous run added for this run's.

    This is the trace half of Qt's `cleanup()` ticket contract
    (snuffling.py:1689), scoped by origin.
    Removal is by identity, so a user-loaded trace that merely shares a name
    survives, and file provenance is left alone."""
    global traces
    old = derived_traces.get(origin)
    if old:
        old_ids = {id(t) for t in old}
        traces = [t for t in traces if id(t) not in old_ids]
    traces = list(traces) + list(new_traces)
    derived_traces[origin] = list(new_traces)
    mark_traces_changed()


def _digest_owner(source_id: str) -> str:
    return "source:%s" % source_id if source_id else "anonymous"


def add_digest_owner(digest: str, source_id: str) -> None:
    """Record that `source_id` holds the content with this digest."""
    digest_owners.setdefault(digest, set()).add(_digest_owner(source_id))


def remove_digest_owner(digest: str, source_id: str) -> None:
    """Release `source_id`'s claim on a digest; the last release hands it to a suppressed source."""
    owners = digest_owners.get(digest)
    if owners is None:
        return
    owners.discard(_digest_owner(source_id))
    if not owners:
        del digest_owners[digest]
        _promote_suppressed_source(digest)


def _promote_suppressed_source(digest: str) -> None:
    for source_id in source_traces:
        if source_digests.get(source_id) != digest:
            continue
        traces = suppressed_source_traces.pop(source_id, None)
        if traces is None:
            continue
        replace_source(source_id, traces)
        add_digest_owner(digest, source_id)
        return


def _clear_dataset_metadata() -> None:
    """Drop the events, station coordinates and responses of a replaced scene.

    Any of them surviving into the next scene would sort it by stale coordinates
    or deconvolve an overlapping NSLC against the wrong response.
    Only scene replacements come through here, never a user append."""
    global response_generation
    _clear_event_context()
    stations.clear()
    warned_no_response.clear()
    if station_xmls:
        station_xmls.clear()
        # The transfer-function cache keys on the response generation, so the
        # bump retires every entry built from the dropped documents.
        response_generation += 1


def _clear_event_context() -> None:
    event_registry.clear()
    derived_events.clear()
    activate_event(None)


def is_trace_visible(trace: ptrace.Trace) -> bool:
    """Whether the ':' command-line filters leave this trace visible.

    Uses pyrocko's `match_nslc`, as snuffler's hide and quick filters do; this is
    the `trace_filter` snuffler passes into `prepare_cutout2`.
    """
    from pyrocko.util import match_nslc

    if quick_filter_patterns is not None and not match_nslc(quick_filter_patterns, trace.nslc_id):
        return False
    return not (hidden_patterns and match_nslc(hidden_patterns, trace.nslc_id))


def visible_traces() -> List[ptrace.Trace]:
    """The traces the ':' command-line filters leave visible."""
    return [trace for trace in traces if is_trace_visible(trace)]


def scene_meta() -> scene_pb2.SceneMeta:
    """The time span, track count and sampling range of the visible traces."""
    traces = list(visible_traces())
    return scene_pb2.SceneMeta(
        tmin=min((float(trace.tmin) for trace in traces), default=0.0),
        tmax=max((float(trace.tmax) for trace in traces), default=1.0),
        track_count=len(traces),
        deltat_min=min((float(trace.deltat) for trace in traces), default=0.0),
        deltat_max=max((float(trace.deltat) for trace in traces), default=0.0),
    )


def station_at(net: str, sta: str, loc: str) -> Optional[Tuple[float, float, float, float]]:
    """The station a trace belongs to, resolved as snuffler resolves it.

    ``station_attrib`` (pile_viewer.py:1379) tries ``(net, sta, loc)`` first and falls
    back to ``(net, sta)``, so a station file that names location codes places each
    instrument exactly and one that omits them still places every trace of a station.
    """
    return stations.get((net, sta, loc)) or stations.get((net, sta))


def add_station(
    net: str, sta: str, loc: str, lat: float, lon: float, elevation: float, depth: float
) -> None:
    """File a station under both of snuffler's keys (``add_stations``,
    pile_viewer.py:1406), so lookups can be specific and still always resolve."""
    entry = (float(lat), float(lon), float(elevation), float(depth))
    stations[(net, sta, loc)] = entry
    stations[(net, sta)] = entry


def stations_by_name() -> Dict[Tuple[str, str], Tuple[float, float, float, float]]:
    """One entry per ``(net, sta)``, for the consumers that enumerate stations rather
    than resolve a trace's own — the map, the distance table, cake, the seismosizer.

    Every station sits in ``stations`` under two keys, so iterating it directly
    would visit co-located instruments twice.
    These surfaces name a station without a location code and cannot show more
    than one of them anyway; the one that survives is the last one added, as in
    snuffler's own two-element key.
    """
    return {(key[0], key[1]): value for key, value in stations.items() if len(key) == 2}


def register_event(event_hash: str, event: Any) -> bool:
    """File an event in the registry, evicting the oldest inactive one when it is full.

    False when nothing could be evicted.
    """
    if event_hash not in event_registry and len(event_registry) >= marker_budget.MAX_MARKER_BUDGET:
        removable = next((key for key in event_registry if key != active_event_hash), None)
        if removable is None:
            return False
        event_registry.pop(removable, None)
    event_registry[event_hash] = event
    event_registry.move_to_end(event_hash)
    return True


def known_event_hash(event: Any) -> Optional[str]:
    """The registry key of an event with the same origin, to within a microsecond and microdegree."""
    values = (
        float(event.time),
        float(event.lat),
        float(event.lon),
        float(event.depth or 0.0),
    )
    for event_hash, known in event_registry.items():
        known_values = (
            float(known.time),
            float(known.lat),
            float(known.lon),
            float(known.depth or 0.0),
        )
        if all(abs(left - right) <= 1e-6 for left, right in zip(values, known_values, strict=True)):
            return event_hash
    return None


def activate_event(event_hash: Optional[str]) -> None:
    """Set every active-event projection from one registry entry, or clear all."""
    global active_event, active_event_hash, active_moment_tensor
    event = event_registry.get(event_hash) if event_hash else None
    if event is None:
        active_event = None
        active_event_hash = ""
        active_moment_tensor = None
        return
    active_event = (
        float(event.lat),
        float(event.lon),
        float(event.depth or 0.0),
        float(event.time),
    )
    active_event_hash = str(event_hash)
    active_moment_tensor = event.moment_tensor


def replace_derived_events(origin: str, events: Mapping[str, Any]) -> None:
    """Swap the events a snuffling's previous run registered for this run's."""
    old_events = derived_events.get(origin, {})
    active_was_retired = False
    for event_hash, owned_event in old_events.items():
        if event_hash not in events and event_registry.get(event_hash) is owned_event:
            event_registry.pop(event_hash, None)
            active_was_retired = active_was_retired or active_event_hash == event_hash
    event_registry.update(events)
    derived_events[origin] = dict(events)
    if active_was_retired:
        activate_event(None)
    elif active_event_hash in events:
        activate_event(active_event_hash)


class DatasetState(NamedTuple):
    """Every session global a transaction snapshots, rolls back and commits."""

    traces: List[ptrace.Trace]
    source_traces: "OrderedDict[str, List[ptrace.Trace]]"
    source_digests: Dict[str, str]
    suppressed_source_traces: Dict[str, List[ptrace.Trace]]
    digest_owners: Dict[str, "set[str]"]
    derived_traces: Dict[str, List[ptrace.Trace]]
    derived_events: Dict[str, Dict[str, Any]]
    pile_is_example: bool
    stations: Dict[Tuple[str, ...], Tuple[float, float, float, float]]
    active_event: Optional[Tuple[float, float, float, float]]
    active_event_hash: str
    active_moment_tensor: Any
    event_registry: "OrderedDict[str, Any]"
    station_xmls: List[Any]
    warned_no_response: "set[NslcId]"
    hidden_patterns: List[str]
    quick_filter_patterns: Optional[List[str]]
    scaling_hooks: "OrderedDict[str, Tuple[Optional[float], Optional[float]]]"
    response_generation: int
    gf_stores: Dict[str, str]


class SessionSnapshot(NamedTuple):
    """The committed dataset and the generation it was committed at."""

    generation: int
    state: DatasetState


class SessionUpdate(NamedTuple):
    """A job's dataset, captured for commit or parked while a view runs."""

    state: DatasetState


class StaleSessionCommit(NamedTuple):
    """A rejected commit: the store moved past the transaction's generation."""

    expected_generation: int
    actual_generation: int


def empty_dataset_state(loaded: List[ptrace.Trace]) -> DatasetState:
    """A dataset holding only `loaded`, with no metadata, events or filters."""
    return DatasetState(
        traces=list(loaded),
        source_traces=OrderedDict(),
        source_digests={},
        suppressed_source_traces={},
        digest_owners={},
        derived_traces={},
        derived_events={},
        pile_is_example=False,
        stations={},
        active_event=None,
        active_event_hash="",
        active_moment_tensor=None,
        event_registry=OrderedDict(),
        station_xmls=[],
        warned_no_response=set(),
        hidden_patterns=[],
        quick_filter_patterns=None,
        scaling_hooks=OrderedDict(),
        response_generation=0,
        gf_stores={},
    )


def capture_dataset_state() -> DatasetState:
    """The session globals, with fresh containers over the same traces and events."""
    return DatasetState(
        traces=list(traces),
        source_traces=OrderedDict(
            (source_id, list(owned)) for source_id, owned in source_traces.items()
        ),
        source_digests=dict(source_digests),
        suppressed_source_traces={
            source_id: list(owned) for source_id, owned in suppressed_source_traces.items()
        },
        digest_owners={digest: set(owners) for digest, owners in digest_owners.items()},
        derived_traces={origin: list(owned) for origin, owned in derived_traces.items()},
        derived_events={origin: dict(owned) for origin, owned in derived_events.items()},
        pile_is_example=pile_is_example,
        stations=dict(stations),
        active_event=active_event,
        active_event_hash=active_event_hash,
        active_moment_tensor=active_moment_tensor,
        event_registry=OrderedDict(event_registry),
        station_xmls=list(station_xmls),
        warned_no_response=set(warned_no_response),
        hidden_patterns=list(hidden_patterns),
        quick_filter_patterns=None
        if quick_filter_patterns is None
        else list(quick_filter_patterns),
        scaling_hooks=OrderedDict(scaling_hooks),
        response_generation=response_generation,
        gf_stores=dict(gf_stores),
    )


def install_dataset_state(dataset: DatasetState) -> None:
    """Make `dataset` the session state, in fresh containers, and retire every cache."""
    global traces, source_traces, source_digests, suppressed_source_traces, digest_owners
    global derived_traces, derived_events, pile_is_example
    global stations, active_event, active_event_hash, active_moment_tensor, event_registry
    global station_xmls, warned_no_response, response_generation
    global hidden_patterns, quick_filter_patterns, scaling_hooks, gf_stores
    traces = list(dataset.traces)
    source_traces = OrderedDict(
        (source_id, list(owned)) for source_id, owned in dataset.source_traces.items()
    )
    source_digests = dict(dataset.source_digests)
    suppressed_source_traces = {
        source_id: list(owned) for source_id, owned in dataset.suppressed_source_traces.items()
    }
    digest_owners = {digest: set(owners) for digest, owners in dataset.digest_owners.items()}
    derived_traces = {origin: list(owned) for origin, owned in dataset.derived_traces.items()}
    derived_events = {origin: dict(owned) for origin, owned in dataset.derived_events.items()}
    pile_is_example = dataset.pile_is_example
    stations = dict(dataset.stations)
    active_event = dataset.active_event
    active_event_hash = dataset.active_event_hash
    active_moment_tensor = dataset.active_moment_tensor
    event_registry = OrderedDict(dataset.event_registry)
    station_xmls = list(dataset.station_xmls)
    warned_no_response = set(dataset.warned_no_response)
    hidden_patterns = list(dataset.hidden_patterns)
    quick_filter_patterns = (
        None if dataset.quick_filter_patterns is None else list(dataset.quick_filter_patterns)
    )
    scaling_hooks = OrderedDict(dataset.scaling_hooks)
    response_generation = dataset.response_generation
    gf_stores = dict(dataset.gf_stores)
    mark_traces_changed()


def clone_dataset_state(dataset: DatasetState) -> DatasetState:
    """A deep copy a job can mutate without touching the committed snapshot."""
    trace_copies: Dict[int, ptrace.Trace] = {}
    # One memo across every event-carrying field: replace_derived_events retires
    # registry entries by object identity, and active_moment_tensor aliases the
    # registered event's moment tensor, so the clones must keep sharing objects.
    event_memo: Dict[int, Any] = {}

    def clone_trace(trace: ptrace.Trace) -> ptrace.Trace:
        cloned = trace_copies.get(id(trace))
        if cloned is None:
            cloned = trace.copy(data=True)
            trace_copies[id(trace)] = cloned
        return cloned

    def clone_traces(originals: Iterable[ptrace.Trace]) -> List[ptrace.Trace]:
        return [clone_trace(trace) for trace in originals]

    return DatasetState(
        traces=clone_traces(dataset.traces),
        source_traces=OrderedDict(
            (source_id, clone_traces(owned)) for source_id, owned in dataset.source_traces.items()
        ),
        source_digests=dict(dataset.source_digests),
        suppressed_source_traces={
            source_id: clone_traces(owned)
            for source_id, owned in dataset.suppressed_source_traces.items()
        },
        digest_owners={digest: set(owners) for digest, owners in dataset.digest_owners.items()},
        derived_traces={
            origin: clone_traces(owned) for origin, owned in dataset.derived_traces.items()
        },
        derived_events=copy.deepcopy(dataset.derived_events, event_memo),
        pile_is_example=dataset.pile_is_example,
        stations=dict(dataset.stations),
        active_event=dataset.active_event,
        active_event_hash=dataset.active_event_hash,
        active_moment_tensor=copy.deepcopy(dataset.active_moment_tensor, event_memo),
        event_registry=copy.deepcopy(dataset.event_registry, event_memo),
        station_xmls=copy.deepcopy(dataset.station_xmls),
        warned_no_response=set(dataset.warned_no_response),
        hidden_patterns=list(dataset.hidden_patterns),
        quick_filter_patterns=None
        if dataset.quick_filter_patterns is None
        else list(dataset.quick_filter_patterns),
        scaling_hooks=OrderedDict(dataset.scaling_hooks),
        response_generation=dataset.response_generation,
        gf_stores=dict(dataset.gf_stores),
    )


class _ViewLaneCaches(NamedTuple):
    data_generation: int
    cutout_key: Optional[Tuple[Any, ...]]
    cutout_traces: Optional[List[ptrace.Trace]]
    cutout_notices: List[str]
    transfer_function_cache: Dict[Tuple[Any, ...], np.ndarray]
    cached_pile: Optional[Any]
    cached_pile_generation: Optional[int]
    box_styles: Dict[Tuple[Any, ...], int]
    box_styles_generation: Optional[int]


def _capture_view_caches() -> _ViewLaneCaches:
    return _ViewLaneCaches(
        data_generation=data_generation,
        cutout_key=cutout_key,
        cutout_traces=cutout_traces,
        cutout_notices=list(cutout_notices),
        transfer_function_cache=dict(transfer_function_cache),
        cached_pile=cached_pile,
        cached_pile_generation=cached_pile_generation,
        box_styles=dict(box_styles),
        box_styles_generation=box_styles_generation,
    )


def _install_view_caches(caches: _ViewLaneCaches) -> None:
    global data_generation, cutout_key, cutout_traces
    global cached_pile, cached_pile_generation, box_styles_generation
    data_generation = caches.data_generation
    cutout_key = caches.cutout_key
    cutout_traces = caches.cutout_traces
    cutout_notices[:] = caches.cutout_notices
    transfer_function_cache.clear()
    transfer_function_cache.update(caches.transfer_function_cache)
    cached_pile = caches.cached_pile
    cached_pile_generation = caches.cached_pile_generation
    box_styles.clear()
    box_styles.update(caches.box_styles)
    box_styles_generation = caches.box_styles_generation


class SessionStore:
    """The committed session state and the transaction a running job works on.

    begin_transaction() pays the transaction's single deep clone; snapshot() and
    capture_update() copy containers only, over objects the other side never
    mutates.
    A view that interleaves with a job sees the committed snapshot: enter_view
    and exit_view swap the dataset state and the view-lane caches together, so
    the job never sees view caches and the view never sees the job's.
    A partial enter_view/exit_view self-heals on the next cycle; interrupts
    cannot reach the view lane because GetView is not cancellable.
    """

    def __init__(self) -> None:
        self._generation = 0
        self._transaction_snapshot: Optional[SessionSnapshot] = None
        self._suspended_job_update: Optional[SessionUpdate] = None
        self._view_caches: Optional[_ViewLaneCaches] = None
        self._job_caches: Optional[_ViewLaneCaches] = None
        self._view_projected = False

    @property
    def generation(self) -> int:
        return self._generation

    def snapshot(self) -> SessionSnapshot:
        return SessionSnapshot(self._generation, capture_dataset_state())

    def begin_transaction(self, snapshot: SessionSnapshot) -> None:
        self._transaction_snapshot = snapshot
        install_dataset_state(clone_dataset_state(snapshot.state))

    def capture_update(self) -> SessionUpdate:
        return SessionUpdate(capture_dataset_state())

    def restore_snapshot(self, snapshot: SessionSnapshot) -> None:
        install_dataset_state(snapshot.state)

    def rollback(self, snapshot: SessionSnapshot) -> None:
        self.restore_snapshot(snapshot)
        self._transaction_snapshot = None
        # The rollback reinstalled the state the view caches were built
        # against, so the now-direct view lane keeps them instead of
        # rebuilding the pile and cutout from scratch.
        if self._view_caches is not None:
            _install_view_caches(self._view_caches)
        self._drop_view_projection()

    def enter_view(self) -> None:
        if self._transaction_snapshot is None:
            return
        if self._suspended_job_update is None:
            self._suspended_job_update = SessionUpdate(capture_dataset_state())
            self._job_caches = _capture_view_caches()
        self._view_projected = False
        install_dataset_state(self._transaction_snapshot.state)
        if self._view_caches is not None:
            _install_view_caches(self._view_caches)
        self._view_projected = True

    def exit_view(self) -> None:
        # The job's state reference is dropped only after the reinstall, and the
        # view caches are stashed only when the projection completed, so an
        # interrupted cycle cannot orphan the job's state or mislabel its caches.
        job_update = self._suspended_job_update
        if job_update is None:
            return
        if self._view_projected:
            self._view_caches = _capture_view_caches()
        self._view_projected = False
        install_dataset_state(job_update.state)
        if self._job_caches is not None:
            _install_view_caches(self._job_caches)
        self._job_caches = None
        self._suspended_job_update = None

    def commit(self, expected: int, update: SessionUpdate) -> Optional[StaleSessionCommit]:
        actual = self._generation
        if expected != actual:
            # The job lane is serialized, so a mismatch is a protocol fault
            # upstream; guarded because a wrong commit cannot be undone.
            self._transaction_snapshot = None
            self._drop_view_projection()
            return StaleSessionCommit(expected, actual)
        install_dataset_state(update.state)
        self._generation += 1
        self._transaction_snapshot = None
        self._drop_view_projection()
        return None

    def _drop_view_projection(self) -> None:
        self._suspended_job_update = None
        self._view_caches = None
        self._job_caches = None
        self._view_projected = False


session_store = SessionStore()
