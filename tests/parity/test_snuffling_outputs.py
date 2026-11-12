"""Compare produced traces, picks, reports and spectra with upstream snuffling calls."""

from __future__ import annotations

import harness  # isort: skip # noqa: F401

import asyncio
from typing import Any, Iterator

import numpy as np
import pytest
import qt_oracle
from corpus import build as corpus
from matplotlib.backends.backend_agg import FigureCanvasAgg
from matplotlib.figure import Figure
from pyrocko import io
from pyrocko.gui.snuffler.marker import Marker
from pyrocko.gui.snuffler.snufflings.ampspec import AmpSpec
from pyrocko.gui.snuffler.snufflings.minmax import MinMaxSnuffling
from pyrocko.gui.snuffler.snufflings.rms import RootMeanSquareSnuffling
from pyrocko.gui.snuffler.snufflings.stalta import DetectorSTALTA
from snuffler_bridge import figures, rms, snufflings, stalta, state
from snuffler_web_proto import snuffling_pb2


@pytest.fixture
def scene() -> Iterator[qt_oracle.QtViewer]:
    path = corpus.path("demo.mseed")
    state.set_traces(io.load(str(path)))
    native = qt_oracle.QtViewer([path])
    host = qt_oracle.host_window(native.frame, native.frame.controls())
    host.is_closing = lambda: False
    native.view.show_all = False
    native.view.set_time_range(native.pile.tmin, native.pile.tmax)
    native.view.sortingmode_change()
    yield native
    host.close()
    native.close()
    state.set_traces([])


def request_for(native: qt_oracle.QtViewer, selected: bool) -> snuffling_pb2.SnufflingRequestParams:
    request = snuffling_pb2.SnufflingRequestParams()
    # Leave real samples on both sides for the snufflings' settling padding.
    native.view.set_time_range(native.pile.tmin + 10.0, native.pile.tmax - 10.0)
    request.view.tmin, request.view.tmax = native.view.get_time_range()
    if selected:
        nslc = state.traces[0].nslc_id
        start, end = request.view.tmin + 100.0, request.view.tmin + 200.0
        marker = Marker([nslc], start, end)
        marker.selected = True
        native.view.add_markers([marker])
        span = request.selected_spans.add(tmin=start, tmax=end)
        span.nslc_ids.add(net=nslc[0], sta=nslc[1], loc=nslc[2], cha=nslc[3])
    return request


def configure(snuffling: Any, native: qt_oracle.QtViewer, **parameters: Any) -> None:
    snuffling.setup()
    snuffling.init_gui(native.view, None, None)
    for name, value in parameters.items():
        snuffling.set_parameter(name, value)


def assert_derived(native: qt_oracle.QtViewer, origin: str) -> None:
    expected = sorted(
        (trace for trace in native.pile.iter_traces() if trace.meta and trace.meta.get("tabu")),
        key=lambda trace: trace.full_id,
    )
    actual = sorted(state.derived_traces[origin], key=lambda trace: trace.full_id)
    assert expected and len(actual) == len(expected)
    for left, right in zip(expected, actual, strict=True):
        assert left.nslc_id == right.nslc_id
        assert left.tmin == pytest.approx(right.tmin, abs=1e-6, rel=0.0)
        assert left.deltat == right.deltat
        np.testing.assert_allclose(left.get_ydata(), right.get_ydata(), rtol=1e-10, atol=1e-12)


@pytest.mark.parametrize("selected", [False, True])
@pytest.mark.parametrize("group,logarithmic", [(False, False), (True, False), (True, True)])
def test_rms_output_matches_upstream(
    scene: qt_oracle.QtViewer, selected: bool, group: bool, logarithmic: bool
) -> None:
    request = request_for(scene, selected)
    request.snufflings.rms.CopyFrom(
        snuffling_pb2.RmsSnufflingParams(
            block_length_s=10.0,
            group_components=group,
            logarithmic=logarithmic,
            highpass=0.1,
            lowpass=10.0,
        )
    )
    oracle = RootMeanSquareSnuffling()
    configure(
        oracle,
        scene,
        block_length=10.0,
        group_channels=group,
        log=logarithmic,
        highpass=0.1,
        lowpass=10.0,
    )
    oracle.call()
    asyncio.run(rms.run(request))
    assert_derived(scene, "rms")


