# Fast multi-model ML inference in the browser (WebGPU) — research + plan

> Status: **researched, ready to implement.** Replaces the pure-numpy ONNX
> interpreter with onnxruntime-web (WebGPU EP + wasm fallback) so the *whole*
> SeisBench model zoo runs, GPU-accelerated — bundled by default (the `embed-data`
> feature) or fetched on demand. Captured 2026-07-05.

## Why not Rust / pure-WebGPU-in-Rust (the premise, checked)

WebGPU is reachable (`navigator.gpu` in the Pyodide worker; the app is already
cross-origin-isolated, so `SharedArrayBuffer` + multithreaded wasm work). But on
**wasm** there is no live, complete, on-demand pure-Rust WebGPU ONNX runtime:

- **wonnx** (pure-Rust WebGPU ONNX) — archived May 2025; never supported
  LSTM/attention. Dead end.
- **burn** (`burn-wgpu`) — WebGPU works, but ONNX import is **build-time codegen**
  (ONNX→Rust), so it can't load arbitrary models at runtime; op coverage for
  RNN/attention is partial.
- **tract** — pure-Rust, good op coverage, **CPU-only (no WebGPU)**.
- **`ort` crate on wasm (`ort-web`)** — does **not** do pure-Rust inference; it
  **bridges to onnxruntime's Emscripten build** (i.e. wraps onnxruntime-web). No
  benefit over calling onnxruntime-web directly, plus extra glue.
- The app renders with **glow (WebGL2), not wgpu** (`Cargo.toml` eframe
  `default-features=false, features=["glow"]`; `WebOptions::default()` in
  `src/main.rs`) — there is **no `wgpu::Device`** to reuse; adding one means
  switching the eframe backend.

**Topology** also forbids Rust inference cheaply: the bridge is **strictly
one-way Rust→worker** (`src/bridge.rs` client → `web/bridge.ts` postMessage →
`web/bridge_worker.ts` → `bridge.py`). There is **no Python→Rust / worker→Rust
callback**. Inference in main-thread Rust would need a brand-new reverse channel.

⇒ The best in-browser WebGPU ONNX engine is **onnxruntime-web itself**. It loads
**inside the Pyodide worker** and `bridge.py` drives it via `import js` (same
mechanism already used by `pyfetch` and `_asset_url`), preserving the one-way
architecture. This delivers every real goal: **GPU speed, arbitrary models on
demand, and dropping `onnx_infer.py`** — just in JS, not Rust.

## Why the SeisBench source can't be reused directly — and the anti-drift plan

seisbench 0.11.7 `Requires-Dist: torch>=1.10.0`; `seisbench/models/base.py`
imports torch at module top and every model is a `torch.nn.Module`. pyrocko's own
`sources/pyrocko/src/gui/snuffler/snufflings/seisbench_picker.py` just wraps
seisbench. **torch has no wasm build ⇒ neither imports in Pyodide.** So the
faithful numpy pre/post port stays — but it must not silently drift from
seisbench. The mitigation is three concrete, mandatory measures:

1. **Vendor verbatim, don't re-derive.** Copy the exact pre/post functions from
   the seisbench source (`annotate_batch_pre`/`annotate_batch_post` per model,
   `_cut_fragments`, `stack_windows`/blend, `picks_from_annotations`,
   `trigger_onset`), swapping only torch tensor ops for numpy. Cite the source
   file+function in a comment above each ported function.
2. **Pin the version.** Record the exact seisbench version the port and the model
   export were taken from (a `SEISBENCH_VERSION` constant + a note in
   `pybuild/export_seisbench_models.py`); bump deliberately, never implicitly.
3. **Per-model host parity test (the guard).** Promote
   `scratchpad/seisbench_validate.py` into a **committed** host test that, for
   EACH shipped model, runs the *real* pinned seisbench `model.classify()` on a
   test stream and asserts our port's picks match to **≤1 sample** and the
   probability-curve correlation > 0.999. Wire it into the taskfile so CI runs it.

## Model op-coverage reality

- **PhaseNet / GPD** — pure CNN (Conv/ConvTranspose/pool). Fully WebGPU.
- **EQTransformer** — CNN + **BiLSTM + self-attention**. onnxruntime-web runs
  attention/conv on WebGPU and **falls back per-op to the wasm/CPU EP** for
  anything without a GPU kernel (LSTM), within one session via graph
  partitioning. So it runs; just not 100% on GPU. This is why a *real* runtime
  (not the hand-written numpy interpreter) is needed for the zoo.

## Target architecture

- **Build step** `pybuild/export_seisbench_models.py` (extends
  `export_phasenet.py`): loop over a curated `(model_class, weights)` set, export
  each to a raw **`.onnx`** (onnxruntime-web loads `.onnx` directly — the `.npz`
  format was only for the numpy interpreter and goes away), and emit a
  **`web/assets/seisbench_models.json` manifest** capturing per-model pre/post
  params: `sampling_rate`, `in_samples`, `component_order`, `labels`, output
  semantics (PhaseNet PSN vs EQTransformer detection+P+S), `norm` type +
  `norm_amp_per_comp`/`norm_detrend`, `overlap`, `blinding`, and calibrated
  `default_args` thresholds. Re-validate each export against onnxruntime on the
  host.
