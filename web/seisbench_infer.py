"""SeisBench ONNX inference in Pyodide, driven by onnxruntime-web.

The exported ONNX graph runs in JavaScript (`web/bridge_worker.ts`) so it can use
the browser's WebGPU implementation. This module keeps the SeisBench pre/post
pipeline in Python/numpy because torch and seisbench do not import in Pyodide.
The functions below are ports from SeisBench 0.11.7 and cite the source function
they mirror.
"""

from __future__ import annotations

import json
import math
from collections import deque
from typing import Any, Callable, NamedTuple, cast

import numpy as np
from numpy.typing import NDArray

SEISBENCH_VERSION = "0.11.7"

_MANIFEST: dict[str, Any] | None = None


class Annotation(NamedTuple):
    data: NDArray[np.float64]
    sampling_rate: float
    start_offset: float


async def load_manifest() -> dict[str, Any]:
    global _MANIFEST
    if _MANIFEST is None:
        import js
        from pyodide.http import pyfetch

        url = str(js.URL.new("assets/seisbench_models.json", js.location.href).href)
        resp = await pyfetch(url)
        if not resp.ok:
            raise RuntimeError("fetch %s: HTTP %s" % (url, resp.status))
        _MANIFEST = cast(dict[str, Any], json.loads(str(await resp.string())))
        if _MANIFEST.get("seisbench_version") != SEISBENCH_VERSION:
            raise RuntimeError(
                "SeisBench manifest version %r does not match runtime port %r"
                % (_MANIFEST.get("seisbench_version"), SEISBENCH_VERSION)
            )
    return _MANIFEST


def model_spec(manifest: dict[str, Any], model: str, weights: str) -> dict[str, Any]:
    for spec in cast(list[dict[str, Any]], manifest.get("models", [])):
        if spec.get("model") == model and spec.get("weights") == weights:
            return spec
    available = ", ".join(
        "%s/%s" % (m.get("model", "?"), m.get("weights", "?"))
        for m in cast(list[dict[str, Any]], manifest.get("models", []))
    )
    raise ValueError("unknown SeisBench model %s/%s (available: %s)" % (model, weights, available))


async def annotate(
    model_url: str,
    data: NDArray[np.float32],
    spec: dict[str, Any],
    progress: Callable[[], None] | None = None,
    progress_label: str = "SeisBench picker",
) -> Annotation:
    output_type = str(spec["output_type"])
    if output_type == "array":
        return await _annotate_array(model_url, data, spec, progress, progress_label)
    if output_type == "point":
        return await _annotate_point(model_url, data, spec, progress, progress_label)
    raise NotImplementedError("unsupported SeisBench output_type %r" % output_type)


def batch_count(data: NDArray[np.float32], spec: dict[str, Any]) -> int:
    batch_size = int(spec.get("batch_size", 256))
    n_windows = _window_count(data, spec)
    if n_windows == 0:
        return 0
    return int(math.ceil(n_windows / batch_size))


def labels(spec: dict[str, Any]) -> list[str]:
    return [str(label) for label in cast(list[Any], spec["labels"])]


def default_threshold(spec: dict[str, Any], phase: str) -> float:
    default_args = cast(dict[str, Any], spec.get("default_args", {}))
    value = default_args.get("%s_threshold" % phase)
    if value is not None:
        return float(value)
    return float(spec.get("phase_threshold", 0.3))


def phase_channel(spec: dict[str, Any], phase: str) -> int:
    return labels(spec).index(phase)


# Port of seisbench.models.PhaseNet.annotate_batch_pre,
# seisbench.models.EQTransformer.annotate_batch_pre, and
# seisbench.models.GPD.annotate_batch_pre (SeisBench 0.11.7).
def _annotate_batch_pre(
    batch: NDArray[np.float32],
    spec: dict[str, Any],
) -> NDArray[np.float32]:
    model = str(spec["model"])
    norm = str(spec.get("norm", ""))
    norm_amp_per_comp = bool(spec.get("norm_amp_per_comp", False))
    norm_detrend = bool(spec.get("norm_detrend", False))

    out = batch - batch.mean(axis=-1, keepdims=True)
    if norm_detrend:
        out = _detrend(out)

    if model == "GPD":
        return np.ascontiguousarray(out, dtype=np.float32)

    if norm_amp_per_comp:
        peak = np.max(np.abs(out), axis=-1, keepdims=True)
        out = out / (peak + 1e-10)
    elif norm == "std":
        axis: int | tuple[int, int] = -1 if model == "PhaseNet" else (-1, -2)
        std = np.std(out, axis=axis, keepdims=True, ddof=1)
        out = out / (std + 1e-10)
    elif norm == "peak":
        peak = np.max(np.abs(out), axis=-1, keepdims=True)
        out = out / (peak + 1e-10)

    if model == "EQTransformer":
        tap = 0.5 * (1.0 + np.cos(np.linspace(np.pi, 2.0 * np.pi, 6, dtype=np.float32)))
        out[:, :, :6] *= tap
        out[:, :, -6:] *= tap[::-1]

    return np.ascontiguousarray(out, dtype=np.float32)


