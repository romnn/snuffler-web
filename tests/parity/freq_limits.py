"""Property test for the one pair that documents itself as a copy.

``bridge._decon_freq_limits`` says it computes the deconvolution taper limits
"exactly as snuffler's ``PileViewer.get_freq_limits``". A stated contract like that
is worth checking over a grid rather than at the single point some scenario happens
to reach, because the two disagree only in the corners: near Nyquist, at very short
traces, with one corner set and not the other.

Run through :mod:`run_oracle` (``task test:parity -- decon-freq-limits`` alone).
"""

from __future__ import annotations

# harness first: it puts the bridge on sys.path before the import below.
import harness  # noqa: F401 — imported for its side effects  # isort: skip

from typing import Iterator, List, Optional, Tuple

import bridge

Limits = Tuple[float, float, float, float]

# Sampling rates from a strong-motion array down to a very long-period channel, plus
# one whose period no float represents exactly.
DELTATS = (0.005, 0.01, 0.02, 0.05, 1.0 / 7.0, 0.2, 1.0, 60.0)
# 0 is in here because the two guard it differently, which is the point.
DATA_LENS = (0, 1, 2, 10, 100, 3_000, 100_000)
HIGHPASSES: Tuple[Optional[float], ...] = (None, 0.0, 0.001, 0.05, 0.5, 5.0, 100.0)
LOWPASSES: Tuple[Optional[float], ...] = (None, 0.0, 0.01, 0.5, 2.0, 50.0, 1_000.0)


def grid() -> Iterator[Tuple[float, int, Optional[float], Optional[float]]]:
    for deltat in DELTATS:
        for data_len in DATA_LENS:
            for highpass in HIGHPASSES:
                for lowpass in LOWPASSES:
                    yield deltat, data_len, highpass, lowpass


def qt_limits(view: object, deltat: float, data_len: int) -> Limits:
    return tuple(view.get_freq_limits(deltat, data_len))  # type: ignore[attr-defined,return-value]


def run(view: object) -> Tuple[int, List[str]]:
    """Compare the pair across the whole grid; return (points, failures)."""
    failures: List[str] = []
    points = 0
    for deltat, data_len, highpass, lowpass in grid():
        view.highpass = highpass  # type: ignore[attr-defined]
        view.lowpass = lowpass  # type: ignore[attr-defined]
        try:
            expected = qt_limits(view, deltat, data_len)
        except ZeroDivisionError:
            # Qt divides by `data_len` unguarded; the web clamps it to 1. An empty
            # trace never reaches Qt's deconvolution, so this is the web being
            # defensive rather than the two disagreeing about a real case.
            if data_len == 0:
                continue
            raise
        actual = bridge._decon_freq_limits(deltat, data_len, highpass, lowpass)
        points += 1
        if any(
            abs(a - b) > 1e-12 * max(1.0, abs(b)) for a, b in zip(actual, expected, strict=True)
        ):
            failures.append(
                f"deltat={deltat!r} n={data_len} hp={highpass!r} lp={lowpass!r}: "
                f"web {actual} vs Qt {expected}"
            )
    return points, failures