- **Runtime JS** (`web/bridge_worker.ts`): a lazy-loaded helper exposed to
  Pyodide, e.g. `globalThis.__snufflerOrtInfer(modelUrl, f32, dims) ->
  Promise<{data, dims}>`, that dynamic-imports `onnxruntime-web/webgpu`, sets
  `ort.env.wasm.wasmPaths` to bundled **same-origin** artifacts, creates+caches
  an `InferenceSession` per `modelUrl` with `executionProviders:['webgpu','wasm']`,
  and runs it. **Do NOT enable ORT's proxy-worker** (incompatible with the WebGPU
  EP; we already are in a worker).
- **Runtime Python** `web/seisbench_infer.py` (replaces `onnx_infer.py` +
  `seisbench_pick.py`): faithful per-model windowing/normalization/blending/pick
  extraction (vendored from seisbench), calling `await
  js.__snufflerOrtInfer(...)` for the forward pass. `bridge.py`
  `_seisbench_pick_result` gains a `model`/`weights` parameter; `_ensure_phasenet`
  becomes `_ensure_seisbench_model(name)` that fetches the chosen `.onnx` on
  demand (bundled, or via the existing `snuffler-cors-proxy` for the seisbench
  model repo).
- **Proto/UI**: add a `seisbench` message to `SnufflingParams` (model name,
  weights, optional threshold overrides); Rust panel `src/snufflings/seisbench.rs`
  gets model + weights dropdowns.
- **Drop**: `web/onnx_infer.py`, `web/assets/phasenet.npz`, the `.npz` path in the
  export script, and the `onnx_infer`/`seisbench_pick` staging lines specific to
  the interpreter (`bridge_worker.ts`).

## Bundling — the `embed-data` feature (default ON)

The onnxruntime-web **engine** artifacts (~a few MB,
`ort-wasm-simd-threaded.jsep.*`) and the **tiny model manifest**
(`seisbench_models.json`) are **always** bundled same-origin (COEP requires it) —
they're the runtime, not data.

The **model `.onnx` weights** (the "curated set", a few MB) are gated by a new
cargo feature **`embed-data`, on by default**:

- **`embed-data` on (default):** the `.onnx` files are copied into `dist/` and the
  app fetches them from local `assets/models/…` — fully offline, one download.
- **`embed-data` off:** the `.onnx` files are NOT bundled; the app fetches the
  chosen model on demand from the seisbench model repository through the existing
  `snuffler-cors-proxy` (COEP-safe, cached after first use) — a leaner initial
  build.

Mechanism (follow the existing `threads` / py-mt-hook patterns — a feature spans
three build systems):
- **cargo**: add `embed-data` to `snuffler-web`'s default features; the app reads
  `cfg!(feature = "embed-data")` to choose the local-vs-remote model base URL and
  puts it in the picker request (protobuf DTO — no hardcoded URLs leaking through
  side channels).
- **Trunk**: a conditional post_build **copy hook** (mirroring the py-mt
  conditional-copy hook in `Trunk.toml`) stages `web/assets/models/*.onnx`.
  *As shipped this is opt-OUT* (`SNUFFLER_EMBED_DATA=0`), not opt-in: the shell
  switch has to default the same way the cargo feature does, or a build path that
  never sets it (the GitHub Pages workflow drives trunk directly) ships an app
  that asks for weights nobody copied — a green deploy that 404s on every pick.
  The hook also fails the build when a weight named by the manifest is missing.
- **mise/taskfile**: the default `serve`/`build` tasks just build with the
  feature; only the lean variant — `--no-default-features` (re-adding the needed
  defaults) — sets `SNUFFLER_EMBED_DATA=0` to match.

## Risks / validation gates

1. **onnxruntime-web loads under COEP-credentialless in the module worker** with
   same-origin `wasmPaths`, WebGPU EP active, proxy-worker OFF. (Primary risk —
   validate first with a trivial model.)
2. **EQTransformer ONNX export** (LSTM) — `dynamo=False` may mishandle LSTM; try
   `dynamo=True`/opset tweaks; validate export against onnxruntime on host.
3. **WebGPU absent (Firefox/Safari)** → `['webgpu','wasm']` auto-falls back to CPU
   wasm. Still works, slower. Verify.
4. **Per-model faithfulness** — browser picks must match `seisbench.classify()`
   (pinned version) to ≤1 sample for each shipped model; commit that as a test.

The ready-to-hand implementation-agent prompt lives in
[`ml-webgpu-inference-agent-prompt.md`](./ml-webgpu-inference-agent-prompt.md).
