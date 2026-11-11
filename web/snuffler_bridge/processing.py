"""Trace processing as snuffler applies it before drawing.

Filters, instrument-response removal and horizontal rotation, each following
the branch of ``PileViewerMain.prepare_cutout2`` it ports.
"""

from __future__ import annotations

import math
from typing import Any, List, Optional, Tuple

import numpy as np
from pyrocko import trace as ptrace

from . import state
from .nslc import NslcId


def decon_freq_limits(
    deltat: float, data_len: int, hp: Optional[float], lp: Optional[float]
) -> Tuple[float, float, float, float]:
    """Cosine-taper frequency limits for response deconvolution, exactly as
    snuffler's ``PileViewer.get_freq_limits``: anchored to the current highpass /
    lowpass passband, clamped to a Nyquist-safe band and a data-length abs-min."""
    nyquist_safe = (1.0 / (2.0 * deltat)) * 0.9
    abs_min = 0.01 / (max(1, data_len) * deltat)
    taper_width = 0.5
    if hp:
        f2 = min(max(abs_min, hp), nyquist_safe)
        f1 = min(max(abs_min, f2 * (1.0 - taper_width)), nyquist_safe)
    else:
        f1 = f2 = abs_min
    if lp:
        f3 = min(lp, nyquist_safe)
        f4 = min(f3 * (1.0 + taper_width), nyquist_safe)
    else:
        f3 = f4 = nyquist_safe
    return f1, f2, f3, f4


def instrument_response(nslc_id: NslcId, tmin: float, tmax: float) -> Any:
    """The response for `nslc_id` over the view window, resolved as snuffler
    resolves it (`get_instrument_response`, pile_viewer.py:4468).

    The first StationXML document that resolves wins, a document that raises is
    skipped, and a channel that resolves nowhere is reported once.
    """
    response = None
    for station_xml in state.station_xmls:
        try:
            response = station_xml.get_pyrocko_response(nslc_id, timespan=(tmin, tmax))
            break
        except Exception:  # noqa: BLE001 — try the next document, exactly as Qt does
            continue
    if response is None:
        if nslc_id not in state.warned_no_response:
            state.warned_no_response.add(nslc_id)
            state.add_view_notice(
                "no instrument response information for %s.%s.%s.%s" % tuple(nslc_id)
            )
        return None
    return response


def remove_response(
    trace: ptrace.Trace,
    enabled: bool,
    hp: Optional[float] = None,
    lp: Optional[float] = None,
    tmin: Optional[float] = None,
    tmax: Optional[float] = None,
) -> ptrace.Trace:
    """Deconvolve the instrument response with snuffler's frequency limits.

    A trace without a matching response passes through with a view notice.
    The response is resolved over the view window (``tmin``/``tmax``), not the
    trace's own span, as the reference does (pile_viewer.py:3845), so a trace
    crossing a metadata epoch boundary resolves for the epoch on screen.

    The result is in the instrument's own input unit (velocity for a broadband),
    as snuffler's ``get_instrument_response`` produces; ``fake_input_units='M'``
    would differ from Qt by a factor of i*omega.

    Takes the already demeaned and filtered segment, the order snuffler
    restitutes in: the frequency limits derive from `data_len`, so a differently
    extended trace would taper differently."""
    if not enabled:
        return trace
    span_tmin = float(trace.tmin) if tmin is None else float(tmin)
    span_tmax = float(trace.tmax) if tmax is None else float(tmax)
    response = instrument_response(trace.nslc_id, span_tmin, span_tmax)
    if response is None:
        return trace
    freqlimits = decon_freq_limits(float(trace.deltat), int(trace.data_len()), hp, lp)
    try:
        return trace.transfer(
            freqlimits=freqlimits,
            transfer_function=response,
            cut_off_fading=True,
            demean=True,
            invert=True,
        )
    except Exception as exc:  # noqa: BLE001 — drawn unrestituted, but never silently
        state.add_view_notice("restitution failed for %s.%s.%s.%s: %s" % (*trace.nslc_id, exc))
        return trace


