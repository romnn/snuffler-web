"""Bridge snuffling behaviour against real pyrocko, in plain CPython.

Tier 1 diffs the paint pipeline and never runs a snuffling; these tests drive
the snuffling entry points of web/snuffler_bridge directly — the same functions the
Pyodide worker calls — so the marker-scope chopping, aggregation and failure
contracts hold without a browser.

    task test:parity:bridge
"""

from __future__ import annotations

# harness first: it prepares sys.path and the pyrocko scratch home before
# anything imports pyrocko or the bridge.
import harness  # isort: skip # noqa: F401

import asyncio
import io
import sys
import types
import zipfile
from pathlib import Path
from typing import Any, Dict, Iterator, List, Tuple

import numpy as np
import pytest
from pyrocko import io as pio
from pyrocko import trace as ptrace
from snuffler_bridge import (
    bundles,
    commands,
    events,
    figures,
    gf_stores,
    maps,
    marker_budget,
    marker_files,
    processing,
    pyrocko_modules,
    rms,
    seisbench,
    service,
    snuffling_scope,
    snufflings,
    staging,
    state,
    stations,
    waveforms,
)
from snuffler_web_proto import bridge_dispatch, bridge_pb2, common_pb2, data_pb2, snuffling_pb2


def make_trace(
    net: str,
    sta: str,
    cha: str,
    tmin: float,
    nsamples: int,
    amplitude: float,
    deltat: float = 0.01,
) -> ptrace.Trace:
    ydata = amplitude * np.sin(np.linspace(0.0, 40.0, nsamples))
    return ptrace.Trace(
        network=net, station=sta, location="", channel=cha, deltat=deltat, tmin=tmin, ydata=ydata
    )


def load_traces(traces: List[ptrace.Trace]) -> None:
    state.traces[:] = traces
    state.mark_traces_changed()


def trace_payloads() -> List[Tuple[Tuple[str, str, str, str], bytes]]:
    """Every loaded trace's NSLC and sample bytes, to prove a call left the scene intact."""
    return [(trace.nslc_id, trace.ydata.tobytes()) for trace in state.traces]


def snuffling_request() -> snuffling_pb2.SnufflingRequestParams:
    request = snuffling_pb2.SnufflingRequestParams()
    request.view.tmin = 0.0
    request.view.tmax = 20.0
    return request


def select_span(
    request: snuffling_pb2.SnufflingRequestParams,
    tmin: float,
    tmax: float,
    nslc: Tuple[str, str, str, str],
) -> None:
    span = request.selected_spans.add()
    span.tmin, span.tmax = tmin, tmax
    nslc_id = span.nslc_ids.add()
    nslc_id.net, nslc_id.sta, nslc_id.loc, nslc_id.cha = nslc


def reset_session() -> None:
    state.traces[:] = []
    state.source_traces.clear()
    state.source_digests.clear()
    state.suppressed_source_traces.clear()
    state.digest_owners.clear()
    state.hidden_patterns[:] = []
    state.derived_traces.clear()
    state.derived_events.clear()
    state.event_registry.clear()
    state.active_event = None
    state.active_event_hash = ""
    state.active_moment_tensor = None
    state.stations.clear()
    state.warned_no_response.clear()
    state.view_notices.clear()
    state.pile_is_example = False
    state.gf_stores.pop("broken", None)
    state.mark_traces_changed()


@pytest.fixture(autouse=True)
def clean_session() -> Iterator[None]:
    # Bridge state is module-global (it is one Pyodide session).
    # Every test starts empty and scrubs what it loaded, so none can colour the next.
    reset_session()
    yield
    reset_session()


