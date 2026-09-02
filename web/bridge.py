"""Type-checked pyrocko shim that runs inside Pyodide and drives the egui
frontend.

This module is the *only* place that touches pyrocko. It exposes a tiny, stable,
protobuf API to the Pyodide worker (`web/bridge_worker.js`), which in turn is
called from Rust (`src/bridge.rs`) via `web/bridge.js`. The worker-facing
``*_pb`` functions parse and return generated protobuf messages; every request
and response on the live path is protobuf bytes (no JSON crosses the boundary).

pyrocko itself is left completely unmodified; we only call its public API
(`pyrocko.trace.Trace`, `pyrocko.io.load`).
"""

from __future__ import annotations

import asyncio
import copy
import heapq
import io
import math
import os
import re
import shutil
import time
import traceback
from collections import OrderedDict
from contextvars import ContextVar
from datetime import datetime, timezone
from importlib import import_module
from typing import (
    Any,
    Callable,
    Dict,
    Iterable,
    Iterator,
    List,
    Mapping,
    NamedTuple,
    Optional,
    Protocol,
    Tuple,
    TypeVar,
    cast,
)
from urllib.parse import quote

import numpy as np
from pyrocko import io as pio
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import (
    bridge_dispatch,
    bridge_pb2,
    common_pb2,
    data_pb2,
    figure_pb2,
    scene_pb2,
    snuffling_pb2,
    view_pb2,
)

NslcId = Tuple[str, str, str, str]
TraceRangeFunc = Callable[..., Mapping[Any, Tuple[Any, Any]]]
OperationReporter = Callable[[int, int, str], bool]
CommitGate = Callable[[], bool]
ExternalWaitCallback = Callable[[], None]
_MarkerResultT = TypeVar("_MarkerResultT", snuffling_pb2.SnufflingResult, data_pb2.SnufMarkerList)
_MarkerBudget = Optional[snuffling_pb2.MarkerBudget]
_OPERATION_REPORTER: ContextVar[Optional[OperationReporter]] = ContextVar(
    "operation_reporter", default=None
)
_BEGIN_EXTERNAL_WAIT: ContextVar[Optional[ExternalWaitCallback]] = ContextVar(
    "begin_external_wait", default=None
)
_END_EXTERNAL_WAIT: ContextVar[Optional[ExternalWaitCallback]] = ContextVar(
    "end_external_wait", default=None
)


class AutoScalerLike(Protocol):
    def make_scale(self, data_range: Tuple[float, float]) -> Tuple[float, float, float]: ...
    def make_exp(self, x: float) -> int: ...


# The current set of loaded traces (snuffler's "pile").
_TRACES: List[ptrace.Trace] = []
# Provenance: traces contributed by each on-disk source (File System Access
# handle id), so "reload modified files" can swap just that source's traces.
_SOURCE_TRACES: "OrderedDict[str, List[ptrace.Trace]]" = OrderedDict()
# Content digest registered for each re-readable source.
# Reload uses this to keep duplicate detection coherent when a file changes.
_SOURCE_DIGESTS: Dict[str, str] = {}
# Parsed traces for sources hidden because another source currently owns the
# same content. Keeping the source materialized lets it take ownership without
# another filesystem notification when the visible owner later changes.
_SUPPRESSED_SOURCE_TRACES: Dict[str, List[ptrace.Trace]] = {}
# Provenance of snuffling-derived traces, keyed by the snuffling's origin tag,
# so a rerun replaces its own previous output (Qt's `cleanup()` ticket
# contract) without touching file provenance.
_DERIVED_TRACES: Dict[str, List[ptrace.Trace]] = {}
# Event-registry objects contributed by each snuffling origin.
# Object identity prevents cleanup from deleting a newer event that reused an
# owned hash through a catalog load, marker load, or frontend edit.
_DERIVED_EVENTS: Dict[str, Dict[str, Any]] = {}
# True while the pile still holds only the boot demo (the synthetic example or
# a checksummed example bundle nobody asked to keep). The first real data that
# would append to it replaces it instead: Qt opens empty, so its append-on-open
# never composes with a pre-seeded pile the way the web's boot example would.
_PILE_IS_EXAMPLE = False
# Owners of each content digest currently represented in the pile.
# A source can retain its last digest while owning no traces after converging on
# another source, so duplicate detection must count owners rather than source IDs.
_DIGEST_OWNERS: Dict[str, "set[str]"] = {}

# Trace visibility driven by the ':' command line, mirroring snuffler's two
# independent filters: `_HIDDEN` is the blacklist `hide`/`unhide` maintain, and
# `_REDUCE` is the quick filter the `n|s|l|c` commands set (when not None, only
# traces matching one of its globs are kept). Matching is fnmatch over the
# "net.sta.loc.cha" string. Data is never dropped, only hidden.
_HIDDEN: List[str] = []
_REDUCE: Optional[List[str]] = None
# Per-pattern amplitude scaling overrides from the ':scaling' command
# (snuffler's scaling hooks). Keyed by n.s.l.c pattern; the '_' key is the global
# (no-pattern) rule. Each value is (vmin, vmax) where None leaves that bound auto.
_SCALING_HOOKS: "OrderedDict[str, Tuple[Optional[float], Optional[float]]]" = OrderedDict()
_DATA_GEN = 0
_RESPONSE_GEN = 0
_VIEW_PERF_LOG_INTERVAL_S = 1.0
_VIEW_PERF_SLOW_MS = 25.0
# The processed-cutout cache, exactly snuffler's: one slot holding the last
# `prepare_cutout2`-style state vector and its processed traces, reused whenever
# the requested window is contained in the cached one and the rest of the vector
# matches (pile_viewer.py:3640).
_CUTOUT_VEC: Optional[Tuple[Any, ...]] = None
_CUTOUT_TRACES: Optional[List[ptrace.Trace]] = None
# The notices the cached cutout's processing produced, re-emitted on every
# cache hit: the drawn data is the same failed-filter/unrestituted data, so the
# warning must outlive the one view that happened to rebuild the cutout.
_CUTOUT_NOTICES: List[str] = []
# snuffler's `tf_cache`: the combined FFT-filtering transfer function per
# (deltat, nfreqs, nslc-or-None). Keyed additionally on the corners and the
# response generation so it can never go stale — Qt instead clears it from the
# filter-change slots.
_TF_CACHE: Dict[Tuple[Any, ...], np.ndarray] = {}
# Frozen scale ranges ('Fix Scale Ranges'). Snuffler remembers them with no
# notion of what produced them, so they survive filter, sort and even pile
# changes (pile_viewer.py:3281); this port matches that and deliberately does not
# key them on any context.
_OLD_DATA_RANGES: Dict[Any, Tuple[float, float]] = {}
# Snuffler's `trace_styles` (`determine_box_styles`, pile_viewer.py:2947): the
# box tint index per (full_id, deltat), recomputed when the pile changes.
_BOX_STYLES: Dict[Tuple[Any, ...], int] = {}
_BOX_STYLES_GEN: Optional[int] = None
# Processing failures collected while building one view, delivered on its meta.
_VIEW_NOTICES: List[str] = []
_LAST_VIEW_PERF_LOG_S = 0.0
# The real `pyrocko.pile.Pile` the view chops through, rebuilt lazily when the
# trace set changes so `pile.chopper` — the exact code Qt calls — does the
# chopping, degapping and rate selection.
_PILE: Optional[Any] = None
_PILE_GEN: Optional[int] = None


def _clear_view_cache() -> None:
    global _CUTOUT_VEC, _CUTOUT_TRACES, _PILE, _PILE_GEN
    _CUTOUT_VEC = None
    _CUTOUT_TRACES = None
    _CUTOUT_NOTICES.clear()
    _TF_CACHE.clear()
    _PILE = None
    _PILE_GEN = None


def _invalidate_interrupted_view() -> None:
    _clear_view_cache()
    _VIEW_NOTICES.clear()
    _OLD_DATA_RANGES.clear()


def _mark_traces_changed() -> None:
    global _DATA_GEN
    _DATA_GEN += 1
    _clear_view_cache()


# The session state a Shift+R code reload carries over: everything the user
# loaded, derived or configured — pile, provenance, metadata, command filters,
# scale memories, mounted stores — plus the generation counters that keep the
# frontend's caches coherent. Deliberately absent: the pure caches (cutouts,
# transfer functions, box styles, waterfall LUTs), which rebuild from the
# restored generations, and per-view transients like the notice list. The
# worker's reload snippet calls `_reload_snapshot`/`_reload_restore` so this
# module owns the list; a name here that stops existing is caught by
# tests/parity/test_reload_keep.py.
_RELOAD_KEEP = (
    "_TRACES",
    "_SOURCE_TRACES",
    "_SOURCE_DIGESTS",
    "_SUPPRESSED_SOURCE_TRACES",
    "_DERIVED_TRACES",
    "_DERIVED_EVENTS",
    "_PILE_IS_EXAMPLE",
    "_DIGEST_OWNERS",
    "_HIDDEN",
    "_REDUCE",
    "_SCALING_HOOKS",
    "_DATA_GEN",
    "_RESPONSE_GEN",
    "_OLD_DATA_RANGES",
    "_STATIONS",
    "_ACTIVE_EVENT",
    "_ACTIVE_HASH",
    "_ACTIVE_MT",
    "_EVENT_REGISTRY",
    "_STATIONXMLS",
    "_GF_STORES",
    "_PLATES",
    "_SEISBENCH_MODEL_URLS",
    "_STAGE_COUNTER",
    # The session store carries the generation Rust fences view replies with; a
    # reload that reset it to zero would leave every later view stale-rejected.
    "_SESSION_STORE",
)


def _reload_snapshot() -> Dict[str, Any]:
    """The session state a code reload must carry over, captured off the *old*
    module just before ``importlib.reload`` — and detached from it.

    The names are popped, not read: ``importlib.reload`` reuses the module
    dict, so a name left behind would survive the reload even when the new
    source no longer defines it — a zombie global nothing reads.
    Popping loses nothing, because re-executing the source recreates every
    living global with its fresh default; the caller must pair this with
    `_reload_restore` (in a ``finally``, so a failed reload cannot strand the
    detached state).

    Keyed by name, not versioned: state survives exactly where old and new
    code agree on the global's name.
    A global renamed or retired between the two versions resets to its fresh
    default on that reload — an accepted loss, since Shift+R is a dev-loop
    tool, not a persistence format."""
    return {name: globals().pop(name) for name in _RELOAD_KEEP if name in globals()}


def _reload_restore(saved: Dict[str, Any]) -> None:
    """Reinstall a `_reload_snapshot` into the freshly reloaded module.

    Only names the *new* module's `_RELOAD_KEEP` still claims are installed:
    a snapshot entry the new code no longer knows would otherwise be parked
    as a zombie global that nothing reads while the successor state sits
    empty.
    State introduced by newer code keeps its fresh default.
    The generation bump makes every cache rebuild against the restored pile."""
    globals().update({name: value for name, value in saved.items() if name in _RELOAD_KEEP})
    _mark_traces_changed()


def _without_empty(traces: Iterable[ptrace.Trace]) -> List[ptrace.Trace]:
    """Drop the sampleless "traces" a MiniSEED file can carry.

    Datacenters ship records with no samples and a sample rate of zero, and pyrocko
    hands each one back as a trace. Nothing can be drawn from it, so keeping it costs
    a blank row that looks like a dead channel — which is a thing this viewer is
    supposed to be able to show, and so a row that lies. pyrocko's own ``pile`` refuses
    such a file outright (``dispatch_key`` takes ``log(deltatmin)``), so there is no
    upstream behaviour to match here: it is this or a traceback.
    """
    return [tr for tr in traces if tr.deltat > 0.0 and tr.ydata is not None and tr.ydata.size]


def _set_traces(traces: List[ptrace.Trace], append: bool = False) -> None:
    global _TRACES, _PILE_IS_EXAMPLE
    # Real data replaces the boot example rather than joining it: appending
    # onto the demo pile would show the user's channels among synthetic noise
    # with nothing marking which is which.
    if append and _PILE_IS_EXAMPLE:
        append = False
    _TRACES = (list(_TRACES) + traces) if append else traces
    if not append:
        _SOURCE_TRACES.clear()
        _SOURCE_DIGESTS.clear()
        _SUPPRESSED_SOURCE_TRACES.clear()
        _DIGEST_OWNERS.clear()
        _DERIVED_TRACES.clear()
        # The replaced scene's events, stations and responses go with its
        # waveforms — user loads only ever append, so nothing user-owned can
        # pass through here except in the boot-example handover above.
        _clear_dataset_metadata()
    _PILE_IS_EXAMPLE = False
    _mark_traces_changed()


def _clear_event_context() -> None:
    _EVENT_REGISTRY.clear()
    _DERIVED_EVENTS.clear()
    _activate_event(None)


def _clear_dataset_metadata() -> None:
    """Drop the non-waveform context of a replaced scene. The events, station
    coordinates and instrument responses all describe the dataset going away;
    any of them surviving into the next scene sorts it wrongly (stale
    coordinates) or deconvolves an overlapping NSLC against the wrong
    response. Only scene *replacements* come through here — an example
    bundle, the boot example giving way to the first real load, EmptyScene, a
    live-stream reset — never a user append."""
    global _RESPONSE_GEN
    _clear_event_context()
    _STATIONS.clear()
    _WARNED_NO_RESPONSE.clear()
    if _STATIONXMLS:
        _STATIONXMLS.clear()
        # The transfer-function cache keys on the response generation, so the
        # bump retires every entry built from the dropped documents.
        _RESPONSE_GEN += 1


def _replace_source(source_id: str, new_traces: List[ptrace.Trace]) -> None:
    """Swap the traces previously loaded from ``source_id`` for freshly-loaded ones
    (the reload path). Removes the old per-source Trace objects by identity and
    appends the new, leaving synthetic/FDSN/other-source traces untouched — the
    faithful analog of pyrocko ``reload_modified`` only touching changed files."""
    global _TRACES
    old = _SOURCE_TRACES.get(source_id)
    if old is None:
        raise ValueError("waveform source %r was not registered by an initial load" % source_id)
    if old:
        old_ids = {id(t) for t in old}
        _TRACES = [t for t in _TRACES if id(t) not in old_ids]
    _TRACES = list(_TRACES) + new_traces
    _SOURCE_TRACES[source_id] = new_traces
    _mark_traces_changed()


def _digest_owner(source_id: str) -> str:
    return "source:%s" % source_id if source_id else "anonymous"


def _add_digest_owner(digest: str, source_id: str) -> None:
    _DIGEST_OWNERS.setdefault(digest, set()).add(_digest_owner(source_id))


def _promote_suppressed_source(digest: str) -> None:
    for source_id in _SOURCE_TRACES:
        if _SOURCE_DIGESTS.get(source_id) != digest:
            continue
        traces = _SUPPRESSED_SOURCE_TRACES.pop(source_id, None)
        if traces is None:
            continue
        _replace_source(source_id, traces)
        _add_digest_owner(digest, source_id)
        return


def _remove_digest_owner(digest: str, source_id: str) -> None:
    owners = _DIGEST_OWNERS.get(digest)
    if owners is None:
        return
    owners.discard(_digest_owner(source_id))
    if not owners:
        del _DIGEST_OWNERS[digest]
        _promote_suppressed_source(digest)


def _replace_derived(origin: str, new_traces: List[ptrace.Trace]) -> None:
    """Swap the traces a snuffling's previous run added for this run's — the
    trace half of Qt's `cleanup()` ticket contract (snuffling.py:1689), scoped
    by origin. Identity-based, so it can never delete a user-loaded trace that
    merely shares a name; and it leaves `_SOURCE_TRACES`, the load digests and
    the boot-example flag alone, because derived traces never came from a file
    and replacing them must not reset file provenance."""
    global _TRACES
    old = _DERIVED_TRACES.get(origin)
    if old:
        old_ids = {id(t) for t in old}
        _TRACES = [t for t in _TRACES if id(t) not in old_ids]
    _TRACES = list(_TRACES) + list(new_traces)
    _DERIVED_TRACES[origin] = list(new_traces)
    _mark_traces_changed()


def _trace_selected(tr: ptrace.Trace) -> bool:
    """Whether the ':' command-line filters leave this trace visible.

    pyrocko's canonical NSLC matcher (shell globs, case-insensitive, anchored on
    the full "net.sta.loc.cha" code), the same one snuffler's hide/quick-filter
    use — match_nslc takes a list of patterns and returns True if any matches.
    This is what snuffler passes into `prepare_cutout2` as its `trace_filter`.
    """
    from pyrocko.util import match_nslc

    if _REDUCE is not None and not match_nslc(_REDUCE, tr.nslc_id):
        return False
    return not (_HIDDEN and match_nslc(_HIDDEN, tr.nslc_id))


def _visible_traces() -> List[ptrace.Trace]:
    return [tr for tr in _TRACES if _trace_selected(tr)]


def _pile() -> Any:
    """The loaded traces as a real `pyrocko.pile.Pile`.

    The view chops through `pile.chopper` — snuffler's own chopping, degapping and
    sampling-rate selection — rather than a reimplementation of it. Rebuilt lazily
    when the trace set changes; the Trace objects are shared, not copied, which is
    safe because the chopper hands out chopped copies.
    """
    global _PILE, _PILE_GEN
    if _PILE is None or _PILE_GEN != _DATA_GEN:
        from pyrocko import pile as ppile

        p = ppile.Pile()
        if _TRACES:
            p.add_file(ppile.MemTracesFile(None, list(_TRACES)))
        _PILE = p
        _PILE_GEN = _DATA_GEN
    return _PILE


# Station coordinates -> (lat, lon, elevation, depth) and the active event
# (lat, lon, depth_m, time), populated by the FDSN fetch helpers below. Used for
# distance/azimuth sorting. The fetch itself runs here in Python (pyfetch over the
# browser's network); URL building, parsing and geo math are all pyrocko/Python.
#
# Keyed as snuffler keys it (`station_keys`, pile_viewer.py:1375): every station is
# filed under both `(net, sta, loc)` and `(net, sta)`, and looked up in that order.
# Co-located instruments differ in more than their location code — IU.ANMO's two
# broadbands hang 145 m and 90 m down the same borehole — so a table with one entry
# per station cannot say where either of them is. The two-element key stays because
# it is snuffler's fallback for a trace whose location code no station file mentions,
# and, as there, the last station added wins it.
_STATIONS: Dict[Tuple[str, ...], Tuple[float, float, float, float]] = {}
_ACTIVE_EVENT: Optional[Tuple[float, float, float, float]] = None
# Registry key of the active event, so machine-made picks (cake, F-keys through
# the frontend) can be associated with it (snuffler `set_event`).
_ACTIVE_HASH: str = ""
# Moment tensor of the active (largest-magnitude) catalog event, if it has one.
# Used as the reference for the marker table's Kagan-angle column.
_ACTIVE_MT: Any = None
# All loaded events, keyed by their marker event_hash, so the user can pick any of
# them as the active event (distance/Kagan/cake/seismosizer reference).
_EVENT_REGISTRY: "OrderedDict[str, Any]" = OrderedDict()


def _register_event(event_hash: str, event: Any) -> bool:
    if event_hash not in _EVENT_REGISTRY and len(_EVENT_REGISTRY) >= MAX_MARKER_BUDGET:
        removable = next((key for key in _EVENT_REGISTRY if key != _ACTIVE_HASH), None)
        if removable is None:
            return False
        _EVENT_REGISTRY.pop(removable, None)
    _EVENT_REGISTRY[event_hash] = event
    _EVENT_REGISTRY.move_to_end(event_hash)
    return True


def _known_event_hash(event: Any) -> Optional[str]:
    values = (
        float(event.time),
        float(event.lat),
        float(event.lon),
        float(event.depth or 0.0),
    )
    for event_hash, known in _EVENT_REGISTRY.items():
        known_values = (
            float(known.time),
            float(known.lat),
            float(known.lon),
            float(known.depth or 0.0),
        )
        if all(abs(left - right) <= 1e-6 for left, right in zip(values, known_values, strict=True)):
            return event_hash
    return None


def _activate_event(event_hash: Optional[str]) -> None:
    """Set every active-event projection from one registry entry, or clear all."""
    global _ACTIVE_EVENT, _ACTIVE_HASH, _ACTIVE_MT
    event = _EVENT_REGISTRY.get(event_hash) if event_hash else None
    if event is None:
        _ACTIVE_EVENT = None
        _ACTIVE_HASH = ""
        _ACTIVE_MT = None
        return
    _ACTIVE_EVENT = (
        float(event.lat),
        float(event.lon),
        float(event.depth or 0.0),
        float(event.time),
    )
    _ACTIVE_HASH = str(event_hash)
    _ACTIVE_MT = event.moment_tensor


def _replace_derived_events(origin: str, events: Mapping[str, Any]) -> None:
    old_events = _DERIVED_EVENTS.get(origin, {})
    active_was_retired = False
    for event_hash, owned_event in old_events.items():
        if event_hash not in events and _EVENT_REGISTRY.get(event_hash) is owned_event:
            _EVENT_REGISTRY.pop(event_hash, None)
            active_was_retired = active_was_retired or _ACTIVE_HASH == event_hash
    _EVENT_REGISTRY.update(events)
    _DERIVED_EVENTS[origin] = dict(events)
    if active_was_retired:
        _activate_event(None)
    elif _ACTIVE_HASH in events:
        _activate_event(_ACTIVE_HASH)


# Instrument responses, used by Deconvolution: the parsed FDSNStationXML
# documents in load order. A list rather than one merged document, because that
# is how snuffler keeps them (`station_xmls`, pile_viewer.py:4470): lookups take
# the first document that resolves, so a channel described twice across files
# still restitutes instead of raising on the ambiguity a merge would create.
_STATIONXMLS: List[Any] = []
# Channels already reported as having no usable response, so the warning fires
# once per response generation rather than once per paint.
_WARNED_NO_RESPONSE: "set[NslcId]" = set()
# CORS-enabled FDSN web services (verified to allow cross-origin fetch).
_FDSN = {
    "geofon": "https://geofon.gfz.de",
    "earthscope": "https://service.earthscope.org",
    "iris": "https://service.iris.edu",
    "usgs": "https://earthquake.usgs.gov",
    "orfeus": "https://www.orfeus-eu.org",
    "ingv": "https://webservices.ingv.it",
    "resif": "https://ws.resif.fr",
    "bgr": "https://eida.bgr.de",
}


def _fdsn_base(name: str, default: str = "geofon") -> str:
    if name.startswith(("http://", "https://")):
        return name.rstrip("/")
    return _FDSN.get(name, _FDSN[default])


_STAGE_DIR = "/tmp/snuffler-staged"
_STAGE_COUNTER = 0


def _stage_bytes(name: str, content: bytes) -> str:
    """Write request bytes into the Pyodide FS and return the path. pyrocko's
    ``io.load`` reads a file, so file-carrying rpcs stage here in Python — the
    worker is a dumb relay and no longer touches the FS for loads."""
    global _STAGE_COUNTER
    os.makedirs(_STAGE_DIR, exist_ok=True)
    _STAGE_COUNTER += 1
    safe = "".join(c if (c.isalnum() or c in "._-") else "_" for c in (name or "data"))[-96:]
    path = "%s/%d-%s" % (_STAGE_DIR, _STAGE_COUNTER, safe or "data")
    with open(path, "wb") as f:
        f.write(content)
    return path


def _asset_url(path: str) -> str:
    """Resolve a bundled-asset path against the worker's location, matching the
    worker's ``new URL(path, self.location.href)``."""
    import js

    return str(js.URL.new(path, js.location.href).href)


async def _fetch_bytes(url: str) -> bytes:
    """GET raw bytes through the browser's fetch (pyodide-http), which is reachable
    from Python inside the worker — so network rpcs need no JS orchestration."""
    from pyodide.http import pyfetch

    resp = await _await_external_operation(pyfetch(url))
    if not resp.ok:
        raise RuntimeError("fetch %s: HTTP %s" % (url, resp.status))
    return bytes(await _await_external_operation(resp.bytes()))


async def _fetch_text_or_empty(url: str) -> str:
    """GET text, returning '' on any non-2xx — an FDSN 'no data' reply parses to an
    empty result rather than an error."""
    from pyodide.http import pyfetch

    resp = await _await_external_operation(pyfetch(url))
    return str(await _await_external_operation(resp.string())) if resp.ok else ""


def _local_cors_proxy_url(url: str) -> str:
    if not (url.startswith("http://") or url.startswith("https://")):
        return url
    try:
        import js

        protocol = str(js.location.protocol)
        host = str(js.location.hostname)
    except Exception:
        return url
    if protocol != "http:" or host not in ("localhost", "127.0.0.1", "0.0.0.0", "::1"):
        return url
    return "http://127.0.0.1:17684/cors?url=%s" % quote(url, safe="")


def _local_fdsn_proxy_url(url: str) -> str:
    return _local_cors_proxy_url(url)


def _seisbench_progress(frac: float, label: str) -> None:
    try:
        import js

        js.__snufflerSeisbenchProgress(float(frac), label)
    except Exception:
        pass


def _operation_progress(done: int, total: Optional[int], phase: str) -> None:
    reporter = _OPERATION_REPORTER.get()
    if reporter is not None and not reporter(done, -1 if total is None else total, phase):
        raise KeyboardInterrupt


async def _await_external_operation(awaitable: Any) -> Any:
    """Await JavaScript while keeping SIGINT out of Pyodide's Future resolver."""
    begin_wait = _BEGIN_EXTERNAL_WAIT.get()
    end_wait = _END_EXTERNAL_WAIT.get()
    if begin_wait is not None:
        begin_wait()
    try:
        return await awaitable
    finally:
        if end_wait is not None:
            end_wait()


async def _operation_safe_point(done: int, total: Optional[int], phase: str) -> None:
    _operation_progress(done, total, phase)
    try:
        import js

        await _await_external_operation(js.__snufflerYieldToMacrotask())
    except ImportError:
        await _await_external_operation(asyncio.sleep(0))


async def _pyfetch_fdsn(url: str) -> Any:
    from pyodide.http import pyfetch

    proxied = _local_fdsn_proxy_url(url)
    try:
        resp = await _await_external_operation(pyfetch(proxied))
    except Exception:
        if proxied != url:
            return await _await_external_operation(pyfetch(url))
        raise
    # The local proxy 403s any host off its allowlist — the normal outcome for
    # a custom base URL on localhost, and not an exception. Retry such a host
    # directly: if it sends CORS headers the query still works, and if not the
    # failure surfaces as the network error it is.
    if resp.status == 403 and proxied != url:
        return await _await_external_operation(pyfetch(url))
    return resp


async def _fetch_fdsn_text_or_empty(url: str) -> str:
    """The document at ``url``, or "" for FDSN's own no-data answers.

    Only 204 and 404 — the codes the standard assigns to "no matching data" —
    read as empty. Every other failure raises so a 403 from the local proxy's
    allowlist or a 500 from the service surfaces as the error it is, rather
    than being reported as "0 loaded"."""
    resp = await _pyfetch_fdsn(url)
    if resp.ok:
        return str(await _await_external_operation(resp.string()))
    if resp.status in (204, 404):
        return ""
    raise RuntimeError("HTTP %s from %s" % (resp.status, url))


def _scene_meta() -> scene_pb2.SceneMeta:
    traces = list(_visible_traces())
    return scene_pb2.SceneMeta(
        tmin=min((float(tr.tmin) for tr in traces), default=0.0),
        tmax=max((float(tr.tmax) for tr in traces), default=1.0),
        track_count=len(traces),
        deltat_min=min((float(tr.deltat) for tr in traces), default=0.0),
        deltat_max=max((float(tr.deltat) for tr in traces), default=0.0),
    )


def _make_example_scene() -> scene_pb2.SceneMeta:
    """Synthesize a small multi-station / 3-component dataset (noise + a
    travelling event packet), so the viewer is populated without any file IO."""
    deltat = 0.02  # 50 Hz
    n = 30_000  # 600 s
    tmin = 1_700_000_000.0
    stations = ["STA01", "STA02", "STA03", "STA04", "STA05", "STA06"]
    channels = ["HHZ", "HHN", "HHE"]
    t = np.arange(n) * deltat
    rng = np.random.default_rng(42)

    traces: List[ptrace.Trace] = []
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
                ptrace.Trace(
                    network="XX",
                    station=sta,
                    location="",
                    channel=cha,
                    deltat=deltat,
                    tmin=tmin,
                    ydata=y.astype(np.float64),
                )
            )

    _set_traces(traces)
    # The synthetic demo is a boot pile, not user data: the first real load
    # replaces it (see `_set_traces`).
    global _PILE_IS_EXAMPLE
    _PILE_IS_EXAMPLE = True
    return _scene_meta()


def _load_path_traces(path: str, fmt: str) -> List[ptrace.Trace]:
    """Load waveform traces already staged in Pyodide's filesystem."""
    fmt2 = "detect" if fmt in (None, "", "detect") else fmt
    return _without_empty(pio.load(path, format=fmt2))


def _load_path_scene(name: str, path: str, fmt: str, append: bool = False) -> scene_pb2.SceneMeta:
    """Load a file already staged in Pyodide's FS with pyrocko's ``io.load``."""
    del name
    _set_traces(_load_path_traces(path, fmt), append=append)
    return _scene_meta()


# snuffler's `gap_lap_tolerance`: the largest gap it will interpolate across and the
# largest overlap it will trim, both in samples.
_GAP_LAP_TOLERANCE = 5.0


def _decon_freq_limits(
    deltat: float, data_len: int, hp: Optional[float], lp: Optional[float]
) -> Tuple[float, float, float, float]:
    """Cosine-taper frequency limits for response deconvolution, exactly as
    snuffler's ``PileViewer.get_freq_limits``: anchored to the current highpass /
    lowpass passband, clamped to a Nyquist-safe band and a data-length abs-min."""
    nyquist_safe = (1.0 / (2.0 * deltat)) * 0.9
    abs_min = 0.01 / (max(1, data_len) * deltat)
    taper_width = 0.5
    if hp:
        f2 = min(max(abs_min, hp), nyquist_safe)
        f1 = min(max(abs_min, f2 * (1.0 - taper_width)), nyquist_safe)
    else:
        f1 = f2 = abs_min
    if lp:
        f3 = min(lp, nyquist_safe)
        f4 = min(f3 * (1.0 + taper_width), nyquist_safe)
    else:
        f3 = f4 = nyquist_safe
    return f1, f2, f3, f4


def _view_notice(message: str) -> None:
    """Record a processing failure for the view being built.

    Delivered on `ViewMeta.notices` so the frontend can say what was drawn
    unprocessed — snuffler lets these exceptions reach the user, and silently
    drawing unfiltered data as filtered is the one failure mode worse than
    either. Deduplicated per view.
    """
    if message not in _VIEW_NOTICES:
        _VIEW_NOTICES.append(message)


def _get_instrument_response(nslc_id: NslcId, tmin: float, tmax: float) -> Any:
    """The response for `nslc_id` over the view window, resolved as snuffler
    resolves it (`get_instrument_response`, pile_viewer.py:4468): the StationXML
    documents stay a list in load order and the first one that resolves wins; a
    document that raises — including on an ambiguous double match — is skipped;
    and a channel that resolves nowhere is reported rather than silently passed
    through.
    """
    resp = None
    for sx in _STATIONXMLS:
        try:
            resp = sx.get_pyrocko_response(nslc_id, timespan=(tmin, tmax))
            break
        except Exception:  # noqa: BLE001 — try the next document, exactly as Qt does
            continue
    if resp is None:
        if nslc_id not in _WARNED_NO_RESPONSE:
            _WARNED_NO_RESPONSE.add(nslc_id)
            _view_notice("no instrument response information for %s.%s.%s.%s" % tuple(nslc_id))
        return None
    return resp


def _remove_response(
    tr: ptrace.Trace,
    enabled: bool,
    hp: Optional[float] = None,
    lp: Optional[float] = None,
    tmin: Optional[float] = None,
    tmax: Optional[float] = None,
) -> ptrace.Trace:
    """Deconvolve the instrument response using StationXML fetched from an FDSN
    station service (Data ▸ Download ▸ Fetch responses).
    A trace without a matching response passes through with a view notice.
    The frequency limits + cut-off-fading match snuffler's deconvolution exactly.

    The response is requested over the *view window* (``tmin``/``tmax``), not the
    trace's own span, because that is what the reference passes
    (pile_viewer.py:3845) — a trace crossing a metadata epoch boundary should
    resolve for the epoch on screen.

    The result is in the instrument's own input unit — velocity for a broadband —
    because that is what snuffler's ``get_instrument_response`` produces. Passing
    ``fake_input_units='M'`` here instead would append a differentiation stage and
    yield displacement, differing from Qt by a factor of i*omega across the band with
    nothing on screen to say so.

    Takes the *already demeaned and filtered* segment, because that is the order
    snuffler restitutes in: its restitution block runs over `processed_traces`, after
    the lowpass/highpass. The order matters even though both steps are linear — the
    frequency limits are derived from `data_len`, so deconvolving a differently
    extended trace tapers differently."""
    if not enabled:
        return tr
    span_tmin = float(tr.tmin) if tmin is None else float(tmin)
    span_tmax = float(tr.tmax) if tmax is None else float(tmax)
    resp = _get_instrument_response(tr.nslc_id, span_tmin, span_tmax)
    if resp is None:
        return tr
    freqlimits = _decon_freq_limits(float(tr.deltat), int(tr.data_len()), hp, lp)
    try:
        return tr.transfer(
            freqlimits=freqlimits,
            transfer_function=resp,
            cut_off_fading=True,
            demean=True,
            invert=True,
        )
    except Exception as exc:  # noqa: BLE001 — drawn unrestituted, but never silently
        _view_notice("restitution failed for %s.%s.%s.%s: %s" % (*tr.nslc_id, exc))
        return tr


# Subsort -> order of nslc elements for the sort/grouping key (snuffler's subsort).
_SUBSORT = {
    "nslc": (0, 1, 2, 3),
    "nscl": (0, 1, 3, 2),
    "sncl": (1, 0, 3, 2),
    "lnsc": (2, 0, 1, 3),
    "cnsl": (3, 0, 1, 2),
    # Three elements, not four: these are snuffler's "Grouped by Location" pair, and
    # the element left out of the key is the one the rows group over — every location
    # of a channel lands on the same row. Listing all four here instead made them
    # duplicates of `nscl` and `sncl`.
    "nsc_l": (0, 1, 3),
    "snc_l": (1, 0, 3),
}


def _subsort_perm(subsort: str) -> Tuple[int, ...]:
    """The gather permutation of a subsort; nslc order for an unknown name."""
    return _SUBSORT.get(subsort, (0, 1, 2, 3))


