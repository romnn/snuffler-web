"""Host parity guard for the browser SeisBench inference path.

Run through:
  task test:seisbench
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path
from typing import Any, cast

import numpy as np
import obspy
import onnxruntime as ort

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "web"))

import seisbench_infer as sbi  # noqa: E402


def _manifest() -> dict[str, Any]:
    import json

    return json.loads((ROOT / "web" / "assets" / "seisbench_models.json").read_text())


def _model_class(name: str) -> Any:
    import seisbench.models as sbm

    return getattr(sbm, name)


def _example_stream() -> obspy.Stream:
    stream = obspy.read()
    for tr in stream:
        tr.data = np.tile(tr.data, 8)
    return stream


def _seisbench_group(model: Any, stream: obspy.Stream) -> Any:
    argdict = model.default_args.copy()
    stream = stream.copy()
    stream.merge(-1)
    model.annotate_stream_pre(stream, argdict)
    stream = model.annotate_stream_validate(stream, argdict)
    argdict["sampling_rate"] = stream[0].stats.sampling_rate
    comp_dict, _ = model._build_comp_dict(
        stream,
        model._argdict_get_with_default(argdict, "flexible_horizontal_components"),
    )
    groups = model._grouping.group_stream(
        stream,
        strict=model._argdict_get_with_default(argdict, "strict"),
        min_length_s=(model.in_samples - 1) / argdict["sampling_rate"],
        comp_dict=comp_dict,
    )
    if not groups:
        raise RuntimeError(f"{model.name}: no SeisBench groups in example stream")
    return model.stream_to_array(obspy.Stream(groups[0]), argdict)


def _aligned_curves(
    ref_trace: Any, ours: sbi.Annotation, ours_start_abs: float
) -> tuple[np.ndarray, np.ndarray]:
    ref = np.asarray(ref_trace.data, dtype=np.float64)
    base = int(round((ref_trace.stats.starttime.timestamp - ours_start_abs) * ours.sampling_rate))
    if base < 0:
        ref = ref[-base:]
        base = 0
    m = min(ref.size, ours.data.shape[0] - base)
    return ref[:m], ours.data[base : base + m]


def _torch_output(model: Any, batch: np.ndarray) -> np.ndarray:
    import torch

    with torch.no_grad():
        output = model(torch.from_numpy(batch))
    if isinstance(output, tuple):
        output = torch.stack(output, dim=1)
    return cast(np.ndarray, output.detach().cpu().numpy())


def _validate_onnx_export(model: Any, sess: ort.InferenceSession, input_name: str) -> None:
    rng = np.random.default_rng(1)
    in_channels = int(model.in_channels)
    in_samples = int(model.in_samples)
    worst = 0.0
    for batch_size in (1, 2, 5):
        batch = rng.standard_normal((batch_size, in_channels, in_samples)).astype(np.float32)
        ref = _torch_output(model, batch)
        got = sess.run(None, {input_name: batch})[0]
        worst = max(worst, float(np.max(np.abs(got - ref))))
    if worst >= 1e-3:
        raise AssertionError(
            f"{model.name}: ONNX output diverges from torch, max|diff|={worst:.3e}"
        )


def _ours_pick_times(
    ours: sbi.Annotation,
    spec: dict[str, Any],
    phase: str,
    group_start: float,
) -> list[float]:
    threshold = sbi.default_threshold(spec, phase)
    curve = ours.data[:, sbi.phase_channel(spec, phase)]
    return [
        group_start + ours.start_offset + idx / ours.sampling_rate
        for idx, _value in sbi.extract_picks(curve, threshold)
    ]


def _upstream_pick_times(classification: Any, phase: str) -> list[float]:
    return sorted(
        float(pick.peak_time.timestamp) for pick in classification.picks if pick.phase == phase
    )


async def _validate_picks(
    model: Any,
    spec: dict[str, Any],
    ours: sbi.Annotation,
    group_start: float,
    stream: obspy.Stream,
) -> None:
    classification = await model.classify_async(stream.copy())
    tolerance = 1.0 / ours.sampling_rate + 1e-6
    for phase in ("P", "S"):
        if phase not in sbi.labels(spec):
            continue
        ref_times = _upstream_pick_times(classification, phase)
        ours_times = sorted(_ours_pick_times(ours, spec, phase, group_start))
        if len(ours_times) != len(ref_times):
            raise AssertionError(
                f"{spec['model']}/{spec['weights']} {phase}: "
                f"{len(ours_times)} picks, upstream has {len(ref_times)}"
            )
        for i, (ref, got) in enumerate(zip(ref_times, ours_times, strict=True)):
            delta = abs(ref - got)
            if delta > tolerance:
                raise AssertionError(
                    f"{spec['model']}/{spec['weights']} {phase} pick {i}: "
                    f"{delta:.6f}s from upstream, tolerance {tolerance:.6f}s"
                )


async def _validate_model(spec: dict[str, Any]) -> None:
    cls = _model_class(str(spec["model"]))
    model = cls.from_pretrained(str(spec["weights"]))
    model.eval()
    model_path = ROOT / "web" / "assets" / "models" / str(spec["file"])
    sess = ort.InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
    input_name = sess.get_inputs()[0].name
    _validate_onnx_export(model, sess, input_name)

    async def host_ort(_model_url: str, batch: np.ndarray, _progress_label: str = "") -> np.ndarray:
        return sess.run(None, {input_name: np.asarray(batch, dtype=np.float32)})[0]

    sbi._ort_forward = host_ort  # type: ignore[attr-defined]
    stream = _example_stream()
    group = _seisbench_group(model, stream)
    ours = await sbi.annotate(str(model_path), group.data.astype(np.float32), spec)
    ref_ann = await model.annotate_async(stream.copy())
    await _validate_picks(model, spec, ours, group.start_time.timestamp, stream)

    for phase in ("P", "S"):
        if phase not in sbi.labels(spec):
            continue
        ref_trace = ref_ann.select(channel=f"{spec['model']}_{phase}")[0]
        ref, all_ours = _aligned_curves(
            ref_trace, ours, group.start_time.timestamp + ours.start_offset
        )
        ours_curve = all_ours[:, sbi.phase_channel(spec, phase)]
        both = ~np.isnan(ref) & ~np.isnan(ours_curve)
        if not both.any():
            raise AssertionError(f"{spec['model']}/{spec['weights']} {phase}: no overlap")
        corr = float(np.corrcoef(ref[both], ours_curve[both])[0, 1])
        if corr <= 0.999:
            raise AssertionError(
                f"{spec['model']}/{spec['weights']} {phase}: curve corr {corr:.6f}"
            )


async def main() -> None:
    manifest = _manifest()
    if manifest["seisbench_version"] != sbi.SEISBENCH_VERSION:
        raise AssertionError("manifest/runtime SeisBench version mismatch")
    for spec in manifest["models"]:
        await _validate_model(spec)
        print(f"PASS {spec['model']}/{spec['weights']}")


if __name__ == "__main__":
    asyncio.run(main())
