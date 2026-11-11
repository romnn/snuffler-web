"""Export curated SeisBench models to ONNX for onnxruntime-web.

Run:
  uv run --no-project --with seisbench==0.11.7 --with torch --with onnx \
      --with onnxruntime python pybuild/export_seisbench_models.py

The runtime port in web/seisbench_infer.py is pinned to the same SeisBench
version.
Bump both together and rerun the parity test.
"""

from __future__ import annotations

import json
import pathlib
import shutil
import sys
import tempfile
from dataclasses import dataclass
from typing import Any

import numpy as np
import onnx

SEISBENCH_VERSION = "0.11.7"
ROOT = pathlib.Path(__file__).resolve().parent.parent
MODEL_DIR = ROOT / "web" / "assets" / "models"
MANIFEST = ROOT / "web" / "assets" / "seisbench_models.json"


@dataclass(frozen=True)
class ExportTarget:
    class_name: str
    weights: str
    batch_size: int


TARGETS = [
    ExportTarget("PhaseNet", "instance", 256),
    ExportTarget("EQTransformer", "original", 16),
    ExportTarget("GPD", "stead", 512),
]


class SingleOutputWrapper:
    """Torch module wrapper that canonicalizes SeisBench outputs to one tensor.

    Some models (EQTransformer) return one tensor per label; ONNX gets them
    stacked along the channel axis, so every exported graph has a single output.
    """

    def __init__(self, model: Any) -> None:
        import torch

        class _Wrapper(torch.nn.Module):
            def __init__(self, inner: Any) -> None:
                super().__init__()
                self.inner = inner

            def forward(self, waveform: Any) -> Any:
                output = self.inner(waveform)
                if isinstance(output, tuple):
                    return torch.stack(output, dim=1)
                return output

        self.module = _Wrapper(model)


def jsonable(value: Any) -> Any:
    if isinstance(value, tuple):
        return [jsonable(x) for x in value]
    if isinstance(value, list):
        return [jsonable(x) for x in value]
    if isinstance(value, dict):
        return {str(key): jsonable(item) for key, item in value.items()}
    if isinstance(value, np.generic):
        return value.item()
    return value


# An `annotate()` argument's default: the model's own override, else SeisBench's
# class-wide default.
def annotate_default(model: Any, key: str) -> Any:
    if key in model.default_args:
        return model.default_args[key]
    return model._annotate_args[key][1]


def phase_threshold(model: Any, phase: str) -> float:
    key = f"{phase}_threshold"
    if key in model.default_args:
        return float(model.default_args[key])
    return float(model._annotate_args["*_threshold"][1])


def export_onnx(target: ExportTarget, onnx_path: pathlib.Path) -> dict[str, Any]:
    import seisbench
    import seisbench.models as sbm
    import torch

    if seisbench.__version__ != SEISBENCH_VERSION:
        raise RuntimeError(f"expected seisbench {SEISBENCH_VERSION}, got {seisbench.__version__}")

    model_class = getattr(sbm, target.class_name)
    model = model_class.from_pretrained(target.weights)
    model.eval()
    wrapper = SingleOutputWrapper(model).module.eval()
    channels = int(model.in_samples and model.in_channels or 3)
    example_input = torch.randn(1, channels, int(model.in_samples))
    # The batch axis stays dynamic so the browser can batch windows freely.
    torch.onnx.export(
        wrapper,
        example_input,
        str(onnx_path),
        input_names=["waveform"],
        output_names=["probs"],
        dynamo=False,
        opset_version=17,
        dynamic_axes={"waveform": {0: "batch"}, "probs": {0: "batch"}},
    )
    onnx.checker.check_model(str(onnx_path))
    validate_onnx(wrapper, onnx_path, int(model.in_samples), int(model.in_channels))
    return model_manifest(target, model, onnx_path.name)


def validate_onnx(wrapper: Any, path: pathlib.Path, in_samples: int, in_channels: int) -> None:
    """Fail unless ONNX Runtime reproduces torch on random input, at several batch
    sizes, to within 1e-3 (the outputs are probabilities)."""
    import onnxruntime as ort
    import torch

    session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    input_name = session.get_inputs()[0].name
    random = np.random.default_rng(1)
    worst_abs_diff = 0.0
    for batch in (1, 2, 5):
        waveforms = random.standard_normal((batch, in_channels, in_samples)).astype(np.float32)
        with torch.no_grad():
            torch_probs = wrapper(torch.from_numpy(waveforms)).detach().numpy()
        onnx_probs = session.run(None, {input_name: waveforms})[0]
        worst_abs_diff = max(worst_abs_diff, float(np.max(np.abs(onnx_probs - torch_probs))))
    print(f"{path.name}: validation worst max|diff| vs onnxruntime: {worst_abs_diff:.3e}")
    if worst_abs_diff >= 1e-3:
        raise RuntimeError(f"{path.name}: ONNX Runtime output diverges from torch")


# Everything web/seisbench_infer.py needs to pre- and post-process like SeisBench.
def model_manifest(target: ExportTarget, model: Any, filename: str) -> dict[str, Any]:
    model_args = model.get_model_args()
    labels = model.labels
    if isinstance(labels, str):
        labels = list(labels)
    default_args = dict(model.default_args)
    for phase in ("P", "S"):
        if phase in labels:
            default_args.setdefault(f"{phase}_threshold", phase_threshold(model, phase))
    if "Detection" in labels:
        default_args.setdefault(
            "detection_threshold", annotate_default(model, "detection_threshold")
        )

    spec = {
        "model": target.class_name,
        "weights": target.weights,
        "weights_version": str(model.weights_version),
        "file": filename,
        "sampling_rate": float(model.sampling_rate),
        "in_samples": int(model.in_samples),
        "component_order": str(model.component_order),
        "labels": labels,
        "output_type": str(model.output_type),
        "output_layout": "channels_first",
        "pred_sample": jsonable(model.pred_sample),
        "norm": model_args.get("norm", ""),
        "norm_amp_per_comp": bool(model_args.get("norm_amp_per_comp", False)),
        "norm_detrend": bool(model_args.get("norm_detrend", False)),
        "overlap": jsonable(annotate_default(model, "overlap")),
        "stacking": jsonable(annotate_default(model, "stacking")),
        "blinding": jsonable(annotate_default(model, "blinding"))
        if "blinding" in model._annotate_args
        else [0, 0],
        "stride": jsonable(annotate_default(model, "stride")),
        "default_args": jsonable(default_args),
        "filter_args": jsonable(model_args.get("filter_args")),
        "filter_kwargs": jsonable(model_args.get("filter_kwargs")),
        "batch_size": target.batch_size,
    }
    return spec


def main() -> None:
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    MANIFEST.parent.mkdir(parents=True, exist_ok=True)
    models: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory() as temp_dir:
        for target in TARGETS:
            stem = f"{target.class_name.lower()}_{target.weights.lower()}"
            onnx_path = pathlib.Path(temp_dir) / f"{stem}.onnx"
            print(f"exporting {target.class_name}/{target.weights}")
            spec = export_onnx(target, onnx_path)
            shutil.copy2(onnx_path, MODEL_DIR / onnx_path.name)
            models.append(spec)
            print(f"wrote {MODEL_DIR / onnx_path.name} ({onnx_path.stat().st_size} bytes)")

    manifest = {
        "seisbench_version": SEISBENCH_VERSION,
        "onnx_opset": 17,
        "models": models,
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"wrote {MANIFEST}")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        raise
