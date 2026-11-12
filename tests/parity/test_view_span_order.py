"""A grouped row's span stack must not depend on the time window.

Co-located channels of one grouped row draw on top of each other, so the last span
painted decides the row's visible colour.
Ordering the members by their *chopped* tmin, which snaps to each trace's own sample
grid, would let two sample rates swap places as the window shifts, and a small pan or
zoom would recolour the row (IU TUC BH2 in the multiloc example flips blue to orange).
These tests pin that the stack is keyed on channel identity: identical across windows
and equal to the nslc order.

    task test:parity:bridge
"""

from __future__ import annotations

# harness first: it prepares sys.path and the pyrocko scratch home before
# anything imports pyrocko or the bridge.
import harness  # isort: skip # noqa: F401

from typing import Iterator, List

import numpy as np
import pytest
from pyrocko import trace as ptrace
from snuffler_bridge import state, view
from snuffler_web_proto import view_pb2


def located_trace(loc: str, deltat: float, n: int) -> ptrace.Trace:
    rng = np.random.default_rng(int(loc) + 1)
    return ptrace.Trace(
        network="IU",
        station="TUC",
        location=loc,
        channel="BH2",
        deltat=deltat,
        tmin=0.0,
        ydata=rng.normal(size=n),
    )


def span_locs(tmin: float, tmax: float) -> List[str]:
    req = view_pb2.ViewParams(
        tmin=tmin,
        tmax=tmax,
        bins_per_trace=1200,
        sort="name",
        subsort="nsc_l",
        mode="wiggle",
        scaling_base="minmax",
        scaling_group="individual",
        gain=1.0,
        track_offset=0,
        track_count=1 << 32,
        visible_length=20_000.0,
    )
    meta, _ys = view.get_view_data(req)
    assert len(meta.tracks) == 1, "one grouped row carries every location"
    return [span.loc for span in meta.tracks[0].spans]


@pytest.fixture(autouse=True)
def clean_session() -> Iterator[None]:
    state.traces[:] = []
    state.hidden_patterns[:] = []
    state.view_notices.clear()
    state.cutout_key = None
    state.cutout_traces = None
    state.mark_traces_changed()
    yield
    state.traces[:] = []
    state.cutout_key = None
    state.cutout_traces = None
    state.mark_traces_changed()


def _view_meta(
    tmin: float, tmax: float, mode: str = "wiggle", adaptive: bool = False
) -> "view_pb2.ViewMeta":
    req = view_pb2.ViewParams(
        tmin=tmin,
        tmax=tmax,
        bins_per_trace=1200,
        sort="name",
        subsort="nslc",
        mode=mode,
        scaling_base="minmax",
        scaling_group="individual",
        gain=1.0,
        track_offset=0,
        track_count=1 << 32,
        adaptive_visible_length=adaptive,
    )
    if not adaptive:
        req.visible_length = 20_000.0
    meta, _ys = view.get_view_data(req)
    return meta


def test_waterfall_keeps_the_full_row_universe() -> None:
    # The frontend treats meta.tracks as the whole row stack: it derives the scroll
    # range and the next page request from its length.
    # Dropping span-less rows would kill scrolling and ratchet the stack down to the
    # occupied rows.
    rng = np.random.default_rng(3)
    inside = ptrace.Trace(
        network="XX",
        station="AAA",
        channel="HHZ",
        deltat=0.1,
        tmin=0.0,
        ydata=rng.normal(size=2000),
    )
    outside = ptrace.Trace(
        network="XX",
        station="BBB",
        channel="HHZ",
        deltat=0.1,
        tmin=5_000.0,
        ydata=rng.normal(size=2000),
    )
    state.traces[:] = [inside, outside]
    state.mark_traces_changed()

    meta = _view_meta(0.0, 100.0, mode="waterfall")
    assert len(meta.tracks) == 2, "the empty row stays in the stack"
    occupied = {track.sta: bool(track.spans) for track in meta.tracks}
    assert occupied == {"AAA": True, "BBB": False}


def test_adaptive_envelope_stride_survives_a_sub_sample_pan() -> None:
    # The envelope's stride derives from the window, not from the clipped
    # slice length: the slice gains or loses a sample with the window's
    # sub-sample phase, and a slice-derived stride could jump a quarter-octave
    # rung on a pure pan, re-binning the whole trace at once.
    rng = np.random.default_rng(5)
    state.traces[:] = [
        ptrace.Trace(
            network="XX",
            station="AAA",
            channel="HHZ",
            deltat=0.01,
            tmin=0.0,
            ydata=rng.normal(size=360_000),
        )
    ]
    state.mark_traces_changed()

    def span_dts(tmin: float) -> List[float]:
        meta = _view_meta(tmin, tmin + 600.0, adaptive=True)
        return [span.deltat for track in meta.tracks for span in track.spans]

    baseline = span_dts(100.0)
    assert baseline, "the adaptive window must produce spans"
    for shift in (0.003, 0.007, 0.0099):
        assert span_dts(100.0 + shift) == baseline, f"stride moved under a {shift}s pan"


