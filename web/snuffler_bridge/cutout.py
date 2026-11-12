"""The processed cutout: the traces snuffler would draw for a time window."""

from __future__ import annotations

import math
from typing import Any, FrozenSet, List, Optional, Tuple

import numpy as np
from pyrocko import trace as ptrace

from . import processing, state

# snuffler's `gap_lap_tolerance`: the largest gap it will interpolate across and the
# largest overlap it will trim, both in samples.
GAP_LAP_TOLERANCE = 5.0

# `accessor_id` for the view's pile access bookkeeping (Qt passes `id(self)`).
_VIEW_ACCESSOR = "view"


def prepare_cutout(
    tmin: float,
    tmax: float,
    *,
    visible_length: Optional[float],
    bins_per_trace: int,
    hp: Optional[float],
    lp: Optional[float],
    demean: bool,
    degap: bool,
    fft: bool,
    lphp: bool,
    ads: bool,
    remove_resp: bool,
    rotate: float,
    liberal_fetch: bool,
    full_segments: bool = False,
    selected_nslcs: Optional[FrozenSet[Tuple[str, str, str, str]]] = None,
) -> List[ptrace.Trace]:
    """The traces snuffler would draw for this window: a transcription of
    `PileViewerMain.prepare_cutout2` (pile_viewer.py:3590) running the same
    pyrocko code Qt calls.

    Stages, in the reference's order:

    - derive `min_deltat_allow`, the finest sampling the window fetches
      (`visible_length` is None for the Adaptive policy, which drops that floor
      and decimates the payload instead);
    - optionally snap the fetch window to a power-of-two grid (Liberal Fetch);
    - chop a tpad-padded window through `pile.chopper`, which degaps;
    - demean, filter (optionally after anti-aliased downsampling), restitute
      and rotate horizontal pairs;
    - chop back to the requested window plus four samples per side.

    The processing extent is the padded fetch window, not the whole trace, as
    in snuffler.
    Cached as Qt caches it (pile_viewer.py:3632): one state vector, reused when
    the new window is contained in the cached one and every other member matches.
    """
    p = state.pile()
    if p.is_empty():
        return []

    # An integral starts at each continuous source segment, never at a moving crop.
    # Process only the requested rows, at native sampling, and cache that fixed extent.
    if full_segments:
        tmin, tmax = float(p.tmin), float(p.tmax)
        visible_length = None
        ads = False
        liberal_fetch = False

    # The finest sampling the window fetches
    tsee = tmax - tmin
    if visible_length is None:
        # Adaptive: no channel is refused, so there is no floor to derive.
        # 'Allow Downsampling' aims at the canvas resolution, two samples per
        # pixel as a min/max envelope emits.
        min_deltat_allow = 0.0
        min_deltat_wo_decimate = tsee / max(1.0, 2.0 * float(bins_per_trace))
    else:
        nmax = float(visible_length) if visible_length > 0.0 else 20_000.0
        min_deltat_wo_decimate = tsee / nmax
        min_deltat_w_decimate = min_deltat_wo_decimate / 32.0

        min_deltat_allow = min_deltat_wo_decimate
        if lp is not None:
            target_deltat_lp = 0.25 / float(lp)
            if target_deltat_lp > min_deltat_wo_decimate:
                min_deltat_allow = min_deltat_w_decimate

        # A power of e, not of ten: likely an upstream slip for `log10`, kept
        # because it is the reference behaviour.
        min_deltat_allow = math.exp(int(math.floor(math.log(min_deltat_allow))))

    # The fetch window, snapped to a power-of-two grid under Liberal Fetch
    requested_tmin = tmin
    requested_tmax = tmax
    if liberal_fetch:
        tlen = float(ptrace.nextpow2((tmax - tmin) * 1.5))
        tmin = math.floor(tmin / tlen) * tlen
        tmax = math.ceil(tmax / tlen) * tlen

    tpad = min(processing.adequate_tpad(hp, lp), tsee * 3.0)

    # The anti-alias downsample target only belongs in the key where the loop
    # below actually runs: under Adaptive it follows `bins_per_trace`, and keying on it
    # unconditionally would throw the processed traces away on every resize.
    decimate_ran = ads and lp is not None and not fft
    decimate_key = processing.float_key(min_deltat_wo_decimate) if decimate_ran else None

    # The cache key: snuffler's state vector
    key = (
        tmin,
        tmax,
        tpad,
        tuple(state.hidden_patterns),
        None if state.quick_filter_patterns is None else tuple(state.quick_filter_patterns),
        degap,
        demean,
        processing.float_key(lp),
        processing.float_key(hp),
        fft,
        lphp,
        min_deltat_allow,
        decimate_key,
        processing.float_key(rotate),
        ads,
        remove_resp,
        state.data_generation,
        state.response_generation,
        full_segments,
        selected_nslcs,
    )

    if (
        state.cutout_key is not None
        and state.cutout_traces is not None
        and state.cutout_key[0] <= key[0]
        and key[1] <= state.cutout_key[1]
        and key[2:] == state.cutout_key[2:]
    ):
        # Cache hit
        processed_traces = state.cutout_traces
        for message in state.cutout_notices:
            state.add_view_notice(message)
    else:
        # Cache miss: chop and process
        # A fresh processing pass warns once per channel again: the previous
        # pass's dedup must not silence a condition that still holds.
        state.warned_no_response.clear()
        notices_before = len(state.view_notices)
        processed_traces = []
        if p.deltatmax is not None and p.deltatmax >= min_deltat_allow:

            def select_group(gr: Any) -> bool:
                return bool(gr.deltatmax >= min_deltat_allow)

            def select_trace(trace: ptrace.Trace) -> bool:
                return (
                    trace.deltat >= min_deltat_allow
                    and state.is_trace_visible(trace)
                    and (selected_nslcs is None or trace.nslc_id in selected_nslcs)
                )

            for traces in p.chopper(
                tmin=tmin,
                tmax=tmax,
                tpad=tpad,
                want_incomplete=True,
                degap=degap,
                maxgap=GAP_LAP_TOLERANCE,
                maxlap=GAP_LAP_TOLERANCE,
                keep_current_files_open=True,
                group_selector=select_group,
                trace_selector=select_trace,
                accessor_id=_VIEW_ACCESSOR,
                snap=(math.floor, math.ceil),
                include_last=True,
            ):
                if demean:
                    for trace in traces:
                        if trace.meta and trace.meta.get("tabu", False):
                            continue
                        y = trace.get_ydata()
                        trace.set_ydata(y - np.mean(y))

                for trace in traces:
                    if not (trace.meta and trace.meta.get("tabu", False)):
                        if fft:
                            processing.filter_segment(
                                trace, hp, lp, True, lphp, remove_resp, tmin, tmax
                            )
                        else:
                            if ads and lp is not None:
                                while trace.deltat < min_deltat_wo_decimate:
                                    trace.downsample(2, demean=False)
                            processing.filter_segment(trace, hp, lp, False, lphp)
                    processed_traces.append(trace)

            # Outside the chopper loop: the inverse transfer is not idempotent,
            # so running it per batch over the accumulated list would restitute
            # earlier batches repeatedly if the chopper ever yields more than once.
            if remove_resp and not fft:
                processed_traces = [
                    processing.remove_response(trace, True, hp, lp, tmin, tmax)
                    for trace in processed_traces
                ]

        if rotate != 0.0:
            processing.rotate_pairs_inplace(processed_traces, rotate)

        state.cutout_key = key
        state.cutout_traces = processed_traces
        state.cutout_notices[:] = state.view_notices[notices_before:]

    if full_segments:
        return processed_traces

    # Chop back to the requested window plus four samples per side
    chopped_traces = []
    for trace in processed_traces:
        chop_tmin = requested_tmin - trace.deltat * 4
        chop_tmax = requested_tmax + trace.deltat * 4
        try:
            ctrace = trace.chop(chop_tmin, chop_tmax, inplace=False)
        except ptrace.NoData:
            continue
        # Qt drops any chopped trace shorter than two samples
        # (pile_viewer.py:3905): a one-sample overlap becomes nothing, not a row.
        if ctrace.data_len() < 2:
            continue
        chopped_traces.append(ctrace)

    return chopped_traces