def test_initial_file_source_uses_append_rules_before_registering(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    boot = make_trace("BOOT", "DEMO", "BHZ", 0.0, 100, 1.0)
    real = make_trace("XX", "REAL", "BHZ", 0.0, 100, 2.0)
    reloaded = make_trace("XX", "REAL", "BHZ", 0.0, 100, 3.0)
    other = make_trace("YY", "OTHER", "BHZ", 0.0, 100, 4.0)
    final = make_trace("ZZ", "FINAL", "BHZ", 0.0, 100, 5.0)
    state.traces[:] = [boot]
    state.pile_is_example = True
    duplicate_other = make_trace("YY", "OTHER", "BHZ", 0.0, 100, 4.0)
    loads = iter([[real], [reloaded], [other], [duplicate_other], [final]])
    monkeypatch.setattr(staging, "stage_bytes", lambda name, content: name)
    monkeypatch.setattr(waveforms, "load_path_traces", lambda path, file_format: next(loads))
    servicer = service.BridgeServicer()

    initial = data_pb2.LoadBytesRequest(
        name="real.mseed",
        format="detect",
        append=True,
        content=b"first",
        source_id="src0",
    )
    result = servicer.load_bytes(initial, None)

    # The first real handle replaces the boot example and becomes reloadable.
    assert result.loaded
    assert state.traces == [real]
    assert state.source_traces["src0"] == [real]
    assert not state.pile_is_example

    duplicate = data_pb2.LoadBytesRequest(
        name="copy.mseed",
        format="detect",
        append=True,
        content=b"first",
        source_id="src1",
    )
    duplicate_result = servicer.load_bytes(duplicate, None)

    # Source identity does not exempt an initial open from duplicate detection.
    assert not duplicate_result.loaded
    assert "src1" not in state.source_traces
    assert state.traces == [real]

    replacement = data_pb2.LoadBytesRequest(
        name="real.mseed",
        format="detect",
        append=True,
        content=b"changed",
        source_id="src0",
        replace_source=True,
    )
    replacement_result = servicer.load_bytes(replacement, None)

    # Only the explicit reload path swaps the registered source by identity.
    assert replacement_result.loaded
    assert state.traces == [reloaded]
    assert state.source_traces["src0"] == [reloaded]

    other_request = data_pb2.LoadBytesRequest(
        name="other.mseed",
        format="detect",
        append=True,
        content=b"other",
        source_id="src2",
    )
    assert servicer.load_bytes(other_request, None).loaded

    collision = data_pb2.LoadBytesRequest(
        name="real.mseed",
        format="detect",
        append=True,
        content=b"other",
        source_id="src0",
        replace_source=True,
    )
    collision_result = servicer.load_bytes(collision, None)

    # A reload converging on another source retires its old traces atomically.
    assert collision_result.loaded
    assert state.source_traces["src0"] == []
    assert state.traces == [other]

    final_request = data_pb2.LoadBytesRequest(
        name="real.mseed",
        format="detect",
        append=True,
        content=b"final",
        source_id="src0",
        replace_source=True,
    )
    assert servicer.load_bytes(final_request, None).loaded

    repeated_other = data_pb2.LoadBytesRequest(
        name="third.mseed",
        format="detect",
        append=True,
        content=b"other",
        source_id="src3",
    )
    repeated_result = servicer.load_bytes(repeated_other, None)

    # A leaving B's digest cannot retire B's ownership and admit a third copy.
    assert not repeated_result.loaded
    assert state.traces == [other, final]


def test_suppressed_source_is_promoted_when_the_digest_owner_leaves(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    a_x = make_trace("AA", "SOURCE", "BHZ", 0.0, 100, 1.0)
    b_y = make_trace("BB", "OWNER", "BHZ", 0.0, 100, 2.0)
    a_y = make_trace("AA", "SOURCE", "BHZ", 0.0, 100, 2.0)
    b_z = make_trace("BB", "OWNER", "BHZ", 0.0, 100, 3.0)
    loads = iter([[a_x], [b_y], [a_y], [b_z]])
    monkeypatch.setattr(staging, "stage_bytes", lambda name, content: name)
    monkeypatch.setattr(waveforms, "load_path_traces", lambda path, file_format: next(loads))
    servicer = service.BridgeServicer()

    def request(source_id: str, content: bytes, replace: bool = False):
        return data_pb2.LoadBytesRequest(
            name=source_id + ".mseed",
            format="detect",
            append=True,
            content=content,
            source_id=source_id,
            replace_source=replace,
        )

    assert servicer.load_bytes(request("a", b"x"), None).loaded
    assert servicer.load_bytes(request("b", b"y"), None).loaded
    assert servicer.load_bytes(request("a", b"y", True), None).loaded
    assert state.source_traces["a"] == []
    assert state.suppressed_source_traces["a"] == [a_y]

    assert servicer.load_bytes(request("b", b"z", True), None).loaded

    # No filesystem notification is due for unchanged A. Its parsed traces
    # therefore have to take ownership as part of B retiring the shared digest.
    assert state.source_traces["a"] == [a_y]
    assert "a" not in state.suppressed_source_traces
    assert state.traces == [b_z, a_y]
    repeated = servicer.load_bytes(request("c", b"y"), None)
    assert not repeated.loaded


def test_failed_bundle_load_restores_the_entire_previous_dataset(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from pyrocko import model

    old = make_trace("OLD", "KEEP", "BHZ", 0.0, 100, 1.0)
    new = make_trace("NEW", "DROP", "BHZ", 0.0, 100, 2.0)
    old_event = model.Event(time=5.0, lat=1.0, lon=2.0, depth=3000.0, name="old")
    stationxml = object()
    state.traces[:] = [old]
    state.source_traces["old"] = [old]
    state.source_digests["old"] = "old-digest"
    state.digest_owners["old-digest"] = {"source:old"}
    state.stations[("OLD", "KEEP")] = (1.0, 2.0, 0.0, 0.0)
    state.event_registry["old"] = old_event
    state.station_xmls.append(stationxml)
    state.activate_event("old")

    def archive(entries: List[Tuple[str, bytes]]) -> bytes:
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w") as bundle:
            for name, payload in entries:
                bundle.writestr(name, payload)
        return out.getvalue()

    monkeypatch.setattr(staging, "stage_bytes", lambda name, content: name)

    def load_path(path: str, file_format: str) -> List[ptrace.Trace]:
        if "invalid" in path:
            raise ValueError("invalid waveform")
        return [new]

    monkeypatch.setattr(waveforms, "load_path_traces", load_path)

    with pytest.raises(ValueError, match="invalid waveform"):
        bundles.load_bundle(
            archive([("a-valid.mseed", b"valid"), ("z-invalid.mseed", b"invalid")]),
            "broken-waveforms.zip",
        )

    def fail_metadata(texts: List[str]) -> Any:
        raise ValueError("invalid StationXML")

    monkeypatch.setattr(stations, "load_station_xml", fail_metadata)
    with pytest.raises(ValueError, match="invalid StationXML"):
        bundles.load_bundle(
            archive([("a-valid.mseed", b"valid"), ("metadata.xml", b"invalid")]),
            "broken-metadata.zip",
        )

    # Neither waveform parsing nor later metadata parsing may publish a partial
    # replacement of the live dataset.
    assert state.traces == [old]
    assert state.source_traces == {"old": [old]}
    assert state.source_digests == {"old": "old-digest"}
    assert state.digest_owners == {"old-digest": {"source:old"}}
    assert state.stations[("OLD", "KEEP")] == (1.0, 2.0, 0.0, 0.0)
    assert state.event_registry == {"old": old_event}
    assert state.active_event_hash == "old"
    assert state.station_xmls == [stationxml]


def test_valid_live_reset_retires_the_boot_example_flag(tmp_path: Path) -> None:
    state.traces[:] = [make_trace("BOOT", "DEMO", "BHZ", 0.0, 100, 1.0)]
    state.pile_is_example = True
    live = make_trace("LV", "STRM", "BHZ", 0.0, 100, 1.0)
    path = tmp_path / "live.mseed"
    pio.save([live], str(path), format="mseed")

    waveforms.append_live_scene("live", path.read_bytes(), reset=True)

    assert [trace.nslc_id for trace in state.traces] == [live.nslc_id]
    assert not state.pile_is_example


def test_missing_response_without_stationxml_emits_a_notice() -> None:
    trace = make_trace("XX", "STA1", "BHZ", 0.0, 100, 1.0)

    assert processing.remove_response(trace, True) is trace
    assert state.view_notices == ["no instrument response information for XX.STA1..BHZ"]


def test_map_resolves_requested_registry_events_and_active_stations() -> None:
    from pyrocko import model

    event = model.Event(
        time=10.0,
        lat=1.0,
        lon=2.0,
        depth=3000.0,
        magnitude=4.5,
        name="event-a",
    )
    state.event_registry["event-a"] = event
    state.active_event = (1.0, 2.0, 3000.0, 10.0)
    state.stations[("XX", "NEAR")] = (3.0, 4.0, 0.0, 0.0)
    state.stations[("XX", "FAR")] = (5.0, 6.0, 0.0, 0.0)
    state.traces[:] = [
        make_trace("XX", "NEAR", "BHZ", 0.0, 100, 1.0),
        make_trace("XX", "FAR", "BHZ", 7200.0, 100, 1.0),
    ]

    result = maps.map_data(
        data_pb2.MapRequest(
            event_hashes=["event-a"],
            only_active_event=True,
            active_event_hash="event-a",
            active_event_time=10.0,
        )
    )

    # The event registry is the coordinate owner, including events absent from
    # any catalog-specific side list.
    assert [item.event_hash for item in result.events] == ["event-a"]
    assert [(item.lat, item.lon) for item in result.events] == [(1.0, 2.0)]
    assert [station.label for station in result.stations] == ["XX.NEAR"]


def test_event_state_canonicalizes_creation_and_edits() -> None:
    state.stations[("XX", "STA1", "")] = (12.5, 45.25, 0.0, 0.0)
    request = data_pb2.SetActiveEventRequest(
        event_hash="local:event",
        time=123.0,
        nslc=common_pb2.Nslc(net="XX", sta="STA1", loc="", cha="BHZ"),
    )

    created = events.set_active_event(request)

    assert created.active_hash == "local:event"
    snapshot = created.events[0]
    assert (snapshot.time, snapshot.lat, snapshot.lon, snapshot.depth) == (
        123.0,
        12.5,
        45.25,
        0.0,
    )

    marker_file = data_pb2.MarkerFile()
    marker = marker_file.markers.add()
    marker.tmin = 123.0
    marker.tmax = 123.0
    marker.is_event = True
    marker.event_hash = "local:event"
    exported = marker_files.markers_to_text(marker_file)
    round_tripped = marker_files.markers_from_text(exported)

    # Export resolves the registry snapshot instead of rebuilding missing Rust
    # fields as a zero-coordinate event.
    assert round_tripped.markers[0].lat == 12.5
    assert round_tripped.markers[0].lon == 45.25

    snapshot.lat = 13.0
    snapshot.depth = 4200.0
    snapshot.name = "edited"
    updated = events.update_event(snapshot)

    canonical = updated.events[0]
    assert (canonical.lat, canonical.depth, canonical.name) == (13.0, 4200.0, "edited")
    assert state.active_event == (13.0, 45.25, 4200.0, 123.0)


def test_removing_events_retires_the_active_registry_entry() -> None:
    from pyrocko import model

    removed = model.Event(time=10.0, lat=1.0, lon=2.0, name="removed")
    retained = model.Event(time=20.0, lat=3.0, lon=4.0, name="retained")
    state.event_registry["removed"] = removed
    state.event_registry["retained"] = retained
    state.activate_event("removed")

    result = events.remove_events(data_pb2.RemoveEventsRequest(event_hashes=["removed", "removed"]))

    assert not result.has_event
    assert state.active_event_hash == ""
    assert list(state.event_registry) == ["retained"]
    goto = commands.goto_result(["removed"], 0.0, 30.0)
    assert goto.action == "error"


def test_catalog_activation_replaces_origin_hash_and_moment_tensor_together(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from pyrocko import model
    from pyrocko import moment_tensor as mtm

    old = model.Event(
        time=1.0,
        lat=1.0,
        lon=2.0,
        magnitude=5.0,
        name="old",
        moment_tensor=mtm.MomentTensor(
            mnn=1.0,
            mee=-1.0,
            mdd=0.0,
            mne=0.0,
            mnd=0.0,
            med=0.0,
        ),
    )
    state.event_registry["old"] = old
    state.activate_event("old")
    text_event = "|".join(
        [
            "new",
            "1970-01-01T00:00:10",
            "10",
            "20",
            "5",
            "author",
            "catalog",
            "",
            "",
            "",
            "4.0",
            "",
            "region",
        ]
    )

    text_result = events.parse_fdsn_text(text_event)

    assert state.active_event_hash == "new"
    assert state.active_moment_tensor is None
    old_metric = next(
        metric for metric in text_result.event_state.kagan if metric.event_hash == "old"
    )
    assert not old_metric.HasField("kagan_deg")

    state.activate_event("old")
    local = model.Event(time=20.0, lat=30.0, lon=40.0, magnitude=3.0, name="local")
    monkeypatch.setattr(model, "load_events", lambda path: [local])

    local_result = events.load_event_file("event data")

    assert state.active_event_hash == "local"
    assert state.active_moment_tensor is None
    old_metric = next(
        metric for metric in local_result.event_state.kagan if metric.event_hash == "old"
    )
    assert not old_metric.HasField("kagan_deg")

    state.activate_event("old")
    pyrocko_modules.marker_module()
    from pyrocko.io import quakeml

    empty = types.SimpleNamespace(get_pyrocko_events=lambda: [])
    monkeypatch.setattr(quakeml.QuakeML, "load_xml", staticmethod(lambda **kwargs: empty))

    empty_result = events.parse_quakeml("<quakeml />")

    assert state.active_event is None
    assert state.active_event_hash == ""
    assert state.active_moment_tensor is None
    assert not empty_result.event_state.has_event


def test_minmax_folds_file_split_fragments_into_one_row() -> None:
    # One channel split across two contiguous files with different amplitudes,
    # plus a second channel.
    load_traces(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 1000, 1.0),
            make_trace("XX", "STA1", "BHZ", 10.0, 1000, 5.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 2.0),
        ]
    )
    result = asyncio.run(snufflings.run("minmax", snuffling_request()))
    rows = [line for line in result.text.splitlines()[1:] if line.strip()]
    # Qt prints one aggregate per NSLC (`trace.minmax`, minmax.py:49) — never
    # one row of local extrema per file fragment.
    assert len(rows) == 2
    (bhz_row,) = [line for line in rows if "BHZ" in line]
    fields = bhz_row.split()
    # The extrema span both fragments: the quieter first file must not mask
    # the ±5 swing of the second.
    assert abs(float(fields[1]) + 5.0) < 0.1
    assert abs(float(fields[2]) - 5.0) < 0.1


def test_minmax_honours_the_markers_channel_scope() -> None:
    load_traces(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 2.0),
        ]
    )
    request = snuffling_request()
    select_span(request, 0.0, 20.0, ("XX", "STA1", "*", "BHN"))
    result = asyncio.run(snufflings.run("minmax", request))
    rows = [line for line in result.text.splitlines()[1:] if line.strip()]
    # A channel-scoped selected marker admits only its own channel.
    assert len(rows) == 1
    assert "BHN" in rows[0]


