"""The waterfall view: per-row amplitude strips and their colour mapping."""

from __future__ import annotations

import math
from typing import Callable, Dict, List, Tuple

import numpy as np
from snuffler.bridge.v1 import view_pb2


def row_strips(
    slices: "List[Tuple[np.ndarray, float, float]]",
    bins_per_trace: int,
    window: Tuple[float, float],
    want_abs: bool,
    want_integrate: bool,
    color_scale: str = "lin",
) -> "List[Tuple[np.ndarray, float, float]]":
    """Snuffler-style waterfall strips for one track row: normalized amplitude
    over time, on one shared amplitude scale and one shared time resolution.

    Snuffler normalises per image row, a whole trace there
    (``pile_viewer_waterfall.py:148``).
    A row here is a track that can carry several segments, so the peak and the
    decimation stride are shared across every strip of the row.

    Takes ``(yview, t0, deltat)`` per segment; returns ``(strip, t0, dt)`` per
    segment, strips as little-endian float32.
    ``color_scale`` (``lin`` | ``sqrt`` | ``log``) compresses the envelope before
    colour-mapping; snuffler only has ``lin``.
    """
    from scipy import signal as sps

    if not slices:
        return []
    # One target resolution for the row: the coarsest any piece would have
    # chosen on its own, derived from the window so a pure pan cannot change it.
    window_len = max(0.0, float(window[1]) - float(window[0]))
    target_dt = max(
        float(deltat) * max(1, int(window_len / (float(deltat) * max(1, 2 * int(bins_per_trace)))))
        for _y, _t0, deltat in slices
    )
    prepared: "List[Tuple[np.ndarray, float, float]]" = []
    for y, t0, deltat in slices:
        step = max(1, int(round(target_dt / float(deltat))))
        # Anchor the take to the trace's absolute sample grid, as
        # `view._display_envelope` anchors its bins and Qt does
        # (pile_viewer_waterfall.py:124), so a pan slides the strip.
        base = int(math.floor(float(t0) / float(deltat) + 0.5))
        offset = (-base) % step
        data = np.ascontiguousarray(np.asarray(y, dtype=np.float64)[offset::step])
        if want_integrate:
            data = np.cumsum(data) * float(deltat)
        prepared.append((data, float(t0) + offset * float(deltat), float(deltat) * step))

    peak = max(
        (float(np.abs(data).max()) if data.size else 0.0 for data, _t0, _dt in prepared),
        default=0.0,
    )
    out: "List[Tuple[np.ndarray, float, float]]" = []
    for data, t0, dt in prepared:
        if peak > 0.0:
            data = data / peak
        if want_abs and data.size:
            hilbert: Callable[[np.ndarray], np.ndarray] = getattr(sps, "hilbert")
            data = np.abs(hilbert(data))
            if color_scale == "sqrt":
                data = np.sqrt(data)
            elif color_scale == "log":
                # map [0, 1] -> [0, 1] with log compression (log10(1)=0, log10(10)=1).
                data = np.log10(data * 9.0 + 1.0)
        out.append((np.ascontiguousarray(data, dtype="<f4"), t0, dt))
    return out


_colormap_luts: Dict[str, np.ndarray] = {}


def _colormap_lut(name: str) -> np.ndarray:
    """The 256x3 uint8 RGB lookup table of a waterfall colormap, cached per name.

    Sampled from matplotlib, so the colours are byte-identical to the Qt
    snuffler, which colours its waterfall with the same matplotlib colormaps.
    """
    key = name or "viridis"
    lut = _colormap_luts.get(key)
    if lut is None:
        import matplotlib as mpl

        try:
            cmap = mpl.colormaps[key]
        except KeyError:
            cmap = mpl.colormaps["viridis"]
        xs = np.linspace(0.0, 1.0, 256)
        rgb = np.asarray(cmap(xs))[:, :3] * 255.0
        lut = np.ascontiguousarray(rgb.round().clip(0.0, 255.0).astype(np.uint8))
        _colormap_luts[key] = lut
    return lut


def _strip_rgba(
    strip: np.ndarray,
    lo: float,
    hi: float,
    clip_lo: float,
    clip_hi: float,
    reverse: bool,
    lut: np.ndarray,
) -> np.ndarray:
    """Colour-map one waterfall amplitude strip to RGBA, packed one pixel per
    float32 so it rides the same zero-copy ``ys`` buffer.

    Clips the ``[lo, hi]`` band by ``clip_lo/clip_hi``, optionally reverses, then
    looks up the LUT.
    Symmetry is not handled here: in snuffler it constrains the clip pair
    (`set_symetry`, gui/util.py:817), which the frontend enforces on the sliders."""  # spellcheck:ignore-line
    span = max(hi - lo, 1e-6)
    band_lo = lo + clip_lo * span
    band_hi = lo + clip_hi * span
    rng = max(band_hi - band_lo, 1e-6)
    tn = np.clip((strip.astype(np.float64) - band_lo) / rng, 0.0, 1.0)
    if reverse:
        tn = 1.0 - tn
    # matplotlib indexes its 256-entry LUT with int(t * 256) clipped to 255.
    idx = np.minimum((tn * 256.0).astype(np.int64), 255)
    rgba = np.empty((idx.size, 4), dtype=np.uint8)
    rgba[:, :3] = lut[idx]
    rgba[:, 3] = 255
    return rgba.reshape(-1).view("<f4")


def colorize(meta: view_pb2.ViewMeta, ys: np.ndarray, request: view_pb2.ViewParams) -> None:
    """Replace each waterfall track's amplitude strip in ``ys`` with its colours, in place.

    The colours are packed as float32, one pixel per value.
    Runs after the amplitude ranges are settled, so every strip shares the same
    colour scale.
    """
    lut = _colormap_lut(request.waterfall_colormap)
    clip_lo = float(request.waterfall_clip_low)
    clip_hi = float(request.waterfall_clip_high)
    if not clip_hi > clip_lo:
        clip_lo, clip_hi = 0.0, 1.0
    reverse = bool(request.waterfall_reverse_colormap)
    for t in meta.tracks:
        if not t.frequency_bin_count:
            continue
        for span in t.spans:
            off = int(span.offset)
            cnt = int(span.count)
            strip = ys[off : off + cnt]
            if not cnt or strip.size != cnt:
                continue
            ys[off : off + cnt] = _strip_rgba(
                strip,
                float(span.ymin),
                float(span.ymax),
                clip_lo,
                clip_hi,
                reverse,
                lut,
            )
