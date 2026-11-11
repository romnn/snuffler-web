"""The RunSnuffling rpc: dispatch to the built-in snufflings by name."""

from __future__ import annotations

from pyrocko import trace as ptrace
from snuffler.bridge.v1 import snuffling_pb2

from . import cake, polarization, seismosizer, snuffling_scope, stalta


def _minmax_result(request: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Qt's Minimum/Maximum snuffling: the raw value range of every selected channel."""
    # Qt's Minimum/Maximum reports raw pile values (minmax.py:42 passes no
    # `main_bandpass`), and `trace.minmax` folds every chopped piece into
    # one row per NSLC (minmax.py:49).
    mima = ptrace.minmax(
        segment
        for batch in snuffling_scope.chopper_selected_traces(request, degap=False)
        for segment in batch.traces
        if segment.ydata is not None and len(segment.ydata)
    )
    lines = ["%-16s %13s %13s %13s" % ("channel", "min", "max", "peak-to-peak")]
    for nslc in sorted(mima):
        mi, ma = (float(v) for v in mima[nslc])
        lines.append("%-16s %+.4e %+.4e %.4e" % (".".join(nslc), mi, ma, ma - mi))
    text = "\n".join(lines) if len(lines) > 1 else "No data in the visible window."
    return snuffling_pb2.SnufflingResult(show_window=True, text=text)


async def run(
    name: str, request: snuffling_pb2.SnufflingRequestParams
) -> snuffling_pb2.SnufflingResult:
    """Run a built-in snuffling over the current window."""
    if name == "cake":
        return cake.run(request)

    if name == "seismosizer":
        return seismosizer.run(request)

    if name == "minmax":
        return _minmax_result(request)

    if name == "stalta":
        return await stalta.run(request)

    if name == "polarization":
        return polarization.text_result(request)

    return snuffling_pb2.SnufflingResult(show_window=True, text="Unknown snuffling: %s" % name)
