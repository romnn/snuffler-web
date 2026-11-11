"""Waveform loading: staged files, the boot example, FDSN downloads and live SeedLink data."""

from __future__ import annotations

import math
from typing import Iterable, List, Optional

import numpy as np
from pyrocko import io as pio
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import bridge_pb2, data_pb2, scene_pb2

from . import events, snuffling_scope, staging, state


def _without_empty(traces: Iterable[ptrace.Trace]) -> List[ptrace.Trace]:
    """Drop the sampleless "traces" a MiniSEED file can carry.

    Datacenters ship records with no samples and a sample rate of zero, and pyrocko
    hands each one back as a trace.
    Kept, each would draw a blank row indistinguishable from a dead channel.
    pyrocko's own ``pile`` refuses such a file outright (``dispatch_key`` takes
    ``log(deltatmin)``), so there is no upstream behaviour to match.
    """
    return [
        trace
        for trace in traces
        if trace.deltat > 0.0 and trace.ydata is not None and trace.ydata.size
    ]


def make_example_scene() -> scene_pb2.SceneMeta:
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

    state.set_traces(traces)
    # The synthetic demo is a boot pile, not user data: the first real load
    # replaces it (see `state.set_traces`).
    state.pile_is_example = True
    return state.scene_meta()


def load_path_traces(path: str, file_format: str) -> List[ptrace.Trace]:
    """Load waveform traces already staged in Pyodide's filesystem."""
    resolved_format = "detect" if file_format in (None, "", "detect") else file_format
    return _without_empty(pio.load(path, format=resolved_format))


def load_path_scene(path: str, file_format: str) -> scene_pb2.SceneMeta:
    """Replace the pile with a file already staged in Pyodide's FS."""
    state.set_traces(load_path_traces(path, file_format))
    return state.scene_meta()


def append_downloaded_scene(path: str) -> scene_pb2.SceneMeta:
    """Load staged FDSN MiniSEED bytes into the pile and return the scene.

    Appends: Qt's download snuffling adds the fetched traces to whatever is
    loaded (`Pile.add_file` via download.py:140), it never replaces the pile.
    """
    traces = list(pio.load(path, format="mseed"))
    state.set_traces(traces, append=True)
    return state.scene_meta()


def _chop_trace_from(trace: ptrace.Trace, tmin: float) -> Optional[ptrace.Trace]:
    if not math.isfinite(tmin) or tmin <= 0.0 or trace.tmin >= tmin:
        return trace
    if trace.tmax < tmin:
        return None
    try:
        return trace.chop(tmin, trace.tmax, inplace=False, include_last=True)
    except ptrace.NoData:
        return None


def _trim_live_traces(traces: List[ptrace.Trace], tmin: float) -> List[ptrace.Trace]:
    out = []
    for trace in traces:
        chopped = _chop_trace_from(trace, tmin)
        if chopped is not None:
            out.append(chopped)
    return out


def append_live_scene(
    source: str,
    raw: bytes,
    start_time: float = 0.0,
    reset: bool = False,
    retain_seconds: float = 0.0,
) -> scene_pb2.SceneMeta:
    """Append live MiniSEED records and merge adjacent segments by NSLC/time."""
    del source
    if not raw:
        return state.scene_meta()

    traces = list(pio.load(staging.stage_bytes("live.mseed", raw), format="mseed"))
    if reset:
        # Parsing has succeeded, so replacement and insertion now belong to the
        # same dispatcher transaction.
        # A malformed batch zero leaves the old scene.
        state.set_traces([])
    if start_time > 0.0:
        traces = _trim_live_traces(traces, start_time)
    if not traces:
        return state.scene_meta()

    merged_input = list(state.traces) + traces
    if retain_seconds > 0.0 and merged_input:
        latest = max(trace.tmax for trace in merged_input)
        retain_from = latest - retain_seconds
        if start_time > 0.0:
            retain_from = max(start_time, retain_from)
        merged_input = _trim_live_traces(merged_input, retain_from)
    elif start_time > 0.0:
        merged_input = _trim_live_traces(merged_input, start_time)

    merged = sorted(
        merged_input,
        key=lambda trace: (
            trace.network,
            trace.station,
            trace.location,
            trace.channel,
            trace.tmin,
        ),
    )
    state.traces = ptrace.degapper(merged)
    state.mark_traces_changed()
    return state.scene_meta()


def load_file(request: data_pb2.LoadBytesRequest) -> data_pb2.LoadBytesResult:
    """Load a file's bytes into the pile, or swap a re-read source's traces.

    Duplicates are detected by content digest, since a browser file has no path.
    """
    content = bytes(request.content)
    import hashlib

    def result(loaded: bool) -> data_pb2.LoadBytesResult:
        return data_pb2.LoadBytesResult(
            scene=state.scene_meta(), loaded=loaded, event_state=events.event_state()
        )

    digest = hashlib.sha256(content).hexdigest()
    if request.replace_source:
        if not request.source_id:
            raise ValueError("source replacement requires a source_id")
        previous_digest = state.source_digests.get(request.source_id)
        if previous_digest is None:
            raise ValueError(
                "waveform source %r was not registered by an initial load" % request.source_id
            )
        if digest == previous_digest:
            return result(False)
        if digest in state.digest_owners:
            traces = load_path_traces(staging.stage_bytes(request.name, content), request.format)
            state.replace_source(request.source_id, [])
            state.source_digests[request.source_id] = digest
            state.suppressed_source_traces[request.source_id] = traces
            state.remove_digest_owner(previous_digest, request.source_id)
            return result(True)
        traces = load_path_traces(staging.stage_bytes(request.name, content), request.format)
        state.replace_source(request.source_id, traces)
        state.source_digests[request.source_id] = digest
        state.suppressed_source_traces.pop(request.source_id, None)
        state.remove_digest_owner(previous_digest, request.source_id)
        state.add_digest_owner(digest, request.source_id)
        return result(True)

    # Refuse a file already in the pile, as Qt's pile refuses a known
    # absolute path (pile.py:1115) — by content here, since a browser file
    # has no path.
    if request.append and digest in state.digest_owners:
        # Qt logs the refusal ("File already in pile", pile.py:1117);
        # the worker console is this bridge's log.
        print("[py.load] %s already in pile, skipped" % (request.name or "file"))
        return result(False)

    path = staging.stage_bytes(request.name, content)
    traces = load_path_traces(path, request.format)
    state.set_traces(traces, append=bool(request.append))
    if request.source_id:
        state.source_traces[request.source_id] = traces
        state.source_digests[request.source_id] = digest
    if request.append or request.source_id:
        state.add_digest_owner(digest, request.source_id)
    return result(True)


def export_mseed() -> bridge_pb2.FileBlob:
    """Qt's download Save trigger, waveform half (download.py:147).

    Every loaded original trace as one MiniSEED file via pyrocko's own writer.
    Derived (tabu) traces stay out, as they are not data.
    """
    from pyrocko import io as pio

    traces = [trace for trace in state.traces if not snuffling_scope.is_tabu_trace(trace)]
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