def _fft_filter(
    segment: ptrace.Trace,
    hp: Optional[float],
    lp: Optional[float],
    remove_resp: bool = False,
    resp_tmin: float = 0.0,
    resp_tmax: float = 0.0,
) -> None:
    """snuffler's 'FFT filtering' branch (pile_viewer.py:3696): linear-detrend,
    multiply the (pow2-padded) spectrum by one combined transfer function, irfft,
    then retrend.

    The transfer function folds the order-4 Butterworth low/high responses
    together with, when Deconvolution is on, the cosine taper and the inverted
    instrument response.
    With Deconvolution on the fitted trend is not re-added (pile_viewer.py:3794),
    even when no response was found for the channel."""
    from pyrocko import response

    it = np.arange(segment.data_len(), dtype=float)
    y = np.asarray(segment.ydata, dtype=np.float64)
    coeffs = np.polyfit(it, y, 1)
    slope, offset = float(coeffs[0]), float(coeffs[1])
    segment.set_ydata(y - slope * it - offset)
    freqs, fdata = segment.spectrum(pad_to_pow2=True, tfade=None)
    nfreqs = int(fdata.size)
    # Responses are epoch-dependent, so a fingerprint of the resolved response
    # rides the cache key: two windows straddling a StationXML epoch boundary
    # must not share a transfer function.
    # It hashes the serialized response because each resolve builds a fresh
    # object.
    inst_resp = instrument_response(segment.nslc_id, resp_tmin, resp_tmax) if remove_resp else None
    key = (
        float(segment.deltat),
        nfreqs,
        tuple(segment.nslc_id) if remove_resp else None,
        None if inst_resp is None else hash(inst_resp.dump()),
        float_key(hp),
        float_key(lp),
        state.response_generation,
    )
    tf = state.transfer_function_cache.get(key)
    if tf is None:
        resps = []
        if lp is not None:
            resps.append(response.ButterworthResponse(order=4, corner=float(lp), type="low"))
        if hp is not None:
            resps.append(response.ButterworthResponse(order=4, corner=float(hp), type="high"))
        if resps:
            tf = response.MultiplyResponse(resps).evaluate(freqs)
        else:
            tf = np.ones(nfreqs, dtype=complex)
        if remove_resp:
            if inst_resp is not None:
                f1, f2, f3, f4 = decon_freq_limits(
                    float(segment.deltat), int(segment.data_len()), hp, lp
                )
                taper = ptrace.CosTaper(f1, f2, f3, f4)
                taper_vals = np.ones(nfreqs)
                taper(taper_vals, float(freqs[0]), float(freqs[1] - freqs[0]))
                inst_tf = inst_resp.evaluate(freqs)
                with np.errstate(divide="ignore", invalid="ignore"):
                    inv_inst_tf = np.where(np.abs(inst_tf) > 0.0, 1.0 / inst_tf, 0.0)
                tf = tf * taper_vals * inv_inst_tf
        state.transfer_function_cache[key] = tf
    filtered = np.fft.irfft(fdata * tf)[: segment.data_len()]
    if not remove_resp:
        segment.set_ydata(it * slope + filtered + offset)
    else:
        segment.set_ydata(filtered)


def filter_segment(
    segment: ptrace.Trace,
    hp: Optional[float],
    lp: Optional[float],
    fft: bool,
    lphp: bool,
    remove_resp: bool = False,
    resp_tmin: float = 0.0,
    resp_tmax: float = 0.0,
) -> None:
    """Apply the main filter to a chopped trace, honoring the FFT-filtering and
    bandpass-as-low+highpass options.

    Filter corners at or above Nyquist are skipped (snuffler's ``< 0.5/deltat``
    guards).
    With FFT filtering on, Deconvolution is folded into the same pass
    (pile_viewer.py:3699) and snuffler skips the separate restitution
    (pile_viewer.py:3840), so the caller must not restitute again.
    The lowpass/highpass stages pass `demean=False`, as Qt does
    (pile_viewer.py:3821), because those pyrocko filters otherwise demean on
    their own even with Demean off.
    A filter that throws rolls the segment back to its unprocessed samples and
    reports the failure with the view."""
    original = segment.ydata
    try:
        if fft:
            if hp is not None or lp is not None or remove_resp:
                _fft_filter(segment, hp, lp, remove_resp, resp_tmin, resp_tmax)
            return
        fmax = 0.5 / float(segment.deltat)
        if not lphp and hp is not None and lp is not None and hp < lp < fmax and hp < fmax:
            segment.bandpass(2, float(hp), float(lp))
        else:
            if lp is not None and lp < fmax:
                segment.lowpass(4, float(lp), demean=False)
            if hp is not None and (lp is None or hp < lp) and hp < fmax:
                segment.highpass(4, float(hp), demean=False)
    except Exception as exc:  # noqa: BLE001 — drawn unfiltered, but never silently
        # Either branch can fail half-way (after the detrend, or between the
        # lowpass and the highpass).
        # Every pyrocko filter replaces ydata rather than mutating it, so the
        # saved reference is exactly the unprocessed array.
        segment.set_ydata(original)
        state.add_view_notice("filter failed for %s.%s.%s.%s: %s" % (*segment.nslc_id, exc))


def float_key(value: Optional[float]) -> Optional[float]:
    """A float rounded for use in a cache key, so float noise cannot miss the cache."""
    return None if value is None else round(float(value), 9)


def adequate_tpad(hp: Optional[float], lp: Optional[float]) -> float:
    """`get_adequate_tpad` (pile_viewer.py:3576) minus the snuffling-hook term,
    which this port does not run: padding for the current filter corners only."""
    tpad = 0.0
    for f in (hp, lp):
        if f is not None:
            tpad = max(tpad, 2.0 / float(f))
    return tpad


def rotate_pairs_inplace(processed_traces: List[ptrace.Trace], rotate_deg: float) -> None:
    """Rotate horizontal pairs in place, exactly as `prepare_cutout2` does
    (pile_viewer.py:3865).

    Pairs the N/E or 1/2 channels of one net.sta.loc whose sampling and start
    agree within tolerance (`deltat` to 0.1 %, `tmin` to 1 % of a sample) at equal
    length.
    """
    phi = rotate_deg / 180.0 * math.pi
    cphi = math.cos(phi)
    sphi = math.sin(phi)
    for a in processed_traces:
        for b in processed_traces:
            if (
                a.network == b.network
                and a.station == b.station
                and a.location == b.location
                and (
                    (a.channel.lower().endswith("n") and b.channel.lower().endswith("e"))
                    or (a.channel.endswith("1") and b.channel.endswith("2"))
                )
                and abs(a.deltat - b.deltat) < a.deltat * 0.001
                and abs(a.tmin - b.tmin) < a.deltat * 0.01
                and len(a.get_ydata()) == len(b.get_ydata())
            ):
                aydata = a.get_ydata() * cphi + b.get_ydata() * sphi
                bydata = -a.get_ydata() * sphi + b.get_ydata() * cphi
                a.set_ydata(aydata)
                b.set_ydata(bydata)
