"""The rpc entry point: every `BridgeService` rpc reaches pyrocko through `call`.

The worker (`web/bridge_worker.ts`) calls `call`, which dispatches through the
generated typed servicer and returns a protobuf envelope, never an exception.
pyrocko itself is unmodified; only its public API is called.
"""

from __future__ import annotations

from typing import Any, Dict, Optional, Tuple, cast

import numpy as np
from snuffler_web_proto import (
    bridge_dispatch,
    bridge_pb2,
    common_pb2,
    data_pb2,
    figure_pb2,
    scene_pb2,
    snuffling_pb2,
    view_pb2,
)

from . import (
    bundles,
    cake,
    commands,
    events,
    fdsn,
    fetch,
    figures,
    gf_stores,
    maps,
    marker_budget,
    marker_files,
    operation,
    rms,
    seisbench,
    snufflings,
    staging,
    state,
    station_places,
    stations,
    view,
    waveforms,
)


def _raw_bytes(value: Any) -> bytes:
    """The request payload as bytes, whether the worker passed bytes or a JS proxy."""
    if value is None:
        return b""
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value)
    if hasattr(value, "to_py"):
        return bytes(value.to_py())
    return bytes(value)


class BridgeServicer:
    """Implements the generated `BridgeServicer`, one typed method per rpc.

    The generated `dispatch()` parses the request, packs the BridgeResult oneof
    and converts any exception to a BridgeError.
    mypy checks these signatures structurally against the generated Protocol."""

    def make_example(self, request: bridge_pb2.Empty, data: Any) -> scene_pb2.SceneMeta:
        return waveforms.make_example_scene()

    def empty_scene(self, request: bridge_pb2.Empty, data: Any) -> scene_pb2.SceneMeta:
        # The replace path inside `state.set_traces` clears the scene's events,
        # stations and responses with the waveforms.
        state.set_traces([])
        return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)

    def load_bytes(self, request: data_pb2.LoadBytesRequest, data: Any) -> data_pb2.LoadBytesResult:
        return waveforms.load_file(request)

    async def load_url(self, request: data_pb2.LoadUrlRequest, data: Any) -> scene_pb2.SceneMeta:
        name = request.url.rsplit("/", 1)[-1] or "data"
        raw = await fetch.fetch_bytes(fetch.asset_url(request.url))
        return waveforms.load_path_scene(staging.stage_bytes(name, raw), request.format)

    async def list_examples(self, request: bridge_pb2.Empty, data: Any) -> data_pb2.ExampleList:
        return await bundles.example_list()

    async def load_bundle(
        self, request: data_pb2.LoadBundleRequest, data: Any
    ) -> data_pb2.BundleResult:
        return await bundles.load_requested(request)

    async def get_view(
        self, request: view_pb2.ViewParams, data: Any
    ) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
        await station_places.prepare()
        return view.get_view_data(request)

    async def render_figure(
        self, request: figure_pb2.FigureRequest, data: Any
    ) -> figure_pb2.FigureResult:
        operation.report_progress(0, 3, "Preparing figure")
        # Cake figures need the earth models staged, as the cake 'Run' does.
        if request.kind in ("cake_model", "cake_rays"):
            model = request.params.snufflings.cake.model or ""
            if not model.startswith(gf_stores.LABEL_PREFIX):
                await cake.stage_earth_models()
        operation.report_progress(1, 3, "Rendering figure")
        result = figures.render(request)
        operation.report_progress(3, 3, "Encoding figure")
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
            model = request.params.snufflings.cake.model or ""
            if not model.startswith(gf_stores.LABEL_PREFIX):
                await cake.stage_earth_models()
        if request.name == "seisbench_picker":
            result = await seisbench.pick(request.params)
        else:
            result = await snufflings.run(request.name, request.params)
        if origin and not result.origin:
            result.origin = origin
        return marker_budget.truncate_marker_result(result, request.params.marker_budget)

    async def rms_traces(
        self, request: snuffling_pb2.SnufflingRequestParams, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return snuffling_pb2.SnufflingResult(
            show_window=False,
            origin="rms",
            scene=await rms.run(request),
        )

    def run_command(self, request: data_pb2.CommandRequest, data: Any) -> data_pb2.CommandResult:
        return commands.run(request)

    async def map_data(self, request: data_pb2.MapRequest, data: Any) -> data_pb2.MapData:
        return maps.map_data(request, await maps.plate_boundaries())

    def set_active_event(
        self, request: data_pb2.SetActiveEventRequest, data: Any
    ) -> common_pb2.ActiveEventResult:
        return events.set_active_event(request)

    def update_event(
        self, request: common_pb2.EventSnapshot, data: Any
    ) -> common_pb2.ActiveEventResult:
        return events.update_event(request)

    def remove_events(
        self, request: data_pb2.RemoveEventsRequest, data: Any
    ) -> common_pb2.ActiveEventResult:
        return events.remove_events(request)

    async def load_gf_store_url(
        self, request: data_pb2.StringRequest, data: Any
    ) -> bridge_pb2.StringValue:
        return await gf_stores.load_from_url(request.value)

    def markers_to_text(self, request: data_pb2.MarkerFile, data: Any) -> bridge_pb2.StringValue:
        return bridge_pb2.StringValue(value=marker_files.markers_to_text(request))

    def load_markers(
        self, request: data_pb2.LoadMarkersRequest, data: Any
    ) -> data_pb2.SnufMarkerList:
        return marker_files.markers_from_text(request.text, request.marker_budget)

    def load_station_xml(
        self, request: data_pb2.LoadStationXmlRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return stations.load_station_xml(list(request.texts))

    def load_stations(
        self, request: data_pb2.LoadStationsRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return stations.load_station_file(request.text)

    def load_events(
        self, request: data_pb2.LoadEventsRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return events.load_event_file(request.text, request.marker_budget)

    def add_cake_model(
        self, request: data_pb2.AddCakeModelRequest, data: Any
    ) -> bridge_pb2.StringValue:
        return bridge_pb2.StringValue(value=cake.add_model(request.name, bytes(request.content)))

    async def download_waveforms(
        self, request: data_pb2.WaveformDownloadRequest, data: Any
    ) -> scene_pb2.SceneMeta:
        operation.report_progress(0, None, "Downloading waveforms")
        response = await fdsn.fetch(fdsn.dataselect_url(request))
        if response.status == 204:
            return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
        if not response.ok:
            raise RuntimeError("dataselect: HTTP %s" % response.status)
        raw = bytes(await operation.await_external(response.bytes()))
        if not raw:
            return scene_pb2.SceneMeta(tmin=0.0, tmax=1.0)
        operation.report_progress(len(raw), len(raw), "Parsing waveforms")
        return waveforms.append_downloaded_scene(staging.stage_bytes("fdsn.mseed", raw))

    def ingest_mini_seed(
        self, request: data_pb2.IngestMiniSeedRequest, data: Any
    ) -> data_pb2.IngestMiniSeedResult:
        if request.batch_seq_last < request.batch_seq_first:
            raise ValueError("SeedLink batch sequence range is reversed")
        if request.reset and request.batch_seq_first != 0:
            raise ValueError("SeedLink reset must be batch zero")
        return data_pb2.IngestMiniSeedResult(
            scene=waveforms.append_live_scene(
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
        text = await fdsn.fetch_text_or_empty(fdsn.event_url(request))
        if request.with_moment_tensors and text.lstrip().startswith("<"):
            return events.parse_quakeml(text, request.marker_budget)
        return events.parse_fdsn_text(text, request.marker_budget)

    def export_waveforms(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.FileBlob:
        return waveforms.export_mseed()

    def export_stations(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.FileBlob:
        return stations.export_station_file()

    async def fetch_stations(
        self, request: data_pb2.StationRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return stations.parse_fdsn_text(await fdsn.fetch_text_or_empty(fdsn.station_url(request)))

    async def fetch_responses(
        self, request: data_pb2.StationRequest, data: Any
    ) -> snuffling_pb2.SnufflingResult:
        return stations.load_station_xml(
            [await fdsn.fetch_text_or_empty(fdsn.response_url(request))]
        )

    def prepare_scientific_tools(
        self, request: bridge_pb2.Empty, data: Any
    ) -> bridge_pb2.StringValue:
        import bootstrap

        warmed = bootstrap.warm()
        return bridge_pb2.StringValue(value=",".join(warmed))

    def mount_gf_store(self, request: bridge_pb2.Empty, data: Any) -> bridge_pb2.StringValue:
        if not data:
            raise ValueError("mounting a GF store requires a mounted directory path")
        return bridge_pb2.StringValue(value=gf_stores.register(str(data)))


_SERVICER = BridgeServicer()


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
    """Dispatch a bridge method through the generated typed servicer.

    Returns {"result": <BridgeResult bytes>, "buffer"?: ndarray} and never
    raises: any exception becomes a BridgeError envelope."""
    token = operation.OPERATION_REPORTER.set(cast(Optional[operation.OperationReporter], reporter))
    begin_wait_token = operation.BEGIN_EXTERNAL_WAIT.set(
        cast(Optional[operation.ExternalWaitCallback], begin_external_wait)
    )
    end_wait_token = operation.END_EXTERNAL_WAIT.set(
        cast(Optional[operation.ExternalWaitCallback], end_external_wait)
    )
    commit_gate = cast(Optional[operation.CommitGate], begin_commit)

    sealed: Optional[bool] = None

    def seal_execution() -> bool:
        """Linearize this exit against cancellation; False when cancellation won.

        The gate CAS disarms the interrupt byte so rollback and reply
        marshalling never receive a stray SIGINT.
        Memoized because the gate is one-shot: a commit failing after sealing
        would otherwise read False and report a cancellation nobody asked for.
        """
        nonlocal sealed
        if sealed is None:
            sealed = commit_gate is None or commit_gate()
        return sealed

    def finish_call_result(
        result: Dict[str, Any], snapshot: Optional[state.SessionSnapshot]
    ) -> Dict[str, Any]:
        sealed_in_time = seal_execution()
        if snapshot is not None:
            state.session_store.rollback(snapshot)
        return result if sealed_in_time else bridge_dispatch.cancelled_result()

    snapshot: Optional[state.SessionSnapshot] = None
    in_view_lane = False
    try:
        # Interpreter ownership is granted here, inside the protecting try,
        # never by the worker before the call, so Pyodide's coroutine plumbing
        # between the JS invocation and this frame can never consume an
        # interrupt.
        # A pending cancellation arms the byte and raises on the next bytecode.
        if enter_protected_frame is not None:
            enter_protected_frame()
        # An unknown method id must surface as a typed envelope, not escape to
        # JS as a transport error.
        activity = bridge_dispatch.ACTIVITY_BY_METHOD[method]
        if activity.lane == "VIEW":
            in_view_lane = True
            state.session_store.enter_view()
            result = await bridge_dispatch.dispatch(_SERVICER, method, _raw_bytes(request), data)
            envelope = bridge_pb2.BridgeResult.FromString(result["result"])
            if envelope.WhichOneof("result") == "cancelled":
                state.invalidate_interrupted_view()
                result = await bridge_dispatch.dispatch(
                    _SERVICER, method, _raw_bytes(request), data
                )
            return finish_call_result(result, None)
        if activity.session_access == "MUTATING":
            snapshot = state.session_store.snapshot()
            state.session_store.begin_transaction(snapshot)
        result = await bridge_dispatch.dispatch(_SERVICER, method, _raw_bytes(request), data)
        if snapshot is None:
            return finish_call_result(result, None)
        envelope = bridge_pb2.BridgeResult.FromString(result["result"])
        if envelope.WhichOneof("result") in ("error", "cancelled", "stale"):
            return finish_call_result(result, snapshot)
        if not seal_execution():
            state.session_store.rollback(snapshot)
            return bridge_dispatch.cancelled_result()
        update = state.session_store.capture_update()
        state.session_store.restore_snapshot(snapshot)
        stale_commit = state.session_store.commit(snapshot.generation, update)
        if stale_commit is None:
            return result
        stale = bridge_pb2.BridgeResult()
        stale.stale.expected_generation = stale_commit.expected_generation
        stale.stale.actual_generation = stale_commit.actual_generation
        return {"result": stale.SerializeToString()}
    except KeyboardInterrupt:
        # No interrupt reaches the view lane, whose only rpc GetView is not
        # cancellable; a cancellable view rpc would need enter_view/exit_view
        # made atomic first.
        seal_execution()
        if snapshot is not None:
            state.session_store.rollback(snapshot)
        return bridge_dispatch.cancelled_result()
    except BaseException as exc:
        cancellation_won = not seal_execution()
        if snapshot is not None:
            state.session_store.rollback(snapshot)
        # Cancellation-won takes precedence over the failure, matching the
        # error-envelope path in finish_call_result.
        if cancellation_won:
            return bridge_dispatch.cancelled_result()
        return bridge_dispatch.failure_result(exc)
    finally:
        if in_view_lane:
            state.session_store.exit_view()
        operation.OPERATION_REPORTER.reset(token)
        operation.BEGIN_EXTERNAL_WAIT.reset(begin_wait_token)
        operation.END_EXTERNAL_WAIT.reset(end_wait_token)
