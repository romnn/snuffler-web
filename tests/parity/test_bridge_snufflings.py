"""Bridge snuffling behaviour against real pyrocko, in plain CPython.

Tier 1 diffs the paint pipeline and never runs a snuffling; these tests drive
the snuffling entry points of web/bridge.py directly — the same functions the
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

import bridge
import numpy as np
import pytest
from pyrocko import trace as ptrace


def make_trace(
    net: str, sta: str, cha: str, tmin: float, n: int, amp: float, deltat: float = 0.01
) -> ptrace.Trace:
    y = amp * np.sin(np.linspace(0.0, 40.0, n))
    return ptrace.Trace(
        network=net, station=sta, location="", channel=cha, deltat=deltat, tmin=tmin, ydata=y
    )


def load(traces: List[ptrace.Trace]) -> None:
    bridge._TRACES[:] = traces
    bridge._mark_traces_changed()


def base_req() -> bridge.snuffling_pb2.SnufflingRequestParams:
    req = bridge.snuffling_pb2.SnufflingRequestParams()
    req.view.tmin = 0.0
    req.view.tmax = 20.0
    return req


def select_span(
    req: bridge.snuffling_pb2.SnufflingRequestParams,
    tmin: float,
    tmax: float,
    nslc: Tuple[str, str, str, str],
) -> None:
    span = req.selected_spans.add()
    span.tmin, span.tmax = tmin, tmax
    n = span.nslc_ids.add()
    n.net, n.sta, n.loc, n.cha = nslc


@pytest.fixture(autouse=True)
def clean_session() -> Iterator[None]:
    # Bridge state is module-global (it is one Pyodide session); every test
    # starts empty and scrubs what it loaded so none can colour the next.
    bridge._TRACES[:] = []
    bridge._SOURCE_TRACES.clear()
    bridge._SOURCE_DIGESTS.clear()
    bridge._SUPPRESSED_SOURCE_TRACES.clear()
    bridge._DIGEST_OWNERS.clear()
    bridge._DERIVED_TRACES.clear()
    bridge._DERIVED_EVENTS.clear()
    bridge._EVENT_REGISTRY.clear()
    bridge._ACTIVE_EVENT = None
    bridge._ACTIVE_HASH = ""
    bridge._ACTIVE_MT = None
    bridge._STATIONS.clear()
    bridge._WARNED_NO_RESPONSE.clear()
    bridge._VIEW_NOTICES.clear()
    bridge._PILE_IS_EXAMPLE = False
    yield
    bridge._TRACES[:] = []
    bridge._SOURCE_TRACES.clear()
    bridge._SOURCE_DIGESTS.clear()
    bridge._SUPPRESSED_SOURCE_TRACES.clear()
    bridge._DIGEST_OWNERS.clear()
    bridge._HIDDEN[:] = []
    bridge._DERIVED_TRACES.clear()
    bridge._DERIVED_EVENTS.clear()
    bridge._EVENT_REGISTRY.clear()
    bridge._ACTIVE_EVENT = None
    bridge._ACTIVE_HASH = ""
    bridge._ACTIVE_MT = None
    bridge._STATIONS.clear()
    bridge._WARNED_NO_RESPONSE.clear()
    bridge._VIEW_NOTICES.clear()
    bridge._PILE_IS_EXAMPLE = False
    bridge._GF_STORES.pop("broken", None)
    bridge._mark_traces_changed()


def test_initial_file_source_uses_append_rules_before_registering(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    boot = make_trace("BOOT", "DEMO", "BHZ", 0.0, 100, 1.0)
    real = make_trace("XX", "REAL", "BHZ", 0.0, 100, 2.0)
    reloaded = make_trace("XX", "REAL", "BHZ", 0.0, 100, 3.0)
    other = make_trace("YY", "OTHER", "BHZ", 0.0, 100, 4.0)
    final = make_trace("ZZ", "FINAL", "BHZ", 0.0, 100, 5.0)
    bridge._TRACES[:] = [boot]
    bridge._PILE_IS_EXAMPLE = True
    duplicate_other = make_trace("YY", "OTHER", "BHZ", 0.0, 100, 4.0)
    loads = iter([[real], [reloaded], [other], [duplicate_other], [final]])
    monkeypatch.setattr(bridge, "_stage_bytes", lambda name, content: name)
    monkeypatch.setattr(bridge, "_load_path_traces", lambda path, fmt: next(loads))
    servicer = bridge._BridgeServicer()

    initial = bridge.data_pb2.LoadBytesRequest(
        name="real.mseed",
        fmt="detect",
        append=True,
        content=b"first",
        source_id="src0",
    )
    result = servicer.load_bytes(initial, None)

    # The first real handle replaces the boot example and becomes reloadable.
    assert result.loaded
    assert bridge._TRACES == [real]
    assert bridge._SOURCE_TRACES["src0"] == [real]
    assert not bridge._PILE_IS_EXAMPLE

    duplicate = bridge.data_pb2.LoadBytesRequest(
        name="copy.mseed",
        fmt="detect",
        append=True,
        content=b"first",
        source_id="src1",
    )
    duplicate_result = servicer.load_bytes(duplicate, None)

    # Source identity no longer exempts an initial open from duplicate detection.
    assert not duplicate_result.loaded
    assert "src1" not in bridge._SOURCE_TRACES
    assert bridge._TRACES == [real]

    replacement = bridge.data_pb2.LoadBytesRequest(
        name="real.mseed",
        fmt="detect",
        append=True,
        content=b"changed",
        source_id="src0",
        replace_source=True,
    )
    replacement_result = servicer.load_bytes(replacement, None)

    # Only the explicit reload path swaps the registered source by identity.
    assert replacement_result.loaded
    assert bridge._TRACES == [reloaded]
    assert bridge._SOURCE_TRACES["src0"] == [reloaded]

    other_request = bridge.data_pb2.LoadBytesRequest(
        name="other.mseed",
        fmt="detect",
        append=True,
        content=b"other",
        source_id="src2",
    )
    assert servicer.load_bytes(other_request, None).loaded

    collision = bridge.data_pb2.LoadBytesRequest(
        name="real.mseed",
        fmt="detect",
        append=True,
        content=b"other",
        source_id="src0",
        replace_source=True,
    )
    collision_result = servicer.load_bytes(collision, None)

    # A reload converging on another source retires its old traces atomically.
    assert collision_result.loaded
    assert bridge._SOURCE_TRACES["src0"] == []
    assert bridge._TRACES == [other]

    final_request = bridge.data_pb2.LoadBytesRequest(
        name="real.mseed",
        fmt="detect",
        append=True,
        content=b"final",
        source_id="src0",
        replace_source=True,
    )
    assert servicer.load_bytes(final_request, None).loaded

    repeated_other = bridge.data_pb2.LoadBytesRequest(
        name="third.mseed",
        fmt="detect",
        append=True,
        content=b"other",
        source_id="src3",
    )
    repeated_result = servicer.load_bytes(repeated_other, None)

    # A leaving B's digest cannot retire B's ownership and admit a third copy.
    assert not repeated_result.loaded
    assert bridge._TRACES == [other, final]


def test_suppressed_source_is_promoted_when_the_digest_owner_leaves(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    a_x = make_trace("AA", "SOURCE", "BHZ", 0.0, 100, 1.0)
    b_y = make_trace("BB", "OWNER", "BHZ", 0.0, 100, 2.0)
    a_y = make_trace("AA", "SOURCE", "BHZ", 0.0, 100, 2.0)
    b_z = make_trace("BB", "OWNER", "BHZ", 0.0, 100, 3.0)
    loads = iter([[a_x], [b_y], [a_y], [b_z]])
    monkeypatch.setattr(bridge, "_stage_bytes", lambda name, content: name)
    monkeypatch.setattr(bridge, "_load_path_traces", lambda path, fmt: next(loads))
    servicer = bridge._BridgeServicer()

    def request(source_id: str, content: bytes, replace: bool = False):
        return bridge.data_pb2.LoadBytesRequest(
            name=source_id + ".mseed",
            fmt="detect",
            append=True,
            content=content,
            source_id=source_id,
            replace_source=replace,
        )

    assert servicer.load_bytes(request("a", b"x"), None).loaded
    assert servicer.load_bytes(request("b", b"y"), None).loaded
    assert servicer.load_bytes(request("a", b"y", True), None).loaded
    assert bridge._SOURCE_TRACES["a"] == []
    assert bridge._SUPPRESSED_SOURCE_TRACES["a"] == [a_y]

    assert servicer.load_bytes(request("b", b"z", True), None).loaded

    # No filesystem notification is due for unchanged A. Its parsed traces
    # therefore have to take ownership as part of B retiring the shared digest.
    assert bridge._SOURCE_TRACES["a"] == [a_y]
    assert "a" not in bridge._SUPPRESSED_SOURCE_TRACES
    assert bridge._TRACES == [b_z, a_y]
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
    bridge._TRACES[:] = [old]
    bridge._SOURCE_TRACES["old"] = [old]
    bridge._SOURCE_DIGESTS["old"] = "old-digest"
    bridge._DIGEST_OWNERS["old-digest"] = {"source:old"}
    bridge._STATIONS[("OLD", "KEEP")] = (1.0, 2.0, 0.0, 0.0)
    bridge._EVENT_REGISTRY["old"] = old_event
    bridge._STATIONXMLS.append(stationxml)
    bridge._activate_event("old")

    def archive(entries: List[Tuple[str, bytes]]) -> bytes:
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w") as bundle:
            for name, payload in entries:
                bundle.writestr(name, payload)
        return out.getvalue()

    monkeypatch.setattr(bridge, "_stage_bytes", lambda name, content: name)

    def load_path(path: str, fmt: str) -> List[ptrace.Trace]:
        if "invalid" in path:
            raise ValueError("invalid waveform")
        return [new]

    monkeypatch.setattr(bridge, "_load_path_traces", load_path)

    with pytest.raises(ValueError, match="invalid waveform"):
        bridge._load_bundle_result(
            archive([("a-valid.mseed", b"valid"), ("z-invalid.mseed", b"invalid")]),
            "broken-waveforms.zip",
        )

    def fail_metadata(texts: List[str]) -> Any:
        raise ValueError("invalid StationXML")

    monkeypatch.setattr(bridge, "_load_responses_result", fail_metadata)
    with pytest.raises(ValueError, match="invalid StationXML"):
        bridge._load_bundle_result(
            archive([("a-valid.mseed", b"valid"), ("metadata.xml", b"invalid")]),
            "broken-metadata.zip",
        )

    # Neither waveform parsing nor later metadata parsing may publish a partial
    # replacement of the live dataset.
    assert bridge._TRACES == [old]
    assert bridge._SOURCE_TRACES == {"old": [old]}
    assert bridge._SOURCE_DIGESTS == {"old": "old-digest"}
    assert bridge._DIGEST_OWNERS == {"old-digest": {"source:old"}}
    assert bridge._STATIONS[("OLD", "KEEP")] == (1.0, 2.0, 0.0, 0.0)
    assert bridge._EVENT_REGISTRY == {"old": old_event}
    assert bridge._ACTIVE_HASH == "old"
    assert bridge._STATIONXMLS == [stationxml]


def test_valid_live_reset_retires_the_boot_example_flag(tmp_path: Path) -> None:
    bridge._TRACES[:] = [make_trace("BOOT", "DEMO", "BHZ", 0.0, 100, 1.0)]
    bridge._PILE_IS_EXAMPLE = True
    live = make_trace("LV", "STRM", "BHZ", 0.0, 100, 1.0)
    path = tmp_path / "live.mseed"
    bridge.pio.save([live], str(path), format="mseed")

    bridge._append_mseed_scene("live", path.read_bytes(), reset=True)

    assert [trace.nslc_id for trace in bridge._TRACES] == [live.nslc_id]
    assert not bridge._PILE_IS_EXAMPLE


def test_missing_response_without_stationxml_emits_a_notice() -> None:
    tr = make_trace("XX", "STA1", "BHZ", 0.0, 100, 1.0)

    assert bridge._remove_response(tr, True) is tr
    assert bridge._VIEW_NOTICES == ["no instrument response information for XX.STA1..BHZ"]


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
    bridge._EVENT_REGISTRY["event-a"] = event
    bridge._ACTIVE_EVENT = (1.0, 2.0, 3000.0, 10.0)
    bridge._STATIONS[("XX", "NEAR")] = (3.0, 4.0, 0.0, 0.0)
    bridge._STATIONS[("XX", "FAR")] = (5.0, 6.0, 0.0, 0.0)
    bridge._TRACES[:] = [
        make_trace("XX", "NEAR", "BHZ", 0.0, 100, 1.0),
        make_trace("XX", "FAR", "BHZ", 7200.0, 100, 1.0),
    ]

    result = bridge._map_data_result(
        bridge.data_pb2.MapRequest(
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
    bridge._STATIONS[("XX", "STA1", "")] = (12.5, 45.25, 0.0, 0.0)
    request = bridge.data_pb2.SetActiveEventRequest(
        event_hash="local:event",
        t=123.0,
        nslc=bridge.common_pb2.Nslc(net="XX", sta="STA1", loc="", cha="BHZ"),
    )

    created = bridge._set_active_event(request)

    assert created.active_hash == "local:event"
    snapshot = created.events[0]
    assert (snapshot.time, snapshot.lat, snapshot.lon, snapshot.depth) == (
        123.0,
        12.5,
        45.25,
        0.0,
    )

    marker_file = bridge.data_pb2.MarkerFile()
    marker = marker_file.markers.add()
    marker.tmin = 123.0
    marker.tmax = 123.0
    marker.is_event = True
    marker.event_hash = "local:event"
    exported = bridge._markers_to_text(marker_file)
    round_tripped = bridge._markers_from_text(exported)

    # Export resolves the registry snapshot instead of rebuilding missing Rust
    # fields as a zero-coordinate event.
    assert round_tripped.markers[0].lat == 12.5
    assert round_tripped.markers[0].lon == 45.25

    snapshot.lat = 13.0
    snapshot.depth = 4200.0
    snapshot.name = "edited"
    updated = bridge._update_event(snapshot)

    canonical = updated.events[0]
    assert (canonical.lat, canonical.depth, canonical.name) == (13.0, 4200.0, "edited")
    assert bridge._ACTIVE_EVENT == (13.0, 45.25, 4200.0, 123.0)


def test_removing_events_retires_the_active_registry_entry() -> None:
    from pyrocko import model

    removed = model.Event(time=10.0, lat=1.0, lon=2.0, name="removed")
    retained = model.Event(time=20.0, lat=3.0, lon=4.0, name="retained")
    bridge._EVENT_REGISTRY["removed"] = removed
    bridge._EVENT_REGISTRY["retained"] = retained
    bridge._activate_event("removed")

    result = bridge._remove_events(
        bridge.data_pb2.RemoveEventsRequest(event_hashes=["removed", "removed"])
    )

    assert not result.has_event
    assert bridge._ACTIVE_HASH == ""
    assert list(bridge._EVENT_REGISTRY) == ["retained"]
    goto = bridge._goto_result(["removed"], 0.0, 30.0)
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
    bridge._EVENT_REGISTRY["old"] = old
    bridge._activate_event("old")
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

    text_result = bridge._parse_events_result(text_event)

    assert bridge._ACTIVE_HASH == "new"
    assert bridge._ACTIVE_MT is None
    old_metric = next(
        metric for metric in text_result.event_state.kagan if metric.event_hash == "old"
    )
    assert not old_metric.HasField("kagan_deg")

    bridge._activate_event("old")
    local = model.Event(time=20.0, lat=30.0, lon=40.0, magnitude=3.0, name="local")
    monkeypatch.setattr(model, "load_events", lambda path: [local])

    local_result = bridge._load_events_result("event data")

    assert bridge._ACTIVE_HASH == "local"
    assert bridge._ACTIVE_MT is None
    old_metric = next(
        metric for metric in local_result.event_state.kagan if metric.event_hash == "old"
    )
    assert not old_metric.HasField("kagan_deg")

    bridge._activate_event("old")
    bridge._marker_module()
    from pyrocko.io import quakeml

    empty = types.SimpleNamespace(get_pyrocko_events=lambda: [])
    monkeypatch.setattr(quakeml.QuakeML, "load_xml", staticmethod(lambda **kwargs: empty))

    empty_result = bridge._parse_events_quakeml("<quakeml />")

    assert bridge._ACTIVE_EVENT is None
    assert bridge._ACTIVE_HASH == ""
    assert bridge._ACTIVE_MT is None
    assert not empty_result.event_state.has_event


def test_minmax_folds_file_split_fragments_into_one_row() -> None:
    # One channel split across two contiguous files with different amplitudes,
    # plus a second channel.
    load(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 1000, 1.0),
            make_trace("XX", "STA1", "BHZ", 10.0, 1000, 5.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 2.0),
        ]
    )
    res = asyncio.run(bridge._run_snuffling_result("minmax", base_req()))
    rows = [ln for ln in res.text.splitlines()[1:] if ln.strip()]
    # Qt prints one aggregate per NSLC (`trace.minmax`, minmax.py:49) — never
    # one row of local extrema per file fragment.
    assert len(rows) == 2
    (bhz_row,) = [ln for ln in rows if "BHZ" in ln]
    fields = bhz_row.split()
    # The extrema span both fragments: the quieter first file must not mask
    # the ±5 swing of the second.
    assert abs(float(fields[1]) + 5.0) < 0.1
    assert abs(float(fields[2]) - 5.0) < 0.1


def test_minmax_honours_the_markers_channel_scope() -> None:
    load(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 2.0),
        ]
    )
    req = base_req()
    select_span(req, 0.0, 20.0, ("XX", "STA1", "*", "BHN"))
    res = asyncio.run(bridge._run_snuffling_result("minmax", req))
    rows = [ln for ln in res.text.splitlines()[1:] if ln.strip()]
    # A channel-scoped selected marker admits only its own channel.
    assert len(rows) == 1
    assert "BHN" in rows[0]


def test_the_chopper_adapter_degaps_and_orders() -> None:
    load(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 1000, 1.0),
            make_trace("XX", "STA1", "BHZ", 10.0, 1000, 5.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 2.0),
        ]
    )
    segs = [tr for batch in bridge._chopper_selected_traces(base_req()) for tr in batch.traces]
    # Default kwargs degap: the contiguous file-split channel arrives as one
    # joined trace, in the pile chopper's `full_id` order (pile.py:1213).
    assert len([tr for tr in segs if tr.channel == "BHZ"]) == 1
    assert segs == sorted(segs, key=lambda tr: tr.full_id)


def test_stalta_and_rms_cross_a_contiguous_file_boundary() -> None:
    fragments = [
        make_trace("XX", "STA1", "BHZ", -1.0, 200, 1.0),
        make_trace("XX", "STA1", "BHZ", 1.0, 200, 2.0),
    ]
    load(fragments)
    request = base_req()
    request.view.tmin = 0.0
    request.view.tmax = 2.0
    request.snufflings.stalta.stalta_swin = 0.1
    request.snufflings.stalta.stalta_ratio = 2.0
    request.snufflings.stalta.stalta_level = 0.5
    request.snufflings.stalta.stalta_show_level_traces = True

    asyncio.run(bridge._run_snuffling_result("stalta", request))

    stalta = bridge._DERIVED_TRACES["stalta"]
    assert len([trace for trace in stalta if trace.station == "STA1"]) == 1

    request.snufflings.rms.rms_block = 2.0
    asyncio.run(bridge._rms_scene(request))

    rms = bridge._DERIVED_TRACES["rms"]
    assert len(rms) == 1
    assert rms[0].data_len() == 1


def test_spectrogram_inventory_merges_split_fragments() -> None:
    load(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 1000, 1.0),
            make_trace("XX", "STA1", "BHZ", 10.0, 1000, 2.0),
        ]
    )

    _tmin, _tmax, panels = bridge._spectrogram_inventory(base_req(), 5.0, 1.0)

    assert panels == [(("XX", "STA1", "", "BHZ"), 0.01)]


def test_failed_stalta_and_rms_calls_preserve_the_committed_scene(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    load([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    stale_stalta = make_trace("DER", "STA", "BHZ", 0.0, 100, 1.0)
    stale_rms = make_trace("DER", "RMS", "BHZ", 0.0, 100, 1.0)
    bridge._replace_derived("stalta", [stale_stalta])
    bridge._replace_derived("rms", [stale_rms])

    def fail_chopper(*args: Any, **kwargs: Any) -> Iterator[Any]:
        yield from ()
        raise RuntimeError("injected chopper failure")

    monkeypatch.setattr(bridge, "_chopper_selected_traces", fail_chopper)

    def trace_bytes() -> List[Tuple[Tuple[str, str, str, str], bytes]]:
        return [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES]

    before = trace_bytes()
    stalta_request = bridge.snuffling_pb2.NamedSnufflingRequest(name="stalta", params=base_req())
    stalta_reply = asyncio.run(
        bridge.call(
            "/snuffler.bridge.v1.BridgeService/RunSnuffling",
            stalta_request.SerializeToString(),
        )
    )
    stalta_result = bridge.bridge_pb2.BridgeResult.FromString(stalta_reply["result"])

    assert stalta_result.WhichOneof("result") == "error"
    assert "injected chopper failure" in stalta_result.error.message
    assert trace_bytes() == before

    rms_reply = asyncio.run(
        bridge.call(
            "/snuffler.bridge.v1.BridgeService/RmsTraces",
            base_req().SerializeToString(),
        )
    )
    rms_result = bridge.bridge_pb2.BridgeResult.FromString(rms_reply["result"])

    assert rms_result.WhichOneof("result") == "error"
    assert "injected chopper failure" in rms_result.error.message
    assert trace_bytes() == before
    assert len(bridge._DERIVED_TRACES["stalta"]) == 1
    assert len(bridge._DERIVED_TRACES["rms"]) == 1


@pytest.mark.parametrize(
    "failure",
    [SystemExit("stop"), GeneratorExit("close"), BaseException("base")],
    ids=["system-exit", "generator-exit", "base-exception"],
)
def test_outer_boundary_serializes_every_base_exception(
    monkeypatch: pytest.MonkeyPatch, failure: BaseException
) -> None:
    load([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    before = [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES]

    async def fail_dispatch(*args: Any, **kwargs: Any) -> Dict[str, Any]:
        raise failure

    monkeypatch.setattr(bridge.bridge_dispatch, "dispatch", fail_dispatch)
    reply = asyncio.run(bridge.call("/snuffler.bridge.v1.BridgeService/EmptyScene"))
    result = bridge.bridge_pb2.BridgeResult.FromString(reply["result"])

    assert result.WhichOneof("result") == "error"
    assert result.error.kind == type(failure).__name__
    assert result.error.message == str(failure)
    assert [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES] == before


def test_cancellation_gate_wins_before_commit_and_preserves_the_scene() -> None:
    load([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    before = [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES]

    reply = asyncio.run(
        bridge.call(
            "/snuffler.bridge.v1.BridgeService/EmptyScene",
            begin_commit=lambda: False,
        )
    )
    result = bridge.bridge_pb2.BridgeResult.FromString(reply["result"])

    assert result.WhichOneof("result") == "cancelled"
    assert [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES] == before


def test_malformed_seedlink_reset_preserves_the_committed_scene() -> None:
    load([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    before = [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES]
    request = bridge.data_pb2.IngestMiniSeedRequest(
        source="test",
        content=b"not miniSEED",
        reset=True,
        stream_id=7,
        session_epoch=3,
        batch_seq_first=0,
        batch_seq_last=0,
    )

    reply = asyncio.run(
        bridge.call(
            "/snuffler.bridge.v1.BridgeService/IngestMiniSeed",
            request.SerializeToString(),
        )
    )
    result = bridge.bridge_pb2.BridgeResult.FromString(reply["result"])

    assert result.WhichOneof("result") == "error"
    assert [(trace.nslc_id, trace.ydata.tobytes()) for trace in bridge._TRACES] == before


def test_marker_budget_truncates_before_serialization_and_interns_nslcs() -> None:
    result = bridge.snuffling_pb2.SnufflingResult()
    result.nslc_table.add(net="XX", sta="STA", loc="", cha="BHZ")
    for index, strength in enumerate((0.1, 0.9, 0.2, 0.8, 0.3)):
        marker = result.markers.add()
        marker.t = float(index)
        marker.incidence_angle = strength
        marker.nslc_indexes.append(0)

    bridge._truncate_marker_result(result, bridge.snuffling_pb2.MarkerBudget(max_markers=2))

    assert len(result.markers) == 2
    assert [marker.t for marker in result.markers] == [1.0, 3.0]
    assert result.markers_dropped == 3
    assert result.marker_budget == 2
    assert len(result.nslc_table) == 1
    assert all(list(marker.nslc_indexes) == [0] for marker in result.markers)


def test_marker_accumulator_bounds_candidates_and_compacts_unique_nslcs() -> None:
    accumulator = bridge._MarkerAccumulator(bridge.snuffling_pb2.MarkerBudget(max_markers=3))
    for index in range(1000):
        marker = bridge.snuffling_pb2.SnufMarker(t=float(index))
        accumulator.add(
            marker,
            [("XX", f"S{index:04d}", "", "BHZ")],
            strength=float(index),
        )

    assert len(accumulator._heap) == 3
    result = accumulator.finish(bridge.snuffling_pb2.SnufflingResult())
    assert len(result.markers) == 3
    assert [marker.t for marker in result.markers] == [997.0, 998.0, 999.0]
    assert len(result.nslc_table) == 3
    assert result.markers_dropped == 997


def test_marker_accumulator_retains_event_and_active_markers_under_pressure() -> None:
    # Event markers carry no incidence angle and score 0.0; without a retention
    # tier, an over-budget file of strong phase picks would displace every
    # event, leaving the picks' event_hash references dangling.
    accumulator = bridge._MarkerAccumulator(bridge.snuffling_pb2.MarkerBudget(max_markers=4))
    for index in range(100):
        pick = bridge.snuffling_pb2.SnufMarker(t=float(index))
        pick.incidence_angle = 50.0 + float(index)
        accumulator.add(pick)
    event = bridge.snuffling_pb2.SnufMarker(t=1000.0, is_event=True)
    accumulator.add(event)
    active = bridge.snuffling_pb2.SnufMarker(t=2000.0, is_event=True, active=True)
    accumulator.add(active)

    result = accumulator.finish(bridge.snuffling_pb2.SnufflingResult())
    assert len(result.markers) == 4
    retained_events = [marker for marker in result.markers if marker.is_event]
    assert {marker.t for marker in retained_events} == {1000.0, 2000.0}
    assert any(marker.active for marker in retained_events)


def test_workspace_clone_preserves_event_identity_across_the_event_maps() -> None:
    # _replace_derived_events retires registry entries by object identity, and
    # _ACTIVE_MT aliases the registered event's moment tensor. The clone must
    # keep those aliases (one shared deepcopy memo); independent copies would
    # leak superseded derived events in the registry forever.
    from pyrocko import model, moment_tensor

    event = model.Event(time=1.0, lat=0.0, lon=0.0, depth=0.0)
    event.moment_tensor = moment_tensor.MomentTensor(strike=30.0, dip=60.0, rake=90.0)
    state = bridge._empty_dataset_state([])
    state = state._replace(
        derived_events={"seismosizer": {"h1": event}},
        event_registry={"h1": event},
        active_mt=event.moment_tensor,
        active_hash="h1",
    )

    clone = bridge._clone_dataset_state(state)

    assert clone.derived_events["seismosizer"]["h1"] is clone.event_registry["h1"]
    assert clone.active_mt is clone.event_registry["h1"].moment_tensor
    assert clone.event_registry["h1"] is not event


def test_event_projection_is_bounded_and_retains_the_active_event() -> None:
    from pyrocko import model

    for index in range(5):
        bridge._register_event(
            f"event-{index}",
            model.Event(time=float(index), lat=float(index), lon=0.0, depth=0.0),
        )
    bridge._activate_event("event-0")

    state = bridge._event_state(2)

    assert len(state.events) == 2
    assert {event.event_hash for event in state.events} == {"event-0", "event-4"}
    assert len(state.kagan) == 2


def test_interrupted_view_invalidation_clears_every_view_owned_cache() -> None:
    bridge._PILE = object()
    bridge._PILE_GEN = 9
    bridge._CUTOUT_VEC = np.array([1.0])
    bridge._CUTOUT_TRACES = [make_trace("XX", "STA", "BHZ", 0.0, 10, 1.0)]
    bridge._CUTOUT_NOTICES[:] = ["cutout"]
    bridge._VIEW_NOTICES[:] = ["view"]
    bridge._OLD_DATA_RANGES["track"] = (1.0, 2.0)

    bridge._invalidate_interrupted_view()

    assert bridge._PILE is None
    assert bridge._PILE_GEN is None
    assert bridge._CUTOUT_VEC is None
    assert bridge._CUTOUT_TRACES is None
    assert bridge._CUTOUT_NOTICES == []
    assert bridge._VIEW_NOTICES == []
    assert bridge._OLD_DATA_RANGES == {}


def test_polarization_analyses_a_hidden_component() -> None:
    load(
        [
            make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0),
            make_trace("XX", "STA1", "BHN", 0.0, 2000, 1.5),
            make_trace("XX", "STA1", "BHE", 0.0, 2000, 2.0),
        ]
    )
    bridge._HIDDEN.append("*.*.*.BHE")
    bridge._mark_traces_changed()
    req = base_req()
    select_span(req, 5.0, 6.0, ("XX", "STA1", "", "BHZ"))
    res = asyncio.run(bridge._run_snuffling_result("polarization", req))
    # Qt choppers the raw pile with only its channel pattern
    # (polarization.py:300): hiding one component must not break the station's
    # three-component analysis.
    assert any("STA1" in ln for ln in res.text.splitlines()[1:])


def test_polarization_requires_the_padded_window() -> None:
    # The analysis window is [5, 6] (t_length 1, offset 0); Qt demands data
    # across the window plus one window length of display margin — [4, 7].
    def run(tmin: float, n: int) -> bool:
        load(
            [
                make_trace("XX", "STA1", cha, tmin, n, amp)
                for cha, amp in (("BHZ", 1.0), ("BHN", 1.5), ("BHE", 2.0))
            ]
        )
        req = base_req()
        select_span(req, 5.0, 6.0, ("XX", "STA1", "", "BHZ"))
        res = asyncio.run(bridge._run_snuffling_result("polarization", req))
        return any("STA1" in ln for ln in res.text.splitlines()[1:])

    # Data covering only [4.5, 6.5] misses the margin: no result — where an
    # unpadded chop would happily produce one.
    assert not run(4.5, 200)
    # Covering [3.9, 7.1] satisfies it.
    assert run(3.9, 320)


def test_seisbench_grid_keeps_every_fragment() -> None:
    # A channel split across two contiguous files must land in the model input
    # whole — seisbench's stream_to_array writes each trace at its offset, and
    # keeping only the last fragment once amputated the first file silently.
    spec = {"sampling_rate": 100.0, "component_order": "ZNE"}
    frags = [
        make_trace("XX", "STA1", "BHZ", 0.0, 300, 1.0),
        make_trace("XX", "STA1", "BHZ", 3.0, 300, 5.0),
    ]
    groups = bridge._seisbench_groups(frags, 0.0, 6.0, spec)
    ((g_tmin, data, nslc),) = groups.values()
    assert g_tmin == 0.0
    assert nslc == ("XX", "STA1", "", "BHZ")
    z = data[0]
    # Both halves carry signal: the first fragment was not overwritten by the
    # second.
    assert float(np.abs(z[:290]).max()) > 0.0
    assert float(np.abs(z[310:]).max()) > 0.0


def test_seisbench_rescaling_changes_the_model_time_grid() -> None:
    spec = {"sampling_rate": 100.0, "component_order": "ZNE"}
    trace = make_trace("XX", "STA1", "BHZ", 0.0, 300, 1.0)

    native = bridge._seisbench_groups([trace], 0.0, 3.0, spec)
    stretched = bridge._seisbench_groups([trace], 0.0, 3.0, spec, 2.0)

    native_samples = next(iter(native.values()))[1].shape[1]
    stretched_samples = next(iter(stretched.values()))[1].shape[1]
    assert stretched_samples > native_samples


def test_stalta_cleanup_preserves_user_cg_locations() -> None:
    user = make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)
    user.location = "cg"
    stale = make_trace("DER", "STA", "BHZ", 0.0, 2000, 1.0)
    stale.meta = {"tabu": True}
    load([user])
    bridge._replace_derived("stalta", [stale])
    req = base_req()
    req.snufflings.stalta.stalta_swin = 0.2
    req.snufflings.stalta.stalta_ratio = 3.0
    req.snufflings.stalta.stalta_level = 0.5

    asyncio.run(bridge._run_snuffling_result("stalta", req))

    assert user in bridge._TRACES
    assert stale not in bridge._TRACES


def test_rms_cleanup_preserves_user_rms_channel_names() -> None:
    user = make_trace("XX", "STA1", "BHZ-RMS", 0.0, 2000, 1.0)
    load([user])
    req = base_req()
    req.snufflings.rms.rms_block = 1.0

    asyncio.run(bridge._rms_scene(req))

    assert user in bridge._TRACES
    assert user not in bridge._DERIVED_TRACES["rms"]


def test_seisbench_streams_each_batch_through_inference(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # 700 s of three-component data: three 300-second chopper windows.
    load(
        [
            make_trace("XX", "STA1", cha, 0.0, 70_000, amp)
            for cha, amp in (("BHZ", 1.0), ("BHN", 1.5), ("BHE", 2.0))
        ]
    )
    events: List[str] = []
    chopper_pads: List[float] = []
    filter_calls: List[Tuple[float | None, float | None, float]] = []

    real_chopper = bridge._chopper_selected_traces

    def logging_chopper(*args: Any, **kwargs: Any) -> Iterator[Any]:
        chopper_pads.append(float(kwargs.get("tpad", 0.0)))
        for batch in real_chopper(*args, **kwargs):
            events.append("chop")
            yield batch

    monkeypatch.setattr(bridge, "_chopper_selected_traces", logging_chopper)

    spec: Dict[str, Any] = {"sampling_rate": 100.0, "component_order": "ZNE"}

    async def fake_ensure(model: str, weights: str, base_url: str) -> Tuple[str, Dict[str, Any]]:
        return "fake://model", spec

    monkeypatch.setattr(bridge, "_ensure_seisbench_model", fake_ensure)
    monkeypatch.setattr(
        bridge,
        "_apply_seisbench_view_filter",
        lambda tr, highpass, lowpass, tcut: filter_calls.append((highpass, lowpass, tcut)) or tr,
    )

    # A stand-in inference module: no ONNX runtime, but the same surface the
    # picker consumes — enough to observe when annotation happens.
    fake = types.ModuleType("seisbench_infer")

    async def annotate(
        url: str, data: np.ndarray, sp: Dict[str, Any], batch_done: Any, prefix: str
    ) -> Any:
        events.append("annotate")
        batch_done()
        return types.SimpleNamespace(
            data=np.zeros((data.shape[1], 3), dtype=np.float32),
            start_offset=0.0,
            sampling_rate=100.0,
        )

    fake.annotate = annotate  # type: ignore[attr-defined]
    fake.batch_count = lambda data, sp: 1  # type: ignore[attr-defined]
    fake.labels = lambda sp: ("P", "S")  # type: ignore[attr-defined]
    fake.phase_channel = lambda sp, phase: 1 if phase == "P" else 2  # type: ignore[attr-defined]
    fake.extract_picks = lambda arr, threshold: []  # type: ignore[attr-defined]
    fake.default_threshold = lambda sp, phase: 0.3  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "seisbench_infer", fake)

    req = base_req()
    req.view.tmax = 700.0
    req.view.highpass = 2.0
    req.snufflings.seisbench.use_predefined_filters = True
    res = asyncio.run(bridge._seisbench_pick_result(req))

    # Inference runs inside the chopper loop: the first window annotates
    # before the second is even chopped, so batches stream and release
    # instead of every station grid accumulating in memory first.
    assert events[:2] == ["chop", "annotate"], events
    assert events.count("chop") == 3, events
    assert chopper_pads == [2.0]
    assert filter_calls
    assert all(call == (2.0, None, 1.0) for call in filter_calls)
    assert res.origin == "seisbench"
    assert "1 station(s)" in res.text


def test_snuffling_preflight_failures_keep_their_cleanup_origin() -> None:
    request = bridge.snuffling_pb2.NamedSnufflingRequest(name="cake")
    request.params.snufflings.cake.cake_model = bridge.GF_STORE_PREFIX + "missing"

    result = asyncio.run(bridge._BridgeServicer().run_snuffling(request, None))

    # Origin is assigned before Cake's active-event preflight can refuse the run.
    assert result.origin == "cake"
    assert "No active event" in result.text


def test_a_failed_seismosizer_run_leaves_the_cleaned_state() -> None:
    from pyrocko import model

    load([make_trace("XX", "STA1", "BHZ", 0.0, 2000, 1.0)])
    # A previous successful run left synthetics in the pile.
    stale = make_trace("SY", "SYN1", "Z", 0.0, 500, 1.0)
    bridge._replace_derived("seismosizer", [stale])
    stale_event = model.Event(time=10.0, lat=1.0, lon=2.0, name="stale synthetic")
    bridge._replace_derived_events("seismosizer", {"stale": stale_event})
    bridge._activate_event("stale")
    assert stale in bridge._TRACES

    bridge._GF_STORES["broken"] = "/nonexistent-store-dir"
    req = base_req()
    req.snufflings.seismosizer.seis_store = bridge.GF_STORE_PREFIX + "broken"
    res = asyncio.run(bridge._run_snuffling_result("seismosizer", req))

    # Qt's `call()` cleans up before any validation (seismosizer.py:97), and
    # every failure — this one dies opening the store — must ship the cleaned
    # scene with the origin tag that retires the stale event marker.
    assert res.origin == "seismosizer"
    assert res.HasField("scene")
    assert res.text.strip()
    assert stale not in bridge._TRACES
    assert bridge._DERIVED_TRACES.get("seismosizer", []) == []
    assert "stale" not in bridge._EVENT_REGISTRY
    assert bridge._DERIVED_EVENTS.get("seismosizer", {}) == {}
    assert bridge._ACTIVE_HASH == ""
    assert res.HasField("event_state")
    assert not res.event_state.has_event


def test_seismosizer_cleanup_preserves_a_newer_event_with_the_same_hash() -> None:
    from pyrocko import model

    synthetic = model.Event(time=10.0, lat=1.0, lon=2.0, name="synthetic")
    replacement = model.Event(time=20.0, lat=3.0, lon=4.0, name="catalog replacement")
    bridge._replace_derived_events("seismosizer", {"shared": synthetic})
    bridge._EVENT_REGISTRY["shared"] = replacement
    bridge._activate_event("shared")

    bridge._replace_derived_events("seismosizer", {})

    # Cleanup owns the object it inserted, not every future use of its hash.
    assert bridge._EVENT_REGISTRY["shared"] is replacement
    assert bridge._ACTIVE_HASH == "shared"
    assert bridge._ACTIVE_EVENT == (3.0, 4.0, 0.0, 20.0)


def test_reload_snapshot_carries_the_session_store() -> None:
    # Shift+R must not reset the session generation Rust fences view replies
    # with: a fresh store at generation zero would leave every later view
    # rejected as stale.
    store = bridge._SESSION_STORE
    saved = bridge._reload_snapshot()
    try:
        assert saved.get("_SESSION_STORE") is store
    finally:
        bridge._reload_restore(saved)
    assert bridge._SESSION_STORE is store


def test_view_projection_is_shallow_and_preserves_workspace_identity() -> None:
    # An interleaved view projects committed state without deep-copying the
    # workspace, and reinstalls the same trace objects afterwards — a handler
    # suspended at a safe point may still hold references into the workspace,
    # and replacing them with clones would silently drop its later mutations
    # from the committed update.
    load([make_trace("XX", "AAA", "BHZ", 0.0, 512, 1.0)])
    snapshot = bridge._SESSION_STORE.snapshot()
    bridge._SESSION_STORE.begin_transaction(snapshot)
    try:
        held = bridge._TRACES[0]
        committed = snapshot.state.traces[0]
        assert held is not committed

        bridge._SESSION_STORE.enter_view()
        assert bridge._TRACES[0] is committed
        bridge._SESSION_STORE.exit_view()
        assert bridge._TRACES[0] is held

        held.set_ydata(held.get_ydata() * 2.0)
        update = bridge._SESSION_STORE.capture_update()
        bridge._SESSION_STORE.restore_snapshot(snapshot)
        outcome = bridge._SESSION_STORE.commit(snapshot.generation, update)
        assert outcome.committed
        assert bridge._TRACES[0] is held
    finally:
        if bridge._SESSION_STORE._transaction_snapshot is not None:
            bridge._SESSION_STORE.rollback(snapshot)
        load([])