@pytest.mark.parametrize("selected", [False, True])
@pytest.mark.parametrize(
    "variant,total", [("centered", False), ("right", False), ("centered", True)]
)
def test_stalta_traces_and_picks_match_upstream(
    scene: qt_oracle.QtViewer, selected: bool, variant: str, total: bool
) -> None:
    request = request_for(scene, selected)
    request.snufflings.stalta.CopyFrom(
        snuffling_pb2.StaltaSnufflingParams(
            short_window_s=1.0,
            long_to_short_window_ratio=3.0,
            trigger_level=0.5,
            show_level_traces=True,
            variant=variant,
            scaling_method="[0-1]",
            apply_to_sum=total,
        )
    )
    oracle = DetectorSTALTA()
    configure(
        oracle,
        scene,
        swin=1.0,
        ratio=3.0,
        level=0.5,
        show_level_traces=True,
        variant=variant,
        scalingmethod="[0-1]",
        apply_to_sum=total,
    )
    oracle.call()
    result = asyncio.run(stalta.run(request))
    assert_derived(scene, "stalta")
    expected = sorted((marker.tmin, tuple(marker.nslc_ids)) for marker in oracle._markers)
    if variant == "right":
        assert expected, "the right-aligned fixture must exercise real trigger picks"
    actual = sorted(
        (
            marker.tmin,
            tuple(
                (
                    result.nslc_table[index].net,
                    result.nslc_table[index].sta,
                    result.nslc_table[index].loc,
                    result.nslc_table[index].cha,
                )
                for index in marker.nslc_indexes
            ),
        )
        for marker in result.markers
    )
    assert len(actual) == len(expected)
    for (qt_time, qt_scope), (web_time, web_scope) in zip(expected, actual, strict=True):
        assert qt_scope == web_scope
        assert qt_time == pytest.approx(web_time, abs=1e-6, rel=0.0)


@pytest.mark.parametrize("selected", [False, True])
def test_minmax_report_matches_upstream(
    scene: qt_oracle.QtViewer, selected: bool, capsys: pytest.CaptureFixture[str]
) -> None:
    request = request_for(scene, selected)
    oracle = MinMaxSnuffling()
    configure(oracle, scene)
    oracle.call()
    expected = {
        line.split(":")[0]: [float(value) for value in line.split(":")[1].split()]
        for line in capsys.readouterr().out.splitlines()
        if ":" in line
    }
    report = asyncio.run(snufflings.run("minmax", request)).text
    actual = {
        fields[0]: [float(value) for value in fields[1:]]
        for line in report.splitlines()[1:]
        if (fields := line.split())
    }
    assert expected, "the upstream report must contain channel rows"
    assert actual.keys() == expected.keys()
    for channel in expected:
        np.testing.assert_allclose(actual[channel], expected[channel], rtol=1e-4, atol=1e-8)


@pytest.mark.parametrize("smoothing", [False, True])
def test_spectrum_plot_data_matches_upstream(
    scene: qt_oracle.QtViewer, smoothing: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    request = request_for(scene, True)
    oracle = AmpSpec()
    configure(oracle, scene, want_smoothing=smoothing, b=40.0)
    qt_figure, web_figure = Figure(), Figure()
    FigureCanvasAgg(qt_figure)
    FigureCanvasAgg(web_figure)
    # Replace the window allocation only; upstream still computes and plots every sample.
    monkeypatch.setattr(oracle, "figure", lambda **_kwargs: qt_figure)
    oracle.call()
    request.snufflings.spectrum.CopyFrom(
        snuffling_pb2.SpectrumSnufflingParams(smoothing=smoothing, smoothing_bandwidth=40.0)
    )
    figures._draw_amplitude_spectra(web_figure, request)
    expected, actual = qt_figure.axes[0].lines, web_figure.axes[0].lines
    assert len(expected) == len(actual) == (2 if smoothing else 1)
    for left, right in zip(expected, actual, strict=True):
        np.testing.assert_allclose(left.get_xdata(), right.get_xdata(), rtol=0.0, atol=0.0)
        np.testing.assert_allclose(left.get_ydata(), right.get_ydata(), rtol=1e-9, atol=1e-12)