def _detrend(batch: NDArray[np.float32]) -> NDArray[np.float32]:
    n = batch.shape[-1]
    if n < 2:
        return batch
    x = np.linspace(-1.0, 1.0, n, dtype=np.float32)
    denom = float(np.sum(x * x))
    slope = np.sum(batch * x, axis=-1, keepdims=True) / denom
    out: NDArray[np.float32] = np.asarray(batch - slope * x, dtype=np.float32)
    return out


# Port of seisbench.models.base.WaveformModel._cut_fragments_array
# (SeisBench 0.11.7).
def _array_offsets(n_samples: int, in_samples: int, overlap: int) -> NDArray[np.int64]:
    if n_samples < in_samples:
        return np.array([], dtype=np.int64)
    step = in_samples - overlap
    offsets = np.arange(0, n_samples - in_samples + 1, step, dtype=np.int64)
    if offsets.size != 0 and int(offsets[-1]) + in_samples < n_samples:
        offsets = np.concatenate([offsets, np.array([n_samples - in_samples], dtype=np.int64)])
    return offsets


def _window_count(data: NDArray[np.float32], spec: dict[str, Any]) -> int:
    in_samples = int(spec["in_samples"])
    output_type = str(spec["output_type"])
    if output_type == "array":
        return int(_array_offsets(data.shape[-1], in_samples, _overlap(in_samples, spec)).size)
    if output_type == "point":
        if data.shape[-1] < in_samples:
            return 0
        stride = int(spec.get("stride", 1))
        return int(math.floor((data.shape[-1] - in_samples) / stride)) + 1
    raise NotImplementedError("unsupported SeisBench output_type %r" % output_type)


async def _annotate_array(
    model_url: str,
    data: NDArray[np.float32],
    spec: dict[str, Any],
    progress: Callable[[], None] | None,
    progress_label: str,
) -> Annotation:
    in_samples = int(spec["in_samples"])
    sampling_rate = float(spec["sampling_rate"])
    overlap = _overlap(in_samples, spec)
    offsets = _array_offsets(data.shape[-1], in_samples, overlap)
    n_labels = len(labels(spec))
    if offsets.size == 0:
        return Annotation(
            np.full((0, n_labels), np.nan, dtype=np.float64),
            sampling_rate,
            0.0,
        )

    windows = np.stack([data[:, off : off + in_samples] for off in offsets])
    predictions = await _predict_batches(
        model_url, _annotate_batch_pre(windows, spec), spec, progress, progress_label
    )
    if predictions.ndim != 3:
        raise RuntimeError("array model returned shape %r" % (predictions.shape,))

    if str(spec.get("output_layout", "channels_first")) == "channels_first":
        predictions = np.transpose(predictions, (0, 2, 1))

    predictions64: NDArray[np.float64] = predictions.astype(np.float64, copy=False)
    prenan, postnan = _blinding(spec)
    if prenan > 0:
        predictions64[:, :prenan, :] = np.nan
    if postnan > 0:
        predictions64[:, -postnan:, :] = np.nan

    pred_sample = cast(list[Any], spec.get("pred_sample", [0, in_samples]))
    pred_sample_factor = predictions64.shape[1] / float(int(pred_sample[1]) - int(pred_sample[0]))
    pred_len = int(math.ceil((int(offsets[-1]) + in_samples) * pred_sample_factor))
    stacked = _stack_array_predictions(
        predictions64,
        offsets,
        pred_len,
        pred_sample_factor,
        str(spec.get("stacking", "avg")).lower(),
    )
    return Annotation(
        stacked, sampling_rate * pred_sample_factor, int(pred_sample[0]) / sampling_rate
    )


# Port of seisbench.models.base.WaveformModel._cut_fragments_point and
# _reassemble_blocks_point (SeisBench 0.11.7).
async def _annotate_point(
    model_url: str,
    data: NDArray[np.float32],
    spec: dict[str, Any],
    progress: Callable[[], None] | None,
    progress_label: str,
) -> Annotation:
    in_samples = int(spec["in_samples"])
    sampling_rate = float(spec["sampling_rate"])
    stride = int(spec.get("stride", 1))
    offsets = np.arange(0, data.shape[-1] - in_samples + 1, stride, dtype=np.int64)
    n_labels = len(labels(spec))
    if offsets.size == 0:
        return Annotation(
            np.full((0, n_labels), np.nan, dtype=np.float64),
            sampling_rate / stride,
            int(spec["pred_sample"]) / sampling_rate,
        )

    windows = np.stack([data[:, off : off + in_samples] for off in offsets])
    predictions = await _predict_batches(
        model_url, _annotate_batch_pre(windows, spec), spec, progress, progress_label
    )
    if predictions.ndim == 1:
        predictions = predictions.reshape(-1, 1)
    return Annotation(
        predictions.astype(np.float64, copy=False),
        sampling_rate / stride,
        int(spec["pred_sample"]) / sampling_rate,
    )


def _overlap(in_samples: int, spec: dict[str, Any]) -> int:
    overlap = float(spec.get("overlap", 0))
    if overlap < 1:
        return int(overlap * in_samples)
    return int(overlap)


