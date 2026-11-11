# 3. SeisBench inference on onnxruntime-web

**Status:** accepted · **Date:** 2026-07-05

## Context

Upstream snuffler's `seisbench_picker` snuffling runs SeisBench models through torch.
torch has no WebAssembly build, and seisbench imports it at module top, so neither imports
in Pyodide.
The first port ran one model (PhaseNet) through a hand-written numpy ONNX interpreter, which
was CPU-only and could not run EQTransformer's LSTM and attention layers.
The question was which engine runs the SeisBench model zoo in the browser, fast.

The Rust options all fail on something concrete:

- **wonnx** (pure-Rust WebGPU ONNX) was archived in May 2025 and never supported LSTM or
  attention.
- **burn** imports ONNX by build-time code generation, so it cannot load a model chosen at
  runtime, and its RNN and attention coverage is partial.
- **tract** has good operator coverage but runs on the CPU only.
- **`ort` on wasm** wraps onnxruntime's own Emscripten build, which adds glue and no benefit.
- The egui app renders through glow (WebGL2), so there is no `wgpu::Device` to share; a Rust
  WebGPU engine would mean switching eframe's backend.

Inference in the Rust app would also need a channel from Python back to Rust, which the
bridge deliberately does not have: calls go one way, Rust to the worker.

## Decision

The forward pass runs on **onnxruntime-web** with the WebGPU execution provider and the wasm
provider as fallback, in a dedicated module worker (`web/ort_worker.ts`) that the Pyodide
worker starts and can terminate when a session hangs.
onnxruntime-web partitions each graph, so operators without a WebGPU kernel (EQTransformer's
LSTM) fall back to the CPU inside the same session, and browsers without WebGPU run every
model on the wasm provider.

Pre- and post-processing stay in Python (`web/seisbench_infer.py`) as numpy ports of the
SeisBench functions, each citing the function it mirrors.
They are pinned to one SeisBench version (`SEISBENCH_VERSION`, also recorded in
`web/assets/seisbench_models.json`), bumped deliberately and never implicitly.
`pybuild/export_seisbench_models.py` exports each curated model to ONNX and writes the
manifest that carries its windowing, normalisation and threshold parameters.

## Consequences

- Any model in the manifest runs without code changes; adding one is an export plus a
  manifest entry.
- The numpy port can drift from SeisBench, which is why `task test:seisbench` checks every
  exported model against the pinned seisbench's own `classify()`, and
  `task test:seisbench:browser` checks the served app.
- The onnxruntime-web engine is always bundled same-origin, because cross-origin isolation
  forbids loading it from elsewhere.
  Model weights are bundled by default (the `embed-data` feature); a lean build fetches them
  on demand instead.
- ORT's own proxy worker stays off: it is incompatible with the WebGPU provider, and the
  inference already runs in a worker.

## References

- `web/ort_worker.ts`, `web/bridge_worker.ts`, `web/seisbench_infer.py`
- `pybuild/export_seisbench_models.py`, `web/assets/seisbench_models.json`
- `tests/seisbench_validate.py`, `tests/seisbench_browser_validate.ts`