def _dist_m(alat: float, alon: float, blat: float, blon: float) -> float:
    """Surface distance in metres on the WGS84 spheroid, as snuffler measures it.

    ``Location.distance_to`` — what ``set_event_relative_data`` fills ``dist_m``
    from — measures on the WGS84 spheroid via ``orthodrome_ext``, the compiled
    extension the minimal wheel does not ship. But ``distance_accurate50m`` takes
    ``implementation='python'`` and then evaluates the same Meeus formula with plain
    ``math``, agreeing with the C path bit-for-bit. The analytic ``cosdelta`` form
    used here before is spherical, and drifts by several hundred metres over a few
    hundred kilometres.

    The scalar entry point on purpose: the ``_numpy`` one broadcasts scalars to 0-d
    arrays and its Python branch then dies in ``num.where``.
    """
    from pyrocko import orthodrome as od

    return float(od.distance_accurate50m(alat, alon, blat, blon, implementation="python"))


def _dist_deg(alat: float, alon: float, blat: float, blon: float) -> float:
    """Epicentral distance in degrees, converted the way ``Station.dist_deg`` is:
    spheroid metres over the *equatorial* radius, not the mean radius."""
    from pyrocko import orthodrome as od

    return _dist_m(alat, alon, blat, blon) / od.earthradius_equator * od.r2d


def _station_at(net: str, sta: str, loc: str) -> Optional[Tuple[float, float, float, float]]:
    """The station a trace belongs to, resolved as snuffler resolves it.

    ``station_attrib`` (pile_viewer.py:1379) tries ``(net, sta, loc)`` first and falls
    back to ``(net, sta)``, so a station file that names location codes places each
    instrument exactly and one that omits them still places every trace of a station.
    """
    return _STATIONS.get((net, sta, loc)) or _STATIONS.get((net, sta))


def _add_station(
    net: str, sta: str, loc: str, lat: float, lon: float, elevation: float, depth: float
) -> None:
    """File a station under both of snuffler's keys (``add_stations``,
    pile_viewer.py:1406), so lookups can be specific and still always resolve."""
    entry = (float(lat), float(lon), float(elevation), float(depth))
    _STATIONS[(net, sta, loc)] = entry
    _STATIONS[(net, sta)] = entry


def _stations_by_name() -> Dict[Tuple[str, str], Tuple[float, float, float, float]]:
    """One entry per ``(net, sta)``, for the consumers that enumerate stations rather
    than resolve a trace's own — the map, the distance table, cake, the seismosizer.

    Every station sits in ``_STATIONS`` under two keys, so iterating it directly
    would visit co-located instruments twice. These surfaces name a station without a
    location code and cannot show more than one of them anyway; which one survives is
    the same last-one-added that snuffler's own two-element key keeps.
    """
    return {(key[0], key[1]): value for key, value in _STATIONS.items() if len(key) == 2}


def _sort_distance(net: str, sta: str, loc: str, distances_3d: bool) -> Optional[float]:
    """Distance from the active event to a station in metres.

    Three-dimensional distance folds in the source depth and the station's depth.
    ``None`` means there is no active event or no coordinates for the station.
    """
    if _ACTIVE_EVENT is None:
        return None
    st = _station_at(net, sta, loc)
    if st is None:
        return None
    elat, elon, edep, _et = _ACTIVE_EVENT
    slat, slon, _sele, sdep = st
    # Snuffler sorts on `Station.dist_m`, which is the spheroid distance itself, not
    # a degree value converted back with some radius.
    surface_m = _dist_m(elat, elon, slat, slon)
    if distances_3d:
        # `event.depth - station.depth`, exactly as `set_event_relative_data` forms it
        # (station.py:184). This is deliberately *not* the honest vertical leg: it
        # takes the station's borehole depth and ignores its elevation, so a station
        # 1.7 km up a mountain is measured as though it stood at sea level. The
        # geometrically honest term is `event.depth + elevation - depth`, which
        # snuffler does not compute — and parity with snuffler is what this port
        # claims, so it is not ours to quietly correct here. Fixing it belongs
        # upstream; doing it here would make every distance disagree with the Qt
        # application the port is measured against.
        #
        # `sqrt(dd**2 + ...)` rather than `math.hypot`, which computes the same
        # length more accurately: `set_event_relative_data` uses the plain
        # square-root form, and a distance sitting within an ulp of a 0.5 km
        # rounding boundary (IU.ANMO in the bonin corpus, 10152.5 km) prints a
        # different kilometre under the two.
        dd = float(edep) - float(sdep)
        return math.sqrt(dd**2 + surface_m**2)
    return surface_m


def _fmt_m_float(metres: float) -> str:
    """snuffler's `m_float.__str__` (pile_viewer.py:75): kilometres to 0 decimals
    above 10 km, to 1 below, `%.5g` metres under a kilometre."""
    if abs(metres) >= 10000.0:
        return "%g km" % round(metres / 1000.0, 0)
    if abs(metres) >= 1000.0:
        return "%g km" % round(metres / 1000.0, 1)
    return "%.5g m" % metres


def _sort_annotation(net: str, sta: str, loc: str, sort: str, distances_3d: bool) -> str:
    """The sort-value prefix of snuffler's track label: the stringified ssort
    members of the gather key (`plabel`, pile_viewer.py:3521), in snuffler's own
    formats — `m_float` for distances, `deg_float`'s bare `%4.0f` for azimuths,
    `sector_int`'s `[%i]` plus a distance for the 12-block sort. Empty for the
    name sort and for a station without coordinates, whose `None` members
    snuffler skips out of the label.
    """
    if _ACTIVE_EVENT is None or sort not in (
        "distance",
        "azimuth",
        "azimuth_blocks",
        "backazimuth",
    ):
        return ""
    st = _station_at(net, sta, loc)
    if st is None:
        return ""
    if sort == "distance":
        dist = _sort_distance(net, sta, loc, distances_3d)
        return "" if dist is None else _fmt_m_float(dist)
    from pyrocko import orthodrome as od

    elat, elon = _ACTIVE_EVENT[:2]
    slat, slon = st[0], st[1]
    if sort == "backazimuth":
        return "%4.0f" % float(od.azimuth(slat, slon, elat, elon))
    azi = float(od.azimuth(elat, elon, slat, slon))
    if sort == "azimuth":
        return "%4.0f" % azi
    dist = _sort_distance(net, sta, loc, distances_3d)
    sector = "[%i]" % int(round((azi + 15.0) / 30.0))
    return sector if dist is None else "%s %s" % (sector, _fmt_m_float(dist))


def _sort_order(
    traces: List[ptrace.Trace], sort: str, subsort: str, distances_3d: bool = False
) -> List[int]:
    """Index order for the traces. Distance/azimuth sorts use station coordinates
    (from a station fetch) + the active event; without them they fall back to name
    order. With ``distances_3d`` the distance sort adds the source depth and station
    elevation (snuffler's '3D distances' option). All geo math is pyrocko's."""
    perm = _subsort_perm(subsort)

    def namekey(tr: ptrace.Trace) -> Tuple[str, ...]:
        # The gather key itself, which is what snuffler sorts its rows by
        # (`sorted(keys)` over `set_gathering`'s keys). Three elements wide for the
        # location-grouped subsorts, so the traces sharing a row tie here and the
        # stable sort leaves them in the order they arrived.
        nslc = tr.nslc_id
        return tuple(nslc[i] for i in perm)

    geo = sort in ("distance", "azimuth", "azimuth_blocks", "backazimuth")
    if geo and _ACTIVE_EVENT is not None and _STATIONS:
        from pyrocko import orthodrome as od

        elat, elon, _edep, _et = _ACTIVE_EVENT

        # Mirrors snuffler's gather key exactly (`set_gathering`, pile_viewer.py):
        # the sort-mode value first, then the subsorted nslc as the tie-break —
        # never distance, except inside the azimuthal blocks where snuffler
        # subsorts each sector by distance.
        # Stations without coordinates order last, which is what snuffler's
        # None-aware float subclasses do; a leading flag stands in for that here.
        def geokey(tr: ptrace.Trace) -> Tuple[Any, ...]:
            net, sta, loc = tr.nslc_id[0], tr.nslc_id[1], tr.nslc_id[2]
            st = _station_at(net, sta, loc)
            if st is None:
                return (1, 0.0, 0.0, namekey(tr))
            slat, slon, _sele, _sdep = st
            if sort == "distance":
                # Same helper the track annotation shows, so the order the tracks
                # are in always matches the distance printed on them. It only
                # returns None without coordinates, which this branch excluded.
                return (0, _sort_distance(net, sta, loc, distances_3d), 0.0, namekey(tr))
            if sort == "backazimuth":
                return (0, float(od.azimuth(slat, slon, elat, elon)), 0.0, namekey(tr))
            # In azibazi's own (-180, 180] wrap, so the ring is cut where snuffler
            # cuts it — wrapping to [0, 360) starts the same cyclic order at a
            # different track.
            azi = float(od.azimuth(elat, elon, slat, slon))
            if sort == "azimuth":
                return (0, azi, 0.0, namekey(tr))
            # azimuth_blocks: twelve 30° sectors *centred* on multiples of 30, as
            # snuffler bins them (`sector_int(round((azimuth+15.)/30.))`), with
            # the boundary sector split across ±180 exactly as snuffler splits it
            # (`sector_int` never wraps) and each sector subsorted by distance.
            return (
                0,
                float(round((azi + 15.0) / 30.0)),
                _sort_distance(net, sta, loc, distances_3d),
                namekey(tr),
            )

        return sorted(range(len(traces)), key=lambda i: geokey(traces[i]))

    return sorted(range(len(traces)), key=lambda i: namekey(traces[i]))


def _trace_scaling_key(tr: ptrace.Trace, group: str) -> Any:
    """Grouping key for shared amplitude scaling (snuffler's Scaling menu).

    Asked of the trace, never of the track: a row that groups over location carries
    traces with different keys, and snuffler scales each of them separately.
    """
    if group == "common":
        return None
    if group == "per_station":
        return (tr.network, tr.station)
    if group == "per_station_location":
        return (tr.network, tr.station, tr.location)
    if group == "per_component":
        return tr.channel
    return tr.nslc_id


def _scaling_base_modes(base: str) -> Tuple[Any, str]:
    if base == "robust":
        return "minmax", "robust"
    if base == "mean2":
        return 2.0, "minmax"
    if base == "mean4":
        return 4.0, "minmax"
    return "minmax", "minmax"


def _range_trace(
    nslc: NslcId,
    tmin: float,
    deltat: float,
    ydata: np.ndarray,
) -> ptrace.Trace:
    net, sta, loc, cha = nslc
    return ptrace.Trace(
        network=net,
        station=sta,
        location=loc,
        channel=cha,
        tmin=tmin,
        deltat=deltat,
        ydata=np.asarray(ydata, dtype=np.float64),
    )


def _fft_filter(
    seg: ptrace.Trace,
    hp: Optional[float],
    lp: Optional[float],
    remove_resp: bool = False,
    resp_tmin: float = 0.0,
    resp_tmax: float = 0.0,
) -> None:
    """snuffler's 'FFT filtering' branch (pile_viewer.py:3696): linear-detrend,
    multiply the (pow2-padded) spectrum by one combined transfer function, irfft,
    then retrend.

    The transfer function folds the order-4 Butterworth low/high responses
    together with — when Deconvolution is on — the cosine taper and the inverted
    instrument response, all in a single FFT pass; and in that case the fitted
    trend is deliberately *not* re-added (pile_viewer.py:3794), even when no
    response was found for the channel. detrend/retrend are the OLS-line removal
    those snuffler helpers do (np.polyfit == their linregress).

    The transfer function is cached exactly on snuffler's `tf_cache` key —
    (deltat, nfreqs, nslc-when-deconvolving) — widened by the corners and the
    response generation, which Qt handles by clearing the cache instead."""
    from pyrocko import response

    it = np.arange(seg.data_len(), dtype=float)
    y = np.asarray(seg.ydata, dtype=np.float64)
    coeffs = np.polyfit(it, y, 1)
    slope, offset = float(coeffs[0]), float(coeffs[1])
    seg.set_ydata(y - slope * it - offset)
    freqs, fdata = seg.spectrum(pad_to_pow2=True, tfade=None)
    nfreqs = int(fdata.size)
    key = (
        float(seg.deltat),
        nfreqs,
        tuple(seg.nslc_id) if remove_resp else None,
        _float_key(hp),
        _float_key(lp),
        _RESPONSE_GEN,
    )
    tf = _TF_CACHE.get(key)
    if tf is None:
        resps = []
        if lp is not None:
            resps.append(response.ButterworthResponse(order=4, corner=float(lp), type="low"))
        if hp is not None:
            resps.append(response.ButterworthResponse(order=4, corner=float(hp), type="high"))
        if resps:
            tf = response.MultiplyResponse(resps).evaluate(freqs)
        else:
            tf = np.ones(nfreqs, dtype=complex)
        if remove_resp:
            inst_resp = _get_instrument_response(seg.nslc_id, resp_tmin, resp_tmax)
            if inst_resp is not None:
                f1, f2, f3, f4 = _decon_freq_limits(float(seg.deltat), int(seg.data_len()), hp, lp)
                taper = ptrace.CosTaper(f1, f2, f3, f4)
                taper_vals = np.ones(nfreqs)
                taper(taper_vals, float(freqs[0]), float(freqs[1] - freqs[0]))
                inst_tf = inst_resp.evaluate(freqs)
                with np.errstate(divide="ignore", invalid="ignore"):
                    inv_inst_tf = np.where(np.abs(inst_tf) > 0.0, 1.0 / inst_tf, 0.0)
                tf = tf * taper_vals * inv_inst_tf
        _TF_CACHE[key] = tf
    filtered = np.fft.irfft(fdata * tf)[: seg.data_len()]
    if not remove_resp:
        seg.set_ydata(it * slope + filtered + offset)
    else:
        seg.set_ydata(filtered)


def _filter_seg(
    seg: ptrace.Trace,
    hp: Optional[float],
    lp: Optional[float],
    fft: bool,
    lphp: bool,
    remove_resp: bool = False,
    resp_tmin: float = 0.0,
    resp_tmax: float = 0.0,
) -> None:
    """Apply the main filter to a chopped trace, honoring the FFT-filtering and
    bandpass-as-low+highpass options. Filter corners at/above Nyquist are skipped
    (snuffler's ``< 0.5/deltat`` guards), not left to a bare try/except.

    With FFT filtering on, Deconvolution is folded into the same pass — snuffler
    enters that branch when a corner *or* `remove_resp` is set
    (pile_viewer.py:3699) and skips the separate restitution entirely
    (pile_viewer.py:3840), so the caller must not restitute again.

    `demean=False` on the lowpass/highpass stages, as Qt passes it
    (pile_viewer.py:3821): those pyrocko filters demean by default and never add
    the mean back, so leaving the default would remove a mean of its own even
    with Demean off. The bandpass branch has no such parameter to get wrong.
    A filter that still throws rolls the segment back to its unprocessed
    samples and delivers the failure with the view."""
    original = seg.ydata
    try:
        if fft:
            if hp is not None or lp is not None or remove_resp:
                _fft_filter(seg, hp, lp, remove_resp, resp_tmin, resp_tmax)
            return
        fmax = 0.5 / float(seg.deltat)
        if not lphp and hp is not None and lp is not None and hp < lp < fmax and hp < fmax:
            seg.bandpass(2, float(hp), float(lp))
        else:
            if lp is not None and lp < fmax:
                seg.lowpass(4, float(lp), demean=False)
            if hp is not None and (lp is None or hp < lp) and hp < fmax:
                seg.highpass(4, float(hp), demean=False)
    except Exception as exc:  # noqa: BLE001 — drawn unfiltered, but never silently
        # Roll back to the pre-filter samples: the FFT branch detrends before
        # the transfer function can fail, and the IIR branch can apply the
        # lowpass before the highpass throws — either would ship half-processed
        # data labelled "drawn unfiltered". Every pyrocko filter *replaces*
        # ydata (`astype` copies, lfilter/irfft allocate), so the saved
        # reference is exactly the unprocessed array.
        seg.set_ydata(original)
        _view_notice("filter failed for %s.%s.%s.%s: %s" % (*seg.nslc_id, exc))


def _float_key(value: Optional[float]) -> Optional[float]:
    return None if value is None else round(float(value), 9)


def _adequate_tpad(hp: Optional[float], lp: Optional[float]) -> float:
    """`get_adequate_tpad` (pile_viewer.py:3576) minus the snuffling-hook term,
    which this port does not run: padding for the current filter corners only."""
    tpad = 0.0
    for f in (hp, lp):
        if f is not None:
            tpad = max(tpad, 2.0 / float(f))
    return tpad


def _rotate_pairs_inplace(processed_traces: List[ptrace.Trace], rotate_deg: float) -> None:
    """Rotate horizontal pairs in place, exactly as `prepare_cutout2` does
    (pile_viewer.py:3865): after demean, filtering and restitution, pairing the
    N/E or 1/2 channels of one net.sta.loc whose sampling and start agree within
    tolerance — `deltat` to 0.1 %, `tmin` to 1 % of a sample — at equal length.
    The chopper has already aligned both channels of a real pair, which is what
    makes these tolerances reachable; requiring exact float equality on the raw
    traces silently skipped horizontals that start a fraction of a sample apart.
    """
    phi = rotate_deg / 180.0 * math.pi
    cphi = math.cos(phi)
    sphi = math.sin(phi)
    for a in processed_traces:
        for b in processed_traces:
            if (
                a.network == b.network
                and a.station == b.station
                and a.location == b.location
                and (
                    (a.channel.lower().endswith("n") and b.channel.lower().endswith("e"))
                    or (a.channel.endswith("1") and b.channel.endswith("2"))
                )
                and abs(a.deltat - b.deltat) < a.deltat * 0.001
                and abs(a.tmin - b.tmin) < a.deltat * 0.01
                and len(a.get_ydata()) == len(b.get_ydata())
            ):
                aydata = a.get_ydata() * cphi + b.get_ydata() * sphi
                bydata = -a.get_ydata() * sphi + b.get_ydata() * cphi
                a.set_ydata(aydata)
                b.set_ydata(bydata)


# `accessor_id` for the view's pile access bookkeeping (Qt passes `id(self)`).
_VIEW_ACCESSOR = "view"


def _prepare_cutout(
    tmin: float,
    tmax: float,
    *,
    visible_length: Optional[float],
    npx: int,
    hp: Optional[float],
    lp: Optional[float],
    demean: bool,
    degap: bool,
    fft: bool,
    lphp: bool,
    ads: bool,
    remove_resp: bool,
    rotate: float,
    liberal_fetch: bool,
) -> List[ptrace.Trace]:
    """The traces snuffler would be about to draw for this window: a faithful
    transcription of `PileViewerMain.prepare_cutout2` (pile_viewer.py:3590), with
    the chopping, degapping, downsampling, filtering and restitution all done by
    the same pyrocko code Qt calls.

    Stages, in the reference's order: derive `min_deltat_allow` — the finest
    sampling the window can use, below which channels are not fetched at all
    (`visible_length` is None for this frontend's Adaptive policy, which drops
    that floor entirely and decimates the payload instead);
    optionally snap the fetch window to an absolute power-of-two grid (Liberal
    Fetch); chop a tpad-padded window through `pile.chopper` (which degaps);
    demean each chop, skipping `tabu` traces; filter — one combined FFT pass, or
    the Butterworth stages after the optional anti-aliased `Trace.downsample`
    loop; restitute (unless the FFT pass already folded the response in); rotate
    matching horizontal pairs in place; and finally chop back to the requested
    window plus four samples per side, dropping anything shorter than two
    samples.

    The processing extent is therefore the padded fetch window, not the whole
    trace: the mean Demean removes, the filter transient, the detrend line and
    the deconvolution grid all follow the window exactly as they do in snuffler.

    Cached exactly as Qt caches it (pile_viewer.py:3632): one state vector,
    reused when the new window is contained in the cached one and every other
    member matches.
    """
    global _CUTOUT_VEC, _CUTOUT_TRACES
    p = _pile()
    if p.is_empty():
        return []

    tsee = tmax - tmin
    if visible_length is None:
        # Adaptive: no channel is refused, so there is no floor to derive. The
        # decimation target is the canvas's own — two samples per bin, which is
        # what a min/max envelope emits — and it is what 'Allow Downsampling'
        # aims at here instead of a fixed budget's share of the window.
        min_deltat_allow = 0.0
        min_deltat_wo_decimate = tsee / max(1.0, 2.0 * float(npx))
    else:
        nmax = float(visible_length) if visible_length > 0.0 else 20_000.0
        min_deltat_wo_decimate = tsee / nmax
        min_deltat_w_decimate = min_deltat_wo_decimate / 32.0

        min_deltat_allow = min_deltat_wo_decimate
        if lp is not None:
            target_deltat_lp = 0.25 / float(lp)
            if target_deltat_lp > min_deltat_wo_decimate:
                min_deltat_allow = min_deltat_w_decimate

        # A power of *e*, not of ten — almost certainly an upstream slip for
        # `log10`/`10**`, reproduced deliberately because it is the reference
        # behaviour (see `min-deltat-allow` history in tests/parity).
        min_deltat_allow = math.exp(int(math.floor(math.log(min_deltat_allow))))

    tmin_ = tmin
    tmax_ = tmax
    if liberal_fetch:
        tlen = float(ptrace.nextpow2((tmax - tmin) * 1.5))
        tmin = math.floor(tmin / tlen) * tlen
        tmax = math.ceil(tmax / tlen) * tlen

    tpad = min(_adequate_tpad(hp, lp), tsee * 3.0)

    # The anti-alias downsample target only belongs in the key where the loop
    # below actually runs: under Adaptive it follows `npx`, and keying on it
    # unconditionally would throw the processed traces away on every resize.
    decimate_ran = ads and lp is not None and not fft
    decimate_key = _float_key(min_deltat_wo_decimate) if decimate_ran else None

    vec = (
        tmin,
        tmax,
        tpad,
        tuple(_HIDDEN),
        None if _REDUCE is None else tuple(_REDUCE),
        degap,
        demean,
        _float_key(lp),
        _float_key(hp),
        fft,
        lphp,
        min_deltat_allow,
        decimate_key,
        _float_key(rotate),
        ads,
        remove_resp,
        _DATA_GEN,
        _RESPONSE_GEN,
    )

    if (
        _CUTOUT_VEC is not None
        and _CUTOUT_TRACES is not None
        and _CUTOUT_VEC[0] <= vec[0]
        and vec[1] <= _CUTOUT_VEC[1]
        and vec[2:] == _CUTOUT_VEC[2:]
    ):
        processed_traces = _CUTOUT_TRACES
        # The cached data still carries whatever went wrong while it was
        # built; a pan that hits the cache must warn like the view that
        # missed it.
        for message in _CUTOUT_NOTICES:
            _view_notice(message)
    else:
        # A fresh processing pass warns once per channel again: the previous
        # pass's dedup must not silence a condition that still holds.
        _WARNED_NO_RESPONSE.clear()
        notices_before = len(_VIEW_NOTICES)
        processed_traces = []
        if p.deltatmax is not None and p.deltatmax >= min_deltat_allow:

            def group_selector(gr: Any) -> bool:
                return bool(gr.deltatmax >= min_deltat_allow)

            def trace_selectorx(tr: ptrace.Trace) -> bool:
                return tr.deltat >= min_deltat_allow and _trace_selected(tr)

            for traces in p.chopper(
                tmin=tmin,
                tmax=tmax,
                tpad=tpad,
                want_incomplete=True,
                degap=degap,
                maxgap=_GAP_LAP_TOLERANCE,
                maxlap=_GAP_LAP_TOLERANCE,
                keep_current_files_open=True,
                group_selector=group_selector,
                trace_selector=trace_selectorx,
                accessor_id=_VIEW_ACCESSOR,
                snap=(math.floor, math.ceil),
                include_last=True,
            ):
                if demean:
                    for tr in traces:
                        if tr.meta and tr.meta.get("tabu", False):
                            continue
                        y = tr.get_ydata()
                        tr.set_ydata(y - np.mean(y))

                for tr in traces:
                    if not (tr.meta and tr.meta.get("tabu", False)):
                        if fft:
                            _filter_seg(tr, hp, lp, True, lphp, remove_resp, tmin, tmax)
                        else:
                            if ads and lp is not None:
                                while tr.deltat < min_deltat_wo_decimate:
                                    tr.downsample(2, demean=False)
                            _filter_seg(tr, hp, lp, False, lphp)
                    processed_traces.append(tr)

                if remove_resp and not fft:
                    processed_traces = [
                        _remove_response(tr, True, hp, lp, tmin, tmax) for tr in processed_traces
                    ]

        if rotate != 0.0:
            _rotate_pairs_inplace(processed_traces, rotate)

        _CUTOUT_VEC = vec
        _CUTOUT_TRACES = processed_traces
        _CUTOUT_NOTICES[:] = _VIEW_NOTICES[notices_before:]

    chopped_traces = []
    for tr in processed_traces:
        chop_tmin = tmin_ - tr.deltat * 4
        chop_tmax = tmax_ + tr.deltat * 4
        try:
            ctrace = tr.chop(chop_tmin, chop_tmax, inplace=False)
        except ptrace.NoData:
            continue
        # Qt drops any chopped trace shorter than two samples
        # (pile_viewer.py:3905): a one-sample overlap becomes nothing, not a row.
        if ctrace.data_len() < 2:
            continue
        chopped_traces.append(ctrace)

    return chopped_traces


def _slice_segment(
    entry: Tuple[float, float, np.ndarray],
    tmin: float,
    tmax: float,
    sample_support: float = 0.0,
) -> Optional[Tuple[np.ndarray, float, float, int]]:
    seg_tmin, deltat, y = entry
    pad = max(0.0, float(sample_support)) * deltat
    start = max(0, int(math.floor((tmin - pad - seg_tmin) / deltat)))
    stop = min(len(y), int(math.ceil((tmax + pad - seg_tmin) / deltat)) + 1)
    if stop <= start:
        return None
    return y[start:stop], seg_tmin + start * deltat, deltat, start


def _stable_bin_stride(samples_per_bin: float) -> int:
    """Round a bin width up onto a quarter-octave ladder.

    The envelope below bins on an absolute sample grid so that panning slides the
    trace instead of reshuffling which samples are extrema. That only holds while
    the stride itself holds still, and a stride taken straight from the window
    would change with every pixel of zoom. Quantising to powers of 2**0.25 keeps
    it constant across small zooms and never more than 19 % coarser than asked.
    """
    stride = max(1, int(math.ceil(samples_per_bin)))
    if stride <= 1:
        return 1
    value = 1.0
    while value < stride:
        value *= 2.0**0.25
    return max(1, int(math.ceil(value)))