def test_grouped_span_stack_is_identical_across_shifted_windows() -> None:
    # Mixed sample rates are the trigger: the chop start snaps to each trace's
    # own grid, so a tmin-first ordering re-ranked them per window.
    state.traces[:] = [
        located_trace("00", 0.05, 20_000),
        located_trace("10", 0.025, 40_000),
        located_trace("60", 0.025, 40_000),
    ]
    state.mark_traces_changed()

    # Window starts chosen off both sample grids, the way a pan lands.
    stacks = [
        span_locs(start, start + 300.0) for start in (100.0, 100.013, 100.037, 217.511, 402.003)
    ]
    assert all(stack == stacks[0] for stack in stacks), stacks
    assert stacks[0] == ["00", "10", "60"], "the stack is the nslc order"


@pytest.mark.parametrize("stride", [1, 2, 4, 8, 16, 1024])
def test_exact_power_of_two_keeps_its_envelope_stride(stride: int) -> None:
    assert view._stable_bin_stride(float(stride)) == stride


def test_envelope_selects_a_ladder_rung_before_integer_rounding() -> None:
    assert view._stable_bin_stride(2.001) == 3
    assert view._stable_bin_stride(4.001) == 5


@pytest.mark.parametrize("filtered", [False, True])
@pytest.mark.parametrize("absolute", [False, True])
def test_integrated_waterfall_overlap_is_identical_after_a_pan(
    filtered: bool, absolute: bool
) -> None:
    samples = np.arange(10000, dtype=float)
    state.set_traces(
        [
            ptrace.Trace(
                network="XX",
                station="AAA",
                channel="BHZ",
                tmin=0.0,
                deltat=0.1,
                ydata=3.0 + np.sin(samples * 0.03),
            )
        ]
    )

    def pixels(start: float) -> dict[int, int]:
        request = view_pb2.ViewParams(
            tmin=start,
            tmax=start + 200.0,
            bins_per_trace=3000,
            mode="waterfall",
            waterfall_integrate=True,
            waterfall_absolute_amplitudes=absolute,
            adaptive_visible_length=True,
            track_count=1,
            demean=filtered,
        )
        if filtered:
            request.highpass = 0.1
        meta, payload = view.get_view_data(request)
        output: dict[int, int] = {}
        for span in meta.tracks[0].spans:
            rgba = payload[int(span.offset) : int(span.offset + span.count)].view("<u4")
            for index, pixel in enumerate(rgba):
                sample = round((meta.data_tmin + span.relative_start_s + index * span.deltat) / 0.1)
                output[sample] = int(pixel)
        return output

    first, panned = pixels(100.0), pixels(175.03)
    overlap = first.keys() & panned.keys()
    assert len(overlap) > 1000
    assert all(first[sample] == panned[sample] for sample in overlap)


def test_scale_identity_survives_equal_ranges_and_panning() -> None:
    source = located_trace("00", 0.1, 10000)
    second = source.copy()
    second.set_codes(location="10")
    state.set_traces([source, second])

    def keys(start: float, group: str) -> dict[str, int]:
        meta, _payload = view.get_view_data(
            view_pb2.ViewParams(
                tmin=start,
                tmax=start + 200,
                bins_per_trace=1200,
                subsort="nsc_l",
                scaling_group=group,
                track_count=1,
                adaptive_visible_length=True,
            )
        )
        spans = meta.tracks[0].spans
        assert len({(span.ymin, span.ymax) for span in spans}) == 1
        return {span.loc: span.scale_key for span in spans}

    individual = keys(100.0, "individual")
    assert len(set(individual.values())) == 2
    assert keys(120.0, "individual") == individual
    assert len(set(keys(100.0, "common").values())) == 1


def test_unfetched_rows_are_distinct_from_rows_without_data() -> None:
    state.set_traces([located_trace("00", 0.1, 1000), located_trace("10", 0.1, 1000)])
    request = view_pb2.ViewParams(
        tmin=2000.0,
        tmax=2100.0,
        bins_per_trace=1200,
        track_offset=1,
        track_count=1,
        adaptive_visible_length=True,
    )
    meta, _payload = view.get_view_data(request)
    assert [track.samples_omitted for track in meta.tracks] == [True, False]
    assert all(not track.spans for track in meta.tracks)