def test_the_chopper_adapter_degaps_and_orders() -> None:
    load_traces(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 1000, 1.0),
            make_trace("XX", "STA1", "BHZ", 10.0, 1000, 5.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 2.0),
        ]
    )
    segments = [
        trace
        for batch in snuffling_scope.chopper_selected_traces(snuffling_request())
        for trace in batch.traces
    ]
    # Default kwargs degap: the contiguous file-split channel arrives as one
    # joined trace, in the pile chopper's `full_id` order (pile.py:1213).
    assert len([trace for trace in segments if trace.channel == "BHZ"]) == 1
    assert segments == sorted(segments, key=lambda trace: trace.full_id)


def test_stalta_and_rms_cross_a_contiguous_file_boundary() -> None:
    fragments = [
        make_trace("XX", "STA1", "BHZ", -1.0, 200, 1.0),
        make_trace("XX", "STA1", "BHZ", 1.0, 200, 2.0),
    ]
    load_traces(fragments)
    request = snuffling_request()
    request.view.tmin = 0.0
    request.view.tmax = 2.0
    request.snufflings.stalta.short_window_s = 0.1
    request.snufflings.stalta.long_to_short_window_ratio = 2.0
    request.snufflings.stalta.trigger_level = 0.5
    request.snufflings.stalta.show_level_traces = True

    asyncio.run(snufflings.run("stalta", request))

    stalta = state.derived_traces["stalta"]
    assert len([trace for trace in stalta if trace.station == "STA1"]) == 1

    request.snufflings.rms.block_length_s = 2.0
    asyncio.run(rms.run(request))

    rms_traces = state.derived_traces["rms"]
    assert len(rms_traces) == 1
    assert rms_traces[0].data_len() == 1


