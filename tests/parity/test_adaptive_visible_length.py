"""The Adaptive visible length, the one policy tier 1 cannot compare against Qt.

Snuffler has four fixed sample-count budgets and spends them by refusing channels; this
frontend adds a fifth entry that refuses none and decimates the payload instead
(`known_gaps.toml`, `adaptive-visible-length-default`).
Nothing upstream answers the same question, so there is no oracle — what these tests
hold it to is its own two claims: everything is drawn, and what is drawn is what the raw
samples would have drawn.

    task test:parity:bridge

The scenarios keep comparing Qt-comparable budgets: `scenario.DEFAULTS` carries a
float, which cannot spell Adaptive, and `web_oracle` sends the flag off.
"""

from __future__ import annotations

# harness first: it prepares sys.path and the pyrocko scratch home before
# anything imports pyrocko or the bridge.
import harness  # isort: skip # noqa: F401

from typing import Dict, Iterator, List, Tuple

import numpy as np
import pytest
from pyrocko import trace as ptrace
from snuffler_bridge import state, view
from snuffler_web_proto import view_pb2

# Wide enough that a 100 Hz channel is far past every fixed budget, and coarse
# enough that a 1 Hz one is inside all of them.
WINDOW = (0.0, 3600.0)
NPX = 1200


def trace_of(cha: str, deltat: float, y: np.ndarray) -> ptrace.Trace:
    return ptrace.Trace(
        network="XX", station="TEST", location="", channel=cha, deltat=deltat, tmin=0.0, ydata=y
    )


def draw(
    *, adaptive: bool, visible_length: float = 20_000.0
) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
    req = view_pb2.ViewParams(
        tmin=WINDOW[0],
        tmax=WINDOW[1],
        bins_per_trace=NPX,
        sort="name",
        subsort="nslc",
        mode="wiggle",
        scaling_base="minmax",
        scaling_group="individual",
        gain=1.0,
        track_offset=0,
        track_count=1 << 32,
        adaptive_visible_length=adaptive,
    )
    if not adaptive:
        req.visible_length = visible_length
    meta, ys = view.get_view_data(req)
    return meta, ys


def payloads(meta: view_pb2.ViewMeta, ys: np.ndarray) -> Dict[str, List[np.ndarray]]:
    """Every track's spans, by channel, as the samples the painter would draw."""
    out: Dict[str, List[np.ndarray]] = {}
    for track in meta.tracks:
        runs = out.setdefault(track.cha, [])
        for span in track.spans:
            runs.append(np.asarray(ys[span.offset : span.offset + span.count], dtype=np.float64))
    return out


@pytest.fixture(autouse=True)
def clean_session() -> Iterator[None]:
    state.traces[:] = []
    state.hidden_patterns[:] = []
    state.view_notices.clear()
    # The cutout cache is keyed on the pile generation, but these tests ask the
    # same window over and over with different policies; clearing it keeps a
    # failure about the policy rather than about a stale cache.
    state.cutout_key = None
    state.cutout_traces = None
    state.mark_traces_changed()
    yield
    state.traces[:] = []
    state.cutout_key = None
    state.cutout_traces = None
    state.mark_traces_changed()


def test_a_fixed_budget_refuses_the_fine_channel_and_adaptive_draws_it() -> None:
    rng = np.random.default_rng(7)
    coarse = trace_of("LHZ", 1.0, rng.normal(size=3600))
    fine = trace_of("HNZ", 0.01, rng.normal(size=360_000))
    state.traces[:] = [coarse, fine]
    state.mark_traces_changed()

    # Short is upstream's default and this window is an hour: min_deltat_allow lands
    # well above 0.01 s, so the 100 Hz channel is not fetched at all.
    # The row survives — snuffler still stacks it — but it carries nothing to draw.
    short = payloads(*draw(adaptive=False, visible_length=20_000.0))
    assert short["LHZ"], "the 1 Hz channel is inside every fixed budget"
    assert not short["HNZ"], "the 100 Hz channel is what a fixed budget refuses"

    adaptive = payloads(*draw(adaptive=True))
    assert adaptive["LHZ"]
    assert adaptive["HNZ"], "Adaptive refuses nothing"


def test_adaptive_keeps_the_peak_a_subsample_would_step_over() -> None:
    # One spike, one sample wide, in a quiet trace: it survives a min/max
    # envelope and disappears under any stride that does not land on it.
    y = np.zeros(360_000)
    y[123_457] = 42.0
    state.traces[:] = [trace_of("HNZ", 0.01, y)]
    state.mark_traces_changed()

    runs = payloads(*draw(adaptive=True))["HNZ"]
    assert runs
    drawn = np.concatenate(runs)
    assert drawn.max() == pytest.approx(42.0, rel=1e-6), "the spike is what the envelope is for"
    assert drawn.size < y.size / 50, "and it costs a fraction of the samples"


def test_the_adaptive_payload_carries_the_extremes_of_the_samples_it_replaces() -> None:
    rng = np.random.default_rng(11)
    y = np.cumsum(rng.normal(size=360_000))
    state.traces[:] = [trace_of("HNZ", 0.01, y)]
    state.mark_traces_changed()

    # Extra long is 600000 samples, which this hour of 100 Hz is inside, so the
    # same channel comes back at its native rate — the raw answer to compare to.
    raw = np.concatenate(payloads(*draw(adaptive=False, visible_length=600_000.0))["HNZ"])
    drawn = np.concatenate(payloads(*draw(adaptive=True))["HNZ"])

    assert drawn.size < raw.size / 10
    # Both ends: an envelope that clipped the range would fail the first, and one
    # that invented a value outside it would fail the second.
    assert drawn.min() == pytest.approx(raw.min(), rel=1e-6)
    assert drawn.max() == pytest.approx(raw.max(), rel=1e-6)


def test_a_fixed_budget_still_hands_over_raw_samples_at_the_trace_rate() -> None:
    # The four Qt entries are untouched by any of this: unset means upstream's
    # 20000, and what comes back is the samples themselves at the trace's own
    # deltat, which is what tier 1 compares against a painted PileViewerMain.
    state.traces[:] = [trace_of("LHZ", 1.0, np.arange(3600, dtype=float))]
    state.mark_traces_changed()

    meta, _ys = draw(adaptive=False)
    spans = [span for track in meta.tracks for span in track.spans]
    assert spans
    for span in spans:
        assert span.deltat == pytest.approx(1.0)


def test_the_scenario_default_is_a_qt_comparable_budget() -> None:
    # Tier 1 must never compare Adaptive against Qt, which has no counterpart for it.
    # The knob being a float is what makes that structural rather than a convention, and
    # this is the test that says so out loud.
    import scenario

    assert isinstance(scenario.DEFAULTS["visible_length"], float)
    assert scenario.DEFAULTS["visible_length"] > 0.0
