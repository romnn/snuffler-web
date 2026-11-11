"""The GetView rpc: the drawn tracks, spans and scales of one view request."""

from __future__ import annotations

import math
import time
from collections import OrderedDict
from importlib import import_module
from typing import Any, Callable, Dict, List, Mapping, NamedTuple, Optional, Protocol, Tuple, cast

import numpy as np
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import view_pb2

from . import cutout, sorting, state, waterfall
from .nslc import NslcId

_TraceRangeFunc = Callable[..., Mapping[Any, Tuple[Any, Any]]]


class _AutoScalerLike(Protocol):
    def make_scale(self, data_range: Tuple[float, float]) -> Tuple[float, float, float]: ...
    def make_exp(self, x: float) -> int: ...


_PERF_LOG_INTERVAL_S = 1.0

_PERF_SLOW_MS = 25.0

_last_perf_log_s = 0.0


def _scaling_key(trace: ptrace.Trace, group: str) -> Any:
    """Grouping key for shared amplitude scaling (snuffler's Scaling menu).

    Asked of the trace, never of the track: a row that groups over location carries
    traces with different keys, and snuffler scales each of them separately.
    """
    if group == "common":
        return None
    if group == "per_station":
        return (trace.network, trace.station)
    if group == "per_station_location":
        return (trace.network, trace.station, trace.location)
    if group == "per_component":
        return trace.channel
    return trace.nslc_id


def _scaling_base_modes(base: str) -> Tuple[Any, str]:
    if base == "robust":
        return "minmax", "robust"
    if base == "mean2":
        return 2.0, "minmax"
    if base == "mean4":
        return 4.0, "minmax"
    return "minmax", "minmax"


def _range_trace(
    nslc: NslcId,
    tmin: float,
    deltat: float,
    ydata: np.ndarray,
) -> ptrace.Trace:
    net, sta, loc, cha = nslc
    return ptrace.Trace(
        network=net,
        station=sta,
        location=loc,
        channel=cha,
        tmin=tmin,
        deltat=deltat,
        ydata=np.asarray(ydata, dtype=np.float64),
    )


def _slice_segment(
    entry: Tuple[float, float, np.ndarray],
    tmin: float,
    tmax: float,
    sample_support: float = 0.0,
) -> Optional[Tuple[np.ndarray, float, float, int]]:
    seg_tmin, deltat, y = entry
    pad = max(0.0, float(sample_support)) * deltat
    start = max(0, int(math.floor((tmin - pad - seg_tmin) / deltat)))
    stop = min(len(y), int(math.ceil((tmax + pad - seg_tmin) / deltat)) + 1)
    if stop <= start:
        return None
    return y[start:stop], seg_tmin + start * deltat, deltat, start


def _stable_bin_stride(samples_per_bin: float) -> int:
    """Round a bin width up onto a quarter-octave ladder.

    The envelope bins on an absolute sample grid so that panning slides the
    trace instead of reshuffling which samples are extrema.
    That only holds while the stride itself holds still, and an unquantised
    stride would change with every pixel of zoom.
    Quantising to powers of 2**0.25 keeps it constant across small zooms.
    The double ceiling (once on the input, once on the ladder value) makes the
    worst case about 25 % coarser than asked.
    """
    stride = max(1, int(math.ceil(samples_per_bin)))
    if stride <= 1:
        return 1
    value = 1.0
    while value < stride:
        value *= 2.0**0.25
    return max(1, int(math.ceil(value)))


