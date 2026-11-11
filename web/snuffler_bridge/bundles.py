"""Dataset bundles: zip archives carrying waveforms, metadata and events, and the example index."""

from __future__ import annotations

import io
from typing import Any, Dict, List, Optional, Tuple

from pyrocko import trace as ptrace
from snuffler.bridge.v1 import data_pb2, snuffling_pb2

from . import events, fetch, marker_budget, staging, state, stations, waveforms
from .marker_budget import MarkerBudget

# A bundle is one zip carrying a whole dataset: waveforms, station metadata and an
# event catalogue.
# Members are dispatched by content, so a hand-assembled zip loads the same way as
# the shipped examples.
_ZIP_MAGIC = b"PK\x03\x04"

# What the shipped bundles carry so a downloaded copy still says what it is.
_MANIFEST_NAME = "snuffler.json"

# Prose a bundle may carry beside its data, which no loader wants.
_SKIP_SUFFIXES = (".json", ".md", ".txt", ".py", ".cfg", ".toml")


def _member_role(name: str, payload: bytes) -> str:
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
    if lowered.endswith(_SKIP_SUFFIXES):
        return "skip"
    return "waveforms"


def _about(manifest: Dict[str, Any]) -> Optional[data_pb2.BundleAbout]:
    """What the dataset is, for the bundles that describe themselves.

    Not a load report: it stays true while the dataset is open, so the frontend
    can show it again on demand.
    ``None`` for an archive carrying no prose.
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


def load_bundle(raw: bytes, label: str, budget: MarkerBudget = None) -> data_pb2.BundleResult:
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
        if entry.filename.rsplit("/", 1)[-1] == _MANIFEST_NAME:
            try:
                manifest = dict(json.loads(archive.read(entry.filename)))
            except (ValueError, TypeError):
                manifest = {}
            continue
        payload = archive.read(entry.filename)
        groups.setdefault(_member_role(entry.filename, payload), []).append(
            (entry.filename, payload)
        )

    waveform_members = groups.get("waveforms", [])
    if not waveform_members:
        raise ValueError("%s contains no waveform files" % (label or "bundle"))

    traces: List[ptrace.Trace] = []
    for name, payload in waveform_members:
        traces.extend(waveforms.load_path_traces(staging.stage_bytes(name, payload), "detect"))
    about = _about(manifest)
    title = str(manifest.get("title", "")).strip() or label
    manifest_state = manifest.get("state")
    view_state = manifest_state if isinstance(manifest_state, dict) else {}
    view = data_pb2.BundleView(
        sort=str(view_state.get("sort", "")),
        subsort=str(view_state.get("subsort", "")),
    )

    previous = state.capture_dataset_state()
    state.install_dataset_state(state.empty_dataset_state(traces))
    try:
        scene = state.scene_meta()
        loaded = ["%d trace(s) from %d file(s)" % (scene.track_count, len(waveform_members))]
        info = snuffling_pb2.SnufflingResult(show_window=True, origin="bundle")
        bundle_markers = marker_budget.MarkerAccumulator(budget)
        responses = [
            payload.decode("utf-8", "replace") for _name, payload in groups.get("stationxml", [])
        ]
        if responses:
            loaded.append(stations.load_station_xml(responses).text)
        for name, payload in groups.get("stations", []):
            station_result = stations.load_station_file(payload.decode("utf-8", "replace"))
            if station_result.text.startswith(("could not parse", "empty")):
                raise ValueError("%s: %s" % (name, station_result.text))
            loaded.append(station_result.text)
        for name, payload in groups.get("events", []):
            event_result = events.load_event_file(payload.decode("utf-8", "replace"), budget)
            if event_result.text.startswith(("could not parse", "empty")):
                raise ValueError("%s: %s" % (name, event_result.text))
            for marker in event_result.markers:
                copy_marker = snuffling_pb2.SnufMarker()
                copy_marker.CopyFrom(marker)
                bundle_markers.add(copy_marker, marker_budget.marker_nslcs(event_result, marker))
            bundle_markers.total += int(event_result.markers_dropped)
            loaded.append(event_result.text)
        info.text = ("%s\n\n%s" % (title, "\n".join(loaded))).strip()
        bundle_markers.finish(info)
        info.event_state.CopyFrom(events.event_state(bundle_markers.limit))
        # A described dataset opens only its description, since the counts are
        # already in the status line; an undescribed archive still reports.
        info.show_window = about is None
        staged = state.capture_dataset_state()
        result = data_pb2.BundleResult(scene=scene, info=info, view=view, about=about)
    finally:
        state.install_dataset_state(previous)
    state.install_dataset_state(staged)
    return result


async def example_list() -> data_pb2.ExampleList:
    """The example index, fetched rather than compiled in, so a deployment can gain
    a dataset without a new wasm build."""
    import json

    raw = await fetch.fetch_bytes(fetch.asset_url("assets/examples/examples.json"))
    doc = json.loads(raw)
    result = data_pb2.ExampleList()
    for entry in doc.get("examples", []):
        result.examples.add(
            slug=str(entry.get("slug", "")),
            title=str(entry.get("title", "")),
            blurb=str(entry.get("blurb", "")),
            characteristics=[str(item) for item in entry.get("characteristics", [])],
            url=str(entry.get("url", "")),
            bytes=int(entry.get("bytes", 0)),
            sha256=str(entry.get("sha256", "")),
        )
    return result


async def load_requested(request: data_pb2.LoadBundleRequest) -> data_pb2.BundleResult:
    """Load a bundle sent as bytes or fetched by URL, checking its index checksum."""
    if request.content:
        raw = bytes(request.content)
        label = request.name or "bundle"
    else:
        label = request.name or request.url.rsplit("/", 1)[-1] or "bundle"
        raw = await fetch.fetch_bytes(fetch.asset_url(request.url))
    if request.sha256:
        import hashlib

        digest = hashlib.sha256(raw).hexdigest()
        if digest != request.sha256:
            # Otherwise a truncated download surfaces as a parse error deep
            # inside pyrocko.
            raise ValueError(
                "%s does not match the checksum in the index (expected %s…, got %s…): "
                "the download was truncated, or the index is stale"
                % (label, request.sha256[:12], digest[:12])
            )
    result = load_bundle(raw, label, request.marker_budget)
    # A checksummed bundle came from the examples index (user files carry
    # no checksum): it is boot/demo data, replaced by the first real load.
    if request.sha256:
        state.pile_is_example = True
    return result
