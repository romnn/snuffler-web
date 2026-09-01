"""A grouped row's span stack must not depend on the time window.

Co-located channels of one grouped row draw on top of each other, so the last
span painted decides the row's visible colour. The members used to be ordered
by their *chopped* tmin — a value that snaps to each trace's own sample grid —
so two sample rates could swap places as the window shifted, and a small pan or
zoom visibly recoloured the row (IU TUC BH2 in the multiloc example flipped
blue to orange). The order is keyed on channel identity now; these tests pin
that the stack is identical across windows and equals the nslc order.

    task test:parity:bridge
"""

from __future__ import annotations

# harness first: it prepares sys.path and the pyrocko scratch home before
# anything imports pyrocko or the bridge.
import harness  # isort: skip # noqa: F401

from typing import Iterator, List

import bridge
import numpy as np
import pytest
from pyrocko import trace as ptrace


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
    req = bridge.view_pb2.ViewParams(
        tmin=tmin,
        tmax=tmax,
        npx=1200,
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
    meta, _ys = bridge._get_view_data(req)
    assert len(meta.tracks) == 1, "one grouped row carries every location"
    return [span.loc for span in meta.tracks[0].spans]


@pytest.fixture(autouse=True)
def clean_session() -> Iterator[None]:
    bridge._TRACES[:] = []
    bridge._HIDDEN[:] = []
    bridge._VIEW_NOTICES.clear()
    bridge._CUTOUT_VEC = None
    bridge._CUTOUT_TRACES = None
    bridge._mark_traces_changed()
    yield
    bridge._TRACES[:] = []
    bridge._CUTOUT_VEC = None
    bridge._CUTOUT_TRACES = None
    bridge._mark_traces_changed()


def test_grouped_span_stack_is_identical_across_shifted_windows() -> None:
    # Mixed sample rates are the trigger: the chop start snaps to each trace's
    # own grid, so a tmin-first ordering re-ranked them per window.
    bridge._TRACES[:] = [
        located_trace("00", 0.05, 20_000),
        located_trace("10", 0.025, 40_000),
        located_trace("60", 0.025, 40_000),
    ]
    bridge._mark_traces_changed()

    # Window starts chosen off both sample grids, the way a pan lands.
    stacks = [
        span_locs(start, start + 300.0) for start in (100.0, 100.013, 100.037, 217.511, 402.003)
    ]
    assert all(stack == stacks[0] for stack in stacks), stacks
    assert stacks[0] == ["00", "10", "60"], "the stack is the nslc order"