def _blinding(spec: dict[str, Any]) -> tuple[int, int]:
    raw = cast(list[Any], spec.get("blinding", [0, 0]))
    return int(raw[0]), int(raw[1])


async def _predict_batches(
    model_url: str,
    windows: NDArray[np.float32],
    spec: dict[str, Any],
    progress: Callable[[], None] | None,
    progress_label: str,
) -> NDArray[np.float32]:
    batch_size = int(spec.get("batch_size", 256))
    outputs: list[NDArray[np.float32]] = []
    for start in range(0, windows.shape[0], batch_size):
        outputs.append(
            await _ort_forward(model_url, windows[start : start + batch_size], progress_label)
        )
        if progress is not None:
            progress()
    if not outputs:
        return np.empty((0,), dtype=np.float32)
    return np.concatenate(outputs, axis=0)


async def _ort_forward(
    model_url: str, batch: NDArray[np.float32], progress_label: str
) -> NDArray[np.float32]:
    import js

    contiguous = np.ascontiguousarray(batch, dtype=np.float32)
    result = await js.__snufflerOrtInfer(
        model_url,
        contiguous.reshape(-1),
        [int(x) for x in contiguous.shape],
        progress_label,
    )
    dims = _js_int_list(result.dims)
    raw = bytes(
        js.Uint8Array.new(
            result.data.buffer,
            result.data.byteOffset,
            result.data.byteLength,
        )
    )
    arr = np.frombuffer(raw, dtype=np.float32).copy()
    return arr.reshape(dims)


def _js_int_list(value: Any) -> list[int]:
    if hasattr(value, "to_py"):
        return [int(x) for x in value.to_py()]
    return [int(x) for x in value]


# Port of seisbench.models.base.WaveformModel._stack_predictions_array
# (SeisBench 0.11.7), specialized to instrument grouping.
def _stack_array_predictions(
    predictions: NDArray[np.float64],
    offsets: NDArray[np.int64],
    pred_len: int,
    pred_sample_factor: float,
    method: str,
) -> NDArray[np.float64]:
    n_labels = predictions.shape[-1]
    if method == "avg":
        acc = np.zeros((pred_len, n_labels), dtype=np.float64)
        cnt = np.zeros((pred_len, n_labels), dtype=np.int64)
        for pred, offset in zip(predictions, offsets, strict=True):
            start = int(round(int(offset) * pred_sample_factor))
            stop = min(start + pred.shape[0], pred_len)
            seg = pred[: stop - start]
            valid = ~np.isnan(seg)
            acc_view = acc[start:stop]
            cnt_view = cnt[start:stop]
            acc_view[valid] += seg[valid]
            cnt_view[valid] += 1
        out = np.full((pred_len, n_labels), np.nan, dtype=np.float64)
        valid_count = cnt > 0
        out[valid_count] = acc[valid_count] / cnt[valid_count]
        return out

    if method == "max":
        out = np.full((pred_len, n_labels), np.nan, dtype=np.float64)
        for pred, offset in zip(predictions, offsets, strict=True):
            start = int(round(int(offset) * pred_sample_factor))
            stop = min(start + pred.shape[0], pred_len)
            seg = pred[: stop - start]
            cur = out[start:stop]
            out[start:stop] = np.where(np.isnan(cur), seg, np.fmax(cur, seg))
        return out

    raise ValueError("unknown SeisBench stacking method %r" % method)


# Port of obspy.signal.trigger.trigger_onset as called by
# seisbench.models.base.WaveformModel.picks_from_annotations (SeisBench 0.11.7).
def trigger_onset(cf: NDArray[np.float64], thres1: float, thres2: float) -> list[tuple[int, int]]:
    ind1 = np.where(cf >= thres1)[0]
    if ind1.size == 0:
        return []
    ind2 = np.where(cf >= thres2)[0]
    on = deque([int(ind1[0])])
    of = deque([-1])
    ind2_ = np.empty_like(ind2, dtype=bool)
    ind2_[:-1] = np.diff(ind2) > 1
    ind2_[-1] = True
    of.extend(int(i) for i in ind2[ind2_])
    on.extend(int(i) for i in ind1[np.where(np.diff(ind1) > 1)[0] + 1])
    of.append(int(ind2[-1]))
    of.append(len(cf))
    picks: list[tuple[int, int]] = []
    while on[-1] > of[0]:
        while on[0] <= of[0]:
            on.popleft()
        while of[0] < on[0]:
            of.popleft()
        picks.append((on[0], of[0]))
    return picks


# Port of seisbench.models.base.WaveformModel.picks_from_annotations
# (SeisBench 0.11.7), returning compact tuples for bridge marker creation.
def extract_picks(curve: NDArray[np.float64], threshold: float) -> list[tuple[int, float]]:
    cf = np.nan_to_num(curve, nan=-1.0)
    picks: list[tuple[int, float]] = []
    for s0, s1 in trigger_onset(cf, threshold, threshold / 2.0):
        seg = cf[s0 : s1 + 1]
        k = int(np.argmax(seg))
        picks.append((s0 + k, float(seg[k])))
    return picks