def _display_envelope(
    y: np.ndarray, deltat: float, t0: float, bins: int
) -> Tuple[np.ndarray, float, float]:
    """Reduce a slice to about ``bins`` bins, as the min and max of each.

    This is what the Adaptive visible length spends instead of refusing channels:
    every channel is fetched at its own rate, and the payload is cut down here to
    the resolution the canvas can draw. Two samples per bin — the bin's extremes,
    in the order they occur — emitted at half the bin width, so the run stays a
    uniform span (the renderer draws nothing else) and the peak inside each bin
    survives, which plain subsampling would drop and an anti-alias decimation
    would smooth away.

    Bins are anchored to the trace's absolute sample grid rather than to the
    slice, so a pan moves the trace without changing which sample is a peak.
    Up to one bin at each end falls outside the grid and is dropped; the slice
    carries four samples of margin for exactly that kind of trim.

    Returns the payload with its new sample spacing and start time, or the input
    unchanged when it is already at or below the target.
    """
    n = int(y.size)
    if bins <= 0 or n <= 2 * bins:
        return y, deltat, t0
    stride = _stable_bin_stride(n / float(bins))
    if stride < 2:
        return y, deltat, t0
    # Which absolute sample the slice starts on; deltat divides the trace's own
    # tmin exactly, so this is the index pyrocko would give it.
    base = int(math.floor(t0 / deltat + 0.5))
    first_bin = -(-base // stride)
    last_bin = (base + n) // stride
    nbins = int(last_bin - first_bin)
    if nbins < 2:
        return y, deltat, t0
    start = int(first_bin * stride - base)
    block = np.asarray(y[start : start + nbins * stride], dtype=np.float64).reshape(nbins, stride)
    lo = block.min(axis=1)
    hi = block.max(axis=1)
    rising = block.argmin(axis=1) <= block.argmax(axis=1)
    out = np.empty(nbins * 2, dtype=np.float64)
    out[0::2] = np.where(rising, lo, hi)
    out[1::2] = np.where(rising, hi, lo)
    return out, deltat * stride * 0.5, t0 + start * deltat


def _waterfall_row_strips(
    slices: "List[Tuple[np.ndarray, float, float]]",
    npx: int,
    want_abs: bool,
    want_integrate: bool,
    color_scale: str = "lin",
) -> "List[Tuple[np.ndarray, float, float]]":
    """Snuffler-style waterfall strips for one track row: normalized amplitude
    over time, on one shared amplitude scale and one shared time resolution.

    Snuffler normalises per image row, and a row there is a whole trace
    (``pile_viewer_waterfall.py:148``; ``_common_scale`` is always on). This
    port's row is a *track*, which can carry several segments, so the peak is
    taken across every strip of the row before dividing — two pieces of a gapped
    channel share one colour scale, as they share one labelled row. The
    decimation stride is derived once per row for the same reason: both pieces
    keep one time resolution.

    Takes ``(yview, t0, deltat)`` per segment; returns ``(strip, t0, dt)`` per
    segment, strips as little-endian float32. ``color_scale`` (``lin`` | ``sqrt``
    | ``log``) compresses the envelope before colour-mapping — a frontend
    addition, linear by default, which is all snuffler has.
    """
    from scipy import signal as sps

    if not slices:
        return []
    # One target resolution for the row: the coarsest any piece would have
    # chosen on its own, so no piece is stretched past its own sampling.
    target_dt = max(
        float(deltat) * max(1, int(np.asarray(y).size // max(1, 2 * int(npx))))
        for y, _t0, deltat in slices
    )
    prepared: "List[Tuple[np.ndarray, float, float]]" = []
    for y, t0, deltat in slices:
        step = max(1, int(round(target_dt / float(deltat))))
        data = np.ascontiguousarray(np.asarray(y, dtype=np.float64)[::step])
        if want_integrate:
            data = np.cumsum(data) * float(deltat)
        prepared.append((data, float(t0), float(deltat) * step))

    peak = max(
        (float(np.abs(data).max()) if data.size else 0.0 for data, _t0, _dt in prepared),
        default=0.0,
    )
    out: "List[Tuple[np.ndarray, float, float]]" = []
    for data, t0, dt in prepared:
        if peak > 0.0:
            data = data / peak
        if want_abs and data.size:
            hilbert: Callable[[np.ndarray], np.ndarray] = getattr(sps, "hilbert")
            data = np.abs(hilbert(data))
            if color_scale == "sqrt":
                data = np.sqrt(data)
            elif color_scale == "log":
                # map [0, 1] -> [0, 1] with log compression (log10(1)=0, log10(10)=1).
                data = np.log10(data * 9.0 + 1.0)
        out.append((np.ascontiguousarray(data, dtype="<f4"), t0, dt))
    return out


_WF_LUTS: Dict[str, np.ndarray] = {}


def _wf_colormap_lut(name: str) -> np.ndarray:
    """256x3 uint8 RGB lookup table for a waterfall colormap, sampled from
    matplotlib so the colours are byte-identical to the Qt snuffler (which colours
    its waterfall with the same matplotlib colormaps). Cached per name."""
    key = name or "viridis"
    lut = _WF_LUTS.get(key)
    if lut is None:
        import matplotlib as mpl

        try:
            cmap = mpl.colormaps[key]
        except KeyError:
            cmap = mpl.colormaps["viridis"]
        xs = np.linspace(0.0, 1.0, 256)
        rgb = np.asarray(cmap(xs))[:, :3] * 255.0
        lut = np.ascontiguousarray(rgb.round().clip(0.0, 255.0).astype(np.uint8))
        _WF_LUTS[key] = lut
    return lut


def _wf_rgba(
    strip: np.ndarray,
    lo: float,
    hi: float,
    clip_lo: float,
    clip_hi: float,
    reverse: bool,
    lut: np.ndarray,
) -> np.ndarray:
    """Colour-map one waterfall amplitude strip to RGBA, packed one pixel per
    float32 (its 4 little-endian bytes ARE the RGBA) so it rides the same
    zero-copy ``ys`` buffer; the frontend just reinterprets and blits. Mirrors the
    former Rust normalisation exactly: clip the ``[lo, hi]`` band by
    ``clip_lo/clip_hi``, optional reverse, then LUT lookup. Symmetry is not
    handled here: in snuffler it constrains the clip *pair* (`set_symetry`,  # spellcheck:ignore-line
    gui/util.py:817), which the frontend enforces on the sliders themselves."""
    span = max(hi - lo, 1e-6)
    band_lo = lo + clip_lo * span
    band_hi = lo + clip_hi * span
    rng = max(band_hi - band_lo, 1e-6)
    tn = np.clip((strip.astype(np.float64) - band_lo) / rng, 0.0, 1.0)
    if reverse:
        tn = 1.0 - tn
    # matplotlib indexes its 256-entry LUT with int(t * 256) clipped to 255;
    # * 255 picked every interior value's neighbour.
    idx = np.minimum((tn * 256.0).astype(np.int64), 255)
    rgba = np.empty((idx.size, 4), dtype=np.uint8)
    rgba[:, :3] = lut[idx]
    rgba[:, 3] = 255
    return rgba.reshape(-1).view("<f4")


def _colorize_waterfall(meta: view_pb2.ViewMeta, ys: np.ndarray, req: view_pb2.ViewParams) -> None:
    """Replace each waterfall track's amplitude strip in ``ys`` with matplotlib
    RGBA (packed as float32), in place. Runs after the global min/max is fixed so
    every strip shares the same colour scale."""
    lut = _wf_colormap_lut(req.wf_colormap)
    clip_lo = float(req.wf_clip_lo)
    clip_hi = float(req.wf_clip_hi)
    if not clip_hi > clip_lo:
        clip_lo, clip_hi = 0.0, 1.0
    # Symmetry is a constraint the frontend applies to the clip pair (snuffler's
    # `set_symetry`), so by the time the band arrives here it is already  # spellcheck:ignore-line
    # symmetric when it should be; no colour-side handling remains.
    reverse = bool(req.wf_reverse)
    for t in meta.tracks:
        if not t.nf:
            continue
        for span in t.spans:
            off = int(span.offset)
            cnt = int(span.count)
            strip = ys[off : off + cnt]
            if not cnt or strip.size != cnt:
                continue
            ys[off : off + cnt] = _wf_rgba(
                strip,
                float(span.ymin),
                float(span.ymax),
                clip_lo,
                clip_hi,
                reverse,
                lut,
            )


def _maybe_log_view_perf(
    mode: str,
    tmin: float,
    tmax: float,
    npx: int,
    track_offset: int,
    track_count: int,
    total_tracks: int,
    wanted_tracks: int,
    sample_tracks: int,
    y_floats: int,
    t0: float,
    t_visible: float,
    t_prepare: float,
    t_loop: float,
    t_scale: float,
    t_concat: float,
    t_done: float,
) -> None:
    global _LAST_VIEW_PERF_LOG_S
    total_ms = (t_done - t0) * 1000.0
    now = time.monotonic()
    if total_ms < _VIEW_PERF_SLOW_MS and now - _LAST_VIEW_PERF_LOG_S < _VIEW_PERF_LOG_INTERVAL_S:
        return
    _LAST_VIEW_PERF_LOG_S = now
    print(
        "[py.view] "
        f"mode={mode} window_s={tmax - tmin:.3f} npx={npx} "
        f"track_page={track_offset}..{track_offset + track_count} "
        f"total_tracks={total_tracks} wanted_tracks={wanted_tracks} "
        f"sample_tracks={sample_tracks} ys_floats={y_floats} "
        f"ys_mb={y_floats * 4.0 / 1_000_000.0:.3f} "
        f"total_ms={total_ms:.2f} visible_ms={(t_visible - t0) * 1000.0:.2f} "
        f"prep_ms={(t_prepare - t_visible) * 1000.0:.2f} "
        f"loop_ms={(t_loop - t_prepare) * 1000.0:.2f} "
        f"scale_ms={(t_scale - t_loop) * 1000.0:.2f} "
        f"concat_ms={(t_concat - t_scale) * 1000.0:.2f}"
    )


def _gather_sort_value(tr: ptrace.Trace, sort: str, distances_3d: bool) -> Tuple[Any, ...]:
    """The sort-mode value snuffler prepends to a track's gather key.

    ``set_gathering`` builds the key as ``ssort(tr) + <subsorted nslc>``
    (pile_viewer.py:1741), so the value a geo sort orders by is *part of the row's
    identity*, not just its position. Two traces the subsort would merge onto one
    row are pulled apart whenever they disagree here — which is how a station whose
    instruments sit at different depths comes apart under a 3D distance sort, having
    tied under every other sort. Sorting by name prepends nothing (``lambda tr: ()``)
    and nothing is ever split.

    ``None`` for a trace with no station, matching the ``default_getter`` each geo
    entry passes; traces that share that fate share a row, as they do in snuffler.
    """
    if sort not in ("distance", "azimuth", "azimuth_blocks", "backazimuth"):
        return ()
    net, sta, loc = tr.nslc_id[0], tr.nslc_id[1], tr.nslc_id[2]
    st = _station_at(net, sta, loc)
    if sort == "distance":
        return (_sort_distance(net, sta, loc, distances_3d),)
    if st is None:
        return (None, None) if sort == "azimuth_blocks" else (None,)
    from pyrocko import orthodrome as od

    elat, elon = (_ACTIVE_EVENT or (0.0, 0.0, 0.0, 0.0))[:2]
    slat, slon = st[0], st[1]
    if sort == "backazimuth":
        return (float(od.azimuth(slat, slon, elat, elon)),)
    azi = float(od.azimuth(elat, elon, slat, slon))
    if sort == "azimuth":
        return (azi,)
    return (float(round((azi + 15.0) / 30.0)), _sort_distance(net, sta, loc, distances_3d))


def _gather_key(
    tr: ptrace.Trace, perm: Tuple[int, ...], sort: str, distances_3d: bool
) -> Tuple[Any, ...]:
    """A trace's row identity: the sort value followed by the subsort permutation
    of the nslc id, exactly snuffler's gather key (`set_gathering`,
    pile_viewer.py:1741)."""
    return _gather_sort_value(tr, sort, distances_3d) + tuple(tr.nslc_id[i] for i in perm)


def _row_universe(
    sort: str, subsort: str, distances_3d: bool
) -> List[Tuple[Tuple[Any, ...], ptrace.Trace]]:
    """The track rows of the whole pile, in draw order.

    Snuffler's stack is one row per gather key of *every* blacklist-selected trace
    (`set_gathering` keys the pile, pile_viewer.py:1750), whether or not it has
    data in the window — a channel with no data on screen keeps its labelled empty
    row, so panning across a gap cannot reshuffle the stack. Rows are ordered by
    the same key the traces sort by; each carries a representative trace for the
    label and the annotation.
    """
    selected = _visible_traces()
    order = _sort_order(selected, sort, subsort, distances_3d)
    perm = _subsort_perm(subsort)
    rows: "OrderedDict[Tuple[Any, ...], ptrace.Trace]" = OrderedDict()
    for i in order:
        tr = selected[i]
        rows.setdefault(_gather_key(tr, perm, sort, distances_3d), tr)
    return list(rows.items())


def _pile_color_keys(subsort: str) -> List[str]:
    """Snuffler's `color_keys` (`pile.gather_keys(color)`, pile_viewer.py:1753):
    the distinct values of the subsort's coloring function over the whole pile —
    no blacklist, no window — in sorted order. The trace palette numbers over this
    list, so a key keeps its colour as the view scrolls; numbering over the
    visible slice instead recolours the same station on every scroll, which
    snuffler cannot do.

    The per-subsort coloring functions are the second halves of snuffler's subsort
    `menudef` (pile_viewer.py:1020): location for NSLC and the two grouped-by-
    location modes, channel for NSCL/SNCL/LNSC, and the net+sta+loc composite for
    CNSL — dot-joined here, which sorts identically to snuffler's tuple since
    the codes cannot contain dots.
    """

    def color(tr: ptrace.Trace) -> str:
        if subsort in ("nslc", "nsc_l", "snc_l"):
            return tr.nslc_id[2]
        if subsort == "cnsl":
            return "%s.%s.%s" % (tr.nslc_id[0], tr.nslc_id[1], tr.nslc_id[2])
        return tr.nslc_id[3]

    return sorted({color(tr) for tr in _TRACES})


def _box_styles() -> Dict[Tuple[Any, ...], int]:
    """Snuffler's `determine_box_styles` (pile_viewer.py:2947): walk the pile's
    traces in `full_id` order and step the style index at every discontinuity in
    (nslc, deltat, adjacency), so each contiguous run of files shares a tint and
    neighbouring runs alternate. Keyed per (full_id, deltat) exactly as Qt keys
    `trace_styles`; cached until the pile changes, as Qt recomputes only on a
    pile change."""
    global _BOX_STYLES, _BOX_STYLES_GEN
    if _BOX_STYLES_GEN == _DATA_GEN:
        return _BOX_STYLES
    traces = sorted(_TRACES, key=lambda tr: tr.full_id)
    istyle = 0
    styles: Dict[Tuple[Any, ...], int] = {}
    previous: Optional[ptrace.Trace] = None
    for tr in traces:
        if previous is not None and not (
            previous.nslc_id == tr.nslc_id
            and previous.deltat == tr.deltat
            and abs(previous.tmax - tr.tmin) < _GAP_LAP_TOLERANCE * tr.deltat
        ):
            istyle += 1
        styles[(tr.full_id, tr.deltat)] = istyle
        previous = tr
    _BOX_STYLES = styles
    _BOX_STYLES_GEN = _DATA_GEN
    return styles


def _trace_boxes_by_row(
    tmin: float,
    tmax: float,
    row_index: Dict[Tuple[Any, ...], int],
    perm: Tuple[int, ...],
    sort: str,
    distances_3d: bool,
) -> Dict[int, List[Tuple[float, float, int]]]:
    """Snuffler's trace boxes (`draw_trace_boxes`, pile_viewer.py:2968): the
    pile's data coverage per row over the view window.

    Sourced from the pile rather than the fetched cutout, which is the property
    that matters: a channel `min_deltat_allow` declined to fetch still reports
    where its data lies, so a view zoomed far beyond the data keeps an anchor to
    navigate back to. One box per overlapping trace, or — above Qt's 500-trace
    threshold — one per (row, style) batch spanning first-appended `tmin` to
    last-appended `tmax` in `full_id` order, quirks and all."""
    styles = _box_styles()
    overlapping = [tr for tr in _TRACES if tr.overlaps(tmin, tmax) and _trace_selected(tr)]
    overlapping.sort(key=lambda tr: tr.full_id)
    boxes: Dict[int, List[Tuple[float, float, int]]] = {}
    if len(overlapping) < 500:
        for tr in overlapping:
            row = row_index.get(_gather_key(tr, perm, sort, distances_3d))
            if row is None:
                continue
            style = styles.get((tr.full_id, tr.deltat), 0)
            boxes.setdefault(row, []).append((float(tr.tmin), float(tr.tmax), style))
    else:
        merged: "OrderedDict[Tuple[int, int], Tuple[float, float]]" = OrderedDict()
        for tr in overlapping:
            row = row_index.get(_gather_key(tr, perm, sort, distances_3d))
            if row is None:
                continue
            style = styles.get((tr.full_id, tr.deltat), 0)
            bounds = merged.get((row, style))
            merged[(row, style)] = (
                (float(tr.tmin), float(tr.tmax)) if bounds is None else (bounds[0], float(tr.tmax))
            )
        for (row, style), (btmin, btmax) in merged.items():
            boxes.setdefault(row, []).append((btmin, btmax, style))
    return boxes


class _SpanBuild(NamedTuple):
    """One run of samples on its way into a ``ViewSpan``, with what produced it.

    The source trace rides along because the scale is settled after the whole stack
    is known — a span is scaled by *its own* trace's scaling key, which on a
    location-grouped row is not the row's.
    """

    t0_rel: float
    dt: float
    npoints: int
    payload: np.ndarray
    source: ptrace.Trace
    # Waterfall only: the strip's own amplitude range, before the global pass.
    wf_range: Optional[Tuple[float, float]] = None


def _get_view_data(req: view_pb2.ViewParams) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
    """Compute the requested view (wiggle waveforms or a Qt-style waterfall).

    Returns (ViewMeta, ys).
    For wiggles `ys` holds uniform samples at the native or anti-alias-decimated
    rate — or, under the Adaptive visible length, at the bin rate of the min/max
    envelope `_display_envelope` reduced them to.
    For the waterfall it holds per-track normalized amplitude strips for the
    requested track page.
    """
    perf_t0 = time.monotonic()
    tmin = float(req.tmin)
    tmax = float(req.tmax)
    visible_tmin = float(req.visible_tmin) if req.HasField("visible_tmin") else tmin
    visible_tmax = float(req.visible_tmax) if req.HasField("visible_tmax") else tmax
    scale_tmin = float(req.scale_tmin) if req.HasField("scale_tmin") else visible_tmin
    scale_tmax = float(req.scale_tmax) if req.HasField("scale_tmax") else visible_tmax
    npx = int(req.npx)
    track_offset = max(0, int(req.track_offset))
    track_count = max(0, int(req.track_count))
    track_end = track_offset + track_count
    hp: Optional[float] = req.highpass if req.HasField("highpass") else None
    lp: Optional[float] = req.lowpass if req.HasField("lowpass") else None
    sort = req.sort or "name"
    subsort = req.subsort or "nslc"
    rotate = float(req.rotate)
    remove_response = bool(req.remove_response)
    mode = req.mode or "wiggle"
    demean = bool(req.demean)
    degap = bool(req.degap)
    allow_ds = bool(req.allow_downsampling)
    fft = bool(req.fft_filtering)
    lphp = bool(req.bandpass_lphp)
    base = req.scaling_base or "minmax"
    scaling_group = req.scaling_group or "individual"
    gain = float(req.gain or 1.0)
    if gain <= 0.0 or not math.isfinite(gain):
        gain = 1.0
    fix_scale_ranges = bool(req.fix_scale_ranges)
    if (
        fix_scale_ranges
        or scale_tmax <= scale_tmin
        or not math.isfinite(scale_tmin)
        or not math.isfinite(scale_tmax)
    ):
        scale_tmin, scale_tmax = visible_tmin, visible_tmax
    show_scale_ranges = bool(req.show_scale_ranges)
    show_scale_axes = bool(req.show_scale_axes)
    wf_abs = bool(req.wf_abs)
    wf_integrate = bool(req.wf_integrate)
    wf_color_scale = req.wf_color_scale or "lin"
    # None is Adaptive: no fetch budget at all, decimated to `npx` on the way out.
    adaptive_visible_length = bool(req.adaptive_visible_length)
    visible_length: Optional[float] = None
    if not adaptive_visible_length:
        visible_length = float(req.visible_length) if req.HasField("visible_length") else 20_000.0
    liberal_fetch = bool(req.liberal_fetch)

    _VIEW_NOTICES.clear()
    traces = _prepare_cutout(
        tmin,
        tmax,
        visible_length=visible_length,
        npx=npx,
        hp=hp,
        lp=lp,
        demean=demean,
        degap=degap,
        fft=fft,
        lphp=lphp,
        ads=allow_ds,
        remove_resp=remove_response,
        rotate=rotate,
        liberal_fetch=liberal_fetch,
    )
    perf_visible = time.monotonic()
    distances_3d = bool(req.distances_3d)
    perm = _subsort_perm(subsort)
    # The row stack comes from the whole pile, not from what the cutout returned:
    # a channel with no data in the window (or one min_deltat_allow declined to
    # fetch) keeps its labelled, empty row exactly as it does under snuffler.
    rows = _row_universe(sort, subsort, distances_3d)
    row_index = {key: idx for idx, (key, _head) in enumerate(rows)}
    members_by_row: List[List[Tuple[int, ptrace.Trace]]] = [[] for _ in rows]
    for index, tr in enumerate(traces):
        row = row_index.get(_gather_key(tr, perm, sort, distances_3d))
        if row is not None:
            members_by_row[row].append((index, tr))
    # Pile-backed data coverage per row, drawn as Qt's trace boxes. The painter
    # gates boxes on view mode and the Show Boxes option, as Qt's drawit does.
    boxes_by_row: Dict[int, List[Tuple[float, float, int]]] = (
        {}
        if mode == "waterfall"
        else _trace_boxes_by_row(tmin, tmax, row_index, perm, sort, distances_3d)
    )
    perf_prepare = time.monotonic()

    meta = view_pb2.ViewMeta(
        view_tmin=tmin,
        view_tmax=tmax,
        session_generation=_SESSION_STORE.generation,
        view_sequence=req.view_sequence,
    )
    meta.color_keys.extend(_pile_color_keys(subsort))
    ys_parts: List[np.ndarray] = []
    range_traces: List[ptrace.Trace] = []
    data_ranges: Dict[Any, Tuple[float, float]] = {}
    # Every emitted span with the trace behind it, so the scaling pass below can key
    # each one on its own trace rather than on the row it happens to share.
    scaled_spans: List[Tuple[view_pb2.ViewSpan, ptrace.Trace]] = []
    fast_individual_minmax = base == "minmax" and scaling_group == "individual"
    offset = 0
    view_idx = 0
    wanted_tracks = 0
    sample_tracks = 0
    for row_idx, (_row_key, head) in enumerate(rows):
        # Time order within a row, with the nslc id as a tiebreak: two traces can
        # start at the same instant when the row groups several channels together,
        # and a stack that reshuffles between identical requests is worse than
        # either order.
        members = sorted(members_by_row[row_idx], key=lambda item: (item[1].tmin, item[1].nslc_id))
        wanted_tracks += 1
        # A row is named by what it groups on: an element the rows merge over drops
        # out of the label and becomes '*' in the identity rather than showing one
        # member's value. Both are snuffler's own representations — its track label
        # is the gather key, and its `track_patterns` hold '*' for the merged
        # element, which is what keeps a marker matched to its row even when the
        # marker's own location has no data in the window.
        net, sta, loc, cha = (head.nslc_id[i] if i in perm else "*" for i in range(4))
        # The label is snuffler's: the gather key space-joined, sort value first,
        # nslc elements in the subsort's permuted order, `None`s skipped and the
        # empty location kept (`plabel`, pile_viewer.py:3521) — so the subsort is
        # visible in the label and a distance-sorted row reads "123 km XX STA01
        # HHZ" here as it does there.
        nslc_part = " ".join(head.nslc_id[i] for i in perm)
        label = nslc_part
        # The head's own location, not the row identity's '*': a row that merges over
        # locations still holds instruments at one distance, because the gather key
        # carries that distance. Reading it off '*' would fall back to whichever
        # location the station table filed last and mislabel one of the split rows.
        annotation = _sort_annotation(
            head.nslc_id[0], head.nslc_id[1], head.nslc_id[2], sort, bool(req.distances_3d)
        )
        if annotation:
            label = "%s %s" % (annotation, nslc_part)
        want_samples = track_offset <= view_idx < track_end
        if want_samples:
            sample_tracks += 1
        view_idx += 1

        spans: List[_SpanBuild] = []
        nf = 0
        fmax = 0.0
        wf_slices: "List[Tuple[np.ndarray, float, float, ptrace.Trace]]" = []
        for _tr_idx, tr in members:
            # Data and scale ranges for the requested track page only: snuffler's
            # trace_selector restricts the chopper to `shown_tracks_range`
            # (pile_viewer.py:1798), so its group scales range over the shown
            # rows — scrolling the stack changes a common scale there, and now
            # here too.
            if not want_samples:
                continue
            # `tr` is already processed and chopped to the requested window plus
            # four samples per side — exactly the trace snuffler is about to draw.
            if tr.ydata is None or not len(tr.ydata):
                continue
            entry = (
                float(tr.tmin),
                float(tr.deltat),
                np.asarray(tr.get_ydata(), dtype=np.float64),
            )

            if mode == "waterfall":
                sliced = _slice_segment(entry, tmin, tmax, 4.0)
                if sliced is None:
                    continue
                yview, ytmin, deltat, _sample_offset = sliced
                # Collected per row and normalised together below, so every
                # segment of the row shares one colour scale and one stride.
                wf_slices.append((yview, float(ytmin), float(deltat), tr))
                continue

            data_sliced = _slice_segment(entry, tmin, tmax, 4.0)
            # Four samples of margin on the scale slice too: snuffler's minmax
            # runs over the final chopped traces, which the chop widened by four
            # samples at each end (pile_viewer.py:3894).
            scale_sliced = _slice_segment(entry, scale_tmin, scale_tmax, 4.0)
            if scale_sliced is not None:
                yrange, yrmin, rdt, _range_offset = scale_sliced
                if fast_individual_minmax:
                    # Merged, not overwritten: the segments of one channel share a
                    # scaling key, so the range has to span all of them.
                    # nanmin/nanmax because `trace.minmax` uses them
                    # (trace.py:2549): Qt ignores a NaN sample and produces a
                    # real range, so the fast path must too.
                    lo, hi = float(np.nanmin(yrange)), float(np.nanmax(yrange))
                    previous = data_ranges.get(tr.nslc_id)
                    data_ranges[tr.nslc_id] = (
                        (lo, hi)
                        if previous is None
                        else (min(previous[0], lo), max(previous[1], hi))
                    )
                else:
                    range_traces.append(_range_trace(tr.nslc_id, yrmin, rdt, yrange))
            if data_sliced is None:
                continue
            yview, ytmin, deltat, _sample_offset = data_sliced
            # Under a fixed visible length the payload is the samples themselves,
            # always: with Allow Downsampling on the data was already anti-alias
            # decimated by `Trace.downsample` inside the cutout, exactly as Qt
            # decimates — there is no separate display envelope to substitute for
            # it. Adaptive is the case that needs one, because nothing bounded
            # what was fetched.
            if adaptive_visible_length:
                yview, deltat, ytmin = _display_envelope(yview, deltat, ytmin, npx)
            if yview.size:
                spans.append(
                    _SpanBuild(ytmin - tmin, deltat, int(yview.size), yview.astype("<f4"), tr)
                )

        if wf_slices:
            strips = _waterfall_row_strips(
                [(y, t0, dt) for y, t0, dt, _tr in wf_slices],
                npx,
                wf_abs,
                wf_integrate,
                wf_color_scale,
            )
            for (strip, t0, dt), (_y, _t0, _dt, tr) in zip(strips, wf_slices, strict=True):
                if not strip.size:
                    continue
                nf = 1
                wf_range = (float(strip.min()), float(strip.max())) if wf_abs else None
                spans.append(_SpanBuild(t0 - tmin, dt, int(strip.size), strip, tr, wf_range))
        # In waterfall mode a row without an image strip is dropped; a wiggle row
        # is kept even with no spans — snuffler's stack is one labelled row per
        # pile gather key, occupied or not.
        if mode == "waterfall" and not spans:
            continue
        t = meta.tracks.add()
        t.net, t.sta, t.loc, t.cha = net, sta, loc, cha
        t.label = label
        t.sort_annotation = annotation
        # Boxes only for the requested track page, like the spans: rows outside
        # it are stack placeholders the painter never shows.
        if want_samples:
            for btmin, btmax, bstyle in boxes_by_row.get(row_idx, []):
                box = t.boxes.add()
                box.tmin = btmin
                box.tmax = btmax
                box.style = bstyle
        if mode == "waterfall":
            t.nf = int(nf)
            t.fmax = fmax
        for build in spans:
            span = t.spans.add()
            span.t0_rel = build.t0_rel
            span.dt = build.dt
            span.count = build.npoints
            span.offset = int(offset)
            span.loc = build.source.nslc_id[2]
            span.ymax = 1.0
            span.yscale = 1.0
            if mode == "waterfall":
                span.ymin, span.ymax = build.wf_range or ((0.0, 1.0) if wf_abs else (-1.0, 1.0))
            scaled_spans.append((span, build.source))
            ys_parts.append(np.ascontiguousarray(build.payload, dtype="<f4"))
            offset += int(build.payload.size)

    perf_loop = time.monotonic()
    if mode == "waterfall":
        if wf_abs:
            lo = min((span.ymin for span, _ in scaled_spans), default=0.0)
            hi = max((span.ymax for span, _ in scaled_spans), default=1.0)
            if hi <= lo:
                hi = lo + 1e-12
            for span, _source in scaled_spans:
                span.ymin = lo
                span.ymax = hi
    elif range_traces or data_ranges:
        if range_traces:
            mode_param, outer_mode = _scaling_base_modes(base)

            def scaling_key(tr: ptrace.Trace) -> Any:
                return _trace_scaling_key(tr, scaling_group)

            trace_minmax = cast(TraceRangeFunc, getattr(ptrace, "minmax"))
            raw_ranges = trace_minmax(
                range_traces,
                key=scaling_key,
                mode=mode_param,
                outer_mode=outer_mode,
            )
            data_ranges.update(
                {key: (float(lo), float(hi)) for key, (lo, hi) in raw_ranges.items()}
            )
        # Exactly snuffler's freeze (pile_viewer.py:3281): remember the live
        # ranges while the option is off, serve the remembered ones while it is
        # on, with no notion of what produced them — they survive filter, sort
        # and pile changes, which is what makes "freeze, change the passband,
        # compare amplitudes" work. One deliberate improvement: a *copy* is
        # stored where Qt keeps an alias, so ':scaling' overrides applied below
        # cannot leak into what gets remembered.
        if fix_scale_ranges:
            data_ranges.update(_OLD_DATA_RANGES)
        else:
            _OLD_DATA_RANGES.clear()
            _OLD_DATA_RANGES.update(data_ranges)

        # ':scaling' overrides land after minmax + fix-range and before make_scale
        # divides by gain, exactly Qt's order (pile_viewer apply_scaling_hooks).
        _apply_scaling_hooks(data_ranges)

        auto_scaler = cast(
            Callable[..., AutoScalerLike], getattr(import_module("pyrocko.plot"), "AutoScaler")
        )
        yscaler = auto_scaler(
            no_exp_interval=(-3, 2),
            approx_ticks=15.0,
            snap=(show_scale_ranges or show_scale_axes) and not show_scale_axes,
        )
        # Per span, keyed on the span's own trace: snuffler projects per
        # (track, scaling key), so two sensors sharing a location-grouped row are
        # each drawn against their own amplitude scale.
        for span, source in scaled_spans:
            dymin, dymax = data_ranges.get(_trace_scaling_key(source, scaling_group), (0.0, 1.0))
            # An equal range is real data — a dead channel pinned at one value — and
            # AutoScaler pads it to ±1 around the value, exactly as snuffler centres
            # such a flatline.
            # Only a non-finite or inverted range has no meaning. With nanmin /
            # nanmax upstream (matching `trace.minmax`) the non-finite case is an
            # *all*-NaN slice, where Qt hands AutoScaler NaN and draws a nonsense
            # axis — a crash-shaped behaviour not worth reproducing.
            if not math.isfinite(dymin) or not math.isfinite(dymax) or dymax < dymin:
                dymin, dymax = 0.0, 1.0
            ymin, ymax, yinc = yscaler.make_scale((dymin / gain, dymax / gain))
            ymin = float(ymin)
            ymax = float(ymax)
            half = max(0.5 * (ymax - ymin), 1e-12)
            span.ymin = ymin
            span.ymax = ymax
            span.ymean = 0.5 * (ymin + ymax)
            span.yscale = max(gain * half, 1e-12)
            # The scaler's tick increment and label exponent ride along so 'Show
            # Scale Axes' draws snuffler's exact ticks and numbers
            # (pile_viewer.py:3357) — Qt reads both off this same AutoScaler.
            span.yinc = float(yinc)
            span.yexp = int(yscaler.make_exp(yinc)) if float(yinc) > 0.0 else 0

    perf_scale = time.monotonic()
    ys = (
        np.ascontiguousarray(np.concatenate(ys_parts), dtype="<f4")
        if ys_parts
        else np.zeros(0, dtype="<f4")
    )
    if mode == "waterfall" and ys.size:
        _colorize_waterfall(meta, ys, req)
    meta.notices.extend(_VIEW_NOTICES)
    perf_concat = time.monotonic()
    _maybe_log_view_perf(
        mode,
        tmin,
        tmax,
        npx,
        track_offset,
        track_count,
        len(traces),
        wanted_tracks,
        sample_tracks,
        int(ys.size),
        perf_t0,
        perf_visible,
        perf_prepare,
        perf_loop,
        perf_scale,
        perf_concat,
        time.monotonic(),
    )
    return meta, ys


def _is_tabu_trace(tr: ptrace.Trace) -> bool:
    meta = getattr(tr, "meta", None)
    return bool(isinstance(meta, dict) and meta.get("tabu"))


def _konno_ohmachi_smooth(values: np.ndarray, freqs: np.ndarray, bandwidth: float) -> np.ndarray:
    bandwidth = max(float(bandwidth or 40.0), 1.0)
    values64 = np.asarray(values, dtype=np.float64)
    freqs64 = np.asarray(freqs, dtype=np.float64)
    out = np.empty_like(values64)
    for i, fc in enumerate(freqs64):
        if fc <= 0.0:
            out[i] = values64[i]
            continue
        with np.errstate(divide="ignore", invalid="ignore"):
            x = bandwidth * np.log10(freqs64 / fc)
            w = (np.sin(x) / x) ** 4
        w[~np.isfinite(w)] = 0.0
        w[i] = 1.0
        denom = float(w.sum())
        out[i] = float(np.sum(w * values64) / denom) if denom > 0.0 else values64[i]
    return out


class _FigureEmpty(Exception):
    """Raised by a figure draw fn when there is nothing to plot (no selected
    traces, missing event, ...). render_figure turns it into a FigureResult whose
    ``error`` the frontend shows as text — mirroring how snuffling errors surface
    in the viewer instead of crashing it."""


_MPL_READY = False


def _ensure_mpl() -> Any:
    """Import matplotlib once with the headless Agg backend (never the Pyodide
    HTML5 canvas backend). Returns the matplotlib module."""
    global _MPL_READY
    import matplotlib

    if not _MPL_READY:
        matplotlib.use("Agg")
        _MPL_READY = True
    return matplotlib


def _render_figure_png(
    req: figure_pb2.FigureRequest, draw: Callable[[Any], None]
) -> figure_pb2.FigureResult:
    """Render a matplotlib figure headlessly and return it as PNG bytes.

    This is the faithfulness seam: ``draw`` runs the *exact* pyrocko/matplotlib
    plotting code, so the frontend shows the same figure the Qt snuffler would —
    no hand-rolled egui reimplementation to drift. Sizing follows the egui window
    (device pixels + dpi); ``dark`` picks matplotlib's dark style so the figure
    matches the app theme. A :class:`_FigureEmpty` becomes an ``error`` result."""
    _ensure_mpl()
    import matplotlib.style as mstyle
    from matplotlib.backends.backend_agg import FigureCanvasAgg
    from matplotlib.figure import Figure

    dpi = float(req.dpi) if req.dpi and req.dpi > 0 else 100.0
    w_px = int(req.width_px) or 640
    h_px = int(req.height_px) or 480
    w_in = max(1.0, w_px / dpi)
    h_in = max(1.0, h_px / dpi)
    style = "dark_background" if req.dark else "default"
    buf = io.BytesIO()
    with mstyle.context(style):
        fig = Figure(figsize=(w_in, h_in), dpi=dpi)
        FigureCanvasAgg(fig)
        try:
            draw(fig)
        except _FigureEmpty as exc:
            return figure_pb2.FigureResult(error=str(exc))
        fig.savefig(buf, format="png", dpi=dpi, facecolor=fig.get_facecolor())
    return figure_pb2.FigureResult(png=buf.getvalue())


def _inview_work_traces(view: view_pb2.ViewParams) -> List[ptrace.Trace]:
    """The traces a `mode='inview'` snuffling operates on: every visible trace
    whose gather-key row lies in the shown track window — Qt's
    `trace_selector` tests row membership (pile_viewer.py:1798), so a grouped
    row contributes *all* its members, not one list slot per track. Returned
    in the pile chopper's `full_id` order (`_process_chopped`, pile.py:1213),
    not the viewer's sort, and never rotated: `chopper_selected_traces` hands
    snufflings the pile's data, with the GUI rotation explicitly not respected
    (snuffling.py:985)."""
    sort = view.sort or "name"
    subsort = view.subsort or "nslc"
    distances_3d = bool(view.distances_3d)
    rows = _row_universe(sort, subsort, distances_3d)
    row_index = {key: i for i, (key, _tr) in enumerate(rows)}
    track_offset = max(0, int(view.track_offset))
    track_count = max(0, int(view.track_count) or len(rows))
    lo, hi = track_offset, track_offset + track_count
    perm = _subsort_perm(subsort)
    inview = [
        tr
        for tr in _visible_traces()
        if lo <= row_index.get(_gather_key(tr, perm, sort, distances_3d), -1) < hi
    ]
    return sorted(inview, key=lambda tr: tr.full_id)


def _fig_ampspec(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Amplitude spectra of the visible traces — a faithful port of pyrocko's
    ``snufflings/ampspec.py`` ``AmpSpec.call`` plotting: |spectrum| on log-log
    axes, per-trace ``cm.Accent`` colours, optional Konno-Ohmachi smoothing, and
    the same axis labels/legend/title. The FFT (``Trace.spectrum``) and smoothing
    are pyrocko's, so nothing is reimplemented."""
    from matplotlib import cm

    spectrum = req.snufflings.spectrum
    want_smoothing = bool(spectrum.spec_smoothing)
    bandwidth = float(spectrum.spec_smoothing_bandwidth or 40.0)

    # The selected markers' spans *with their channel patterns*, else the
    # visible window — the run scope Qt gives every snuffling
    # (`chopper_selected_traces`, ampspec.py:61).
    # The batches arrive marker-outer, so the per-segment colours follow Qt's
    # order, and the chopper degaps: a channel split across files contributes
    # one spectrum, not one per fragment.
    segs: List[ptrace.Trace] = []
    for batch in _chopper_selected_traces(req):
        for seg in batch.traces:
            if seg.ydata is None or len(seg.ydata) < 8:
                continue
            seg.ydata = seg.ydata.astype(np.float64)
            segs.append(seg)

    if not segs:
        raise _FigureEmpty("No data in the visible window.")

    axes = fig.add_subplot(111)
    colors = cm.Accent(np.linspace(0.0, 1.0, len(segs)))
    alpha = 0.2 if want_smoothing else 1.0
    flimits: List[float] = []
    alimits: List[float] = []
    for i, seg in enumerate(segs):
        seg.ydata = seg.ydata - seg.ydata.mean()
        freq, spec = seg.spectrum()
        absa = np.abs(spec)
        if freq.size > 2:
            flimits.append(float(freq[1]))
            flimits.append(float(freq[-1]))
        # The y-limit statistics run over the *positive* bins only: Qt's bare
        # `log(0)` propagates -inf into mean and std; flooring the log instead
        # lets one flatlined channel drag the shared axis thirty decades down
        # and squash every real spectrum. Masking is the honest version of both.
        positive = absa[absa > 0.0]
        if positive.size:
            labsa = np.log(positive)
            std = float(np.std(labsa))
            mean = float(np.mean(labsa))
            alimits.append(math.exp(mean - 5.0 * std))
            alimits.append(math.exp(mean + 5.0 * std))
        color = colors[i]
        label = ".".join(seg.nslc_id)
        axes.plot(freq, absa, c=color, alpha=alpha, label=label)
        if want_smoothing:
            smoothed = _konno_ohmachi_smooth(absa, freq, bandwidth)
            axes.plot(freq, np.abs(smoothed), c=color, alpha=1.0, label=label)

    axes.set_xscale("log")
    axes.set_yscale("log")
    if alimits:
        axes.set_ylim(min(alimits), max(alimits))
    if flimits:
        axes.set_xlim(min(flimits), max(flimits))
    axes.set_xlabel("Frequency [Hz]")
    axes.set_ylabel("Counts")
    handles, labels = axes.get_legend_handles_labels()
    leg = dict(zip(labels, handles, strict=True))
    if len(segs) > 1:
        axes.legend(
            list(leg.values()),
            list(leg.keys()),
            loc=2,
            borderaxespad=0.0,
            bbox_to_anchor=(1.05, 1.0),
        )
        fig.subplots_adjust(right=0.8, left=0.1)
    else:
        axes.set_title(list(leg.keys())[0], fontsize=16)
        fig.subplots_adjust(right=0.9, left=0.1)


def _spectro_cmap(name: str) -> Any:
    """The spectrogram snuffling's colour tables, built exactly as
    ``snufflings/spectrogram.py`` does: a desaturated Tango-colour ramp for
    ``spectro``, else any matplotlib colormap via pyrocko's ``mpl_get_cmap``."""
    from matplotlib.colors import LinearSegmentedColormap
    from pyrocko import plot as pplot

    if name == "spectro":
        cols = [
            pplot.tango_colors[x]
            for x in ["skyblue1", "chameleon1", "butter1", "orange1", "scarletred1", "plum3"]
        ]

        def to01(c: Any) -> Tuple[float, float, float]:
            return (c[0] / 255.0, c[1] / 255.0, c[2] / 255.0)

        def desat(c: Tuple[float, float, float], a: float) -> Tuple[float, ...]:
            mean = (c[0] + c[1] + c[2]) / 3.0
            return tuple(cc * a + mean * (1.0 - a) for cc in c)

        return LinearSegmentedColormap.from_list("spectro", [desat(to01(c), 0.8) for c in cols])
    return pplot.mpl_get_cmap(name)


def _spectrogram_inventory(
    req: snuffling_pb2.SnufflingRequestParams, tinc: float, tpad: float
) -> Tuple[float, float, List[Tuple[NslcId, float]]]:
    """Prescan Qt's chopper stream into unique channel/sample-rate panels."""
    times: List[float] = []
    sampling: Dict[NslcId, "set[float]"] = {}
    for batch in _chopper_selected_traces(
        req,
        mode="inview",
        tinc=tinc,
        tpad=tpad,
        want_incomplete=False,
        load_data=False,
    ):
        times.extend((float(batch.tmin), float(batch.tmax)))
        for trace in batch.traces:
            sampling.setdefault(trace.nslc_id, set()).add(float(trace.deltat))
    if not times or not sampling:
        raise _FigureEmpty("No visible traces to compute a spectrogram for.")
    panels = sorted((nslc, deltat) for nslc, values in sampling.items() for deltat in values)
    return min(times), max(times), panels


def _fig_spectrogram(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Spectrogram of each visible channel — a faithful port of pyrocko's
    ``snufflings/spectrogram.py``: an absolute window grid (NaN where a window is
    incomplete), pyrocko's PSD normalisation, the `nt_max`/`nf_max` size guards,
    one *shared* colour scale with a colorbar so the panels are comparable, a
    formatted time axis, and the `2/twin` frequency floor."""
    p = req.snufflings.spectrogram
    twin = float(p.spectrogram_twin or 100.0)
    overlap = float(p.spectrogram_overlap or 75.0)
    taper_name = p.spectrogram_taper or "Hanning"
    fmin = float(p.spectrogram_fmin) if p.HasField("spectrogram_fmin") else None
    fmax = float(p.spectrogram_fmax) if p.HasField("spectrogram_fmax") else None
    color_scale = p.spectrogram_color_scale or "log"
    # Qt's image-size guards (spectrogram.py:91): more grid columns or rows than
    # this would build an unreasonably large mesh — a limit that matters more in
    # a browser, not less.
    nt_max, nf_max = 2000, 500

    def downsample_plan(n: int, n_max: int) -> Tuple[int, int]:
        n_mean = (n - 1) // n_max + 1
        return n // n_mean, n_mean

    tapers = {
        "Hanning": np.hanning,
        "Hamming": np.hamming,
        "Blackman": np.blackman,
        "Bartlett": np.bartlett,
    }
    taper_fn = tapers.get(taper_name, np.hanning)
    cmap = _spectro_cmap(p.spectrogram_color_table or "spectro")

    tpad = twin * overlap / 100.0 * 0.5
    tinc = twin - 2.0 * tpad
    if tinc <= 0.0:
        raise _FigureEmpty("Overlap is too high for the window length.")
    tmin, tmax, panels = _spectrogram_inventory(req, tinc, tpad)

    nt = int(round((tmax - tmin) / tinc))
    if nt > nt_max:
        _, nt_mean = downsample_plan(nt, nt_max)
        raise _FigureEmpty(
            "Number of samples in spectrogram time axis: %i. Resulting image "
            "will be unreasonably large. Consider increasing window length to "
            "about %g s." % (nt, (tinc * nt_mean) / (1.0 - overlap / 100.0))
        )
    if nt < 1:
        raise _FigureEmpty("The window is shorter than one spectrogram column.")

    n = len(panels)
    ncols = (n - 1) // 5 + 1
    nrows = (n - 1) // ncols + 1
    plotted = 0
    zvalues: List[float] = []
    meshes: List[Any] = []
    axes_list: List[Any] = []
    for i, (nslc, deltat) in enumerate(panels):
        nsamples_want = int(math.floor((tinc + 2.0 * tpad) / deltat))
        if nsamples_want < 4:
            continue
        nfft = 1 << (nsamples_want - 1).bit_length()  # next power of two
        df = 1.0 / (deltat * nfft)
        nf = nfft // 2 + 1
        ifmin = int(math.ceil(fmin / df)) if fmin else 0
        ifmax = min(int(math.floor(fmax / df)) + 1, nf) if fmax else nf
        nf_show = ifmax - ifmin
        if nf_show < 2:
            continue
        nf_show_ds, nf_mean = downsample_plan(nf_show, nf_max)
        win = taper_fn(nsamples_want)
        wnorm = deltat * 2.0 / (df * float(np.sum(win**2)))
        freqs = np.arange(ifmin, ifmax) * df
        if nf_mean != 1:
            nf_trim = (nf_show // nf_mean) * nf_mean
            freqs = np.mean(freqs[:nf_trim].reshape((-1, nf_mean)), axis=1)
        amps = np.full((nf_show_ds, nt), np.nan)
        have_data = False

        def wanted_trace(trace: ptrace.Trace, expected: NslcId = nslc) -> bool:
            return trace.nslc_id == expected

        for batch in _chopper_selected_traces(
            req,
            mode="inview",
            trace_filter=wanted_trace,
            tinc=tinc,
            tpad=tpad + deltat,
            want_incomplete=False,
        ):
            traces = batch.traces
            if len(traces) != 1:
                continue
            trace = traces[0]
            if float(trace.deltat) != deltat:
                raise ValueError(
                    "Unexpected sampling rate on channel %s: %g"
                    % (".".join(nslc), float(trace.deltat))
                )
            trace.chop(float(batch.tmin) - tpad, trace.tmax, inplace=True)
            if trace.ydata is None:
                continue
            trace.set_ydata(trace.ydata[:nsamples_want])
            if trace.data_len() != nsamples_want:
                continue
            y = trace.ydata.astype(np.float64)
            y = (y - y.mean()) * win
            a = np.abs(np.fft.rfft(y, n=nfft))[ifmin:ifmax] ** 2 * wnorm
            if nf_mean != 1:
                nf_trim = (nf_show // nf_mean) * nf_mean
                a = np.mean(a[:nf_trim].reshape((-1, nf_mean)), axis=1)
            tmid = 0.5 * (float(trace.tmax) + float(trace.tmin))
            index = int(round((tmid - tmin) / tinc))
            if index < 0 or nt <= index:
                continue
            amps[:, index] = a
            have_data = True
        if not have_data:
            continue
        times = tmin + 0.5 * tinc + tinc * np.arange(nt)
        if color_scale == "log":
            with np.errstate(divide="ignore"):
                amps = np.log(amps)
        elif color_scale == "sqrt":
            amps = np.sqrt(amps)
        finite = amps[np.isfinite(amps)]
        if not finite.size:
            continue
        mean_a, std_a = float(np.mean(finite)), float(np.std(finite))
        zvalues.append(max(float(np.min(finite)), mean_a - 3.0 * std_a))
        zvalues.append(min(float(np.max(finite)), mean_a + 3.0 * std_a))
        icol, irow = i % ncols, i // ncols
        axes = fig.add_subplot(nrows, ncols, irow * ncols + icol + 1)
        mesh = axes.pcolormesh(times, freqs, amps, cmap=cmap, shading="gouraud")
        # Qt's frequency floor: `2/twin` when no explicit fmin is set
        # (spectrogram.py:324) — below that a window holds under two cycles.
        axes.set_ylim(fmin if fmin else 2.0 / twin, fmax if fmax else float(freqs[-1]))
        axes.set_title(".".join(code for code in nslc if code), fontsize=9)
        axes.set_ylabel("Frequency [Hz]")
        axes.grid(color="black", alpha=0.3)
        try:
            from pyrocko import plot as pplot

            pplot.mpl_time_axis(axes, 10.0 / ncols)
        except Exception:  # noqa: BLE001 — fall back to epoch seconds
            pass
        meshes.append(mesh)
        axes_list.append(axes)
        plotted += 1

    if plotted == 0:
        raise _FigureEmpty("Not enough data in the window for the chosen window length.")
    # One shared colour scale plus a colorbar (Qt's `psd` colour dim), so the
    # panels are comparable — the whole point of drawing them side by side.
    zmin, zmax = min(zvalues), max(zvalues)
    for mesh in meshes:
        mesh.set_clim(zmin, zmax)
    label = {"log": "log PSD", "sqrt": "sqrt PSD"}.get(color_scale, "PSD")
    fig.colorbar(meshes[-1], ax=axes_list, label=label)


def _pca(data: np.ndarray) -> Tuple[np.ndarray, np.ndarray, np.ndarray, float, float]:
    """Principal-component analysis of an ``(ncomp, nsamples)`` array — a verbatim
    port of ``snufflings/polarization.py`` ``Polarization.pca`` (covariance ->
    ``eigh`` -> azimuth/incidence of the largest eigenvector)."""
    cov = np.cov(data)
    evals, evecs = np.linalg.eigh(cov)
    pc = evecs[:, -1]
    eh = math.hypot(float(pc[0]), float(pc[1]))
    if data.shape[0] > 2:
        incidence = math.degrees(math.atan2(eh, abs(float(pc[2]))))
    else:
        incidence = 90.0
    azimuth = math.degrees(math.atan2(float(pc[1]), float(pc[0])))
    azimuth = (-azimuth) % 180.0 - 90.0
    return cov, evals, evecs, azimuth, incidence


# "Fix Scale" (polarization panel): the per-station amplitude scale of the last
# run, reused while the switch is on so successive windows plot comparably.
_POL_FIXED_AMAX: Dict[Tuple[str, str, str], float] = {}

_POL_COLORS = {
    "line": "#204a87",  # sky blue 3 (tango)
    "rot": "#a40000",  # scarlet red 3
    "bazi": "#4e9a06",  # chameleon 3
    "ell": "#75507b",  # plum 2
    "win": (0.9, 0.9, 0.7),
}


_POL_SCHEMES: List[List[str]] = [
    ["E", "N", "Z"],
    ["1", "2", "Z"],
    ["1", "2", "3"],
    ["0", "1", "2"],
    ["R", "T", "Z"],
    ["E", "N"],
    ["1", "2"],
    ["0", "1"],
]


def _polarization_scheme(by_channel: Dict[str, ptrace.Trace]) -> Optional[List[ptrace.Trace]]:
    """Order a station's channels ``[E-like, N-like(, Z-like)]`` by Qt's
    component schemes (`polarization.py:250`): the Cube-datalogger names first,
    then the eight last-character schemes. ``None`` when the group matches none.
    """
    channels = sorted(by_channel)
    for cube in (["p2", "p1", "p0"], ["p2", "p1"]):
        if channels == sorted(cube):
            return [by_channel[ch] for ch in cube]
    comps = {ch[-1]: ch for ch in by_channel if ch}
    if len(comps) != len(by_channel):
        return None
    for scheme in _POL_SCHEMES:
        if sorted(comps) == sorted(scheme):
            return [by_channel[comps[c]] for c in scheme]
    return None


def _polarization_channels(
    nsl: Tuple[str, str, str],
    chagroup: str,
    w_tmin: float,
    w_tmax: float,
    hp: Optional[float],
    lp: Optional[float],
    t_length: float,
) -> Dict[str, ptrace.Trace]:
    """One polarization window's filtered channel segments, keyed by channel —
    the extraction both the figure and its text twin start from, so their
    completeness demands cannot drift apart.

    Qt's polarization choppers the raw pile with only its own channel pattern
    (polarization.py:300) — no viewer hide/quick filters, no row window, no
    GUI rotation (the snuffling derives its own R/T rotation from the
    back-azimuth) — and requires data across the window, one window length of
    display margin, and the filter's settling pad, so a hidden component still
    analyses and a gappy or too-short one drops out.
    Each segment comes back filtered and chopped to the display margin
    ``[w_tmin - t_length, w_tmax + t_length]``; callers chop further as their
    output needs."""
    from pyrocko import util as putil

    # The filter needs settling room: three periods of the lowest corner,
    # capped so a slider mid-drag cannot demand an insane cutout
    # (polarization.py:270); tpad is the display margin around the window.
    if hp is not None:
        tpad_filter = 3.0 / float(hp)
    elif lp is not None:
        tpad_filter = 3.0 / float(lp)
    else:
        tpad_filter = 0.0
    tpad_filter = min(tpad_filter, 5.0 * t_length)
    tpad = t_length

    pattern = ".".join(nsl + (chagroup + "?",))

    def match(tr: ptrace.Trace, pattern: str = pattern) -> bool:
        return bool(putil.match_nslc(pattern, tr.nslc_id))

    by_channel: Dict[str, ptrace.Trace] = {}
    for batch in _pile().chopper(
        tmin=w_tmin,
        tmax=w_tmax,
        tpad=tpad + tpad_filter,
        want_incomplete=False,
        style="batch",
        trace_selector=match,
    ):
        for seg in batch.traces:
            if seg.ydata is None or len(seg.ydata) < 3:
                continue
            seg.ydata = seg.ydata.astype(np.float64)
            # The snuffling's own corners, always as 4th-order lowpass then
            # highpass and only below the trace's Nyquist (polarization.py:308).
            try:
                if lp is not None and float(lp) < 0.5 / seg.deltat:
                    seg.lowpass(4, float(lp))
                if hp is not None and float(hp) < 0.5 / seg.deltat:
                    seg.highpass(4, float(hp))
                seg.chop(w_tmin - tpad, w_tmax + tpad)
            except Exception:  # noqa: BLE001 — window not covered
                continue
            by_channel[seg.channel] = seg
    return by_channel


def _fig_polarization(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Per-station polarization figure — a faithful port of
    ``snufflings/polarization.py``: the analysis windows come from the *selected
    markers* (`get_selected`, polarization.py:205 — the snuffling fails without
    one), shifted and sized by Offset and Length; three covariance-hodogram
    projections with a moving dot, optional 2D eigensystem ellipses, a
    backazimuth indicator, and the rotated trace panel labelled with the rotated
    channel names. The eigen-analysis (``_pca``) is pyrocko's math; the
    interactive dynamic axes placement is replaced by a static grid."""
    from matplotlib import patches

    pol = req.snufflings.polarization
    t_length = float(pol.polarization_t_length or 1.0)
    ft_offset = float(pol.polarization_ft_offset)
    delta_azi = float(pol.polarization_azimuth)
    rotate_to_rt = bool(pol.polarization_rotate_to_rt)
    dot_position = min(1.0, max(0.0, float(pol.polarization_dot_position)))
    show_2d = bool(pol.polarization_show_eigensystem_2d)
    show_3d = bool(pol.polarization_show_eigensystem_3d)
    fix_scale = bool(pol.polarization_fix_scale)
    hp = pol.polarization_highpass if pol.HasField("polarization_highpass") else None
    lp = pol.polarization_lowpass if pol.HasField("polarization_lowpass") else None

    # Qt's windows (`get_selected` + `transform_time_span`): one per selected
    # channel-scoped marker, anchored at the marker start, Offset·Length later,
    # Length long. An unscoped marker contributes nothing there either.
    windows: List[Tuple[Tuple[str, str, str], str, float, float]] = []
    for s in req.selected_spans:
        w_tmin = float(s.tmin) + t_length * ft_offset
        w_tmax = w_tmin + t_length
        for n in s.nslc_ids:
            windows.append(((n.net, n.sta, n.loc), n.cha[:-1] if n.cha else "", w_tmin, w_tmax))
    if not windows:
        raise _FigureEmpty(
            "No selected markers.\n\nCreate and select markers at points of "
            "interest. Normal and phase markers are accepted."
        )

    bazis: Dict[Tuple[str, str, str], float] = {}
    if _ACTIVE_EVENT is not None and _STATIONS:
        from pyrocko import orthodrome as od

        elat, elon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
        for nsl, _chagroup, _wt0, _wt1 in windows:
            st = _station_at(nsl[0], nsl[1], nsl[2])
            if st is not None:
                bazis[nsl] = float(od.azimuth(st[0], st[1], elat, elon))

    stations = []
    for nsl, chagroup, w_tmin, w_tmax in windows:
        by_channel: Dict[str, ptrace.Trace] = {}
        for cha, seg in _polarization_channels(
            nsl, chagroup, w_tmin, w_tmax, hp, lp, t_length
        ).items():
            # Demean by the *analysis* chop's mean (polarization.py:326): a
            # DC offset displaces the hodogram and corrupts the covariance.
            try:
                seg_chop = seg.chop(w_tmin, w_tmax, inplace=False)
            except Exception:  # noqa: BLE001 — window not covered
                continue
            if seg_chop.ydata is None or not len(seg_chop.ydata):
                continue
            seg.ydata = seg.ydata - float(np.mean(seg_chop.ydata))
            by_channel[cha] = seg
        group = _polarization_scheme(by_channel) if by_channel else None
        if group is None or len(group) != 3:
            continue
        stations.append((nsl, group, w_tmin, w_tmax))

    if not stations:
        raise _FigureEmpty(
            "No matching traces. Check time and channel settings. Traces may "
            "not contain gaps within the extracted time window and in the "
            "padding areas left and right."
        )

    nst = len(stations)
    drawn = 0
    for i, (nsl, group, w_tmin, w_tmax) in enumerate(stations):
        te, tn, tz = group
        ye, yn, yz = (np.asarray(t.ydata, dtype=np.float64) for t in (te, tn, tz))
        ns = min(len(yn), len(ye), len(yz))
        if ns < 3:
            continue
        yn, ye, yz = yn[:ns], ye[:ns], yz[:ns]

        bazimuth = delta_azi
        if rotate_to_rt:
            if nsl not in bazis:
                raise _FigureEmpty(
                    "Cannot rotate to RT. Station coordinates must be available "
                    'and an event must be marked as the "active" event.'
                )
            bazimuth += bazis[nsl] + 90.0
        if bazimuth != 0.0:
            ca, sa = math.cos(math.radians(bazimuth)), math.sin(math.radians(bazimuth))
            # pyrocko's `trace.rotate` matrix: n' = cos·n + sin·e,
            # e' = −sin·n + cos·e.
            yn_r, ye_r = ca * yn + sa * ye, -sa * yn + ca * ye
            cha_e_rot, cha_n_rot = te.channel[-1] + "'", tn.channel[-1] + "'"
        else:
            yn_r, ye_r = yn, ye
            cha_e_rot, cha_n_rot = te.channel[-1], tn.channel[-1]

        amax = float(np.sqrt(yn**2 + ye**2 + yz**2).max()) or 1.0
        # Fix Scale: hold the station's amplitude scale from the previous run
        # so successive windows plot comparably; off, the store follows along.
        if fix_scale and nsl in _POL_FIXED_AMAX:
            amax = _POL_FIXED_AMAX[nsl]
        else:
            _POL_FIXED_AMAX[nsl] = amax
        ipos = int(round(dot_position * (ns - 1)))
        line, rot = _POL_COLORS["line"], _POL_COLORS["rot"]

        ax01 = fig.add_subplot(nst, 4, i * 4 + 1)
        ax02 = fig.add_subplot(nst, 4, i * 4 + 2)
        ax12 = fig.add_subplot(nst, 4, i * 4 + 3)
        axtr = fig.add_subplot(nst, 4, i * 4 + 4)

        # Panel labels are Qt's: the unrotated horizontals first, then each
        # *rotated channel name* against Z (polarization.py:482) — never a
        # literal R/T, which for a transverse-first rotation reads backwards.
        for axes, xdata, ydata, xlab, ylab in (
            (ax01, ye, yn, te.channel[-1], tn.channel[-1]),
            (ax02, ye_r, yz, cha_e_rot, "Z"),
            (ax12, yn_r, yz, cha_n_rot, "Z"),
        ):
            axes.set_aspect(1.0)
            axes.plot(xdata, ydata, color=line, lw=1.0)
            axes.plot([xdata[ipos]], [ydata[ipos]], "o", ms=4, color=line)
            axes.set_xlim(-amax * 1.05, amax * 1.05)
            axes.set_ylim(-amax * 1.05, amax * 1.05)
            axes.set_xlabel(xlab)
            axes.set_ylabel(ylab)
            axes.set_xticks([])
            axes.set_yticks([])
            if show_2d and xdata.size >= 3:
                _cov, evals, evecs, _azi, _inc = _pca(np.vstack([xdata, ydata]))
                axes.add_patch(
                    patches.Ellipse(
                        (0.0, 0.0),
                        width=math.sqrt(abs(float(evals[0]))) * 2.0,
                        height=math.sqrt(abs(float(evals[1]))) * 2.0,
                        angle=math.degrees(math.atan2(float(evecs[1, 0]), float(evecs[0, 0]))),
                        zorder=-10,
                        fc=_POL_COLORS["ell"],
                        ec=_POL_COLORS["ell"],
                        alpha=0.3,
                    )
                )

        # Show 3D Eigensystem: the principal axis of the 3-component
        # covariance, projected into each hodogram's coordinate pair.
        if show_3d and ns >= 3:
            _c3, ev3, evec3, _a3, _i3 = _pca(np.vstack([ye_r, yn_r, yz]))
            p_e, p_n, p_z = (float(evec3[j, -1]) for j in range(3))
            scale3 = amax
            for axes, dx, dy in ((ax01, p_e, p_n), (ax02, p_e, p_z), (ax12, p_n, p_z)):
                axes.plot(
                    [-scale3 * dx, scale3 * dx],
                    [-scale3 * dy, scale3 * dy],
                    "-",
                    lw=2,
                    zorder=-15,
                    color=_POL_COLORS["ell"],
                    alpha=0.6,
                )

        if nsl in bazis:
            eb = bazis[nsl]
            ax01.plot(
                [0.0, amax * math.cos(math.radians(90.0 - eb))],
                [0.0, amax * math.sin(math.radians(90.0 - eb))],
                "--",
                lw=3,
                zorder=-20,
                color=_POL_COLORS["bazi"],
            )

        deltat = float(tz.deltat)
        times = float(tz.tmin) + np.arange(ns) * deltat
        for itr, (ydata, _lab) in enumerate(((ye_r, cha_e_rot), (yn_r, cha_n_rot), (yz, "Z"))):
            col = rot if (itr < 2 and bazimuth != 0.0) else line
            axtr.plot(times, ydata / (2.0 * amax) + itr, color=col, lw=1.0)
            axtr.plot([times[ipos]], [ydata[ipos] / (2.0 * amax) + itr], "o", ms=4, color=col)
        # The shaded band is the *analysis* window inside the padded traces.
        axtr.axvspan(w_tmin, w_tmax, color=_POL_COLORS["win"], zorder=-20)
        axtr.set_ylim(-1, 3)
        axtr.set_yticks([0, 1, 2])
        axtr.set_yticklabels([cha_e_rot, cha_n_rot, "Z"])
        axtr.set_xlabel("Time [s]")
        ax01.set_title(".".join(x for x in nsl if x) or nsl[1])
        drawn += 1

    if drawn == 0:
        raise _FigureEmpty("No 3-component data in the window.")
    fig.tight_layout()


def _fig_cake_model(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Cake 'Plot Model' — pyrocko's *exact* matplotlib model figure (P/S velocity
    vs depth over the shaded layer cross-section). Reuses
    ``pyrocko.plot.cake_plot.my_model_plot`` (pure matplotlib, no Qt) instead of
    re-drawing it in Rust, so it can never drift from the Qt snuffler."""
    from pyrocko import cake
    from pyrocko.plot import cake_plot

    model, _phases, _tshift, _use_station_depth, _mode = _cake_section(req)
    mod = _cake_load_model(cake, model)
    cake_plot.my_model_plot(mod, fig=fig)


def _fig_cake_rays(fig: Any, req: snuffling_pb2.SnufflingRequestParams) -> None:
    """Cake 'Plot Rays' — pyrocko's *exact* ray-path figure (ray fan + source dot +
    receiver triangles over the shaded model). Reuses
    ``pyrocko.plot.cake_plot.my_rays_plot``; all geometry is cake's ray tracer."""
    from pyrocko import cake
    from pyrocko.plot import cake_plot

    model, phase_names, _tshift, use_station_depth, _mode = _cake_section(req)
    if _ACTIVE_EVENT is None or not _STATIONS:
        raise _FigureEmpty(
            "Cake ray paths need an active event and station coordinates.\n"
            "Search a catalog event and fetch station coordinates first."
        )
    wanted, _errors = _cake_wanted_phases(cake, phase_names)
    if not wanted:
        raise _FigureEmpty("No phases selected.")

    mod = _cake_load_model(cake, model)
    _elat, _elon, edepth, _etime = _ACTIVE_EVENT
    allrays: List[Any] = []
    distances: List[float] = []
    zstop = 0.0
    for _net, _sta, dist_deg, depth in _cake_stations():
        rdepth = float(depth) if use_station_depth else 0.0
        allrays.extend(
            mod.arrivals(phases=wanted, distances=[dist_deg], zstart=edepth, zstop=rdepth)
        )
        distances.append(float(dist_deg))
        zstop = rdepth
    if not allrays:
        raise _FigureEmpty("No ray arrivals for the selected phases and distances.")

    # zstart/zstop are depths in metres; distances are epicentral degrees — the
    # units my_rays_plot's axis scaling (labels_rays) expects.
    cake_plot.my_rays_plot(mod, None, allrays, edepth, zstop, distances=distances, fig=fig)


def _fig_beachball(fig: Any, req: figure_pb2.FigureRequest) -> None:
    """Focal-mechanism beachball for a catalog event's moment tensor, drawn by
    pyrocko's own matplotlib beachball (deviatoric, scarlet-red compressional
    quadrants, like the Qt snuffler marker editor) — never re-derived in Rust."""
    from pyrocko import moment_tensor as mtm
    from pyrocko.plot import tango_colors
    from pyrocko.plot.beachball import plot_beachball_mpl

    m = req.mt
    mt = mtm.MomentTensor(mnn=m.mnn, mee=m.mee, mdd=m.mdd, mne=m.mne, mnd=m.mnd, med=m.med)
    axes = fig.add_subplot(1, 1, 1, aspect=1.0)
    axes.set_axis_off()
    axes.set_xlim(-1.1, 1.1)
    axes.set_ylim(-1.1, 1.1)
    color_t = tuple(c / 255.0 for c in tango_colors["scarletred3"])
    plot_beachball_mpl(
        mt,
        axes,
        beachball_type="deviatoric",
        size=2.0,
        size_units="data",
        position=(0.0, 0.0),
        color_t=color_t,
        color_p="white",
        edgecolor="white" if req.dark else "black",
        linewidth=1.5,
    )
    strike, dip, rake = mt.both_strike_dip_rake()[0]
    axes.set_title("strike %.0f°  dip %.0f°  rake %.0f°" % (strike, dip, rake))


_FIGURES: Dict[str, Callable[[Any, snuffling_pb2.SnufflingRequestParams], None]] = {
    "ampspec": _fig_ampspec,
    "spectrogram": _fig_spectrogram,
    "polarization": _fig_polarization,
    "cake_model": _fig_cake_model,
    "cake_rays": _fig_cake_rays,
}


def _render_figure(req: figure_pb2.FigureRequest) -> figure_pb2.FigureResult:
    """Dispatch a FigureRequest to the matching pyrocko/matplotlib figure. Any
    error (bad params, no data, plotting failure) becomes an ``error`` result the
    frontend shows as text, so a broken figure never takes down the viewer."""
    if req.kind == "beachball":

        def draw(fig: Any) -> None:
            _fig_beachball(fig, req)

    else:
        draw_kind = _FIGURES.get(req.kind)
        if draw_kind is None:
            return figure_pb2.FigureResult(error="unknown figure: %s" % req.kind)

        def draw(fig: Any) -> None:
            draw_kind(fig, req.params)

    try:
        return _render_figure_png(req, draw)
    except Exception as exc:  # noqa: BLE001 — surface as text, never raise across the worker
        return figure_pb2.FigureResult(
            error="%s: %s\n\n%s" % (type(exc).__name__, exc, traceback.format_exc())
        )


async def _load_gf_store_url(base_url: str) -> bridge_pb2.StringValue:
    """Mount a Green's-functions store by fetching its files from an HTTP base URL
    into the Pyodide filesystem (no File-System-Access-API directory picker). The
    base must serve a ``.manifest`` (newline-separated relative file paths). Then
    register the store like a directory mount and return its "GF Store: <id>" label."""
    from pyodide.http import pyfetch

    base = base_url.rstrip("/")
    manifest = await _await_external_operation(pyfetch(base + "/.manifest"))
    if not manifest.ok:
        raise RuntimeError("no .manifest at %s (HTTP %s)" % (base, manifest.status))
    files = [
        line.strip()
        for line in (await _await_external_operation(manifest.string())).splitlines()
        if line.strip()
    ]
    # Download into a per-attempt staging tree and move it into place only once
    # every file landed: the session transaction rolls back _GF_STORES on
    # failure or cancellation, but not the filesystem, and a later attempt
    # reuses this root — stale files from a half-downloaded manifest must not
    # shadow the new store's contents.
    global _STAGE_COUNTER
    _STAGE_COUNTER += 1
    staging = "/gfstores/.staging%d" % _STAGE_COUNTER
    os.makedirs(staging, exist_ok=True)
    try:
        for rel in files:
            resp = await _await_external_operation(pyfetch(base + "/" + rel))
            if not resp.ok:
                raise RuntimeError("fetch %s: HTTP %s" % (rel, resp.status))
            dest = "%s/%s" % (staging, rel)
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "wb") as handle:
                handle.write(bytes(await _await_external_operation(resp.bytes())))
        root = "/gfstores/store%d" % len(_GF_STORES)
        shutil.rmtree(root, ignore_errors=True)
        os.replace(staging, root)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return bridge_pb2.StringValue(value=gf_store_label(root))


def _ensure_real_gf() -> Any:
    """Import the FULL ``pyrocko.gf`` (LocalEngine/DCSource/Target/Request), first
    evicting the lightweight stub that :func:`_gf_modules` installs for the cake
    GF-store path — that stub has only ``gf.store``/``gf.meta`` and would make the
    seismosizer fail. Needs ``ensureGf`` (requests + pyodide-http), which the
    GF-store mount already installs."""
    import importlib
    import sys

    existing = sys.modules.get("pyrocko.gf")
    if existing is not None and getattr(existing, "_gf_stub", False):
        for key in [k for k in sys.modules if k == "pyrocko.gf" or k.startswith("pyrocko.gf.")]:
            del sys.modules[key]
    return importlib.import_module("pyrocko.gf")


def _seismosizer_run(req: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Synthetic seismograms from a double-couple point source via
    ``pyrocko.gf.LocalEngine`` and a mounted Green's-functions store — a faithful
    port of ``snufflings/seismosizer.py`` (DCSource). Adds the synthetic traces to
    the pile and a source event marker. Needs a mounted GF store + `ensureGf`."""
    # Qt's `call()` opens with `self.cleanup()` (seismosizer.py:97) — before
    # any validation — so even a failing run clears the previous run's
    # synthetics and marker.
    # Every failure carries the updated scene and the origin tag; the tag is
    # what retires the old event marker.
    _replace_derived("seismosizer", [])
    _replace_derived_events("seismosizer", {})

    def refuse(text: str) -> snuffling_pb2.SnufflingResult:
        result = snuffling_pb2.SnufflingResult(
            show_window=True, text=text, scene=_scene_meta(), origin="seismosizer"
        )
        result.event_state.CopyFrom(_event_state())
        return result

    try:
        return _seismosizer_result(req, refuse)
    except Exception as exc:  # noqa: BLE001 — surface, don't raise
        # Qt surfaces any exception out of `call()` in the snuffling error
        # dialog with the cleanup above already done — a damaged store,
        # regularization or event conversion must not fail differently from
        # the two refusals `fail()` handles.
        # Without the scene and origin on this path, the frontend would keep
        # the previous run's output.
        return refuse("%s: %s\n\n%s" % (type(exc).__name__, exc, traceback.format_exc()))


def _seismosizer_result(
    req: snuffling_pb2.SnufflingRequestParams,
    refuse: Callable[[str], snuffling_pb2.SnufflingResult],
) -> snuffling_pb2.SnufflingResult:
    """The Seismosizer run proper, after `_seismosizer_run`'s cleanup: build
    the source and targets, process them through the mounted store, and hand
    every invalid input to ``refuse``."""
    p = req.snufflings.seismosizer
    store_label = p.seis_store or ""
    if not store_label.startswith(GF_STORE_PREFIX):
        return refuse("Select a mounted GF store first (use “Add GF Store…”).")
    sid = store_label[len(GF_STORE_PREFIX) :]
    store_dir = _GF_STORES.get(sid)
    if store_dir is None:
        return refuse("GF store %r is not mounted." % sid)

    gf = _ensure_real_gf()
    from pyrocko import model

    km = 1000.0
    if _ACTIVE_EVENT is not None:
        elat, elon, _edep, etime = _ACTIVE_EVENT
    else:
        elat, elon, etime = 0.0, 0.0, 0.0

    # Qt's four STF choices with the duration applied to the instance —
    # including duration 0, which passes a zero-duration STF object rather than
    # none (`get_stf`, seismosizer.py:65).
    stf_type = p.seis_stf_type or "half sin"
    stf_of = {
        "half sin": gf.HalfSinusoidSTF,
        "triangular": gf.TriangularSTF,
        "boxcar": gf.BoxcarSTF,
    }.get(stf_type)
    if stf_of is None and stf_type != "None":
        # Qt's Choice widget makes an unknown label unrepresentable (its
        # `get_stf` dict lookup would KeyError); over the bridge it is an
        # invalid request, refused rather than silently run with no STF.
        return refuse("Unknown STF type %r." % stf_type)
    stf = stf_of(duration=float(p.seis_stf_duration)) if stf_of is not None else None
    common = dict(
        time=etime + float(p.seis_time),
        lat=elat,
        lon=elon,
        north_shift=float(p.seis_north_km) * km,
        east_shift=float(p.seis_east_km) * km,
        depth=float(p.seis_depth_km) * km,
    )
    # Qt's five Seismosizer source snufflings, one per `pyrocko.gf` source type
    # (seismosizer.py:280-560), each built with its own parameter set.
    src_type = p.seis_source_type or "DCSource"
    if src_type == "MTSource":
        from pyrocko import moment_tensor as mtm

        # Relative components normalized to unit moment, then scaled to the
        # magnitude's moment (`get_m6`, seismosizer.py:385).
        rm6 = np.array(
            [p.seis_rmnn, p.seis_rmee, p.seis_rmdd, p.seis_rmne, p.seis_rmnd, p.seis_rmed],
            dtype=float,
        )
        m9 = np.array(mtm.symmat6(*rm6))
        m0_unscaled = math.sqrt(float(np.sum(m9**2))) / math.sqrt(2.0)
        if m0_unscaled == 0.0:
            return refuse("MTSource needs a non-zero relative moment tensor.")
        m9 = m9 / m0_unscaled * mtm.magnitude_to_moment(float(p.seis_magnitude))
        source = gf.MTSource(m6=mtm.to6(m9), stf=stf, **common)
    elif src_type == "SFSource":
        source = gf.SFSource(
            fn=float(p.seis_fn), fe=float(p.seis_fe), fd=float(p.seis_fd), stf=stf, **common
        )
    elif src_type == "RectangularSource":
        source = gf.RectangularSource(
            magnitude=float(p.seis_magnitude),
            strike=float(p.seis_strike),
            dip=float(p.seis_dip),
            rake=float(p.seis_rake),
            length=float(p.seis_length_km) * km,
            width=float(p.seis_width_km) * km,
            nucleation_x=float(p.seis_nucleation_x),
            nucleation_y=float(p.seis_nucleation_y),
            velocity=float(p.seis_velocity),
            stf=stf,
            **common,
        )
    elif src_type == "PseudoDynamicRupture":
        source = gf.PseudoDynamicRupture(
            nx=max(1, int(p.seis_nx)),
            ny=max(1, int(p.seis_ny)),
            slip=float(p.seis_slip),
            strike=float(p.seis_strike),
            dip=float(p.seis_dip),
            rake=float(p.seis_rake),
            length=float(p.seis_length_km) * km,
            width=float(p.seis_width_km) * km,
            nucleation_x=float(p.seis_nucleation_x),
            nucleation_y=float(p.seis_nucleation_y),
            gamma=float(p.seis_gamma),
            # Qt hardcodes these (seismosizer.py:577) — except nthreads, which
            # is single here because Pyodide has no thread pool for this.
            nthreads=1,
            pure_shear=True,
            smooth_rupture=True,
            **common,
        )
    elif src_type == "DCSource":
        source = gf.DCSource(
            magnitude=float(p.seis_magnitude),
            strike=float(p.seis_strike),
            dip=float(p.seis_dip),
            rake=float(p.seis_rake),
            stf=stf,
            **common,
        )
    else:
        # The five types above are the closed set Qt's Choice offers; an
        # unknown label silently becoming a DCSource would run the wrong
        # physics without a word.
        return refuse("Unknown source type %r." % src_type)
    source.regularize()

    stations = []
    if _STATIONS:
        for (net, sta), (slat, slon, sele, sdep) in _stations_by_name().items():
            stations.append(
                model.Station(
                    network=net, station=sta, lat=slat, lon=slon, elevation=sele, depth=sdep
                )
            )
    else:  # snuffler's fallback: two symmetric synthetic stations
        for lat, lon in [(5.0, 0.0), (-5.0, 0.0)]:
            stations.append(
                model.Station(network="SY", station="%g_%g" % (lat, lon), lat=lat, lon=lon)
            )

    # Qt targets the (loc, cha) pairs present in the *marker-scoped* traces
    # per station (`chopper_selected_traces(fallback=True, mode='visible')`,
    # seismosizer.py:114), always adding ('', N/E/Z), with codes `loc + '-syn'`
    # and azimuth/dip assigned for T/R/1/2 channel endings (seismosizer.py:120).
    # Per marker, a trace must match that marker's channel patterns *and* hold
    # data in its span — a channel silent across the whole scope yields no
    # synthetic target.
    spans = _snuffling_spans(req)
    s2c: Dict[Tuple[str, str], "set[Tuple[str, str]]"] = {}
    for tr in _visible_traces():
        if not any((sel is None or sel(tr)) and tr.overlaps(s0, s1) for s0, s1, sel in spans):
            continue
        net, sta, loc, cha = tr.nslc_id
        s2c.setdefault((net, sta), set()).add((loc, cha))
    for st in stations:
        s2c.setdefault((st.network, st.station), set()).update(("", cha) for cha in "NEZ")

    targets = []
    for st in stations:
        for loc, cha in sorted(s2c.get((st.network, st.station), set())):
            target = gf.Target(
                codes=(st.network, st.station, loc + "-syn", cha),
                quantity="displacement",
                lat=st.lat,
                lon=st.lon,
                depth=st.depth,
                store_id=sid,
                interpolation="nearest_neighbor",
                optimization="enable",
            )
            _azi_fwd, bazi = source.azibazi_to(target)
            if cha.endswith("T"):
                target.dip, target.azimuth = 0.0, bazi + 270.0
            elif cha.endswith("R"):
                target.dip, target.azimuth = 0.0, bazi + 180.0
            elif cha.endswith("1"):
                target.dip, target.azimuth = 0.0, 0.0
            elif cha.endswith("2"):
                target.dip, target.azimuth = 0.0, 90.0
            targets.append(target)

    engine = gf.LocalEngine(store_dirs=[store_dir])
    # Pyodide's mounted (NativeFS/OPFS) filesystem does not support mmap, so open
    # the store without memmap (also fine, just slightly slower, on native builds).
    engine._open_stores[sid] = gf.store.Store(store_dir, use_memmap=False)  # noqa: SLF001
    request = gf.Request(sources=[source], targets=targets)
    request.regularize()
    try:
        traces = engine.process(request, nthreads=0).pyrocko_traces()
    except (gf.meta.OutOfBounds, gf.store_ext.StoreExtError) as exc:
        # Qt's `self.fail(e)` on the same two exceptions (seismosizer.py:204):
        # the message, with the cleanup above already done.
        return refuse(str(exc))

    waveform_type = p.seis_waveform_type or "Displacement [m]"
    if waveform_type.startswith("Velocity"):
        for tr in traces:
            tr.set_ydata(np.diff(tr.ydata) / tr.deltat)
    elif waveform_type.startswith("Acceleration"):
        for tr in traces:
            tr.set_ydata(np.diff(np.diff(tr.ydata)) / tr.deltat**2)
    if waveform_type.endswith(("[nm]", "[nm/s]", "[nm/s^2]")):
        for tr in traces:
            tr.set_ydata(tr.ydata * 1e9)

    result = snuffling_pb2.SnufflingResult(show_window=True, origin="seismosizer")
    markers = _MarkerAccumulator(req.marker_budget)
    # Qt adds `EventMarker(source.pyrocko_event())` (seismosizer.py:143): the
    # synthetic event carries its full origin and mechanism, not just a label.
    ev = source.pyrocko_event()
    ev_hash = str(ev.get_hash())
    # The event's own magnitude, not the dial's: a PseudoDynamicRupture
    # derives its moment from slip and geometry, and an SFSource has none at
    # all — the configured value would be wrong for one and a fiction for the
    # other.
    result.text = "%d synthetic trace(s) from a %s%s." % (
        len(traces),
        src_type,
        " (M%.1f)" % float(ev.magnitude) if ev.magnitude is not None else "",
    )
    marker = snuffling_pb2.SnufMarker()
    marker.t = float(source.time)
    marker.is_event = True
    marker.event_name = str(ev.name) if ev.name else "synthetic %s" % src_type
    if ev.magnitude is not None:
        marker.magnitude = float(ev.magnitude)
    marker.lat = float(ev.lat)
    marker.lon = float(ev.lon)
    marker.depth = float(ev.depth or 0.0)
    mt = ev.moment_tensor
    if mt is not None:
        mnn, mee, mdd, mne, mnd, med = (float(x) for x in mt.m6())
        marker.moment_tensor.mnn = mnn
        marker.moment_tensor.mee = mee
        marker.moment_tensor.mdd = mdd
        marker.moment_tensor.mne = mne
        marker.moment_tensor.mnd = mnd
        marker.moment_tensor.med = med
    marker.event_hash = ev_hash
    markers.add(marker, strength=float(ev.magnitude or 0.0))
    # Commit only now, with every fallible step behind us: event conversion or
    # marker construction throwing above must leave the cleaned state, never a
    # scene holding new synthetics without their marker.
    # Qt's rerun contract: `call()` opens with `self.cleanup()`
    # (seismosizer.py:97), so a new run replaces the previous run's synthetics
    # instead of stacking over them — and only those, never the file pile or
    # its provenance.
    _replace_derived_events("seismosizer", {ev_hash: ev})
    _replace_derived("seismosizer", traces)
    result.scene.CopyFrom(_scene_meta())
    result.event_state.CopyFrom(_event_state())
    return markers.finish(result)


# Resolved SeisBench ONNX model URLs, keyed by "Model/weights/base". The actual
# ORT InferenceSession is cached in the JS worker because that is where WebGPU
# lives; Python caches only the manifest lookup and URL resolution.
_SEISBENCH_MODEL_URLS: Dict[str, str] = {}


def _join_url(base_url: str, filename: str) -> str:
    return base_url.rstrip("/") + "/" + filename


async def _ensure_seisbench_model(
    model: str, weights: str, base_url: str
) -> Tuple[str, Dict[str, Any]]:
    """Resolve and cache the chosen exported ONNX model URL."""
    import seisbench_infer as sbi

    manifest = await sbi.load_manifest()
    spec = sbi.model_spec(manifest, model, weights)
    filename = str(spec["file"])
    base = base_url.strip()
    if not base:
        raise RuntimeError(
            "SeisBench ONNX model base URL is not configured; use the default "
            "embed-data build or set SNUFFLER_ONNX_MODEL_BASE_URL for lean builds"
        )
    cache_key = "%s/%s/%s" % (model, weights, base)
    if cache_key not in _SEISBENCH_MODEL_URLS:
        if base.startswith(("http://", "https://")):
            url = _local_cors_proxy_url(_join_url(base, filename))
        else:
            url = _asset_url(_join_url(base, filename))
        _SEISBENCH_MODEL_URLS[cache_key] = url
    return _SEISBENCH_MODEL_URLS[cache_key], spec


def _apply_seisbench_filter(seg: ptrace.Trace, spec: Dict[str, Any]) -> None:
    args = spec.get("filter_args")
    kwargs = spec.get("filter_kwargs") or {}
    if not args:
        return
    kind = str(args[0] if isinstance(args, list) else args)
    order = int(kwargs.get("corners", 4))
    try:
        if kind == "highpass":
            seg.highpass(order, float(kwargs["freq"]), nyquist_exception=True)
        elif kind == "lowpass":
            seg.lowpass(order, float(kwargs["freq"]), nyquist_exception=True)
        elif kind == "bandpass":
            seg.bandpass(order, float(kwargs["freqmin"]), float(kwargs["freqmax"]))
    except Exception:
        pass


def _apply_seisbench_view_filter(
    tr: ptrace.Trace,
    highpass: Optional[float],
    lowpass: Optional[float],
    tcut: float,
) -> ptrace.Trace:
    """Apply Qt's default SeisBench preprocessing from the main viewer passband."""
    seg = tr.copy()
    if lowpass is not None:
        seg.lowpass(4, lowpass, nyquist_exception=False)
    if highpass is not None:
        seg.highpass(4, highpass, nyquist_exception=False)
    try:
        seg.chop(seg.tmin + tcut, seg.tmax - tcut)
    except ptrace.NoData:
        pass
    return seg


def _seisbench_groups(
    traces: List[ptrace.Trace],
    tmin: float,
    tmax: float,
    spec: Dict[str, Any],
    scale_factor: float = 1.0,
) -> "OrderedDict[Tuple[str, str, str], Tuple[float, np.ndarray, NslcId]]":
    """Assemble per-(net,sta,loc) component arrays at the model sampling rate.

    Mirrors seisbench's stream_to_array: a zero-filled (3, N) grid with each
    component written at its sample offset (Z->0, N/1->1, E/2->2), missing
    components left as zeros. Returns (group_tmin, data, representative_nslc).
    """
    sampling_rate = float(spec["sampling_rate"])
    component_order = str(spec["component_order"])
    comp_row = {comp: i for i, comp in enumerate(component_order)}
    if "N" in comp_row:
        comp_row.setdefault("1", comp_row["N"])
    if "E" in comp_row:
        comp_row.setdefault("2", comp_row["E"])
    dt = 1.0 / sampling_rate
    staged: "OrderedDict[Tuple[str, str, str], Dict[int, List[Tuple[float, np.ndarray]]]]" = (
        OrderedDict()
    )
    rep: Dict[Tuple[str, str, str], NslcId] = {}
    for tr in traces:
        net, sta, loc, cha = tr.nslc_id
        if not cha:
            continue
        row = comp_row.get(cha[-1].upper())
        if row is None:
            continue
        try:
            seg = tr.chop(tmin, tmax, inplace=False, include_last=True)
        except Exception:
            continue
        if seg.ydata is None or len(seg.ydata) < 2:
            continue
        seg.ydata = seg.ydata.astype(np.float64)
        if scale_factor != 1.0:
            seg.deltat *= scale_factor
        if abs(seg.deltat - dt) > 1e-9:
            try:
                seg.resample(dt)
            except Exception:
                continue
        _apply_seisbench_filter(seg, spec)
        key = (net, sta, loc)
        # A component row collects *every* fragment, exactly as seisbench's
        # stream_to_array writes each trace at its offset — a channel split
        # across files must not shrink to whichever fragment came last.
        staged.setdefault(key, {}).setdefault(row, []).append(
            (float(seg.tmin), np.asarray(seg.ydata, dtype=np.float32))
        )
        if row == 0 or key not in rep:
            rep[key] = (net, sta, loc, cha)  # prefer the vertical channel as the label

    out: "OrderedDict[Tuple[str, str, str], Tuple[float, np.ndarray, NslcId]]" = OrderedDict()
    for key, comps in staged.items():
        g_tmin = min(t for frags in comps.values() for t, _ in frags)
        g_tmax = max(t + (len(y) - 1) * dt for frags in comps.values() for t, y in frags)
        n = int(round((g_tmax - g_tmin) * sampling_rate)) + 1
        if n < 4:
            continue
        data = np.zeros((len(component_order), n), dtype=np.float32)
        for row, frags in comps.items():
            for t0, y in frags:
                off = int(round((t0 - g_tmin) * sampling_rate))
                if off < 0:
                    y = y[-off:]
                    off = 0
                m = min(len(y), n - off)
                if m > 0:
                    data[row, off : off + m] = y[:m]
        out[key] = (g_tmin, data, rep[key])
    return out


async def _seisbench_pick_result(
    req: snuffling_pb2.SnufflingRequestParams,
) -> snuffling_pb2.SnufflingResult:
    """SeisBench deep-learning P/S picker over the selected marker spans, or
    the visible window when nothing is selected.

    The ONNX forward pass runs in the JS worker through onnxruntime-web; numpy
    pre/post-processing mirrors the pinned SeisBench 0.11.7 source.
    """
    import seisbench_infer as sbi

    params = req.snufflings.seisbench
    model = params.model or "PhaseNet"
    weights = params.weights or "instance"
    scale_factor = min(10.0, max(0.1, float(params.scale_factor or 1.0)))
    _replace_derived("seisbench", [])
    progress_prefix = "SeisBench %s/%s" % (model, weights)
    _seisbench_progress(0.0, "%s · loading model" % progress_prefix)
    model_url, spec = await _ensure_seisbench_model(model, weights, params.model_base_url or "")
    component_order = str(spec["component_order"])
    _seisbench_progress(0.01, "%s · preparing waveform chunks" % progress_prefix)
    # Qt's picker runs the marker-span scope through the pile chopper in
    # 300-second batches padded by one second plus the main high-pass filter's
    # settling margin (seisbench_picker.py:248), in `mode='visible'` — one pass
    # per selected span, over that span's channels only.
    # Collapsing several spans to their envelope would change the *output*,
    # not just the cost: the model would see, and pick in, the data between
    # disjoint markers, and a channel would be picked during a span whose
    # marker never selected it.
    tinc, model_pad = 300.0, 1.0
    use_view_filter = bool(params.use_predefined_filters)
    view_highpass = (
        float(req.view.highpass) if use_view_filter and req.view.HasField("highpass") else None
    )
    view_lowpass = (
        float(req.view.lowpass) if use_view_filter and req.view.HasField("lowpass") else None
    )
    filter_pad = 2.0 / view_highpass if view_highpass is not None else 0.0
    tpad = model_pad + filter_pad
    result = snuffling_pb2.SnufflingResult(show_window=True, origin="seisbench")
    markers = _MarkerAccumulator(req.marker_budget)
    n_p = n_s = 0
    annotation_traces: List[ptrace.Trace] = []
    # The bar's total comes from span arithmetic alone, so no pre-pass over
    # the data is needed: windows advance it in equal shares and the model's
    # mini-batches fill the current share.
    # Each chopper batch streams straight through inference and is released,
    # as Qt's loop does — collecting every station's resampled grid first
    # would hold the whole selection in memory at once.
    total_windows = max(
        1,
        sum(
            max(1, math.ceil((s_tmax - s_tmin) / tinc))
            for s_tmin, s_tmax, _sel in _snuffling_spans(req)
        ),
    )
    completed_windows = 0
    window_batches = 1
    done_in_window = 0

    def batch_done() -> None:
        nonlocal done_in_window
        done_in_window += 1
        share = min(1.0, done_in_window / window_batches)
        frac = min(0.99, 0.20 + 0.79 * (completed_windows + share) / total_windows)
        _seisbench_progress(frac, "%s · running inference" % progress_prefix)

    thresholds = {
        "P": params.p_threshold
        if params.HasField("p_threshold")
        else sbi.default_threshold(spec, "P"),
        "S": params.s_threshold
        if params.HasField("s_threshold")
        else sbi.default_threshold(spec, "S"),
    }
    # Distinct stations across all batches, for the summary line — overlapping
    # markers can run the same station twice, as Qt's per-marker chopper does.
    stations_picked = set()
    for batch in _chopper_selected_traces(req, mode="visible", tinc=tinc, tpad=tpad):
        # Qt drops every `tabu` batch trace before inference
        # (seisbench_picker.py:257): RMS curves and STA/LTA level traces are
        # derived data the model must never pick on.
        batch_traces = [tr for tr in batch.traces if not _is_tabu_trace(tr)]
        if not batch_traces:
            completed_windows += 1
            continue
        wmin, wmax = float(batch.tmin), float(batch.tmax)
        if use_view_filter:
            batch_traces = [
                _apply_seisbench_view_filter(tr, view_highpass, view_lowpass, filter_pad)
                for tr in batch_traces
            ]
        groups = _seisbench_groups(
            batch_traces,
            wmin - model_pad,
            wmax + model_pad,
            spec,
            scale_factor,
        )
        window_batches = max(
            1, sum(sbi.batch_count(data, spec) for _g_tmin, data, _nslc in groups.values())
        )
        done_in_window = 0
        for key, (g_tmin, data, nslc) in groups.items():
            stations_picked.add(key)
            ann = await sbi.annotate(model_url, data, spec, batch_done, progress_prefix)
            if params.show_annotation_traces and ann.data.size:
                labels = sbi.labels(spec)
                start = g_tmin + ann.start_offset / scale_factor
                if scale_factor != 1.0:
                    blinding = spec.get("blinding") or [0, 0]
                    blind_samples = max(int(value) for value in blinding)
                    start -= (blind_samples / 100.0) * (1.0 - 1.0 / scale_factor)
                for label_index, label in enumerate(labels):
                    if label == "N":
                        continue
                    annotation = ptrace.Trace(
                        network=key[0],
                        station=key[1],
                        location=key[2],
                        channel="%s_%s" % (model, label),
                        tmin=start,
                        deltat=1.0 / (ann.sampling_rate * scale_factor),
                        ydata=np.asarray(ann.data[:, label_index], dtype=np.float64),
                        meta={"tabu": True, "annotation": True},
                    )
                    try:
                        annotation.chop(wmin, wmax)
                    except ptrace.NoData:
                        continue
                    annotation_traces.append(annotation)
            # Qt's SeisBench picker uses kind 0 for P and 1 for S, marks the
            # picks automatic, and rides the pick probability on the marker's
            # incidence angle (seisbench_picker.py:341) — the number a user
            # most wants back from an ML picker.
            for phase, kind in (("P", 0), ("S", 1)):
                if phase not in sbi.labels(spec):
                    continue
                threshold = float(thresholds[phase])
                for idx, val in sbi.extract_picks(
                    ann.data[:, sbi.phase_channel(spec, phase)], threshold
                ):
                    model_time = g_tmin + ann.start_offset + idx / ann.sampling_rate
                    t_pick = g_tmin + (model_time - g_tmin) / scale_factor
                    # Qt keeps only picks inside the batch window
                    # (seisbench_picker.py:334): the padding exists for model
                    # context, and a pick in it would duplicate across
                    # neighbouring batches.
                    if not wmin <= t_pick < wmax:
                        continue
                    marker = snuffling_pb2.SnufMarker()
                    marker.t = t_pick
                    marker.kind = kind
                    marker.phase = phase
                    marker.automatic = True
                    marker.incidence_angle = float(val)
                    markers.add(marker, [nslc], float(val))
                    n_p, n_s = (n_p + 1, n_s) if phase == "P" else (n_p, n_s + 1)
        completed_windows += 1
    if not stations_picked:
        result.text = "SeisBench picker: no %s data in the visible window." % component_order
    else:
        result.text = "SeisBench %s/%s: %d P and %d S pick(s) across %d station(s)." % (
            model,
            weights,
            n_p,
            n_s,
            len(stations_picked),
        )
    _replace_derived("seisbench", annotation_traces)
    result.scene.CopyFrom(_scene_meta())
    _seisbench_progress(1.0, "%s · complete" % progress_prefix)
    return markers.finish(result)


def _snuffling_spans(
    req: snuffling_pb2.SnufflingRequestParams,
    skip_instant: bool = False,
) -> List[Tuple[float, float, Optional[Any]]]:
    """The processing scope Qt gives every snuffling (`chopper_selected_traces`,
    snuffling.py:1000): one ``(tmin, tmax, trace_selector)`` per selected
    non-event marker — the selector is the marker's channel patterns — or the
    visible window with no extra filter when nothing is selected.

    ``skip_instant`` reproduces STA/LTA's ``marker_selector`` (stalta.py:163),
    which drops zero-length picks from the scope."""
    from pyrocko import util as putil

    view = req.view
    spans: List[Tuple[float, float, Optional[Any]]] = []
    for s in req.selected_spans:
        if skip_instant and float(s.tmax) <= float(s.tmin):
            continue
        pats = ["%s.%s.%s.%s" % (n.net, n.sta, n.loc, n.cha) for n in s.nslc_ids]
        sel: Optional[Any] = None
        if pats:

            def sel(tr: ptrace.Trace, pats: List[str] = pats) -> bool:
                return bool(putil.match_nslc(pats, tr.nslc_id))

        spans.append((float(s.tmin), float(s.tmax), sel))
    if not spans:
        spans.append((float(view.tmin), float(view.tmax), None))
    return spans


def _matches_selection(tr: ptrace.Trace, spans: List[Tuple[float, float, Optional[Any]]]) -> bool:
    """Whether any selected span's channel patterns admit this trace — the
    union of `chopper_selected_traces`' per-marker selectors, for the runners
    that collapse the spans themselves to one window."""
    return any(sel is None or sel(tr) for _s0, _s1, sel in spans)


def _chopper_selected_traces(
    req: snuffling_pb2.SnufflingRequestParams,
    mode: str = "inview",
    skip_instant: bool = False,
    trace_filter: Optional[Callable[[ptrace.Trace], bool]] = None,
    **chopper_kwargs: Any,
) -> Iterator[Any]:
    """Qt's `Snuffling.chopper_selected_traces` (snuffling.py:952) over the
    real pile: one `Pile.chopper` run per selected non-event marker — its span
    and channel patterns become `tmin`/`tmax`/`trace_selector` — or one over
    the visible window when nothing is selected (`fallback=True` there).

    The viewer contributes its `mode` selector, intersected with the marker's, the
    composition Qt builds at snuffling.py:1072: the shown track rows for
    ``'inview'`` (the default there too), the hide/quick filters for
    ``'visible'``.
    Yields the chopper's batches, so degapping of file-split traces, `full_id`
    ordering and incomplete-window weeding come from pyrocko's own
    `_process_chopped` (pile.py:1213), not a reimplementation.
    """
    viewer_sel: Callable[[ptrace.Trace], bool]
    if mode == "inview":
        rows = {id(tr) for tr in _inview_work_traces(req.view)}

        def viewer_sel(tr: ptrace.Trace) -> bool:
            return id(tr) in rows
    elif mode == "visible":
        viewer_sel = _trace_selected
    else:
        # Qt raises on an unknown mode too; 'all' has no consumer here yet.
        raise ValueError("invalid mode argument: %r" % mode)

    p = _pile()
    for s_tmin, s_tmax, sel in _snuffling_spans(req, skip_instant=skip_instant):

        def selector(tr: ptrace.Trace, sel: Optional[Any] = sel) -> bool:
            return (
                viewer_sel(tr)
                and (trace_filter is None or trace_filter(tr))
                and (sel is None or sel(tr))
            )

        yield from p.chopper(
            tmin=s_tmin, tmax=s_tmax, trace_selector=selector, style="batch", **chopper_kwargs
        )


async def _run_snuffling_result(
    name: str, req: snuffling_pb2.SnufflingRequestParams
) -> snuffling_pb2.SnufflingResult:
    """Run a built-in snuffling over the current window."""
    if name == "cake":
        return _cake_run(req)

    if name == "seismosizer":
        return _seismosizer_run(req)

    if name == "minmax":
        # Qt's Minimum/Maximum reports *raw* pile values (minmax.py:42 passes
        # no `main_bandpass`), so no view passband here — the numbers
        # deliberately describe the data, not the screen.
        # Its chopper runs the default `mode='inview'` with `degap=False`, and
        # `trace.minmax` folds every chopped piece into one (min, max) per
        # NSLC (minmax.py:49) — a channel split across files or picked by
        # several markers still yields a single row.
        mima = ptrace.minmax(
            seg
            for batch in _chopper_selected_traces(req, degap=False)
            for seg in batch.traces
            if seg.ydata is not None and len(seg.ydata)
        )
        lines = ["%-16s %13s %13s %13s" % ("channel", "min", "max", "peak-to-peak")]
        for nslc in sorted(mima):
            mi, ma = (float(v) for v in mima[nslc])
            lines.append("%-16s %+.4e %+.4e %.4e" % (".".join(nslc), mi, ma, ma - mi))
        text = "\n".join(lines) if len(lines) > 1 else "No data in the visible window."
        return snuffling_pb2.SnufflingResult(show_window=True, text=text)

    if name == "stalta":
        stalta = req.snufflings.stalta
        shp = stalta.stalta_highpass if stalta.HasField("stalta_highpass") else None
        slp = stalta.stalta_lowpass if stalta.HasField("stalta_lowpass") else None
        tshort = float(stalta.stalta_swin or 30.0)
        ratio = float(stalta.stalta_ratio or 3.0)
        level = float(stalta.stalta_level or 0.5)
        variant = stalta.stalta_variant or "centered"
        scaling_name = stalta.stalta_scalingmethod or "[0-1]"
        apply_to_sum = bool(stalta.stalta_apply_to_sum)
        show_level_traces = bool(stalta.stalta_show_level_traces)
        scaling_method = {
            "[0-1]": 1,
            "[-1/ratio,1]": 2,
            "[-1/ratio,1] clipped to [0,1]": 3,
        }.get(scaling_name, 1)
        result = snuffling_pb2.SnufflingResult(show_window=False, origin="stalta")
        markers = _MarkerAccumulator(req.marker_budget)
        lwin = tshort * ratio
        derived: List[ptrace.Trace] = []
        deltat_min = min((tr.deltat for tr in _TRACES if tr.deltat > 0.0), default=0.01)
        tinc = max(lwin * 2.0, 500_000.0 * deltat_min)
        batches_done = 0
        for batch in _chopper_selected_traces(
            req,
            mode="visible",
            skip_instant=True,
            trace_filter=lambda trace: not _is_tabu_trace(trace),
            tinc=tinc,
            tpad=lwin,
            want_incomplete=False,
        ):
            batches_done += 1
            await _operation_safe_point(batches_done, None, "STA/LTA windows")
            sumtrace = None
            isum = 0
            for tr in batch.traces:
                if tr.ydata is None or len(tr.ydata) < 64:
                    continue
                tr.ydata = tr.ydata.astype(np.float64)
                if slp is not None:
                    tr.lowpass(4, float(slp), nyquist_exception=True)
                if shp is not None:
                    tr.highpass(4, float(shp), nyquist_exception=True)
                method = tr.sta_lta_right if variant == "right" else tr.sta_lta_centered
                try:
                    method(tshort, lwin, scalingmethod=scaling_method)
                except TypeError:
                    method(tshort, lwin)
                tr.chop(batch.tmin, batch.tmax)
                original_nslc = tr.nslc_id
                if not apply_to_sum:
                    tpeaks, _apeaks = tr.peaks(level, tshort)
                    for tp in tpeaks:
                        marker = snuffling_pb2.SnufMarker()
                        marker.t = float(tp)
                        markers.add(marker, [original_nslc])
                # Qt renames the level trace into location `cg` keeping the
                # channel (stalta.py:189) and tags it tabu.
                tr.set_codes(location="cg")
                tr.meta = {"tabu": True}
                if show_level_traces:
                    derived.append(tr)
                # The `.SUM.cg.` mean trace at the pile's finest sampling
                # (stalta.py:196), built whether or not it is shown.
                if sumtrace is None:
                    ny = int((tr.tmax - tr.tmin) / deltat_min)
                    if ny > 0:
                        sumtrace = ptrace.Trace(
                            network="",
                            station="SUM",
                            location="cg",
                            channel="",
                            deltat=deltat_min,
                            tmin=float(tr.tmin),
                            ydata=np.zeros(ny),
                            meta={"tabu": True},
                        )
                if sumtrace is not None:
                    sumtrace.add(tr, left=None, right=None)
                    isum += 1
            if sumtrace is not None and isum > 0:
                sumtrace.ydata = sumtrace.ydata / float(isum)
                if apply_to_sum:
                    tpeaks, _apeaks = sumtrace.peaks(level, tshort)
                    for tp in tpeaks:
                        marker = snuffling_pb2.SnufMarker()
                        marker.t = float(tp)
                        markers.add(marker, [("*", "*", "*", "*")])
                if show_level_traces:
                    derived.append(sumtrace)
        _replace_derived("stalta", derived if show_level_traces else [])
        result.scene.CopyFrom(_scene_meta())
        result.text = "STA/LTA: %d detection(s) (short=%.3gs, ratio=%g, level=%g)" % (
            markers.total,
            tshort,
            ratio,
            level,
        )
        return markers.finish(result)

    if name == "polarization":
        # Covariance-eigenvalue polarization analysis (Jurkevics, 1988) per
        # 3-component station — the text twin of the figure: the same
        # raw-pile chopper, marker-derived windows, component schemes, own
        # 4th-order filters with settling pad, demeaning and rotation
        # convention, reported as numbers.
        polarization = req.snufflings.polarization
        php = (
            polarization.polarization_highpass
            if polarization.HasField("polarization_highpass")
            else None
        )
        plp = (
            polarization.polarization_lowpass
            if polarization.HasField("polarization_lowpass")
            else None
        )
        t_length = float(polarization.polarization_t_length or 1.0)
        ft_offset = float(polarization.polarization_ft_offset)
        azimuth = float(polarization.polarization_azimuth)

        windows: List[Tuple[Tuple[str, str, str], str, float, float]] = []
        for s in req.selected_spans:
            w_tmin = float(s.tmin) + t_length * ft_offset
            w_tmax = w_tmin + t_length
            for pn in s.nslc_ids:
                windows.append(
                    ((pn.net, pn.sta, pn.loc), pn.cha[:-1] if pn.cha else "", w_tmin, w_tmax)
                )
        if not windows:
            return snuffling_pb2.SnufflingResult(
                show_window=True,
                text=(
                    "No selected markers.\n\nCreate and select markers at points "
                    "of interest. Normal and phase markers are accepted."
                ),
            )

        lines = ["%-16s %8s %8s %6s %6s" % ("station", "az[°]", "inc[°]", "rect", "plan")]
        for nsl, chagroup, w_tmin, w_tmax in windows:
            by_channel: Dict[str, np.ndarray] = {}
            trs: Dict[str, ptrace.Trace] = {}
            # The same extraction the figure runs — raw pile, channel pattern
            # only, complete padded windows — then chopped to the bare analysis
            # window, since only the numbers are reported.
            for cha, seg in _polarization_channels(
                nsl, chagroup, w_tmin, w_tmax, php, plp, t_length
            ).items():
                try:
                    seg = seg.chop(w_tmin, w_tmax, inplace=False)
                except Exception:  # noqa: BLE001 — window not covered
                    continue
                if seg.ydata is None or len(seg.ydata) < 8:
                    continue
                trs[cha] = seg
            group = _polarization_scheme(trs) if trs else None
            if group is None or len(group) != 3:
                continue
            te_, tn_, tz_ = group
            for key, seg in (("E", te_), ("N", tn_), ("Z", tz_)):
                y = np.asarray(seg.ydata, dtype=np.float64)
                by_channel[key] = y - float(np.mean(y))
            m = min(len(by_channel[k]) for k in ("Z", "N", "E"))
            if m < 8:
                continue
            yz = by_channel["Z"][:m]
            yn = by_channel["N"][:m]
            ye = by_channel["E"][:m]
            if azimuth != 0.0:
                # pyrocko's `trace.rotate` matrix, the same convention the
                # figure draws with: n' = cos·n + sin·e, e' = −sin·n + cos·e.
                theta = np.radians(azimuth)
                yn, ye = (
                    np.cos(theta) * yn + np.sin(theta) * ye,
                    -np.sin(theta) * yn + np.cos(theta) * ye,
                )
            data = np.vstack([yz, yn, ye])
            data = data - data.mean(axis=1, keepdims=True)
            evals, evecs = np.linalg.eigh(np.cov(data))  # ascending
            order_ev = np.argsort(evals)[::-1]
            l1, l2, l3 = (float(evals[i]) for i in order_ev)
            vz, vn, ve = evecs[:, order_ev[0]]
            rect = 1.0 - (l2 + l3) / (2.0 * l1) if l1 > 0.0 else 0.0
            plan = 1.0 - 2.0 * l3 / (l1 + l2) if (l1 + l2) > 0.0 else 0.0
            az = float(np.degrees(np.arctan2(ve, vn))) % 360.0
            inc = float(np.degrees(np.arccos(min(1.0, abs(float(vz))))))
            lines.append(
                "%-16s %8.1f %8.1f %6.2f %6.2f"
                % (".".join(x for x in nsl if x), az, inc, rect, plan)
            )

        text = (
            "\n".join(lines)
            if len(lines) > 1
            else "Need a selected marker over 3-component (Z + 2 horizontal) data."
        )
        return snuffling_pb2.SnufflingResult(show_window=True, text=text)

    return snuffling_pb2.SnufflingResult(show_window=True, text="Unknown snuffling: %s" % name)


# --- FDSN web services (network) -------------------------------------------
# These helpers build the FDSN request URLs and parse the responses. The fetch
# runs in Python too (pyfetch, in the async servicer methods above), so the whole
# workflow is one rpc and the worker stays a dumb relay.


def _dataselect_url(req: data_pb2.WaveformDownloadRequest) -> str:
    """FDSN dataselect (waveform) query URL."""
    base = _fdsn_base(req.datacenter)
    q = "&".join(
        [
            "net=%s" % (req.net or "*"),
            "sta=%s" % (req.sta or "*"),
            "loc=%s" % (req.loc or "*"),
            "cha=%s" % (req.cha or "*"),
            "starttime=%s" % req.start,
            "endtime=%s" % req.end,
            "nodata=204",
        ]
    )
    return "%s/fdsnws/dataselect/1/query?%s" % (base, q)


def _load_waveform_path_scene(path: str) -> scene_pb2.SceneMeta:
    """Load staged FDSN MiniSEED bytes into the pile and return the scene.

    Appends: Qt's download snuffling adds the fetched traces to whatever is
    loaded (`Pile.add_file` via download.py:140), it never replaces the pile.
    """
    if not path:
        return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
    traces = list(pio.load(path, format="mseed"))
    _set_traces(traces, append=True)
    return _scene_meta()


def _chop_trace_from(tr: ptrace.Trace, tmin: float) -> Optional[ptrace.Trace]:
    if not math.isfinite(tmin) or tmin <= 0.0 or tr.tmin >= tmin:
        return tr
    if tr.tmax < tmin:
        return None
    try:
        return tr.chop(tmin, tr.tmax, inplace=False, include_last=True)
    except ptrace.NoData:
        return None


def _trim_live_traces(traces: List[ptrace.Trace], tmin: float) -> List[ptrace.Trace]:
    out = []
    for tr in traces:
        chopped = _chop_trace_from(tr, tmin)
        if chopped is not None:
            out.append(chopped)
    return out


def _append_mseed_scene(
    source: str,
    raw: bytes,
    start_time: float = 0.0,
    reset: bool = False,
    retain_seconds: float = 0.0,
) -> scene_pb2.SceneMeta:
    """Append live MiniSEED records and merge adjacent segments by NSLC/time."""
    global _TRACES
    del source
    if not raw:
        return _scene_meta()

    traces = list(pio.load(_stage_bytes("live.mseed", raw), format="mseed"))
    if reset:
        # Parsing has succeeded, so replacement and insertion now belong to the
        # same dispatcher transaction. Malformed batch zero leaves the old scene.
        _set_traces([])
    if start_time > 0.0:
        traces = _trim_live_traces(traces, start_time)
    if not traces:
        return _scene_meta()

    merged_input = list(_TRACES) + traces
    if retain_seconds > 0.0 and merged_input:
        latest = max(tr.tmax for tr in merged_input)
        retain_from = latest - retain_seconds
        if start_time > 0.0:
            retain_from = max(start_time, retain_from)
        merged_input = _trim_live_traces(merged_input, retain_from)
    elif start_time > 0.0:
        merged_input = _trim_live_traces(merged_input, start_time)

    merged = sorted(
        merged_input,
        key=lambda tr: (
            tr.network,
            tr.station,
            tr.location,
            tr.channel,
            tr.tmin,
        ),
    )
    _TRACES = ptrace.degapper(merged)
    _mark_traces_changed()
    return _scene_meta()


def _event_url(req: data_pb2.CatalogRequest) -> str:
    """FDSN event (catalog) query URL. Text format by default; full QuakeML with
    all origins (so focal-mechanism moment tensors are included) when the request
    asks for MTs — this is the browser-feasible MT source (GlobalCMT/GEOFON-mt are
    blocked by mixed-content/CORS)."""
    base = _fdsn_base(req.datacenter)
    if req.with_mt:
        q = "&".join(
            [
                "format=xml",
                "starttime=%s" % req.start,
                "endtime=%s" % req.end,
                "minmagnitude=%s" % req.minmag,
                "includeallorigins=true",
                "orderby=time",
            ]
        )
    else:
        q = "&".join(
            [
                "format=text",
                "starttime=%s" % req.start,
                "endtime=%s" % req.end,
                "minmagnitude=%s" % req.minmag,
                "orderby=time",
                "limit=1000",
            ]
        )
    return "%s/fdsnws/event/1/query?%s" % (base, q)


def _ftime(s: str) -> float:
    s = s.strip().rstrip("Z")
    try:
        return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()
    except Exception:
        return 0.0


def _parse_events_result(text: str, budget: _MarkerBudget = None) -> snuffling_pb2.SnufflingResult:
    """Parse FDSN event 'text' format into event markers; set the active event
    (largest magnitude) for distance/azimuth sorting.

    Every event carries its full origin (lat/lon/depth/magnitude), its identity
    (event id, region, catalog) and is put in ``_EVENT_REGISTRY``, so ``e`` on
    one activates the *real* origin — and the auto-activated marker itself is
    flagged ``active`` so the state is visible on the canvas and in the table.
    """
    from pyrocko import model
    from pyrocko import orthodrome as od

    result = snuffling_pb2.SnufflingResult(show_window=True)
    markers = _MarkerAccumulator(budget)
    lines: List[str] = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        c = line.split("|")
        if len(c) < 11:
            continue
        t = _ftime(c[1])
        lat = float(c[2]) if c[2] else 0.0
        lon = float(c[3]) if c[3] else 0.0
        depth = (float(c[4]) if c[4] else 0.0) * 1000.0
        mag = float(c[10]) if len(c) > 10 and c[10] else 0.0
        # FDSN text columns: EventID | Time | Lat | Lon | Depth | Author |
        # Catalog | ... | Magnitude | MagAuthor | EventLocationName.
        event_id = c[0].strip()
        catalog = c[6].strip() if len(c) > 6 else ""
        region = c[12] if len(c) > 12 else ""
        when = c[1].replace("T", " ")[:19]
        ev = model.Event(
            time=t,
            lat=lat,
            lon=lon,
            depth=depth,
            magnitude=mag,
            name=event_id or None,
            region=region or None,
            catalog=catalog or None,
        )
        h = event_id or str(ev.get_hash())
        marker = snuffling_pb2.SnufMarker()
        marker.t = t
        marker.is_event = True
        marker.event_hash = h
        if event_id:
            marker.event_name = event_id
        if region:
            marker.region = region
        if catalog:
            marker.catalog = catalog
        marker.magnitude = mag
        marker.lat = lat
        marker.lon = lon
        marker.depth = depth
        if len(lines) < 30:
            lines.append("M%.1f  %s  %s" % (mag, when, region))
        markers.add(marker, strength=mag, payload=ev)
    retained = markers.retained_candidates()
    best = max(
        retained,
        key=lambda item: float(item[0].magnitude) if item[0].HasField("magnitude") else 0.0,
        default=None,
    )
    for marker, event in retained:
        _register_event(marker.event_hash, event)
    if best is not None:
        best_marker = best[0]
        _activate_event(best_marker.event_hash)
        best_marker.active = True
        for marker, _event in retained:
            marker.dist_active_km = (
                float(
                    od.distance_accurate50m(
                        float(marker.lat),
                        float(marker.lon),
                        float(best_marker.lat),
                        float(best_marker.lon),
                    )
                )
                / 1000.0
            )
        header = (
            "%d event(s) found; active event M%.1f (distance sort enabled).\n"
            "Jumped the view to the events — load waveforms for this time to "
            "see them on data." % (markers.total, float(best_marker.magnitude))
        )
    else:
        _activate_event(None)
        header = "No events found in this time window."
    body = "\n".join(lines)
    result.text = header + ("\n\n" + body if body else "")
    return _finalize_event_markers(result, markers)


def _short_event_name(name: Optional[str]) -> str:
    """A compact event code from a QuakeML public id, e.g.
    ``quakeml:earthquake.usgs.gov/...?eventid=us6000jllz&...`` → ``us6000jllz`` or
    ``quakeml:us.anss.org/event/us6000jllz`` → ``us6000jllz``."""
    s = name or ""
    match = re.search(r"eventid=(\w+)", s)
    if match:
        return match.group(1)
    return s.rstrip("/").rsplit("/", 1)[-1].rsplit(":", 1)[-1]


def _parse_events_quakeml(text: str, budget: _MarkerBudget = None) -> snuffling_pb2.SnufflingResult:
    """Parse an FDSN QuakeML event response into event markers carrying moment
    tensors, and precompute the Kagan angle of every MT event to the active
    (largest-magnitude) event. Reuses pyrocko's own QuakeML reader + MomentTensor
    (no seismology reimplemented in Rust). Also populates the event registry and
    ``_ACTIVE_EVENT`` so the Map overlay and distance sort keep working."""
    from pyrocko import moment_tensor as mtm
    from pyrocko import orthodrome as od
    from pyrocko import util

    # quakeml.py eagerly imports pyrocko.gui.snuffler.marker; install the stub that
    # avoids running the Qt snuffler package init first (same trick as markers IO).
    _marker_module()
    from pyrocko.io import quakeml

    result = snuffling_pb2.SnufflingResult(show_window=True)
    markers = _MarkerAccumulator(budget)
    qml = quakeml.QuakeML.load_xml(string=text)
    events = qml.get_pyrocko_events()

    entries = []
    best = None
    for ev in events:
        mag = float(ev.magnitude) if ev.magnitude is not None else 0.0
        lat = float(ev.lat)
        lon = float(ev.lon)
        depth = float(ev.depth) if ev.depth is not None else 0.0
        entries.append((ev, mag, lat, lon, depth))
        if best is None or mag > best[1]:
            best = (ev, mag, lat, lon, depth)

    if best is not None:
        best_event = best[0]
        best_hash = str(best_event.name) if best_event.name else str(best_event.get_hash())
        _register_event(best_hash, best_event)
        _activate_event(best_hash)
    else:
        _activate_event(None)

    lines: List[str] = []
    for ev, mag, _lat, _lon, _depth in entries:
        event_hash = str(ev.name) if ev.name else str(ev.get_hash())
        short = _short_event_name(ev.name)
        marker = snuffling_pb2.SnufMarker()
        marker.t = float(ev.time)
        marker.is_event = True
        marker.event_hash = event_hash
        if short:
            marker.event_name = short
        if getattr(ev, "region", None):
            marker.region = str(ev.region)
        if getattr(ev, "catalog", None):
            marker.catalog = str(ev.catalog)
        marker.magnitude = mag
        marker.lat = float(ev.lat)
        marker.lon = float(ev.lon)
        if ev.depth is not None:
            marker.depth = float(ev.depth)
        if best is not None:
            # The auto-activated event is *visible*: the largest-magnitude
            # marker carries the active flag, and every event row gets its
            # distance to it for the table's Dist [km] column.
            if ev is best[0]:
                marker.active = True
            marker.dist_active_km = (
                float(od.distance_accurate50m(float(ev.lat), float(ev.lon), best[2], best[3]))
                / 1000.0
            )
        mt = ev.moment_tensor
        if mt is not None:
            mnn, mee, mdd, mne, mnd, med = (float(x) for x in mt.m6())
            marker.moment_tensor.mnn = mnn
            marker.moment_tensor.mee = mee
            marker.moment_tensor.mdd = mdd
            marker.moment_tensor.mne = mne
            marker.moment_tensor.mnd = mnd
            marker.moment_tensor.med = med
            if _ACTIVE_MT is not None:
                marker.kagan_deg = float(mtm.kagan_angle(_ACTIVE_MT, mt))
        when = util.time_to_str(ev.time)[:19]
        if len(lines) < 30:
            lines.append("M%.1f  %s  %s%s" % (mag, when, short, "  [MT]" if mt is not None else ""))
        markers.add(marker, strength=mag, payload=ev)

    n_mt = sum(1 for e, *_ in entries if e.moment_tensor is not None)
    retained = markers.retained_candidates()
    for marker, event in retained:
        _register_event(marker.event_hash, event)
    retained_best = max(
        retained,
        key=lambda item: float(item[0].magnitude) if item[0].HasField("magnitude") else 0.0,
        default=None,
    )
    if retained_best is not None:
        retained_best[0].active = True
        _activate_event(retained_best[0].event_hash)
    else:
        _activate_event(None)
    if best is not None:
        header = (
            "%d event(s) found, %d with moment tensors; active event M%.1f "
            "(distance sort + Kagan enabled)." % (markers.total, n_mt, best[1])
        )
    else:
        header = "No events found in this time window."
    body = "\n".join(lines)
    result.text = header + ("\n\n" + body if body else "")
    return _finalize_event_markers(result, markers)


def _moment_tensor_message(moment_tensor: Any) -> Optional[common_pb2.MomentTensor]:
    if moment_tensor is None:
        return None
    mnn, mee, mdd, mne, mnd, med = (float(value) for value in moment_tensor.m6())
    return common_pb2.MomentTensor(mnn=mnn, mee=mee, mdd=mdd, mne=mne, mnd=mnd, med=med)


def _event_snapshot(event_hash: str, event: Any) -> common_pb2.EventSnapshot:
    snapshot = common_pb2.EventSnapshot(
        event_hash=event_hash,
        time=float(event.time),
        lat=float(event.lat),
        lon=float(event.lon),
        depth=float(event.depth or 0.0),
    )
    if event.magnitude is not None:
        snapshot.magnitude = float(event.magnitude)
    for field in ("name", "region", "catalog"):
        value = getattr(event, field, None)
        if value:
            setattr(snapshot, field, str(value))
    moment_tensor = _moment_tensor_message(getattr(event, "moment_tensor", None))
    if moment_tensor is not None:
        snapshot.moment_tensor.CopyFrom(moment_tensor)
    return snapshot


def _event_state(limit: Optional[int] = None) -> common_pb2.ActiveEventResult:
    """Project the authoritative event registry and its active-event metrics."""
    from pyrocko import moment_tensor as mtm
    from pyrocko import orthodrome as od

    active = _EVENT_REGISTRY.get(_ACTIVE_HASH) if _ACTIVE_HASH else None
    active_mt = getattr(active, "moment_tensor", None) if active is not None else None
    result = common_pb2.ActiveEventResult(has_event=active is not None)
    if active is not None:
        result.active_hash = _ACTIVE_HASH
    bounded = max(1, min(limit or DEFAULT_MARKER_BUDGET, MAX_MARKER_BUDGET))
    items = list(_EVENT_REGISTRY.items())
    selected = items[-bounded:]
    if _ACTIVE_HASH and all(event_hash != _ACTIVE_HASH for event_hash, _event in selected):
        active_item = next(
            (item for item in items if item[0] == _ACTIVE_HASH),
            None,
        )
        if active_item is not None:
            selected = [active_item, *selected[-(bounded - 1) :]] if bounded > 1 else [active_item]
    for event_hash, event in selected:
        result.events.add().CopyFrom(_event_snapshot(event_hash, event))
        metric = result.kagan.add()
        metric.event_hash = event_hash
        moment_tensor = getattr(event, "moment_tensor", None)
        if active_mt is not None and moment_tensor is not None:
            metric.kagan_deg = float(mtm.kagan_angle(active_mt, moment_tensor))
        if active is not None:
            metric.dist_km = (
                float(
                    od.distance_accurate50m(
                        float(event.lat),
                        float(event.lon),
                        float(active.lat),
                        float(active.lon),
                    )
                )
                / 1000.0
            )
    return result


def _set_active_event(req: data_pb2.SetActiveEventRequest) -> common_pb2.ActiveEventResult:
    """Set (or clear) the user-chosen active event — the single reference every
    downstream consumer (station distances, Kagan column, cake/seismosizer origin,
    map centre) reads from ``_ACTIVE_EVENT``/``_ACTIVE_MT``. Faithful port of
    snuffler ``set_active_event_marker`` / ``deactivate_event_marker``; recomputes
    the Kagan angle of every registered event against the new active MT."""
    from pyrocko import model

    if req.deactivate:
        _activate_event(None)
        return _event_state()

    ev = _EVENT_REGISTRY.get(req.event_hash) if req.event_hash else None
    if ev is None:
        # Hand-made event marker: derive lat/lon from the marker's single station if
        # it is bound to one, else keep the previous active event's location (Qt).
        lat = lon = 0.0
        bound = (
            _station_at(req.nslc.net, req.nslc.sta, req.nslc.loc) if req.HasField("nslc") else None
        )
        if bound is not None:
            lat, lon, _elev, _depth = bound
        elif _ACTIVE_EVENT is not None:
            lat, lon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
        ev = model.Event(time=float(req.t), lat=lat, lon=lon, depth=0.0)
        if req.event_hash:
            _register_event(req.event_hash, ev)

    _activate_event(req.event_hash or None)

    return _event_state()


def _update_event(snapshot: common_pb2.EventSnapshot) -> common_pb2.ActiveEventResult:
    """Replace one registry event from the frontend's complete edited snapshot."""
    from pyrocko import model
    from pyrocko import moment_tensor as mtm

    if not snapshot.event_hash:
        raise ValueError("event update requires an event_hash")
    moment_tensor = None
    if snapshot.HasField("moment_tensor"):
        tensor = snapshot.moment_tensor
        moment_tensor = mtm.MomentTensor(
            mnn=tensor.mnn,
            mee=tensor.mee,
            mdd=tensor.mdd,
            mne=tensor.mne,
            mnd=tensor.mnd,
            med=tensor.med,
        )
    event = model.Event(
        time=float(snapshot.time),
        lat=float(snapshot.lat),
        lon=float(snapshot.lon),
        depth=float(snapshot.depth),
        magnitude=(float(snapshot.magnitude) if snapshot.HasField("magnitude") else None),
        name=(snapshot.name if snapshot.HasField("name") else None),
        region=(snapshot.region if snapshot.HasField("region") else None),
        catalog=(snapshot.catalog if snapshot.HasField("catalog") else None),
        moment_tensor=moment_tensor,
    )
    _register_event(snapshot.event_hash, event)
    if snapshot.event_hash == _ACTIVE_HASH:
        _activate_event(snapshot.event_hash)
    return _event_state()


def _remove_events(request: data_pb2.RemoveEventsRequest) -> common_pb2.ActiveEventResult:
    event_hashes = set(request.event_hashes)
    for event_hash in event_hashes:
        _EVENT_REGISTRY.pop(event_hash, None)
    if _ACTIVE_HASH in event_hashes:
        _activate_event(None)
    return _event_state()


def _station_url(req: data_pb2.StationRequest) -> str:
    """FDSN station query URL — channel level, as Qt's download resolves them
    (download.py:78), with the location, the time span, and the optional
    radius search around the active event's epicentre."""
    base = _fdsn_base(req.datacenter)
    extra = []
    if req.start:
        extra.append("starttime=%s" % req.start)
    if req.end:
        extra.append("endtime=%s" % req.end)
    if req.HasField("radius_deg") and _ACTIVE_EVENT is not None:
        extra.append("latitude=%.5f" % _ACTIVE_EVENT[0])
        extra.append("longitude=%.5f" % _ACTIVE_EVENT[1])
        extra.append("maxradius=%.4f" % float(req.radius_deg))
    q = "&".join(
        [
            "net=%s" % (req.net or "*"),
            "sta=%s" % (req.sta or "*"),
            "loc=%s" % (req.loc or "*"),
            "cha=%s" % (req.cha or "*"),
            *extra,
            "level=channel",
            "format=text",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def _parse_stations_result(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse FDSN station text and remember coordinates for sorting/cake/map.

    The query asks for *channel* level, whose rows carry the location code and
    the sensor depth (`#Net|Sta|Loc|Cha|Lat|Lon|Elev|Depth|…`) — exactly what
    the per-location station table wants. Station-level rows
    (`#Net|Sta|Lat|Lon|Elev|Site|…`) still parse, as the fallback for servers
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
                _add_station(c[0], c[1], c[2], lat, lon, elev, depth)
            else:
                if len(c) < 5:
                    continue
                key = (c[0], c[1])
                _STATIONS[key] = (float(c[2]), float(c[3]), float(c[4]), 0.0)
            if key not in seen:
                seen.add(key)
                n += 1
        except Exception:
            pass
    return snuffling_pb2.SnufflingResult(
        show_window=True, text="%d station(s) with coordinates loaded" % n
    )


def _load_stations_result(text: str) -> snuffling_pb2.SnufflingResult:
    """Parse a local pyrocko station file into the ``_STATIONS`` coordinate table
    (distance/azimuth sorting, cake, map), via pyrocko's own ``model.load_stations``."""
    global _STATIONS
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
    for st in stations:
        try:
            _add_station(
                st.network,
                st.station,
                st.location,
                float(st.lat),
                float(st.lon),
                float(st.elevation),
                float(st.depth),
            )
            n += 1
        except Exception:  # noqa: BLE001 — skip malformed entries
            pass
    return snuffling_pb2.SnufflingResult(
        show_window=True, text="%d station(s) with coordinates loaded" % n
    )


def _station_distances_result() -> data_pb2.StationDistanceList:
    """Great-circle distance/azimuth/backazimuth (degrees) from the active event
    to each known station, via pyrocko's ``orthodrome`` — for the marker/station
    distance columns. Empty (``has_event=False``) until an event is active."""
    result = data_pb2.StationDistanceList(has_event=_ACTIVE_EVENT is not None)
    if _ACTIVE_EVENT is None or not _STATIONS:
        return result
    from pyrocko import orthodrome as od

    elat, elon = _ACTIVE_EVENT[0], _ACTIVE_EVENT[1]
    for (net, sta), (slat, slon, _elev, _depth) in _stations_by_name().items():
        entry = result.stations.add()
        entry.net = net
        entry.sta = sta
        entry.distance_deg = _dist_deg(elat, elon, slat, slon)
        entry.azimuth_deg = float(od.azimuth(elat, elon, slat, slon)) % 360.0
        entry.backazimuth_deg = float(od.azimuth(slat, slon, elat, elon)) % 360.0
    return result


def _load_events_result(text: str, budget: _MarkerBudget = None) -> snuffling_pb2.SnufflingResult:
    """Parse a local pyrocko event file into event markers and set the active
    event (largest magnitude, for distance/azimuth sorting), via pyrocko's own
    ``model.load_events``. Mirrors the FDSN catalog path."""
    if not text.strip():
        _activate_event(None)
        result = snuffling_pb2.SnufflingResult(show_window=True, text="empty event file")
        result.event_state.CopyFrom(_event_state())
        return result
    import tempfile

    from pyrocko import model

    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as handle:
        handle.write(text)
        path = handle.name
    try:
        events = model.load_events(path)
    except Exception as exc:  # noqa: BLE001 — surface a parse failure as text
        return snuffling_pb2.SnufflingResult(
            show_window=True, text="could not parse event file: %s" % exc
        )
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass

    from pyrocko import orthodrome as od

    result = snuffling_pb2.SnufflingResult(show_window=True)
    markers = _MarkerAccumulator(budget)
    for ev in events:
        lat = float(ev.lat)
        lon = float(ev.lon)
        depth = float(ev.depth or 0.0)
        mag = float(ev.magnitude) if ev.magnitude is not None else 0.0
        # The full origin and identity ride the marker, and the event goes in
        # the registry, so activating it later uses the real origin instead of
        # a rebuilt zero-depth stand-in.
        h = str(ev.name) if ev.name else str(ev.get_hash())
        marker = snuffling_pb2.SnufMarker()
        marker.t = float(ev.time)
        marker.is_event = True
        marker.event_hash = h
        if ev.name:
            marker.event_name = str(ev.name)[:96]
        if getattr(ev, "region", None):
            marker.region = str(ev.region)
        if getattr(ev, "catalog", None):
            marker.catalog = str(ev.catalog)
        marker.magnitude = mag
        marker.lat = lat
        marker.lon = lon
        marker.depth = depth
        markers.add(marker, strength=mag, payload=ev)
    retained = markers.retained_candidates()
    best = max(
        retained,
        key=lambda item: float(item[0].magnitude) if item[0].HasField("magnitude") else 0.0,
        default=None,
    )
    for marker, event in retained:
        _register_event(marker.event_hash, event)
    if best is not None:
        best_marker = best[0]
        _activate_event(best_marker.event_hash)
        # Auto-activation stays, made visible on the marker itself.
        best_marker.active = True
        for marker, _event in retained:
            marker.dist_active_km = (
                float(
                    od.distance_accurate50m(
                        float(marker.lat),
                        float(marker.lon),
                        float(best_marker.lat),
                        float(best_marker.lon),
                    )
                )
                / 1000.0
            )
        result.text = "%d event(s) loaded; active event M%.1f (distance sort enabled)." % (
            markers.total,
            float(best_marker.magnitude),
        )
    else:
        _activate_event(None)
        result.text = "No events in file."
    return _finalize_event_markers(result, markers)


def _response_url(req: data_pb2.StationRequest) -> str:
    """FDSN station query URL at response level (StationXML with instrument
    responses, for deconvolution)."""
    base = _fdsn_base(req.datacenter)
    q = "&".join(
        [
            "net=%s" % (req.net or "*"),
            "sta=%s" % (req.sta or "*"),
            "cha=%s" % (req.cha or "*"),
            "level=response",
        ]
    )
    return "%s/fdsnws/station/1/query?%s" % (base, q)


def _load_responses_result(xml_texts: List[str]) -> snuffling_pb2.SnufflingResult:
    """Parse StationXML documents into instrument responses.

    Appended to the document list rather than merged into one: snuffler keeps its
    ``station_xmls`` as a list in load order and resolves each channel against
    the first document that answers (pile_viewer.py:4470), so a second load adds
    responses instead of discarding the first load's, and a channel described in
    two documents restitutes instead of raising on the merge's ambiguity.

    Station coordinates are also harvested for distance/azimuth sorting.
    """
    global _RESPONSE_GEN
    xml_texts = [text for text in xml_texts if text.strip()]
    if not xml_texts:
        return snuffling_pb2.SnufflingResult(show_window=True, text="no station metadata returned")
    from pyrocko.io import stationxml

    station_xmls = [stationxml.load_xml(string=text) for text in xml_texts]
    _STATIONXMLS.extend(station_xmls)
    _RESPONSE_GEN += 1
    _WARNED_NO_RESPONSE.clear()
    _clear_view_cache()
    nchan = sum(
        len(st.channel_list)
        for sx in station_xmls
        for net in sx.network_list
        for st in net.station_list
    )
    # Coordinates via `get_pyrocko_stations`, the same call snuffler feeds
    # `--stationxml` files through — not the station element's own lat/lon.
    # The two can genuinely disagree (real files carry channel positions ~100 m
    # away from the station element), and snuffler's distance/azimuth sorts follow
    # the channel-derived position, epoch handling and all.
    for st in (station for sx in station_xmls for station in sx.get_pyrocko_stations()):
        try:
            _add_station(
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


# One archive carrying a whole dataset — waveforms, station metadata and an event
# catalogue as the datacenter delivered them. `File ▸ Examples` fetches these, but
# nothing below is specific to the ones this app ships: members are dispatched by
# what they are, so a zip someone assembled by hand loads exactly the same way. That
# is the point of using a zip at all — a dataset is never one file, and handing out
# one file is the difference between a teaching aid and a set of instructions.
_ZIP_MAGIC = b"PK\x03\x04"

# What the shipped bundles carry so a downloaded copy still says what it is.
_BUNDLE_MANIFEST = "snuffler.json"

# Prose a bundle may carry beside its data, which no loader wants.
_BUNDLE_SKIP_SUFFIXES = (".json", ".md", ".txt", ".py", ".cfg", ".toml")


class _DatasetState(NamedTuple):
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
    active_hash: str
    active_mt: Any
    event_registry: "OrderedDict[str, Any]"
    stationxmls: List[Any]
    warned_no_response: "set[NslcId]"
    hidden: List[str]
    reduce: Optional[List[str]]
    scaling_hooks: "OrderedDict[str, Tuple[Optional[float], Optional[float]]]"
    response_gen: int
    gf_stores: Dict[str, str]


class SessionSnapshot(NamedTuple):
    generation: int
    state: _DatasetState


class SessionUpdate(NamedTuple):
    state: _DatasetState


class StaleSessionCommit(NamedTuple):
    """A rejected commit: the store moved past the transaction's generation."""

    expected_generation: int
    actual_generation: int


def _clone_dataset_state(state: _DatasetState) -> _DatasetState:
    trace_copies: Dict[int, ptrace.Trace] = {}
    # One memo across every event-object-carrying field: _replace_derived_events
    # retires registry entries by object identity, and _ACTIVE_MT aliases the
    # registered event's moment tensor. Independent deepcopy calls would give
    # the workspace two distinct copies of the same event, so retirement would
    # silently stop matching and superseded derived events would leak in the
    # registry forever. (_ACTIVE_EVENT needs no copy: it is an immutable float
    # tuple snapshot, not a reference into the registry.)
    event_memo: Dict[int, Any] = {}

    def clone_trace(trace: ptrace.Trace) -> ptrace.Trace:
        cloned = trace_copies.get(id(trace))
        if cloned is None:
            cloned = trace.copy(data=True)
            trace_copies[id(trace)] = cloned
        return cloned

    def clone_traces(traces: Iterable[ptrace.Trace]) -> List[ptrace.Trace]:
        return [clone_trace(trace) for trace in traces]

    return _DatasetState(
        traces=clone_traces(state.traces),
        source_traces=OrderedDict(
            (source_id, clone_traces(traces)) for source_id, traces in state.source_traces.items()
        ),
        source_digests=dict(state.source_digests),
        suppressed_source_traces={
            source_id: clone_traces(traces)
            for source_id, traces in state.suppressed_source_traces.items()
        },
        digest_owners={digest: set(owners) for digest, owners in state.digest_owners.items()},
        derived_traces={
            origin: clone_traces(traces) for origin, traces in state.derived_traces.items()
        },
        derived_events=copy.deepcopy(state.derived_events, event_memo),
        pile_is_example=state.pile_is_example,
        stations=dict(state.stations),
        active_event=state.active_event,
        active_hash=state.active_hash,
        active_mt=copy.deepcopy(state.active_mt, event_memo),
        event_registry=copy.deepcopy(state.event_registry, event_memo),
        stationxmls=copy.deepcopy(state.stationxmls),
        warned_no_response=set(state.warned_no_response),
        hidden=list(state.hidden),
        reduce=None if state.reduce is None else list(state.reduce),
        scaling_hooks=OrderedDict(state.scaling_hooks),
        response_gen=state.response_gen,
        gf_stores=dict(state.gf_stores),
    )


class _ViewLaneCaches(NamedTuple):
    data_gen: int
    cutout_vec: Optional[Tuple[Any, ...]]
    cutout_traces: Optional[List[ptrace.Trace]]
    cutout_notices: List[str]
    tf_cache: Dict[Tuple[Any, ...], np.ndarray]
    pile: Optional[Any]
    pile_gen: Optional[int]
    box_styles: Dict[Tuple[Any, ...], int]
    box_styles_gen: Optional[int]


def _capture_view_caches() -> _ViewLaneCaches:
    return _ViewLaneCaches(
        data_gen=_DATA_GEN,
        cutout_vec=_CUTOUT_VEC,
        cutout_traces=_CUTOUT_TRACES,
        cutout_notices=list(_CUTOUT_NOTICES),
        tf_cache=dict(_TF_CACHE),
        pile=_PILE,
        pile_gen=_PILE_GEN,
        box_styles=dict(_BOX_STYLES),
        box_styles_gen=_BOX_STYLES_GEN,
    )


def _install_view_caches(caches: _ViewLaneCaches) -> None:
    global _CUTOUT_VEC, _CUTOUT_TRACES, _DATA_GEN, _PILE, _PILE_GEN, _BOX_STYLES_GEN
    _DATA_GEN = caches.data_gen
    _CUTOUT_VEC = caches.cutout_vec
    _CUTOUT_TRACES = caches.cutout_traces
    _CUTOUT_NOTICES[:] = caches.cutout_notices
    _TF_CACHE.clear()
    _TF_CACHE.update(caches.tf_cache)
    _PILE = caches.pile
    _PILE_GEN = caches.pile_gen
    _BOX_STYLES.clear()
    _BOX_STYLES.update(caches.box_styles)
    _BOX_STYLES_GEN = caches.box_styles_gen


class SessionStore:
    # Clone economy: begin_transaction() pays the transaction's single deep
    # clone, containing the only objects a handler can mutate.
    # snapshot() and capture_update() capture shallow state: fresh containers
    # over objects the respective install path never lets the other side mutate.
    # An interleaved view and the commit therefore cost container copies, not a
    # full copy of every trace's samples.
    #
    # The view projection is owned by the store (not by call()'s locals), the
    # workspace reference survives until reinstalled, and the cache stash is
    # gated on a completed projection — so a partial enter_view/exit_view
    # self-heals on the next cycle instead of orphaning the workspace or
    # letting job caches masquerade as committed-view caches. This is healing,
    # not atomicity: interrupts cannot reach the view lane today (GetView is
    # NOT_CANCELLABLE and the ownership handoff clears the byte), and that
    # gate is what a cancellable view rpc would have to replace. View-lane
    # caches swap together with the dataset state: the view keeps caches built
    # against the committed snapshot across interleaves, and the job never
    # sees them.
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
        return SessionSnapshot(self._generation, _dataset_state())

    def begin_transaction(self, snapshot: SessionSnapshot) -> None:
        self._transaction_snapshot = snapshot
        _install_dataset_state(_clone_dataset_state(snapshot.state))

    def capture_update(self) -> SessionUpdate:
        return SessionUpdate(_dataset_state())

    def restore_snapshot(self, snapshot: SessionSnapshot) -> None:
        _install_dataset_state(snapshot.state)

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
            self._suspended_job_update = SessionUpdate(_dataset_state())
            self._job_caches = _capture_view_caches()
        self._view_projected = False
        _install_dataset_state(self._transaction_snapshot.state)
        if self._view_caches is not None:
            _install_view_caches(self._view_caches)
        self._view_projected = True

    def exit_view(self) -> None:
        # The workspace reference is dropped only after the reinstall: an
        # interrupt landing mid-way then leaves it captured, and the next
        # enter_view/exit_view cycle re-projects and re-installs the same
        # workspace instead of orphaning the job's state. The caches are
        # stashed only when the projection completed — an interrupt inside
        # enter_view would otherwise let job-era caches masquerade as
        # committed-view caches on the next interleave.
        job_update = self._suspended_job_update
        if job_update is None:
            return
        if self._view_projected:
            self._view_caches = _capture_view_caches()
        self._view_projected = False
        _install_dataset_state(job_update.state)
        if self._job_caches is not None:
            _install_view_caches(self._job_caches)
        self._job_caches = None
        self._suspended_job_update = None

    def commit(self, expected: int, update: SessionUpdate) -> Optional[StaleSessionCommit]:
        actual = self._generation
        if expected != actual:
            # Defensive: the job lane is serialized, so a mismatch means a
            # protocol fault upstream; kept as a typed guard because a wrong
            # commit is a one-way door.
            self._transaction_snapshot = None
            self._drop_view_projection()
            return StaleSessionCommit(expected, actual)
        _install_dataset_state(update.state)
        self._generation += 1
        self._transaction_snapshot = None
        self._drop_view_projection()
        return None

    def _drop_view_projection(self) -> None:
        self._suspended_job_update = None
        self._view_caches = None
        self._job_caches = None
        self._view_projected = False


def _dataset_state() -> _DatasetState:
    return _DatasetState(
        traces=list(_TRACES),
        source_traces=OrderedDict(
            (source_id, list(traces)) for source_id, traces in _SOURCE_TRACES.items()
        ),
        source_digests=dict(_SOURCE_DIGESTS),
        suppressed_source_traces={
            source_id: list(traces) for source_id, traces in _SUPPRESSED_SOURCE_TRACES.items()
        },
        digest_owners={digest: set(owners) for digest, owners in _DIGEST_OWNERS.items()},
        derived_traces={origin: list(traces) for origin, traces in _DERIVED_TRACES.items()},
        derived_events={origin: dict(events) for origin, events in _DERIVED_EVENTS.items()},
        pile_is_example=_PILE_IS_EXAMPLE,
        stations=dict(_STATIONS),
        active_event=_ACTIVE_EVENT,
        active_hash=_ACTIVE_HASH,
        active_mt=_ACTIVE_MT,
        event_registry=OrderedDict(_EVENT_REGISTRY),
        stationxmls=list(_STATIONXMLS),
        warned_no_response=set(_WARNED_NO_RESPONSE),
        hidden=list(_HIDDEN),
        reduce=None if _REDUCE is None else list(_REDUCE),
        scaling_hooks=OrderedDict(_SCALING_HOOKS),
        response_gen=_RESPONSE_GEN,
        gf_stores=dict(_GF_STORES),
    )


def _install_dataset_state(state: _DatasetState) -> None:
    global _ACTIVE_EVENT, _ACTIVE_HASH, _ACTIVE_MT, _PILE_IS_EXAMPLE, _TRACES
    global _DERIVED_EVENTS, _DERIVED_TRACES, _DIGEST_OWNERS
    global _SOURCE_DIGESTS, _SOURCE_TRACES
    global _SUPPRESSED_SOURCE_TRACES
    global _EVENT_REGISTRY, _STATIONS, _STATIONXMLS, _WARNED_NO_RESPONSE
    global _GF_STORES, _HIDDEN, _REDUCE, _RESPONSE_GEN, _SCALING_HOOKS
    _TRACES = list(state.traces)
    _SOURCE_TRACES = OrderedDict(
        (source_id, list(traces)) for source_id, traces in state.source_traces.items()
    )
    _SOURCE_DIGESTS = dict(state.source_digests)
    _SUPPRESSED_SOURCE_TRACES = {
        source_id: list(traces) for source_id, traces in state.suppressed_source_traces.items()
    }
    _DIGEST_OWNERS = {digest: set(owners) for digest, owners in state.digest_owners.items()}
    _DERIVED_TRACES = {origin: list(traces) for origin, traces in state.derived_traces.items()}
    _DERIVED_EVENTS = {origin: dict(events) for origin, events in state.derived_events.items()}
    _PILE_IS_EXAMPLE = state.pile_is_example
    _STATIONS = dict(state.stations)
    _ACTIVE_EVENT = state.active_event
    _ACTIVE_HASH = state.active_hash
    _ACTIVE_MT = state.active_mt
    _EVENT_REGISTRY = OrderedDict(state.event_registry)
    _STATIONXMLS = list(state.stationxmls)
    _WARNED_NO_RESPONSE = set(state.warned_no_response)
    _HIDDEN = list(state.hidden)
    _REDUCE = None if state.reduce is None else list(state.reduce)
    _SCALING_HOOKS = OrderedDict(state.scaling_hooks)
    _RESPONSE_GEN = state.response_gen
    _GF_STORES = dict(state.gf_stores)
    _mark_traces_changed()


_SESSION_STORE = SessionStore()


def _empty_dataset_state(traces: List[ptrace.Trace]) -> _DatasetState:
    return _DatasetState(
        traces=list(traces),
        source_traces=OrderedDict(),
        source_digests={},
        suppressed_source_traces={},
        digest_owners={},
        derived_traces={},
        derived_events={},
        pile_is_example=False,
        stations={},
        active_event=None,
        active_hash="",
        active_mt=None,
        event_registry=OrderedDict(),
        stationxmls=[],
        warned_no_response=set(),
        hidden=[],
        reduce=None,
        scaling_hooks=OrderedDict(),
        response_gen=0,
        gf_stores={},
    )


def _bundle_role(name: str, payload: bytes) -> str:
    """Which loader a bundle member belongs to, decided by its name and — where the
    name is ambiguous — by its first bytes.

    Waveforms are the default rather than a listed set of extensions, because
    ``io.load(format='detect')`` reads a dozen formats and a member this function
    has never heard of is far more likely to be one of them than to be prose.
    """
    lowered = name.lower()
    if lowered.endswith(".xml"):
        return "stationxml"
    if lowered.endswith(".stations"):
        return "stations"
    if lowered.endswith((".yaml", ".yml")):
        # pyrocko writes stations and events in the same guts yaml; only the tag
        # says which, and guessing wrong loads a catalogue as coordinates.
        head = payload[:4096].decode("utf-8", "replace")
        if "!pf.Station" in head:
            return "stations"
        if "!pf.Event" in head:
            return "events"
        return "skip"
    if lowered.endswith(".events"):
        return "events"
    if lowered.endswith(_BUNDLE_SKIP_SUFFIXES):
        return "skip"
    return "waveforms"


def _bundle_about(manifest: Dict[str, Any]) -> Optional[data_pb2.BundleAbout]:
    """What the dataset is, for the bundles that describe themselves.

    This is the whole reason the examples are worth shipping — someone who opens one
    should learn what they are looking at, not just watch traces appear. It is not a
    load report: it says nothing about this particular load, so it stays true while
    the dataset is open and the frontend can show it again on demand.

    ``None`` for an archive carrying no prose, which is most of them: any zip of data
    files loads here, and one a user assembled has nothing to say about itself.
    """
    blurb = str(manifest.get("blurb", "")).strip()
    characteristics = [
        str(item) for item in manifest.get("characteristics", []) if str(item).strip()
    ]
    citation = str(manifest.get("citation", "")).strip()
    if not (blurb or characteristics or citation):
        return None
    parts = []
    if blurb:
        parts.append(blurb)
    if characteristics:
        # Not "what to try": these are the characteristics of the recording, and
        # the reason this dataset is in the menu rather than another one.
        parts.append(
            "What is unique about this dataset:\n"
            + "\n".join("  • %s" % item for item in characteristics)
        )
    if citation:
        parts.append("Data: %s" % citation)
    return data_pb2.BundleAbout(
        title=str(manifest.get("title", "")).strip(),
        body="\n\n".join(parts),
    )


def _load_bundle_result(
    raw: bytes, label: str, budget: _MarkerBudget = None
) -> data_pb2.BundleResult:
    """Load every data file in one archive, in the order the loaders depend on.

    Waveforms first, since they define the scene and replace whatever was loaded
    before; then station metadata; then events, because choosing the active event
    enables a distance sort that reads the coordinates the StationXML just brought.
    """
    import json
    import zipfile

    if raw[:4] != _ZIP_MAGIC:
        raise ValueError("%s is not a zip archive" % (label or "bundle"))

    archive = zipfile.ZipFile(io.BytesIO(raw))
    manifest: Dict[str, Any] = {}
    groups: Dict[str, List[Tuple[str, bytes]]] = {}
    for entry in sorted(archive.infolist(), key=lambda info: info.filename):
        if entry.is_dir():
            continue
        if entry.filename.rsplit("/", 1)[-1] == _BUNDLE_MANIFEST:
            try:
                manifest = dict(json.loads(archive.read(entry.filename)))
            except (ValueError, TypeError):
                manifest = {}
            continue
        payload = archive.read(entry.filename)
        groups.setdefault(_bundle_role(entry.filename, payload), []).append(
            (entry.filename, payload)
        )

    waveforms = groups.get("waveforms", [])
    if not waveforms:
        raise ValueError("%s contains no waveform files" % (label or "bundle"))

    traces: List[ptrace.Trace] = []
    for name, payload in waveforms:
        traces.extend(_load_path_traces(_stage_bytes(name, payload), "detect"))
    about = _bundle_about(manifest)
    title = str(manifest.get("title", "")).strip() or label
    manifest_state = manifest.get("state")
    state = manifest_state if isinstance(manifest_state, dict) else {}
    view = data_pb2.BundleView(
        sort=str(state.get("sort", "")),
        subsort=str(state.get("subsort", "")),
    )

    previous = _dataset_state()
    _install_dataset_state(_empty_dataset_state(traces))
    try:
        scene = _scene_meta()
        loaded = ["%d trace(s) from %d file(s)" % (scene.track_count, len(waveforms))]
        info = snuffling_pb2.SnufflingResult(show_window=True, origin="bundle")
        bundle_markers = _MarkerAccumulator(budget)
        responses = [
            payload.decode("utf-8", "replace") for _name, payload in groups.get("stationxml", [])
        ]
        if responses:
            loaded.append(_load_responses_result(responses).text)
        for name, payload in groups.get("stations", []):
            stations = _load_stations_result(payload.decode("utf-8", "replace"))
            if stations.text.startswith(("could not parse", "empty")):
                raise ValueError("%s: %s" % (name, stations.text))
            loaded.append(stations.text)
        for name, payload in groups.get("events", []):
            events = _load_events_result(payload.decode("utf-8", "replace"), budget)
            if events.text.startswith(("could not parse", "empty")):
                raise ValueError("%s: %s" % (name, events.text))
            for marker in events.markers:
                nslcs = [
                    (
                        events.nslc_table[index].net,
                        events.nslc_table[index].sta,
                        events.nslc_table[index].loc,
                        events.nslc_table[index].cha,
                    )
                    for index in marker.nslc_indexes
                    if index < len(events.nslc_table)
                ]
                copy_marker = snuffling_pb2.SnufMarker()
                copy_marker.CopyFrom(marker)
                bundle_markers.add(copy_marker, nslcs)
            bundle_markers.total += int(events.markers_dropped)
            loaded.append(events.text)
        info.text = ("%s\n\n%s" % (title, "\n".join(loaded))).strip()
        bundle_markers.finish(info)
        info.event_state.CopyFrom(_event_state(bundle_markers.limit))
        # A described dataset opens one window, and it is the description: the counts
        # are already in the status line, and burying the prose under them was how the
        # same three lines about instrument responses ended up in front of every reader
        # of every example.
        # An undescribed archive still reports, since otherwise
        # nothing at all would say what it did.
        info.show_window = about is None
        staged = _dataset_state()
        result = data_pb2.BundleResult(scene=scene, info=info, view=view, about=about)
    finally:
        _install_dataset_state(previous)
    _install_dataset_state(staged)
    return result


async def _example_list() -> data_pb2.ExampleList:
    """The example index, fetched rather than compiled in, so a deployment can gain
    a dataset without a new wasm build."""
    import json

    raw = await _fetch_bytes(_asset_url("assets/examples/examples.json"))
    doc = json.loads(raw)
    result = data_pb2.ExampleList()
    for entry in doc.get("examples", []):
        result.examples.add(
            slug=str(entry.get("slug", "")),
            title=str(entry.get("title", "")),
            blurb=str(entry.get("blurb", "")),
            characteristics=[str(item) for item in entry.get("characteristics", [])],
            citation=str(entry.get("citation", "")),
            url=str(entry.get("url", "")),
            bytes=int(entry.get("bytes", 0)),
            sha256=str(entry.get("sha256", "")),
        )
    return result


_PLATES: Optional[List[data_pb2.PlateBoundary]] = None


async def _plate_boundaries() -> List[data_pb2.PlateBoundary]:
    """Parse pyrocko's shipped Bird (2003) ``plates.kml`` into lon/lat polylines
    (memoized). Pure geometry passthrough of pyrocko's own data via stdlib XML —
    the map overlays exactly what the Qt map snuffling draws."""
    global _PLATES
    if _PLATES is not None:
        return _PLATES
    import xml.etree.ElementTree as ET

    plates: List[data_pb2.PlateBoundary] = []
    text = await _fetch_text_or_empty(_asset_url("plates.kml"))
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
    _PLATES = plates
    return _PLATES


def _map_data_result(
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
            _EVENT_REGISTRY.get(request.active_event_hash)
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
                (tr.nslc_id[0], tr.nslc_id[1])
                for tr in _TRACES
                if tr.tmax >= active_time - 3600.0 and tr.tmin <= active_time + 3600.0
            }

    for (net, sta), (lat, lon, _elev, _depth) in _stations_by_name().items():
        if active_stations is not None and (net, sta) not in active_stations:
            continue
        # Qt's map shows the viewer's stations *minus the blacklist*
        # (map/__init__.py:242): a station the user hid is hidden everywhere.
        if _HIDDEN and match_nslc(_HIDDEN, (net, sta, "", "")):
            continue
        station = msg.stations.add()
        station.label = "%s.%s" % (net, sta)
        station.lat = lat
        station.lon = lon
    from pyrocko import util

    for event_hash in request.event_hashes:
        ev = _EVENT_REGISTRY.get(event_hash)
        if ev is None:
            continue
        event = msg.events.add()
        event.lat = float(ev.lat)
        event.lon = float(ev.lon)
        event.mag = float(ev.magnitude) if ev.magnitude is not None else 0.0
        event.depth = float(ev.depth) if ev.depth is not None else 0.0
        event.name = (
            str(getattr(ev, "region", "") or "")
            or _short_event_name(getattr(ev, "name", None))
            or "(Event)"
        )
        event.time = util.time_to_str(float(ev.time))[:19]
        event.event_hash = event_hash
    if plates:
        msg.plate_boundaries.extend(plates)
    return msg


# --- Marker file I/O (pyrocko marker format) -------------------------------


_marker_mod: Any = None
_gf_store_mod: Any = None
_gf_meta_mod: Any = None
# Mounted Green's-function stores: dropdown id -> filesystem dir (Pyodide FS).
_GF_STORES: Dict[str, str] = {}
# Dropdown label prefix for GF-store earth models (mirrors Qt's "GF Store: <id>").
GF_STORE_PREFIX = "GF Store: "


def _marker_module() -> Any:
    """Load pyrocko's ``marker`` module without running the snuffler package init.

    ``import pyrocko.gui.snuffler.marker`` would first execute
    ``pyrocko/gui/snuffler/__init__.py``, which eagerly imports the full Qt
    snuffler application, and PyQt is not in our Pyodide wheel — nor could it be.
    marker.py itself only needs util/plot/model/trace, so
    install a lightweight stub for the ``snuffler`` package (carrying the real
    directory on ``__path__``) and import the submodule through it.
    """
    global _marker_mod
    if _marker_mod is None:
        import importlib
        import sys
        import types

        import pyrocko.gui

        pkg_name = "pyrocko.gui.snuffler"
        existing = sys.modules.get(pkg_name)
        if existing is None or not getattr(existing, "_snuffler_stub", False):
            stub = types.ModuleType(pkg_name)
            stub.__path__ = [os.path.join(os.path.dirname(pyrocko.gui.__file__), "snuffler")]
            stub.__package__ = pkg_name
            setattr(stub, "_snuffler_stub", True)
            sys.modules[pkg_name] = stub
        _marker_mod = importlib.import_module(pkg_name + ".marker")
    return _marker_mod


def _gf_modules() -> Tuple[Any, Any]:
    """Load ``pyrocko.gf.store`` + ``pyrocko.gf.meta`` WITHOUT running
    ``pyrocko/gf/__init__.py``.

    ``import pyrocko.gf`` pulls seismosizer -> ws -> ``import requests`` (only
    needed by RemoteEngine). Reading a local store's config + 1D earth model needs
    neither requests nor ``store_ext``, so install a stub ``pyrocko.gf`` package
    (real dir on ``__path__``) and import just the two submodules — the same trick
    as :py:func:`_marker_module`.
    """
    global _gf_store_mod, _gf_meta_mod
    if _gf_store_mod is None or _gf_meta_mod is None:
        import importlib
        import sys
        import types

        import pyrocko

        pkg = "pyrocko.gf"
        existing = sys.modules.get(pkg)
        if existing is None or not getattr(existing, "_gf_stub", False):
            stub = types.ModuleType(pkg)
            stub.__path__ = [os.path.join(os.path.dirname(pyrocko.__file__), "gf")]
            stub.__package__ = pkg
            setattr(stub, "_gf_stub", True)
            sys.modules[pkg] = stub
        _gf_meta_mod = importlib.import_module(pkg + ".meta")
        _gf_store_mod = importlib.import_module(pkg + ".store")
    return _gf_store_mod, _gf_meta_mod


def gf_store_label(store_dir: str) -> str:
    """Read a mounted GF store's config (no ``store_ext`` / network needed),
    register it so its embedded 1D earth model is selectable in Cake Phase, and
    return its dropdown label.

    Raises on failure; the ``call()`` failsafe turns that into a BridgeError.
    """
    gfstore, _gfmeta = _gf_modules()
    cfg = gfstore.Store(store_dir).config
    sid = str(cfg.id)
    _GF_STORES[sid] = store_dir
    return GF_STORE_PREFIX + sid


def _time_fractional_digits() -> int:
    """Sub-second digits for saved marker times, derived from the data's sample
    interval exactly as Qt derives them (``time_fractional_digits``,
    `pile_viewer.py:1855`): 50 Hz data gets four decimals, not a fixed three."""
    deltatmin = min((float(tr.deltat) for tr in _TRACES if tr.deltat > 0.0), default=0.0)
    if deltatmin <= 0.0:
        deltatmin = 0.001
    return min(9, max(1, int(-math.floor(math.log10(deltatmin))) + 2))


def _markers_to_text(req: data_pb2.MarkerFile) -> str:
    """Serialize markers to pyrocko's snuffler marker-file format."""
    pm = _marker_module()
    from pyrocko import model
    from pyrocko import moment_tensor as mtm

    # Build the event objects first, keyed by the web-side identity hash: a
    # phase pick associated with an event *in the same file* is linked through
    # the shared ``model.Event`` object, so both rows write the same
    # ``Event.get_hash()`` and ``associate_phases_to_events`` re-links them on
    # any pyrocko side.
    events: Dict[str, Any] = {}
    for m in req.markers:
        if not m.is_event:
            continue
        event_hash = m.event_hash if m.HasField("event_hash") and m.event_hash else ""
        canonical = _EVENT_REGISTRY.get(event_hash) if event_hash else None
        if canonical is not None:
            events[event_hash] = canonical
            continue
        mt = None
        if m.HasField("moment_tensor"):
            t = m.moment_tensor
            mt = mtm.MomentTensor(mnn=t.mnn, mee=t.mee, mdd=t.mdd, mne=t.mne, mnd=t.mnd, med=t.med)
        ev: Any = model.Event(
            time=float(m.tmin),
            lat=float(m.lat) if m.HasField("lat") else 0.0,
            lon=float(m.lon) if m.HasField("lon") else 0.0,
            depth=float(m.depth) if m.HasField("depth") else 0.0,
            magnitude=float(m.magnitude) if m.HasField("magnitude") else None,
            # The event's own name — never the display label; a hand-made event
            # is Qt's ``name='Event'`` (`marker.py:503`).
            name=(m.event_name if m.HasField("event_name") and m.event_name else "Event"),
            region=(m.region if m.HasField("region") and m.region else None),
            catalog=(m.catalog if m.HasField("catalog") and m.catalog else None),
            moment_tensor=mt,
        )
        if event_hash:
            events[event_hash] = ev

    ms = []
    for m in req.markers:
        tmin = float(m.tmin)
        tmax = float(m.tmax)
        kind = int(m.kind)
        ids = [(n.net, n.sta, n.loc, n.cha) for n in m.nslc_ids]
        phase = m.phase if m.HasField("phase") else None
        polarity = int(m.polarity) if m.HasField("polarity") else None
        unc = float(m.uncertainty) if m.HasField("uncertainty") else None
        # Event markers save as real EventMarkers (origin + focal mechanism), not
        # downgraded to plain markers.
        if m.is_event:
            ev = events.get(m.event_hash) if m.HasField("event_hash") else None
            if ev is None:
                # An event without a web-side hash still saves; rebuild inline.
                mt = None
                if m.HasField("moment_tensor"):
                    t = m.moment_tensor
                    mt = mtm.MomentTensor(
                        mnn=t.mnn, mee=t.mee, mdd=t.mdd, mne=t.mne, mnd=t.mnd, med=t.med
                    )
                ev = model.Event(
                    time=tmin,
                    lat=float(m.lat) if m.HasField("lat") else 0.0,
                    lon=float(m.lon) if m.HasField("lon") else 0.0,
                    depth=float(m.depth) if m.HasField("depth") else 0.0,
                    magnitude=float(m.magnitude) if m.HasField("magnitude") else None,
                    name=(m.event_name if m.HasField("event_name") and m.event_name else "Event"),
                    moment_tensor=mt,
                )
            ms.append(pm.EventMarker(ev, kind))
        # A polarity, uncertainty, angle or association is only meaningful on a
        # PhaseMarker, so promote any marker that carries one even if it has no
        # phase name yet.
        elif (
            phase
            or polarity is not None
            or unc is not None
            or m.HasField("event_hash")
            or m.HasField("automatic")
            or m.HasField("incidence_angle")
            or m.HasField("takeoff_angle")
        ):
            ev = events.get(m.event_hash) if m.HasField("event_hash") else None
            ms.append(
                pm.PhaseMarker(
                    ids,
                    tmin,
                    tmax,
                    kind,
                    phasename=phase,
                    polarity=polarity,
                    uncertainty=unc,
                    automatic=bool(m.automatic) if m.HasField("automatic") else None,
                    incidence_angle=(
                        float(m.incidence_angle) if m.HasField("incidence_angle") else None
                    ),
                    takeoff_angle=(float(m.takeoff_angle) if m.HasField("takeoff_angle") else None),
                    # Association: through the event object when its row is in
                    # this file (both rows then share one hash); otherwise the
                    # stored hash and event time ride along raw, pyrocko's own
                    # fallback keys.
                    event=ev,
                    event_hash=(None if ev is not None else (m.event_hash or None)),
                    event_time=(
                        float(m.event_time) if ev is None and m.HasField("event_time") else None
                    ),
                )
            )
        else:
            ms.append(pm.Marker(ids, tmin, tmax, kind))
    path = "/tmp/markers_out.txt"
    pm.save_markers(ms, path, fdigits=_time_fractional_digits())
    with open(path) as f:
        return f.read()


def _markers_from_text(text: str, budget: _MarkerBudget = None) -> data_pb2.SnufMarkerList:
    """Parse a pyrocko marker file into the SnufMarker messages the egui side adds.

    Re-links phase picks to their events (``associate_phases_to_events``, which
    Qt's own loader calls) and registers the loaded events so ``e`` activates
    the real origin rather than rebuilding a zero-depth stand-in."""
    pm = _marker_module()
    from pyrocko import orthodrome as od

    result = data_pb2.SnufMarkerList()
    markers = _MarkerAccumulator(budget)
    path = "/tmp/markers_in.txt"
    with open(path, "w") as f:
        f.write(text)
    # A malformed file raises out of here — the dispatch turns it into the
    # error dialog Qt shows — rather than reporting success with zero markers,
    # matching the station and event loaders beside it.
    ms = pm.load_markers(path)
    pm.associate_phases_to_events(ms)
    for mk in ms:
        # One bad row must not lose the whole file: parse each marker
        # defensively and skip only the one that fails.
        try:
            ids = mk.get_nslc_ids()
            nslcs = [
                (
                    str(nslc_id[0]) if len(nslc_id) > 0 else "",
                    str(nslc_id[1]) if len(nslc_id) > 1 else "",
                    str(nslc_id[2]) if len(nslc_id) > 2 else "",
                    str(nslc_id[3]) if len(nslc_id) > 3 else "",
                )
                for nslc_id in ids
            ]
            # Built off-list and appended at the end so a half-parsed marker is
            # dropped whole rather than kept truncated.
            marker = snuffling_pb2.SnufMarker()
            marker.t = float(mk.tmin)
            marker.tmax = float(mk.tmax)
            # Qt accepts any integer kind; the transport is unsigned 32-bit and
            # the frontend colours 0..7, so fold negatives the way Python's
            # modulo already does.
            marker.kind = int(mk.kind or 0) % 256
            marker.is_event = isinstance(mk, pm.EventMarker)
            event = None
            if isinstance(mk, pm.EventMarker):
                ev = mk.get_event()
                event = ev
                if ev is not None:
                    if ev.name:
                        marker.event_name = str(ev.name)[:96]
                    if getattr(ev, "region", None):
                        marker.region = str(ev.region)
                    if getattr(ev, "catalog", None):
                        marker.catalog = str(ev.catalog)
                    if ev.magnitude is not None:
                        marker.magnitude = float(ev.magnitude)
                    if ev.depth is not None:
                        marker.depth = float(ev.depth)
                    marker.lat = float(ev.lat)
                    marker.lon = float(ev.lon)
                    if ev.moment_tensor is not None:
                        mnn, mee, mdd, mne, mnd, med = (float(x) for x in ev.moment_tensor.m6())
                        marker.moment_tensor.mnn = mnn
                        marker.moment_tensor.mee = mee
                        marker.moment_tensor.mdd = mdd
                        marker.moment_tensor.mne = mne
                        marker.moment_tensor.mnd = mnd
                        marker.moment_tensor.med = med
                try:
                    ehash = mk.get_event_hash()
                except Exception:
                    ehash = None
                known_hash = _known_event_hash(ev) if ev is not None else None
                if known_hash or ehash:
                    marker.event_hash = known_hash or str(ehash)
                if ev is not None and _ACTIVE_EVENT is not None:
                    marker.dist_active_km = (
                        float(
                            od.distance_accurate50m(
                                float(ev.lat),
                                float(ev.lon),
                                _ACTIVE_EVENT[0],
                                _ACTIVE_EVENT[1],
                            )
                        )
                        / 1000.0
                    )
            if isinstance(mk, pm.PhaseMarker):
                try:
                    phase = mk.get_phasename()
                except Exception:
                    phase = None
                if phase is not None:
                    marker.phase = phase
                try:
                    polarity = mk.get_polarity()
                except Exception:
                    polarity = None
                if polarity is not None:
                    marker.polarity = int(polarity)
                # PhaseMarker stores these privately (no public getters).
                unc = getattr(mk, "_uncertainty", None)
                if unc is not None:
                    marker.uncertainty = float(unc)
                automatic = getattr(mk, "_automatic", None)
                if automatic is not None:
                    marker.automatic = bool(automatic)
                inc = getattr(mk, "_incidence_angle", None)
                if inc is not None:
                    marker.incidence_angle = float(inc)
                tko = getattr(mk, "_takeoff_angle", None)
                if tko is not None:
                    marker.takeoff_angle = float(tko)
                # The associated event's hash and time (phase-to-event link) —
                # after association the hash comes off the shared event object,
                # so it matches the event row's own.
                try:
                    ehash = mk.get_event_hash()
                except Exception:
                    ehash = None
                if ehash:
                    marker.event_hash = str(ehash)
                etime = None
                try:
                    etime = mk.get_event_time()
                except Exception:
                    etime = None
                if etime is not None:
                    marker.event_time = float(etime)
            markers.add(marker, nslcs, payload=event)
        except Exception:
            continue
    for marker, event in markers.retained_candidates():
        if event is not None and marker.event_hash:
            _register_event(marker.event_hash, event)
    return _finalize_event_markers(result, markers)


# Directory holding the cake earth models (tiny .nd text files shipped as build
# assets and fetched into the Pyodide FS on first cake use; the minimal wheel has
# no data dir, so cake.builtin_models() is empty and models must be loaded by
# path). They are fetched here in Python (pyfetch) — see _ensure_earth_models.
MODEL_DIR = "/earthmodels"

# Builtin cake models served under earthmodels/<stem>.nd (matches the Cake Phase
# dropdown). Fetched lazily on first cake run so the boot path stays light.
_CAKE_MODELS = [
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
_EARTH_MODELS_STAGED = False


async def _ensure_earth_models() -> None:
    """Fetch the builtin cake models into the Pyodide FS once, before a cake run
    that uses one. A model that fails to fetch is skipped (just not selectable)."""
    global _EARTH_MODELS_STAGED
    if _EARTH_MODELS_STAGED:
        return
    os.makedirs(MODEL_DIR, exist_ok=True)
    for stem in _CAKE_MODELS:
        path = "%s/%s.nd" % (MODEL_DIR, stem)
        if os.path.exists(path):
            continue
        text = await _fetch_text_or_empty(_asset_url("earthmodels/%s.nd" % stem))
        if text:
            with open(path, "w") as f:
                f.write(text)
    _EARTH_MODELS_STAGED = True


def _cake_model_path(name: str) -> str:
    """Filesystem path of a staged cake model from its dropdown name."""
    return "%s/%s.nd" % (MODEL_DIR, name)


def _cake_load_model(cake: Any, name: str) -> Any:
    """Load the cake earth model for a Cake Phase model selection.

    A ``GF Store: <id>`` selection uses the mounted store's embedded 1D model
    (``config.earthmodel_1d``, read without ``store_ext``); any other name is a
    staged builtin ``.nd`` model loaded by path.
    """
    if name.startswith(GF_STORE_PREFIX):
        sid = name[len(GF_STORE_PREFIX) :]
        store_dir = _GF_STORES.get(sid)
        if store_dir is None:
            raise ValueError("GF store %r is not mounted" % sid)
        gfstore, _gfmeta = _gf_modules()
        mod = gfstore.Store(store_dir).config.earthmodel_1d
        if mod is None:
            raise ValueError("GF store %r has no 1D earth model" % sid)
        return mod
    return cake.load_model(_cake_model_path(name))


def _cake_section(
    req: snuffling_pb2.SnufflingRequestParams,
) -> Tuple[str, List[str], float, bool, str]:
    """Extract the Cake Phase knobs from the snuffling params."""
    c = req.snufflings.cake
    model = c.cake_model or "ak135-f-continental.m"
    phases = [str(x) for x in (list(c.cake_phases) or ["~P"])]
    tshift = float(c.cake_tshift)
    use_station_depth = bool(c.cake_use_station_depth)
    mode = c.cake_mode or "run"
    return model, phases, tshift, use_station_depth, mode


def _cake_wanted_phases(cake: Any, names: List[str]) -> Tuple[List[Any], List[str]]:
    """Turn the enabled phase names into cake ``PhaseDef`` objects.

    A leading ``~`` selects a raw definition (``cake.PhaseDef``); any other name
    is expanded via the classic-phase table (``cake.PhaseDef.classic``), exactly
    as the Qt Cake Phase snuffling does. The friendly name is stashed on each
    PhaseDef so ``ray.given_phase().name`` reports it back. Parse failures are
    collected (not raised) so one bad custom phase does not sink the whole run.
    """
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


def _cake_stations() -> List[Tuple[str, str, float, float]]:
    """``(net, sta, dist_deg, depth_m)`` for the stations cake gets rays at.

    Qt's scope (`get_active_event_and_stations`, snuffling.py:806): stations
    whose traces hold data within an hour of the event, one per distinct
    station *entry* — a borehole and a surface sensor at one site are two,
    resolved per trace through the location-specific key with the station-wide
    fallback. Distances are great-circle degrees from the active event (which
    is what ``cake.arrivals`` wants).
    """
    if _ACTIVE_EVENT is None:
        return []
    elat, elon, _edep, etime = _ACTIVE_EVENT
    seen = set()
    out: List[Tuple[str, str, float, float]] = []
    for tr in _TRACES:
        if tr.tmax < etime - 3600.0 or tr.tmin > etime + 3600.0:
            continue
        net, sta, loc, _cha = tr.nslc_id
        for key in ((net, sta, loc), (net, sta)):
            entry = _STATIONS.get(key)
            if entry is None:
                continue
            ident = (net, sta, entry)
            if ident in seen:
                continue
            seen.add(ident)
            slat, slon, _elev, depth = entry
            out.append((net, sta, _dist_deg(elat, elon, slat, slon), depth))
    return out


def _cake_run(req: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Theoretical arrivals → phase markers (the Cake Phase 'Run' action).

    All travel-time computation is pyrocko's :py:mod:`pyrocko.cake` (pure
    numpy/scipy ray tracing) and the geometry is :py:func:`_dist_deg`; this only
    marshals the rays into markers and a per-pick angle table.
    """
    model, phase_names, tshift, use_station_depth, _mode = _cake_section(req)
    if _ACTIVE_EVENT is None:
        return snuffling_pb2.SnufflingResult(
            show_window=True,
            text="No active event. Run a catalog search first (Data ▸ Catalog search).",
        )
    if not _STATIONS:
        return snuffling_pb2.SnufflingResult(
            show_window=True,
            text="No station coordinates. Fetch stations first (Data ▸ Download ▸ Fetch stations).",
        )

    from pyrocko import cake

    _elat, _elon, edepth, etime = _ACTIVE_EVENT
    mod = _cake_load_model(cake, model)
    wanted, errors = _cake_wanted_phases(cake, phase_names)

    result = snuffling_pb2.SnufflingResult(show_window=True, origin="cake")
    markers = _MarkerAccumulator(req.marker_budget)
    lines = ["%-14s %-8s %12s %8s %8s" % ("station", "phase", "t-rel[s]", "inc[deg]", "tko[deg]")]
    nstations = 0
    for net, sta, dist_deg, depth in _cake_stations():
        nstations += 1
        # `station.depth`, which is what Qt's "Use station depth" means
        # (cake_phase.py). This used to approximate a borehole from a negative
        # elevation because the station table carried no depth; it does now.
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
            marker.t = etime + tt
            marker.tmax = etime + tt
            # Qt's Cake Phase conventions (cake_phase.py:163): a fixed kind 2,
            # `('*', '*')` for "any location, any channel" — an empty location
            # means "the blank location" to pyrocko and matches nothing else —
            # the angles on the marker itself, and the event association.
            marker.kind = 2
            marker.phase = name
            marker.incidence_angle = inc
            marker.takeoff_angle = tko
            if _ACTIVE_HASH:
                marker.event_hash = _ACTIVE_HASH
            if _ACTIVE_EVENT is not None:
                marker.event_time = _ACTIVE_EVENT[3]
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


def _command_glob(scope: Optional[str], pattern: str) -> str:
    """Build an "n.s.l.c" fnmatch glob from an optional scope + pattern."""
    idx = {"n": 0, "s": 1, "l": 2, "c": 3}
    if scope in idx:
        fields = ["*", "*", "*", "*"]
        fields[idx[scope]] = pattern
        return ".".join(fields)
    # No scope: the token is the glob itself, anchored over the full
    # "net.sta.loc.cha" string exactly as snuffler passes it to `match_nslc`
    # (pile_viewer.py:4326). A substring convenience here would make the
    # documented `hide n <pattern>` form's bare tokens destructive typos:
    # `hide n` would hide every channel whose id contains an n.
    return pattern


def _visibility_patterns(args: List[str]) -> List[str]:
    """The globs a ``hide``/``unhide`` line addresses, following snuffler's
    ``inputline_finished``: a lone argument is the pattern itself, while a leading
    ``n|s|l|c`` scopes every argument after it (``hide n GE GR`` hides two
    networks). Anything else addresses nothing."""
    match args:
        case [pattern]:
            return [_command_glob(None, pattern)]
        case [("n" | "s" | "l" | "c") as scope, *rest] if rest:
            return [_command_glob(scope, tok) for tok in rest]
        case _:
            return []


def _set_hidden(hide: bool, globs: List[str]) -> data_pb2.CommandResult:
    """Add the globs to the blacklist, or take them off it. Unhiding a pattern
    that is not blacklisted is an error, as it is in snuffler
    (``remove_blacklist_pattern``)."""
    global _HIDDEN
    for glob in globs:
        if hide:
            _hide_pattern(glob)
        elif glob in _HIDDEN:
            _HIDDEN = [h for h in _HIDDEN if h != glob]
        else:
            # Whatever came off the blacklist before the unknown pattern stays
            # off it, so the partially-updated scene rides along with the error.
            return _command_error("Pattern not found in blacklist.", with_scene=True)
    return _command_scene()


def _set_scaling(pattern: Optional[str], vmin_tok: str, vmax_tok: str) -> data_pb2.CommandResult:
    """Pin the amplitude range of the tracks matching ``pattern`` (every track
    when it is None), the ':scaling' command's rule. Bounds are read with the same
    ``float_or_none`` snuffler uses, so 'nan' leaves that bound automatic."""
    from pyrocko.model import float_or_none

    try:
        vmin, vmax = float_or_none(vmin_tok), float_or_none(vmax_tok)
    except ValueError:
        return _command_error("scaling: vmin/vmax must be numbers or 'nan'")
    _SCALING_HOOKS["_" if pattern is None else pattern] = (vmin, vmax)
    return _command_scene()


def _empty_channel_patterns(tmin: float, tmax: float) -> List[str]:
    """The exact "n.s.l.c" globs of channels with no data in the visible window,
    for snuffler's ``hide empty`` (``add_blacklist_pattern('empty')``)."""
    have_data = set()
    for tr in _TRACES:
        if tr.tmax >= tmin and tr.tmin <= tmax:
            have_data.add(tr.nslc_id)
    empty = {tr.nslc_id for tr in _TRACES} - have_data
    return sorted(".".join(nslc) for nslc in empty)


def _hide_pattern(glob: str) -> None:
    """Blacklist a glob, moving an already-listed one to the end (snuffler's
    ``add_blacklist_pattern``)."""
    global _HIDDEN
    _HIDDEN = [h for h in _HIDDEN if h != glob]
    _HIDDEN.append(glob)


def _command_scene() -> data_pb2.CommandResult:
    result = data_pb2.CommandResult(action="scene")
    result.scene.CopyFrom(_scene_meta())
    return result


def _command_error(message: str, with_scene: bool = False) -> data_pb2.CommandResult:
    """A rejected command. ``with_scene`` carries the scene along for a command
    that changed what is shown before it failed."""
    result = data_pb2.CommandResult(action="error", message=message)
    if with_scene:
        result.scene.CopyFrom(_scene_meta())
    return result


def _apply_scaling_hooks(data_ranges: Dict[Any, Tuple[float, float]]) -> None:
    """Apply the ':scaling' command's per-pattern amplitude overrides to the
    computed data ranges (snuffler's ``apply_scaling_hooks``): for each hook in
    sorted pattern order, replace lo/hi where the bound is not None. The '_' key is
    the global rule applied to every track; a real pattern is matched via
    ``match_nslc`` against the scaling key (an nslc tuple or a channel string).
    ``None`` keys (common-scale mode) have no channel identity and are skipped."""
    if not _SCALING_HOOKS:
        return
    from pyrocko.util import match_nslc

    for pattern in sorted(_SCALING_HOOKS):
        vmin, vmax = _SCALING_HOOKS[pattern]
        for key in list(data_ranges):
            if pattern != "_":
                if key is None or not match_nslc(pattern, key):
                    continue
            lo, hi = data_ranges[key]
            if vmin is not None:
                lo = vmin
            if vmax is not None:
                hi = vmax
            data_ranges[key] = (lo, hi)


def _goto_window_len(spec: str) -> Optional[float]:
    """The visible window (seconds) matching how specific a ``goto`` date is
    (snuffler): a bare year → ~1yr, year-month → ~1 month, a full date → 1 day,
    +hour → 1h, +minute → 1min; a fully-specified time keeps the current window."""
    s = spec.strip()
    if re.fullmatch(r"\d{4}", s):
        return 12 * 32 * 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}", s):
        return 32 * 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}", s):
        return 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}[ tT]\d{1,2}", s):
        return 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}[ tT]\d{1,2}:\d{1,2}", s):
        return 60.0
    return None


def _goto_result(args: List[str], tmin: float, tmax: float) -> data_pb2.CommandResult:
    """Snuffler's ``goto`` arm: a date, a bare time of day (taken on the date at
    the centre of the visible window), or the name of a loaded event."""
    if not args:
        return _command_error("usage: goto <time>")
    spec = " ".join(args).strip()
    from pyrocko import util

    # A bare "HH:MM[:SS[.XXX]]" means that time on the day being looked at.
    if re.fullmatch(r"\d{1,2}:\d{2}(:\d{2}(\.\d+)?)?", spec):
        day = util.time_to_str((tmin + tmax) * 0.5, format="%Y-%m-%d")
        try:
            return data_pb2.CommandResult(
                action="goto", t=float(util.str_to_time_fillup("%s %s" % (day, spec)))
            )
        except Exception:
            return _command_error("goto: cannot read time: %s" % spec)

    try:
        result = data_pb2.CommandResult(action="goto", t=float(util.str_to_time_fillup(spec)))
    except Exception:
        pass
    else:
        tlen = _goto_window_len(spec)
        if tlen is not None:
            result.tlen = tlen
        elif spec.lower() in ("today", "yesterday", "tomorrow"):
            result.tlen = 24 * 3600.0
        return result

    # Not a time — fall back to a loaded event by name (snuffler
    # go_to_event_by_name): jump the view to that event's origin time.
    target = spec.lower()
    for h, ev in _EVENT_REGISTRY.items():
        names = {
            (ev.name or "").lower(),
            _short_event_name(ev.name).lower(),
            h.lower(),
        }
        if target in names:
            return data_pb2.CommandResult(action="goto", t=float(ev.time))
    return _command_error("no event or time matching: %s" % spec)


def _command_result(request: data_pb2.CommandRequest) -> data_pb2.CommandResult:
    """Execute a snuffler ':' command line into a CommandResult whose ``action``
    is ``goto`` (with ``t``), ``scene`` (with ``scene``), ``error`` (with
    ``message``), or ``none``.

    Supported, matching snuffler's ``inputline_finished``: ``n|s|l|c [<pattern>...]``
    (quick filter); ``hide <pattern>`` / ``hide n|s|l|c <tok>...`` (also
    ``hide empty``); ``unhide [<pattern>]``; ``scaling [<pattern>] <vmin> <vmax>``;
    ``goto <time>|<eventname>``. ``markers`` never reaches Python — marker
    visibility is Rust-owned state, handled there.
    """
    global _HIDDEN, _REDUCE
    parts = request.line.split()
    if not parts:
        return data_pb2.CommandResult(action="none")
    cmd, args = parts[0].lower(), parts[1:]

    match cmd, args:
        case "goto", spec:
            return _goto_result(spec, request.visible_tmin, request.visible_tmax)

        # The quick filter: several patterns are OR'd ("c HH? BH?"), and a command
        # with no pattern at all turns the filter off again.
        case ("n" | "s" | "l" | "c") as scope, patterns:
            _REDUCE = [_command_glob(scope, pat) for pat in patterns] if patterns else None
            return _command_scene()

        case "unhide", []:
            _HIDDEN = []
            return _command_scene()

        case "hide", ["empty"]:
            for empty in _empty_channel_patterns(request.visible_tmin, request.visible_tmax):
                _hide_pattern(empty)
            return _command_scene()

        case ("hide" | "unhide") as verb, _:
            return _set_hidden(verb == "hide", _visibility_patterns(args))

        # snuffler's 'scaling' arms, by argument count: none clears every rule,
        # one is a usage error, two set the global rule, three set a per-pattern
        # rule, and more are silently ignored.
        case "scaling", []:
            _SCALING_HOOKS.clear()
            return _command_scene()
        case "scaling", [_]:
            return _command_error("usage: scaling [n.s.l.c] vmin vmax")
        case "scaling", [vmin, vmax]:
            return _set_scaling(None, vmin, vmax)
        case "scaling", [pattern, vmin, vmax]:
            return _set_scaling(pattern, vmin, vmax)
        case "scaling", _:
            return _command_scene()

        case _:
            return _command_error("No such command: %s" % cmd)


async def _rms_scene(req: snuffling_pb2.SnufflingRequestParams) -> scene_pb2.SceneMeta:
    """Blockwise root-mean-square derived traces (pyrocko's RMS snuffling).

    For each visible trace, RMS is computed over fixed-length blocks across the
    visible window and emitted as a new low-rate trace (channel suffix '-RMS').
    With grouping on, a station's components are combined into one RMS trace.
    Replaces the derived traces and returns the updated scene.
    """
    rms = req.snufflings.rms
    hp = rms.rms_highpass if rms.HasField("rms_highpass") else None
    lp = rms.rms_lowpass if rms.HasField("rms_lowpass") else None
    block = float(rms.rms_block or 100.0)
    group = bool(rms.rms_group)
    logrms = bool(rms.rms_log)

    if block <= 0.0:
        return _scene_meta()

    tpad = 0.0
    for freq in (hp, lp):
        if freq is not None and float(freq) > 0.0:
            tpad = max(tpad, 1.0 / float(freq))

    def grouping(nslc: Tuple[str, str, str, str]) -> Tuple[str, str, str, str]:
        return nslc[:3] + (nslc[3][:-1],) if group else nslc

    targets: Dict[Tuple[str, str, str, str], ptrace.Trace] = {}
    completed: List[ptrace.Trace] = []
    for batch in _chopper_selected_traces(
        req,
        mode="visible",
        tinc=block,
        tpad=tpad,
        want_incomplete=False,
    ):
        await _operation_safe_point(len(completed) + len(targets), None, "RMS windows")
        tcur = float(batch.tmin) + 0.5 * block
        rms_by_key: Dict[Tuple[str, str, str, str], List[float]] = {}
        for tr in batch.traces:
            if _is_tabu_trace(tr):
                continue
            if lp is not None:
                tr.lowpass(4, float(lp), nyquist_exception=True)
            if hp is not None:
                tr.highpass(4, float(hp), nyquist_exception=True)
            try:
                tr.chop(batch.tmin, batch.tmax)
            except ptrace.NoData:
                continue
            value = 0.0
            if tr.ydata is not None and tr.ydata.size != 0:
                y = tr.ydata.astype(float)
                value = float(np.sqrt(np.sum(y**2) / tr.ydata.size))
            rms_by_key.setdefault(grouping(tr.nslc_id), []).append(value)

        for key, values in rms_by_key.items():
            net, sta, loc, key_cha = key
            target = targets.get(key)
            if target is None or abs((target.tmax + block) - tcur) > 0.01 * block:
                if target is not None:
                    completed.append(target)
                target = ptrace.Trace(
                    network=net,
                    station=sta,
                    location=loc,
                    channel=key_cha + "-RMS",
                    tmin=tcur,
                    deltat=block,
                    ydata=np.zeros(0, dtype=float),
                    meta={"tabu": True},
                )
                targets[key] = target

            sample = np.atleast_1d(np.sqrt(np.sum(np.array(values, dtype=float) ** 2)))
            if logrms and bool(sample[0] != 0.0):
                sample = np.log(sample)
            targets[key].append(sample)

    completed.extend(targets.values())
    _replace_derived("rms", completed)
    return _scene_meta()


def _raw_bytes(value: Any) -> bytes:
    if value is None:
        return b""
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value)
    if hasattr(value, "to_py"):
        return bytes(value.to_py())
    return bytes(value)


def _nslc_pb(value: NslcId) -> common_pb2.Nslc:
    net, sta, loc, cha = value
    return common_pb2.Nslc(net=net, sta=sta, loc=loc, cha=cha)


DEFAULT_MARKER_BUDGET = 20_000
MAX_MARKER_BUDGET = 100_000


def _marker_limit(budget: _MarkerBudget) -> int:
    requested = int(budget.max_markers if budget is not None else 0) or DEFAULT_MARKER_BUDGET
    return max(1, min(requested, MAX_MARKER_BUDGET))


def _replace_marker_rows(
    result: _MarkerResultT,
    rows: Iterable[Tuple[snuffling_pb2.SnufMarker, Iterable[NslcId]]],
) -> None:
    del result.markers[:]
    del result.nslc_table[:]
    nslc_table: Dict[NslcId, int] = {}
    for marker, nslcs in rows:
        target = result.markers.add()
        target.CopyFrom(marker)
        del target.nslc_indexes[:]
        for nslc in nslcs:
            table_index = nslc_table.get(nslc)
            if table_index is None:
                table_index = len(result.nslc_table)
                result.nslc_table.add().CopyFrom(_nslc_pb(nslc))
                nslc_table[nslc] = table_index
            target.nslc_indexes.append(table_index)


def _marker_retention_rank(
    marker: snuffling_pb2.SnufMarker, index: int, strength: Optional[float] = None
) -> Tuple[int, int, float, float, int]:
    """One marker's claim to survive a budget cut; higher tuples are kept.

    Retention tiers before raw strength: event markers carry no incidence angle
    and would otherwise score 0.0, so a mixed over-budget set would keep only
    phase picks and drop every event those picks reference. The active marker
    outranks everything. Shared by ``_MarkerAccumulator`` and
    ``_truncate_marker_result`` so the two bounds cannot drift apart.
    """
    if strength is None and marker.HasField("incidence_angle"):
        strength = float(marker.incidence_angle)
    score = float(strength or 0.0)
    if not math.isfinite(score):
        score = 0.0
    return (
        int(marker.active),
        int(marker.is_event),
        score,
        -float(marker.t),
        -index,
    )


class _MarkerAccumulator:
    """Keeps only the strongest marker candidates while counting every result."""

    def __init__(self, budget: _MarkerBudget) -> None:
        self.limit = _marker_limit(budget)
        self.total = 0
        self._heap: List[
            Tuple[
                Tuple[int, int, float, float, int],
                int,
                snuffling_pb2.SnufMarker,
                Tuple[NslcId, ...],
                Any,
            ]
        ] = []

    def add(
        self,
        marker: snuffling_pb2.SnufMarker,
        nslcs: Iterable[NslcId] = (),
        strength: Optional[float] = None,
        payload: Any = None,
    ) -> None:
        index = self.total
        self.total += 1
        quality = _marker_retention_rank(marker, index, strength)
        entry = (quality, index, marker, tuple(nslcs), payload)
        if len(self._heap) < self.limit:
            heapq.heappush(self._heap, entry)
        elif quality > self._heap[0][0]:
            heapq.heapreplace(self._heap, entry)

    def finish(self, result: _MarkerResultT) -> _MarkerResultT:
        _replace_marker_rows(
            result,
            (
                (marker, nslcs)
                for _quality, _index, marker, nslcs, _payload in sorted(
                    self._heap, key=lambda entry: entry[1]
                )
            ),
        )
        result.markers_dropped += self.total - len(self._heap)
        result.marker_budget = self.limit
        return result

    def retained_candidates(self) -> List[Tuple[snuffling_pb2.SnufMarker, Any]]:
        return [
            (marker, payload)
            for _quality, _index, marker, _nslcs, payload in sorted(
                self._heap, key=lambda entry: entry[1]
            )
        ]


def _finalize_event_markers(result: _MarkerResultT, markers: _MarkerAccumulator) -> _MarkerResultT:
    result.event_state.CopyFrom(_event_state(markers.limit))
    return markers.finish(result)


def _truncate_marker_result(result: _MarkerResultT, budget: _MarkerBudget) -> _MarkerResultT:
    limit = _marker_limit(budget)
    count = len(result.markers)
    result.marker_budget = limit
    if count <= limit:
        return result
    ranked = sorted(
        enumerate(result.markers),
        key=lambda item: _marker_retention_rank(item[1], item[0]),
        reverse=True,
    )[:limit]
    ranked.sort(key=lambda item: item[0])
    kept: List[Tuple[snuffling_pb2.SnufMarker, List[NslcId]]] = []
    for _, marker in ranked:
        copy_marker = snuffling_pb2.SnufMarker()
        copy_marker.CopyFrom(marker)
        nslcs = [
            (
                result.nslc_table[index].net,
                result.nslc_table[index].sta,
                result.nslc_table[index].loc,
                result.nslc_table[index].cha,
            )
            for index in marker.nslc_indexes
            if index < len(result.nslc_table)
        ]
        kept.append((copy_marker, nslcs))
    _replace_marker_rows(result, kept)
    result.markers_dropped += count - limit
    return result


class _BridgeServicer:
    """Implements the generated `BridgeServicer`: one typed method per rpc (proto
    request in, proto response out), each delegating to the pyrocko-backed compute
    functions above. The generated `dispatch()` parses the request, packs the
    BridgeResult oneof, and converts any exception to a BridgeError — so there is
    no hand-written dispatch table or per-method serialize/parse boilerplate.
    mypy structurally checks these signatures against the generated Protocol."""

    def make_example(self, request: bridge_pb2.Empty, data: Any) -> scene_pb2.SceneMeta:
        return _make_example_scene()

    def empty_scene(self, request: bridge_pb2.Empty, data: Any) -> scene_pb2.SceneMeta:
        # The replace path inside `_set_traces` clears the scene's events,
        # stations and responses with the waveforms.
        _set_traces([])
        return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)

    def load_bytes(self, request: data_pb2.LoadBytesRequest, data: Any) -> data_pb2.LoadBytesResult:
        content = bytes(request.content)
        import hashlib

        def result(loaded: bool) -> data_pb2.LoadBytesResult:
            return data_pb2.LoadBytesResult(
                scene=_scene_meta(), loaded=loaded, event_state=_event_state()
            )

        digest = hashlib.sha256(content).hexdigest()
        if request.replace_source:
            if not request.source_id:
                raise ValueError("source replacement requires a source_id")
            previous_digest = _SOURCE_DIGESTS.get(request.source_id)
            if previous_digest is None:
                raise ValueError(
                    "waveform source %r was not registered by an initial load" % request.source_id
                )
            if digest == previous_digest:
                return result(False)
            if digest in _DIGEST_OWNERS:
                traces = _load_path_traces(_stage_bytes(request.name, content), request.fmt)
                _replace_source(request.source_id, [])
                _SOURCE_DIGESTS[request.source_id] = digest
                _SUPPRESSED_SOURCE_TRACES[request.source_id] = traces
                _remove_digest_owner(previous_digest, request.source_id)
                return result(True)
            traces = _load_path_traces(_stage_bytes(request.name, content), request.fmt)
            _replace_source(request.source_id, traces)
            _SOURCE_DIGESTS[request.source_id] = digest
            _SUPPRESSED_SOURCE_TRACES.pop(request.source_id, None)
            _remove_digest_owner(previous_digest, request.source_id)
            _add_digest_owner(digest, request.source_id)
            return result(True)

        # Refuse a file already in the pile, as Qt's pile refuses a known
        # absolute path (pile.py:1115) — by content here, since a browser file
        # has no path.
        if request.append and digest in _DIGEST_OWNERS:
            # Qt logs the refusal ("File already in pile", pile.py:1117);
            # the worker console is this bridge's log.
            print("[py.load] %s already in pile, skipped" % (request.name or "file"))
            return result(False)

        path = _stage_bytes(request.name, content)
        traces = _load_path_traces(path, request.fmt)
        _set_traces(traces, append=bool(request.append))
        if request.source_id:
            _SOURCE_TRACES[request.source_id] = traces
            _SOURCE_DIGESTS[request.source_id] = digest
        if request.append or request.source_id:
            _add_digest_owner(digest, request.source_id)
        return result(True)

    async def load_url(self, request: data_pb2.LoadUrlRequest, data: Any) -> scene_pb2.SceneMeta:
        name = request.url.rsplit("/", 1)[-1] or "data"
        raw = await _fetch_bytes(_asset_url(request.url))
        return _load_path_scene(name, _stage_bytes(name, raw), request.fmt, False)

    async def list_examples(self, request: bridge_pb2.Empty, data: Any) -> data_pb2.ExampleList:
        return await _example_list()

    async def load_bundle(
        self, request: data_pb2.LoadBundleRequest, data: Any
    ) -> data_pb2.BundleResult:
        if request.content:
            raw = bytes(request.content)
            label = request.name or "bundle"
        else:
            label = request.name or request.url.rsplit("/", 1)[-1] or "bundle"
            raw = await _fetch_bytes(_asset_url(request.url))
        if request.sha256:
            import hashlib

            digest = hashlib.sha256(raw).hexdigest()
            if digest != request.sha256:
                # Without this the failure surfaces as a parse error from deep
                # inside pyrocko, which reads as "this dataset is broken" rather
                # than "the download did not finish".
                raise ValueError(
                    "%s does not match the checksum in the index (expected %s…, got %s…): "
                    "the download was truncated, or the index is stale"
                    % (label, request.sha256[:12], digest[:12])
                )
        result = _load_bundle_result(raw, label, request.marker_budget)
        # A checksummed bundle came from the examples index (user files carry
        # no checksum): it is boot/demo data, replaced by the first real load.
        if request.sha256:
            global _PILE_IS_EXAMPLE
            _PILE_IS_EXAMPLE = True
        return result

    def get_view(
        self, request: view_pb2.ViewParams, data: Any
    ) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
        return _get_view_data(request)

    async def render_figure(
        self, request: figure_pb2.FigureRequest, data: Any
    ) -> figure_pb2.FigureResult:
        _operation_progress(0, 3, "Preparing figure")
        # Cake figures need the earth-model data files staged first (same as the
        # cake 'Run' snuffling); other figures compute from loaded traces.
        if request.kind in ("cake_model", "cake_rays"):
            model = request.params.snufflings.cake.cake_model or ""
            if not model.startswith(GF_STORE_PREFIX):
                await _ensure_earth_models()
        _operation_progress(1, 3, "Rendering figure")
        result = _render_figure(request)
        _operation_progress(3, 3, "Encoding figure")
        return result

    async def run_snuffling(
        self, request: snuffling_pb2.NamedSnufflingRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        origin = {
            "cake": "cake",
            "stalta": "stalta",
            "seisbench_picker": "seisbench",
            "seismosizer": "seismosizer",
        }.get(request.name, "")
        if request.name == "cake":
            model = request.params.snufflings.cake.cake_model or ""
            if not model.startswith(GF_STORE_PREFIX):
                await _ensure_earth_models()
        if request.name == "seisbench_picker":
            result = await _seisbench_pick_result(request.params)
        else:
            result = await _run_snuffling_result(request.name, request.params)
        if origin and not result.origin:
            result.origin = origin
        return _truncate_marker_result(result, request.params.marker_budget)

    async def rms_traces(
        self, request: snuffling_pb2.SnufflingRequestParams, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return snuffling_pb2.SnufflingResult(
            show_window=False,
            origin="rms",
            scene=await _rms_scene(request),
        )

    def run_command(self, request: data_pb2.CommandRequest, data: Any) -> data_pb2.CommandResult:
        return _command_result(request)

    async def map_data(self, request: data_pb2.MapRequest, data: Any) -> data_pb2.MapData:
        return _map_data_result(request, await _plate_boundaries())

    def station_distances(
        self, request: bridge_pb2.Empty, data: Any
    ) -> data_pb2.StationDistanceList:
        return _station_distances_result()

    def set_active_event(
        self, request: data_pb2.SetActiveEventRequest, data: Any
    ) -> common_pb2.ActiveEventResult:
        return _set_active_event(request)

    def update_event(
        self, request: common_pb2.EventSnapshot, data: Any
    ) -> common_pb2.ActiveEventResult:
        return _update_event(request)

    def remove_events(
        self, request: data_pb2.RemoveEventsRequest, data: Any
    ) -> common_pb2.ActiveEventResult:
        return _remove_events(request)

    async def load_gf_store_url(
        self, request: data_pb2.StringRequest, data: Any
    ) -> bridge_pb2.StringValue:
        return await _load_gf_store_url(request.value)

    def markers_to_text(self, request: data_pb2.MarkerFile, data: Any) -> bridge_pb2.StringValue:
        return bridge_pb2.StringValue(value=_markers_to_text(request))

    def load_markers(
        self, request: data_pb2.LoadMarkersRequest, data: Any
    ) -> data_pb2.SnufMarkerList:
        return _markers_from_text(request.text, request.marker_budget)

    def load_station_xml(
        self, request: data_pb2.LoadStationXmlRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        # Same parsing as fetched FDSN responses: appends to _STATIONXMLS
        # (deconvolution) and harvests station coordinates (distance/azimuth
        # sorting, cake).
        return _load_responses_result(list(request.texts))

    def load_stations(
        self, request: data_pb2.LoadStationsRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _load_stations_result(request.text)

    def load_events(
        self, request: data_pb2.LoadEventsRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _load_events_result(request.text, request.marker_budget)

    def add_cake_model(
        self, request: data_pb2.AddCakeModelRequest, data: Any
    ) -> bridge_pb2.StringValue:
        name = request.name[:-3] if request.name.lower().endswith(".nd") else request.name
        name = name or "model"
        os.makedirs(MODEL_DIR, exist_ok=True)
        with open(_cake_model_path(name), "wb") as f:
            f.write(bytes(request.content))
        return bridge_pb2.StringValue(value=name)

    async def download_waveforms(
        self, request: data_pb2.WaveformDownloadRequest, data: Any
    ) -> scene_pb2.SceneMeta:
        _operation_progress(0, None, "Downloading waveforms")
        resp = await _pyfetch_fdsn(_dataselect_url(request))
        if resp.status == 204:
            return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
        if not resp.ok:
            raise RuntimeError("dataselect: HTTP %s" % resp.status)
        raw = bytes(await _await_external_operation(resp.bytes()))
        if not raw:
            return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
        _operation_progress(len(raw), len(raw), "Parsing waveforms")
        return _load_waveform_path_scene(_stage_bytes("fdsn.mseed", raw))

    def ingest_mini_seed(
        self, request: data_pb2.IngestMiniSeedRequest, data: Any
    ) -> data_pb2.IngestMiniSeedResult:
        if request.batch_seq_last < request.batch_seq_first:
            raise ValueError("SeedLink batch sequence range is reversed")
        if request.reset and request.batch_seq_first != 0:
            raise ValueError("SeedLink reset must be batch zero")
        return data_pb2.IngestMiniSeedResult(
            scene=_append_mseed_scene(
                request.source,
                bytes(request.content),
                request.start_time,
                request.reset,
                request.retain_seconds,
            ),
            stream_id=request.stream_id,
            session_epoch=request.session_epoch,
            ack_seq_first=request.batch_seq_first,
            ack_seq_last=request.batch_seq_last,
        )

    async def catalog_search(
        self, request: data_pb2.CatalogRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        text = await _fetch_fdsn_text_or_empty(_event_url(request))
        if request.with_mt and text.lstrip().startswith("<"):
            return _parse_events_quakeml(text, request.marker_budget)
        return _parse_events_result(text, request.marker_budget)

    def export_waveforms(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.FileBlob:
        """Qt's download Save trigger, waveform half (download.py:147): every
        loaded original trace as one MiniSEED file via pyrocko's own writer;
        derived (tabu) traces stay out, as they are not data."""
        from pyrocko import io as pio

        traces = [tr for tr in _TRACES if not _is_tabu_trace(tr)]
        if not traces:
            raise ValueError("No waveforms loaded to save.")
        path = "/tmp/waveforms_out.mseed"
        pio.save(traces, path)
        with open(path, "rb") as f:
            return bridge_pb2.FileBlob(
                filename="waveforms.mseed",
                content=f.read(),
                mime="application/vnd.fdsn.mseed",
            )

    def export_stations(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.FileBlob:
        """Qt's download Save trigger, station half: the coordinate table as a
        pyrocko station file (`model.dump_stations`)."""
        from pyrocko import model

        stations = [
            model.Station(
                network=key[0],
                station=key[1],
                location=key[2],
                lat=lat,
                lon=lon,
                elevation=elev,
                depth=depth,
            )
            for key, (lat, lon, elev, depth) in _STATIONS.items()
            if len(key) == 3
        ]
        if not stations:
            raise ValueError("No station coordinates loaded to save.")
        path = "/tmp/stations_out.txt"
        model.dump_stations(stations, path)
        with open(path) as f:
            return bridge_pb2.FileBlob(
                filename="stations.txt", content=f.read().encode(), mime="text/plain"
            )

    async def fetch_stations(
        self, request: data_pb2.StationRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _parse_stations_result(await _fetch_fdsn_text_or_empty(_station_url(request)))

    async def fetch_responses(
        self, request: data_pb2.StationRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return _load_responses_result([await _fetch_fdsn_text_or_empty(_response_url(request))])

    def prepare_scientific_tools(
        self, request: bridge_pb2.Empty, data: Any
    ) -> bridge_pb2.StringValue:
        import bootstrap

        warmed = bootstrap.warm()
        return bridge_pb2.StringValue(value=",".join(warmed))

    def mount_gf_store(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.StringValue:
        if not data:
            raise ValueError("mounting a GF store requires a mounted directory path")
        return bridge_pb2.StringValue(value=gf_store_label(str(data)))


_SERVICER = _BridgeServicer()


async def call(
    method: str,
    request: Any = b"",
    data: Any = None,
    reporter: Any = None,
    begin_commit: Any = None,
    begin_external_wait: Any = None,
    end_external_wait: Any = None,
    enter_protected_frame: Any = None,
) -> Dict[str, Any]:
    """Dispatch a bridge method through the generated typed servicer; always
    return {"result": <BridgeResult bytes>, "buffer"?: ndarray}. Async because some
    rpcs do network I/O (pyfetch); never raises — any exception becomes a
    BridgeError (the request is decoded off a JS proxy here, before dispatch)."""
    token = _OPERATION_REPORTER.set(cast(Optional[OperationReporter], reporter))
    begin_wait_token = _BEGIN_EXTERNAL_WAIT.set(
        cast(Optional[ExternalWaitCallback], begin_external_wait)
    )
    end_wait_token = _END_EXTERNAL_WAIT.set(cast(Optional[ExternalWaitCallback], end_external_wait))
    commit_gate = cast(Optional[CommitGate], begin_commit)

    sealed: Optional[bool] = None

    def seal_execution() -> bool:
        """Linearize this exit against cancellation. The gate CAS disarms the
        interrupt byte and moves the control word somewhere no later
        `requestCancellation` can arm it, so rollback and reply marshalling can
        never receive a stray SIGINT. Returns False when cancellation won.

        Memoized: the gate is a one-shot CAS, so a second invocation — a commit
        that failed after sealing, unwinding into the BaseException arm — would
        read False and mislabel the failure as a cancellation nobody asked for.
        """
        nonlocal sealed
        if sealed is None:
            sealed = commit_gate is None or commit_gate()
        return sealed

    def finish_call_result(
        result: Dict[str, Any], snapshot: Optional[SessionSnapshot]
    ) -> Dict[str, Any]:
        if not seal_execution():
            if snapshot is not None:
                _SESSION_STORE.rollback(snapshot)
            return bridge_dispatch.cancelled_result()
        if snapshot is not None:
            _SESSION_STORE.rollback(snapshot)
        return result

    snapshot: Optional[SessionSnapshot] = None
    in_view_lane = False
    try:
        # Interpreter ownership is granted HERE, from the first protected
        # bytecodes, never by the worker before the call: until this callback
        # runs the control word stays ...WithoutInterpreter, where cancellation
        # cannot arm SIGINT, so Pyodide's coroutine-scheduling plumbing between
        # the JS invocation and this frame can never consume an interrupt. If
        # cancellation is already pending, the callback arms the byte and the
        # very next bytecode raises the typed cancellation inside this try.
        if enter_protected_frame is not None:
            enter_protected_frame()
        # The activity lookup runs inside the protecting try: a cancellation
        # interrupt landing in these first bytecodes — or a method id the
        # generated table does not know — must surface as a typed envelope, not
        # escape to JS as a transport error.
        activity = bridge_dispatch.ACTIVITY_BY_METHOD[method]
        if activity.lane == "VIEW":
            in_view_lane = True
            _SESSION_STORE.enter_view()
            result = await bridge_dispatch.dispatch(_SERVICER, method, _raw_bytes(request), data)
            envelope = bridge_pb2.BridgeResult.FromString(result["result"])
            if envelope.WhichOneof("result") == "cancelled":
                _invalidate_interrupted_view()
                result = await bridge_dispatch.dispatch(
                    _SERVICER, method, _raw_bytes(request), data
                )
            return finish_call_result(result, None)
        # Transaction setup runs inside the protecting try too: a cancellation
        # interrupt during the workspace clone must surface as the typed
        # cancelled envelope, not escape as a transport error with
        # _transaction_snapshot left installed.
        if activity.session_access == "MUTATING":
            snapshot = _SESSION_STORE.snapshot()
            _SESSION_STORE.begin_transaction(snapshot)
        result = await bridge_dispatch.dispatch(_SERVICER, method, _raw_bytes(request), data)
        if snapshot is None:
            return finish_call_result(result, None)
        envelope = bridge_pb2.BridgeResult.FromString(result["result"])
        if envelope.WhichOneof("result") in ("error", "cancelled", "stale"):
            return finish_call_result(result, snapshot)
        if not seal_execution():
            _SESSION_STORE.rollback(snapshot)
            return bridge_dispatch.cancelled_result()
        update = _SESSION_STORE.capture_update()
        _SESSION_STORE.restore_snapshot(snapshot)
        stale_commit = _SESSION_STORE.commit(snapshot.generation, update)
        if stale_commit is None:
            return result
        stale = bridge_pb2.BridgeResult()
        stale.stale.expected_generation = stale_commit.expected_generation
        stale.stale.actual_generation = stale_commit.actual_generation
        return {"result": stale.SerializeToString()}
    except KeyboardInterrupt:
        # On the view lane this is defensive only: no interrupt can reach it
        # today — GetView is the sole VIEW rpc and is NOT_CANCELLABLE, and the
        # ownership handoff clears the byte before a view runs. If a cancellable
        # view rpc is ever added, revisit enter_view/exit_view first: they
        # self-heal from a partial projection (retained workspace, gated cache
        # stash) but are not fully atomic. The typed envelope keeps the reply
        # structured so Rust retries the view.
        seal_execution()
        if snapshot is not None:
            _SESSION_STORE.rollback(snapshot)
        return bridge_dispatch.cancelled_result()
    except BaseException as exc:
        cancellation_won = not seal_execution()
        if snapshot is not None:
            _SESSION_STORE.rollback(snapshot)
        # Cancellation-won takes precedence over the failure, matching the
        # error-envelope path in finish_call_result.
        if cancellation_won:
            return bridge_dispatch.cancelled_result()
        return bridge_dispatch.failure_result(exc)
    finally:
        if in_view_lane:
            _SESSION_STORE.exit_view()
        _OPERATION_REPORTER.reset(token)
        _BEGIN_EXTERNAL_WAIT.reset(begin_wait_token)
        _END_EXTERNAL_WAIT.reset(end_wait_token)