def _display_envelope(
    y: np.ndarray, deltat: float, t0: float, bins: int, window_len: float
) -> Tuple[np.ndarray, float, float]:
    """Reduce a slice to about ``bins`` bins, as the min and max of each.

    The Adaptive visible length fetches every channel at its own rate and cuts
    the payload down here to the resolution the canvas can draw.
    Each bin emits its two extremes in the order they occur, at half the bin
    width, so the run stays a uniform span and every peak survives.

    Bins are anchored to the trace's absolute sample grid rather than to the
    slice, so a pan moves the trace without changing which sample is a peak.
    Up to one bin at each end falls outside the grid and is dropped.

    Returns the payload with its new sample spacing and start time, or the input
    unchanged when it is already at or below the target.
    """
    n = int(y.size)
    if bins <= 0 or n == 0 or deltat <= 0.0:
        return y, deltat, t0
    # The stride derives from the window, not the clipped slice length, which
    # gains or loses a sample on a pure pan and could cross a ladder rung.
    stride = _stable_bin_stride(window_len / (float(deltat) * float(bins)))
    if stride < 2 or n <= 2 * stride:
        return y, deltat, t0
    # Which absolute sample the slice starts on; deltat divides the trace's own
    # tmin exactly, so this is the index pyrocko would give it.
    base = int(math.floor(t0 / deltat + 0.5))
    first_bin = -(-base // stride)
    last_bin = (base + n) // stride
    nbins = int(last_bin - first_bin)
    if nbins < 2:
        return y, deltat, t0
    start = int(first_bin * stride - base)
    block = np.asarray(y[start : start + nbins * stride], dtype=np.float64).reshape(nbins, stride)
    lo = block.min(axis=1)
    hi = block.max(axis=1)
    rising = block.argmin(axis=1) <= block.argmax(axis=1)
    out = np.empty(nbins * 2, dtype=np.float64)
    out[0::2] = np.where(rising, lo, hi)
    out[1::2] = np.where(rising, hi, lo)
    return out, deltat * stride * 0.5, t0 + start * deltat


def _maybe_log_perf(
    mode: str,
    tmin: float,
    tmax: float,
    bins_per_trace: int,
    track_offset: int,
    track_count: int,
    total_tracks: int,
    wanted_tracks: int,
    sample_tracks: int,
    y_floats: int,
    t0: float,
    t_visible: float,
    t_prepare: float,
    t_loop: float,
    t_scale: float,
    t_concat: float,
    t_done: float,
) -> None:
    global _last_perf_log_s
    total_ms = (t_done - t0) * 1000.0
    now = time.monotonic()
    if total_ms < _PERF_SLOW_MS and now - _last_perf_log_s < _PERF_LOG_INTERVAL_S:
        return
    _last_perf_log_s = now
    print(
        "[py.view] "
        f"mode={mode} window_s={tmax - tmin:.3f} bins_per_trace={bins_per_trace} "
        f"track_page={track_offset}..{track_offset + track_count} "
        f"total_tracks={total_tracks} wanted_tracks={wanted_tracks} "
        f"sample_tracks={sample_tracks} ys_floats={y_floats} "
        f"ys_mb={y_floats * 4.0 / 1_000_000.0:.3f} "
        f"total_ms={total_ms:.2f} visible_ms={(t_visible - t0) * 1000.0:.2f} "
        f"prep_ms={(t_prepare - t_visible) * 1000.0:.2f} "
        f"loop_ms={(t_loop - t_prepare) * 1000.0:.2f} "
        f"scale_ms={(t_scale - t_loop) * 1000.0:.2f} "
        f"concat_ms={(t_concat - t_scale) * 1000.0:.2f}"
    )


def _color_keys(subsort: str) -> List[str]:
    """Snuffler's `color_keys` (`pile.gather_keys(color)`, pile_viewer.py:1753):
    the distinct values of the subsort's coloring function over the whole pile,
    sorted, so a key keeps its colour as the view scrolls.

    The coloring functions are the second halves of snuffler's subsort `menudef`
    (pile_viewer.py:1020).
    CNSL's net+sta+loc composite is dot-joined, which sorts identically to
    snuffler's tuple since the codes cannot contain dots.
    """

    def color(trace: ptrace.Trace) -> str:
        if subsort in ("nslc", "nsc_l", "snc_l"):
            return trace.nslc_id[2]
        if subsort == "cnsl":
            return "%s.%s.%s" % (trace.nslc_id[0], trace.nslc_id[1], trace.nslc_id[2])
        return trace.nslc_id[3]

    return sorted({color(trace) for trace in state.traces})


def _box_styles() -> Dict[Tuple[Any, ...], int]:
    """The box tint index per (full_id, deltat): snuffler's `determine_box_styles`.

    As pile_viewer.py:2947 does, walk the pile's traces in `full_id` order and
    step the style index at every discontinuity in (nslc, deltat, adjacency), so
    each contiguous run of files shares a tint and neighbouring runs alternate.
    Keyed exactly as Qt keys `trace_styles`, and cached until the pile changes,
    as Qt recomputes only on a pile change.
    """
    if state.box_styles_generation == state.data_generation:
        return state.box_styles
    traces = sorted(state.traces, key=lambda trace: trace.full_id)
    istyle = 0
    styles: Dict[Tuple[Any, ...], int] = {}
    previous: Optional[ptrace.Trace] = None
    for trace in traces:
        if previous is not None and not (
            previous.nslc_id == trace.nslc_id
            and previous.deltat == trace.deltat
            and abs(previous.tmax - trace.tmin) < cutout.GAP_LAP_TOLERANCE * trace.deltat
        ):
            istyle += 1
        styles[(trace.full_id, trace.deltat)] = istyle
        previous = trace
    state.box_styles = styles
    state.box_styles_generation = state.data_generation
    return styles


def _trace_boxes_by_row(
    tmin: float,
    tmax: float,
    row_index: Dict[Tuple[Any, ...], int],
    perm: Tuple[int, ...],
    sort: str,
    distances_3d: bool,
) -> Dict[int, List[Tuple[float, float, int]]]:
    """Snuffler's trace boxes (`draw_trace_boxes`, pile_viewer.py:2968): the
    pile's data coverage per row over the view window.

    Sourced from the pile rather than the fetched cutout, so a channel
    `min_deltat_allow` declined to fetch still shows where its data lies.
    One box per overlapping trace, or, above Qt's 500-trace threshold, one per
    (row, style) batch spanning first-appended `tmin` to last-appended `tmax`."""
    styles = _box_styles()
    overlapping = [
        trace
        for trace in state.traces
        if trace.overlaps(tmin, tmax) and state.is_trace_visible(trace)
    ]
    overlapping.sort(key=lambda trace: trace.full_id)
    boxes: Dict[int, List[Tuple[float, float, int]]] = {}
    if len(overlapping) < 500:
        for trace in overlapping:
            row = row_index.get(sorting.gather_key(trace, perm, sort, distances_3d))
            if row is None:
                continue
            style = styles.get((trace.full_id, trace.deltat), 0)
            boxes.setdefault(row, []).append((float(trace.tmin), float(trace.tmax), style))
    else:
        merged: "OrderedDict[Tuple[int, int], Tuple[float, float]]" = OrderedDict()
        for trace in overlapping:
            row = row_index.get(sorting.gather_key(trace, perm, sort, distances_3d))
            if row is None:
                continue
            style = styles.get((trace.full_id, trace.deltat), 0)
            bounds = merged.get((row, style))
            merged[(row, style)] = (
                (float(trace.tmin), float(trace.tmax))
                if bounds is None
                else (bounds[0], float(trace.tmax))
            )
        for (row, style), (btmin, btmax) in merged.items():
            boxes.setdefault(row, []).append((btmin, btmax, style))
    return boxes


class _SpanBuild(NamedTuple):
    """One run of samples on its way into a ``ViewSpan``, with what produced it.

    The source trace rides along because the scale is settled after the whole stack
    is known — a span is scaled by *its own* trace's scaling key, which on a
    location-grouped row is not the row's.
    """

    relative_start_s: float
    deltat: float
    npoints: int
    payload: np.ndarray
    source: ptrace.Trace
    # Waterfall only: the strip's own amplitude range, before the global pass.
    waterfall_range: Optional[Tuple[float, float]] = None


def get_view_data(request: view_pb2.ViewParams) -> Tuple[view_pb2.ViewMeta, np.ndarray]:
    """Compute the requested view (wiggle waveforms or a Qt-style waterfall).

    Returns (ViewMeta, ys).
    For wiggles `ys` holds uniform samples at the native or anti-alias-decimated
    rate — or, under the Adaptive visible length, at the bin rate of the min/max
    envelope `_display_envelope` reduced them to.
    For the waterfall it holds per-track normalized amplitude strips for the
    requested track page.
    """
    # Decode the request
    perf_t0 = time.monotonic()
    tmin = float(request.tmin)
    tmax = float(request.tmax)
    visible_tmin = float(request.visible_tmin) if request.HasField("visible_tmin") else tmin
    visible_tmax = float(request.visible_tmax) if request.HasField("visible_tmax") else tmax
    scale_tmin = float(request.scale_tmin) if request.HasField("scale_tmin") else visible_tmin
    scale_tmax = float(request.scale_tmax) if request.HasField("scale_tmax") else visible_tmax
    bins_per_trace = int(request.bins_per_trace)
    track_offset = max(0, int(request.track_offset))
    track_count = max(0, int(request.track_count))
    track_end = track_offset + track_count
    hp: Optional[float] = request.highpass if request.HasField("highpass") else None
    lp: Optional[float] = request.lowpass if request.HasField("lowpass") else None
    sort = request.sort or "name"
    subsort = request.subsort or "nslc"
    rotate = float(request.rotate_deg)
    remove_response = bool(request.remove_response)
    mode = request.mode or "wiggle"
    demean = bool(request.demean)
    degap = bool(request.degap)
    allow_ds = bool(request.allow_downsampling)
    fft = bool(request.fft_filtering)
    lphp = bool(request.bandpass_lphp)
    base = request.scaling_base or "minmax"
    scaling_group = request.scaling_group or "individual"
    gain = float(request.gain or 1.0)
    if gain <= 0.0 or not math.isfinite(gain):
        gain = 1.0
    fix_scale_ranges = bool(request.fix_scale_ranges)
    if (
        fix_scale_ranges
        or scale_tmax <= scale_tmin
        or not math.isfinite(scale_tmin)
        or not math.isfinite(scale_tmax)
    ):
        scale_tmin, scale_tmax = visible_tmin, visible_tmax
    show_scale_ranges = bool(request.show_scale_ranges)
    show_scale_axes = bool(request.show_scale_axes)
    waterfall_absolute = bool(request.waterfall_absolute_amplitudes)
    waterfall_integrate = bool(request.waterfall_integrate)
    waterfall_color_scale = request.waterfall_color_scale or "lin"
    # None is Adaptive: no fetch budget at all, decimated to `bins_per_trace` on the way out.
    adaptive_visible_length = bool(request.adaptive_visible_length)
    visible_length: Optional[float] = None
    if not adaptive_visible_length:
        visible_length = (
            float(request.visible_length) if request.HasField("visible_length") else 20_000.0
        )
    liberal_fetch = bool(request.liberal_fetch)

    # Cut out and process the traces
    state.view_notices.clear()
    traces = cutout.prepare_cutout(
        tmin,
        tmax,
        visible_length=visible_length,
        bins_per_trace=bins_per_trace,
        hp=hp,
        lp=lp,
        demean=demean,
        degap=degap,
        fft=fft,
        lphp=lphp,
        ads=allow_ds,
        remove_resp=remove_response,
        rotate=rotate,
        liberal_fetch=liberal_fetch,
    )
    perf_visible = time.monotonic()

    # Assign the traces to rows
    distances_3d = bool(request.distances_3d)
    perm = sorting.subsort_permutation(subsort)
    rows = sorting.row_universe(sort, subsort, distances_3d)
    row_index = {key: idx for idx, (key, _head) in enumerate(rows)}
    members_by_row: List[List[Tuple[int, ptrace.Trace]]] = [[] for _ in rows]
    for index, trace in enumerate(traces):
        row = row_index.get(sorting.gather_key(trace, perm, sort, distances_3d))
        if row is not None:
            members_by_row[row].append((index, trace))
    # Pile-backed data coverage per row, drawn as Qt's trace boxes.
    # The painter gates boxes on view mode and the Show Boxes option, as Qt's
    # drawit does.
    boxes_by_row: Dict[int, List[Tuple[float, float, int]]] = (
        {}
        if mode == "waterfall"
        else _trace_boxes_by_row(tmin, tmax, row_index, perm, sort, distances_3d)
    )
    perf_prepare = time.monotonic()

    # Build each row's spans
    meta = view_pb2.ViewMeta(
        data_tmin=tmin,
        data_tmax=tmax,
        session_generation=state.session_store.generation,
        view_sequence=request.view_sequence,
        subsort=subsort,
    )
    meta.color_keys.extend(_color_keys(subsort))
    ys_parts: List[np.ndarray] = []
    range_traces: List[ptrace.Trace] = []
    data_ranges: Dict[Any, Tuple[float, float]] = {}
    # Every emitted span with the trace behind it, so the scaling pass below can key
    # each one on its own trace rather than on the row it happens to share.
    scaled_spans: List[Tuple[view_pb2.ViewSpan, ptrace.Trace]] = []
    fast_individual_minmax = base == "minmax" and scaling_group == "individual"
    offset = 0
    view_idx = 0
    wanted_tracks = 0
    sample_tracks = 0
    for row_idx, (_row_key, head) in enumerate(rows):
        # One layering per row, independent of the window, so a pan cannot
        # reorder which co-located trace draws last.
        # Chopped tmins snap to each member's own sample grid, so they cannot
        # lead the key; tmin only orders the gapped segments of one channel.
        members = sorted(
            members_by_row[row_idx],
            key=lambda item: (item[1].nslc_id, item[1].deltat, item[1].tmin),
        )
        wanted_tracks += 1
        # An element the rows merge over becomes '*' in the identity, as in
        # snuffler's `track_patterns`, so a marker stays matched to its row.
        net, sta, loc, cha = (head.nslc_id[i] if i in perm else "*" for i in range(4))
        # The label is snuffler's `plabel` (pile_viewer.py:3521): the gather key
        # space-joined, sort value first, nslc elements in subsort order.
        nslc_part = " ".join(head.nslc_id[i] for i in perm)
        label = nslc_part
        # The head's own location, not the row identity's '*': the gather key
        # carries the distance, so every member of the row shares it.
        annotation = sorting.sort_annotation(
            head.nslc_id[0], head.nslc_id[1], head.nslc_id[2], sort, bool(request.distances_3d)
        )
        if annotation:
            label = "%s %s" % (annotation, nslc_part)
        want_samples = track_offset <= view_idx < track_end
        if want_samples:
            sample_tracks += 1
        view_idx += 1

        spans: List[_SpanBuild] = []
        frequency_bin_count = 0
        waterfall_slices: "List[Tuple[np.ndarray, float, float, ptrace.Trace]]" = []
        for _tr_idx, trace in members:
            # Data and scale ranges for the requested track page only, as
            # snuffler's trace_selector restricts the chopper to
            # `shown_tracks_range` (pile_viewer.py:1798).
            if not want_samples:
                continue
            if trace.ydata is None or not len(trace.ydata):
                continue
            entry = (
                float(trace.tmin),
                float(trace.deltat),
                np.asarray(trace.get_ydata(), dtype=np.float64),
            )

            if mode == "waterfall":
                sliced = _slice_segment(entry, tmin, tmax, 4.0)
                if sliced is None:
                    continue
                yview, ytmin, deltat, _sample_offset = sliced
                # Collected per row and normalised together below, so every
                # segment of the row shares one colour scale and one stride.
                waterfall_slices.append((yview, float(ytmin), float(deltat), trace))
                continue

            data_sliced = _slice_segment(entry, tmin, tmax, 4.0)
            # Four samples of margin on the scale slice too: snuffler's minmax
            # runs over the final chopped traces, which the chop widened by four
            # samples at each end (pile_viewer.py:3894).
            scale_sliced = _slice_segment(entry, scale_tmin, scale_tmax, 4.0)
            if scale_sliced is not None:
                yrange, yrmin, rdt, _range_offset = scale_sliced
                if fast_individual_minmax:
                    # Merged across the segments of one channel, which share a
                    # scaling key.
                    # nanmin/nanmax because `trace.minmax` uses them (trace.py:2549).
                    lo, hi = float(np.nanmin(yrange)), float(np.nanmax(yrange))
                    previous = data_ranges.get(trace.nslc_id)
                    data_ranges[trace.nslc_id] = (
                        (lo, hi)
                        if previous is None
                        else (min(previous[0], lo), max(previous[1], hi))
                    )
                else:
                    range_traces.append(_range_trace(trace.nslc_id, yrmin, rdt, yrange))
            if data_sliced is None:
                continue
            yview, ytmin, deltat, _sample_offset = data_sliced
            # Only Adaptive needs a display envelope: under a fixed visible length
            # the cutout already bounded the fetch and decimated as Qt does.
            if adaptive_visible_length:
                yview, deltat, ytmin = _display_envelope(
                    yview, deltat, ytmin, bins_per_trace, tmax - tmin
                )
            if yview.size:
                spans.append(
                    _SpanBuild(ytmin - tmin, deltat, int(yview.size), yview.astype("<f4"), trace)
                )

        if waterfall_slices:
            strips = waterfall.row_strips(
                [(y, t0, dt) for y, t0, dt, _tr in waterfall_slices],
                bins_per_trace,
                (tmin, tmax),
                waterfall_absolute,
                waterfall_integrate,
                waterfall_color_scale,
            )
            for (strip, t0, dt), (_y, _t0, _dt, trace) in zip(
                strips, waterfall_slices, strict=True
            ):
                if not strip.size:
                    continue
                frequency_bin_count = 1
                waterfall_range = (
                    (float(strip.min()), float(strip.max())) if waterfall_absolute else None
                )
                spans.append(
                    _SpanBuild(t0 - tmin, dt, int(strip.size), strip, trace, waterfall_range)
                )
            if waterfall_absolute and spans:
                # One colour range per row, spanning its segments: the clip
                # sliders are relative to each track's own range, and a page-wide
                # range would recolour a track when a louder neighbour scrolls in.
                row_lo = min(build.waterfall_range[0] for build in spans if build.waterfall_range)
                row_hi = max(build.waterfall_range[1] for build in spans if build.waterfall_range)
                if row_hi <= row_lo:
                    row_hi = row_lo + 1e-12
                spans = [
                    build._replace(waterfall_range=(row_lo, row_hi))
                    if build.waterfall_range
                    else build
                    for build in spans
                ]
        # A row without spans is kept in both modes: the frontend treats
        # `meta.tracks` as the full row universe and derives the scroll range and
        # the next page request from its length.
        t = meta.tracks.add()
        t.net, t.sta, t.loc, t.cha = net, sta, loc, cha
        t.label = label
        t.sort_annotation = annotation
        # Boxes only for the requested track page, like the spans: rows outside
        # it are stack placeholders the painter never shows.
        if want_samples:
            for btmin, btmax, bstyle in boxes_by_row.get(row_idx, []):
                box = t.boxes.add()
                box.tmin = btmin
                box.tmax = btmax
                box.style = bstyle
        if mode == "waterfall":
            t.frequency_bin_count = int(frequency_bin_count)
        for build in spans:
            span = t.spans.add()
            span.relative_start_s = build.relative_start_s
            span.deltat = build.deltat
            span.count = build.npoints
            span.offset = int(offset)
            span.loc = build.source.nslc_id[2]
            span.ymax = 1.0
            span.yscale = 1.0
            if mode == "waterfall":
                span.ymin, span.ymax = build.waterfall_range or (
                    (0.0, 1.0) if waterfall_absolute else (-1.0, 1.0)
                )
            scaled_spans.append((span, build.source))
            ys_parts.append(np.ascontiguousarray(build.payload, dtype="<f4"))
            offset += int(build.payload.size)

    perf_loop = time.monotonic()

    # Scale the spans; the waterfall's colour range was settled per row above
    if mode != "waterfall" and (range_traces or data_ranges):
        if range_traces:
            mode_param, outer_mode = _scaling_base_modes(base)

            def scaling_key(trace: ptrace.Trace) -> Any:
                return _scaling_key(trace, scaling_group)

            trace_minmax = cast(_TraceRangeFunc, getattr(ptrace, "minmax"))
            raw_ranges = trace_minmax(
                range_traces,
                key=scaling_key,
                mode=mode_param,
                outer_mode=outer_mode,
            )
            data_ranges.update(
                {key: (float(lo), float(hi)) for key, (lo, hi) in raw_ranges.items()}
            )
        # Snuffler's freeze (pile_viewer.py:3281): remember the live ranges while
        # the option is off and serve the remembered ones while it is on.
        # A copy is stored where Qt keeps an alias, so ':scaling' overrides
        # applied below cannot leak into what gets remembered.
        if fix_scale_ranges:
            data_ranges.update(state.fixed_data_ranges)
        else:
            state.fixed_data_ranges.clear()
            state.fixed_data_ranges.update(data_ranges)

        # ':scaling' overrides land after minmax + fix-range and before make_scale
        # divides by gain, exactly Qt's order (pile_viewer apply_scaling_hooks).
        _apply_scaling_hooks(data_ranges)

        auto_scaler = cast(
            Callable[..., _AutoScalerLike], getattr(import_module("pyrocko.plot"), "AutoScaler")
        )
        yscaler = auto_scaler(
            no_exp_interval=(-3, 2),
            approx_ticks=15.0,
            snap=(show_scale_ranges or show_scale_axes) and not show_scale_axes,
        )
        # Per span, keyed on the span's own trace: snuffler projects per
        # (track, scaling key), so two sensors sharing a location-grouped row are
        # each drawn against their own amplitude scale.
        for span, source in scaled_spans:
            dymin, dymax = data_ranges.get(_scaling_key(source, scaling_group), (0.0, 1.0))
            # An equal range is a flatline that AutoScaler pads to ±1, as snuffler
            # does.
            # A non-finite range comes from an all-NaN slice, where Qt draws a
            # nonsense axis; that is not reproduced.
            if not math.isfinite(dymin) or not math.isfinite(dymax) or dymax < dymin:
                dymin, dymax = 0.0, 1.0
            ymin, ymax, ytick_step = yscaler.make_scale((dymin / gain, dymax / gain))
            ymin = float(ymin)
            ymax = float(ymax)
            half = max(0.5 * (ymax - ymin), 1e-12)
            span.ymin = ymin
            span.ymax = ymax
            span.ymean = 0.5 * (ymin + ymax)
            span.yscale = max(gain * half, 1e-12)
            # The tick increment and label exponent let 'Show Scale Axes' draw
            # snuffler's exact ticks (pile_viewer.py:3357).
            span.ytick_step = float(ytick_step)
            span.ytick_exponent = (
                int(yscaler.make_exp(ytick_step)) if float(ytick_step) > 0.0 else 0
            )

    perf_scale = time.monotonic()

    # Assemble the payload
    ys = (
        np.ascontiguousarray(np.concatenate(ys_parts), dtype="<f4")
        if ys_parts
        else np.zeros(0, dtype="<f4")
    )
    if mode == "waterfall" and ys.size:
        waterfall.colorize(meta, ys, request)
    meta.notices.extend(state.view_notices)
    perf_concat = time.monotonic()
    _maybe_log_perf(
        mode,
        tmin,
        tmax,
        bins_per_trace,
        track_offset,
        track_count,
        len(traces),
        wanted_tracks,
        sample_tracks,
        int(ys.size),
        perf_t0,
        perf_visible,
        perf_prepare,
        perf_loop,
        perf_scale,
        perf_concat,
        time.monotonic(),
    )
    return meta, ys


def _apply_scaling_hooks(data_ranges: Dict[Any, Tuple[float, float]]) -> None:
    """Apply the ':scaling' overrides to the data ranges, as ``apply_scaling_hooks`` does.

    For each hook in sorted pattern order, replace lo/hi where the bound is not
    None.
    The '_' key is the rule for every track; a real pattern is matched via
    ``match_nslc`` against the scaling key (an nslc tuple or a channel string).
    ``None`` keys (common-scale mode) have no channel identity and are skipped.
    """
    if not state.scaling_hooks:
        return
    from pyrocko.util import match_nslc

    for pattern in sorted(state.scaling_hooks):
        vmin, vmax = state.scaling_hooks[pattern]
        for key in list(data_ranges):
            if pattern != "_":
                if key is None or not match_nslc(pattern, key):
                    continue
            lo, hi = data_ranges[key]
            if vmin is not None:
                lo = vmin
            if vmax is not None:
                hi = vmax
            data_ranges[key] = (lo, hi)