def test_spectrogram_inventory_merges_split_fragments() -> None:
    load_traces(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 1000, 1.0),
            make_trace("XX", "STA1", "BHZ", 10.0, 1000, 2.0),
        ]
    )

    _tmin, _tmax, panels = figures.spectrogram_inventory(snuffling_request(), 5.0, 1.0)

    assert panels == [(("XX", "STA1", "", "BHZ"), 0.01)]


def test_failed_stalta_and_rms_calls_preserve_the_committed_scene(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    load_traces([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    stale_stalta = make_trace("DER", "STA", "BHZ", 0.0, 100, 1.0)
    stale_rms = make_trace("DER", "RMS", "BHZ", 0.0, 100, 1.0)
    state.replace_derived_traces("stalta", [stale_stalta])
    state.replace_derived_traces("rms", [stale_rms])

    def fail_chopper(*args: Any, **kwargs: Any) -> Iterator[Any]:
        yield from ()
        raise RuntimeError("injected chopper failure")

    monkeypatch.setattr(snuffling_scope, "chopper_selected_traces", fail_chopper)

    before = trace_payloads()
    stalta_request = snuffling_pb2.NamedSnufflingRequest(name="stalta", params=snuffling_request())
    stalta_reply = asyncio.run(
        service.call(
            "/snuffler_web_proto.BridgeService/RunSnuffling",
            stalta_request.SerializeToString(),
        )
    )
    stalta_result = bridge_pb2.BridgeResult.FromString(stalta_reply["result"])

    assert stalta_result.WhichOneof("result") == "error"
    assert "injected chopper failure" in stalta_result.error.message
    assert trace_payloads() == before

    rms_reply = asyncio.run(
        service.call(
            "/snuffler_web_proto.BridgeService/RmsTraces",
            snuffling_request().SerializeToString(),
        )
    )
    rms_result = bridge_pb2.BridgeResult.FromString(rms_reply["result"])

    assert rms_result.WhichOneof("result") == "error"
    assert "injected chopper failure" in rms_result.error.message
    assert trace_payloads() == before
    assert len(state.derived_traces["stalta"]) == 1
    assert len(state.derived_traces["rms"]) == 1


@pytest.mark.parametrize(
    "failure",
    [SystemExit("stop"), GeneratorExit("close"), BaseException("base")],
    ids=["system-exit", "generator-exit", "base-exception"],
)
def test_outer_boundary_serializes_every_base_exception(
    monkeypatch: pytest.MonkeyPatch, failure: BaseException
) -> None:
    load_traces([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    before = trace_payloads()

    async def fail_dispatch(*args: Any, **kwargs: Any) -> Dict[str, Any]:
        raise failure

    monkeypatch.setattr(bridge_dispatch, "dispatch", fail_dispatch)
    reply = asyncio.run(service.call("/snuffler_web_proto.BridgeService/EmptyScene"))
    result = bridge_pb2.BridgeResult.FromString(reply["result"])

    assert result.WhichOneof("result") == "error"
    assert result.error.kind == type(failure).__name__
    assert result.error.message == str(failure)
    assert trace_payloads() == before


def test_cancellation_gate_wins_before_commit_and_preserves_the_scene() -> None:
    load_traces([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    before = trace_payloads()

    reply = asyncio.run(
        service.call(
            "/snuffler_web_proto.BridgeService/EmptyScene",
            begin_commit=lambda: False,
        )
    )
    result = bridge_pb2.BridgeResult.FromString(reply["result"])

    assert result.WhichOneof("result") == "cancelled"
    assert trace_payloads() == before


def test_malformed_seedlink_reset_preserves_the_committed_scene() -> None:
    load_traces([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    before = trace_payloads()
    request = data_pb2.IngestMiniSeedRequest(
        source="test",
        content=b"not miniSEED",
        reset=True,
        stream_id=7,
        session_epoch=3,
        batch_seq_first=0,
        batch_seq_last=0,
    )

    reply = asyncio.run(
        service.call(
            "/snuffler_web_proto.BridgeService/IngestMiniSeed",
            request.SerializeToString(),
        )
    )
    result = bridge_pb2.BridgeResult.FromString(reply["result"])

    assert result.WhichOneof("result") == "error"
    assert trace_payloads() == before


def test_marker_budget_truncates_before_serialization_and_interns_nslcs() -> None:
    result = snuffling_pb2.SnufflingResult()
    result.nslc_table.add(net="XX", sta="STA", loc="", cha="BHZ")
    for index, strength in enumerate((0.1, 0.9, 0.2, 0.8, 0.3)):
        marker = result.markers.add()
        marker.tmin = float(index)
        marker.incidence_angle = strength
        marker.nslc_indexes.append(0)

    marker_budget.truncate_marker_result(result, snuffling_pb2.MarkerBudget(max_markers=2))

    assert len(result.markers) == 2
    assert [marker.tmin for marker in result.markers] == [1.0, 3.0]
    assert result.markers_dropped == 3
    assert result.marker_budget == 2
    assert len(result.nslc_table) == 1
    assert all(list(marker.nslc_indexes) == [0] for marker in result.markers)


def test_marker_accumulator_bounds_candidates_and_compacts_unique_nslcs() -> None:
    accumulator = marker_budget.MarkerAccumulator(snuffling_pb2.MarkerBudget(max_markers=3))
    for index in range(1000):
        marker = snuffling_pb2.SnufMarker(tmin=float(index))
        accumulator.add(
            marker,
            [("XX", f"S{index:04d}", "", "BHZ")],
            strength=float(index),
        )

    assert len(accumulator._heap) == 3
    result = accumulator.finish(snuffling_pb2.SnufflingResult())
    assert len(result.markers) == 3
    assert [marker.tmin for marker in result.markers] == [997.0, 998.0, 999.0]
    assert len(result.nslc_table) == 3
    assert result.markers_dropped == 997


def test_marker_accumulator_retains_event_and_active_markers_under_pressure() -> None:
    # Event markers carry no incidence angle and score 0.0; without a retention
    # tier, an over-budget file of strong phase picks would displace every
    # event, leaving the picks' event_hash references dangling.
    accumulator = marker_budget.MarkerAccumulator(snuffling_pb2.MarkerBudget(max_markers=4))
    for index in range(100):
        pick = snuffling_pb2.SnufMarker(tmin=float(index))
        pick.incidence_angle = 50.0 + float(index)
        accumulator.add(pick)
    event = snuffling_pb2.SnufMarker(tmin=1000.0, is_event=True)
    accumulator.add(event)
    active = snuffling_pb2.SnufMarker(tmin=2000.0, is_event=True, active=True)
    accumulator.add(active)

    result = accumulator.finish(snuffling_pb2.SnufflingResult())
    assert len(result.markers) == 4
    retained_events = [marker for marker in result.markers if marker.is_event]
    assert {marker.tmin for marker in retained_events} == {1000.0, 2000.0}
    assert any(marker.active for marker in retained_events)


def test_workspace_clone_preserves_event_identity_across_the_event_maps() -> None:
    # state.replace_derived_events retires registry entries by object identity,
    # and state.active_moment_tensor aliases the registered event's moment tensor.
    # The clone must keep those aliases (one shared deepcopy memo); independent
    # copies would leak superseded derived events in the registry forever.
    from pyrocko import model, moment_tensor

    event = model.Event(time=1.0, lat=0.0, lon=0.0, depth=0.0)
    event.moment_tensor = moment_tensor.MomentTensor(strike=30.0, dip=60.0, rake=90.0)
    dataset = state.empty_dataset_state([])
    dataset = dataset._replace(
        derived_events={"seismosizer": {"h1": event}},
        event_registry={"h1": event},
        active_moment_tensor=event.moment_tensor,
        active_event_hash="h1",
    )

    clone = state.clone_dataset_state(dataset)

    assert clone.derived_events["seismosizer"]["h1"] is clone.event_registry["h1"]
    assert clone.active_moment_tensor is clone.event_registry["h1"].moment_tensor
    assert clone.event_registry["h1"] is not event


def test_event_projection_is_bounded_and_retains_the_active_event() -> None:
    from pyrocko import model

    for index in range(5):
        state.register_event(
            f"event-{index}",
            model.Event(time=float(index), lat=float(index), lon=0.0, depth=0.0),
        )
    state.activate_event("event-0")

    projection = events.event_state(2)

    assert len(projection.events) == 2
    assert {event.event_hash for event in projection.events} == {"event-0", "event-4"}
    assert len(projection.kagan) == 2


def test_interrupted_view_invalidation_preserves_frozen_scales() -> None:
    state.cached_pile = object()
    state.cached_pile_generation = 9
    state.cutout_key = np.array([1.0])
    state.cutout_traces = [make_trace("XX", "STA", "BHZ", 0.0, 10, 1.0)]
    state.cutout_notices[:] = ["cutout"]
    state.view_notices[:] = ["view"]
    state.fixed_data_ranges["track"] = (1.0, 2.0)

    state.invalidate_interrupted_view()

    assert state.cached_pile is None
    assert state.cached_pile_generation is None
    assert state.cutout_key is None
    assert state.cutout_traces is None
    assert state.cutout_notices == []
    assert state.view_notices == []
    assert state.fixed_data_ranges == {"track": (1.0, 2.0)}


def test_polarization_analyses_a_hidden_component() -> None:
    load_traces(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 1.5),
            make_trace("XX", "STA1", "BHE", 0.0, 2000, 2.0),
        ]
    )
    state.hidden_patterns.append("*.*.*.BHE")
    state.mark_traces_changed()
    request = snuffling_request()
    select_span(request, 5.0, 6.0, ("XX", "STA1", "", "BHZ"))
    result = asyncio.run(snufflings.run("polarization", request))
    # Qt choppers the raw pile with only its channel pattern
    # (polarization.py:300): hiding one component must not break the station's
    # three-component analysis.
    assert any("STA1" in line for line in result.text.splitlines()[1:])


def test_polarization_requires_the_padded_window() -> None:
    # The analysis window is [5, 6] (t_length 1, offset 0); Qt demands data
    # across the window plus one window length of display margin — [4, 7].
    def has_result(tmin: float, nsamples: int) -> bool:
        load_traces(
            [
                make_trace("XX", "STA1", cha, tmin, nsamples, amp)
                for cha, amp in (("BHZ", 1.0), ("BHN", 1.5), ("BHE", 2.0))
            ]
        )
        request = snuffling_request()
        select_span(request, 5.0, 6.0, ("XX", "STA1", "", "BHZ"))
        result = asyncio.run(snufflings.run("polarization", request))
        return any("STA1" in line for line in result.text.splitlines()[1:])

    # Data covering only [4.5, 6.5] misses the margin: no result — where an
    # unpadded chop would happily produce one.
    assert not has_result(4.5, 200)
    # Covering [3.9, 7.1] satisfies it.
    assert has_result(3.9, 320)


def test_seisbench_grid_keeps_every_fragment() -> None:
    # A channel split across two contiguous files must land in the model input
    # whole, as seisbench's stream_to_array writes each trace at its offset;
    # keeping only the last fragment would silently amputate the first file.
    spec = {"sampling_rate": 100.0, "component_order": "ZNE"}
    fragments = [
        make_trace("XX", "STA1", "BHZ", 0.0, 300, 1.0),
        make_trace("XX", "STA1", "BHZ", 3.0, 300, 5.0),
    ]
    groups = seisbench.station_groups(fragments, 0.0, 6.0, spec)
    ((group_tmin, data, nslc),) = groups.values()
    assert group_tmin == 0.0
    assert nslc == ("XX", "STA1", "", "BHZ")
    vertical = data[0]
    # Both halves carry signal: the first fragment was not overwritten by the
    # second.
    assert float(np.abs(vertical[:290]).max()) > 0.0
    assert float(np.abs(vertical[310:]).max()) > 0.0


def test_seisbench_rescaling_changes_the_model_time_grid() -> None:
    spec = {"sampling_rate": 100.0, "component_order": "ZNE"}
    trace = make_trace("XX", "STA1", "BHZ", 0.0, 300, 1.0)

    native = seisbench.station_groups([trace], 0.0, 3.0, spec)
    stretched = seisbench.station_groups([trace], 0.0, 3.0, spec, 2.0)

    native_samples = next(iter(native.values()))[1].shape[1]
    stretched_samples = next(iter(stretched.values()))[1].shape[1]
    assert stretched_samples > native_samples


def test_stalta_cleanup_preserves_user_cg_locations() -> None:
    user = make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)
    user.location = "cg"
    stale = make_trace("DER", "STA", "BHZ", 0.0, 2000, 1.0)
    stale.meta = {"tabu": True}
    load_traces([user])
    state.replace_derived_traces("stalta", [stale])
    request = snuffling_request()
    request.snufflings.stalta.short_window_s = 0.2
    request.snufflings.stalta.long_to_short_window_ratio = 3.0
    request.snufflings.stalta.trigger_level = 0.5

    asyncio.run(snufflings.run("stalta", request))

    assert user in state.traces
    assert stale not in state.traces


def test_rms_cleanup_preserves_user_rms_channel_names() -> None:
    user = make_trace("XX", "STA1", "BHZ-RMS", 0.0, 2000, 1.0)
    load_traces([user])
    request = snuffling_request()
    request.snufflings.rms.block_length_s = 1.0

    asyncio.run(rms.run(request))

    assert user in state.traces
    assert user not in state.derived_traces["rms"]


def test_seisbench_streams_each_batch_through_inference(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # 700 s of three-component data: three 300-second chopper windows.
    load_traces(
        [
            make_trace("XX", "STA1", cha, 0.0, 70_000, amp)
            for cha, amp in (("BHZ", 1.0), ("BHN", 1.5), ("BHE", 2.0))
        ]
    )
    call_order: List[str] = []
    chopper_pads: List[float] = []
    filter_calls: List[Tuple[float | None, float | None, float]] = []

    real_chopper = snuffling_scope.chopper_selected_traces

    def logging_chopper(*args: Any, **kwargs: Any) -> Iterator[Any]:
        chopper_pads.append(float(kwargs.get("tpad", 0.0)))
        for batch in real_chopper(*args, **kwargs):
            call_order.append("chop")
            yield batch

    monkeypatch.setattr(snuffling_scope, "chopper_selected_traces", logging_chopper)

    spec: Dict[str, Any] = {"sampling_rate": 100.0, "component_order": "ZNE"}

    async def fake_ensure(model: str, weights: str, base_url: str) -> Tuple[str, Dict[str, Any]]:
        return "fake://model", spec

    monkeypatch.setattr(seisbench, "_ensure_model", fake_ensure)
    monkeypatch.setattr(
        seisbench,
        "_apply_view_filter",
        lambda trace, highpass, lowpass, tcut: filter_calls.append((highpass, lowpass, tcut))
        or trace,
    )

    # A stand-in inference module: no ONNX runtime, but the same surface the
    # picker consumes — enough to observe when annotation happens.
    fake = types.ModuleType("seisbench_infer")

    async def annotate(
        url: str, data: np.ndarray, model_spec: Dict[str, Any], batch_done: Any, prefix: str
    ) -> Any:
        call_order.append("annotate")
        batch_done()
        return types.SimpleNamespace(
            data=np.zeros((data.shape[1], 3), dtype=np.float32),
            start_offset=0.0,
            sampling_rate=100.0,
        )

    fake.annotate = annotate  # type: ignore[attr-defined]
    fake.batch_count = lambda data, model_spec: 1  # type: ignore[attr-defined]
    fake.labels = lambda model_spec: ("P", "S")  # type: ignore[attr-defined]
    fake.phase_channel = lambda model_spec, phase: 1 if phase == "P" else 2  # type: ignore[attr-defined]
    fake.extract_picks = lambda arr, threshold: []  # type: ignore[attr-defined]
    fake.default_threshold = lambda model_spec, phase: 0.3  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "seisbench_infer", fake)

    request = snuffling_request()
    request.view.tmax = 700.0
    request.view.highpass = 2.0
    request.snufflings.seisbench.use_predefined_filters = True
    result = asyncio.run(seisbench.pick(request))

    # Inference runs inside the chopper loop: the first window annotates
    # before the second is even chopped, so batches stream and release
    # instead of every station grid accumulating in memory first.
    assert call_order[:2] == ["chop", "annotate"], call_order
    assert call_order.count("chop") == 3, call_order
    assert chopper_pads == [2.0]
    assert filter_calls
    assert all(call == (2.0, None, 1.0) for call in filter_calls)
    assert result.origin == "seisbench"
    assert "1 station(s)" in result.text


def test_snuffling_preflight_failures_keep_their_cleanup_origin() -> None:
    request = snuffling_pb2.NamedSnufflingRequest(name="cake")
    request.params.snufflings.cake.model = gf_stores.LABEL_PREFIX + "missing"

    result = asyncio.run(service.BridgeServicer().run_snuffling(request, None))

    # Origin is assigned before Cake's active-event preflight can refuse the run.
    assert result.origin == "cake"
    assert "No active event" in result.text


def test_a_failed_seismosizer_run_leaves_the_cleaned_state() -> None:
    from pyrocko import model

    load_traces([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    # A previous successful run left synthetics in the pile.
    stale = make_trace("SY", "SYN1", "Z", 0.0, 500, 1.0)
    state.replace_derived_traces("seismosizer", [stale])
    stale_event = model.Event(time=10.0, lat=1.0, lon=2.0, name="stale synthetic")
    state.replace_derived_events("seismosizer", {"stale": stale_event})
    state.activate_event("stale")
    assert stale in state.traces

    state.gf_stores["broken"] = "/nonexistent-store-dir"
    request = snuffling_request()
    request.snufflings.seismosizer.store = gf_stores.LABEL_PREFIX + "broken"
    result = asyncio.run(snufflings.run("seismosizer", request))

    # Qt's `call()` cleans up before any validation (seismosizer.py:97), and
    # every failure — this one dies opening the store — must ship the cleaned
    # scene with the origin tag that retires the stale event marker.
    assert result.origin == "seismosizer"
    assert result.HasField("scene")
    assert result.text.strip()
    assert stale not in state.traces
    assert state.derived_traces.get("seismosizer", []) == []
    assert "stale" not in state.event_registry
    assert state.derived_events.get("seismosizer", {}) == {}
    assert state.active_event_hash == ""
    assert result.HasField("event_state")
    assert not result.event_state.has_event


def test_seismosizer_cleanup_preserves_a_newer_event_with_the_same_hash() -> None:
    from pyrocko import model

    synthetic = model.Event(time=10.0, lat=1.0, lon=2.0, name="synthetic")
    replacement = model.Event(time=20.0, lat=3.0, lon=4.0, name="catalog replacement")
    state.replace_derived_events("seismosizer", {"shared": synthetic})
    state.event_registry["shared"] = replacement
    state.activate_event("shared")

    state.replace_derived_events("seismosizer", {})

    # Cleanup owns the object it inserted, not every future use of its hash.
    assert state.event_registry["shared"] is replacement
    assert state.active_event_hash == "shared"
    assert state.active_event == (3.0, 4.0, 0.0, 20.0)


def test_reload_snapshot_carries_the_session_store() -> None:
    # Shift+R must not reset the session generation Rust fences view replies
    # with: a fresh store at generation zero would leave every later view
    # rejected as stale.
    store = state.session_store
    saved = state.reload_snapshot()
    try:
        assert saved.get("session_store") is store
    finally:
        state.reload_restore(saved)
    assert state.session_store is store


def test_view_projection_is_shallow_and_preserves_workspace_identity() -> None:
    # An interleaved view projects committed state without deep-copying the
    # workspace, and reinstalls the same trace objects afterwards — a handler
    # suspended at a safe point may still hold references into the workspace,
    # and replacing them with clones would silently drop its later mutations
    # from the committed update.
    load_traces([make_trace("XX", "AAA", "BHZ", 0.0, 512, 1.0)])
    snapshot = state.session_store.snapshot()
    state.session_store.begin_transaction(snapshot)
    try:
        held = state.traces[0]
        committed = snapshot.state.traces[0]
        assert held is not committed

        state.session_store.enter_view()
        assert state.traces[0] is committed
        state.session_store.exit_view()
        assert state.traces[0] is held

        held.set_ydata(held.get_ydata() * 2.0)
        update = state.session_store.capture_update()
        state.session_store.restore_snapshot(snapshot)
        outcome = state.session_store.commit(snapshot.generation, update)
        assert outcome is None
        assert state.traces[0] is held
    finally:
        if state.session_store._transaction_snapshot is not None:
            state.session_store.rollback(snapshot)
        load_traces([])


@pytest.mark.parametrize("failure", [RuntimeError("injected failure"), KeyboardInterrupt()])
def test_failed_or_cancelled_mutation_preserves_committed_analysis_results(
    monkeypatch: pytest.MonkeyPatch, failure: BaseException
) -> None:
    load_traces([make_trace("XX", "OLD", "BHZ", 0.0, 100, 1.0)])
    state.replace_derived_traces("stalta", [make_trace("DER", "OLD", "BHZ", 0.0, 100, 1.0)])
    before = trace_payloads()

    async def fail_dispatch(*args: Any, **kwargs: Any) -> Dict[str, Any]:
        state.replace_derived_traces("stalta", [make_trace("DER", "NEW", "BHZ", 0.0, 100, 2.0)])
        raise failure

    monkeypatch.setattr(bridge_dispatch, "dispatch", fail_dispatch)
    reply = asyncio.run(service.call("/snuffler_web_proto.BridgeService/RunSnuffling"))
    result = bridge_pb2.BridgeResult.FromString(reply["result"])
    assert result.WhichOneof("result") == (
        "cancelled" if isinstance(failure, KeyboardInterrupt) else "error"
    )
    assert trace_payloads() == before
    assert state.derived_traces["stalta"][0].station == "OLD"


def test_stale_commit_requires_an_injected_generation_fault(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    load_traces([make_trace("XX", "OLD", "BHZ", 0.0, 100, 1.0)])
    normal = bridge_pb2.BridgeResult.FromString(
        asyncio.run(service.call("/snuffler_web_proto.BridgeService/EmptyScene"))["result"]
    )
    assert normal.WhichOneof("result") == "scene"
    before = state.session_store.generation
    commit = state.session_store.commit

    def stale_commit(expected: int, update: state.SessionUpdate) -> Any:
        return commit(expected - 1, update)

    monkeypatch.setattr(state.session_store, "commit", stale_commit)
    result = bridge_pb2.BridgeResult.FromString(
        asyncio.run(service.call("/snuffler_web_proto.BridgeService/EmptyScene"))["result"]
    )
    assert result.WhichOneof("result") == "stale"
    assert result.stale.expected_generation == before - 1
    assert result.stale.actual_generation == before
    assert state.session_store.generation == before
